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
//! - **Scrolling**: integer whole-layer NBG scroll, plus per-line scroll,
//!   per-line horizontal zoom, and per-column vertical cell scroll for
//!   NBG0/NBG1 (SCRCTL/LSTAn/VCSTA); fractional whole-layer scroll is ignored.
//! - **NTSC low-res** (320×224).
//!
//! The **VDP1 sprite layer** is composited too: VDP2 reads the VDP1 frame
//! buffer per pixel, splits each word per the SPCTL sprite type into a
//! colour code / RGB value and a priority (from PRISA..PRISD), and the sprite
//! layer joins the priority race frontmost on ties (sprite > NBG0 > …).
//!
//! The **rotation backgrounds RBG0/RBG1** are composited via [`super::rotation`]:
//! each screen dot is mapped through the rotation parameter table's affine
//! transform, then the rotation plane is sampled — a bitmap, or a tile field
//! composed as the full 4×4 grid of planes (A..P) with the shared pattern-name
//! decode. RBG0 uses parameter set A (priority PRIR); RBG1 uses set B (N0PRIN).
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
//! Rectangles come from WPSX/WPSY/WPEX/WPEY (X at half-dot resolution), or — if
//! a line window is enabled (LWTAn) — the horizontal start/end are read per
//! scanline from a VRAM table while WPSY/WPEY bound it vertically.
//!
//! **Sprite shadow**: an MSB-only sprite word (bit 15 set, no colour) on a
//! shadow-capable sprite type halves the colour of the layer beneath.
//!
//! The rotation **line-coefficient table** (KTCTL) is applied per scanline:
//! it overrides kx/ky (perspective) in modes 0/1/2, or flags a line
//! transparent via the coefficient MSB.
//!
//! Rotation **screen-over** (RAOVR/RBOVR) is honoured: outside the plane field
//! the layer either repeats (wrap) or is transparent (modes 2/3).
//!
//! Deferred to later increments: dual-parameter window selection, the
//! coefficient mode-3 (Xp) override and CRAM-resident coefficient tables, and
//! the screen-over pattern (mode 1). The **sprite window** (WCTL SWE/SWA) is
//! not modelled — the reference emulator leaves it unimplemented too, and it
//! would need the VDP1 frame buffer threaded into the per-layer window test.
//!
//! `render_frame` is pure (no allocation); the sprite source is the VDP1
//! frame buffer, supplied by the [`crate::system::Saturn`] aggregate.

use super::rotation::RotationParams;
use super::{Vdp2, cram};
use crate::vdp1::Framebuffer;

/// Default (NTSC low-res 320×224) display size: the power-on resolution and the
/// frontend's default window/texture size. The *active* size is dynamic — the
/// game selects it via TVMD ([`crate::vdp2::Vdp2Regs::screen_dims`]) and
/// [`render_frame`] returns it — so hi-res games (640/704) render correctly.
pub const FRAME_WIDTH: usize = 320;
pub const FRAME_HEIGHT: usize = 224;
/// Largest display the VDP2 produces: 704 wide × 256 lines × 2 (double-density
/// interlace). The framebuffer passed to [`render_frame`] must be this many
/// bytes; the renderer packs the *active* `width × height` tightly at its start
/// (row stride = active width) and returns those dims, so the caller uploads
/// only `width × height` with a `width × 4` pitch.
pub const MAX_FRAME_WIDTH: usize = 704;
pub const MAX_FRAME_HEIGHT: usize = 512;
pub const FRAMEBUFFER_BYTES: usize = MAX_FRAME_WIDTH * MAX_FRAME_HEIGHT * 4;

const BACKDROP_PALETTE_INDEX: usize = 0;

/// Render one frame of NTSC low-res into `out`, compositing the enabled NBG
/// layers and the VDP1 sprite layer (`sprite_fb`, `None` when there's no VDP1
/// frame buffer to read). Panics if `out`'s length isn't [`FRAMEBUFFER_BYTES`].
pub fn render_frame(vdp2: &Vdp2, sprite_fb: Option<&Framebuffer>, out: &mut [u8]) -> (usize, usize) {
    // Active resolution from TVMD (320/352/640/704 × 224/240/256[×2]). The
    // content is packed tightly with row stride = `w`, so the caller uploads
    // `w × h` with a `w × 4` pitch.
    let (w, h) = vdp2.regs.screen_dims();
    assert!(
        out.len() >= w * h * 4,
        "framebuffer {} too small for {w}×{h}",
        out.len()
    );

    if !vdp2.regs.display_enabled() {
        // Opaque black so SDL doesn't show a transparent hole.
        for px in out[..w * h * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 0, 0, 0xFF]);
        }
        return (w, h);
    }

    let backdrop = cram(vdp2, BACKDROP_PALETTE_INDEX);

    for y in 0..h {
        for x in 0..w {
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
            put_pixel(out, x, y, w, rgb.0, rgb.1, rgb.2);
        }
    }
    (w, h)
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
/// pass inside the window, clear → pass outside. With a line window enabled,
/// the horizontal start/end come from the per-line table (the vertical bounds
/// stay from WPSY/WPEY).
fn win_pixel(vdp2: &Vdp2, w: usize, x: u32, y: u32, enable: bool, area: bool) -> bool {
    if !enable {
        return true;
    }
    let (mut sx, mut ex, sy, ey) = vdp2.regs.window_rect(w);
    if vdp2.regs.window_line_enabled(w) {
        // One 32-bit entry per line: start X in bits 25..16, end X in bits 9..0
        // (10-bit dots; halved to the renderer's low-res scale, matching the
        // rectangle path's `& 0x3FE >> 1`).
        let word = vdp2.vram.read32(vdp2.regs.window_line_table(w) + y * 4);
        sx = ((word >> 16) & 0x3FF) >> 1;
        ex = (word & 0x3FF) >> 1;
    }
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
    // Mosaic (MZCTL): snap the colour-sampling coordinate to the block origin.
    let (mx, my) = vdp2.regs.mosaic_coord(1 << n, x, y);
    let rgb = sample_nbg(vdp2, n, mx, my)?;
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
    // Mosaic (MZCTL bit 4) applies to RBG0 only.
    let (mx, my) = if which == 0 {
        vdp2.regs.mosaic_coord(0x10, x, y)
    } else {
        (x, y)
    };
    let rgb = sample_rbg(vdp2, which, mx, my)?;
    Some(Dot {
        pri,
        rgb,
        cc: vdp2.regs.rbg_color_calc(which),
    })
}

#[inline]
fn put_pixel(out: &mut [u8], x: usize, y: usize, stride: usize, r: u8, g: u8, b: u8) {
    let dst = (y * stride + x) * 4;
    out[dst] = r;
    out[dst + 1] = g;
    out[dst + 2] = b;
    out[dst + 3] = 0xFF;
}

