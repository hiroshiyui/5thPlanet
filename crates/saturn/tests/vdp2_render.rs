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
        assert_eq!(
            &out[px..px + 4],
            &[0, 0xFF, 0, 0xFF],
            "green backdrop at ({x},{y})"
        );
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
    assert_eq!(
        &out[px..px + 4],
        &[132, 132, 132, 0xFF],
        "untouched grey dot"
    );
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
    assert_eq!(
        &out[px..px + 4],
        &[0xFF, 0, 0, 0xFF],
        "src x=100 maps to screen x=50"
    );
    // Neighbours sample even source columns (98, 102) — both empty → backdrop.
    let left = (60 * FRAME_WIDTH + 49) * 4;
    assert_eq!(
        &out[left..left + 4],
        &[0, 0, 0, 0xFF],
        "screen x=49 → src 98 (empty)"
    );
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
    assert_eq!(
        &out[a..a + 4],
        &[123, 0, 132, 0xFF],
        "code 4: SFCODE bit set → blended"
    );
    // Dot B: cc gated off → opaque red.
    let b = (60 * FRAME_WIDTH + 52) * 4;
    assert_eq!(
        &out[b..b + 4],
        &[255, 0, 0, 0xFF],
        "code 2: SFCODE bit clear → opaque"
    );
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
    assert_eq!(
        &out[a..a + 4],
        &[255, 0, 0, 0xFF],
        "code 4: NBG0 prio→3, wins tie (red)"
    );
    let b = (60 * FRAME_WIDTH + 52) * 4;
    assert_eq!(
        &out[b..b + 4],
        &[0, 255, 0, 0xFF],
        "code 2: NBG0 prio→2, NBG1 shows (green)"
    );
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
        for &(k, val) in &[
            (4u32, ONE),
            (5, ONE),
            (7, ONE),
            (11, ONE),
            (19, ONE),
            (20, ONE),
        ] {
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
    assert_eq!(
        &out[px..px + 4],
        &[0xFF, 0, 0, 0xFF],
        "RPMD=0 → param A bitmap (red)"
    );

    sat.bus.write16(0x05F8_00B0, 0x0001, AccessKind::Data); // RPMD = 1 → param B
    sat.run_frame(&mut out);
    assert_eq!(
        &out[px..px + 4],
        &[0, 0xFF, 0, 0xFF],
        "RPMD=1 → param B bitmap (green)"
    );
}

#[test]
fn hires_mode_renders_the_rotation_layer_at_half_dot_resolution() {
    // In the 640/704-dot modes the rotation layer renders at *normal* dot
    // resolution: each rotation dot spans two display dots (Mednafen draws
    // the RBG line buffer 352 wide, `LB.rotabsel[x >> 1]`). With an identity
    // transform, plane dot N must appear at display x = 2N and 2N+1.
    // Regression for VF2's "phantom ring-out": stepping the rotation walk
    // per display dot compressed the 704-dot fight floor 2× to screen-left.
    const ONE: u32 = 1 << 16;
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8002, AccessKind::Data); // HRESO=2: 640×224
    sat.bus.write16(REG_BGON, 0x0010, AccessKind::Data); // RBG0 only
    sat.bus.write16(0x05F8_002A, 0x1200, AccessKind::Data); // CHCTLB: R0BMEN + 8bpp
    sat.bus.write16(0x05F8_00FC, 0x0001, AccessKind::Data); // PRIR = 1
    sat.bus.write16(0x05F8_00BC, 0x0002, AccessKind::Data); // RPTAU
    sat.bus.write16(0x05F8_00BE, 0x0000, AccessKind::Data); // RPTAL
    for &(k, val) in &[
        (4u32, ONE),
        (5, ONE),
        (7, ONE),
        (11, ONE),
        (19, ONE),
        (20, ONE),
    ] {
        sat.bus.vdp2.vram.write32(0x40000 + k * 4, val);
    }
    sat.bus.vdp2.cram.write16(2, 0x001F); // code 1 = red
    // One red plane dot at (10, 10).
    sat.bus.vdp2.vram.write8(10 * 512 + 10, 1);

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    let (w, _) = sat.run_frame(&mut out);
    assert_eq!(w, 640);
    let px = |x: usize| (10 * w + x) * 4;
    // Plane dot 10 occupies display dots 20 and 21 — and nothing else nearby.
    assert_eq!(
        &out[px(20)..px(20) + 4],
        &[0xFF, 0, 0, 0xFF],
        "first half of the doubled dot"
    );
    assert_eq!(
        &out[px(21)..px(21) + 4],
        &[0xFF, 0, 0, 0xFF],
        "second half of the doubled dot"
    );
    assert_ne!(
        &out[px(10)..px(10) + 4],
        &[0xFF, 0, 0, 0xFF],
        "x=10 would be the un-doubled bug"
    );
    assert_ne!(&out[px(22)..px(22) + 4], &[0xFF, 0, 0, 0xFF]);
}

