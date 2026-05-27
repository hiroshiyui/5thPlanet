//! VDP2 frame renderer — composites the enabled NBG layers into an
//! RGBA8888 framebuffer from the current VRAM / CRAM / register state.
//!
//! Scope (M5 task #4, increment 1 — multi-layer compositing):
//!
//! - **NBG0–3**, each in cell (tile) or — for NBG0/1 — bitmap format,
//!   sampled per pixel and **composited by priority**: the highest
//!   `PRINA`/`PRINB` priority with a non-transparent dot wins; ties resolve
//!   to the lower-numbered layer (NBG0 frontmost), matching VDP2's default
//!   order. A layer with priority 0 is not displayed.
//! - **Colour formats**: 4bpp / 8bpp paletted (CRAM mode 0, RGB555) for both
//!   tile and bitmap, plus 16bpp RGB direct-colour for bitmap. Palette index
//!   0 (paletted) / value 0 (RGB) is transparent.
//! - **Backdrop** = CRAM index 0 (the real BKTAU/BKTAL backdrop register is
//!   a later refinement; palette entry 0 is what splash software programs).
//! - **Scrolling**: integer NBG scroll; fractional scroll and zoom ignored.
//! - **NTSC low-res** (320×224).
//!
//! Deferred to later increments: RBG0/1 rotation, the VDP1 sprite layer,
//! windows, line-scroll, colour calculation, 2-word pattern names, 2×2-cell
//! characters, plane sizes beyond the single 512×512 page, the 8bpp-tile
//! colour bank, and CRAM modes 1/2.
//!
//! `render_frame` is `&Vdp2 -> &mut [u8]` — pure, no allocation.

use super::{Vdp2, cram};

pub const FRAME_WIDTH: usize = 320;
pub const FRAME_HEIGHT: usize = 224;
pub const FRAMEBUFFER_BYTES: usize = FRAME_WIDTH * FRAME_HEIGHT * 4;

const BACKDROP_PALETTE_INDEX: usize = 0;

/// One tile plane is 64×64 cells of 8×8 px = 512×512 px; scroll wraps here.
/// (Larger plane sizes from PLSZ are a later refinement.)
const TILE_PLANE_WIDTH_PX: u32 = 64 * 8;

/// Render one frame of NTSC low-res into `out`. Panics if `out`'s length
/// isn't exactly [`FRAMEBUFFER_BYTES`].
pub fn render_frame(vdp2: &Vdp2, out: &mut [u8]) {
    assert_eq!(
        out.len(),
        FRAMEBUFFER_BYTES,
        "framebuffer must be 320×224×4"
    );

    if !vdp2.regs.display_enabled() {
        // Opaque black so SDL doesn't show a transparent hole.
        for px in out.chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 0, 0, 0xFF]);
        }
        return;
    }

    let backdrop = vdp2.cram.color_rgb888_mode0(BACKDROP_PALETTE_INDEX);

    for y in 0..FRAME_HEIGHT {
        for x in 0..FRAME_WIDTH {
            // Pick the highest-priority non-transparent NBG dot; ties go to
            // the lower-numbered layer (it's visited first and only a strictly
            // higher priority replaces it).
            let mut winner: Option<(u8, (u8, u8, u8))> = None;
            for n in 0..4 {
                if !vdp2.regs.nbg_enabled(n) {
                    continue;
                }
                let pri = vdp2.regs.nbg_priority(n);
                if pri == 0 {
                    continue;
                }
                if winner.map(|(wp, _)| pri <= wp).unwrap_or(false) {
                    continue; // can't beat the current winner
                }
                if let Some(rgb) = sample_nbg(vdp2, n, x as u32, y as u32) {
                    winner = Some((pri, rgb));
                }
            }
            let (r, g, b) = winner.map(|(_, rgb)| rgb).unwrap_or(backdrop);
            put_pixel(out, x, y, r, g, b);
        }
    }
}

#[inline]
fn put_pixel(out: &mut [u8], x: usize, y: usize, r: u8, g: u8, b: u8) {
    let dst = (y * FRAME_WIDTH + x) * 4;
    out[dst] = r;
    out[dst + 1] = g;
    out[dst + 2] = b;
    out[dst + 3] = 0xFF;
}

/// Sample NBG`n` at screen `(x, y)`, returning `None` for a transparent dot.
fn sample_nbg(vdp2: &Vdp2, n: usize, x: u32, y: u32) -> Option<(u8, u8, u8)> {
    let (scroll_x, scroll_y) = vdp2.regs.nbg_scroll(n);
    let depth = vdp2.regs.nbg_color_mode(n);
    let sx = x + scroll_x;
    let sy = y + scroll_y;
    if vdp2.regs.nbg_bitmap_enabled(n) {
        sample_bitmap(vdp2, n, depth, sx, sy)
    } else {
        sample_tile(vdp2, n, depth, sx, sy)
    }
}

