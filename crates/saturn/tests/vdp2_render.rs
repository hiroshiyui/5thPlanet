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

#[test]
fn nbg0_horizontal_reduction_halves_the_layer() {
    // ZMXN0 = 2.0 reduces NBG0 by half: each screen dot steps two source
    // pixels, so source x=100 lands at screen x=50.
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data); // NBG0
    sat.bus.write16(REG_CHCTLA, 0x0012, AccessKind::Data); // bitmap, 8bpp
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA.N0PRIN = 1
    sat.bus.vdp2.cram.write16(7 * 2, 0x001F); // CRAM[7] = red
    sat.bus.write16(0x05F8_0078, 0x0002, AccessKind::Data); // ZMXIN0 = 2 (integer)
    sat.bus.write16(0x05F8_007A, 0x0000, AccessKind::Data); // ZMXDN0 = 0 (fraction)
    sat.bus.vdp2.vram.write8(60 * 512 + 100, 7); // source pixel (100, 60)

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    let px = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(&out[px..px + 4], &[0xFF, 0, 0, 0xFF], "src x=100 maps to screen x=50");
    // Neighbours sample even source columns (98, 102) — both empty → backdrop.
    let left = (60 * FRAME_WIDTH + 49) * 4;
    assert_eq!(&out[left..left + 4], &[0, 0, 0, 0xFF], "screen x=49 → src 98 (empty)");
}

#[test]
fn nbg0_fractional_scroll_shifts_the_sampled_source_pixel() {
    // With a non-integer reduction (ZMXN0 = 1.5) a +0.5 X scroll fraction tips
    // the accumulator across a pixel boundary: screen x=1 samples source 2
    // (red) instead of source 1 (green). This proves the 8-bit scroll fraction
    // is wired (it is otherwise invisible under nearest sampling at 1:1).
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data); // NBG0
    sat.bus.write16(REG_CHCTLA, 0x0012, AccessKind::Data); // bitmap, 8bpp
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA.N0PRIN = 1
    sat.bus.vdp2.cram.write16(6 * 2, 0x03E0); // CRAM[6] = green (source col 1)
    sat.bus.vdp2.cram.write16(7 * 2, 0x001F); // CRAM[7] = red   (source col 2)
    sat.bus.write16(0x05F8_0078, 0x0001, AccessKind::Data); // ZMXIN0 = 1
    sat.bus.write16(0x05F8_007A, 0x8000, AccessKind::Data); // ZMXDN0 = 0.5 → inc 1.5
    sat.bus.write16(0x05F8_0072, 0x8000, AccessKind::Data); // SCXDN0 = 0.5 scroll fraction
    sat.bus.vdp2.vram.write8(30 * 512 + 1, 6); // source (1, 30) = green
    sat.bus.vdp2.vram.write8(30 * 512 + 2, 7); // source (2, 30) = red

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    let px = (30 * FRAME_WIDTH + 1) * 4;
    assert_eq!(
        &out[px..px + 4],
        &[0xFF, 0, 0, 0xFF],
        "the +0.5 fraction makes screen x=1 sample source 2 (red), not source 1 (green)"
    );
}