#[test]
fn special_color_calc_mode3_uses_cram_msb_in_rgb888_mode() {
    // SFCCMD mode 3 on a paletted dot keys colour calc on the CRAM entry's
    // colour-calc MSB. In RGB888 CRAM mode (RAMCTL.CRMD=2) that MSB is bit 31 of
    // the 32-bit entry. Two red dots whose entries differ only in that bit: the
    // MSB-set one blends with the backdrop, the MSB-clear one stays opaque.
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(0x05F8_000E, 0x2000, AccessKind::Data); // RAMCTL CRMD = 2 (RGB888)
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data); // NBG0
    sat.bus.write16(REG_CHCTLA, 0x0012, AccessKind::Data); // NBG0 bitmap, 8bpp
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA N0PRIN = 1
    // Backdrop = blue (read as RGB555 from VRAM regardless of CRAM mode).
    sat.bus.vdp2.vram.write16(0x200, 0x7C00);
    sat.bus.write16(0x05F8_00AC, 0x0000, AccessKind::Data); // BKTAU
    sat.bus.write16(0x05F8_00AE, 0x0100, AccessKind::Data); // BKTAL word 0x100
    // RGB888 entries (0x00BBGGRR): both red; only bit 31 (the cc MSB) differs.
    sat.bus.vdp2.cram.write32(5 * 4, 0x8000_00FF); // CRAM[5] = red, cc-MSB set
    sat.bus.vdp2.cram.write32(6 * 4, 0x0000_00FF); // CRAM[6] = red, cc-MSB clear
    // Colour calc for NBG0: enable (CCCTL bit 0), ratio 16, SFCCMD mode 3.
    sat.bus.write16(0x05F8_00EC, 0x0001, AccessKind::Data); // CCCTL N0 cc enable
    sat.bus.write16(0x05F8_0108, 0x0010, AccessKind::Data); // CCRNA N0 ratio = 16
    sat.bus.write16(0x05F8_00EE, 0x0003, AccessKind::Data); // SFCCMD NBG0 = 3
    sat.bus.vdp2.vram.write8(60 * 512 + 50, 5); // dot A: CRAM[5] (MSB set)
    sat.bus.vdp2.vram.write8(60 * 512 + 52, 6); // dot B: CRAM[6] (MSB clear)

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    // Dot A: cc on → red over blue at alpha = (31-16)*255/31 = 123.
    let a = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(
        &out[a..a + 4],
        &[123, 0, 132, 0xFF],
        "CRAM MSB set → blended"
    );
    // Dot B: MSB clear → cc gated off → opaque red.
    let b = (60 * FRAME_WIDTH + 52) * 4;
    assert_eq!(
        &out[b..b + 4],
        &[255, 0, 0, 0xFF],
        "CRAM MSB clear → opaque"
    );
}