/// Bitmap dimensions in pixels for the `N*BMSZ` size code.
fn bitmap_dims(size: u8) -> (u32, u32) {
    match size & 3 {
        0 => (512, 256),
        1 => (512, 512),
        2 => (1024, 256),
        _ => (1024, 512),
    }
}

fn sample_bitmap(vdp2: &Vdp2, n: usize, depth: u8, sx: u32, sy: u32) -> Option<(u8, u8, u8)> {
    let base = vdp2.regs.nbg_bitmap_base(n);
    let (w, h) = bitmap_dims(vdp2.regs.nbg_bitmap_size(n));
    let (px, py) = (sx % w, sy % h);
    match depth {
        // 16bpp RGB555 direct colour.
        3 => {
            let off = base + (py * w + px) * 2;
            let entry = vdp2.vram.read16(off);
            (entry & 0x7FFF != 0).then(|| cram::rgb555_to_888(entry))
        }
        // 8bpp paletted (256 colour).
        1 => {
            let idx = vdp2.vram.read8(base + py * w + px) as usize;
            (idx != 0).then(|| vdp2.cram.color_rgb888_mode0(idx))
        }
        // 4bpp paletted (16 colour). The BMPNA palette bank is a later
        // refinement; the nibble indexes the low palette directly.
        _ => {
            let byte = vdp2.vram.read8(base + (py * w + px) / 2);
            let nibble = if px & 1 == 0 { byte >> 4 } else { byte & 0xF } as usize;
            (nibble != 0).then(|| vdp2.cram.color_rgb888_mode0(nibble))
        }
    }
}

