//! End-to-end rendering through `Saturn::run_frame` (M3 task #6).
//!
//! Pre-loads VDP2 state (VRAM bitmap + CRAM palette + the right
//! registers to enable bitmap NBG0), runs one frame, and inspects
//! the resulting framebuffer at specific pixels. Proves the chain
//! Saturn → run_for → render_frame → output buffer is wired.

use saturn::Saturn;
use saturn::vdp2::{FRAME_HEIGHT, FRAME_WIDTH, FRAMEBUFFER_BYTES};
use sh2::bus::{AccessKind, Bus};

const REG_TVMD: u32 = 0x05F8_0000;
const REG_BGON: u32 = 0x05F8_0020;
const REG_CHCTLA: u32 = 0x05F8_0028;

#[test]
fn run_frame_returns_default_resolution() {
    let mut sat = Saturn::with_blank_bios();
    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    // Power-on/default TVMD is NTSC low-res; run_frame reports the active dims.
    let dims = sat.run_frame(&mut out);
    assert_eq!(dims, (FRAME_WIDTH, FRAME_HEIGHT));
    assert!(out.len() >= dims.0 * dims.1 * 4);
}

#[test]
fn display_off_yields_opaque_black_frame_even_after_running() {
    let mut sat = Saturn::with_blank_bios();
    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    let (w, h) = sat.run_frame(&mut out);
    for px in out[..w * h * 4].chunks_exact(4) {
        assert_eq!(px, &[0, 0, 0, 0xFF]);
    }
}

#[test]
fn bitmap_nbg0_through_run_frame_picks_up_synthetic_scene() {
    let mut sat = Saturn::with_blank_bios();
    // Halt the slave so its arbitrary state doesn't write anywhere
    // unexpected during the run.
    sat.halt_slave();

    // Program VDP2: DISP on, NBG0 on, bitmap mode.
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data);
    sat.bus.write16(REG_CHCTLA, 0x0012, AccessKind::Data); // N0BMEN + N0CHCN=1 (8bpp)
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA.N0PRIN = 1
    // CRAM: index 0 = black, index 7 = pure red.
    sat.bus.vdp2.cram.write16(0, 0x0000);
    sat.bus.vdp2.cram.write16(7 * 2, 0x001F);
    // Bitmap: paint pixel (50, 60) with palette index 7. The hardware
    // bitmap is 512 px wide (N0BMSZ=0), independent of the 320-px screen.
    let off = 60u32 * 512 + 50;
    sat.bus.vdp2.vram.write8(off, 7);

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    let px = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(&out[px..px + 4], &[0xFF, 0, 0, 0xFF], "red at (50,60)");
    // Pixel right next door is the backdrop — the back screen at BKTA=0 reads
    // VRAM word 0, which is 0 (black) here.
    let px_next = (60 * FRAME_WIDTH + 51) * 4;
    assert_eq!(&out[px_next..px_next + 4], &[0, 0, 0, 0xFF]);
}

#[test]
fn back_screen_single_colour_fills_the_backdrop_from_vram() {
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    // DISP on, no backgrounds enabled → the whole frame is the back screen.
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0000, AccessKind::Data);
    // Back-screen table at VRAM word 0x100 (byte 0x200), single RGB555 green.
    sat.bus.vdp2.vram.write16(0x200, 0x03E0); // g = 0x1F
    sat.bus.write16(0x05F8_00AC, 0x0000, AccessKind::Data); // BKTAU: hi=0, BKCLMD=0
    sat.bus.write16(0x05F8_00AE, 0x0100, AccessKind::Data); // BKTAL: word addr 0x100

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    // Every sampled pixel is the single back-screen colour (green).
    for &(x, y) in &[(0usize, 0usize), (160, 100), (319, 223)] {
        let px = (y * FRAME_WIDTH + x) * 4;
        assert_eq!(&out[px..px + 4], &[0, 0xFF, 0, 0xFF], "green backdrop at ({x},{y})");
    }
}

#[test]
fn back_screen_per_line_colour_advances_one_word_per_scanline() {
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0000, AccessKind::Data);
    // Per-line table at word 0x100: line 0 = red, line 1 = blue, line 2 = white.
    sat.bus.vdp2.vram.write16(0x200, 0x001F); // r
    sat.bus.vdp2.vram.write16(0x202, 0x7C00); // b
    sat.bus.vdp2.vram.write16(0x204, 0x7FFF); // white
    sat.bus.write16(0x05F8_00AC, 0x8000, AccessKind::Data); // BKTAU: BKCLMD = per-line
    sat.bus.write16(0x05F8_00AE, 0x0100, AccessKind::Data);

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    let row = |y: usize| -> [u8; 4] {
        let px = (y * FRAME_WIDTH + 10) * 4;
        out[px..px + 4].try_into().unwrap()
    };
    assert_eq!(row(0), [0xFF, 0, 0, 0xFF], "line 0 red");
    assert_eq!(row(1), [0, 0, 0xFF, 0xFF], "line 1 blue");
    assert_eq!(row(2), [0xFF, 0xFF, 0xFF, 0xFF], "line 2 white");
}

