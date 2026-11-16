//! On-chip DMA controller (DMAC) end-to-end tests.
//!
//! The DMAC runs inside [`sh2::Cpu::step`]: once a channel is enabled
//! (DMAOR.DME + CHCR.DE, TE clear) the next instruction step drains the
//! transfer through the external bus. These tests set the channel registers
//! up via the on-chip register window, then step a NOP to let the transfer
//! run, and check the copied bytes / the completion flag / the interrupt.

use sh2::Cpu;
use sh2::harness::MemBus;
use sh2::{InterruptSource, OnChip};

const PC0: u32 = 0x0000_1000;

// DMAC register addresses (channel 0).
const SAR0: u32 = 0xFFFF_FF80;
const DAR0: u32 = 0xFFFF_FF84;
const TCR0: u32 = 0xFFFF_FF88;
const CHCR0: u32 = 0xFFFF_FF8C;
const DMAOR: u32 = 0xFFFF_FFB0;

/// Build a CPU sitting on a one-NOP program at `PC0`, plus its bus.
fn cpu_with_nop() -> (Cpu, MemBus) {
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[0x0009]); // NOP
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    (cpu, bus)
}

/// CHCR helper: destination mode, source mode (0 fixed/1 inc/2 dec), transfer
/// size (0 byte/1 word/2 long/3 block), plus DE and optionally IE.
fn chcr(dm: u32, sm: u32, ts: u32, ie: bool) -> u32 {
    (dm << 14) | (sm << 12) | (ts << 10) | ((ie as u32) << 2) | 1
}

#[test]
fn dmac_copies_a_longword_block_through_the_bus() {
    let (mut cpu, mut bus) = cpu_with_nop();
    // Source block at 0x3000, destination at 0x4000.
    for i in 0..4u32 {
        bus.write_u32(0x3000 + i * 4, 0x1111_0000 + i);
    }
    cpu.onchip.write32(SAR0, 0x3000);
    cpu.onchip.write32(DAR0, 0x4000);
    cpu.onchip.write32(TCR0, 4); // 4 longwords
    cpu.onchip.write32(CHCR0, chcr(1, 1, 2, false)); // inc/inc, long
    cpu.onchip.write32(DMAOR, 1); // DME

    cpu.step(&mut bus); // the NOP step drains the DMA

    for i in 0..4u32 {
        let (v, _) = sh2::bus::Bus::read32(&mut bus, 0x4000 + i * 4, sh2::bus::AccessKind::Data);
        assert_eq!(v, 0x1111_0000 + i, "longword {i} copied");
    }
    // Completion: TE latched, TCR zeroed, SAR/DAR advanced past the block.
    assert_eq!(cpu.onchip.dmac.channels[0].chcr & 0b10, 0b10, "TE set");
    assert_eq!(cpu.onchip.dmac.channels[0].tcr, 0, "TCR cleared");
    assert_eq!(cpu.onchip.dmac.channels[0].sar, 0x3010);
    assert_eq!(cpu.onchip.dmac.channels[0].dar, 0x4010);
}

#[test]
fn dmac_byte_size_with_fixed_destination() {
    let (mut cpu, mut bus) = cpu_with_nop();
    for i in 0..8u32 {
        bus.as_mut_slice()[(0x3000 + i) as usize] = (0xA0 + i) as u8;
    }
    cpu.onchip.write32(SAR0, 0x3000);
    cpu.onchip.write32(DAR0, 0x5000);
    cpu.onchip.write32(TCR0, 8);
    cpu.onchip.write32(CHCR0, chcr(0, 1, 0, false)); // dest fixed, src inc, byte
    cpu.onchip.write32(DMAOR, 1);

    cpu.step(&mut bus);

    // Fixed destination → only the last byte written remains at 0x5000.
    assert_eq!(bus.as_slice()[0x5000], 0xA7, "last source byte landed");
    assert_eq!(cpu.onchip.dmac.channels[0].sar, 0x3008, "source advanced");
    assert_eq!(cpu.onchip.dmac.channels[0].dar, 0x5000, "dest fixed");
}