#[test]
fn special_color_calc_mode3_always_blends_rgb_direct_dots() {
    // SFCCMD mode 3 on an RGB direct-colour (16bpp) dot enables colour calc
    // unconditionally (no palette/MSB to consult — Mednafen `TA_isrgb` forces
    // the CCE bit). So a 16bpp NBG0 dot blends with the backdrop.
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data); // NBG0
    sat.bus.write16(REG_CHCTLA, 0x0032, AccessKind::Data); // NBG0 bitmap, RGB555 (CHCN=3)
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA N0PRIN = 1
    // Backdrop = blue.
    sat.bus.vdp2.vram.write16(0x200, 0x7C00);
    sat.bus.write16(0x05F8_00AC, 0x0000, AccessKind::Data); // BKTAU
    sat.bus.write16(0x05F8_00AE, 0x0100, AccessKind::Data); // BKTAL word 0x100
    // Colour calc: enable (CCCTL bit 0), ratio 16, SFCCMD mode 3.
    sat.bus.write16(0x05F8_00EC, 0x0001, AccessKind::Data); // CCCTL N0 cc enable
    sat.bus.write16(0x05F8_0108, 0x0010, AccessKind::Data); // CCRNA N0 ratio = 16
    sat.bus.write16(0x05F8_00EE, 0x0003, AccessKind::Data); // SFCCMD NBG0 = 3
    // 16bpp RGB555 red dot (bit 15 clear — proves cc is on regardless of the MSB).
    sat.bus.vdp2.vram.write16((60 * 512 + 50) * 2, 0x001F);

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    // Red over blue at alpha = (31-16)*255/31 = 123 → [123, 0, 132].
    let px = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(
        &out[px..px + 4],
        &[123, 0, 132, 0xFF],
        "RGB direct dot blends under SFCCMD 3"
    );
}

#[test]
fn rbg0_special_color_calc_mode2_gates_blending_by_sfcode() {
    // Exercises the RBG0 special-function path: SFPRMD/SFCCMD layer index 4. As
    // for NBG, SFCCMD mode 2 = the bitmap special-cc bit (BMSCC, BMPNB) masked
    // off when the dot's palette code fails the SFCODE test. Identity rotation,
    // so screen (x,y) samples plane (x,y).
    const ONE: u32 = 1 << 16;
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0010, AccessKind::Data); // RBG0 only
    sat.bus.write16(0x05F8_002A, 0x1200, AccessKind::Data); // CHCTLB: R0BMEN + 8bpp
    sat.bus.write16(0x05F8_00FC, 0x0001, AccessKind::Data); // PRIR (RBG0 priority) = 1
    // Rotation parameter table at VRAM byte 0x40000; identity transform.
    sat.bus.write16(0x05F8_00BC, 0x0002, AccessKind::Data); // RPTAU
    sat.bus.write16(0x05F8_00BE, 0x0000, AccessKind::Data); // RPTAL
    for &(k, val) in &[
        (4u32, ONE),
        (5, ONE),
        (7, ONE),
        (11, ONE),
        (19, ONE),
        (20, ONE),
    ] {
        sat.bus.vdp2.vram.write32(0x40000 + k * 4, val);
    }
    // Backdrop = blue.
    sat.bus.vdp2.vram.write16(0x200, 0x7C00);
    sat.bus.write16(0x05F8_00AC, 0x0000, AccessKind::Data); // BKTAU
    sat.bus.write16(0x05F8_00AE, 0x0100, AccessKind::Data); // BKTAL word 0x100
    // CRAM[2] = CRAM[4] = red (both dots red; only their cc differs).
    sat.bus.vdp2.cram.write16(2 * 2, 0x001F);
    sat.bus.vdp2.cram.write16(4 * 2, 0x001F);
    // Colour calc for RBG0: enable (CCCTL bit 4), ratio 16 (CCRR).
    sat.bus.write16(0x05F8_00EC, 0x0010, AccessKind::Data); // CCCTL R0 cc enable
    sat.bus.write16(0x05F8_010C, 0x0010, AccessKind::Data); // CCRR ratio = 16
    // Special cc: BMSCC (BMPNB bit 4), SFCCMD RBG0 (bits 8..9) = mode 2.
    sat.bus.write16(0x05F8_002E, 0x0010, AccessKind::Data); // BMPNB: R0 BMSCC
    sat.bus.write16(0x05F8_00EE, 0x0200, AccessKind::Data); // SFCCMD RBG0 = 2
    sat.bus.write16(0x05F8_0024, 0x0000, AccessKind::Data); // SFSEL → SFCODE-A
    sat.bus.write16(0x05F8_0026, 0x0004, AccessKind::Data); // SFCODE-A = 0x04 (bit 2)
    sat.bus.vdp2.vram.write8(60 * 512 + 50, 4); // dot A: code 4 → (4>>1)&7=2 → bit set
    sat.bus.vdp2.vram.write8(60 * 512 + 52, 2); // dot B: code 2 → (2>>1)&7=1 → bit clear

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    // Dot A: cc on → red over blue at alpha = (31-16)*255/31 = 123.
    let a = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(
        &out[a..a + 4],
        &[123, 0, 132, 0xFF],
        "code 4: SFCODE bit set → blended"
    );
    // Dot B: cc gated off → opaque red.
    let b = (60 * FRAME_WIDTH + 52) * 4;
    assert_eq!(
        &out[b..b + 4],
        &[255, 0, 0, 0xFF],
        "code 2: SFCODE bit clear → opaque"
    );
}

