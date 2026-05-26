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
//!
//! **Gouraud shading is deferred** (next increment) — CMDPMOD bit 2 is
//! parsed but the per-scanline colour correction is not yet applied.
//!
//! The algorithm mirrors MAME's `saturn_v.cpp` plotter (used as a
//! behaviour reference); the field semantics follow the VDP1 User's
//! Manual, which is authoritative where the two differ.

use super::command::Command;
use super::framebuffer::{FB_HEIGHT, FB_STRIDE, Framebuffer};
use super::vram::Vram;

/// 16.16 fixed-point fraction shift for sub-pixel edge stepping.
const FRAC_SHIFT: i32 = 16;

/// Inclusive clipping rectangle in frame-buffer pixel coordinates.
#[derive(Clone, Copy, Debug)]
struct Rect {
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
}

/// One quad corner with its texture coordinate (16.16 once scaled).
#[derive(Clone, Copy, Debug, Default)]
struct SPoint {
    x: i32,
    y: i32,
    u: i32,
    v: i32,
}

/// A walked edge: framebuffer x plus texture (u, v), all 16.16.
#[derive(Clone, Copy, Debug, Default)]
struct Edge {
    x: i32,
    u: i32,
    v: i32,
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
    /// Number of commands visited (drives draw-end timing later).
    pub command_count: u32,
    /// Final command-table position (>>3 form for COPR).
    pub copr: u16,
}

pub struct Plotter<'a> {
    vram: &'a Vram,
    fb: &'a mut Framebuffer,
    local_x: i32,
    local_y: i32,
    sys_clip: Rect,
    user_clip: Rect,
    cmd: Command,
    mode: PixelMode,
}

impl<'a> Plotter<'a> {
    pub fn new(vram: &'a Vram, fb: &'a mut Framebuffer) -> Self {
        // Default clip is the whole 512×256 frame buffer; command 0x09
        // (system clipping) narrows it, command 0x08 sets the user one.
        let full = Rect {
            min_x: 0,
            max_x: FB_STRIDE - 1,
            min_y: 0,
            max_y: FB_HEIGHT - 1,
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
        }
    }