#[test]
fn dmac_decrement_mode_walks_backwards() {
    let (mut cpu, mut bus) = cpu_with_nop();
    bus.write_u16(0x3000, 0xCAFE);
    bus.write_u16(0x3002, 0xBEEF);
    cpu.onchip.write32(SAR0, 0x3000);
    cpu.onchip.write32(DAR0, 0x6004);
    cpu.onchip.write32(TCR0, 2);
    cpu.onchip.write32(CHCR0, chcr(2, 1, 1, false)); // dest dec, src inc, word
    cpu.onchip.write32(DMAOR, 1);

    cpu.step(&mut bus);

    assert_eq!(bus.as_slice()[0x6004], 0xCA, "first word at start dest");
    assert_eq!(bus.as_slice()[0x6002], 0xBE, "second word one word lower");
    assert_eq!(
        cpu.onchip.dmac.channels[0].dar, 0x6000,
        "dest decremented twice"
    );
}

#[test]
fn dmac_completion_raises_channel_interrupt_when_ie_set() {
    let (mut cpu, mut bus) = cpu_with_nop();
    // IPRA DMAC priority (bits 11..8) = 9 so the source can be taken.
    cpu.onchip.intc.ipra = 0x0900;
    bus.write_u32(0x3000, 0xDEAD_BEEF);
    cpu.onchip.write32(SAR0, 0x3000);
    cpu.onchip.write32(DAR0, 0x4000);
    cpu.onchip.write32(TCR0, 1);
    cpu.onchip.write32(CHCR0, chcr(1, 1, 2, true)); // IE set
    cpu.onchip.write32(DMAOR, 1);

    cpu.step(&mut bus);

    let pending = cpu.onchip.intc.next_pending(0);
    assert_eq!(
        pending,
        Some((InterruptSource::DmacCh0, 9)),
        "transfer-end raises the DMAC channel-0 interrupt"
    );
}

#[test]
fn dmac_does_not_run_while_master_disabled() {
    let (mut cpu, mut bus) = cpu_with_nop();
    bus.write_u32(0x3000, 0x1234_5678);
    cpu.onchip.write32(SAR0, 0x3000);
    cpu.onchip.write32(DAR0, 0x4000);
    cpu.onchip.write32(TCR0, 1);
    cpu.onchip.write32(CHCR0, chcr(1, 1, 2, false));
    cpu.onchip.write32(DMAOR, 0); // DME clear → no transfer

    cpu.step(&mut bus);

    let (v, _) = sh2::bus::Bus::read32(&mut bus, 0x4000, sh2::bus::AccessKind::Data);
    assert_eq!(v, 0, "no transfer with the master enable off");
    assert_eq!(cpu.onchip.dmac.channels[0].chcr & 0b10, 0, "TE not set");
}

#[test]
fn dmac_does_not_run_while_a_dmaor_fault_bit_is_set() {
    // DMAOR run condition is `& 0x07 == 0x01` (DME set, NMIF + AE clear), not
    // just DME — a stuck AE (bit 2) or NMIF (bit 1) blocks the channel
    // (Mednafen DMA_RunCond).
    let (mut cpu, mut bus) = cpu_with_nop();
    bus.write_u32(0x3000, 0x1234_5678);
    cpu.onchip.write32(SAR0, 0x3000);
    cpu.onchip.write32(DAR0, 0x4000);
    cpu.onchip.write32(TCR0, 1);
    cpu.onchip.write32(CHCR0, chcr(1, 1, 2, false));
    cpu.onchip.write32(DMAOR, 1); // DME
    // AE is hardware-set only (software can't write a 1 there), so model a
    // fault latched by an earlier misaligned transfer.
    cpu.onchip.dmac.latch_ae();

    cpu.step(&mut bus);

    let (v, _) = sh2::bus::Bus::read32(&mut bus, 0x4000, sh2::bus::AccessKind::Data);
    assert_eq!(v, 0, "AE fault bit blocks the transfer");
    assert_eq!(cpu.onchip.dmac.channels[0].chcr & 0b10, 0, "TE not set");
}