#[test]
fn extended_color_calc_blends_front_over_second_third_average() {
    // Extended colour calc (CCCTL EXCEN, bit 10): the front layer's colour-calc
    // partner becomes the average of the 2nd and 3rd layers instead of just the
    // 2nd. NBG0 (front, red) over NBG1 (green) + the back screen (blue, the 3rd
    // "layer"): with EXCEN the partner is avg(green, blue) = (0,127,127); without
    // it, just green.
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data); // low-res (HRESO 0) → EXCC eligible
    sat.bus.write16(REG_BGON, 0x0003, AccessKind::Data); // NBG0 + NBG1
    sat.bus.write16(REG_CHCTLA, 0x1212, AccessKind::Data); // both bitmap, 8bpp
    sat.bus.write16(0x05F8_003C, 0x0010, AccessKind::Data); // MPOFN: NBG1 base offset 1
    sat.bus.write16(0x05F8_00F8, 0x0203, AccessKind::Data); // PRINA: N0=3 (front), N1=2
    // Back screen = blue.
    sat.bus.vdp2.vram.write16(0x200, 0x7C00);
    sat.bus.write16(0x05F8_00AC, 0x0000, AccessKind::Data); // BKTAU
    sat.bus.write16(0x05F8_00AE, 0x0100, AccessKind::Data); // BKTAL word 0x100
    sat.bus.vdp2.cram.write16(7 * 2, 0x001F); // CRAM[7] = red   (NBG0)
    sat.bus.vdp2.cram.write16(6 * 2, 0x03E0); // CRAM[6] = green (NBG1)
    // NBG0 cc enable + ratio 16; NBG1 cc bit set (the EXCC 2nd-layer condition).
    sat.bus.write16(0x05F8_0108, 0x0010, AccessKind::Data); // CCRNA N0 ratio = 16
    sat.bus.vdp2.vram.write8(60 * 512 + 50, 7); // NBG0 red
    sat.bus.vdp2.vram.write8(0x20000 + 60 * 512 + 50, 6); // NBG1 green

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    let px = (60 * FRAME_WIDTH + 50) * 4;

    // EXCEN off: front blends only with the 2nd layer (green) at alpha 123.
    sat.bus.write16(0x05F8_00EC, 0x0003, AccessKind::Data); // CCCTL: N0 + N1 cc, EXCEN off
    sat.run_frame(&mut out);
    assert_eq!(
        &out[px..px + 4],
        &[123, 132, 0, 0xFF],
        "EXCEN off → red over green"
    );

    // EXCEN on: front blends with avg(green, blue) = (0,127,127).
    sat.bus.write16(0x05F8_00EC, 0x0403, AccessKind::Data); // CCCTL: + EXCEN (bit 10)
    sat.run_frame(&mut out);
    assert_eq!(
        &out[px..px + 4],
        &[123, 65, 65, 0xFF],
        "EXCEN on → red over avg(green, blue)"
    );
}