    /// Walk the command table from offset 0 and draw every command.
    /// Mirrors MAME's `vdp1_process_list`: the position advances per
    /// CMDCTRL jump mode, with a single level of call/return nesting.
    pub fn process_list(&mut self) -> PlotResult {
        const MAX_COMMANDS: u32 = 16383;
        let mut position: u32 = 0;
        let mut count: u32 = 0;
        let mut nest: Option<u32> = None;

        while count < MAX_COMMANDS {
            count += 1;
            self.cmd = Command::read(self.vram, position * 0x20);

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
                    self.draw_normal_sprite(&clip);
                }
                0x1 => {
                    self.cmd.ispoly = false;
                    self.set_pixel_mode();
                    self.draw_scaled_sprite(&clip);
                }
                0x2 | 0x3 => {
                    self.cmd.ispoly = false;
                    self.set_pixel_mode();
                    self.draw_distorted_sprite(&clip);
                }
                0x4 => {
                    self.cmd.ispoly = true;
                    self.set_pixel_mode();
                    self.draw_distorted_sprite(&clip);
                }
                0x5 | 0x7 => {
                    self.cmd.ispoly = true;
                    self.set_pixel_mode();
                    self.draw_poly_line(&clip);
                }
                0x6 => {
                    self.cmd.ispoly = true;
                    self.set_pixel_mode();
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
                    // Local coordinate origin (signed).
                    self.local_x = (self.cmd.xa as i16) as i32;
                    self.local_y = (self.cmd.ya as i16) as i32;
                }
                _ => {
                    // Illegal command: hardware prematurely ends the list.
                    break;
                }
            }
        }

        PlotResult {
            command_count: count,
            copr: ((position * 0x20) >> 3) as u16,
        }
    }

    #[inline]
    fn x2s(&self, v: u16) -> i32 {
        (v as i16) as i32 + self.local_x
    }
    #[inline]
    fn y2s(&self, v: u16) -> i32 {
        (v as i16) as i32 + self.local_y
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
    /// linear texel index `v*xsize + u`.
    fn draw_pixel(&mut self, x: i32, y: i32, pd: i32, off: i32) {
        if self.mode == PixelMode::Poly {
            if (0..FB_STRIDE).contains(&x) && (0..FB_HEIGHT).contains(&y) {
                self.fb.set_pixel(x, y, self.cmd.colr);
            }
            return;
        }
        self.draw_pixel_generic(x, y, pd, off);
    }

    fn texel(&self, byte_addr: i32) -> u8 {
        self.vram.read8((byte_addr as u32) & 0x7_FFFF)
    }

    fn draw_pixel_generic(&mut self, x: i32, y: i32, pd: i32, off: i32) {
        let pmod = self.cmd.pmod;
        let mesh = pmod & 0x100 != 0;
        if mesh && (x ^ y) & 1 == 0 {
            return;
        }
        if !((0..FB_STRIDE).contains(&x) && (0..FB_HEIGHT).contains(&y)) {
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

        // Colour-calc mode: CMDPMOD bits 1-0.
        match pmod & 0x3 {
            0 => self.fb.set_pixel(x, y, pix), // replace
            1 => {
                // shadow: halve the destination if its MSB is set.
                let d = self.fb.pixel(x, y);
                if d & 0x8000 != 0 {
                    self.fb.set_pixel(x, y, ((d & !0x8421) >> 1) | 0x8000);
                }
            }
            2 => {
                // half luminance.
                self.fb.set_pixel(x, y, ((pix & !0x8421) >> 1) | 0x8000);
            }
            3 => {
                // half transparent: blend with destination if its MSB set.
                let d = self.fb.pixel(x, y);
                if d & 0x8000 != 0 {
                    self.fb.set_pixel(x, y, alpha_blend_rgb555(d, pix) | 0x8000);
                } else {
                    self.fb.set_pixel(x, y, pix);
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
        let mut dy = y;
        while dy <= max_y {
            let mut uu = u;
            let mut dx = x;
            while dx <= max_x {
                self.draw_pixel(dx, dy, pd, uu);
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

        let mut x = (self.cmd.xa as i16) as i32;
        let mut y = (self.cmd.ya as i16) as i32;
        let mut screen_w = (self.cmd.xb as i16) as i32;
        let mut screen_h = (self.cmd.yb as i16) as i32;
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
        (v as i16) as i32 + self.local_x
    }
    #[inline]
    fn y2s_i(&self, v: i32) -> i32 {
        (v as i16) as i32 + self.local_y
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
        let (mut slux, mut slvx) = (0, 0);
        if xx1 != xx2 {
            let d = xx2 - xx1;
            slux = (e2.u - e1.u) / d;
            slvx = (e2.v - e1.v) / d;
        }
        if xx1 < clip.min_x {
            let d = clip.min_x - xx1;
            u += slux * d;
            v += slvx * d;
            xx1 = clip.min_x;
        }
        let xend = xx2.min(clip.max_x);
        while xx1 <= xend {
            self.draw_pixel(xx1, y, pd, (v >> FRAC_SHIFT) * xsize + (u >> FRAC_SHIFT));
            xx1 += 1;
            u += slux;
            v += slvx;
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
            e1.x += delta * s1.x;
            e1.u += delta * s1.u;
            e1.v += delta * s1.v;
            e2.x += delta * s2.x;
            e2.u += delta * s2.u;
            e2.v += delta * s2.v;
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
            a1.x += delta * m1.x;
            a1.u += delta * m1.u;
            a1.v += delta * m1.v;
            a2.x += delta * m2.x;
            a2.u += delta * m2.u;
            a2.v += delta * m2.v;
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
                let (mut slux, mut slvx) = (0, 0);
                if xx1 != xx2 {
                    let d = xx2 - xx1;
                    slux = (a2.u - a1.u) / d;
                    slvx = (a2.v - a1.v) / d;
                }
                if xx1 <= clip.max_x || xx2 >= clip.min_x {
                    if xx1 < clip.min_x {
                        let d = clip.min_x - xx1;
                        u += slux * d;
                        v += slvx * d;
                        xx1 = clip.min_x;
                    }
                    let xend = xx2.min(clip.max_x);
                    while xx1 <= xend {
                        self.draw_pixel(
                            xx1,
                            yy1,
                            pd,
                            (v >> FRAC_SHIFT) * xsize + (u >> FRAC_SHIFT),
                        );
                        xx1 += 1;
                        u += slux;
                        v += slvx;
                    }
                }
            }
            a1.x += m1.x;
            a1.u += m1.u;
            a1.v += m1.v;
            a2.x += m2.x;
            a2.u += m2.u;
            a2.v += m2.v;
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
        // Duplicate the four corners so edge walks can wrap around.
        let mut p = [SPoint::default(); 8];
        for i in 0..4 {
            let sp = SPoint {
                x: q[i].x << FRAC_SHIFT,
                y: q[i].y,
                u: q[i].u << FRAC_SHIFT,
                v: q[i].v << FRAC_SHIFT,
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
            let mut e1 = Edge {
                x: p[0].x,
                u: p[0].u,
                v: p[0].v,
            };
            let mut e2 = e1;
            for pt in p.iter().take(4).skip(1) {
                if pt.x < e1.x {
                    e1 = Edge {
                        x: pt.x,
                        u: pt.u,
                        v: pt.v,
                    };
                }
                if pt.x > e2.x {
                    e2 = Edge {
                        x: pt.x,
                        u: pt.u,
                        v: pt.v,
                    };
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
        let mut e1 = Edge {
            x: p[ps1].x,
            u: p[ps1].u,
            v: p[ps1].v,
        };
        let mut e2 = Edge {
            x: p[ps2].x,
            u: p[ps2].u,
            v: p[ps2].v,
        };
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
                e1 = Edge {
                    x: p[ps1].x,
                    u: p[ps1].u,
                    v: p[ps1].v,
                };
                e2 = Edge {
                    x: p[ps2].x,
                    u: p[ps2].u,
                    v: p[ps2].v,
                };
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
                e1 = Edge {
                    x: p[ps1].x,
                    u: p[ps1].u,
                    v: p[ps1].v,
                };
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
                e2 = Edge {
                    x: p[ps2].x,
                    u: p[ps2].u,
                    v: p[ps2].v,
                };
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
        return Edge { x: 0, u: 0, v: 0 };
    }
    Edge {
        x: (from.x - other.x) / delta,
        u: (from.u - other.u) / delta,
        v: (from.v - other.v) / delta,
    }
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
