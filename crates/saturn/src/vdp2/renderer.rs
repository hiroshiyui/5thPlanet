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
//! - **Tile maps** decode the full pattern-name set: 1-word (CNSM 12-bit
//!   char vs 10-bit char + H/V flip, with SPCN/SPLT supplement) and 2-word
//!   (15-bit char + 7-bit palette + flip) entries, 8×8 and 16×16 characters,
//!   and plane sizes 1×1 / 2×1 / 2×2 pages composed across planes A–D.
//! - **Colour formats**: 4bpp / 8bpp paletted for both tile and bitmap, plus
//!   16bpp RGB direct-colour for bitmap. Palette index 0 (paletted) / value 0
//!   (RGB) is transparent. 8bpp tiles select a CRAM colour bank from the
//!   pattern-name palette field. Palette lookups honour the live CRAM mode —
//!   RGB555 (modes 0/1) or true RGB888 (modes 2/3).
//! - **Backdrop** = CRAM index 0 (the real BKTAU/BKTAL backdrop register is
//!   a later refinement; palette entry 0 is what splash software programs).
//! - **Scrolling**: integer whole-layer NBG scroll, plus per-line scroll for
//!   NBG0/NBG1 (SCRCTL/LSTAn — integer H/V, LSS interval); fractional scroll
//!   and line zoom are ignored.
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
//! **Colour calculation** (CCCTL): the top two opaque dots by priority are
//! kept, and when the front layer enables colour calc it blends with the dot
//! below — ratio mode (alpha = `(31-CCRT)/31`) or additive (CCMD). NBG0–3 use
//! CCRNA/CCRNB, RBG0 uses CCRR, and sprites use CCRSA..D selected per type,
//! gated by SPCCEN + the SPCCCS/SPCCN priority condition.
//!
//! **Windows** (W0/W1): each layer's WCTL byte enables W0/W1 with an
//! inside/outside area bit and AND/OR logic; a windowed-out dot is suppressed.
//! Rectangles come from WPSX/WPSY/WPEX/WPEY (X at half-dot resolution).
//!
//! **Sprite shadow**: an MSB-only sprite word (bit 15 set, no colour) on a
//! shadow-capable sprite type halves the colour of the layer beneath.
//!
//! Deferred to later increments: the line-coefficient table (per-line scaling)
//! and dual-parameter window selection, line windows, the sprite window plane,
//! and vertical-cell-scroll / line-zoom.
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

    let backdrop = cram(vdp2, BACKDROP_PALETTE_INDEX);

    for y in 0..FRAME_HEIGHT {
        for x in 0..FRAME_WIDTH {
            let (sx, sy) = (x as u32, y as u32);
            // Evaluate layers in VDP2's default front-to-back order, keeping
            // the top two by priority (front order wins ties) so colour
            // calculation can blend the front layer with the one below it:
            // sprite > RBG0 > NBG0 > RBG1 > NBG1..3.
            let mut top: Option<Dot> = None;
            let mut second: Option<Dot> = None;
            let mut shadow = false;

            // The sprite layer may produce a colour dot or an MSB shadow.
            if window_allows(vdp2, vdp2.regs.sprite_window_control(), sx, sy) {
                match sprite_fb.and_then(|fb| sample_sprite(vdp2, fb, sx, sy)) {
                    Some(SpriteDot::Colour(d)) => insert_dot(&mut top, &mut second, Some(d)),
                    Some(SpriteDot::Shadow) => shadow = true,
                    None => {}
                }
            }
            insert_dot(&mut top, &mut second, rbg_layer(vdp2, 0, sx, sy));
            insert_dot(&mut top, &mut second, nbg_layer(vdp2, 0, sx, sy));
            insert_dot(&mut top, &mut second, rbg_layer(vdp2, 1, sx, sy));
            insert_dot(&mut top, &mut second, nbg_layer(vdp2, 1, sx, sy));
            insert_dot(&mut top, &mut second, nbg_layer(vdp2, 2, sx, sy));
            insert_dot(&mut top, &mut second, nbg_layer(vdp2, 3, sx, sy));

            let mut rgb = match top {
                Some(t) => match t.cc {
                    Some((ratio, add)) => {
                        let below = second.map(|s| s.rgb).unwrap_or(backdrop);
                        blend(t.rgb, below, ratio, add)
                    }
                    None => t.rgb,
                },
                None => backdrop,
            };
            // MSB-shadow sprites darken whatever shows beneath them by half.
            if shadow {
                rgb = (rgb.0 >> 1, rgb.1 >> 1, rgb.2 >> 1);
            }
            put_pixel(out, x, y, rgb.0, rgb.1, rgb.2);
        }
    }
}