#[test]
fn vram_cycle_pattern_gates_a_tile_layers_character_fetch() {
    // VRAM cycle-pattern (VCP) gating: a tile-NBG character fetch from a VRAM
    // bank the CYCx table doesn't grant reads as a transparent dummy → the layer
    // blanks to the backdrop. A minimal 8bpp tile NBG0 (NT + char both in bank0)
    // renders when bank0 is granted NBG0's CG slot, and vanishes when it isn't.
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(0x05F8_000E, 0x0300, AccessKind::Data); // RAMCTL: VRAM_Mode 3 → esb = bank
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data); // NBG0
    sat.bus.write16(REG_CHCTLA, 0x0010, AccessKind::Data); // NBG0 tile, 8bpp, 8×8 char
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA N0PRIN = 1
    // Backdrop = blue (so a blanked layer shows blue, not black).
    sat.bus.vdp2.vram.write16(0x200, 0x7C00);
    sat.bus.write16(0x05F8_00AC, 0x0000, AccessKind::Data); // BKTAU
    sat.bus.write16(0x05F8_00AE, 0x0100, AccessKind::Data); // BKTAL word 0x100
    sat.bus.vdp2.cram.write16(7 * 2, 0x001F); // CRAM[7] = red
    // 2-word pattern-name entry (0,0): char number 2 (8bpp → byte base 2×0x20 = 64).
    sat.bus.vdp2.vram.write32(0, 2);
    sat.bus.vdp2.vram.write8(64, 7); // char pixel (0,0) = palette index 7 (red)

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    let px = 0; // screen (0,0)

    // Grant: bank0 gets NBG0 name-table (code 0) + character (code 4) slots.
    sat.bus.write16(0x05F8_0010, 0x0400, AccessKind::Data); // CYCA0 bank0 slots: N0NT,N0CG,…
    sat.run_frame(&mut out);
    assert_eq!(
        &out[px..px + 4],
        &[255, 0, 0, 0xFF],
        "bank0 grants NBG0 CG → tile renders (red)"
    );

    // Deny: remove the NBG0 character slot (no bank holds code 4) → char fetch
    // dummied → transparent → backdrop blue shows.
    sat.bus.write16(0x05F8_0010, 0x0000, AccessKind::Data); // all slots N0NT, none = N0CG
    sat.run_frame(&mut out);
    assert_eq!(
        &out[px..px + 4],
        &[0, 0, 255, 0xFF],
        "no CG grant → NBG0 char blanks → backdrop"
    );
}

#[test]
fn nbg_8bpp_two_word_pn_palette_field_selects_the_256_colour_bank() {
    // 256-colour (8bpp): a 2-word pattern name's 7-bit palette field selects the
    // CRAM bank from bits [6:4] ONLY (a 256-entry palette spans CRAM addr [7:0],
    // so the low 4 bits are ignored) — the bank lands at CRAM addr [10:8].
    // Greatest Nine '98 regression: with palette field 0x10 the dot must read
    // CRAM bank 1 (entry 0x100 + index); the old code used the full field as
    // `<<8`, giving 0x1000 which folds (mod 0x1000 in CRAM mode 0) back to bank 0
    // — the scrambled team-flag previews. No prior test exercised a non-zero
    // 8bpp palette bank, which is how this slipped through.
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(0x05F8_000E, 0x0300, AccessKind::Data); // RAMCTL: VRAM mode 3, CRAM mode 0
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data); // NBG0
    sat.bus.write16(REG_CHCTLA, 0x0010, AccessKind::Data); // NBG0 tile, 8bpp, 8×8 char
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA N0PRIN = 1
    sat.bus.write16(0x05F8_0010, 0x0400, AccessKind::Data); // CYCA0 grant NBG0 NT + CG
    // Decoy in bank 0 (the address the over-shift wrongly folds to): green.
    sat.bus.vdp2.cram.write16(7 * 2, 0x03E0); // CRAM[7] = green (WRONG bank)
    // The correct 256-colour bank for palette field 0x10 is bank 1 → CRAM[0x107].
    sat.bus.vdp2.cram.write16((0x100 + 7) * 2, 0x001F); // CRAM[0x107] = red (RIGHT bank)
    // 2-word PN (0,0): char 2 (8bpp byte base 2×0x20=64), palette field 0x10.
    sat.bus.vdp2.vram.write32(0, (0x10 << 16) | 2);
    sat.bus.vdp2.vram.write8(64, 7); // pixel (0,0) = colour index 7
    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);
    assert_eq!(
        &out[0..4],
        &[255, 0, 0, 0xFF],
        "8bpp 2-word PN palette 0x10 → CRAM bank 1 (red), not the folded bank 0 (green)"
    );
}

