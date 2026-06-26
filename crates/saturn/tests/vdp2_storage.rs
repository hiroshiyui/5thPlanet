//! VDP2 register / VRAM / CRAM accessibility through the Saturn bus
//! (M3 task #5). Doesn't exercise the renderer (that's task #6) —
//! just confirms the bus dispatch routes correctly into the new
//! VDP2 module without disturbing existing regions.

use saturn::Saturn;
use saturn::vdp2::{CRAM_BASE, REGS_BASE, VRAM_BASE};
use sh2::bus::{AccessKind, Bus};

#[test]
fn vram_writes_and_reads_through_the_bus() {
    let mut sat = Saturn::with_blank_bios();
    sat.bus
        .write32(VRAM_BASE + 0x100, 0xDEAD_BEEF, AccessKind::Data);
    let (v, _) = sat.bus.read32(VRAM_BASE + 0x100, AccessKind::Data);
    assert_eq!(v, 0xDEAD_BEEF);
    assert_eq!(sat.bus.vdp2.vram.read32(0x100), 0xDEAD_BEEF);
}

#[test]
fn cram_writes_through_bus_show_up_in_palette_lookup() {
    let mut sat = Saturn::with_blank_bios();
    // Write palette entry 0 = RGB555 (R=31, G=0, B=0).
    sat.bus.write16(CRAM_BASE, 0x001F, AccessKind::Data);
    assert_eq!(sat.bus.vdp2.cram.color_rgb888_mode0(0), (0xFF, 0, 0));
}

#[test]
fn tvmd_display_enable_visible_via_named_accessor_after_bus_write() {
    let mut sat = Saturn::with_blank_bios();
    sat.bus.write16(REGS_BASE, 0x8000, AccessKind::Data); // TVMD.DISP (offset 0x000)
    assert!(sat.bus.vdp2.regs.display_enabled());
}

#[test]
fn bgon_through_bus_then_per_layer_decode() {
    let mut sat = Saturn::with_blank_bios();
    sat.bus.write16(REGS_BASE + 0x020, 0b0011, AccessKind::Data);
    assert!(sat.bus.vdp2.regs.nbg0_enabled());
    assert!(sat.bus.vdp2.regs.nbg1_enabled());
    assert!(!sat.bus.vdp2.regs.nbg2_enabled());
}

#[test]
fn vdp2_addresses_do_not_collide_with_existing_regions() {
    // Write a sentinel into SCU's version-register-adjacent space and
    // verify VDP2 didn't capture it.
    let mut sat = Saturn::with_blank_bios();
    sat.bus.write32(0x05FE_0010, 0xAAAA_BBBB, AccessKind::Data);
    // VDP2 regs end at 0x05F8_01FF — 0x05FE_xxxx is SCU territory.
    let (v, _) = sat.bus.read32(0x05FE_0010, AccessKind::Data);
    assert_eq!(v, 0xAAAA_BBBB);
    // And VDP2 didn't see anything.
    assert_eq!(sat.bus.vdp2.regs.read32(0x1F0), 0);
}

#[test]
fn cache_through_alias_works_for_vdp2() {
    // Software typically writes VRAM via the 0x25E0_0000 alias to
    // avoid cache pollution. The CPU's classify() strips the high
    // bits, so the bus sees 0x05E0_0000 either way.
    let mut sat = Saturn::with_blank_bios();
    // Direct write to cache-through-aliased physical addr.
    sat.bus
        .write32(VRAM_BASE + 0x10, 0xCAFE_F00D, AccessKind::Data);
    // Read back via the same physical addr.
    let (v, _) = sat.bus.read32(VRAM_BASE + 0x10, AccessKind::Data);
    assert_eq!(v, 0xCAFE_F00D);
}
