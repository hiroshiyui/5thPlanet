//! VDP1 plotter — the sprite/polygon draw engine.
//!
//! VDP1 renders by walking a *command table* in VRAM (see
//! [`super::command::Command`]) and drawing each command's primitive
//! into the frame buffer, which VDP2 later composites as the sprite
//! layer. This module ports the geometry and pixel pipeline:
//!
//! * **List walker** — END / jump / skip / call (one level) / return,
//!   matching CMDCTRL bits 14-12 (VDP1 manual §"Jump Mode").
//! * **Coordinate commands** — local-coordinate origin (type 0xA),
//!   system clipping (0x9), user clipping (0x8).
//! * **Primitives** — normal/scaled/distorted sprites (textured), and
//!   polygon/line/polyline (untextured), all reduced to a textured-quad
//!   rasteriser using forward-differenced edge walking with 16.16 fixed
//!   point (`FRAC_SHIFT`).
//! * **Pixel pipeline** — all six CMDPMOD colour modes, the SPD
//!   (transparent-pixel disable), MESH and end-code controls, plus the
//!   replace / shadow / half-luminance / half-transparent colour-calc
//!   modes.
//! * **Gouraud shading** (CMDPMOD bit 2) — the four per-vertex colours from
//!   the CMDGRDA table are interpolated across each primitive (per-edge in the
//!   quad rasteriser, bilinear over the 1:1 normal-sprite rect) and each
//!   RGB555 channel is offset by `correction - 16`, clamped 0..31.
//!
//! The algorithm mirrors MAME's `saturn_v.cpp` plotter (used as a
//! behaviour reference); the field semantics follow the VDP1 User's
//! Manual, which is authoritative where the two differ.

use super::command::Command;
use super::framebuffer::{FB_HEIGHT, FB_STRIDE, Framebuffer};
use super::vram::Vram;

/// 16.16 fixed-point fraction shift for sub-pixel edge stepping.
const FRAC_SHIFT: i32 = 16;

/// Debug (`SAT_VDP1LOG`): dump each drawn command at plot time. The env var is
/// read once and cached, so `Plotter::new` (per plot) pays a single atomic load
/// rather than a process-global env lookup.
#[inline]
fn vdp1log() -> bool {
    use std::sync::OnceLock;
    static VDP1LOG: OnceLock<bool> = OnceLock::new();
    *VDP1LOG.get_or_init(|| std::env::var_os("SAT_VDP1LOG").is_some())
}

/// Inclusive clipping rectangle in frame-buffer pixel coordinates.
#[derive(Clone, Copy, Debug)]
struct Rect {
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
}

#[inline]
fn sign_extend(value: u16, bits: u32) -> i32 {
    ((value as i32) << (32 - bits)) >> (32 - bits)
}

/// One quad corner with its texture coordinate and per-vertex gouraud colour
/// channels (all 16.16 once scaled).
#[derive(Clone, Copy, Debug, Default)]
struct SPoint {
    x: i32,
    y: i32,
    u: i32,
    v: i32,
    r: i32,
    g: i32,
    b: i32,
}

/// A walked edge: framebuffer x, texture (u, v), and gouraud (r, g, b), all
/// 16.16.
#[derive(Clone, Copy, Debug, Default)]
struct Edge {
    x: i32,
    u: i32,
    v: i32,
    r: i32,
    g: i32,
    b: i32,
}

impl Edge {
    /// The edge anchored at a quad corner (drops the corner's y).
    fn at(p: &SPoint) -> Self {
        Edge {
            x: p.x,
            u: p.u,
            v: p.v,
            r: p.r,
            g: p.g,
            b: p.b,
        }
    }

    /// Advance every channel by `n` steps of slope `s` (forward differencing).
    fn advance(&mut self, s: &Edge, n: i32) {
        self.x += n * s.x;
        self.u += n * s.u;
        self.v += n * s.v;
        self.r += n * s.r;
        self.g += n * s.g;
        self.b += n * s.b;
    }
}

/// Which pixel writer the current command selects.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PixelMode {
    /// Untextured polygon/line in plain replace mode — writes CMDCOLR
    /// unconditionally (no transparency), matching MAME's `drawpixel_poly`.
    Poly,
    /// The full path: colour-mode decode, transparency, end-code, mesh,
    /// and the colour-calc modes.
    Generic,
}

/// Outcome of processing a command list.
#[derive(Clone, Copy, Debug)]
pub struct PlotResult {
    /// Number of commands visited (per-command setup cost in draw timing).
    pub command_count: u32,
    /// Number of frame-buffer dots processed (per-pixel cost in draw timing).
    pub pixels: u32,
    /// Modelled draw duration in SH-2 cycles — the Mednafen-faithful cost
    /// walk ([`super::timing::DrawTiming`], M12 task #6). Drives
    /// [`super::Vdp1::begin_plot`]'s `busy_until`.
    pub cycles: u64,
    /// Final command-table position (>>3 form for COPR).
    pub copr: u16,
}

