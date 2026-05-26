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
    // Kick the plot the way the BIOS does: write PTMR with the
    // immediate-draw mode bit set.
    v.write16(REGS_BASE + 0x04, 0x0002);

    assert_eq!(v.fb.pixel(35, 35), 0x001F, "PTMR write drove the plotter");
    assert_eq!(
        v.regs.read16(0x10) & 0x0002,
        0x0002,
        "draw-end flag latched"
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