/// One layer's contribution at a pixel: priority, colour, and the colour-calc
/// descriptor `(ratio 0..31, additive?)` when this layer blends with the dot
/// below it.
#[derive(Clone, Copy)]
struct Dot {
    pri: u8,
    rgb: (u8, u8, u8),
    cc: Option<(u8, bool)>,
}

/// Look up CRAM palette `index` honouring the live CRAM mode (RGB555 for
/// modes 0/1, RGB888 for modes 2/3).
#[inline]
fn cram(vdp2: &Vdp2, index: usize) -> (u8, u8, u8) {
    vdp2.cram.color_rgb888(index, vdp2.regs.cram_mode())
}

/// The sprite layer's contribution: a normal colour dot, or an MSB shadow that
/// darkens the layer beneath instead of drawing.
enum SpriteDot {
    Colour(Dot),
    Shadow,
}

/// Slot `cand` into the running top-two by priority. Front-order callers win
/// ties (strict `>` keeps the earlier dot), and a displaced top becomes second.
fn insert_dot(top: &mut Option<Dot>, second: &mut Option<Dot>, cand: Option<Dot>) {
    let Some(d) = cand else { return };
    if d.pri == 0 {
        return;
    }
    match *top {
        Some(t) if d.pri > t.pri => {
            *second = *top;
            *top = Some(d);
        }
        Some(_) => {
            if second.is_none_or(|s| d.pri > s.pri) {
                *second = Some(d);
            }
        }
        None => *top = Some(d),
    }
}

/// Blend front colour `t` over `b`. Ratio mode (CCMD=0) weights the front by
/// `(31-ratio)/31`; additive mode (CCMD=1) saturating-adds the two.
fn blend(t: (u8, u8, u8), b: (u8, u8, u8), ratio: u8, add: bool) -> (u8, u8, u8) {
    if add {
        (
            t.0.saturating_add(b.0),
            t.1.saturating_add(b.1),
            t.2.saturating_add(b.2),
        )
    } else {
        let alpha = (0x1F - ratio as u32) * 255 / 0x1F;
        let mix = |t: u8, b: u8| ((t as u32 * alpha + b as u32 * (255 - alpha)) / 255) as u8;
        (mix(t.0, b.0), mix(t.1, b.1), mix(t.2, b.2))
    }
}

/// Whether `ctl` (a per-layer WCTL byte) permits drawing at `(x, y)`. Combines
/// W0 and W1 per the layer's logic bit; the sprite window is a later
/// refinement (its enable bit, if set, is treated as "always pass").
fn window_allows(vdp2: &Vdp2, ctl: u8, x: u32, y: u32) -> bool {
    let (w0e, w0a) = (ctl & 0x02 != 0, ctl & 0x01 != 0);
    let (w1e, w1a) = (ctl & 0x08 != 0, ctl & 0x04 != 0);
    if !w0e && !w1e {
        return true;
    }
    let w0 = win_pixel(vdp2, 0, x, y, w0e, w0a);
    let w1 = win_pixel(vdp2, 1, x, y, w1e, w1a);
    // LOG bit (0x80): set = OR the two windows, clear = AND them.
    if ctl & 0x80 != 0 { w0 || w1 } else { w0 && w1 }
}