/// One pass over a VDP1 command list: borrows VRAM (read) and the frame buffer
/// (write), carries the local-coordinate / clipping / pixel-mode state the
/// command stream mutates, and tallies the dots drawn for the draw-duration
/// model. Built per plot by [`Plotter::new`] and driven by [`super::Vdp1`].
pub struct Plotter<'a> {
    vram: &'a Vram,
    fb: &'a mut Framebuffer,
    local_x: i32,
    local_y: i32,
    sys_clip: Rect,
    user_clip: Rect,
    cmd: Command,
    mode: PixelMode,
    /// Per-vertex gouraud colours (R, G, B; 5-bit each) for the four quad
    /// corners A–D when CMDPMOD bit 2 is set, else `None`.
    gouraud: Option<[(i32, i32, i32); 4]>,
    /// Count of frame-buffer dots processed (drives the draw-duration model).
    pixels: u32,
    /// TVMR bit 0 — the frame buffer is 8 bits/pixel (1024×256 bytes); dots
    /// are written as single bytes and gouraud / colour-calc don't apply
    /// (Mednafen `PlotPixel` bpp8).
    bpp8: bool,
    /// FBCR bit 3 (DIE) — double-interlace plotting: only the DIL-selected
    /// field's lines are drawn, at half the y coordinate.
    die: bool,
    /// FBCR bit 2 (DIL) — which field DIE draws (0 = even lines, 1 = odd).
    dil: bool,
    /// Debug (`SAT_VDP1LOG`): dump each drawn command at plot time.
    log: bool,
}

impl<'a> Plotter<'a> {
    pub fn new(vram: &'a Vram, fb: &'a mut Framebuffer, bpp8: bool, die: bool, dil: bool) -> Self {
        // Default clip is the whole frame buffer — 512×256 dots, doubled
        // horizontally in the 8bpp layout and vertically under DIE (source-y
        // space is two fields). Command 0x09 (system clipping) narrows it,
        // command 0x08 sets the user one.
        let full = Rect {
            min_x: 0,
            max_x: if bpp8 { FB_STRIDE * 2 } else { FB_STRIDE } - 1,
            min_y: 0,
            max_y: if die { FB_HEIGHT * 2 } else { FB_HEIGHT } - 1,
        };
        Self {
            vram,
            fb,
            local_x: 0,
            local_y: 0,
            sys_clip: full,
            user_clip: full,
            cmd: Command::default(),
            mode: PixelMode::Generic,
            gouraud: None,
            pixels: 0,
            bpp8,
            die,
            dil,
            log: vdp1log(),
        }
    }

    /// Resolve the destination frame-buffer line for source `y`, honouring
    /// double-interlace plotting (Mednafen `PlotPixel` `die`): with FBCR.DIE
    /// only the DIL-selected field is drawn, at `y >> 1`; the other field's
    /// dots are dropped.
    #[inline]
    fn fb_y(&self, y: i32) -> Option<i32> {
        if self.die {
            if (y & 1) != self.dil as i32 {
                return None;
            }
            Some(y >> 1)
        } else {
            Some(y)
        }
    }

    /// Write one frame-buffer dot at the *resolved* line `fy`, honouring the
    /// TVM 8bpp layout: a byte at the 1024-wide stride (the low 8 bits of the
    /// colour), or the default 16-bit RGB555 word (Mednafen `PlotPixel`).
    #[inline]
    fn fb_put(&mut self, x: i32, fy: i32, pix: u16) {
        if !(0..FB_HEIGHT).contains(&fy) {
            return;
        }
        if self.bpp8 {
            if (0..FB_STRIDE * 2).contains(&x) {
                self.fb.set_pixel8(x, fy, pix as u8);
            }
        } else if (0..FB_STRIDE).contains(&x) {
            self.fb.set_pixel(x, fy, pix);
        }
    }

    /// Read the four per-vertex gouraud colours from VRAM when CMDPMOD bit 2
    /// (gouraud enable) is set; otherwise clear them. The table is four
    /// consecutive big-endian RGB555 words at `CMDGRDA << 3` (VDP1 manual
    /// §"Gouraud Shading"), one per vertex A, B, C, D.
    fn load_gouraud(&mut self) {
        self.gouraud = (self.cmd.pmod & 0x0004 != 0).then(|| {
            let base = (self.cmd.grda as u32) * 8;
            let rgb = |o: u32| {
                let w = self.vram.read16((base + o) & 0x7_FFFF);
                (
                    (w & 0x1F) as i32,
                    ((w >> 5) & 0x1F) as i32,
                    ((w >> 10) & 0x1F) as i32,
                )
            };
            [rgb(0), rgb(2), rgb(4), rgb(6)]
        });
    }