/// Per-dot rotation coefficients (VF2's fight floor): with a VRAM bank
/// RDBS-granted for coefficient reads, the coefficient address walks
/// `DKAx` per dot — here two dots of one line read different table
/// entries, one of them transparent (bit 15 of the 1-word form). A
/// per-line-only read could never make dots of one line differ.
#[test]
fn rotation_coefficients_walk_per_dot_when_a_bank_is_granted() {
    const ONE: u32 = 1 << 16;
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0010, AccessKind::Data); // RBG0 only
    sat.bus.write16(0x05F8_002A, 0x1200, AccessKind::Data); // CHCTLB: R0BMEN + 8bpp
    sat.bus.write16(0x05F8_00FC, 0x0001, AccessKind::Data); // PRIR = 1
    // RDBS: bank A1 = coefficient (value 1 at bits 3-2), banks split.
    sat.bus.write16(0x05F8_000E, 0x0304, AccessKind::Data); // RAMCTL: VRxMD split + A1=COEFF
    // Rotation table at 0x40000 (bank B0): identity + KAst pointing at the
    // coefficient table in bank A1 (byte 0x20000 → entry 0x10000 → KAst raw
    // = entry << 16).
    sat.bus.write16(0x05F8_00BC, 0x0002, AccessKind::Data); // RPTAU
    sat.bus.write16(0x05F8_00BE, 0x0000, AccessKind::Data); // RPTAL
    for &(k, val) in &[
        (4u32, ONE),
        (5, ONE),
        (7, ONE),
        (11, ONE),
        (19, ONE),
        (20, ONE),
    ] {
        sat.bus.vdp2.vram.write32(0x40000 + k * 4, val);
    }
    sat.bus.vdp2.vram.write32(0x40000 + 23 * 4, 0x0001_0000); // DKAx = 1 entry/dot (.10 units ×1024, raw <<6)
    // KTCTL: RBG0 coefficient enable + 1-word size, mode 0 (kx & ky).
    sat.bus.write16(0x05F8_00B4, 0x0003, AccessKind::Data);
    // KTAOF param A = 1: the accumulator's `ktaof << 26` lands the table at
    // u16-word 0x10000 = byte 0x20000 = bank A1 (KAst alone spans one bank).
    sat.bus.write16(0x05F8_00B6, 0x0001, AccessKind::Data);
    // Coefficient table at bank A1 byte 0x20000 (KTAOF=1): dot 0 → 1.0
    // (0x0400 in the 1-word 5.10 form), dot 1 → transparent (bit 15).
    sat.bus.vdp2.vram.write16(0x20000, 0x0400);
    sat.bus.vdp2.vram.write16(0x20002, 0x8400);
    sat.bus.vdp2.vram.write16(0x20004, 0x0400);
    // Bitmap row 10: red dots everywhere (code 1).
    sat.bus.vdp2.cram.write16(2, 0x001F);
    for x in 0..16u32 {
        sat.bus.vdp2.vram.write8(10 * 512 + x, 1);
    }

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);
    let px = |x: usize| (10 * FRAME_WIDTH + x) * 4;
    assert_eq!(
        &out[px(0)..px(0) + 4],
        &[0xFF, 0, 0, 0xFF],
        "dot 0: coeff 1.0 → red"
    );
    assert_ne!(
        &out[px(1)..px(1) + 4],
        &[0xFF, 0, 0, 0xFF],
        "dot 1: per-dot transparent coefficient"
    );
    assert_eq!(
        &out[px(2)..px(2) + 4],
        &[0xFF, 0, 0, 0xFF],
        "dot 2: visible again"
    );
}