#[test]
fn special_color_calc_mode2_gates_blending_by_sfcode() {
    // SFCCMD mode 2: per-dot colour calc = the bitmap special-cc bit (BMSCC),
    // then masked off when the dot's palette code fails the SFCODE test. So at a
    // code whose SFCODE bit is set the dot blends with the backdrop; at a code
    // whose bit is clear it stays opaque.
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data); // NBG0
    sat.bus.write16(REG_CHCTLA, 0x0012, AccessKind::Data); // NBG0 bitmap, 8bpp
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA N0PRIN = 1
    // Backdrop = blue, table at VRAM word 0x100 (clear of the bitmap at base 0).
    sat.bus.vdp2.vram.write16(0x200, 0x7C00); // RGB555 blue (B=31)
    sat.bus.write16(0x05F8_00AC, 0x0000, AccessKind::Data); // BKTAU
    sat.bus.write16(0x05F8_00AE, 0x0100, AccessKind::Data); // BKTAL word 0x100
    // CRAM[2] = CRAM[4] = red (both dots are red; only their cc differs).
    sat.bus.vdp2.cram.write16(2 * 2, 0x001F);
    sat.bus.vdp2.cram.write16(4 * 2, 0x001F);
    // Colour calc for NBG0: enable (CCCTL bit 0), ratio 16 → alpha ≈ 0.48.
    sat.bus.write16(0x05F8_00EC, 0x0001, AccessKind::Data); // CCCTL N0 cc enable
    sat.bus.write16(0x05F8_0108, 0x0010, AccessKind::Data); // CCRNA N0 ratio = 16
    // Special cc: BMSCC (BMPNA bit 4), SFCCMD NBG0 = mode 2, SFCODE-A = bit 2.
    sat.bus.write16(0x05F8_002C, 0x0010, AccessKind::Data); // BMPNA: N0 BMSCC
    sat.bus.write16(0x05F8_00EE, 0x0002, AccessKind::Data); // SFCCMD NBG0 = 2
    sat.bus.write16(0x05F8_0024, 0x0000, AccessKind::Data); // SFSEL → SFCODE-A
    sat.bus.write16(0x05F8_0026, 0x0004, AccessKind::Data); // SFCODE-A = 0x04 (bit 2)
    sat.bus.vdp2.vram.write8(60 * 512 + 50, 4); // dot A: code 4 → (4>>1)&7=2 → bit set
    sat.bus.vdp2.vram.write8(60 * 512 + 52, 2); // dot B: code 2 → (2>>1)&7=1 → bit clear

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    // Dot A: cc on → red over blue at alpha = (31-16)*255/31 = 123.
    let a = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(&out[a..a + 4], &[123, 0, 132, 0xFF], "code 4: SFCODE bit set → blended");
    // Dot B: cc gated off → opaque red.
    let b = (60 * FRAME_WIDTH + 52) * 4;
    assert_eq!(&out[b..b + 4], &[255, 0, 0, 0xFF], "code 2: SFCODE bit clear → opaque");
}

#[test]
fn special_priority_mode2_raises_lsb_by_sfcode() {
    // SFPRMD mode 2: priority LSB = the bitmap special-priority bit (BMSPR),
    // masked off when the dot's palette code fails the SFCODE test. NBG0 (reg
    // priority 2) competes with NBG1 (priority 3): where NBG0's code passes
    // SFCODE its priority rises to 3 and it wins the tie (front-order); where it
    // fails, it drops to 2 and NBG1 shows through.
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0003, AccessKind::Data); // NBG0 + NBG1
    sat.bus.write16(REG_CHCTLA, 0x1212, AccessKind::Data); // both bitmap, 8bpp
    sat.bus.write16(0x05F8_003C, 0x0010, AccessKind::Data); // MPOFN: NBG1 base offset 1
    sat.bus.write16(0x05F8_00F8, 0x0302, AccessKind::Data); // PRINA: N0=2, N1=3
    // CRAM: NBG0 red (codes 2 & 4), NBG1 green (code 1).
    sat.bus.vdp2.cram.write16(2, 0x03E0); // CRAM[1] = green
    sat.bus.vdp2.cram.write16(2 * 2, 0x001F);
    sat.bus.vdp2.cram.write16(4 * 2, 0x001F);
    // Special priority: BMSPR (BMPNA bit 5), SFPRMD NBG0 = mode 2, SFCODE-A bit 2.
    sat.bus.write16(0x05F8_002C, 0x0020, AccessKind::Data); // BMPNA: N0 BMSPR
    sat.bus.write16(0x05F8_00EA, 0x0002, AccessKind::Data); // SFPRMD NBG0 = 2
    sat.bus.write16(0x05F8_0024, 0x0000, AccessKind::Data); // SFSEL → SFCODE-A
    sat.bus.write16(0x05F8_0026, 0x0004, AccessKind::Data); // SFCODE-A = 0x04 (bit 2)
    // NBG0 at base 0; NBG1 at base 0x20000.
    sat.bus.vdp2.vram.write8(60 * 512 + 50, 4); // NBG0 dot A: code 4 → passes
    sat.bus.vdp2.vram.write8(60 * 512 + 52, 2); // NBG0 dot B: code 2 → fails
    sat.bus.vdp2.vram.write8(0x20000 + 60 * 512 + 50, 1); // NBG1 green at A
    sat.bus.vdp2.vram.write8(0x20000 + 60 * 512 + 52, 1); // NBG1 green at B

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    let a = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(&out[a..a + 4], &[255, 0, 0, 0xFF], "code 4: NBG0 prio→3, wins tie (red)");
    let b = (60 * FRAME_WIDTH + 52) * 4;
    assert_eq!(&out[b..b + 4], &[0, 255, 0, 0xFF], "code 2: NBG0 prio→2, NBG1 shows (green)");
}