    /// Walk the command table from offset 0 and draw every command.
    /// Mirrors MAME's `vdp1_process_list`: the position advances per
    /// CMDCTRL jump mode, with a single level of call/return nesting.
    /// `timing` is the persistent draw-cycle accumulator (clip/local state
    /// survives across plots — see [`super::timing::DrawTiming`]).
    pub fn process_list(&mut self, timing: &mut super::timing::DrawTiming) -> PlotResult {
        const MAX_COMMANDS: u32 = 16383;
        let mut position: u32 = 0;
        let mut count: u32 = 0;
        let mut nest: Option<u32> = None;
        timing.set_bpp8(self.bpp8);
        let mut cycles: u64 = 0;

        while count < MAX_COMMANDS {
            count += 1;
            let cmd_addr = position * 0x20;
            self.cmd = Command::read(self.vram, cmd_addr);
            // Draw-cycle model: charge every fetched command (end and
            // skip-flagged ones cost their 16-cycle fetch too).
            let cmd_cycles = timing.command(self.vram, cmd_addr);
            cycles += cmd_cycles;

            if self.cmd.is_end() {
                break;
            }

            let mut draw = true;
            // CMDCTRL bits 14-12: jump-mode select. Bit 14 (0x4000) is
            // the SKIP flag; bits 13-12 select NEXT/ASSIGN/CALL/RETURN.
            match self.cmd.jump_mode() {
                0x0000 => position += 1,                          // next
                0x1000 => position = (self.cmd.link >> 2) as u32, // jump
                0x2000 => match nest {
                    // call (one level deep; nested calls ignored)
                    None => {
                        nest = Some(position + 1);
                        position = (self.cmd.link >> 2) as u32;
                    }
                    Some(_) => position += 1,
                },
                0x3000 => match nest.take() {
                    // return
                    Some(ret) => position = ret,
                    None => break,
                },
                0x4000 => {
                    draw = false;
                    position += 1;
                }
                0x5000 => {
                    draw = false;
                    position = (self.cmd.link >> 2) as u32;
                }
                0x6000 => {
                    draw = false;
                    match nest {
                        None => {
                            nest = Some(position + 1);
                            position = (self.cmd.link >> 2) as u32;
                        }
                        Some(_) => position += 1,
                    }
                }
                0x7000 => {
                    draw = false;
                    match nest.take() {
                        Some(ret) => position = ret,
                        None => break,
                    }
                }
                _ => unreachable!(),
            }

            if !draw {
                continue;
            }

            // Debug (SAT_VDP1LOG): dump every drawn command at plot time —
            // the post-swap VRAM rewrite makes after-the-fact list walks
            // unreliable. Observer-only.
            if self.log {
                let (cw, ch) = self.cmd.char_size();
                eprintln!(
                    "VDP1CMD @{:05X} cost={cmd_cycles} ctrl={:04X} pmod={:04X} colr={:04X} srca={:04X} size={cw}x{ch} A=({},{}) B=({},{}) C=({},{})",
                    cmd_addr,
                    self.cmd.ctrl,
                    self.cmd.pmod,
                    self.cmd.colr,
                    self.cmd.srca,
                    self.cmd.xa as i16,
                    self.cmd.ya as i16,
                    self.cmd.xb as i16,
                    self.cmd.yb as i16,
                    self.cmd.xc as i16,
                    self.cmd.yc as i16,
                );
            }

            // Clipping mode: CMDPMOD bit 10 selects the user clip,
            // otherwise the system clip bounds the primitive.
            let clip = if self.cmd.pmod & 0x0400 != 0 {
                self.user_clip
            } else {
                self.sys_clip
            };

            match self.cmd.command_type() {
                0x0 => {
                    self.cmd.ispoly = false;
                    self.set_pixel_mode();
                    self.load_gouraud();
                    self.draw_normal_sprite(&clip);
                }
                0x1 => {
                    self.cmd.ispoly = false;
                    self.set_pixel_mode();
                    self.load_gouraud();
                    self.draw_scaled_sprite(&clip);
                }
                0x2 | 0x3 => {
                    self.cmd.ispoly = false;
                    self.set_pixel_mode();
                    self.load_gouraud();
                    self.draw_distorted_sprite(&clip);
                }
                0x4 => {
                    self.cmd.ispoly = true;
                    self.set_pixel_mode();
                    self.load_gouraud();
                    self.draw_distorted_sprite(&clip);
                }
                0x5 | 0x7 => {
                    self.cmd.ispoly = true;
                    self.set_pixel_mode();
                    self.load_gouraud();
                    self.draw_poly_line(&clip);
                }
                0x6 => {
                    self.cmd.ispoly = true;
                    self.set_pixel_mode();
                    self.load_gouraud();
                    self.draw_line(&clip);
                }
                0x8 => {
                    // User clipping coordinates.
                    self.user_clip = Rect {
                        min_x: self.cmd.xa as i32,
                        max_x: self.cmd.xc as i32,
                        min_y: self.cmd.ya as i32,
                        max_y: self.cmd.yc as i32,
                    };
                }
                0x9 => {
                    // System clipping: lower-right only, origin (0,0).
                    self.sys_clip = Rect {
                        min_x: 0,
                        max_x: self.cmd.xc as i32,
                        min_y: 0,
                        max_y: self.cmd.yc as i32,
                    };
                }
                0xA => {
                    // Local coordinates are signed 11-bit values.
                    self.local_x = sign_extend(self.cmd.xa, 11);
                    self.local_y = sign_extend(self.cmd.ya, 11);
                }
                _ => {
                    // Illegal command: hardware prematurely ends the list.
                    break;
                }
            }
        }

        PlotResult {
            command_count: count,
            pixels: self.pixels,
            cycles,
            copr: ((position * 0x20) >> 3) as u16,
        }
    }

    #[inline]
    fn x2s(&self, v: u16) -> i32 {
        sign_extend(v, 13) + self.local_x
    }
    #[inline]
    fn y2s(&self, v: u16) -> i32 {
        sign_extend(v, 13) + self.local_y
    }

    /// Pick the pixel writer the way MAME's `vdp1_set_drawpixel` does:
    /// the untextured replace case uses the no-transparency `Poly`
    /// writer; everything else goes through `Generic`.
    fn set_pixel_mode(&mut self) {
        let pmod = self.cmd.pmod;
        let ty = self.cmd.command_type();
        let plain_replace = pmod & 0x7 == 0;
        let ecd = pmod & 0x80 != 0; // end-code disable
        let mesh = pmod & 0x100 != 0;
        let msb_on = pmod & 0x8000 != 0;
        self.mode = if (ty & 0x4) != 0 && plain_replace && ecd && !mesh && !msb_on {
            PixelMode::Poly
        } else {
            PixelMode::Generic
        };
    }

    // ---- pixel pipeline -----------------------------------------------