/// One window's pass/fail at `(x, y)`: disabled → always pass; `area` set →
/// pass inside the rectangle, clear → pass outside.
fn win_pixel(vdp2: &Vdp2, w: usize, x: u32, y: u32, enable: bool, area: bool) -> bool {
    if !enable {
        return true;
    }
    let (sx, ex, sy, ey) = vdp2.regs.window_rect(w);
    let inside = x >= sx && x <= ex && y >= sy && y <= ey;
    if area { inside } else { !inside }
}

/// An enabled, in-window NBG layer's dot at `(x, y)`, or `None`.
fn nbg_layer(vdp2: &Vdp2, n: usize, x: u32, y: u32) -> Option<Dot> {
    if !vdp2.regs.nbg_enabled(n) {
        return None;
    }
    let pri = vdp2.regs.nbg_priority(n);
    if pri == 0 || !window_allows(vdp2, vdp2.regs.nbg_window_control(n), x, y) {
        return None;
    }
    let rgb = sample_nbg(vdp2, n, x, y)?;
    Some(Dot {
        pri,
        rgb,
        cc: vdp2.regs.nbg_color_calc(n),
    })
}

/// An enabled, in-window rotation layer's dot at `(x, y)`, or `None`.
fn rbg_layer(vdp2: &Vdp2, which: usize, x: u32, y: u32) -> Option<Dot> {
    if !vdp2.regs.rbg_enabled(which) {
        return None;
    }
    let pri = vdp2.regs.rbg_priority(which);
    // RBG0 has its own window control byte; RBG1 (sharing NBG0's slot) is
    // ungated for now.
    let gated = which != 0 || window_allows(vdp2, vdp2.regs.rbg0_window_control(), x, y);
    if pri == 0 || !gated {
        return None;
    }
    let rgb = sample_rbg(vdp2, which, x, y)?;
    Some(Dot {
        pri,
        rgb,
        cc: vdp2.regs.rbg_color_calc(which),
    })
}

#[inline]
fn put_pixel(out: &mut [u8], x: usize, y: usize, r: u8, g: u8, b: u8) {
    let dst = (y * FRAME_WIDTH + x) * 4;
    out[dst] = r;
    out[dst + 1] = g;
    out[dst + 2] = b;
    out[dst + 3] = 0xFF;
}

/// Per-line scroll offset `(dx, dy)` for NBG0/NBG1 at screen line `y`, read
/// from the line-scroll table (SCRCTL/LSTAn). Only the integer part of each
/// enabled component (bits 26..16) is applied; line zoom occupies its table
/// slot but isn't yet applied.
fn line_scroll(vdp2: &Vdp2, n: usize, y: u32) -> (u32, u32) {
    let r = &vdp2.regs;
    let (lscx, lscy, lzmx) = (
        r.nbg_line_scroll_x(n),
        r.nbg_line_scroll_y(n),
        r.nbg_line_zoom_x(n),
    );
    if !lscx && !lscy {
        return (0, 0);
    }
    let stride = (lscx as u32 + lscy as u32 + lzmx as u32) * 4;
    let entry = r.nbg_line_scroll_table(n) + (y / r.nbg_line_scroll_interval(n)) * stride;
    let mut off = entry;
    let int = |w: u32| (w >> 16) & 0x07FF;
    let dx = if lscx {
        let v = int(vdp2.vram.read32(off));
        off += 4;
        v
    } else {
        0
    };
    let dy = if lscy { int(vdp2.vram.read32(off)) } else { 0 };
    (dx, dy)
}

/// Sample NBG`n` at screen `(x, y)`, returning `None` for a transparent dot.
fn sample_nbg(vdp2: &Vdp2, n: usize, x: u32, y: u32) -> Option<(u8, u8, u8)> {
    let (mut scroll_x, mut scroll_y) = vdp2.regs.nbg_scroll(n);
    // NBG0/NBG1 support per-line scroll on top of the whole-layer scroll.
    if n < 2 {
        let (dx, dy) = line_scroll(vdp2, n, y);
        scroll_x += dx;
        scroll_y += dy;
    }
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
            (idx != 0).then(|| cram(vdp2, idx))
        }
        // 4bpp paletted (16 colour). The BMPNA palette bank is a later
        // refinement; the nibble indexes the low palette directly.
        _ => {
            let byte = vdp2.vram.read8(base + (py * w + px) / 2);
            let nibble = if px & 1 == 0 { byte >> 4 } else { byte & 0xF } as usize;
            (nibble != 0).then(|| cram(vdp2, nibble))
        }
    }
}