#[test]
fn rbg0_coefficient_table_supplies_the_line_colour_index() {
    // KTCTL bit 4 (coefficient-table line-colour enable): the per-dot line-colour
    // CRAM index comes from the top byte (bits 30..24) of each rotation
    // coefficient word, NOT the LCTA table (Mednafen `LB.lc = (coeff>>24)&0x7F`).
    // This was Wachenröder's 3D-battle white floor: the LCTA table was all-zero
    // so our (then LCTA-only) line colour resolved to CRAM[0]=light-grey, and the
    // RBG0 floor's additive colour-calc washed the scene white. The coefficient
    // word instead carries a dark line-colour index. Regression: with a coeff
    // line-index of 5 (green) and CRAM[0]=blue, the additive blend must produce
    // red+green=yellow (coeff path) — never red+blue=magenta (the stale LCTA path).
    const ONE: u32 = 1 << 16;
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0010, AccessKind::Data); // RBG0 only
    sat.bus.write16(0x05F8_002A, 0x1200, AccessKind::Data); // CHCTLB: R0BMEN + 8bpp
    sat.bus.write16(0x05F8_00FC, 0x0001, AccessKind::Data); // PRIR (RBG0 priority) = 1
    // Rotation parameter table at VRAM byte 0x20000 (word 0x10000): identity.
    sat.bus.write16(0x05F8_00BC, 0x0001, AccessKind::Data); // RPTAU → word 0x10000
    sat.bus.write16(0x05F8_00BE, 0x0000, AccessKind::Data); // RPTAL
    for &(k, val) in &[
        (4u32, ONE),
        (5, ONE),
        (7, ONE),
        (11, ONE),
        (19, ONE),
        (20, ONE),
    ] {
        sat.bus.vdp2.vram.write32(0x20000 + k * 4, val);
    }
    // KTCTL: coefficient enable (bit 0) + longword size (bit 1 = 0) + mode 0
    // (kx & ky) + line-colour-from-coefficient (bit 4). No RDBS bank grant →
    // per-line (non-bank-gated) coefficient read.
    sat.bus.write16(0x05F8_00B4, 0x0011, AccessKind::Data);
    sat.bus.write16(0x05F8_00B6, 0x0001, AccessKind::Data); // KTAOF param A = 1 → coeff @ byte 0x40000
    // Longword coefficient @ byte 0x40000: value 1.0 (identity scale) with the
    // line-colour index 5 packed into the top byte (0x05 << 24).
    sat.bus.vdp2.vram.write32(0x40000, 0x0501_0000);
    // Line-colour screen on for RBG0; LCTA table all-zero (→ index 0 = the stale
    // path's CRAM[0]).
    sat.bus.write16(0x05F8_00E8, 0x0010, AccessKind::Data); // LNCLEN: RBG0 (bit 4)
    sat.bus.write16(0x05F8_00A8, 0x0000, AccessKind::Data); // LCTAU
    sat.bus.write16(0x05F8_00AA, 0x0100, AccessKind::Data); // LCTAL: word 0x100 (clear)
    // Additive colour calc for RBG0 (CCCTL bit 4 = R0CCEN, bit 8 = CCMD add).
    sat.bus.write16(0x05F8_00EC, 0x0110, AccessKind::Data);
    sat.bus.vdp2.cram.write16(0, 0x7C00); // CRAM[0] = blue  (stale LCTA path)
    sat.bus.vdp2.cram.write16(2, 0x001F); // CRAM[1] = red   (the RBG0 dot)
    sat.bus.vdp2.cram.write16(5 * 2, 0x03E0); // CRAM[5] = green (coeff line colour)
    // Bitmap row 10: red dots (code 1).
    for x in 0..16u32 {
        sat.bus.vdp2.vram.write8(10 * 512 + x, 1);
    }

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    let px = (10 * FRAME_WIDTH + 5) * 4;
    assert_eq!(
        &out[px..px + 4],
        &[0xFF, 0xFF, 0, 0xFF],
        "RBG0 red dot + coefficient line colour (green, index 5) = yellow — \
         not red + CRAM[0] blue = magenta"
    );
}

/// RAMCTL.CRKTE: the coefficient table reads from the upper half of CRAM.
#[test]
fn rotation_coefficients_read_from_cram_when_crkte() {
    const ONE: u32 = 1 << 16;
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0010, AccessKind::Data);
    sat.bus.write16(0x05F8_002A, 0x1200, AccessKind::Data);
    sat.bus.write16(0x05F8_00FC, 0x0001, AccessKind::Data);
    sat.bus.write16(0x05F8_000E, 0x8000, AccessKind::Data); // RAMCTL.CRKTE
    sat.bus.write16(0x05F8_00BC, 0x0002, AccessKind::Data);
    sat.bus.write16(0x05F8_00BE, 0x0000, AccessKind::Data);
    for &(k, val) in &[
        (4u32, ONE),
        (5, ONE),
        (7, ONE),
        (11, ONE),
        (19, ONE),
        (20, ONE),
    ] {
        sat.bus.vdp2.vram.write32(0x40000 + k * 4, val);
    }
    sat.bus.write16(0x05F8_00B4, 0x0003, AccessKind::Data); // coeff on, 1-word
    // CRAM upper half entry 0: transparent → the whole layer vanishes; a
    // VRAM-resident table would have read 0 (opaque, scale 0) instead.
    sat.bus.vdp2.cram.write16(0x800, 0x8400);
    sat.bus.vdp2.cram.write16(2, 0x001F);
    sat.bus.vdp2.vram.write8(10 * 512 + 10, 1);

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);
    let px = (10 * FRAME_WIDTH + 10) * 4;
    assert_ne!(
        &out[px..px + 4],
        &[0xFF, 0, 0, 0xFF],
        "CRKTE coefficient (transparent) must gate the dot"
    );
}