    /// Write one pixel. `pd` is the character byte address; `off` is the
    /// linear texel index `v*xsize + u`; `shade` is the interpolated gouraud
    /// colour (16.16 per channel) when gouraud is active.
    fn draw_pixel(&mut self, x: i32, y: i32, pd: i32, off: i32, shade: Option<(i32, i32, i32)>) {
        self.pixels += 1; // a processed dot, drawn or clipped, costs draw time
        if self.mode == PixelMode::Poly {
            if let Some(fy) = self.fb_y(y) {
                self.fb_put(x, fy, self.cmd.colr);
            }
            return;
        }
        self.draw_pixel_generic(x, y, pd, off, shade);
    }

    fn texel(&self, byte_addr: i32) -> u8 {
        self.vram.read8((byte_addr as u32) & 0x7_FFFF)
    }

    fn draw_pixel_generic(
        &mut self,
        x: i32,
        y: i32,
        pd: i32,
        off: i32,
        shade: Option<(i32, i32, i32)>,
    ) {
        let pmod = self.cmd.pmod;
        let mesh = pmod & 0x100 != 0;
        // Mesh keys off the *source* y (Mednafen `PlotPixel` applies it
        // before the interlace line-halving).
        if mesh && (x ^ y) & 1 == 0 {
            return;
        }
        let Some(fy) = self.fb_y(y) else { return };
        let in_bounds = (0..FB_HEIGHT).contains(&fy)
            && (0..if self.bpp8 { FB_STRIDE * 2 } else { FB_STRIDE }).contains(&x);
        if !in_bounds {
            return;
        }
        let spd = pmod & 0x40 != 0; // transparent-pixel disable

        let raw: i32;
        let mut pix: u16;
        if self.cmd.ispoly {
            raw = self.cmd.colr as i32;
            pix = self.cmd.colr;
        } else {
            // Colour mode: CMDPMOD bits 5-3.
            let endcode: i32;
            match pmod & 0x0038 {
                0x0000 => {
                    // 16-colour bank, 4bpp.
                    let mut r = self.texel(pd + off / 2) as i32;
                    r = if off & 1 != 0 {
                        r & 0x0F
                    } else {
                        (r & 0xF0) >> 4
                    };
                    raw = r;
                    pix = (r as u16).wrapping_add(self.cmd.colr & 0xFFF0);
                    endcode = 0x0F;
                }
                0x0008 => {
                    // 16-colour lookup table, 4bpp.
                    let mut r = self.texel(pd + off / 2) as i32;
                    r = if off & 1 != 0 {
                        r & 0x0F
                    } else {
                        (r & 0xF0) >> 4
                    };
                    raw = r;
                    let lut = (self.cmd.colr as u32) * 8 + (r as u32) * 2;
                    pix = self.vram.read16(lut & 0x7_FFFF);
                    endcode = 0x0F;
                }
                0x0010 => {
                    // 64-colour bank, 8bpp.
                    let r = self.texel(pd + off) as i32;
                    raw = r;
                    pix = (r as u16).wrapping_add(self.cmd.colr & 0xFFC0);
                    endcode = 0xFF;
                }
                0x0018 => {
                    // 128-colour bank, 8bpp.
                    let r = self.texel(pd + off) as i32;
                    raw = r;
                    pix = (r as u16).wrapping_add(self.cmd.colr & 0xFF80);
                    endcode = 0xFF;
                }
                0x0020 => {
                    // 256-colour bank, 8bpp.
                    let r = self.texel(pd + off) as i32;
                    raw = r;
                    pix = (r as u16).wrapping_add(self.cmd.colr & 0xFF00);
                    endcode = 0xFF;
                }
                0x0028 => {
                    // 32,768-colour RGB, 16bpp (big-endian halfword).
                    let r = self.vram.read16(((pd + off * 2) as u32) & 0x7_FFFF);
                    raw = r as i32;
                    pix = r;
                    endcode = 0x7FFF;
                }
                0x0038 => {
                    // Invalid setting: hardware reads VRAM address 0.
                    let r = self.vram.read16(0);
                    raw = r as i32;
                    pix = r;
                    endcode = -1; // never matches a u16 raw
                }
                // REVIEW(magic): 0x0030 is documented illegal; MAME emits
                // a random pixel. We emit a defined transparent pixel
                // rather than nondeterministic output.
                _ => {
                    raw = 0;
                    pix = 0;
                    endcode = 0xFF;
                }
            }

            // End-code: unless disabled (CMDPMOD bit 7), a run-terminator
            // texel is skipped.
            if pmod & 0x80 == 0 && raw == endcode {
                return;
            }
        }

        // MSBON — force the MSB (used by VDP2 for shadow/priority).
        pix |= pmod & 0x8000;

        // transpen is 0; draw unless transparent and SPD is off.
        if raw == 0 && !spd {
            return;
        }

        // TVM 8bpp frame buffer: one byte per dot — gouraud and the
        // colour-calc blends don't apply; MSBON instead read-modifies the
        // dot's byte from its containing word with bit 15 forced (Mednafen
        // `PlotPixel` bpp8 path, vdp1_common.h:231).
        if self.bpp8 {
            let b = if pmod & 0x8000 != 0 {
                let word = self.fb.pixel(x >> 1, fy);
                (word | 0x8000) >> (((x & 1) ^ 1) << 3)
            } else {
                pix
            };
            self.fb.set_pixel8(x, fy, b as u8);
            return;
        }

        // Gouraud shading (CMDPMOD bit 2): offset each RGB555 channel by the
        // interpolated correction (centred at 16 → no change), clamped 0..31.
        if let Some((gr, gg, gb)) = shade {
            pix = apply_gouraud(pix, gr, gg, gb);
        }

        // Colour-calc mode: CMDPMOD bits 1-0.
        match pmod & 0x3 {
            0 => self.fb.set_pixel(x, fy, pix), // replace
            1 => {
                // shadow: halve the destination if its MSB is set.
                let d = self.fb.pixel(x, fy);
                if d & 0x8000 != 0 {
                    self.fb.set_pixel(x, fy, ((d & !0x8421) >> 1) | 0x8000);
                }
            }
            2 => {
                // half luminance.
                self.fb.set_pixel(x, fy, ((pix & !0x8421) >> 1) | 0x8000);
            }
            3 => {
                // half transparent: blend with destination if its MSB set.
                let d = self.fb.pixel(x, fy);
                if d & 0x8000 != 0 {
                    self.fb.set_pixel(x, fy, alpha_blend_rgb555(d, pix) | 0x8000);
                } else {
                    self.fb.set_pixel(x, fy, pix);
                }
            }
            _ => unreachable!(),
        }
    }

