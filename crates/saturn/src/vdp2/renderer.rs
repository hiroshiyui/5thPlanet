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
//! The **VDP1 sprite layer** is composited too: VDP2 reads the VDP1 frame
//! buffer per pixel, splits each word per the SPCTL sprite type into a
//! colour code / RGB value and a priority (from PRISA..PRISD), and the sprite
//! layer joins the priority race frontmost on ties (sprite > NBG0 > …).
//!
//! The **rotation backgrounds RBG0/RBG1** are composited via [`super::rotation`]:
//! each screen dot is mapped through the rotation parameter table's affine
//! transform, then the rotation plane (bitmap or single-page tile) is sampled.
//! RBG0 uses parameter set A (priority PRIR); RBG1 uses set B (priority N0PRIN).
//! Full layer order: sprite > RBG0 > NBG0 > RBG1 > NBG1 > NBG2 > NBG3.
//!
//! Deferred to later increments: the line-coefficient table (per-line scaling)
//! and dual-parameter window selection, windows, line-scroll, colour
//! calculation / sprite alpha + shadow, 2-word pattern names, 2×2-cell
//! characters, plane sizes beyond the single 512×512 page, the 8bpp-tile
//! colour bank, and CRAM modes 1/2.
//!
//! `render_frame` is pure (no allocation); the sprite source is the VDP1
//! frame buffer, supplied by the [`crate::system::Saturn`] aggregate.

use super::rotation::RotationParams;
use super::{Vdp2, cram};
use crate::vdp1::Framebuffer;

pub const FRAME_WIDTH: usize = 320;
pub const FRAME_HEIGHT: usize = 224;
pub const FRAMEBUFFER_BYTES: usize = FRAME_WIDTH * FRAME_HEIGHT * 4;

const BACKDROP_PALETTE_INDEX: usize = 0;

/// One tile plane is 64×64 cells of 8×8 px = 512×512 px; scroll wraps here.
/// (Larger plane sizes from PLSZ are a later refinement.)
const TILE_PLANE_WIDTH_PX: u32 = 64 * 8;

/// Render one frame of NTSC low-res into `out`, compositing the enabled NBG
/// layers and the VDP1 sprite layer (`sprite_fb`, `None` when there's no VDP1
/// frame buffer to read). Panics if `out`'s length isn't [`FRAMEBUFFER_BYTES`].
pub fn render_frame(vdp2: &Vdp2, sprite_fb: Option<&Framebuffer>, out: &mut [u8]) {
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
            let (sx, sy) = (x as u32, y as u32);
            // Evaluate layers in VDP2's default front-to-back order; the
            // first dot at a given priority wins (consider only replaces on a
            // strictly higher priority): sprite > RBG0 > NBG0 > RBG1 > NBG1..3.
            let mut winner: Option<(u8, (u8, u8, u8))> = None;
            consider(
                &mut winner,
                sprite_fb.and_then(|fb| sample_sprite(vdp2, fb, sx, sy)),
            );
            consider(&mut winner, rbg_layer(vdp2, 0, sx, sy));
            consider(&mut winner, nbg_layer(vdp2, 0, sx, sy));
            consider(&mut winner, rbg_layer(vdp2, 1, sx, sy));
            consider(&mut winner, nbg_layer(vdp2, 1, sx, sy));
            consider(&mut winner, nbg_layer(vdp2, 2, sx, sy));
            consider(&mut winner, nbg_layer(vdp2, 3, sx, sy));
            let (r, g, b) = winner.map(|(_, rgb)| rgb).unwrap_or(backdrop);
            put_pixel(out, x, y, r, g, b);
        }
    }
}

/// Replace `winner` with `cand` only if `cand` has a strictly higher (nonzero)
/// priority — so layers evaluated earlier win ties.
fn consider(winner: &mut Option<(u8, (u8, u8, u8))>, cand: Option<(u8, (u8, u8, u8))>) {
    if let Some((p, c)) = cand
        && p != 0
        && winner.is_none_or(|(wp, _)| p > wp)
    {
        *winner = Some((p, c));
    }
}