/// In double-density interlace the rotation accumulators advance once per
/// *field* line (display line >> 1) — feeding the raw display line walks the
/// coefficient table at twice the hardware rate (VF2's fight floor started
/// at 31% of the screen instead of ~61%, with a doubly-steep perspective).
/// Coefficient table: entry 0 transparent, entry 1 solid, DKAst = 1
/// entry/line → display lines 0-1 must be transparent and 2-3 solid.
#[test]
fn rotation_coefficient_lines_halve_in_double_density_interlace() {
    const ONE: u32 = 1 << 16;
    let mut sat = Saturn::with_blank_bios();
    sat.halt_slave();
    sat.bus.write16(REG_TVMD, 0x80C0, AccessKind::Data); // DISP | LSMD=11 (DD), 320
    sat.bus.write16(REG_BGON, 0x0010, AccessKind::Data); // RBG0
    sat.bus.write16(0x05F8_002A, 0x1200, AccessKind::Data); // R0BMEN + 8bpp
    sat.bus.write16(0x05F8_00FC, 0x0001, AccessKind::Data); // PRIR = 1
    sat.bus.write16(0x05F8_00BC, 0x0002, AccessKind::Data); // RPTA → 0x40000
    sat.bus.write16(0x05F8_00BE, 0x0000, AccessKind::Data);
    for &(k, val) in &[
        (4u32, ONE),
        (5, ONE),
        (7, ONE),
        (11, ONE),
        (19, ONE),
        (20, ONE),
    ] {
        sat.bus.vdp2.vram.write32(0x40000 + k * 4, val);
    }
    sat.bus.vdp2.vram.write32(0x40000 + 22 * 4, 0x0001_0000); // DKAst = 1 entry/line
    sat.bus.write16(0x05F8_00B4, 0x0003, AccessKind::Data); // coeff on, 1-word, mode 0
    sat.bus.write16(0x05F8_00B6, 0x0001, AccessKind::Data); // KTAOF=1 → bank A1
    sat.bus.vdp2.vram.write16(0x20000, 0x8400); // line 0: transparent
    sat.bus.vdp2.vram.write16(0x20002, 0x0400); // line 1: 1.0
    sat.bus.vdp2.vram.write16(0x20004, 0x0400);
    sat.bus.write16(0x05F8_000E, 0x0304, AccessKind::Data); // RDBS A1=COEFF (per-dot on, DKAx=0)
    // Solid red bitmap everywhere.
    sat.bus.vdp2.cram.write16(2, 0x001F);
    for y in 0..4u32 {
        for x in 0..8u32 {
            sat.bus.vdp2.vram.write8(y * 512 + x, 1);
        }
    }

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);
    let px = |x: usize, y: usize| (y * 320 + x) * 4;
    let red = [0xFFu8, 0, 0, 0xFF];
    assert_ne!(
        &out[px(4, 0)..px(4, 0) + 4],
        &red,
        "display 0 → coeff line 0 (transparent)"
    );
    assert_ne!(
        &out[px(4, 1)..px(4, 1) + 4],
        &red,
        "display 1 → still coeff line 0"
    );
    assert_eq!(
        &out[px(4, 2)..px(4, 2) + 4],
        &red,
        "display 2 → coeff line 1 (solid)"
    );
    assert_eq!(
        &out[px(4, 3)..px(4, 3) + 4],
        &red,
        "display 3 → still coeff line 1"
    );
}
