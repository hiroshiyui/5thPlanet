//! VDP1 register/VRAM/framebuffer stub reachable through the Saturn
//! bus (M4 task #3).
//!
//! There is no VDP1 plotter yet (M5). These tests only confirm that
//! the bus dispatch routes the three VDP1 windows correctly, that they
//! don't collide with neighbouring B-bus regions (CD-block, VDP2), and
//! that the one modeled behaviour — the PTMR→EDSR draw-end handshake —
//! is reachable from the CPU side.

use saturn::Saturn;
use saturn::Vdp1;
use saturn::vdp1::{FB_BASE, FB_END, REGS_BASE, VRAM_BASE, VRAM_END};
use sh2::bus::{AccessKind, Bus};

#[test]
fn vram_and_framebuffer_round_trip_through_the_bus() {
    let mut sat = Saturn::with_blank_bios();
    sat.bus
        .write32(VRAM_BASE + 0x200, 0xDEAD_BEEF, AccessKind::Data);
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
    // Write PTMR (0x04) to kick a plot. The draw is timed, so EDSR.CEF stays
    // clear while it is in progress and latches once the duration elapses.
    sat.bus.write16(REGS_BASE + 0x04, 0x0001, AccessKind::Data);
    let (edsr, _) = sat.bus.read16(REGS_BASE + 0x10, AccessKind::Data);
    assert_eq!(edsr & 0x0002, 0, "draw still in progress");
    sat.bus.vdp1.settle(u64::MAX); // advance past the modelled draw duration
    let (edsr, _) = sat.bus.read16(REGS_BASE + 0x10, AccessKind::Data);
    assert_eq!(edsr & 0x0002, 0x0002, "EDSR.CEF latches at draw-end");
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
    sat.bus
        .write32(0x05E0_0000 + 0x100, 0xCCCC_DDDD, AccessKind::Data); // VDP2 VRAM
    sat.bus
        .write32(VRAM_BASE + 0x100, 0xAAAA_BBBB, AccessKind::Data); // VDP1 VRAM

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

// ---------------------------------------------------------------------
// Plotter — command-list walker + primitive rasterisation.
//
// Commands are 0x20-byte (8 × u32) entries written into VRAM at
// `pos * 0x20`; field layout matches `vdp1::command::Command`.
// ---------------------------------------------------------------------

/// Pack two halfwords into one big-endian command word.
fn w(hi: u16, lo: u16) -> u32 {
    ((hi as u32) << 16) | lo as u32
}

/// Write the eight words of one command at table position `pos`.
fn put(v: &mut Vdp1, pos: u32, words: [u32; 8]) {
    let base = pos * 0x20;
    for (i, word) in words.iter().enumerate() {
        v.vram.write32(base + i as u32 * 4, *word);
    }
}

/// CMDCTRL=0x8000 — list terminator.
const END: [u32; 8] = [0x8000_0000, 0, 0, 0, 0, 0, 0, 0];

#[test]
fn polygon_fills_a_solid_rectangle() {
    let mut v = Vdp1::new();
    // Type 4 polygon, jump-next, plain replace with ECD set (→ the
    // no-transparency Poly writer), colour 0x001F, rectangle (10,10)-(20,20).
    put(
        &mut v,
        0,
        [
            w(0x0004, 0x0000), // CMDCTRL (type 4) | CMDLINK
            w(0x0080, 0x001F), // CMDPMOD (ECD) | CMDCOLR
            w(0x0000, 0x0000), // CMDSRCA | CMDSIZE
            w(10, 10),         // XA,YA
            w(20, 10),         // XB,YB
            w(20, 20),         // XC,YC
            w(10, 20),         // XD,YD
            0,                 // CMDGRDA
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    assert_eq!(v.fb.pixel(15, 15), 0x001F, "interior filled");
    assert_eq!(v.fb.pixel(10, 10), 0x001F, "top-left corner filled");
    assert_eq!(v.fb.pixel(5, 5), 0, "outside the polygon");
    assert_eq!(v.fb.pixel(25, 25), 0, "outside the polygon");
    assert_eq!(v.regs.read16(0x10) & 0x0002, 0x0002, "EDSR.CEF latched");
}

#[test]
fn normal_sprite_blits_16bpp_character() {
    let mut v = Vdp1::new();
    // 8×8 character of RGB555 colour 0x4210 at VRAM byte 0x1000.
    for i in 0..64u32 {
        v.vram.write16(0x1000 + i * 2, 0x4210);
    }
    let srca = (0x1000u32 / 8) as u16;
    put(
        &mut v,
        0,
        [
            w(0x0000, 0x0000), // type 0 normal sprite, jump-next
            w(0x0028, 0x0000), // CMDPMOD: 16bpp RGB colour mode
            w(srca, 0x0108),   // CMDSRCA | CMDSIZE (x=8, y=8)
            w(5, 5),           // XA,YA
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    assert_eq!(v.fb.pixel(5, 5), 0x4210, "sprite origin");
    assert_eq!(v.fb.pixel(12, 12), 0x4210, "sprite far corner (8×8)");
    assert_eq!(v.fb.pixel(4, 4), 0, "above-left of sprite");
    assert_eq!(v.fb.pixel(13, 13), 0, "past the sprite");
}

#[test]
fn normal_sprite_4bpp_adds_colour_bank() {
    let mut v = Vdp1::new();
    // 4bpp character: every byte 0x33 → both nibbles select colour 3.
    for i in 0..(8 * 8 / 2) as u32 {
        v.vram.write8(0x800 + i, 0x33);
    }
    let srca = (0x800u32 / 8) as u16;
    put(
        &mut v,
        0,
        [
            w(0x0000, 0x0000), // normal sprite
            w(0x00C0, 0x0050), // CMDPMOD: SPD+ECD, mode 0 (4bpp); CMDCOLR bank 0x0050
            w(srca, 0x0108),   // 8×8
            w(3, 3),
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    // pixel = nibble(3) | (CMDCOLR & 0xFFF0) = 0x0050 | 3 = 0x0053.
    assert_eq!(v.fb.pixel(3, 3), 0x0053);
    assert_eq!(v.fb.pixel(10, 10), 0x0053);
    assert_eq!(v.fb.pixel(2, 2), 0, "outside the sprite");
}

#[test]
fn line_draws_a_horizontal_run() {
    let mut v = Vdp1::new();
    put(
        &mut v,
        0,
        [
            w(0x0006, 0x0000), // type 6 line
            w(0x0080, 0x03E0), // ECD → Poly writer, colour green
            0,
            w(2, 2), // XA,YA
            w(6, 2), // XB,YB
            0,
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    assert_eq!(v.fb.pixel(2, 2), 0x03E0, "line start");
    assert_eq!(v.fb.pixel(4, 2), 0x03E0, "line middle");
    assert_eq!(v.fb.pixel(6, 2), 0x03E0, "line end");
    assert_eq!(v.fb.pixel(4, 3), 0, "off the line");
}

#[test]
fn local_coordinates_offset_subsequent_primitives() {
    let mut v = Vdp1::new();
    // Type 0xA: set the local-coordinate origin to (100,50).
    put(&mut v, 0, [w(0x000A, 0), 0, 0, w(100, 50), 0, 0, 0, 0]);
    // Polygon (0,0)-(10,10) — drawn relative to the local origin.
    put(
        &mut v,
        1,
        [
            w(0x0004, 0),
            w(0x0080, 0x7C00),
            0,
            w(0, 0),
            w(10, 0),
            w(10, 10),
            w(0, 10),
            0,
        ],
    );
    put(&mut v, 2, END);
    v.process_list();

    assert_eq!(
        v.fb.pixel(105, 55),
        0x7C00,
        "drawn at local-offset position"
    );
    assert_eq!(v.fb.pixel(5, 5), 0, "the unshifted origin stays blank");
}

#[test]
fn skip_jump_mode_does_not_draw_its_primitive() {
    let mut v = Vdp1::new();
    // CMDCTRL 0x4004 — SKIP flag (0x4000) + type 4 polygon.
    put(
        &mut v,
        0,
        [
            w(0x4004, 0),
            w(0x0080, 0x001F),
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    assert_eq!(v.fb.pixel(15, 15), 0, "skipped command must not draw");
}

#[test]
fn end_bit_terminates_before_drawing_later_commands() {
    let mut v = Vdp1::new();
    put(&mut v, 0, END);
    // This polygon sits after the terminator and must never run.
    put(
        &mut v,
        1,
        [
            w(0x0004, 0),
            w(0x0080, 0x001F),
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    v.process_list();

    assert_eq!(v.fb.pixel(15, 15), 0, "list ended before this command");
}

#[test]
fn jump_assign_redirects_the_walker_over_an_intermediate_command() {
    let mut v = Vdp1::new();
    // cmd0: polygon at (10,10) that, after drawing, jumps to position 2
    // (CMDLINK = pos << 2 = 8), skipping cmd1.
    put(
        &mut v,
        0,
        [
            w(0x1004, 8), // jump-assign (0x1000) + type 4
            w(0x0080, 0x001F),
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    // cmd1: polygon at (40,40) — jumped over, must not draw.
    put(
        &mut v,
        1,
        [
            w(0x0004, 0),
            w(0x0080, 0x03E0),
            0,
            w(40, 40),
            w(50, 40),
            w(50, 50),
            w(40, 50),
            0,
        ],
    );
    put(&mut v, 2, END);
    v.process_list();

    assert_eq!(v.fb.pixel(15, 15), 0x001F, "cmd0 drew before jumping");
    assert_eq!(v.fb.pixel(45, 45), 0, "jumped-over cmd1 did not draw");
}

#[test]
fn ptmr_write_through_registers_triggers_the_plot() {
    let mut v = Vdp1::new();
    put(
        &mut v,
        0,
        [
            w(0x0004, 0),
            w(0x0080, 0x001F),
            0,
            w(30, 30),
            w(40, 30),
            w(40, 40),
            w(30, 40),
            0,
        ],
    );
    put(&mut v, 1, END);
    // Kick a one-shot plot: write PTMR with PTM = 0b01 ("draw by request"),
    // which renders immediately on the write. (PTM = 0b10 is automatic draw,
    // which plots at the frame change instead — see Vdp1::frame_change.)
    v.write16(REGS_BASE + 0x04, 0x0001);

    // The pixels are rendered immediately, but draw-end is timed: CEF stays
    // clear until the modelled draw duration elapses.
    assert_eq!(v.fb.pixel(35, 35), 0x001F, "PTMR write drove the plotter");
    assert!(v.is_drawing(), "draw is in progress, not instantaneous");
    assert_eq!(v.regs.read16(0x10) & 0x0002, 0, "CEF not yet latched");
    v.settle(u64::MAX);
    assert_eq!(
        v.regs.read16(0x10) & 0x0002,
        0x0002,
        "draw-end flag latched after the draw duration"
    );
}

#[test]
fn distorted_sprite_maps_a_texture_onto_a_skewed_quad() {
    let mut v = Vdp1::new();
    // 8×8 character, solid colour 0x6318, at VRAM byte 0x2000.
    for i in 0..64u32 {
        v.vram.write16(0x2000 + i * 2, 0x6318);
    }
    let srca = (0x2000u32 / 8) as u16;
    // A parallelogram skewed to the right as y increases.
    put(
        &mut v,
        0,
        [
            w(0x0002, 0x0000), // type 2 distorted sprite
            w(0x0028, 0x0000), // 16bpp RGB
            w(srca, 0x0108),   // 8×8 texture
            w(20, 20),         // A
            w(40, 20),         // B
            w(48, 40),         // C
            w(28, 40),         // D
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    // Interior of the quad receives the texture colour; a point clearly
    // outside it stays blank.
    assert_eq!(v.fb.pixel(30, 25), 0x6318, "inside the distorted quad");
    assert_eq!(v.fb.pixel(5, 5), 0, "outside the quad");
}

#[test]
fn erase_clears_the_ewrr_region_to_the_erase_colour() {
    let mut v = Vdp1::new();
    // Pre-dirty a pixel that lies inside the erase rectangle.
    v.fb.set_pixel(5, 5, 0x7FFF);
    // EWDR = fill colour; EWLR upper-left (0,0); EWRR lower-right such
    // that end_x = 2*8 = 16, end_y = 7+1 = 8.
    v.write16(REGS_BASE + 0x06, 0x1234); // EWDR
    v.write16(REGS_BASE + 0x08, 0x0000); // EWLR (X1=0, Y1=0)
    v.write16(REGS_BASE + 0x0A, (2 << 9) | 7); // EWRR (X3=2, Y3=7)
    // Empty list: erase runs, nothing is drawn over it.
    put(&mut v, 0, END);
    v.process_list();

    assert_eq!(v.fb.pixel(5, 5), 0x1234, "inside erase region cleared");
    assert_eq!(v.fb.pixel(15, 7), 0x1234, "far corner of region cleared");
    assert_eq!(v.fb.pixel(16, 8), 0, "just outside the region untouched");
}

/// Write the four per-vertex gouraud RGB555 colours at VRAM byte `base`
/// (so CMDGRDA = base >> 3) for vertices A, B, C, D.
fn put_gouraud(v: &mut Vdp1, base: u32, abcd: [u16; 4]) {
    for (i, c) in abcd.iter().enumerate() {
        v.vram.write16(base + i as u32 * 2, *c);
    }
}

#[test]
fn gouraud_uniform_offsets_every_channel() {
    let mut v = Vdp1::new();
    put_gouraud(&mut v, 0x100, [0x7FFF; 4]); // all vertices max → +15/channel
    // Type 4 polygon, CMDPMOD = gouraud (0x4) + SPD (0x40); base colour black.
    put(
        &mut v,
        0,
        [
            w(0x0004, 0x0000),
            w(0x0044, 0x0000), // gouraud + SPD, CMDCOLR = 0
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            w(0x0020, 0x0000), // CMDGRDA = 0x100 >> 3 = 0x20
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    // black (0,0,0) + (31-16) on each channel = (15,15,15) → 0x3DEF.
    assert_eq!(v.fb.pixel(15, 15), 0x3DEF, "uniform gouraud brightens");
}

#[test]
fn gouraud_interpolates_across_a_polygon() {
    let mut v = Vdp1::new();
    // Left vertices dark (correction 0 → −16), right vertices bright (+15).
    put_gouraud(&mut v, 0x100, [0x0000, 0x7FFF, 0x7FFF, 0x0000]);
    put(
        &mut v,
        0,
        [
            w(0x0004, 0x0000),
            w(0x0044, 0x0010), // gouraud + SPD, CMDCOLR red = 16
            0,
            w(10, 10), // A (left)
            w(40, 10), // B (right)
            w(40, 20), // C (right)
            w(10, 20), // D (left)
            w(0x0020, 0x0000),
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    // Base R = 16: left correction ≈ 0 → R ≈ 0, right ≈ 31. R must increase
    // left-to-right across the span.
    let left_r = v.fb.pixel(13, 15) & 0x1F;
    let right_r = v.fb.pixel(37, 15) & 0x1F;
    assert!(
        left_r < right_r,
        "gouraud R gradient: left {left_r} should be < right {right_r}"
    );
}

#[test]
fn gouraud_applies_to_a_normal_sprite() {
    let mut v = Vdp1::new();
    // 8×8 character of black (0x0000) at VRAM byte 0x2000.
    for i in 0..64u32 {
        v.vram.write16(0x2000 + i * 2, 0x0000);
    }
    put_gouraud(&mut v, 0x100, [0x7FFF; 4]);
    let srca = (0x2000u32 / 8) as u16;
    put(
        &mut v,
        0,
        [
            w(0x0000, 0x0000), // normal sprite
            w(0x006C, 0x0000), // 16bpp (0x28) + gouraud (0x4) + SPD (0x40)
            w(srca, 0x0108),   // 8×8
            w(5, 5),
            0,
            0,
            0,
            w(0x0020, 0x0000), // CMDGRDA = 0x20
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    assert_eq!(v.fb.pixel(8, 8), 0x3DEF, "normal-sprite gouraud brightens");
}

#[test]
fn draw_end_flag_pops_once_per_plot() {
    let mut v = Vdp1::new();
    put(&mut v, 0, END);
    assert!(!v.take_draw_end(), "no plot yet");
    v.process_list();
    assert!(v.take_draw_end(), "plot finished — draw-end pending");
    assert!(!v.take_draw_end(), "draw-end is consumed on read");
}

#[test]
fn plot_raises_the_scu_sprite_draw_end_interrupt() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    // Stage an (empty) command list and kick the plot through the bus.
    sat.bus.write32(VRAM_BASE, 0x8000_0000, AccessKind::Data); // END
    sat.bus.write16(REGS_BASE + 0x04, 0x0001, AccessKind::Data); // PTMR: one-shot draw
    // The draw is timed; advance past its duration so it completes, then let
    // the aggregate drain the VDP1 draw-end into the SCU.
    sat.bus.vdp1.settle(u64::MAX);
    sat.debug_drain();
    // SCU IST bit 13 = SpriteDrawEnd (sticky until software clears it).
    assert_ne!(
        sat.bus.scu.ist & (1 << 13),
        0,
        "VDP1 draw-end must raise the SCU sprite-draw-end source"
    );
}

// ---------------------------------------------------------------------
// Additional plotter coverage: scaled sprites, polyline, clipping,
// flips, the remaining colour modes, end-code/mesh/colour-calc, MSBON,
// call/return walking, and illegal-command termination.
// ---------------------------------------------------------------------

/// Fill an 8×8 RGB555 character at VRAM byte `base` with a per-column
/// horizontal gradient (column c → colour `c+1`), so h-flip is observable
/// (texel U direction is mapped to the column). Returns CMDSRCA (base>>3).
fn put_hgradient_char(v: &mut Vdp1, base: u32) -> u16 {
    for row in 0..8u32 {
        for col in 0..8u32 {
            v.vram
                .write16(base + (row * 8 + col) * 2, (col + 1) as u16);
        }
    }
    (base / 8) as u16
}

#[test]
fn scaled_sprite_zooms_a_character_to_an_explicit_rect() {
    let mut v = Vdp1::new();
    // 8×8 solid character colour 0x1234 at VRAM byte 0x3000.
    for i in 0..64u32 {
        v.vram.write16(0x3000 + i * 2, 0x1234);
    }
    let srca = (0x3000u32 / 8) as u16;
    // Type 1 scaled sprite with zoom==0 path: the destination rectangle is
    // given by (XA,YA) top-left and (XC,YC) bottom-right (CMDXC/YC). Stretch
    // the 8×8 texture across (10,10)-(40,30): width 31, height 21.
    put(
        &mut v,
        0,
        [
            w(0x0001, 0x0000), // type 1 scaled sprite, zoom field = 0
            w(0x0028, 0x0000), // 16bpp RGB
            w(srca, 0x0108),   // 8×8 texture
            w(10, 10),         // XA,YA (top-left)
            0,                 // XB,YB unused with zoom==0
            w(40, 30),         // XC,YC (bottom-right)
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    // Every interior dot of the stretched rect samples the solid texture.
    assert_eq!(v.fb.pixel(10, 10), 0x1234, "scaled top-left");
    assert_eq!(v.fb.pixel(25, 20), 0x1234, "scaled interior");
    assert_eq!(v.fb.pixel(40, 30), 0x1234, "scaled bottom-right");
    assert_eq!(v.fb.pixel(9, 9), 0, "outside the scaled rect");
    assert_eq!(v.fb.pixel(41, 31), 0, "past the scaled rect");
}

#[test]
fn sprite_coordinates_are_signed_13_bit() {
    let mut v = Vdp1::new();
    for i in 0..64u32 {
        v.vram.write16(0x3000 + i * 2, 0x1234);
    }
    let srca = (0x3000u32 / 8) as u16;
    put(
        &mut v,
        0,
        [
            w(0x0000, 0x0000),
            w(0x0028, 0x0000),
            w(srca, 0x0108),
            w(0x1FFC, 10), // signed 13-bit x = -4
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    assert_eq!(v.fb.pixel(0, 10), 0x1234);
    assert_eq!(v.fb.pixel(3, 17), 0x1234);
    assert_eq!(v.fb.pixel(4, 10), 0);
}

#[test]
fn local_coordinates_are_signed_11_bit() {
    let mut v = Vdp1::new();
    for i in 0..64u32 {
        v.vram.write16(0x3000 + i * 2, 0x2468);
    }
    let srca = (0x3000u32 / 8) as u16;
    put(
        &mut v,
        0,
        [
            w(0x000A, 0x0000), // local-coordinate command
            0,
            0,
            w(0, 0x07F0), // signed 11-bit y = -16
            0,
            0,
            0,
            0,
        ],
    );
    put(
        &mut v,
        1,
        [
            w(0x0000, 0x0000),
            w(0x0028, 0x0000),
            w(srca, 0x0108),
            w(10, 20),
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v, 2, END);
    v.process_list();

    assert_eq!(v.fb.pixel(10, 4), 0x2468);
    assert_eq!(v.fb.pixel(17, 11), 0x2468);
}

#[test]
fn scaled_sprite_zoom_with_display_size_and_centre_anchor() {
    let mut v = Vdp1::new();
    for i in 0..64u32 {
        v.vram.write16(0x3000 + i * 2, 0x2468);
    }
    let srca = (0x3000u32 / 8) as u16;
    // zoom field = 0xA (CMDCTRL bits 11-8): "centre" anchor — the rect is
    // centred on (XA,YA), so the origin is shifted by (-w/2, -h/2). Display
    // size is (XB,YB) = (20,20). Centre at (100,100) → rect (90,90)-(110,110).
    put(
        &mut v,
        0,
        [
            w(0x0A01, 0x0000), // zoom=0xA, type 1
            w(0x0028, 0x0000), // 16bpp RGB
            w(srca, 0x0108),   // 8×8 texture
            w(100, 100),       // XA,YA = zoom centre
            w(20, 20),         // XB,YB = display width/height
            0,
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();

    assert_eq!(v.fb.pixel(100, 100), 0x2468, "centred rect covers the anchor");
    assert_eq!(v.fb.pixel(91, 91), 0x2468, "near the shifted top-left");
    assert_eq!(v.fb.pixel(80, 80), 0, "outside the centred rect");
}

#[test]
fn normal_sprite_horizontal_flip_reverses_columns() {
    let mut v = Vdp1::new();
    let srca = put_hgradient_char(&mut v, 0x4000);
    // No flip first to learn the orientation, then h-flip and compare.
    put(
        &mut v,
        0,
        [
            w(0x0000, 0x0000), // dir = 0 (no flip)
            w(0x0068, 0x0000), // 16bpp RGB (0x28) + SPD (0x40) so colour 0 isn't dropped
            w(srca, 0x0108),
            w(10, 10),
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    // Column 0 → colour 1 at x=10; column 7 → colour 8 at x=17.
    assert_eq!(v.fb.pixel(10, 10), 1, "no-flip left column = first texel");
    assert_eq!(v.fb.pixel(17, 10), 8, "no-flip right column = last texel");

    // Now h-flip (CMDCTRL bit 4 = 0x0010): texel U is read reversed, so the
    // left screen column shows the last texel and vice versa.
    let mut v2 = Vdp1::new();
    let srca2 = put_hgradient_char(&mut v2, 0x4000);
    put(
        &mut v2,
        0,
        [
            w(0x0010, 0x0000), // dir bit0 = h-flip
            w(0x0068, 0x0000),
            w(srca2, 0x0108),
            w(10, 10),
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v2, 1, END);
    v2.process_list();
    assert_eq!(v2.fb.pixel(10, 10), 8, "h-flip: left column = last texel");
    assert_eq!(v2.fb.pixel(17, 10), 1, "h-flip: right column = first texel");
}

#[test]
fn normal_sprite_vertical_flip_reverses_rows() {
    let mut v = Vdp1::new();
    // Per-row gradient: row r → colour r+1.
    for row in 0..8u32 {
        for col in 0..8u32 {
            v.vram.write16(0x4000 + (row * 8 + col) * 2, (row + 1) as u16);
        }
    }
    let srca = (0x4000u32 / 8) as u16;
    // v-flip (CMDCTRL bit 5 = 0x0020).
    put(
        &mut v,
        0,
        [
            w(0x0020, 0x0000), // dir bit1 = v-flip
            w(0x0068, 0x0000), // 16bpp + SPD
            w(srca, 0x0108),
            w(10, 10),
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    // Top screen row shows the last texture row (colour 8); bottom shows the
    // first (colour 1).
    assert_eq!(v.fb.pixel(10, 10), 8, "v-flip: top row = last texel row");
    assert_eq!(v.fb.pixel(10, 17), 1, "v-flip: bottom row = first texel row");
}

#[test]
fn colour_mode_16color_lut_indirects_through_vram() {
    let mut v = Vdp1::new();
    // 4bpp character: every byte 0x55 → both nibbles select index 5.
    for i in 0..(8 * 8 / 2) as u32 {
        v.vram.write8(0x5000 + i, 0x55);
    }
    let srca = (0x5000u32 / 8) as u16;
    // LUT base at VRAM byte 0x6000 → CMDCOLR = 0x6000 >> 3 = 0xC00. The LUT
    // entry for index 5 lives at base + 5*2; set it to a recognisable colour.
    v.vram.write16(0x6000 + 5 * 2, 0x7BDE);
    put(
        &mut v,
        0,
        [
            w(0x0000, 0x0000),
            w(0x0048, 0x0C00), // CMDPMOD: SPD off, colour mode 0x08 (LUT 4bpp)
            w(srca, 0x0108),
            w(3, 3),
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    assert_eq!(v.fb.pixel(3, 3), 0x7BDE, "LUT-resolved colour");
    assert_eq!(v.fb.pixel(10, 10), 0x7BDE, "far corner LUT colour");
}

#[test]
fn colour_mode_64_128_256_add_the_correct_bank_mask() {
    // 8bpp character: every texel byte = 0x05. Each mode adds CMDCOLR masked
    // to a different number of bank bits (64→&0xFFC0, 128→&0xFF80,
    // 256→&0xFF00). CMDCOLR = 0x0140 exercises bits that each mask treats
    // differently.
    let colr: u16 = 0x0140;
    for (pmod_mode, mask) in [(0x0010u16, 0xFFC0u16), (0x0018, 0xFF80), (0x0020, 0xFF00)] {
        let mut v = Vdp1::new();
        for i in 0..64u32 {
            v.vram.write8(0x5000 + i, 0x05);
        }
        let srca = (0x5000u32 / 8) as u16;
        put(
            &mut v,
            0,
            [
                w(0x0000, 0x0000),
                w(0x0040 | pmod_mode, colr), // SPD + colour mode
                w(srca, 0x0108),
                w(3, 3),
                0,
                0,
                0,
                0,
            ],
        );
        put(&mut v, 1, END);
        v.process_list();
        let expect = 0x05u16.wrapping_add(colr & mask);
        assert_eq!(
            v.fb.pixel(3, 3),
            expect,
            "8bpp mode {pmod_mode:#06x}: texel 5 + (CMDCOLR & {mask:#06x})"
        );
    }
}

#[test]
fn end_code_texel_is_skipped_unless_disabled() {
    let mut v = Vdp1::new();
    // 8bpp 64-colour character: texel (0,0) = 0xFF (the 8bpp end-code), the
    // rest = 0x05. With ECD clear, the end-code texel is not drawn.
    for i in 0..64u32 {
        v.vram.write8(0x5000 + i, 0x05);
    }
    v.vram.write8(0x5000, 0xFF); // (col0,row0) = end-code
    let srca = (0x5000u32 / 8) as u16;
    put(
        &mut v,
        0,
        [
            w(0x0000, 0x0000),
            w(0x0050, 0x0000), // SPD (0x40) + 64-colour mode (0x10), ECD clear
            w(srca, 0x0108),
            w(10, 10),
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    // end-code texel left untouched (background 0); a normal texel drew.
    assert_eq!(v.fb.pixel(10, 10), 0, "end-code texel skipped");
    assert_eq!(v.fb.pixel(11, 10), 0x05, "non-end-code texel drawn");

    // With ECD set (CMDPMOD bit 7 = 0x80) the end-code is treated as data.
    let mut v2 = Vdp1::new();
    for i in 0..64u32 {
        v2.vram.write8(0x5000 + i, 0x05);
    }
    v2.vram.write8(0x5000, 0xFF);
    put(
        &mut v2,
        0,
        [
            w(0x0000, 0x0000),
            w(0x00D0, 0x0000), // SPD (0x40) + ECD (0x80) + 64-colour (0x10)
            w(srca, 0x0108),
            w(10, 10),
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v2, 1, END);
    v2.process_list();
    assert_eq!(v2.fb.pixel(10, 10), 0xFF, "ECD set: end-code drawn as colour");
}

#[test]
fn transparent_texel_is_dropped_when_spd_off_and_kept_when_on() {
    // 16bpp character of all-zero pixels (colour 0 = transparent code). With
    // SPD off the sprite leaves the background untouched; with SPD on the
    // zero is drawn as an opaque colour-0 pixel.
    let mut bg = Vdp1::new();
    bg.fb.set_pixel(10, 10, 0x1F);
    for i in 0..64u32 {
        bg.vram.write16(0x2000 + i * 2, 0x0000);
    }
    let srca = (0x2000u32 / 8) as u16;
    put(
        &mut bg,
        0,
        [
            w(0x0000, 0x0000),
            w(0x0028, 0x0000), // 16bpp, SPD off
            w(srca, 0x0108),
            w(10, 10),
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut bg, 1, END);
    bg.process_list();
    assert_eq!(bg.fb.pixel(10, 10), 0x1F, "transparent texel keeps background");

    let mut on = Vdp1::new();
    on.fb.set_pixel(10, 10, 0x1F);
    for i in 0..64u32 {
        on.vram.write16(0x2000 + i * 2, 0x0000);
    }
    put(
        &mut on,
        0,
        [
            w(0x0000, 0x0000),
            w(0x0068, 0x0000), // 16bpp + SPD on
            w(srca, 0x0108),
            w(10, 10),
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut on, 1, END);
    on.process_list();
    assert_eq!(on.fb.pixel(10, 10), 0x0000, "SPD on: colour-0 drawn opaque");
}

#[test]
fn mesh_mode_draws_a_checkerboard() {
    let mut v = Vdp1::new();
    // Type 4 polygon with MESH (CMDPMOD bit 8 = 0x100). Mesh drops dots where
    // (x ^ y) & 1 == 0, leaving a 50% checkerboard.
    put(
        &mut v,
        0,
        [
            w(0x0004, 0x0000),
            w(0x0140, 0x001F), // MESH (0x100) + SPD (0x40), colour blue
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    // (10,10): x^y = 0 → dropped. (11,10): x^y = 1 → drawn.
    assert_eq!(v.fb.pixel(10, 10), 0, "mesh drops even-parity dot");
    assert_eq!(v.fb.pixel(11, 10), 0x001F, "mesh keeps odd-parity dot");
    assert_eq!(v.fb.pixel(12, 10), 0, "mesh drops the next even-parity dot");
}

#[test]
fn half_luminance_colour_calc_halves_the_source() {
    let mut v = Vdp1::new();
    // CMDPMOD colour-calc = 0b10 (half luminance): output = ((pix & !0x8421)
    // >> 1) | 0x8000. For an untextured polygon, pix = CMDCOLR.
    let colr: u16 = 0x7FFF; // all channels 31
    put(
        &mut v,
        0,
        [
            w(0x0004, 0x0000),
            w(0x0042, colr), // SPD (0x40) + calc mode 2 (0x02)
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    let expect = ((colr & !0x8421) >> 1) | 0x8000;
    assert_eq!(v.fb.pixel(15, 15), expect, "half-luminance output");
    // 31 → 15 per channel: 0x7FFF & !0x8421 = 0x7BDE, >>1 = 0x3DEF, |0x8000.
    assert_eq!(expect, 0xBDEF);
}

#[test]
fn half_transparent_blends_with_a_marked_destination() {
    let mut v = Vdp1::new();
    // Destination has its MSB set (required for the half-transparent blend);
    // its colour is white 0x7FFF | MSB. Source colour is black.
    v.fb.set_pixel(15, 15, 0xFFFF); // MSB | all-31
    put(
        &mut v,
        0,
        [
            w(0x0004, 0x0000),
            w(0x0043, 0x0000), // SPD + calc mode 3 (half transparent), CMDCOLR black
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    // 50% average of dest (31,31,31) and src (0,0,0) = (15,15,15), MSB set.
    // (15<<10)|(15<<5)|15 | 0x8000 = 0xBDEF.
    assert_eq!(v.fb.pixel(15, 15), 0xBDEF, "half-transparent blend result");

    // A destination with MSB clear is overwritten outright (no blend).
    let mut v2 = Vdp1::new();
    v2.fb.set_pixel(15, 15, 0x7FFF); // MSB clear
    put(
        &mut v2,
        0,
        [
            w(0x0004, 0x0000),
            w(0x0043, 0x0010), // calc mode 3, CMDCOLR = red 16
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    put(&mut v2, 1, END);
    v2.process_list();
    assert_eq!(v2.fb.pixel(15, 15), 0x0010, "no blend when dest MSB clear");
}

#[test]
fn shadow_calc_halves_only_a_marked_destination() {
    let mut v = Vdp1::new();
    // Shadow (calc mode 1) halves the destination iff its MSB is set; the
    // source colour is ignored.
    v.fb.set_pixel(15, 15, 0xFFFF); // MSB | 31,31,31
    v.fb.set_pixel(16, 16, 0x7FFF); // MSB clear — untouched by shadow
    put(
        &mut v,
        0,
        [
            w(0x0004, 0x0000),
            w(0x0041, 0x001F), // SPD + calc mode 1 (shadow)
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    assert_eq!(v.fb.pixel(15, 15), 0xBDEF, "shadow halves MSB-marked dest");
    assert_eq!(v.fb.pixel(16, 16), 0x7FFF, "shadow leaves unmarked dest alone");
}

#[test]
fn msbon_forces_the_pixel_high_bit() {
    let mut v = Vdp1::new();
    // MSBON (CMDPMOD bit 15 = 0x8000) ORs 0x8000 into every drawn pixel.
    put(
        &mut v,
        0,
        [
            w(0x0004, 0x0000),
            w(0x8040, 0x001F), // MSBON (0x8000) + SPD (0x40), colour blue
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    assert_eq!(v.fb.pixel(15, 15), 0x801F, "MSBON sets the high bit");
}

#[test]
fn polyline_draws_the_four_quad_edges() {
    let mut v = Vdp1::new();
    // Type 5 polyline connects A→B→C→D→A with line segments (no fill).
    put(
        &mut v,
        0,
        [
            w(0x0005, 0x0000),
            w(0x0080, 0x03E0), // ECD → Poly writer, colour green
            0,
            w(10, 10), // A
            w(30, 10), // B
            w(30, 30), // C
            w(10, 30), // D
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    // Each edge is on the rectangle border; the interior is empty.
    assert_eq!(v.fb.pixel(20, 10), 0x03E0, "top edge A→B");
    assert_eq!(v.fb.pixel(30, 20), 0x03E0, "right edge B→C");
    assert_eq!(v.fb.pixel(20, 30), 0x03E0, "bottom edge C→D");
    assert_eq!(v.fb.pixel(10, 20), 0x03E0, "left edge D→A");
    assert_eq!(v.fb.pixel(20, 20), 0, "polyline interior empty");
}

#[test]
fn system_clipping_bounds_a_primitive() {
    let mut v = Vdp1::new();
    // System clip (type 9): lower-right corner (XC,YC). Origin is fixed (0,0).
    // Clip to x<=25, y<=25.
    put(&mut v, 0, [w(0x0009, 0), 0, 0, 0, 0, w(25, 25), 0, 0]);
    // Polygon (10,10)-(40,40) — the part past x=25 / y=25 is clipped away.
    put(
        &mut v,
        1,
        [
            w(0x0004, 0),
            w(0x0080, 0x001F),
            0,
            w(10, 10),
            w(40, 10),
            w(40, 40),
            w(10, 40),
            0,
        ],
    );
    put(&mut v, 2, END);
    v.process_list();
    assert_eq!(v.fb.pixel(20, 20), 0x001F, "inside the system clip drawn");
    assert_eq!(v.fb.pixel(30, 20), 0, "x past the system clip removed");
    assert_eq!(v.fb.pixel(20, 30), 0, "y past the system clip removed");
}

#[test]
fn user_clipping_applies_only_when_cmdpmod_selects_it() {
    let mut v = Vdp1::new();
    // User clip (type 8): rectangle (XA,YA)-(XC,YC) = (15,15)-(25,25).
    put(&mut v, 0, [w(0x0008, 0), 0, 0, w(15, 15), 0, w(25, 25), 0, 0]);
    // Polygon (10,10)-(40,40) with CMDPMOD bit 10 (0x0400) → use the user clip.
    put(
        &mut v,
        1,
        [
            w(0x0004, 0),
            w(0x0480, 0x001F), // user-clip select (0x400) + ECD (0x80)
            0,
            w(10, 10),
            w(40, 10),
            w(40, 40),
            w(10, 40),
            0,
        ],
    );
    put(&mut v, 2, END);
    v.process_list();
    assert_eq!(v.fb.pixel(20, 20), 0x001F, "inside the user clip drawn");
    assert_eq!(v.fb.pixel(12, 12), 0, "above-left of user clip removed");
    assert_eq!(v.fb.pixel(30, 30), 0, "below-right of user clip removed");
}

#[test]
fn fully_offscreen_sprite_is_rejected() {
    let mut v = Vdp1::new();
    for i in 0..64u32 {
        v.vram.write16(0x2000 + i * 2, 0x7FFF);
    }
    let srca = (0x2000u32 / 8) as u16;
    // Normal sprite whose origin is past the bottom-right clip corner: the
    // draw_normal_sprite early-out (x>max_x || y>max_y) rejects it entirely.
    put(
        &mut v,
        0,
        [
            w(0x0000, 0x0000),
            w(0x0068, 0x0000),
            w(srca, 0x0108),
            w(600, 300), // beyond 512×256
            0,
            0,
            0,
            0,
        ],
    );
    put(&mut v, 1, END);
    v.process_list();
    // Nothing in the visible buffer changed (spot-check a few cells).
    assert_eq!(v.fb.pixel(0, 0), 0, "off-screen sprite drew nothing");
    assert_eq!(v.fb.pixel(500, 255), 0, "off-screen sprite drew nothing");
}

#[test]
fn call_and_return_walk_a_subroutine_then_resume() {
    let mut v = Vdp1::new();
    // cmd0: CALL (jump-mode 0x2000) the subroutine at position 3
    //       (CMDLINK = 3 << 2 = 12), drawing a polygon at (10,10) first.
    put(
        &mut v,
        0,
        [
            w(0x2004, 12), // CALL + type 4
            w(0x0080, 0x001F),
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    // cmd1: after RETURN the walker resumes here — polygon at (40,40).
    put(
        &mut v,
        1,
        [
            w(0x0004, 0),
            w(0x0080, 0x03E0),
            0,
            w(40, 40),
            w(50, 40),
            w(50, 50),
            w(40, 50),
            0,
        ],
    );
    put(&mut v, 2, END);
    // cmd3: the subroutine body — polygon at (80,80) — then RETURN.
    put(
        &mut v,
        3,
        [
            w(0x0004, 0),
            w(0x0080, 0x7C00),
            0,
            w(80, 80),
            w(90, 80),
            w(90, 90),
            w(80, 90),
            0,
        ],
    );
    put(
        &mut v,
        4,
        [w(0x3000, 0), 0, 0, 0, 0, 0, 0, 0], // RETURN (jump-mode 0x3000)
    );
    v.process_list();
    assert_eq!(v.fb.pixel(15, 15), 0x001F, "caller drew before the call");
    assert_eq!(v.fb.pixel(85, 85), 0x7C00, "subroutine body drew");
    assert_eq!(v.fb.pixel(45, 45), 0x03E0, "resumed at the return address");
}

#[test]
fn illegal_command_type_terminates_the_list() {
    let mut v = Vdp1::new();
    // Type 0xB is not a defined command → the plotter ends the list early,
    // so the polygon queued after it never draws.
    put(&mut v, 0, [w(0x000B, 0), 0, 0, 0, 0, 0, 0, 0]);
    put(
        &mut v,
        1,
        [
            w(0x0004, 0),
            w(0x0080, 0x001F),
            0,
            w(10, 10),
            w(20, 10),
            w(20, 20),
            w(10, 20),
            0,
        ],
    );
    put(&mut v, 2, END);
    v.process_list();
    assert_eq!(v.fb.pixel(15, 15), 0, "illegal command ended the list early");
}

#[test]
fn timed_draw_end_fires_after_the_draw_duration_via_run_for() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    sat.bus.write32(VRAM_BASE, 0x8000_0000, AccessKind::Data); // END
    sat.bus.write16(REGS_BASE + 0x04, 0x0001, AccessKind::Data); // PTMR: one-shot draw

    // The draw is in progress and has not yet raised the SCU source.
    assert!(
        sat.bus.vdp1.is_drawing(),
        "plot is timed, still in progress"
    );
    assert_eq!(sat.bus.scu.ist & (1 << 13), 0, "no draw-end yet");

    // Advancing global time completes the draw and raises the interrupt.
    sat.run_for(4096);
    assert!(!sat.bus.vdp1.is_drawing(), "plot completed");
    assert_ne!(
        sat.bus.scu.ist & (1 << 13),
        0,
        "draw-end latched after the modelled draw duration"
    );
}