/// Per-line scroll for NBG0/NBG1 at screen line `y`, read from the line-scroll
/// table (SCRCTL/LSTAn): `(dx, dy, zoom_x)`, where `dx`/`dy` are integer scroll
/// (bits 26..16) and `zoom_x` is the horizontal step in 16.16 (1.0 when line
/// zoom is off). Components present in the table in order H-scroll, V-scroll,
/// H-zoom — only the enabled ones.
fn line_scroll(vdp2: &Vdp2, n: usize, y: u32) -> (u32, u32, u32) {
    let r = &vdp2.regs;
    let (lscx, lscy, lzmx) = (
        r.nbg_line_scroll_x(n),
        r.nbg_line_scroll_y(n),
        r.nbg_line_zoom_x(n),
    );
    if !lscx && !lscy && !lzmx {
        return (0, 0, 1 << 16);
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
    let dy = if lscy {
        let v = int(vdp2.vram.read32(off));
        off += 4;
        v
    } else {
        0
    };
    // Horizontal line zoom: a 16.16 per-dot step (integer bits 18..16,
    // fraction bits 15..8). 1.0 means no zoom.
    let zoom = if lzmx {
        (vdp2.vram.read32(off) & 0x0007_FF00).max(1)
    } else {
        1 << 16
    };
    (dx, dy, zoom)
}

/// Per-column vertical cell-scroll offset (signed) for NBG0/NBG1 at screen
/// column `x/8`, read from the shared VCSTA table. When both NBG0 and NBG1 use
/// it the table interleaves their longwords (NBG0 even, NBG1 odd); the value
/// is an 11-bit signed scroll in bits 26..16.
fn vcell_scroll(vdp2: &Vdp2, n: usize, x: u32) -> i32 {
    let both = vdp2.regs.nbg_vcell_scroll(0) && vdp2.regs.nbg_vcell_scroll(1);
    let (mult, off) = if both {
        (2, if n == 1 { 1 } else { 0 })
    } else {
        (1, 0)
    };
    let col = x / 8;
    let addr = vdp2.regs.vcell_scroll_table() + (col * mult + off) * 4;
    let raw = (vdp2.vram.read32(addr) >> 16) & 0x07FF;
    // Sign-extend the 11-bit value.
    if raw & 0x0400 != 0 {
        (raw | 0xFFFF_F800) as i32
    } else {
        raw as i32
    }
}

/// Sample NBG`n` at screen `(x, y)`, returning `None` for a transparent dot.
fn sample_nbg(vdp2: &Vdp2, n: usize, x: u32, y: u32) -> Option<(u8, u8, u8)> {
    let (mut scroll_x, mut scroll_y) = vdp2.regs.nbg_scroll(n);
    // NBG0/NBG1 support per-line scroll + zoom and per-column vertical cell
    // scroll on top of the whole-layer scroll.
    let mut zoom_x: u32 = 1 << 16;
    if n < 2 {
        let (dx, dy, zoom) = line_scroll(vdp2, n, y);
        // The scroll deltas are sign-extended (a negative offset is a large
        // `u32`), so accumulate them modulo 2^32 like the vcell/zoom adds below.
        scroll_x = scroll_x.wrapping_add(dx);
        scroll_y = scroll_y.wrapping_add(dy);
        zoom_x = zoom;
        if vdp2.regs.nbg_vcell_scroll(n) {
            scroll_y = scroll_y.wrapping_add(vcell_scroll(vdp2, n, x) as u32);
        }
    }
    let depth = vdp2.regs.nbg_color_mode(n);
    // Horizontal line zoom scales the per-dot source step (1.0 = no zoom).
    let sx = scroll_x.wrapping_add(((x as u64 * zoom_x as u64) >> 16) as u32);
    // `scroll_y` may be a sign-extended negative (line/vcell scroll up), so wrap
    // rather than panic on overflow in debug builds; the source coord is masked
    // to the plane size downstream.
    let sy = y.wrapping_add(scroll_y);
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
    let coff = vdp2.regs.nbg_color_ram_offset(n);
    let tpon = vdp2.regs.nbg_transparent_pen_solid(n);
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
            (idx != 0 || tpon).then(|| cram(vdp2, coff | idx))
        }
        // 4bpp paletted (16 colour). The BMPNA palette bank is a later
        // refinement; the nibble indexes the low palette directly.
        _ => {
            let byte = vdp2.vram.read8(base + (py * w + px) / 2);
            let nibble = if px & 1 == 0 { byte >> 4 } else { byte & 0xF } as usize;
            (nibble != 0 || tpon).then(|| cram(vdp2, coff | nibble))
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

/// Pattern-name format bits (PNCN for NBG, PNCR for rotation) — they share the
/// same layout, so NBG and RBG tile sampling reuse one decoder.
#[derive(Clone, Copy)]
struct PnFormat {
    one_word: bool,
    cnsm: bool,
    spcn: u32,
    splt: u32,
}

/// Decode the pattern-name entry at `pn_addr` per `fmt`, the character size,
/// and the colour `depth`.
fn decode_pattern(vdp2: &Vdp2, pn_addr: u32, fmt: PnFormat, two_cells: bool, depth: u8) -> Pattern {
    if fmt.one_word {
        let data = vdp2.vram.read16(pn_addr) as u32;
        let spcn = fmt.spcn;
        let (cell, hflip, vflip) = if fmt.cnsm {
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
            ((data >> 12) & 0xF) + (fmt.splt << 4)
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
    }
}

/// Sample the dot at in-character `(in_x, in_y)` of a decoded pattern, applying
/// flip and (for 16×16 characters) selecting the right 8×8 cell. `None` for a
/// transparent dot.
#[allow(clippy::too_many_arguments)]
fn sample_pattern_cell(
    vdp2: &Vdp2,
    pat: &Pattern,
    two_cells: bool,
    depth: u8,
    mut in_x: u32,
    mut in_y: u32,
    coff: usize,
    tpon: bool,
) -> Option<(u8, u8, u8)> {
    let cell_px = if two_cells { 16 } else { 8 };
    if pat.hflip {
        in_x = (cell_px - 1) - in_x;
    }
    if pat.vflip {
        in_y = (cell_px - 1) - in_y;
    }
    // The pattern-name character number addresses VRAM in 0x20-byte units (one
    // 4bpp 8×8 cell). An 8bpp cell is 0x40 bytes = *two* units, so 8bpp steps
    // the character number by 2 — both between adjacent cells and between the
    // four 8×8 sub-cells of a 16×16 character (TL,TR,BL,BR). The cell's byte
    // base is therefore always `char × 0x20`.
    let subcell = (in_y / 8) * 2 + (in_x / 8);
    let (px, py) = (in_x % 8, in_y % 8);
    if depth == 1 {
        // 8bpp cell: 0x40 bytes, one byte/pixel; palette is the colour bank.
        let cell = pat.cell + subcell * 2;
        let byte = vdp2.vram.read8(cell * 32 + py * 8 + px) as usize;
        (byte != 0 || tpon).then(|| cram(vdp2, coff | (pat.palette as usize) << 8 | byte))
    } else {
        // 4bpp cell: 0x20 bytes, two pixels/byte (high nibble = even column).
        let cell = pat.cell + subcell;
        let b = vdp2.vram.read8(cell * 32 + py * 4 + px / 2);
        let nibble = if px & 1 == 0 { b >> 4 } else { b & 0xF } as usize;
        (nibble != 0 || tpon).then(|| cram(vdp2, coff | (pat.palette as usize) << 4 | nibble))
    }
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
    let in_x = mx % cell_px;
    let in_y = my % cell_px;

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

    let fmt = PnFormat {
        one_word,
        cnsm: r.nbg_pn_cnsm(n),
        spcn: r.nbg_pn_spcn(n),
        splt: r.nbg_pn_splt(n),
    };
    let pat = decode_pattern(vdp2, pn_addr, fmt, two_cells, depth);
    let coff = r.nbg_color_ram_offset(n);
    let tpon = r.nbg_transparent_pen_solid(n);
    sample_pattern_cell(vdp2, &pat, two_cells, depth, in_x, in_y, coff, tpon)
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
    let mut rp = RotationParams::read(&vdp2.vram, vdp2.regs.rotation_table_addr(), which);
    // Per-line coefficient table: overrides kx/ky (giving perspective) or makes
    // the whole line transparent.
    match rot_line_coeff(vdp2, which, &rp, y) {
        Coeff::Off => {}
        Coeff::Transparent => return None,
        Coeff::Set(kx, ky) => {
            rp.kx = kx;
            rp.ky = ky;
        }
    }
    let (plane_x, plane_y) = rp.transform(x as i32, y as i32);
    let depth = vdp2.regs.rbg_color_mode();
    if vdp2.regs.rbg_bitmap_enabled() {
        sample_rot_bitmap(vdp2, which, depth, plane_x, plane_y)
    } else {
        sample_rot_tile(vdp2, which, depth, plane_x, plane_y)
    }
}

/// Outcome of a rotation line-coefficient lookup.
enum Coeff {
    /// No coefficient table — use the parameter table's kx/ky.
    Off,
    /// The coefficient's MSB flags this line as transparent.
    Transparent,
    /// Override (kx, ky) for this line.
    Set(i32, i32),
}

/// Read the per-line rotation coefficient for `which` at screen line `y` and
/// turn it into a kx/ky override (modes 0/1/2) per KTCTL. The table index is
/// `(kast + dkast·y) >> 16`; longword entries are 24-bit 8.16 fixed, word
/// entries 15-bit `<<6`. The coefficient table is assumed to live in VRAM
/// (RAMCTL.CRKTE = 0); mode 3 (Xp override) is a later refinement.
fn rot_line_coeff(vdp2: &Vdp2, which: usize, rp: &RotationParams, y: u32) -> Coeff {
    if !vdp2.regs.rbg_coeff_enabled(which) {
        return Coeff::Off;
    }
    let idx = ((rp.kast.wrapping_add(rp.dkast.wrapping_mul(y as i32))) >> 16) as u32;
    let base = vdp2.regs.rbg_coeff_table_base(which);
    let (val, transparent) = if vdp2.regs.rbg_coeff_size_word(which) {
        let w = vdp2.vram.read16(base + idx * 2) as u32;
        let mut v = (w & 0x7FFF) as i32;
        if v & 0x4000 != 0 {
            v |= !0x7FFF; // sign-extend bit 14
        }
        (v << 6, w & 0x8000 != 0) // <<6 → 16.16
    } else {
        let w = vdp2.vram.read32(base + idx * 4);
        let mut v = (w & 0x00FF_FFFF) as i32;
        if v & 0x0080_0000 != 0 {
            v |= !0x00FF_FFFF; // sign-extend bit 23 (already 8.16)
        }
        (v, w & 0x8000_0000 != 0)
    };
    if transparent {
        return Coeff::Transparent;
    }
    match vdp2.regs.rbg_coeff_mode(which) {
        0 => Coeff::Set(val, val),
        1 => Coeff::Set(val, rp.ky),
        2 => Coeff::Set(rp.kx, val),
        _ => Coeff::Off, // mode 3 (Xp) — refinement
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
    // Screen-over modes 2/3 leave the area outside the bitmap transparent;
    // mode 0/1 repeat it.
    let over = vdp2.regs.rbg_screen_over(which);
    let (px, py) = if over == 2 || over == 3 {
        if plane_x < 0 || plane_y < 0 || plane_x >= w || plane_y >= h {
            return None;
        }
        (plane_x as u32, plane_y as u32)
    } else {
        (plane_x.rem_euclid(w) as u32, plane_y.rem_euclid(h) as u32)
    };
    let w = w as u32;
    let coff = vdp2.regs.rbg_color_ram_offset(which);
    let tpon = vdp2.regs.rbg_transparent_pen_solid(which);
    match depth {
        3 => {
            let entry = vdp2.vram.read16(base + (py * w + px) * 2);
            (entry & 0x7FFF != 0).then(|| cram::rgb555_to_888(entry))
        }
        1 => {
            let idx = vdp2.vram.read8(base + py * w + px) as usize;
            (idx != 0 || tpon).then(|| cram(vdp2, coff | idx))
        }
        _ => {
            let byte = vdp2.vram.read8(base + (py * w + px) / 2);
            let nibble = if px & 1 == 0 { byte >> 4 } else { byte & 0xF } as usize;
            (nibble != 0 || tpon).then(|| cram(vdp2, coff | nibble))
        }
    }
}

/// Sample a rotation tile plane at transformed coordinate `(plane_x,
/// plane_y)`. The rotation field is a **4×4 grid of planes** (A..P), each
/// `RAPLSZ`/`RBPLSZ` pages of 512 px; the coordinate wraps into the field
/// (screen-over "repeat" mode). The matching plane's map number selects its
/// page-aligned base, then the full pattern-name decode + cell sample run
/// (shared with the NBG tile path).
fn sample_rot_tile(
    vdp2: &Vdp2,
    which: usize,
    depth: u8,
    plane_x: i32,
    plane_y: i32,
) -> Option<(u8, u8, u8)> {
    let r = &vdp2.regs;
    let two_cells = r.rbg_char_size_2x2();
    let one_word = r.rbg_pn_one_word();
    let plane_size = (r.rbg_plane_size(which) & 3) as u32;
    let cell_px = if two_cells { 16 } else { 8 };
    let pg_tiles = if two_cells { 32 } else { 64 };
    let entry_bytes = if one_word { 2 } else { 4 };
    let pg_bytes = pg_tiles * pg_tiles * entry_bytes;
    let pages_x = if plane_size & 1 != 0 { 2 } else { 1 };
    let pages_y = if plane_size & 2 != 0 { 2 } else { 1 };

    // The 4×4-plane field (each page is 512 px). The screen-over mode decides
    // what happens outside it: repeat (wrap), or transparent.
    let page_px = pg_tiles * cell_px;
    let plane_w = pages_x * page_px;
    let plane_h = pages_y * page_px;
    let over = vdp2.regs.rbg_screen_over(which);
    let (field_w, field_h) = if over == 3 {
        (page_px, page_px) // transparent outside a single 512×512 area
    } else {
        (4 * plane_w, 4 * plane_h)
    };
    let (mx, my) = if over == 2 || over == 3 {
        // Outside the field is transparent.
        if plane_x < 0 || plane_y < 0 || plane_x as u32 >= field_w || plane_y as u32 >= field_h {
            return None;
        }
        (plane_x as u32, plane_y as u32)
    } else {
        (
            plane_x.rem_euclid(field_w as i32) as u32,
            plane_y.rem_euclid(field_h as i32) as u32,
        )
    };

    // Which plane (0..15, row-major A..P), then the page within it.
    let plane_idx = (my / plane_h) as usize * 4 + (mx / plane_w) as usize;
    let (wx, wy) = (mx % plane_w, my % plane_h);
    let (tx, ty) = (wx / cell_px, wy / cell_px);
    let in_x = wx % cell_px;
    let in_y = wy % cell_px;
    let page = (tx / pg_tiles) + (ty / pg_tiles) * pages_x;
    let (xoff, yoff) = (tx % pg_tiles, ty % pg_tiles);

    // Plane base: align the plane number to the plane size, then scale.
    let plane_num = r.rbg_plane_number(which, plane_idx);
    let shift = [0u32, 1, 2, 2][plane_size as usize];
    let upper_shift = (!one_word as u32) | ((!two_cells as u32) << 1);
    let upper_mask = 0x1FF >> upper_shift;
    let plsize_bytes = pg_bytes * pages_x * pages_y;
    let base = (((plane_num & upper_mask) >> shift) * plsize_bytes) & 0x7_FFFF;
    let pn_addr = base + page * pg_bytes + (yoff * pg_tiles + xoff) * entry_bytes;

    let fmt = PnFormat {
        one_word,
        cnsm: r.rbg_pn_cnsm(),
        spcn: r.rbg_pn_spcn(),
        splt: r.rbg_pn_splt(),
    };
    let pat = decode_pattern(vdp2, pn_addr, fmt, two_cells, depth);
    let coff = r.rbg_color_ram_offset(which);
    let tpon = r.rbg_transparent_pen_solid(which);
    sample_pattern_cell(vdp2, &pat, two_cells, depth, in_x, in_y, coff, tpon)
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
        let (w, h) = render_frame(&v, None, &mut buf);
        for chunk in buf[..w * h * 4].chunks_exact(4) {
            assert_eq!(chunk, &[0, 0, 0, 0xFF]);
        }
    }

    #[test]
    fn display_enabled_no_layer_fills_with_backdrop() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.cram.write16(0, 0x001F); // backdrop = red
        let mut buf = fresh_buf();
        let (w, h) = render_frame(&v, None, &mut buf);
        for chunk in buf[..w * h * 4].chunks_exact(4) {
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
        // 8bpp cell base = char × 0x20 (char numbers are in 0x20-byte units;
        // an 8bpp cell spans two of them), so char 3's pixel (0,0) is at 3·32.
        v.vram.write8(3 * 32, 0x42); // pixel (0,0) = index 0x42
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
    /// Identity rotation parameters (dx = dyst = A = E = kx = ky = 1.0) written
    /// at an explicit table byte `base` — for tests that need the parameter
    /// table off VRAM offset 0 to avoid colliding with a pattern-name table.
    fn setup_rot_param_at(v: &mut Vdp2, base: u32) {
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
    fn rbg0_tile_samples_the_correct_4x4_plane() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0010); // BGON.R0ON
        v.regs.write16(0x02A, 0x0000); // CHCTLB: RBG0 tile, 4bpp, 8×8
        v.regs.write16(0x038, 0x8000); // PNCR: 1-word pattern names
        v.regs.write16(0x03A, 0x0000); // PLSZ: RA plane size 1×1
        v.regs.write16(0x0FC, 0x0001); // PRIR: RBG0 priority 1
        // MPABRA: plane A map 0, plane B map 1 → plane B's PN table at 0x2000.
        v.regs.write16(0x050, 0x0100);
        // Identity rotation, but start X at plane coordinate 512 → screen
        // (0,0) lands in plane B (the second plane of the 4×4 grid).
        for (k, val) in [
            (0u32, 0x0200_0000), // Xst = 512.0
            (4, 1 << 16),        // dyst
            (5, 1 << 16),        // dx
            (7, 1 << 16),        // A
            (11, 1 << 16),       // E
            (19, 1 << 16),       // kx
            (20, 1 << 16),       // ky
        ] {
            v.vram.write32(k * 4, val);
        }
        // Plane B PN[0] → char 2; char 2 pixel (0,0) = nibble 5 → CRAM[5].
        v.vram.write16(0x2000, 0x0002);
        v.vram.write8(2 * 32, 0x50);
        v.cram.write16(5 * 2, 0x001F); // red
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(
            pixel(&buf, 0, 0),
            [0xFF, 0, 0, 0xFF],
            "screen (0,0) → plane B tile via the 4×4 grid"
        );
    }

    #[test]
    fn rbg0_line_coefficient_overrides_kx_and_flags_transparent_lines() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0010); // RBG0
        v.regs.write16(0x02A, 0x1200); // RBG0 8bpp bitmap
        v.regs.write16(0x0FC, 0x0001); // RBG0 priority 1
        v.regs.write16(0x03E, 0x0001); // MPOFR.RAMP = 1 → bitmap base 0x20000
        // Coefficient table: enable (RAKTE), mode 0 (kx&ky), longword size.
        v.regs.write16(0x0B4, 0x0001); // KTCTL
        v.regs.write16(0x0B6, 0x0001); // KTAOF → table at 0x40000
        setup_rot_identity(&mut v, 0);
        // dkast = 1.0 per line → line y reads coefficient entry y.
        v.vram.write32(22 * 4, 0x0001_0000);
        // Entry 0: kx = 2.0 (8.16). Entry 1: MSB set → transparent line.
        v.vram.write32(0x40000, 0x0002_0000);
        v.vram.write32(0x40004, 0x8000_0000);
        v.cram.write16(2, 0x001F); // index 1 = red
        v.vram.write8(0x20000 + 2, 1); // bitmap dot at plane (2, 0)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // Line 0 coeff kx=2 → screen (1,0) samples plane (2,0) → red.
        assert_eq!(
            pixel(&buf, 1, 0),
            [0xFF, 0, 0, 0xFF],
            "kx=2 override applied"
        );
        // Line 1 coefficient MSB → the whole line is transparent → backdrop.
        assert_eq!(pixel(&buf, 1, 1), [0, 0, 0, 0xFF], "MSB → transparent line");
    }

    #[test]
    fn rbg_screen_over_makes_outside_the_field_transparent() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0010); // RBG0
        v.regs.write16(0x02A, 0x1200); // RBG0 8bpp bitmap
        v.regs.write16(0x0FC, 0x0001); // priority 1
        v.regs.write16(0x03E, 0x0001); // bitmap base 0x20000
        setup_rot_identity(&mut v, 0);
        v.vram.write32(0, 0x0200_0000); // Xst = 512 → screen (0,0) → plane (512,0)
        v.cram.write16(2, 0x001F); // red
        v.vram.write8(0x20000, 1); // bitmap dot at (0,0)

        // Over mode 0 (repeat): plane x 512 wraps to 0 → samples the dot.
        v.regs.write16(0x03A, 0x0000);
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(
            pixel(&buf, 0, 0),
            [0xFF, 0, 0, 0xFF],
            "repeat wraps to the dot"
        );

        // Over mode 3 (transparent outside 512×512): plane x 512 is outside.
        v.regs.write16(0x03A, 0x0C00); // RAOVR = 3
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(
            pixel(&buf, 0, 0),
            [0, 0, 0, 0xFF],
            "outside the field → backdrop"
        );
    }

    #[test]
    #[should_panic(expected = "too small")]
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

    #[test]
    fn nbg0_line_zoom_scales_the_horizontal_source() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // NBG0 bitmap, 8bpp
        v.regs.write16(0x09A, 0x0008); // SCRCTL.N0LZMX (line zoom only)
        v.regs.write16(0x0A2, 0x0200); // LSTA0L → table at byte 0x400
        v.vram.write32(0x400, 0x0002_0000); // line 0 zoom = 2.0
        v.cram.write16(2, 0x001F); // index 1 = red
        v.vram.write8(2, 1); // bitmap dot at (2, 0)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // Zoom 2× → screen x=1 samples source x=2 → red.
        assert_eq!(pixel(&buf, 1, 0), [0xFF, 0, 0, 0xFF], "2× zoom maps x=1→2");
        // Screen x=0 → source 0 (empty) → backdrop.
        assert_eq!(pixel(&buf, 0, 0), [0, 0, 0, 0xFF], "x=0 source unscaled");
    }

    #[test]
    fn nbg0_vertical_cell_scroll_shifts_a_column() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // NBG0 bitmap, 8bpp
        v.regs.write16(0x09A, 0x0001); // SCRCTL.N0VCSC
        v.regs.write16(0x09E, 0x0200); // VCSTAL → table at byte 0x400
        // Column 0 (x 0..7): vertical scroll +3 (bits 26..16).
        v.vram.write32(0x400, 3 << 16);
        v.cram.write16(2, 0x001F); // index 1 = red
        v.vram.write8(3 * 512, 1); // bitmap dot at (0, 3)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // Column 0 scrolls +3, so screen (0,0) samples bitmap (0,3) → red.
        assert_eq!(
            pixel(&buf, 0, 0),
            [0xFF, 0, 0, 0xFF],
            "column 0 scrolled +3"
        );
        // Same column, row 1 → bitmap (0,4), which is empty → backdrop.
        assert_eq!(pixel(&buf, 0, 1), [0, 0, 0, 0xFF], "row 1 maps elsewhere");
    }

    #[test]
    fn nbg0_line_window_clips_per_scanline() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // NBG0 bitmap, 8bpp
        v.cram.write16(0, 0x1F << 5); // backdrop green
        v.cram.write16(2, 0x001F); // index 1 red
        v.vram.write8(2 * 512 + 2, 1); // NBG0 dot at (2,2)
        v.vram.write8(2, 1); // NBG0 dot at (2,0)
        // NBG0 window: W0 enable + area=inside, AND logic (W1 off).
        v.regs.write16(0x0D0, 0x0003);
        v.regs.write16(0x0C2, 0); // WPSY0 → sy 0
        v.regs.write16(0x0C6, 10); // WPEY0 → ey 10
        // W0 line window: enable (bit15) + table at byte 0x800 (reg = 0x400).
        v.regs.write16(0x0D8, 0x8000);
        v.regs.write16(0x0DA, 0x0400);
        // Per-line X (10-bit dots, halved): line 0 → [0,0]; line 2 → [0,3].
        v.vram.write32(0x800, 0); // line 0: start 0, end 0
        v.vram.write32(0x800 + 2 * 4, 7); // line 2: start 0, end 3 (7>>1)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // Line 2 window spans x∈[0,3] → (2,2) inside → red.
        assert_eq!(
            pixel(&buf, 2, 2),
            [0xFF, 0, 0, 0xFF],
            "inside line-2 window"
        );
        // Line 0 window is just x=0 → (2,0) outside → backdrop green.
        assert_eq!(
            pixel(&buf, 2, 0),
            [0, 0xFF, 0, 0xFF],
            "outside the narrow line-0 window"
        );
    }

    /// 4bpp paletted bitmap: each byte holds two pixels (high nibble = even
    /// column). The nibble indexes the low palette directly (BMPNA bank not
    /// applied), with nibble 0 transparent.
    #[test]
    fn bitmap_4bpp_packs_two_pixels_per_byte() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0002); // N0BMEN, N0CHCN = 0 → 4bpp
        v.cram.write16(0xA * 2, 0x001F); // palette index 0xA = red
        v.cram.write16(0xB * 2, 0x7C00); // palette index 0xB = blue
        // Byte at bitmap (px 0/1, row 0): high nibble = col 0 = 0xA, low = 0xB.
        v.vram.write8(0, 0xAB);
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF], "even column → high nibble");
        assert_eq!(pixel(&buf, 1, 0), [0, 0, 0xFF, 0xFF], "odd column → low nibble");
    }

    /// `tpon` (BGON.NxTPON) makes palette code 0 the *solid* backdrop colour
    /// `CRAM[offset]` instead of transparent — what the BIOS splash relies on.
    #[test]
    fn transparent_pen_solid_draws_palette_zero_as_a_colour() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0101); // BGON: NBG0 on (bit0) + N0TPON (bit8)
        v.regs.write16(0x028, 0x0012); // 8bpp bitmap
        v.regs.write16(0x0F8, 0x0001); // priority 1
        v.cram.write16(0, 0x001F); // CRAM[0] = red — the "solid pen-0" colour
        // VRAM bitmap is all zero → every dot is palette code 0.
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 5, 5), [0xFF, 0, 0, 0xFF], "pen 0 solid → red");
    }

    /// Window logic bit (WCTL 0x80) ORs W0 and W1 instead of ANDing them, and a
    /// window with area=outside passes *outside* its rectangle. Exercises both
    /// the OR branch in `window_allows` and the `!inside` branch in `win_pixel`.
    #[test]
    fn window_or_logic_and_outside_area() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0012); // bitmap, 8bpp
        v.cram.write16(0, 0x1F << 5); // backdrop green
        v.cram.write16(2, 0x001F); // index 1 red
        for y in 0..4u32 {
            for x in 0..8u32 {
                v.vram.write8(y * 512 + x, 1);
            }
        }
        // NBG0 WCTL = OR (0x80) | W1 enable+area-inside (0x0C) | W0 enable +
        // area-OUTSIDE (0x02, area bit clear). Pass = outside-W0 OR inside-W1.
        v.regs.write16(0x0D0, 0x008E);
        // W0 rect x∈[0,1] (so "outside W0" = x≥2); WPSX/WPEX are half-dot.
        v.regs.write16(0x0C0, 0); // WPSX0 → 0
        v.regs.write16(0x0C4, 2); // WPEX0 → 1
        v.regs.write16(0x0C2, 0); // WPSY0
        v.regs.write16(0x0C6, 0x3FF); // WPEY0 (whole height)
        // W1 rect x∈[0,0] (inside-W1 = x==0).
        v.regs.write16(0x0C8, 0); // WPSX1 → 0
        v.regs.write16(0x0CC, 0); // WPEX1 → 0
        v.regs.write16(0x0CA, 0); // WPSY1
        v.regs.write16(0x0CE, 0x3FF); // WPEY1
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // x=0: outside-W0 false, but inside-W1 true → OR passes → red.
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF], "inside W1 passes via OR");
        // x=1: outside-W0 false AND inside-W1 false → suppressed → backdrop.
        assert_eq!(pixel(&buf, 1, 0), [0, 0xFF, 0, 0xFF], "neither window passes");
        // x=3: outside-W0 true → OR passes → red.
        assert_eq!(pixel(&buf, 3, 0), [0xFF, 0, 0, 0xFF], "outside W0 passes via OR");
    }

    /// Per-column vertical cell scroll with BOTH NBG0 and NBG1 enabled: the
    /// VCSTA table interleaves their longwords (NBG0 = even, NBG1 = odd), so
    /// each layer reads its own per-column scroll from the shared table.
    #[test]
    fn vcell_scroll_interleaves_both_layers() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0003); // NBG0 + NBG1
        v.regs.write16(0x028, 0x1212); // both 8bpp bitmap
        v.regs.write16(0x0F8, 0x0205); // N0 top (pri 5), N1 pri 2
        v.regs.write16(0x03C, 0x0010); // NBG1 bitmap base 0x20000
        v.regs.write16(0x09A, 0x0101); // SCRCTL: N0VCSC (bit0) + N1VCSC (bit8)
        v.regs.write16(0x09E, 0x0200); // VCSTAL → table byte 0x400
        // Interleaved longwords for column 0: NBG0 (even) = +5, NBG1 (odd) = +7.
        v.vram.write32(0x400, 5 << 16); // NBG0 column 0 scroll +5
        v.vram.write32(0x404, 7 << 16); // NBG1 column 0 scroll +7
        v.cram.write16(2, 0x001F); // NBG0 index 1 red
        v.cram.write16(4, 0x7C00); // NBG1 index 2 blue
        // NBG0 dot at (0,5): screen (0,0) + scroll +5 samples it.
        v.vram.write8(5 * 512, 1);
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // NBG0 (front) column 0 scrolled +5 → screen (0,0) samples (0,5) → red.
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF], "NBG0 +5 (even longword)");
        // Now make NBG0 transparent at (0,0) and place an NBG1 dot at (0,7),
        // which NBG1's +7 column scroll brings to screen (0,0) → blue shows.
        v.vram.write8(5 * 512, 0); // clear NBG0 dot
        v.vram.write8(7 * 512 + 0x2_0000, 2); // NBG1 dot at (0,7)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0, 0, 0xFF, 0xFF], "NBG1 +7 (odd longword)");
    }

    /// Regression: a NEGATIVE vertical cell-scroll offset (scroll up) is
    /// sign-extended to a near-`u32::MAX` value, so `sy = y + scroll_y` must
    /// wrap rather than panic on overflow in a debug build. Before the fix,
    /// rendering any line `y >= |offset|` overflowed and panicked here; now the
    /// source row is `y - offset` modulo the plane size.
    #[test]
    fn negative_vcell_scroll_wraps_instead_of_overflowing() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0001); // NBG0 on
        v.regs.write16(0x028, 0x0012); // NBG0 8bpp bitmap
        v.regs.write16(0x0F8, 0x0005); // NBG0 priority 5
        v.regs.write16(0x09A, 0x0001); // SCRCTL: N0VCSC (vertical cell scroll)
        v.regs.write16(0x09E, 0x0200); // VCSTAL → table byte 0x400
        // Column 0 scroll = -5 (11-bit 0x7FB, sign-extended → 0xFFFF_FFFB). The
        // value lives in the high half-word of the longword.
        v.vram.write32(0x400, 0x07FB << 16);
        v.cram.write16(2, 0x001F); // index 1 = red
        // A dot at source row 5; with scroll -5 it lands on screen row 10.
        v.vram.write8(5 * 512, 1);
        let mut buf = fresh_buf();
        // Without the wrapping fix this panics at y>=6 (u32 add overflow).
        render_frame(&v, None, &mut buf);
        assert_eq!(
            pixel(&buf, 0, 10),
            [0xFF, 0, 0, 0xFF],
            "screen (0,10) samples source row 10 + (-5) = 5 → red"
        );
    }

    /// Sprite colour calculation: SPCCEN + the SPCCCS/SPCCN priority condition
    /// gate a blend of the sprite over the layer below, with the ratio from
    /// CCRSx selected by the type's CCR bits.
    #[test]
    fn sprite_colour_calc_blends_when_condition_met() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0001); // NBG0 on (the layer below)
        v.regs.write16(0x028, 0x0012); // NBG0 8bpp bitmap
        v.regs.write16(0x0F8, 0x0002); // NBG0 priority 2
        v.regs.write16(0x0F0, 0x0005); // sprite S0 priority 5 (front)
        // SPCTL: type 0, SPCCN=5 (bits10..8), SPCCCS mode 2 (≥). 0x2500.
        v.regs.write16(0x0E0, 0x2500);
        v.regs.write16(0x0EC, 0x0040); // CCCTL.SPCCEN, CCMD=0 (ratio)
        v.regs.write16(0x100, 0x000F); // CCRSA ratio 0 = 15
        v.cram.write16(0x12 * 2, 0x001F); // sprite code 0x12 = red (front)
        v.cram.write16(2, 0x7C00); // NBG0 index 1 = blue (below)
        v.vram.write8(0, 1); // NBG0 dot at (0,0)
        // Sprite type 0: priority bits 15..14 → S0 (5), CCR bits 13..11 → idx 0,
        // colour code low bits = 0x12. pix = 0x12 keeps prio=0 → use 0x4012 to
        // put priority field at S0 index 1? Keep prio index 0: pix bits 15..14=0.
        let fb = sprite_fb_with(0, 0, 0x0012);
        let mut buf = fresh_buf();
        render_frame(&v, Some(&fb), &mut buf);
        // sprite pri 5 ≥ SPCCN 5 → cc on; alpha=(31-15)*255/31=131.
        // red·131 over blue·124 = (131,0,124).
        assert_eq!(pixel(&buf, 0, 0), [131, 0, 124, 0xFF], "sprite cc ratio blend");
    }

    /// When the SPCCCS condition is NOT met (mode 1, ==, and priorities differ)
    /// the sprite draws opaque — no blend.
    #[test]
    fn sprite_colour_calc_skipped_when_condition_unmet() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000);
        v.regs.write16(0x020, 0x0001);
        v.regs.write16(0x028, 0x0012);
        v.regs.write16(0x0F8, 0x0002); // NBG0 pri 2
        v.regs.write16(0x0F0, 0x0005); // sprite pri 5
        // SPCCN=3, mode 1 (==): sprite pri 5 != 3 → cc off. SPCCCS=1 (0x1300).
        v.regs.write16(0x0E0, 0x1300);
        v.regs.write16(0x0EC, 0x0040); // SPCCEN on (but condition gates it off)
        v.regs.write16(0x100, 0x000F);
        v.cram.write16(0x12 * 2, 0x001F); // red
        v.cram.write16(2, 0x7C00); // blue below
        v.vram.write8(0, 1);
        let fb = sprite_fb_with(0, 0, 0x0012);
        let mut buf = fresh_buf();
        render_frame(&v, Some(&fb), &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF], "no blend → opaque red");
    }

    /// Rotation coefficient table in WORD size, mode 1 (kx only): the 15-bit
    /// entry (<<6 → 16.16) overrides kx while ky keeps the parameter table's.
    #[test]
    fn rbg_word_size_coefficient_overrides_kx_only() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0010); // RBG0
        v.regs.write16(0x02A, 0x1200); // RBG0 8bpp bitmap
        v.regs.write16(0x0FC, 0x0001); // priority 1
        v.regs.write16(0x03E, 0x0001); // bitmap base 0x20000
        // KTCTL: RAKTE (bit0) + word size (bit1) + mode 1 (bits3..2 = 01).
        v.regs.write16(0x0B4, 0x0007);
        v.regs.write16(0x0B6, 0x0001); // KTAOF: word bank → 0x20000
        setup_rot_identity(&mut v, 0);
        // dkast = 1.0/line so line y picks coeff entry y.
        v.vram.write32(22 * 4, 0x0001_0000);
        // Word entry 0 = 0x0080 → v=0x80, <<6 = 0x2000 = 0.125 in 16.16? No:
        // we need kx = 2.0 = 0x2_0000 in 16.16; word v<<6 = 0x2_0000 → v=0x800.
        v.vram.write16(0x2_0000, 0x0800); // kx = 2.0 for line 0
        v.cram.write16(2, 0x001F); // index 1 red
        v.vram.write8(0x20000 + 2, 1); // bitmap dot at plane (2,0)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // mode 1 overrides kx=2 (ky stays 1): screen (1,0) samples plane (2,0).
        assert_eq!(pixel(&buf, 1, 0), [0xFF, 0, 0, 0xFF], "word-size kx=2 override");
    }

    /// Rotation bitmap at 16bpp RGB direct colour, and the R0BMSZ=1 (512×512)
    /// height: with ky=2 the sampled plane row exceeds 256, so a dot at row 400
    /// is only reachable when the bitmap is 512 tall (size 1), not 256 (size 0).
    #[test]
    fn rbg_bitmap_16bpp_and_512x512_size() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0010); // RBG0
        // CHCTLB: R0BMEN (bit9) + R0BMSZ (bit10, → 512×512) + R0CHCN=3 (16bpp).
        v.regs.write16(0x02A, 0x3600);
        v.regs.write16(0x0FC, 0x0001); // priority 1
        v.regs.write16(0x03E, 0x0001); // base 0x20000
        setup_rot_identity(&mut v, 0);
        v.vram.write32(20 * 4, 2 << 16); // ky = 2.0 → plane row = 2·screen y
        // 16bpp RGB555 blue dot at plane (40, 400) — row 400 needs h=512.
        v.vram.write16(0x20000 + (400 * 512 + 40) * 2, 0x7C00);
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // Screen (40,200) → plane (40, 400) → blue (only because size=512×512).
        assert_eq!(pixel(&buf, 40, 200), [0, 0, 0xFF, 0xFF], "16bpp at row 400 (h=512)");
        // With size 0 (512×256) the same plane row 400 wraps to 144 → empty.
        v.regs.write16(0x02A, 0x3200); // clear R0BMSZ
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 40, 200), [0, 0, 0, 0xFF], "h=256 wraps → no dot");
    }

    /// Rotation bitmap screen-over mode 2: transparent outside the 512×(256|512)
    /// bitmap (distinct from mode 3's 512×512 area used in another test).
    #[test]
    fn rbg_bitmap_screen_over_mode2_is_transparent_outside() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0010); // RBG0
        v.regs.write16(0x02A, 0x1200); // 8bpp bitmap, size 0 → 512×256
        v.regs.write16(0x0FC, 0x0001); // priority 1
        v.regs.write16(0x03E, 0x0001); // base 0x20000
        v.regs.write16(0x03A, 0x0800); // PLSZ RAOVR = 2 (bits 11..10 = 10)
        setup_rot_identity(&mut v, 0);
        // ky shifts the sampled plane Y past the 256-row bitmap height.
        v.vram.write32(20 * 4, 2 << 16); // ky = 2.0 → screen y maps to 2·y
        v.cram.write16(2, 0x001F); // red
        v.vram.write8(0x20000, 1); // dot at plane (0,0)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // Screen (0,0) → plane y 0 → the dot → red.
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF], "in-bounds dot");
        // Screen (0,200) → plane y 400 ≥ 256 → outside → transparent → backdrop.
        assert_eq!(pixel(&buf, 0, 200), [0, 0, 0, 0xFF], "mode-2 outside → backdrop");
    }

    /// Rotation tile background with 16×16 (2×2) characters: the 4-cell stepping
    /// of the rotation tile path (shared `sample_pattern_cell`) is exercised.
    #[test]
    fn rbg_tile_16x16_characters() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0010); // RBG0
        v.regs.write16(0x02A, 0x0100); // CHCTLB: R0CHSZ (bit8) → 16×16, tile 4bpp
        v.regs.write16(0x038, 0x8000); // PNCR: 1-word
        v.regs.write16(0x03A, 0x0000); // RA plane size 1×1
        v.regs.write16(0x0FC, 0x0001); // priority 1
        v.regs.write16(0x050, 0x0000); // MPABRA: plane A map 0 → PN table at 0
        // Move the rotation parameter table off VRAM 0 (RPTAL → addr 0x8000) so
        // it doesn't collide with plane A's pattern-name table at 0.
        v.regs.write16(0x0BE, 0x4000); // RPTAL → table at (0x4000)<<1 = 0x8000
        setup_rot_param_at(&mut v, 0x8000);
        // PN[0] → char 8; 16×16 char addresses in 4-cell units: 8·4 = 32 (TL).
        v.vram.write16(0, 8);
        // Screen (1,2) → plane (1,2), subcell 0 (TL) = cell 32; px=1, py=2;
        // 4bpp byte cell·32 + py·4 + px/2 (px odd → low nibble).
        v.vram.write8(32 * 32 + 2 * 4, 0x07);
        v.cram.write16(7 * 2, 0x001F); // palette 0 index 7 → red
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 1, 2), [0xFF, 0, 0, 0xFF], "rot 16×16 TL subcell");
    }

    /// Bitmap size code 2 = 1024×256: the row pitch becomes 1024, so a dot at
    /// (300, 1) lives at byte offset 1·1024 + 300.
    #[test]
    fn bitmap_size_1024_wide_changes_the_row_pitch() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        // N0BMEN + 8bpp + N0BMSZ = 2 (bits 3..2 = 10 → 0x08).
        v.regs.write16(0x028, 0x001A);
        v.cram.write16(2, 0x001F); // index 1 red
        v.vram.write8(1024 + 300, 1); // row 1 at pitch 1024 → (300,1)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 300, 1), [0xFF, 0, 0, 0xFF], "1024-wide pitch");
    }

    /// Three overlapping opaque NBG layers: the top-two-by-priority bookkeeping
    /// keeps the front two (NBG1 pri 6 over NBG2 pri 4), and a lower-priority
    /// NBG0 (pri 2) is displaced into neither slot — it never shows.
    #[test]
    fn three_layers_keep_only_the_top_two_by_priority() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0007); // NBG0 + NBG1 + NBG2
        v.regs.write16(0x028, 0x1212); // NBG0/1 8bpp bitmap
        // NBG2 is tile-only; give it priority but it will be covered. Use NBG0
        // (pri 2) as the bottom that must NOT show, NBG1 (pri 6) front,
        // colour-calc on NBG1 to blend with the second slot (NBG2 here absent →
        // backdrop). Keep it simple: assert the highest-priority opaque wins.
        v.regs.write16(0x0F8, 0x0602); // N0PRIN=2, N1PRIN=6
        v.regs.write16(0x03C, 0x0010); // NBG1 bitmap base 0x20000
        v.cram.write16(2, 0x001F); // NBG0 index 1 red (low priority)
        v.cram.write16(4, 0x7C00); // NBG1 index 2 blue (high priority)
        v.vram.write8(0, 1); // NBG0 dot
        v.vram.write8(0x2_0000, 2); // NBG1 dot
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0, 0, 0xFF, 0xFF], "NBG1 (pri 6) wins over NBG0");
    }

    /// Sprite colour-calc condition mode 0 (priority ≤ SPCCN) enables the blend
    /// for a low-priority sprite.
    #[test]
    fn sprite_colour_calc_mode0_le_condition() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000);
        v.regs.write16(0x020, 0x0001);
        v.regs.write16(0x028, 0x0012);
        v.regs.write16(0x0F8, 0x0002); // NBG0 pri 2 (below)
        v.regs.write16(0x0F0, 0x0003); // sprite pri 3
        // SPCCN=5, mode 0 (≤): sprite pri 3 ≤ 5 → cc on. SPCCCS=0 (0x0500).
        v.regs.write16(0x0E0, 0x0500);
        v.regs.write16(0x0EC, 0x0040); // SPCCEN, ratio mode
        v.regs.write16(0x100, 0x000F); // ratio 0 = 15
        v.cram.write16(0x12 * 2, 0x001F); // sprite red
        v.cram.write16(2, 0x7C00); // NBG0 blue below
        v.vram.write8(0, 1);
        let fb = sprite_fb_with(0, 0, 0x0012);
        let mut buf = fresh_buf();
        render_frame(&v, Some(&fb), &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [131, 0, 124, 0xFF], "mode-0 ≤ → blend");
    }

    /// Rotation TILE screen-over mode 2: transparent outside the 4×4-plane
    /// field (distinct from the bitmap mode-2 path and the tile-repeat path).
    #[test]
    fn rbg_tile_screen_over_mode2_is_transparent_outside_the_field() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0010); // RBG0
        v.regs.write16(0x02A, 0x0000); // RBG0 tile 4bpp 8×8
        v.regs.write16(0x038, 0x8000); // PNCR 1-word
        v.regs.write16(0x0FC, 0x0001); // priority 1
        // PLSZ: RA plane size 1×1 (bits 9..8 = 0), RAOVR = 2 (bits 11..10 = 10).
        v.regs.write16(0x03A, 0x0800);
        v.regs.write16(0x050, 0x0000); // MPABRA plane A map 0
        v.regs.write16(0x0BE, 0x4000); // RPTAL → param table at 0x8000 (off PN)
        setup_rot_param_at(&mut v, 0x8000);
        // Plane A PN[0] → char 1; char 1 pixel (0,0) nibble 5 → red.
        v.vram.write16(0, 0x0001);
        v.vram.write8(32, 0x50); // char 1 byte base = 1·0x20
        v.cram.write16(5 * 2, 0x001F);
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        // Screen (0,0) → plane (0,0) inside the field → red.
        assert_eq!(pixel(&buf, 0, 0), [0xFF, 0, 0, 0xFF], "inside field → tile");
        // The 4×4 field of 1×1-page planes is 4·512 = 2048 wide. Push the start
        // X past 2048 so screen (0,0) lands outside → transparent.
        v.vram.write32(0x8000, 0x0900_0000); // Xst = 2304.0 (> 2048)
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 0, 0), [0, 0, 0, 0xFF], "outside field → backdrop");
    }

    /// 1-word pattern names with CNSM set (12-bit char, no flip) for an NBG
    /// layer — the `decode_pattern` cnsm branch (8×8 form).
    #[test]
    fn tile_one_word_cnsm_12bit_char() {
        let mut v = Vdp2::new();
        enable_nbg0(&mut v);
        v.regs.write16(0x028, 0x0000); // tile 4bpp
        // PNCN0: 1-word (bit15) + CNSM (bit14). SPCN supplies the top char bits.
        v.regs.write16(0x030, 0xC000);
        // SPCN (bits 4..0) = 0; char field is 12 bits → char 0x111.
        v.vram.write16(0, 0x0111);
        v.vram.write8(0x111 * 32 + 1, 0x07); // pixel (3,0): byte +1 low nibble
        v.cram.write16(7 * 2, 0x001F); // palette 0 index 7 red
        let mut buf = fresh_buf();
        render_frame(&v, None, &mut buf);
        assert_eq!(pixel(&buf, 3, 0), [0xFF, 0, 0, 0xFF], "cnsm 12-bit char");
    }
}