#[test]
fn rpmd_selects_rotation_parameter_set_for_rbg0() {
    // RBG0 drives its geometry from rotation parameter set A or B per RPMD.
    // Both sets use an identity transform but address different bitmap bases
    // (MPOFR), so RPMD=0 shows parameter A's bitmap and RPMD=1 parameter B's.
    const ONE: u32 = 1 << 16;
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0010, AccessKind::Data); // RBG0 only
    sat.bus.write16(0x05F8_002A, 0x1200, AccessKind::Data); // CHCTLB: R0BMEN + 8bpp
    sat.bus.write16(0x05F8_00FC, 0x0001, AccessKind::Data); // PRIR (RBG0 priority) = 1
    sat.bus.write16(0x05F8_003E, 0x0010, AccessKind::Data); // MPOFR: param B base offset 1
    // Rotation parameter table at VRAM byte 0x40000 (RPTA word addr 0x20000).
    sat.bus.write16(0x05F8_00BC, 0x0002, AccessKind::Data); // RPTAU
    sat.bus.write16(0x05F8_00BE, 0x0000, AccessKind::Data); // RPTAL
    // Identity transform for parameter set A (0x40000) and B (0x40080).
    for base in [0x40000u32, 0x40080] {
        for &(k, val) in &[(4u32, ONE), (5, ONE), (7, ONE), (11, ONE), (19, ONE), (20, ONE)] {
            sat.bus.vdp2.vram.write32(base + k * 4, val);
        }
    }
    // CRAM: code 1 = red (param A bitmap), code 2 = green (param B bitmap).
    sat.bus.vdp2.cram.write16(2, 0x001F);
    sat.bus.vdp2.cram.write16(4, 0x03E0);
    // Bitmap dot at plane (10,10): param A base 0 → red, param B base 0x20000 → green.
    sat.bus.vdp2.vram.write8(10 * 512 + 10, 1);
    sat.bus.vdp2.vram.write8(0x20000 + 10 * 512 + 10, 2);

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    let px = (10 * FRAME_WIDTH + 10) * 4;

    sat.bus.write16(0x05F8_00B0, 0x0000, AccessKind::Data); // RPMD = 0 → param A
    sat.run_frame(&mut out);
    assert_eq!(&out[px..px + 4], &[0xFF, 0, 0, 0xFF], "RPMD=0 → param A bitmap (red)");

    sat.bus.write16(0x05F8_00B0, 0x0001, AccessKind::Data); // RPMD = 1 → param B
    sat.run_frame(&mut out);
    assert_eq!(&out[px..px + 4], &[0, 0xFF, 0, 0xFF], "RPMD=1 → param B bitmap (green)");
}