#[test]
fn dmac_address_mode_3_decrements_like_mode_2() {
    // SM/DM = 3 is "setting prohibited" but the hardware decrements like mode 2
    // (Mednafen `ainc` table maps both 2 and 3 to −unit), rather than refusing
    // to start.
    let (mut cpu, mut bus) = cpu_with_nop();
    bus.write_u16(0x3000, 0xCAFE);
    bus.write_u16(0x3002, 0xBEEF);
    cpu.onchip.write32(SAR0, 0x3000);
    cpu.onchip.write32(DAR0, 0x6004);
    cpu.onchip.write32(TCR0, 2);
    cpu.onchip.write32(CHCR0, chcr(3, 1, 1, false)); // dest mode 3 (→dec), src inc, word
    cpu.onchip.write32(DMAOR, 1);

    cpu.step(&mut bus);

    assert_eq!(bus.as_slice()[0x6004], 0xCA, "first word at start dest");
    assert_eq!(bus.as_slice()[0x6002], 0xBE, "second word one word lower");
    assert_eq!(
        cpu.onchip.dmac.channels[0].dar, 0x6000,
        "mode 3 decremented twice"
    );
    assert_eq!(
        cpu.onchip.dmac.channels[0].chcr & 0b10,
        0b10,
        "completed → TE set"
    );
}

#[test]
fn dmac_misaligned_word_address_sets_ae_and_aborts() {
    // An odd SAR for a word transfer is an address error: the masked access
    // still happens for that unit, AE (DMAOR bit 2) latches, and the channel
    // aborts with TE clear and TCR non-zero (Mednafen PEX_DMAADDR path).
    let (mut cpu, mut bus) = cpu_with_nop();
    bus.write_u16(0x3000, 0xAA55);
    cpu.onchip.write32(SAR0, 0x3001); // odd → misaligned for a word access
    cpu.onchip.write32(DAR0, 0x4000);
    cpu.onchip.write32(TCR0, 4);
    cpu.onchip.write32(CHCR0, chcr(1, 1, 1, false)); // word
    cpu.onchip.write32(DMAOR, 1);

    cpu.step(&mut bus);

    assert_eq!(cpu.onchip.dmac.dmaor & 0b100, 0b100, "AE fault bit latched");
    assert_eq!(
        cpu.onchip.dmac.channels[0].chcr & 0b10,
        0,
        "TE not set (aborted)"
    );
    assert_eq!(cpu.onchip.dmac.channels[0].tcr, 3, "aborted after one unit");
}

#[test]
fn dmac_block_mode_fixed_destination_overwrites() {
    // 16-byte block (TS=3) with a fixed destination writes all four longwords
    // to the same DAR (DM=0 → ainc 0 per longword), so only the last survives;
    // the source advances by 16 regardless of SM (Mednafen block path).
    let (mut cpu, mut bus) = cpu_with_nop();
    for i in 0..4u32 {
        bus.write_u32(0x3000 + i * 4, 0x4000_0000 + i);
    }
    cpu.onchip.write32(SAR0, 0x3000);
    cpu.onchip.write32(DAR0, 0x4000);
    cpu.onchip.write32(TCR0, 4); // four longwords = one block
    cpu.onchip.write32(CHCR0, chcr(0, 0, 3, false)); // dest fixed, src fixed, block
    cpu.onchip.write32(DMAOR, 1);

    cpu.step(&mut bus);

    let (v, _) = sh2::bus::Bus::read32(&mut bus, 0x4000, sh2::bus::AccessKind::Data);
    assert_eq!(
        v, 0x4000_0003,
        "only the last longword survives at the fixed dest"
    );
    assert_eq!(
        cpu.onchip.dmac.channels[0].sar, 0x3010,
        "source advanced by 16"
    );
    assert_eq!(cpu.onchip.dmac.channels[0].dar, 0x4000, "dest fixed");
    assert_eq!(cpu.onchip.dmac.channels[0].tcr, 0, "block consumed");
    assert_eq!(cpu.onchip.dmac.channels[0].chcr & 0b10, 0b10, "TE set");
}

#[test]
fn onchip_owns_dmac_window() {
    assert!(OnChip::owns(CHCR0));
    assert!(OnChip::owns(DMAOR));
}