#[test]
fn line_colour_screen_is_the_colour_calc_partner_of_an_enabled_layer() {
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    // NBG0 bitmap, 8bpp, priority 1, with colour calc enabled at ratio 31
    // (front weight 0 → the result is purely the colour-calc partner colour).
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data);
    sat.bus.write16(REG_CHCTLA, 0x0012, AccessKind::Data); // N0BMEN + 8bpp
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA.N0PRIN = 1
    sat.bus.write16(0x05F8_00EC, 0x0001, AccessKind::Data); // CCCTL: N0 colour-calc on, ratio mode
    sat.bus.write16(0x05F8_0108, 0x001F, AccessKind::Data); // CCRNA: N0 ratio = 31
    // Line-colour screen on for NBG0, table word 0x100 → CRAM index 9 = green.
    sat.bus.write16(0x05F8_00E8, 0x0001, AccessKind::Data); // LNCLEN: NBG0
    sat.bus.write16(0x05F8_00A8, 0x0000, AccessKind::Data); // LCTAU
    sat.bus.write16(0x05F8_00AA, 0x0100, AccessKind::Data); // LCTAL: word 0x100
    sat.bus.vdp2.vram.write16(0x200, 9); // line-colour palette index
    sat.bus.vdp2.cram.write16(9 * 2, 0x03E0); // CRAM[9] = green
    sat.bus.vdp2.cram.write16(7 * 2, 0x001F); // CRAM[7] = red (the NBG0 dot)
    sat.bus.vdp2.vram.write8(60 * 512 + 50, 7); // NBG0 bitmap dot = index 7

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    // At ratio 31 the NBG0 red dot is fully replaced by its colour-calc partner,
    // which the line-colour screen supplies → green.
    let px = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(
        &out[px..px + 4],
        &[0, 0xFF, 0, 0xFF],
        "line colour is the colour-calc partner of the NBG0 dot"
    );
}

#[test]
fn color_offset_adds_signed_per_channel_offset_keyed_on_front_screen() {
    // VDP2 colour-offset function (CLOFEN/CLOFSL + COAR..COBB): the front
    // screen's selected RGB offset is added to the final dot (clamped 0..=255).
    // This is how games do fade-to-black / fade-to-white / tint transitions.
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();

    // NBG0 bitmap, 8bpp, priority 1 (same scene wiring as the synthetic-scene test).
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data);
    sat.bus.write16(REG_CHCTLA, 0x0012, AccessKind::Data); // N0BMEN + 8bpp
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA.N0PRIN = 1
    // CRAM[7] = mid grey (R=G=B=16 → 132 each), so one offset can darken (R),
    // brighten (G), and clamp-high (B) in a single dot.
    sat.bus.vdp2.cram.write16(7 * 2, 0x4210);
    sat.bus.vdp2.vram.write8(60 * 512 + 50, 7);

    // Colour offset for NBG0 (CLOFEN bit 0), set A (CLOFSL bit 0 = 0):
    // R = -64 (9-bit two's-complement 0x1C0), G = +32, B = +200 (clamps).
    sat.bus.write16(0x05F8_0110, 0x0001, AccessKind::Data); // CLOFEN: NBG0
    sat.bus.write16(0x05F8_0112, 0x0000, AccessKind::Data); // CLOFSL: set A
    sat.bus.write16(0x05F8_0114, 0x01C0, AccessKind::Data); // COAR = -64
    sat.bus.write16(0x05F8_0116, 0x0020, AccessKind::Data); // COAG = +32
    sat.bus.write16(0x05F8_0118, 0x00C8, AccessKind::Data); // COAB = +200

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    let px = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(
        &out[px..px + 4],
        &[132 - 64, 132 + 32, 255, 0xFF],
        "grey dot offset by (-64, +32, +200-clamped)"
    );
}

#[test]
fn color_offset_disabled_leaves_dot_unchanged() {
    // CLOFEN clear for the front screen → no offset, even with COAR..COBB set.
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data);
    sat.bus.write16(REG_CHCTLA, 0x0012, AccessKind::Data);
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data);
    sat.bus.vdp2.cram.write16(7 * 2, 0x4210); // mid grey (132,132,132)
    sat.bus.vdp2.vram.write8(60 * 512 + 50, 7);
    // Offsets present but NBG0's enable bit (0) is clear.
    sat.bus.write16(0x05F8_0110, 0x0002, AccessKind::Data); // CLOFEN: NBG1 only
    sat.bus.write16(0x05F8_0114, 0x01C0, AccessKind::Data); // COAR = -64

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    let px = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(&out[px..px + 4], &[132, 132, 132, 0xFF], "untouched grey dot");
}