    // ---- primitives ---------------------------------------------------

    fn draw_normal_sprite(&mut self, clip: &Rect) {
        let mut x = self.x2s(self.cmd.xa);
        let mut y = self.y2s(self.cmd.ya);
        let dir = self.cmd.direction();
        let (mut xsize, mut ysize) = self.cmd.char_size();
        let pd = self.cmd.pattern_addr() as i32;

        if x > clip.max_x || y > clip.max_y {
            return;
        }

        let mut u: i32 = 0;
        let mut dux: i32 = 1;
        let mut duy: i32 = xsize;
        if dir & 0x1 != 0 {
            // h-flip
            dux = -1;
            u = xsize - 1;
        }
        if dir & 0x2 != 0 {
            // v-flip
            duy = -xsize;
            u += xsize * (ysize - 1);
        }
        if y < clip.min_y {
            u += xsize * (clip.min_y - y);
            ysize -= clip.min_y - y;
            y = clip.min_y;
        }
        if x < clip.min_x {
            u += dux * (clip.min_x - x);
            xsize -= clip.min_x - x;
            x = clip.min_x;
        }
        let max_y = (y + ysize - 1).min(clip.max_y);
        let max_x = (x + xsize - 1).min(clip.max_x);
        // For a normal (1:1) sprite the gouraud colours are bilinearly
        // interpolated across the drawn rectangle (A=TL, B=TR, C=BR, D=BL); the
        // quad rasteriser handles the scaled/distorted/polygon cases.
        let (gw, gh) = (max_x - x, max_y - y);
        let mut dy = y;
        while dy <= max_y {
            let mut uu = u;
            let mut dx = x;
            while dx <= max_x {
                let shade = self
                    .gouraud
                    .map(|g| gouraud_bilerp(&g, dx - x, dy - y, gw, gh));
                self.draw_pixel(dx, dy, pd, uu, shade);
                uu += dux;
                dx += 1;
            }
            u += duy;
            dy += 1;
        }
    }

    fn draw_scaled_sprite(&mut self, clip: &Rect) {
        let mut dir = self.cmd.direction();
        let (xsize, ysize) = self.cmd.char_size();
        let pd = self.cmd.pattern_addr() as i32;
        let zoom = ((self.cmd.ctrl & 0x0F00) >> 8) as i32;

        let mut x = sign_extend(self.cmd.xa, 13);
        let mut y = sign_extend(self.cmd.ya, 13);
        let mut screen_w = sign_extend(self.cmd.xb, 13);
        let mut screen_h = sign_extend(self.cmd.yb, 13);
        let mut h_neg = false;
        if screen_w < 0 && zoom != 0 {
            screen_w = -screen_w;
            dir |= 1;
        }
        if screen_h < 0 && zoom != 0 {
            h_neg = true;
            screen_h = -screen_h;
            dir |= 2;
        }
        let x2 = self.cmd.xc;
        let y2 = self.cmd.yc;

        // Zoom-point anchors (CMDCTRL bits 11-8): adjust the origin.
        match zoom {
            0x6 => x -= screen_w / 2,
            0x7 => x -= screen_w,
            0x9 => y -= screen_h / 2,
            0xA => {
                y -= screen_h / 2;
                x -= screen_w / 2;
            }
            0xB => {
                y -= screen_h / 2;
                x -= screen_w;
            }
            0xD => y -= screen_h,
            0xE => {
                y -= screen_h;
                x -= screen_w / 2;
            }
            0xF => {
                y -= screen_h;
                x -= screen_w;
            }
            _ => {}
        }

        let mut q = [SPoint::default(); 4];
        if zoom != 0 {
            let sx = self.x2s_i(x);
            let sy = self.y2s_i(y);
            q[0].x = sx;
            q[0].y = sy;
            q[1].x = sx + screen_w;
            q[1].y = sy;
            q[2].x = sx + screen_w;
            q[2].y = sy + screen_h;
            q[3].x = sx;
            q[3].y = sy + screen_h;
            if h_neg {
                for p in q.iter_mut() {
                    p.y += screen_h;
                }
            }
        } else {
            q[0].x = self.x2s_i(x);
            q[0].y = self.y2s_i(y);
            q[1].x = self.x2s(x2);
            q[1].y = self.y2s_i(y);
            q[2].x = self.x2s(x2);
            q[2].y = self.y2s(y2);
            q[3].x = self.x2s_i(x);
            q[3].y = self.y2s(y2);
        }
        self.assign_uv(&mut q, dir, xsize, ysize);
        self.fill_quad(clip, pd, xsize, &q);
    }

