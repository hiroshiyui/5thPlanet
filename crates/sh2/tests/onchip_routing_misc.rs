//! Width-mixing and helper-routing coverage for `onchip/mod.rs`: the
//! byte-aggregating read16/read32/write32 fallbacks, the DMAC byte
//! read-modify-write helpers, the DIVU sub-word byte RMW, the native 32-bit
//! DMAOR path, the WDT-guarded write16 routing, and INTC VCR byte access.
//! All addresses are absolute SH7604 on-chip register addresses.

use sh2::OnChip;

#[test]
fn read16_aggregates_two_bytes_big_endian_from_a_non_native_window() {
    // The FRT block has no native 16-bit path in read16(); it falls back to
    // two byte reads aggregated big-endian. Write FRC via two bytes then read
    // it back as a halfword.
    let mut o = OnChip::new();
    o.write8(0xFFFF_FE12, 0xAB); // FRC high byte
    o.write8(0xFFFF_FE13, 0xCD); // FRC low byte
    assert_eq!(o.read16(0xFFFF_FE12), 0xABCD, "big-endian byte aggregation");
}

#[test]
fn read32_byte_aggregation_for_non_native_window() {
    // The INTC IPRB/VCR window has no native 32-bit path; read32() aggregates
    // four byte reads. IPRB (0xFFFFFE60) and VCRA (0xFFFFFE62) sit adjacent.
    let mut o = OnChip::new();
    o.write16(0xFFFF_FE60, 0x1122); // IPRB
    o.write16(0xFFFF_FE62, 0x3344); // VCRA
    assert_eq!(
        o.read32(0xFFFF_FE60),
        0x1122_3344,
        "read32 aggregates IPRB:VCRA big-endian"
    );
}

#[test]
fn write32_byte_aggregation_splits_into_four_bytes_for_non_native_window() {
    let mut o = OnChip::new();
    o.write32(0xFFFF_FE60, 0xDEAD_BEEF); // IPRB:VCRA via the byte-split path
    assert_eq!(o.intc.iprb, 0xDEAD, "high halfword → IPRB");
    assert_eq!(o.intc.vcra, 0xBEEF, "low halfword → VCRA");
}

#[test]
fn dmac_byte_read_modify_write_preserves_the_rest_of_the_word() {
    // A byte write to the low byte of CHCR0 (0xFFFFFF8F) must leave the upper
    // three bytes intact (RMW via dmac_write8/dmac_read8).
    let mut o = OnChip::new();
    o.write32(0xFFFF_FF8C, 0x1122_3344); // CHCR0
    o.write8(0xFFFF_FF8F, 0xFF); // low byte only
    assert_eq!(o.dmac.channels[0].chcr, 0x1122_33FF, "only the low byte changed");
    // And byte reads pick the right byte out of the word.
    assert_eq!(o.read8(0xFFFF_FF8C), 0x11, "byte 0 of CHCR0");
    assert_eq!(o.read8(0xFFFF_FF8E), 0x33, "byte 2 of CHCR0");
}

#[test]
fn dmaor_native_32_bit_path_masks_to_16_bits() {
    // DMAOR (0xFFFFFFB0) is a native 32-bit register but only the low 16 bits
    // are storable; the high bits are masked off on write.
    let mut o = OnChip::new();
    o.write32(0xFFFF_FFB0, 0xFFFF_0001);
    assert_eq!(o.dmac.dmaor, 0x0000_0001, "DMAOR masked to 16 bits");
    assert_eq!(o.read32(0xFFFF_FFB0), 0x0000_0001, "native 32-bit read-back");
}

#[test]
fn divu_byte_write_does_read_modify_write_on_the_32_bit_slot() {
    // DVCR (0xFFFFFF08) is 32-bit on hw; a byte write to its low byte must
    // RMW the longword, not clobber the whole register.
    let mut o = OnChip::new();
    o.write32(0xFFFF_FF08, 0x0000_0000);
    o.write8(0xFFFF_FF0B, 0x02); // DVCR low byte → OVFIE
    assert_eq!(o.divu.dvcr & 0xFF, 0x02, "low byte set via RMW");
    // A full-word read sees the RMW result (DVCR.OVFIE bit).
    assert_eq!(o.read32(0xFFFF_FF08) & 0xFF, 0x02, "DVCR word read reflects the RMW");
    // A second byte write to a different byte of the same word leaves byte 0.
    o.write8(0xFFFF_FF08, 0x00); // high byte of DVCR (already 0)
    assert_eq!(o.divu.dvcr & 0xFF, 0x02, "low byte untouched by the high-byte write");
}

#[test]
fn write16_routes_the_whole_halfword_to_the_wdt_guard() {
    // WTCSR (0xFFFFFE80) requires a 16-bit guarded write (high byte = key).
    // OnChip::write16 must hand the WDT the full halfword rather than split it
    // into two key-less byte writes (which the WDT would reject).
    let mut o = OnChip::new();
    // 0xA5 is the WTCSR write key; low byte programs TME + φ/2.
    o.write16(0xFFFF_FE80, 0xA520);
    assert_eq!(o.wdt.wtcsr & 0x20, 0x20, "TME accepted via the guarded write");

    // A key-less byte write to the same register is ignored by the guard.
    let before = o.wdt.wtcsr;
    o.write8(0xFFFF_FE80, 0x00);
    assert_eq!(o.wdt.wtcsr, before, "key-less byte write rejected");
}

#[test]
fn intc_vcr_byte_access_round_trips_through_the_helper() {
    // VCRC sits in the IPRB block at offset 0x06/0x07; exercise the byte
    // halves of intc_read8/intc_write8 directly through the address map.
    let mut o = OnChip::new();
    o.write8(0xFFFF_FE66, 0x12); // VCRC high byte
    o.write8(0xFFFF_FE67, 0x34); // VCRC low byte
    assert_eq!(o.intc.vcrc, 0x1234, "VCRC assembled from two byte writes");
    assert_eq!(o.read8(0xFFFF_FE66), 0x12);
    assert_eq!(o.read8(0xFFFF_FE67), 0x34);
}

#[test]
fn intc_ipra_block_icr_and_vcrwdt_byte_access() {
    // The IPRA block (0xFFFFFEE0..) carries ICR/IPRA/VCRWDT. Exercise the
    // ipra_block=true arm of the byte helpers.
    let mut o = OnChip::new();
    o.write16(0xFFFF_FEE0, 0xAA55); // ICR
    o.write16(0xFFFF_FEE4, 0x00C3); // VCRWDT
    assert_eq!(o.intc.icr, 0xAA55, "ICR round-trips");
    assert_eq!(o.intc.vcrwdt, 0x00C3, "VCRWDT round-trips");
    assert_eq!(o.read8(0xFFFF_FEE0), 0xAA, "ICR high byte");
    assert_eq!(o.read8(0xFFFF_FEE5), 0xC3, "VCRWDT low byte");
}