fn sample_tile(vdp2: &Vdp2, n: usize, depth: u8, sx: u32, sy: u32) -> Option<(u8, u8, u8)> {
    let pn_base = vdp2.regs.nbg_pattern_table_base(n);
    let src_x = sx % TILE_PLANE_WIDTH_PX;
    let src_y = sy % TILE_PLANE_WIDTH_PX;
    let (tile_x, in_x) = (src_x / 8, src_x % 8);
    let (tile_y, in_y) = (src_y / 8, src_y % 8);

    // 1-word pattern name: char number (bits 9..0), palette bank (15..12).
    // Flip flags / supplement bits / 2-word format are later increments.
    let pn_off = pn_base + (tile_y * 64 + tile_x) * 2;
    let pn = vdp2.vram.read16(pn_off);
    let char_num = (pn & 0x03FF) as u32;
    let palette_bank = ((pn >> 12) & 0xF) as usize;

    if depth == 1 {
        // 8bpp cell: 64 bytes/cell, one byte/pixel.
        let byte = vdp2.vram.read8(char_num * 64 + in_y * 8 + in_x) as usize;
        (byte != 0).then(|| vdp2.cram.color_rgb888_mode0(byte))
    } else {
        // 4bpp cell: 32 bytes/cell, two pixels/byte (high nibble = even col).
        let byte = vdp2.vram.read8(char_num * 32 + in_y * 4 + in_x / 2);
        let nibble = if in_x & 1 == 0 { byte >> 4 } else { byte & 0xF } as usize;
        (nibble != 0).then(|| vdp2.cram.color_rgb888_mode0((palette_bank << 4) | nibble))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_buf() -> Vec<u8> {
        vec![0xCD; FRAMEBUFFER_BYTES]
    }

    fn pixel(buf: &[u8], x: usize, y: usize) -> [u8; 4] {
        let o = (y * FRAME_WIDTH + x) * 4;
        [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
    }

    /// Enable display + NBG0 with priority 1 (so it actually composites).
    fn enable_nbg0(v: &mut Vdp2) {
        v.regs.write16(0x000, 0x8000); // TVMD.DISP
        v.regs.write16(0x020, 0x0001); // BGON.NBG0
        v.regs.write16(0x0F8, 0x0001); // PRINA.N0PRIN = 1
    }

    #[test]
    fn display_disabled_emits_opaque_black() {
        let v = Vdp2::new();
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        for chunk in buf.chunks_exact(4) {
            assert_eq!(chunk, &[0, 0, 0, 0xFF]);
        }
    }

    #[test]
    fn display_enabled_no_layer_fills_with_backdrop() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.cram.write16(0, 0x001F); // backdrop = red
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        for chunk in buf.chunks_exact(4) {
            assert_eq!(chunk, &[0xFF, 0, 0, 0xFF]);
        }
    }

    #[test]
    fn priority_zero_layer_is_not_displayed() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0001); // NBG0 on
        v.regs.write16(0x028, 0x0002); // bitmap
        v.regs.write16(0x0F8, 0x0000); // PRINA.N0PRIN = 0 → hidden
        v.cram.write16(0, 0x1F << 5); // backdrop green
        v.cram.write16(2, 0x001F); // index 1 red
        v.vram.write8(0, 1);
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(
            pixel(&buf, 0, 0),
            [0, 0xFF, 0, 0xFF],
            "priority-0 → backdrop"
        );
    }

    #[test]
    fn bitmap_mode_renders_palette_indices_from_vram() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // N0BMEN + N0CHCN=1 (8bpp)
        v.cram.write16(0, 0x0000);
        v.cram.write16(2, 0x1F << 5); // green
        v.cram.write16(4, 0x7C00); // blue
        v.vram.write8(5u32 * 512 + 10, 1); // bitmap pitch 512 (size 0)
        v.vram.write8(100u32 * 512 + 200, 2);
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(pixel(&buf, 10, 5), [0, 0xFF, 0, 0xFF], "green");
        assert_eq!(pixel(&buf, 200, 100), [0, 0, 0xFF, 0xFF], "blue");
    }

    #[test]
    fn bitmap_16bpp_direct_colour() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        // N0BMEN + N0CHCN = 3 (32K colour): CHCTLA = bmen(0x2) | (3<<4)=0x30.
        v.regs.write16(0x028, 0x0032);
        v.vram.write16(5u32 * 512 * 2 + 10 * 2, 0x001F); // (10,5) = red
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(pixel(&buf, 10, 5), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn bitmap_base_follows_map_offset_register() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // N0BMEN + 8bpp
        v.regs.write16(0x03C, 0x0001); // MPOFN.N0MP = 1 → base 0x20000
        v.cram.write16(2, 0x001F); // index 1 red
        v.vram.write8(0x2_0000, 1);
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn bitmap_integer_scroll_shifts_source() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // N0BMEN + 8bpp
        v.regs.write16(0x070, 0x0002); // SCXIN0 = 2
        v.regs.write16(0x074, 0x0003); // SCYIN0 = 3
        v.cram.write16(2, 0x001F); // red
        v.vram.write8(3 * 512 + 2, 1);
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn tile_mode_resolves_pattern_to_character_to_palette() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0000); // tile mode, 4bpp
        // PN at tile (0,0): char 5, palette bank 2 (default map base 0).
        v.vram.write16(0, (2 << 12) | 5);
        // Char 5 pixel (3,4): row 4, col 3 → byte offset 1, low nibble = 7.
        v.vram.write8(5 * 32 + 4 * 4 + 1, 0x07);
        v.cram.write16(0x27 * 2, 0x001F); // index (2<<4)|7 = 0x27 → red
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(pixel(&buf, 3, 4), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn tile_8bpp_uses_full_byte_index() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0010); // N0CHCN = 1 (256 colour)
        v.vram.write16(0, 3); // char 3 at tile (0,0)
        v.vram.write8(3 * 64, 0x42); // pixel (0,0) = index 0x42
        v.cram.write16(0x42 * 2, 0x001F); // red
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn higher_priority_layer_wins_the_pixel() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0003); // NBG0 + NBG1 on
        v.regs.write16(0x028, 0x1212); // N0/N1 BMEN + 8bpp (N0CHCN+N1CHCN=1)
        // NBG0 priority 2, NBG1 priority 5 → NBG1 wins where both opaque.
        v.regs.write16(0x0F8, 0x0502); // N0PRIN=2, N1PRIN=5
        // NBG1 bitmap base via MPOFN.N1MP (bits 5..4) = 1 → 0x20000.
        v.regs.write16(0x03C, 0x0010);
        v.cram.write16(2, 0x001F); // index 1 red  (NBG0)
        v.cram.write16(4, 0x7C00); // index 2 blue (NBG1)
        v.vram.write8(0, 1); // NBG0 dot at (0,0)
        v.vram.write8(0x2_0000, 2); // NBG1 dot at (0,0)
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0, 0, 0xFF, 0xFF], "NBG1 (pri 5) wins");
    }

    #[test]
    fn lower_layer_shows_through_transparent_higher_layer() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000);
        v.regs.write16(0x020, 0x0003); // NBG0 + NBG1
        v.regs.write16(0x028, 0x1212); // both bitmap, 8bpp
        v.regs.write16(0x0F8, 0x0502); // NBG1 higher priority
        v.regs.write16(0x03C, 0x0010); // NBG1 base 0x20000
        v.cram.write16(2, 0x001F); // red (NBG0)
        v.vram.write8(0, 1); // NBG0 opaque red at (0,0)
        v.vram.write8(0x2_0000, 0); // NBG1 transparent at (0,0)
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(
            pixel(&buf, 0, 0),
            [0xFF, 0, 0, 0xFF],
            "NBG1 transparent → NBG0 shows"
        );
    }

    #[test]
    fn nbg0_disabled_leaves_backdrop_intact() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0000); // NBG0 off
        v.cram.write16(0, 0x1F << 5); // backdrop green
        v.vram.write8(0, 1);
        v.cram.write16(2, 0x7C00);
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0, 0xFF, 0, 0xFF]);
    }

    #[test]
    #[should_panic(expected = "framebuffer must be 320×224×4")]
    fn wrong_buffer_size_panics_loudly() {
        let v = Vdp2::new();
        let mut tiny = [0u8; 64];
        render_frame(&v, &mut tiny);
    }
}