/// An enabled NBG layer's (priority, colour) at `(x, y)`, or `None`.
fn nbg_layer(vdp2: &Vdp2, n: usize, x: u32, y: u32) -> Option<(u8, (u8, u8, u8))> {
    if !vdp2.regs.nbg_enabled(n) {
        return None;
    }
    let pri = vdp2.regs.nbg_priority(n);
    (pri != 0).then(|| sample_nbg(vdp2, n, x, y).map(|c| (pri, c)))?
}

/// An enabled rotation layer's (priority, colour) at `(x, y)`, or `None`.
fn rbg_layer(vdp2: &Vdp2, which: usize, x: u32, y: u32) -> Option<(u8, (u8, u8, u8))> {
    if !vdp2.regs.rbg_enabled(which) {
        return None;
    }
    let pri = vdp2.regs.rbg_priority(which);
    (pri != 0).then(|| sample_rbg(vdp2, which, x, y).map(|c| (pri, c)))?
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

// Sprite-data type tables (VDP2 manual §"Sprite Data", values per MAME's
// `saturn_v.cpp`): for each of the 16 SPCTL types, the colour-code mask and
// the shift/mask that select which frame-buffer bits index the eight sprite
// priority registers.
const SPRITE_COLORMASK: [u16; 16] = [
    0x07FF, 0x07FF, 0x07FF, 0x07FF, 0x03FF, 0x07FF, 0x03FF, 0x01FF, 0x007F, 0x003F, 0x003F, 0x003F,
    0x00FF, 0x00FF, 0x00FF, 0x00FF,
];
const SPRITE_PRIO_SHIFT: [u16; 16] = [14, 13, 14, 13, 13, 12, 12, 12, 7, 7, 6, 0, 7, 7, 6, 0];
const SPRITE_PRIO_MASK: [u16; 16] = [3, 7, 1, 3, 3, 7, 7, 7, 1, 1, 3, 0, 1, 1, 3, 0];

/// Sample the VDP1 sprite layer at screen `(x, y)`: read the frame-buffer
/// word, decode colour + priority per the SPCTL sprite type, and return
/// `None` for a transparent dot or a priority-0 (hidden) sprite.
fn sample_sprite(vdp2: &Vdp2, fb: &Framebuffer, x: u32, y: u32) -> Option<(u8, (u8, u8, u8))> {
    let pix = fb.pixel(x as i32, y as i32);
    if pix == 0 {
        return None; // nothing plotted here
    }
    let stype = vdp2.regs.sprite_type();

    // RGB direct colour: MSB set and SPCLMD enabled. Priority comes from
    // sprite register 0.
    if pix & 0x8000 != 0 && vdp2.regs.sprite_rgb_mode() {
        let pri = vdp2.regs.sprite_priority(0);
        return (pri != 0).then(|| (pri, cram::rgb555_to_888(pix)));
    }

    // Palette code: priority bits index PRISA..PRISD; the masked low bits are
    // a CRAM colour code (0 = transparent).
    let pidx = ((pix >> SPRITE_PRIO_SHIFT[stype]) & SPRITE_PRIO_MASK[stype]) as usize;
    let pri = vdp2.regs.sprite_priority(pidx);
    if pri == 0 {
        return None;
    }
    let code = (pix & SPRITE_COLORMASK[stype]) as usize;
    (code != 0).then(|| (pri, vdp2.cram.color_rgb888_mode0(code)))
}

/// Sample rotation background `which` at screen `(x, y)`: transform through
/// its parameter table, then read the rotation plane (bitmap or tile).
fn sample_rbg(vdp2: &Vdp2, which: usize, x: u32, y: u32) -> Option<(u8, u8, u8)> {
    let rp = RotationParams::read(&vdp2.vram, vdp2.regs.rotation_table_addr(), which);
    let (plane_x, plane_y) = rp.transform(x as i32, y as i32);
    let depth = vdp2.regs.rbg_color_mode();
    if vdp2.regs.rbg_bitmap_enabled() {
        sample_rot_bitmap(vdp2, which, depth, plane_x, plane_y)
    } else {
        sample_rot_tile(vdp2, which, depth, plane_x, plane_y)
    }
}

fn sample_rot_bitmap(
    vdp2: &Vdp2,
    which: usize,
    depth: u8,
    plane_x: i32,
    plane_y: i32,
) -> Option<(u8, u8, u8)> {
    let base = vdp2.regs.rbg_bitmap_base(which);
    let w: i32 = 512;
    let h: i32 = if vdp2.regs.rbg_bitmap_size() == 0 {
        256
    } else {
        512
    };
    let px = plane_x.rem_euclid(w) as u32;
    let py = plane_y.rem_euclid(h) as u32;
    let w = w as u32;
    match depth {
        3 => {
            let entry = vdp2.vram.read16(base + (py * w + px) * 2);
            (entry & 0x7FFF != 0).then(|| cram::rgb555_to_888(entry))
        }
        1 => {
            let idx = vdp2.vram.read8(base + py * w + px) as usize;
            (idx != 0).then(|| vdp2.cram.color_rgb888_mode0(idx))
        }
        _ => {
            let byte = vdp2.vram.read8(base + (py * w + px) / 2);
            let nibble = if px & 1 == 0 { byte >> 4 } else { byte & 0xF } as usize;
            (nibble != 0).then(|| vdp2.cram.color_rgb888_mode0(nibble))
        }
    }
}

fn sample_rot_tile(
    vdp2: &Vdp2,
    which: usize,
    depth: u8,
    plane_x: i32,
    plane_y: i32,
) -> Option<(u8, u8, u8)> {
    // Single 512×512 page (full 4×4-page composition is a later refinement).
    // Plane-A map number: RA from MPABRA (0x050), RB from MPABRB (0x060).
    let mpab = vdp2.regs.rbg_plane_a_map(which);
    let pn_base = ((vdp2.regs.rbg_map_offset(which) << 6) | (mpab & 0x3F) as u32) * 0x2000;
    let sx = plane_x.rem_euclid(TILE_PLANE_WIDTH_PX as i32) as u32;
    let sy = plane_y.rem_euclid(TILE_PLANE_WIDTH_PX as i32) as u32;
    let (tile_x, in_x) = (sx / 8, sx % 8);
    let (tile_y, in_y) = (sy / 8, sy % 8);
    let pn = vdp2.vram.read16(pn_base + (tile_y * 64 + tile_x) * 2);
    let char_num = (pn & 0x03FF) as u32;
    let palette_bank = ((pn >> 12) & 0xF) as usize;
    if depth == 1 {
        let byte = vdp2.vram.read8(char_num * 64 + in_y * 8 + in_x) as usize;
        (byte != 0).then(|| vdp2.cram.color_rgb888_mode0(byte))
    } else {
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
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
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0, 0xFF, 0, 0xFF]);
    }

    // ---- sprite layer (VDP1 frame buffer) ----

    /// A VDP1 frame buffer with one RGB555 dot plotted at `(x, y)`.
    fn sprite_fb_with(x: i32, y: i32, dot: u16) -> Framebuffer {
        let mut fb = Framebuffer::new();
        fb.set_pixel(x, y, dot);
        fb
    }

    #[test]
    fn sprite_palette_dot_composites_over_backdrop() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x0F0, 0x0003); // PRISA.S0PRIN = 3
        v.cram.write16(0x12 * 2, 0x001F); // palette code 0x12 = red
        // Type 0: colour code = pix & 0x7FF; priority bits 15..14 = 0 → S0.
        let fb = sprite_fb_with(40, 30, 0x0012);
        let mut buf = fresh_buf();
        render_frame(&v, Some(&fb), &mut buf);
        assert_eq!(pixel(&buf, 40, 30), [0xFF, 0, 0, 0xFF], "sprite dot");
        assert_eq!(pixel(&buf, 41, 30), [0, 0, 0, 0xFF], "elsewhere = backdrop");
    }

    #[test]
    fn sprite_rgb_dot_uses_direct_colour_when_spclmd_set() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x0E0, 0x0020); // SPCTL.SPCLMD (RGB enable), type 0
        v.regs.write16(0x0F0, 0x0001); // S0PRIN = 1
        // MSB set → RGB direct: 0x8000 | red(0x1F).
        let fb = sprite_fb_with(10, 10, 0x801F);
        let mut buf = fresh_buf();
        render_frame(&v, Some(&fb), &mut buf);
        assert_eq!(pixel(&buf, 10, 10), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn sprite_beats_nbg_of_equal_priority() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0001); // NBG0 on (bitmap)
        v.regs.write16(0x028, 0x0012); // N0BMEN + 8bpp
        v.regs.write16(0x0F8, 0x0003); // NBG0 priority 3
        v.regs.write16(0x0F0, 0x0003); // sprite S0 priority 3 (tie)
        v.cram.write16(2, 0x7C00); // NBG0 index 1 = blue
        v.cram.write16(0x12 * 2, 0x001F); // sprite code 0x12 = red
        v.vram.write8(0, 1); // NBG0 dot at (0,0)
        let fb = sprite_fb_with(0, 0, 0x0012);
        let mut buf = fresh_buf();
        render_frame(&v, Some(&fb), &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF], "sprite wins the tie");
    }

    #[test]
    fn higher_priority_nbg_covers_the_sprite() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000);
        v.regs.write16(0x020, 0x0001);
        v.regs.write16(0x028, 0x0012);
        v.regs.write16(0x0F8, 0x0005); // NBG0 priority 5
        v.regs.write16(0x0F0, 0x0003); // sprite priority 3
        v.cram.write16(2, 0x7C00); // NBG0 blue
        v.cram.write16(0x12 * 2, 0x001F); // sprite red
        v.vram.write8(0, 1);
        let fb = sprite_fb_with(0, 0, 0x0012);
        let mut buf = fresh_buf();
        render_frame(&v, Some(&fb), &mut buf);
        assert_eq!(
            pixel(&buf, 0, 0),
            [0, 0, 0xFF, 0xFF],
            "NBG0 (pri 5) covers sprite"
        );
    }

    #[test]
    fn priority_zero_sprite_is_not_shown() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000);
        v.cram.write16(0, 0x1F << 5); // backdrop green
        v.cram.write16(0x12 * 2, 0x001F);
        // S0PRIN defaults to 0 → sprite hidden.
        let fb = sprite_fb_with(5, 5, 0x0012);
        let mut buf = fresh_buf();
        render_frame(&v, Some(&fb), &mut buf);
        assert_eq!(
            pixel(&buf, 5, 5),
            [0, 0xFF, 0, 0xFF],
            "priority-0 sprite → backdrop"
        );
    }

    #[test]
    fn empty_sprite_buffer_leaves_nbg_untouched() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // 8bpp bitmap
        v.cram.write16(2, 0x001F); // red
        v.vram.write8(0, 1);
        let fb = Framebuffer::new(); // all transparent
        let mut buf = fresh_buf();
        render_frame(&v, Some(&fb), &mut buf);
        assert_eq!(
            pixel(&buf, 0, 0),
            [0xFF, 0, 0, 0xFF],
            "NBG0 shows; no sprite"
        );
    }

    // ---- rotation backgrounds (RBG0 / RBG1) ----

    /// Write an identity rotation parameter set (dx = dyst = A = E = kx = ky =
    /// 1.0) for parameter A (which=0, base 0) or B (which=1, base 0x80).
    fn setup_rot_identity(v: &mut Vdp2, which: usize) {
        let base = if which == 0 { 0 } else { 0x80 };
        for (k, val) in [
            (4u32, 1 << 16),
            (5, 1 << 16),
            (7, 1 << 16),
            (11, 1 << 16),
            (19, 1 << 16),
            (20, 1 << 16),
        ] {
            v.vram.write32(base + k * 4, val);
        }
    }

    #[test]
    fn rbg0_identity_bitmap_maps_screen_to_plane() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0010); // BGON.R0ON
        v.regs.write16(0x02A, 0x1200); // CHCTLB: R0BMEN (bit9) + R0CHCN=1 (8bpp)
        v.regs.write16(0x0FC, 0x0001); // PRIR.R0PRIN = 1
        v.regs.write16(0x03E, 0x0001); // MPOFR.RAMP = 1 → bitmap base 0x20000
        setup_rot_identity(&mut v, 0);
        v.cram.write16(5 * 2, 0x001F); // index 5 = red
        v.vram.write8(0x20000 + 30 * 512 + 40, 5); // plane (40,30)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 40, 30), [0xFF, 0, 0, 0xFF], "identity → 1:1");
        assert_eq!(
            pixel(&buf, 41, 30),
            [0, 0, 0, 0xFF],
            "neighbour transparent"
        );
    }

    #[test]
    fn rbg0_rotation_remaps_the_plane() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000);
        v.regs.write16(0x020, 0x0010);
        v.regs.write16(0x02A, 0x1200);
        v.regs.write16(0x0FC, 0x0001);
        v.regs.write16(0x03E, 0x0001); // base 0x20000
        // 90° table at param A: screen (x, y) → plane (-y, x).
        for (k, val) in [
            (4u32, 1 << 16),
            (5, 1 << 16),
            (8, 0xFFFF_0000),
            (10, 1 << 16),
            (19, 1 << 16),
            (20, 1 << 16),
        ] {
            v.vram.write32(k * 4, val);
        }
        v.cram.write16(9 * 2, 0x7C00); // index 9 = blue
        // Screen (10, 0) → plane (0, 10); plant there.
        v.vram.write8(0x20000 + 10 * 512, 9);
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 10, 0), [0, 0, 0xFF, 0xFF], "rotated sample");
    }

    #[test]
    fn rbg0_competes_with_nbg_by_priority() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000);
        v.regs.write16(0x020, 0x0011); // NBG0 + RBG0
        v.regs.write16(0x028, 0x0012); // NBG0 8bpp bitmap
        v.regs.write16(0x02A, 0x1200); // RBG0 8bpp bitmap
        v.regs.write16(0x0F8, 0x0002); // NBG0 priority 2
        v.regs.write16(0x0FC, 0x0005); // RBG0 priority 5
        v.regs.write16(0x03C, 0x0002); // MPOFN.N0MP = 2 → NBG0 base 0x40000
        v.regs.write16(0x03E, 0x0001); // MPOFR.RAMP = 1 → RBG0 base 0x20000
        setup_rot_identity(&mut v, 0); // param table at VRAM 0 (clear of both)
        v.cram.write16(2, 0x001F); // NBG0 red
        v.cram.write16(5 * 2, 0x7C00); // RBG0 blue
        v.vram.write8(0x40000, 1); // NBG0 dot at (0,0)
        v.vram.write8(0x20000, 5); // RBG0 dot at (0,0)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(
            pixel(&buf, 0, 0),
            [0, 0, 0xFF, 0xFF],
            "RBG0 (pri 5) over NBG0"
        );
    }

    #[test]
    fn rbg1_uses_parameter_b_and_its_own_base() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000);
        v.regs.write16(0x020, 0x0020); // BGON.R1ON (bit5)
        v.regs.write16(0x02A, 0x1200); // shared RBG colour/bitmap config
        v.regs.write16(0x0F8, 0x0003); // RBG1 shares N0PRIN = 3
        v.regs.write16(0x03E, 0x0010); // MPOFR.RBMP = 1 → RBG1 base 0x20000
        setup_rot_identity(&mut v, 1); // parameter B
        v.cram.write16(6 * 2, 0x1F << 5); // index 6 = green
        v.vram.write8(0x20000 + 20 * 512 + 15, 6); // plane (15,20)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(
            pixel(&buf, 15, 20),
            [0, 0xFF, 0, 0xFF],
            "RBG1 via parameter B"
        );
    }

    #[test]
    #[should_panic(expected = "framebuffer must be 320×224×4")]
    fn wrong_buffer_size_panics_loudly() {
        let v = Vdp2::new();
        let mut tiny = [0u8; 64];
        render_frame(&v, None, &mut tiny);
    }
}