#[test]
fn chcr_te_clears_only_by_write_zero_after_a_read() {
    // CHCR.TE is hardware-set and clear-only (Mednafen `CHCRM`): a write of 0
    // clears it only once a CHCR read has observed it; a write of 1 never
    // sets it. The transfer-end interrupt request follows TE.
    let (mut cpu, mut bus) = cpu_with_nop();
    cpu.onchip.intc.ipra = 0x0900;
    bus.write_u32(0x3000, 0xDEAD_BEEF);
    cpu.onchip.write32(SAR0, 0x3000);
    cpu.onchip.write32(DAR0, 0x4000);
    cpu.onchip.write32(TCR0, 1);
    cpu.onchip.write32(CHCR0, chcr(1, 1, 2, true));
    cpu.onchip.write32(DMAOR, 1);
    cpu.step(&mut bus);
    assert_eq!(cpu.onchip.dmac.channels[0].chcr & 2, 2, "TE latched");

    // Write 0 to TE (keeping DE/IE) without a prior read: TE survives.
    cpu.onchip.write32(CHCR0, chcr(1, 1, 2, true));
    assert_eq!(cpu.onchip.dmac.channels[0].chcr & 2, 2, "unread TE kept");
    // Read CHCR (observes TE), then write 0: TE clears, the IRQ drops.
    let v = cpu.onchip.read32(CHCR0);
    assert_eq!(v & 2, 2);
    cpu.onchip.write32(CHCR0, v & !(2 | 1)); // clear TE and DE
    assert_eq!(cpu.onchip.dmac.channels[0].chcr & 2, 0, "TE cleared");
    cpu.onchip.refresh_interrupts();
    assert_eq!(cpu.onchip.intc.next_pending(0), None, "IRQ dropped with TE");

    // Software can never set TE.
    cpu.onchip.write32(CHCR0, 0b10);
    assert_eq!(cpu.onchip.dmac.channels[0].chcr & 2, 0, "TE not settable");
}

#[test]
fn dmaor_ae_latched_by_a_misaligned_transfer_needs_read_then_write_zero() {
    let (mut cpu, mut bus) = cpu_with_nop();
    cpu.onchip.write32(SAR0, 0x3001); // misaligned for a longword transfer
    cpu.onchip.write32(DAR0, 0x4000);
    cpu.onchip.write32(TCR0, 1);
    cpu.onchip.write32(CHCR0, chcr(1, 1, 2, false));
    cpu.onchip.write32(DMAOR, 1);
    cpu.step(&mut bus);
    assert_eq!(cpu.onchip.dmac.dmaor & 4, 4, "AE latched");
    cpu.onchip.write32(DMAOR, 1);
    assert_eq!(cpu.onchip.dmac.dmaor & 4, 4, "unread AE survives write-0");
    let _ = cpu.onchip.read32(DMAOR);
    cpu.onchip.write32(DMAOR, 1);
    assert_eq!(cpu.onchip.dmac.dmaor, 1, "read-then-write-0 clears AE");
    // A byte read of DMAOR also counts as the observing read.
    cpu.onchip.dmac.latch_ae();
    let _ = cpu.onchip.read8(DMAOR + 3);
    cpu.onchip.write32(DMAOR, 1);
    assert_eq!(cpu.onchip.dmac.dmaor & 4, 0, "byte read observes AE too");
}

#[test]
fn dmac_channel_1_runs_and_both_channels_complete() {
    // Channel 1 registers sit 0x10 above channel 0.
    let (mut cpu, mut bus) = cpu_with_nop();
    bus.write_u32(0x3000, 0x0102_0304);
    bus.write_u32(0x3100, 0x0506_0708);
    cpu.onchip.write32(SAR0, 0x3000);
    cpu.onchip.write32(DAR0, 0x4000);
    cpu.onchip.write32(TCR0, 1);
    cpu.onchip.write32(CHCR0, chcr(1, 1, 2, false));
    cpu.onchip.write32(SAR0 + 0x10, 0x3100);
    cpu.onchip.write32(DAR0 + 0x10, 0x4100);
    cpu.onchip.write32(TCR0 + 0x10, 1);
    cpu.onchip.write32(CHCR0 + 0x10, chcr(1, 1, 2, false));
    cpu.onchip.write32(DMAOR, 1);
    cpu.step(&mut bus);
    let r = |bus: &mut MemBus, a| sh2::bus::Bus::read32(bus, a, sh2::bus::AccessKind::Data).0;
    assert_eq!(r(&mut bus, 0x4000), 0x0102_0304);
    assert_eq!(r(&mut bus, 0x4100), 0x0506_0708);
    assert_eq!(cpu.onchip.read32(CHCR0 + 0x10) & 2, 2, "ch1 TE");
    assert_eq!(cpu.onchip.read32(SAR0 + 0x10), 0x3104, "ch1 SAR advanced");
    assert_eq!(cpu.onchip.read32(DAR0 + 0x10), 0x4104, "ch1 DAR advanced");
    assert_eq!(cpu.onchip.read32(TCR0 + 0x10), 0, "ch1 TCR drained");
}