/// Decoded pattern-name entry: which 8×8 cell, palette base, and flip.
struct Pattern {
    /// 8×8 cell number of the character's top-left cell.
    cell: u32,
    /// 4bpp palette number (×16 = CRAM offset) / 8bpp colour-bank (×256).
    palette: u32,
    hflip: bool,
    vflip: bool,
}

fn sample_tile(vdp2: &Vdp2, n: usize, depth: u8, sx: u32, sy: u32) -> Option<(u8, u8, u8)> {
    let r = &vdp2.regs;
    let two_cells = r.nbg_char_size_2x2(n); // 16×16 vs 8×8 character
    let one_word = r.nbg_pn_one_word(n);
    let plane_size = (r.nbg_plane_size(n) & 3) as u32;
    let cell_px = if two_cells { 16 } else { 8 };
    let pg_tiles = if two_cells { 32 } else { 64 }; // PN entries per page edge
    let entry_bytes = if one_word { 2 } else { 4 };
    let pg_bytes = pg_tiles * pg_tiles * entry_bytes;
    let pages_x = if plane_size & 1 != 0 { 2 } else { 1 };
    let pages_y = if plane_size & 2 != 0 { 2 } else { 1 };

    // The screen is tiled by a 2×2 arrangement of planes (A,B / C,D); wrap the
    // scrolled coordinate into that whole-map extent (each page is 512 px).
    let page_px = pg_tiles * cell_px;
    let mx = sx % (2 * pages_x * page_px);
    let my = sy % (2 * pages_y * page_px);
    let (tx, ty) = (mx / cell_px, my / cell_px); // PN-entry coordinates
    let mut in_x = mx % cell_px;
    let mut in_y = my % cell_px;

    // Select plane (A/B/C/D), the page within it, and the entry within the page.
    let psh = if two_cells { 5 } else { 6 }; // log2(pg_tiles)
    let mut page = 0u32;
    let mut plane;
    if plane_size & 1 != 0 {
        page = (tx >> psh) & 1;
        plane = (tx >> (psh + 1)) & 1;
    } else {
        plane = (tx >> psh) & 1;
    }
    if plane_size & 2 != 0 {
        page |= (ty >> (psh - 1)) & 2;
        plane |= (ty >> psh) & 2;
    } else {
        plane |= (ty >> (psh - 1)) & 2;
    }
    let (xoff, yoff) = (tx & (pg_tiles - 1), ty & (pg_tiles - 1));

    // Plane base: align the plane number to the plane size, then scale.
    let plane_num = r.nbg_plane_page(n, plane as usize);
    let shift = [0u32, 1, 2, 2][plane_size as usize];
    let upper_shift = (!one_word as u32) | ((!two_cells as u32) << 1);
    let upper_mask = 0x1FF >> upper_shift;
    let plsize_bytes = pg_bytes * pages_x * pages_y;
    let base = (((plane_num & upper_mask) >> shift) * plsize_bytes) & 0x7_FFFF;
    let pn_addr = base + page * pg_bytes + (yoff * pg_tiles + xoff) * entry_bytes;

    // Decode the pattern name.
    let pat = if one_word {
        let data = vdp2.vram.read16(pn_addr) as u32;
        let spcn = r.nbg_pn_spcn(n);
        let (cell, hflip, vflip) = if r.nbg_pn_cnsm(n) {
            // 12-bit char number, no flip.
            let c = if !two_cells {
                (data & 0xFFF) + ((spcn & 0x1C) << 10)
            } else {
                ((data & 0xFFF) << 2) + (spcn & 3) + ((spcn & 0x10) << 10)
            };
            (c, false, false)
        } else {
            // 10-bit char number + 2 flip bits (11 = V, 10 = H).
            let c = if !two_cells {
                (data & 0x3FF) + (spcn << 10)
            } else {
                ((data & 0x3FF) << 2) + (spcn & 3) + ((spcn & 0x1C) << 10)
            };
            (c, data & 0x400 != 0, data & 0x800 != 0)
        };
        let palette = if depth != 0 {
            (data >> 12) & 0x7 // 8bpp colour bank
        } else {
            ((data >> 12) & 0xF) + (r.nbg_pn_splt(n) << 4)
        };
        Pattern {
            cell,
            palette,
            hflip,
            vflip,
        }
    } else {
        let data = vdp2.vram.read32(pn_addr);
        Pattern {
            cell: data & 0x7FFF,
            palette: (data >> 16) & 0x7F,
            hflip: data & 0x4000_0000 != 0,
            vflip: data & 0x8000_0000 != 0,
        }
    };

    if pat.hflip {
        in_x = (cell_px - 1) - in_x;
    }
    if pat.vflip {
        in_y = (cell_px - 1) - in_y;
    }
    // For 16×16 characters the four 8×8 cells are consecutive (TL,TR,BL,BR).
    let cell = pat.cell + (in_y / 8) * 2 + (in_x / 8);
    let (px, py) = (in_x % 8, in_y % 8);

    if depth == 1 {
        // 8bpp cell: 64 bytes, one byte/pixel; palette is the colour bank.
        let byte = vdp2.vram.read8(cell * 64 + py * 8 + px) as usize;
        (byte != 0).then(|| cram(vdp2, (pat.palette as usize) << 8 | byte))
    } else {
        // 4bpp cell: 32 bytes, two pixels/byte (high nibble = even column).
        let b = vdp2.vram.read8(cell * 32 + py * 4 + px / 2);
        let nibble = if px & 1 == 0 { b >> 4 } else { b & 0xF } as usize;
        (nibble != 0).then(|| cram(vdp2, (pat.palette as usize) << 4 | nibble))
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
// Which framebuffer bits select the sprite colour-calc ratio register (CCRSx).
const SPRITE_CCR_SHIFT: [u16; 16] = [11, 11, 11, 11, 10, 11, 10, 9, 0, 6, 0, 6, 0, 6, 0, 6];
const SPRITE_CCR_MASK: [u16; 16] = [7, 3, 7, 3, 7, 1, 3, 7, 0, 1, 0, 3, 0, 1, 0, 3];
// Sprite types 2..7 use framebuffer bit 15 as a shadow flag (0x8000); others
// have no MSB shadow.
const SPRITE_SHADOW: [u16; 16] = [
    0, 0, 0x8000, 0x8000, 0x8000, 0x8000, 0x8000, 0x8000, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// Whether sprite colour-calculation applies to a dot of priority `pri`, and
/// with what ratio: SPCCEN gates it, then SPCCCS compares `pri` to SPCCN
/// (≤ / == / ≥ / always). The ratio comes from CCRSx selected by `ccidx`.
fn sprite_cc(vdp2: &Vdp2, pri: u8, ccidx: usize) -> Option<(u8, bool)> {
    if !vdp2.regs.sprite_color_calc_enabled() {
        return None;
    }
    let n = vdp2.regs.sprite_cc_condition();
    let on = match vdp2.regs.sprite_cc_mode() {
        0 => pri <= n,
        1 => pri == n,
        2 => pri >= n,
        _ => true,
    };
    on.then(|| {
        (
            vdp2.regs.sprite_color_calc_ratio(ccidx),
            vdp2.regs.color_calc_add_mode(),
        )
    })
}

/// Sample the VDP1 sprite layer at screen `(x, y)`: read the frame-buffer
/// word, decode colour + priority + colour-calc per the SPCTL sprite type.
/// Returns `None` for a transparent / priority-0 dot, or a [`SpriteDot`]
/// (colour or MSB shadow).
fn sample_sprite(vdp2: &Vdp2, fb: &Framebuffer, x: u32, y: u32) -> Option<SpriteDot> {
    let pix = fb.pixel(x as i32, y as i32);
    if pix == 0 {
        return None; // nothing plotted here
    }
    let stype = vdp2.regs.sprite_type();

    // MSB shadow: for shadow-capable types a word with only bit 15 set is a
    // pure shadow that darkens the layer below rather than drawing a colour.
    if SPRITE_SHADOW[stype] != 0 && pix == 0x8000 {
        return Some(SpriteDot::Shadow);
    }

    // RGB direct colour: MSB set and SPCLMD enabled. Priority comes from
    // sprite register 0.
    if pix & 0x8000 != 0 && vdp2.regs.sprite_rgb_mode() {
        let pri = vdp2.regs.sprite_priority(0);
        return (pri != 0).then(|| {
            SpriteDot::Colour(Dot {
                pri,
                rgb: cram::rgb555_to_888(pix),
                cc: sprite_cc(vdp2, pri, 0),
            })
        });
    }

    // Palette code: priority bits index PRISA..PRISD; the masked low bits are
    // a CRAM colour code (0 = transparent).
    let pidx = ((pix >> SPRITE_PRIO_SHIFT[stype]) & SPRITE_PRIO_MASK[stype]) as usize;
    let pri = vdp2.regs.sprite_priority(pidx);
    if pri == 0 {
        return None;
    }
    let code = (pix & SPRITE_COLORMASK[stype]) as usize;
    if code == 0 {
        return None;
    }
    let ccidx = ((pix >> SPRITE_CCR_SHIFT[stype]) & SPRITE_CCR_MASK[stype]) as usize;
    Some(SpriteDot::Colour(Dot {
        pri,
        rgb: cram(vdp2, code),
        cc: sprite_cc(vdp2, pri, ccidx),
    }))
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
            (idx != 0).then(|| cram(vdp2, idx))
        }
        _ => {
            let byte = vdp2.vram.read8(base + (py * w + px) / 2);
            let nibble = if px & 1 == 0 { byte >> 4 } else { byte & 0xF } as usize;
            (nibble != 0).then(|| cram(vdp2, nibble))
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
        (byte != 0).then(|| cram(vdp2, byte))
    } else {
        let byte = vdp2.vram.read8(char_num * 32 + in_y * 4 + in_x / 2);
        let nibble = if in_x & 1 == 0 { byte >> 4 } else { byte & 0xF } as usize;
        (nibble != 0).then(|| cram(vdp2, (palette_bank << 4) | nibble))
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
        v.regs.write16(0x030, 0x8000); // PNCN0: 1-word pattern name
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
        v.regs.write16(0x030, 0x8000); // PNCN0: 1-word pattern name
        v.vram.write16(0, 3); // char 3 at tile (0,0)
        v.vram.write8(3 * 64, 0x42); // pixel (0,0) = index 0x42
        v.cram.write16(0x42 * 2, 0x001F); // red
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn tile_two_word_pattern_carries_char_and_palette() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0000); // tile mode, 4bpp; PNCN0 = 0 → 2-word
        // 2-word PN: char (bits 14..0) = 5, palette (bits 22..16) = 2.
        v.vram.write32(0, (2 << 16) | 5);
        // Char 5 pixel (3,4): byte cell·32 + 4·4 + 1, low nibble = 7.
        v.vram.write8(5 * 32 + 4 * 4 + 1, 0x07);
        v.cram.write16(0x27 * 2, 0x001F); // (2<<4)|7 → red
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 3, 4), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn tile_horizontal_flip_mirrors_the_cell() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0000); // tile mode, 4bpp
        v.regs.write16(0x030, 0x8000); // 1-word, CNSM off → flip bits live
        v.vram.write16(0, 0x0400 | 5); // char 5, H-flip (bit 10)
        // Source pixel at col 0, row 4 → high nibble of byte cell·32 + 4·4.
        v.vram.write8(5 * 32 + 4 * 4, 0x70);
        v.cram.write16(7 * 2, 0x001F); // palette 0, index 7 → red
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // H-flip puts column 0 at screen x = 7.
        assert_eq!(pixel(&buf, 7, 4), [0xFF, 0, 0, 0xFF]);
        assert_ne!(pixel(&buf, 0, 4), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn tile_vertical_flip_mirrors_the_cell() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0000);
        v.regs.write16(0x030, 0x8000); // 1-word, CNSM off
        v.vram.write16(0, 0x0800 | 5); // char 5, V-flip (bit 11)
        // Source pixel at col 3, row 0 → low nibble of byte cell·32 + 1.
        v.vram.write8(5 * 32 + 1, 0x07);
        v.cram.write16(7 * 2, 0x001F);
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 3, 7), [0xFF, 0, 0, 0xFF]);
        assert_ne!(pixel(&buf, 3, 0), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn tile_16x16_character_spans_four_cells() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0001); // N0CHSZ = 1 → 16×16, 4bpp
        v.regs.write16(0x030, 0x8000); // 1-word, CNSM off
        // 16×16 char numbers address in 4-cell units: char 8 → 8·4 = 32 (TL),
        // with 33=TR, 34=BL, 35=BR.
        v.vram.write16(0, 8);
        // Screen (9, 10): cell = 32 + (10/8)*2 + (9/8) = 35 (BR); px=1, py=2.
        // 4bpp byte at cell·32 + py·4 + px/2; px=1 odd → low nibble.
        v.vram.write8(35 * 32 + 2 * 4, 0x07);
        v.cram.write16(7 * 2, 0x001F);
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 9, 10), [0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn tile_multi_page_plane_addresses_second_page() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0000); // tile mode, 4bpp
        v.regs.write16(0x030, 0x8000); // 1-word
        v.regs.write16(0x03A, 0x0001); // PLSZ N0 = 1 → 2×1 pages
        // One page is 64×64 entries × 2 bytes = 0x2000. The second page (to the
        // right) starts at x = 512 px. Screen x = 512 selects page 1, tile 0.
        v.regs.write16(0x070, 0x0200); // SCXIN0 = 512 → sample page 1, tile 0
        v.vram.write16(0x2000, 5); // page-1 tile 0 → char 5
        v.vram.write8(5 * 32, 0x70); // pixel (0,0) high nibble = 7
        v.cram.write16(7 * 2, 0x001F);
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

    /// Two opaque bitmap layers with colour calc on the front one blend by the
    /// CCRNA ratio (ratio mode): front=red over below=blue at ratio 15.
    #[test]
    fn colour_calc_ratio_blends_front_over_below() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0003); // NBG0 + NBG1
        v.regs.write16(0x028, 0x1212); // both bitmap, 8bpp
        v.regs.write16(0x0F8, 0x0205); // N0PRIN=5 (top), N1PRIN=2
        v.regs.write16(0x03C, 0x0010); // NBG1 bitmap base 0x20000
        v.regs.write16(0x0EC, 0x0001); // CCCTL.N0CCEN, CCMD=0 (ratio)
        v.regs.write16(0x108, 0x000F); // CCRNA.N0CCRT = 15
        v.cram.write16(2, 0x001F); // index 1 red  (NBG0, front)
        v.cram.write16(4, 0x7C00); // index 2 blue (NBG1, below)
        v.vram.write8(0, 1);
        v.vram.write8(0x2_0000, 2);
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // alpha = (31-15)*255/31 = 131; red·131 over blue·124.
        assert_eq!(pixel(&buf, 0, 0), [131, 0, 124, 0xFF]);
    }

    /// With CCMD=1 the front and below colours add (saturating).
    #[test]
    fn colour_calc_additive_sums_front_and_below() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000);
        v.regs.write16(0x020, 0x0003);
        v.regs.write16(0x028, 0x1212);
        v.regs.write16(0x0F8, 0x0205); // NBG0 on top
        v.regs.write16(0x03C, 0x0010);
        v.regs.write16(0x0EC, 0x0101); // N0CCEN + CCMD=1 (additive)
        v.cram.write16(2, 0x001F); // red
        v.cram.write16(4, 0x7C00); // blue
        v.vram.write8(0, 1);
        v.vram.write8(0x2_0000, 2);
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0xFF, 0xFF], "red + blue");
    }

    /// Window 0 with area=inside clips NBG0 to the rectangle; outside it the
    /// layer is suppressed and the backdrop shows.
    #[test]
    fn window_zero_clips_layer_to_rectangle() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // bitmap, 8bpp
        v.cram.write16(0, 0x1F << 5); // backdrop green
        v.cram.write16(2, 0x001F); // index 1 red
        v.vram.write8(0, 1); // (0,0)
        v.vram.write8(2 * 512 + 3, 1); // (3,2)
        // NBG0 window control: W0 enable + area=inside, AND logic (W1 off).
        v.regs.write16(0x0D0, 0x0003);
        // W0 rect x∈[2,5], y∈[1,3] (X stored at half-dot resolution).
        v.regs.write16(0x0C0, 4); // WPSX0 → sx 2
        v.regs.write16(0x0C2, 1); // WPSY0 → sy 1
        v.regs.write16(0x0C4, 0x0A); // WPEX0 → ex 5
        v.regs.write16(0x0C6, 3); // WPEY0 → ey 3
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 3, 2), [0xFF, 0, 0, 0xFF], "inside window → red");
        assert_eq!(
            pixel(&buf, 0, 0),
            [0, 0xFF, 0, 0xFF],
            "outside window → backdrop"
        );
    }

    /// An MSB-shadow sprite dot (type 2, word = 0x8000) halves the colour of
    /// the NBG layer beneath instead of drawing its own colour.
    #[test]
    fn sprite_msb_shadow_halves_layer_below() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // NBG0 bitmap, 8bpp
        v.regs.write16(0x0E0, 0x0002); // SPCTL.SPTYPE = 2 (shadow-capable)
        v.cram.write16(2, 0x7FFF); // index 1 = white (0xFF,0xFF,0xFF)
        v.vram.write8(0, 1); // NBG0 white at (0,0)
        v.vram.write8(512, 1); // NBG0 white at (0,1)
        let fb = sprite_fb_with(0, 0, 0x8000); // pure shadow at (0,0)
        let mut buf = fresh_buf();
        render_frame(&v, Some(&fb), &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0x7F, 0x7F, 0x7F, 0xFF], "shadowed");
        assert_eq!(pixel(&buf, 0, 1), [0xFF, 0xFF, 0xFF, 0xFF], "unshadowed");
    }

    #[test]
    fn cram_mode2_yields_true_rgb888() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // NBG0 bitmap, 8bpp
        v.regs.write16(0x00E, 0x2000); // RAMCTL.CRMD = 2 (RGB888)
        // Mode-2 entry for index 1 is the 32-bit word at byte 4: 0x00BBGGRR.
        v.cram.write32(4, 0x0056_3412);
        v.vram.write8(0, 1); // bitmap dot → palette index 1
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0x12, 0x34, 0x56, 0xFF]);
    }

    #[test]
    fn nbg0_line_scroll_x_shifts_a_single_line() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // NBG0 bitmap, 8bpp
        v.regs.write16(0x09A, 0x0002); // SCRCTL.N0LSCX, LSS=0 (every line)
        v.regs.write16(0x0A2, 0x0200); // LSTA0L → table at byte 0x400
        // Line 0 entry = 0 (no scroll); line 2 entry = integer 4 (bits 26..16).
        v.vram.write32(0x400 + 2 * 4, 4 << 16);
        v.cram.write16(2, 0x001F); // index 1 = red
        v.vram.write8(2 * 512 + 4, 1); // bitmap dot at (4, 2)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // Line 2 scrolls +4, so screen (0,2) samples bitmap (4,2) → red.
        assert_eq!(pixel(&buf, 0, 2), [0xFF, 0, 0, 0xFF], "line 2 scrolled");
        // Line 0 has no scroll, so screen (0,0) samples the empty (0,0).
        assert_eq!(pixel(&buf, 0, 0), [0, 0, 0, 0xFF], "line 0 unscrolled");
    }
}