    fn draw_distorted_sprite(&mut self, clip: &Rect) {
        let dir = self.cmd.direction();
        let (xsize, ysize, pd) = if self.cmd.ispoly {
            (1, 1, 0)
        } else {
            let (xs, ys) = self.cmd.char_size();
            if xs == 0 || ys == 0 {
                return; // setting prohibited
            }
            (xs, ys, self.cmd.pattern_addr() as i32)
        };

        let mut q = [SPoint::default(); 4];
        q[0].x = self.x2s(self.cmd.xa);
        q[0].y = self.y2s(self.cmd.ya);
        q[1].x = self.x2s(self.cmd.xb);
        q[1].y = self.y2s(self.cmd.yb);
        q[2].x = self.x2s(self.cmd.xc);
        q[2].y = self.y2s(self.cmd.yc);
        q[3].x = self.x2s(self.cmd.xd);
        q[3].y = self.y2s(self.cmd.yd);
        self.assign_uv(&mut q, dir, xsize, ysize);
        self.fill_quad(clip, pd, xsize, &q);
    }

    fn draw_line(&mut self, clip: &Rect) {
        let mut q = [SPoint::default(); 4];
        q[0].x = self.x2s(self.cmd.xa);
        q[0].y = self.y2s(self.cmd.ya);
        q[1].x = self.x2s(self.cmd.xb);
        q[1].y = self.y2s(self.cmd.yb);
        q[2] = q[0];
        q[3] = q[1];
        self.fill_quad(clip, 0, 1, &q);
    }

    fn draw_poly_line(&mut self, clip: &Rect) {
        let pts = [
            (self.cmd.xa, self.cmd.ya, self.cmd.xb, self.cmd.yb),
            (self.cmd.xb, self.cmd.yb, self.cmd.xc, self.cmd.yc),
            (self.cmd.xc, self.cmd.yc, self.cmd.xd, self.cmd.yd),
            (self.cmd.xd, self.cmd.yd, self.cmd.xa, self.cmd.ya),
        ];
        for (ax, ay, bx, by) in pts {
            let mut q = [SPoint::default(); 4];
            q[0].x = self.x2s(ax);
            q[0].y = self.y2s(ay);
            q[1].x = self.x2s(bx);
            q[1].y = self.y2s(by);
            q[2] = q[0];
            q[3] = q[1];
            self.fill_quad(clip, 0, 1, &q);
        }
    }

    #[inline]
    fn x2s_i(&self, v: i32) -> i32 {
        v + self.local_x
    }
    #[inline]
    fn y2s_i(&self, v: i32) -> i32 {
        v + self.local_y
    }

    /// Assign texture coordinates to the four quad corners, honouring
    /// the h/v-flip direction bits.
    fn assign_uv(&self, q: &mut [SPoint; 4], dir: u16, xsize: i32, ysize: i32) {
        if dir & 1 != 0 {
            q[0].u = xsize - 1;
            q[3].u = xsize - 1;
            q[1].u = 0;
            q[2].u = 0;
        } else {
            q[0].u = 0;
            q[3].u = 0;
            q[1].u = xsize - 1;
            q[2].u = xsize - 1;
        }
        if dir & 2 != 0 {
            q[0].v = ysize - 1;
            q[1].v = ysize - 1;
            q[2].v = 0;
            q[3].v = 0;
        } else {
            q[0].v = 0;
            q[1].v = 0;
            q[2].v = ysize - 1;
            q[3].v = ysize - 1;
        }
    }

    // ---- textured-quad rasteriser -------------------------------------

    fn fill_line(&mut self, clip: &Rect, pd: i32, xsize: i32, y: i32, e1: Edge, e2: Edge) {
        if y > clip.max_y || y < clip.min_y {
            return;
        }
        let mut xx1 = e1.x >> FRAC_SHIFT;
        let xx2 = e2.x >> FRAC_SHIFT;
        if xx1 > clip.max_x && xx2 < clip.min_x {
            return;
        }
        let (mut u, mut v) = (e1.u, e1.v);
        let (mut r, mut g, mut b) = (e1.r, e1.g, e1.b);
        let (mut slux, mut slvx) = (0, 0);
        let (mut slr, mut slg, mut slb) = (0, 0, 0);
        if xx1 != xx2 {
            let d = xx2 - xx1;
            slux = (e2.u - e1.u) / d;
            slvx = (e2.v - e1.v) / d;
            slr = (e2.r - e1.r) / d;
            slg = (e2.g - e1.g) / d;
            slb = (e2.b - e1.b) / d;
        }
        if xx1 < clip.min_x {
            let d = clip.min_x - xx1;
            u += slux * d;
            v += slvx * d;
            r += slr * d;
            g += slg * d;
            b += slb * d;
            xx1 = clip.min_x;
        }
        let xend = xx2.min(clip.max_x);
        while xx1 <= xend {
            let shade = self.gouraud.map(|_| (r, g, b));
            self.draw_pixel(
                xx1,
                y,
                pd,
                (v >> FRAC_SHIFT) * xsize + (u >> FRAC_SHIFT),
                shade,
            );
            xx1 += 1;
            u += slux;
            v += slvx;
            r += slr;
            g += slg;
            b += slb;
        }
    }

