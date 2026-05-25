//! VDP1 register/VRAM/framebuffer stub reachable through the Saturn
//! bus (M4 task #3).
//!
//! There is no VDP1 plotter yet (M5). These tests only confirm that
//! the bus dispatch routes the three VDP1 windows correctly, that they
//! don't collide with neighbouring B-bus regions (CD-block, VDP2), and
//! that the one modeled behaviour — the PTMR→EDSR draw-end handshake —
//! is reachable from the CPU side.

use saturn::Saturn;
use saturn::vdp1::{FB_BASE, FB_END, REGS_BASE, VRAM_BASE, VRAM_END};
use sh2::bus::{AccessKind, Bus};

#[test]
fn vram_and_framebuffer_round_trip_through_the_bus() {
    let mut sat = Saturn::with_blank_bios();
    sat.bus.write32(VRAM_BASE + 0x200, 0xDEAD_BEEF, AccessKind::Data);
    sat.bus.write16(FB_BASE + 0x40, 0x7FFF, AccessKind::Data);
    let (vram, _) = sat.bus.read32(VRAM_BASE + 0x200, AccessKind::Data);
    let (fb, _) = sat.bus.read16(FB_BASE + 0x40, AccessKind::Data);
    assert_eq!(vram, 0xDEAD_BEEF);
    assert_eq!(fb, 0x7FFF);
}

#[test]
fn plot_trigger_acks_draw_end_via_the_bus() {
    let mut sat = Saturn::with_blank_bios();
    // EDSR (0x10) starts with no end flag.
    let (edsr, _) = sat.bus.read16(REGS_BASE + 0x10, AccessKind::Data);
    assert_eq!(edsr & 0x0002, 0);
    // Write PTMR (0x04) to kick a plot; EDSR.CEF must latch so BIOS
    // polling on the draw-end flag completes.
    sat.bus.write16(REGS_BASE + 0x04, 0x0001, AccessKind::Data);
    let (edsr, _) = sat.bus.read16(REGS_BASE + 0x10, AccessKind::Data);
    assert_eq!(edsr & 0x0002, 0x0002, "PTMR must set EDSR.CEF");
}

#[test]
fn status_register_modr_reports_version() {
    let mut sat = Saturn::with_blank_bios();
    let (modr, _) = sat.bus.read16(REGS_BASE + 0x16, AccessKind::Data);
    assert_eq!(modr, 0x1000);
}

#[test]
fn vdp1_does_not_collide_with_cd_block_or_vdp2() {
    let mut sat = Saturn::with_blank_bios();
    sat.bus.write16(0x0589_0018, 0xEEEE, AccessKind::Data); // CD CR1
    sat.bus.write32(0x05E0_0000 + 0x100, 0xCCCC_DDDD, AccessKind::Data); // VDP2 VRAM
    sat.bus.write32(VRAM_BASE + 0x100, 0xAAAA_BBBB, AccessKind::Data); // VDP1 VRAM

    let (cd_cr1, _) = sat.bus.read16(0x0589_0018, AccessKind::Data);
    let (vdp2_vram, _) = sat.bus.read32(0x05E0_0000 + 0x100, AccessKind::Data);
    let (vdp1_vram, _) = sat.bus.read32(VRAM_BASE + 0x100, AccessKind::Data);
    assert_eq!(cd_cr1, 0xEEEE);
    assert_eq!(vdp2_vram, 0xCCCC_DDDD);
    assert_eq!(vdp1_vram, 0xAAAA_BBBB);
}

#[test]
fn window_boundaries_route_to_the_right_region() {
    let mut sat = Saturn::with_blank_bios();
    // Last byte of VRAM and first of the framebuffer are distinct.
    sat.bus.write8(VRAM_END, 0x11, AccessKind::Data);
    sat.bus.write8(FB_BASE, 0x22, AccessKind::Data);
    sat.bus.write8(FB_END, 0x33, AccessKind::Data);
    let (a, _) = sat.bus.read8(VRAM_END, AccessKind::Data);
    let (b, _) = sat.bus.read8(FB_BASE, AccessKind::Data);
    let (c, _) = sat.bus.read8(FB_END, AccessKind::Data);
    assert_eq!((a, b, c), (0x11, 0x22, 0x33));
}