    /// Walk one trapezoid band between scanlines `y1..y2`, advancing the
    /// two edges `e1`/`e2` by their slopes `s1`/`s2` and filling each
    /// span. Edges are updated in place for the next band.
    #[allow(clippy::too_many_arguments)]
    fn fill_slope(
        &mut self,
        clip: &Rect,
        pd: i32,
        xsize: i32,
        e1: &mut Edge,
        e2: &mut Edge,
        s1: Edge,
        s2: Edge,
        y1: i32,
        y2: i32,
    ) {
        if y1 > clip.max_y {
            return;
        }
        if y2 <= clip.min_y {
            let delta = y2 - y1;
            e1.advance(&s1, delta);
            e2.advance(&s2, delta);
            return;
        }

        let mut yy1 = y1;
        let mut yy2 = y2;
        if yy2 > clip.max_y {
            yy2 = clip.max_y + 1;
        }

        let mut a1 = *e1;
        let mut a2 = *e2;
        let mut m1 = s1;
        let mut m2 = s2;
        if yy1 < clip.min_y {
            let delta = clip.min_y - yy1;
            a1.advance(&m1, delta);
            a2.advance(&m2, delta);
            yy1 = clip.min_y;
        }

        let swapped = if a1.x > a2.x || (a1.x == a2.x && m1.x > m2.x) {
            std::mem::swap(&mut a1, &mut a2);
            std::mem::swap(&mut m1, &mut m2);
            true
        } else {
            false
        };

        while yy1 < yy2 {
            if yy1 >= clip.min_y {
                let mut xx1 = a1.x >> FRAC_SHIFT;
                let xx2 = a2.x >> FRAC_SHIFT;
                let (mut u, mut v) = (a1.u, a1.v);
                let (mut r, mut g, mut b) = (a1.r, a1.g, a1.b);
                let (mut slux, mut slvx) = (0, 0);
                let (mut slr, mut slg, mut slb) = (0, 0, 0);
                if xx1 != xx2 {
                    let d = xx2 - xx1;
                    slux = (a2.u - a1.u) / d;
                    slvx = (a2.v - a1.v) / d;
                    slr = (a2.r - a1.r) / d;
                    slg = (a2.g - a1.g) / d;
                    slb = (a2.b - a1.b) / d;
                }
                if xx1 <= clip.max_x || xx2 >= clip.min_x {
                    if xx1 < clip.min_x {
                        let d = clip.min_x - xx1;
                        u += slux * d;
                        v += slvx * d;
                        r += slr * d;
                        g += slg * d;
                        b += slb * d;
                        xx1 = clip.min_x;
                    }
                    let xend = xx2.min(clip.max_x);
                    while xx1 <= xend {
                        let shade = self.gouraud.map(|_| (r, g, b));
                        self.draw_pixel(
                            xx1,
                            yy1,
                            pd,
                            (v >> FRAC_SHIFT) * xsize + (u >> FRAC_SHIFT),
                            shade,
                        );
                        xx1 += 1;
                        u += slux;
                        v += slvx;
                        r += slr;
                        g += slg;
                        b += slb;
                    }
                }
            }
            a1.advance(&m1, 1);
            a2.advance(&m2, 1);
            yy1 += 1;
        }

        if swapped {
            *e1 = a2;
            *e2 = a1;
        } else {
            *e1 = a1;
            *e2 = a2;
        }
    }

    fn fill_quad(&mut self, clip: &Rect, pd: i32, xsize: i32, q: &[SPoint; 4]) {
        // Duplicate the four corners so edge walks can wrap around. The
        // gouraud colour at each corner (5-bit per channel) is carried in 16.16
        // and interpolated alongside (u, v); without a table it stays zero and
        // is never applied.
        let gr = self.gouraud;
        let mut p = [SPoint::default(); 8];
        for i in 0..4 {
            let (cr, cg, cb) = gr.map_or((0, 0, 0), |t| t[i]);
            let sp = SPoint {
                x: q[i].x << FRAC_SHIFT,
                y: q[i].y,
                u: q[i].u << FRAC_SHIFT,
                v: q[i].v << FRAC_SHIFT,
                r: cr << FRAC_SHIFT,
                g: cg << FRAC_SHIFT,
                b: cb << FRAC_SHIFT,
            };
            p[i] = sp;
            p[i + 4] = sp;
        }

        let mut pmin = 0usize;
        let mut pmax = 0usize;
        for i in 1..4 {
            if p[i].y < p[pmin].y {
                pmin = i;
            }
            if p[i].y > p[pmax].y {
                pmax = i;
            }
        }

        let mut cury = p[pmin].y;
        let mut limy = p[pmax].y;

        // Degenerate: a single horizontal span.
        if cury == limy {
            let mut e1 = Edge::at(&p[0]);
            let mut e2 = e1;
            for pt in p.iter().take(4).skip(1) {
                if pt.x < e1.x {
                    e1 = Edge::at(pt);
                }
                if pt.x > e2.x {
                    e2 = Edge::at(pt);
                }
            }
            self.fill_line(clip, pd, xsize, cury, e1, e2);
            return;
        }

        if cury > clip.max_y || limy <= clip.min_y {
            return;
        }
        if limy > clip.max_y {
            limy = clip.max_y;
        }

        let mut ps1 = pmin + 4;
        let mut ps2 = pmin;

        // Initial edge/slope setup (MAME's `startup:` label, run once
        // before the main band loop).
        while p[ps1 - 1].y == cury {
            ps1 -= 1;
        }
        while p[ps2 + 1].y == cury {
            ps2 += 1;
        }
        let mut e1 = Edge::at(&p[ps1]);
        let mut e2 = Edge::at(&p[ps2]);
        let mut s1 = slope(&p[ps1], &p[ps1 - 1], cury);
        let mut s2 = slope(&p[ps2], &p[ps2 + 1], cury);

        loop {
            let ya = p[ps1 - 1].y;
            let yb = p[ps2 + 1].y;
            if ya == yb {
                self.fill_slope(clip, pd, xsize, &mut e1, &mut e2, s1, s2, cury, ya);
                cury = ya;
                if cury >= limy {
                    break;
                }
                ps1 -= 1;
                ps2 += 1;
                while p[ps1 - 1].y == cury {
                    ps1 -= 1;
                }
                while p[ps2 + 1].y == cury {
                    ps2 += 1;
                }
                e1 = Edge::at(&p[ps1]);
                e2 = Edge::at(&p[ps2]);
                s1 = slope(&p[ps1], &p[ps1 - 1], cury);
                s2 = slope(&p[ps2], &p[ps2 + 1], cury);
            } else if ya < yb {
                self.fill_slope(clip, pd, xsize, &mut e1, &mut e2, s1, s2, cury, ya);
                cury = ya;
                if cury >= limy {
                    break;
                }
                ps1 -= 1;
                while p[ps1 - 1].y == cury {
                    ps1 -= 1;
                }
                e1 = Edge::at(&p[ps1]);
                s1 = slope(&p[ps1], &p[ps1 - 1], cury);
            } else {
                self.fill_slope(clip, pd, xsize, &mut e1, &mut e2, s1, s2, cury, yb);
                cury = yb;
                if cury >= limy {
                    break;
                }
                ps2 += 1;
                while p[ps2 + 1].y == cury {
                    ps2 += 1;
                }
                e2 = Edge::at(&p[ps2]);
                s2 = slope(&p[ps2], &p[ps2 + 1], cury);
            }
        }

        if cury == limy {
            self.fill_line(clip, pd, xsize, cury, e1, e2);
        }
    }
}

/// Per-scanline forward-difference slope from `from` toward `other`,
/// matching MAME: `delta = cury - other.y`, `slope = (from - other) / delta`.
fn slope(from: &SPoint, other: &SPoint, cury: i32) -> Edge {
    let delta = cury - other.y;
    if delta == 0 {
        return Edge::default();
    }
    Edge {
        x: (from.x - other.x) / delta,
        u: (from.u - other.u) / delta,
        v: (from.v - other.v) / delta,
        r: (from.r - other.r) / delta,
        g: (from.g - other.g) / delta,
        b: (from.b - other.b) / delta,
    }
}

/// Bilinearly interpolate the four corner gouraud colours (A=TL, B=TR, C=BR,
/// D=BL; 5-bit channels) at fractional position `(fx/w, fy/h)`, returning each
/// channel in 16.16. Used by the 1:1 normal-sprite path; the quad rasteriser
/// interpolates per-edge instead.
fn gouraud_bilerp(g: &[(i32, i32, i32); 4], fx: i32, fy: i32, w: i32, h: i32) -> (i32, i32, i32) {
    let lerp = |a: i32, b: i32, n: i32, d: i32| if d == 0 { a } else { a + (b - a) * n / d };
    let chan = |a: i32, b: i32, c: i32, d: i32| {
        let top = lerp(a, b, fx, w); // A → B along the top edge
        let bot = lerp(d, c, fx, w); // D → C along the bottom edge
        lerp(top, bot, fy, h) << FRAC_SHIFT
    };
    (
        chan(g[0].0, g[1].0, g[2].0, g[3].0),
        chan(g[0].1, g[1].1, g[2].1, g[3].1),
        chan(g[0].2, g[1].2, g[2].2, g[3].2),
    )
}

/// Apply a gouraud correction to one RGB555 channel: the 16.16 `correction`
/// is reduced to its 5-bit value and added as `corr - 16`, then clamped to
/// 0..31 (VDP1 manual; mirrors MAME's `_shading`).
#[inline]
fn shade_channel(color: i32, correction: i32) -> i32 {
    let corr = (correction >> FRAC_SHIFT) & 0x1F;
    (color + corr - 16).clamp(0, 0x1F)
}

/// Apply gouraud to a whole RGB555 pixel, preserving the MSB.
fn apply_gouraud(pix: u16, gr: i32, gg: i32, gb: i32) -> u16 {
    let msb = pix & 0x8000;
    let r = shade_channel((pix & 0x1F) as i32, gr) as u16;
    let g = shade_channel(((pix >> 5) & 0x1F) as i32, gg) as u16;
    let b = shade_channel(((pix >> 10) & 0x1F) as i32, gb) as u16;
    msb | (b << 10) | (g << 5) | r
}

/// Average two RGB555 colours (the half-transparent calc mode at the
/// hardware's 50% level). REVIEW(magic): the real blend weight is the
/// SCSP-style 0x80 alpha; a true 50% average is faithful for that level
/// and avoids importing MAME's `alpha_blend_r16` table.
fn alpha_blend_rgb555(d: u16, s: u16) -> u16 {
    let (dr, dg, db) = (d & 0x1F, (d >> 5) & 0x1F, (d >> 10) & 0x1F);
    let (sr, sg, sb) = (s & 0x1F, (s >> 5) & 0x1F, (s >> 10) & 0x1F);
    let r = (dr + sr) >> 1;
    let g = (dg + sg) >> 1;
    let b = (db + sb) >> 1;
    (b << 10) | (g << 5) | r
}
