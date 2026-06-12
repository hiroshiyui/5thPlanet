//! VDP1 draw-cycle accounting — the draw-DURATION model (M12 task #6).
//!
//! A faithful port of Mednafen's VDP1 cycle charges (`vdp1.cpp` `DoDrawing` /
//! `SetupDrawLine`, `vdp1_common.h` `PlotPixel` / `EdgeStepper` / `DrawLine` /
//! `AdjustDrawTiming`, `vdp1_poly.cpp`, `vdp1_sprite.cpp`, `vdp1_line.cpp`),
//! run as a pure *cost walk* alongside our instant rasterisation: no pixels
//! are written, only the cycles the real (and reference) hardware would spend
//! are accumulated. [`Vdp1::begin_plot`](super::Vdp1::begin_plot) uses the
//! total as the plot duration, so `EDSR.CEF` / the sprite-draw-end interrupt
//! land when the oracle's do (boot animation ≈ 26k cycles, the BIOS CD-player
//! panel ≈ 240k ≈ half a frame — measured via mednaref `SS_VDP1DRAW`).
//!
//! The cost model, per command:
//!
//! * **16 cycles** per fetched command — including skip-flagged, end and
//!   illegal commands (`DoDrawing` charges the fetch before looking at the
//!   control word).
//! * Setup: line/polyline +1; gouraud +4 (polygon/sprite) or +2 per line
//!   segment (line commands); a CLUT colour-mode load +16.
//! * Per line span: +8, +4 more when pre-clipping is enabled (`!PCD`), from
//!   `SetupDrawLine`. A fully-clipped span collapses to a point; for
//!   polygons/sprites a clipped non-final span skips its pixel pass entirely.
//! * Per pixel stepped: 1 cycle, or 6 when the plot is read-modify-write
//!   (MSB-on or half-background colour calc). Transparent and clipped pixels
//!   cost the same as drawn ones; a span terminates early once it *leaves*
//!   the clip region after having been inside (the `drawn_ac` rule).
//!   Anti-aliased primitives (polygons/sprites) plot one extra pixel per
//!   minor-axis step.
//! * Each span's pixel cycles are scaled by `AdjustDrawTiming`:
//!   ×(1 + 48/256) in 16 bpp, ×(1 + 24/256) in 8 bpp, through the fractional
//!   accumulator `dta` (Mednafen `DTACounter`).
//!
//! Known approximations (all sub-`VDP1_UpdateTimingGran = 263`-cycle class,
//! Mednafen's own draw-end scheduling granularity, except the first):
//! end-code (`!ECD`) texture truncation is ignored — the walk does not fetch
//! texels, so a textured span that the hardware would cut at the second end
//! code is charged full length; the 263-cycle start credit (`StartDrawing`'s
//! initial `CycleCounter`) is not subtracted.

use super::vram::Vram;

/// Sign-extend the low `bits` of `v`.
#[inline]
fn sxt(v: i32, bits: u32) -> i32 {
    (v << (32 - bits)) >> (32 - bits)
}

/// Mednafen keeps the Bresenham error terms in a `<< (32 - 13)` fixed-point
/// representation so their wrap-around matches the hardware's 13-bit error
/// registers; mirror it exactly (shifted-out bits discard, as in C++ uint32).
#[inline]
fn fx(v: i32) -> u32 {
    (v as u32) << (32 - 13)
}

/// One edge of a polygon/sprite, stepped once per span — a faithful port of
/// Mednafen `EdgeStepper` (gouraud state omitted: it carries no cycle cost).
/// The error terms keep Mednafen's `<< (32 - 13)` fixed-point representation
/// so wrap-around matches the reference bit-for-bit.
#[derive(Clone, Copy, Default)]
struct EdgeStepper {
    d_error: u32,
    d_error_inc: u32,
    d_error_adj: u32,
    d_error_cmp: i32,
    x: u32,
    x_inc: u32,
    x_error: u32,
    x_error_inc: u32,
    x_error_adj: u32,
    x_error_cmp: i32,
    y: u32,
    y_inc: u32,
    y_error: u32,
    y_error_inc: u32,
    y_error_adj: u32,
    y_error_cmp: i32,
}

impl EdgeStepper {
    fn setup(p0: (i32, i32), p1: (i32, i32), dmax: i32) -> Self {
        let dx = sxt(p1.0 - p0.0, 13);
        let dy = sxt(p1.1 - p0.1, 13);
        let abs_dx = dx.abs();
        let abs_dy = dy.abs();
        let max_adxdy = abs_dx.max(abs_dy);
        Self {
            x: p0.0 as u32,
            x_inc: if dx >= 0 { 1u32 } else { 1u32.wrapping_neg() },
            x_error_inc: fx(2 * abs_dx),
            x_error_adj: fx(-(2 * max_adxdy)),
            x_error: fx((max_adxdy - 2 * max_adxdy) - 1),
            x_error_cmp: fx(if dy < 0 { -1 } else { 0 }) as i32,
            y: p0.1 as u32,
            y_inc: if dy >= 0 { 1u32 } else { 1u32.wrapping_neg() },
            y_error_inc: fx(2 * abs_dy),
            y_error_adj: fx(-(2 * max_adxdy)),
            y_error: fx((max_adxdy - 2 * max_adxdy) - 1),
            y_error_cmp: fx(if dx < 0 { -1 } else { 0 }) as i32,
            d_error: fx((dmax - 2 * dmax) - 1),
            d_error_inc: fx(2 * max_adxdy),
            d_error_adj: fx(-(2 * dmax)),
            d_error_cmp: fx(if (if abs_dy > abs_dx { dy } else { dx }) < 0 { -1 } else { 0 })
                as i32,
        }
    }

    #[inline]
    fn vertex(&self) -> (i32, i32) {
        (self.x as i32, self.y as i32)
    }

    #[inline]
    fn step(&mut self) {
        self.d_error = self.d_error.wrapping_add(self.d_error_inc);
        if self.d_error as i32 >= self.d_error_cmp {
            self.d_error = self.d_error.wrapping_add(self.d_error_adj);

            self.x_error = self.x_error.wrapping_add(self.x_error_inc);
            if self.x_error as i32 >= self.x_error_cmp {
                self.x = self.x.wrapping_add(self.x_inc);
                self.x_error = self.x_error.wrapping_add(self.x_error_adj);
            }

            self.y_error = self.y_error.wrapping_add(self.y_error_inc);
            if self.y_error as i32 >= self.y_error_cmp {
                self.y = self.y.wrapping_add(self.y_inc);
                self.y_error = self.y_error.wrapping_add(self.y_error_adj);
            }
        }
    }
}

/// The per-span walk state built by [`DrawTiming::setup_line`] — Mednafen's
/// `line_inner_data`, reduced to the fields that influence cycle count.
struct LineWalk {
    xy: u32,
    term_xy: u32,
    xy_inc: [u32; 2],
    aa_xy_inc: u32,
    error: u32,
    error_inc: u32,
    error_adj: u32,
    error_cmp: i32,
    /// Iteration bound — `max(|dx|, |dy|)` of the span (the masked-equality
    /// terminator fires at or before this by construction; the bound makes
    /// the walk provably finite).
    max_adxdy: i32,
}

/// Draw-cycle accumulator.
///
/// Mirrors the VDP1 drawing state that influences cost: local coordinates and
/// the system/user clip windows are **persistent hardware registers** —
/// command lists routinely set the clip once and rely on it for the rest of
/// the session (the BIOS CD-player panel does exactly that), so this state
/// must survive across plots, like Mednafen's static `SysClipX/Y` /
/// `UserClip*` / `LocalX/Y`. The `dta` fractional accumulator likewise
/// carries `AdjustDrawTiming` residue (Mednafen `DTACounter`). Owned and
/// serialized by [`super::Vdp1`].
#[derive(Clone, Debug, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct DrawTiming {
    local_x: i32,
    local_y: i32,
    sys_clip_x: i32,
    sys_clip_y: i32,
    uclip_x0: i32,
    uclip_y0: i32,
    uclip_x1: i32,
    uclip_y1: i32,
    dta: u32,
    #[serde(skip)]
    bpp8: bool,
}

impl DrawTiming {
    pub fn new() -> Self {
        Self::default()
    }

    /// Latch the frame-buffer depth for the coming walk (Mednafen reads TVMR
    /// live; ours is fixed per plot). Only scales the refresh overhead.
    pub fn set_bpp8(&mut self, bpp8: bool) {
        self.bpp8 = bpp8;
    }

    /// Charge one command at byte offset `addr` in VRAM, returning its draw
    /// cycles (the 16-cycle fetch plus the command body). Updates the clip /
    /// local-coordinate state for commands 0x8–0xB exactly when Mednafen's
    /// `DoDrawing` would (i.e. not for skip-flagged commands).
    pub fn command(&mut self, vram: &Vram, addr: u32) -> u64 {
        let mut w = [0u16; 16];
        for (i, slot) in w.iter_mut().enumerate() {
            *slot = vram.read16(addr.wrapping_add((i as u32) * 2));
        }

        let mut cycles: u64 = 16; // command fetch (DoDrawing VDP1_EAT_CLOCKS(16))
        if w[0] & 0xC000 != 0 {
            return cycles; // end or skip: fetch only
        }
        cycles += match w[0] & 0xF {
            0x0 => self.sprite_cost(&w, SpriteFormat::Normal),
            0x1 => self.sprite_cost(&w, SpriteFormat::Scaled),
            0x2 | 0x3 => self.sprite_cost(&w, SpriteFormat::Distorted),
            0x4 => self.polygon_cost(&w),
            0x5..=0x7 => self.line_cost(&w),
            0x8 | 0xB => {
                self.uclip_x0 = (w[0x6] & 0x1FFF) as i32;
                self.uclip_y0 = (w[0x7] & 0x1FFF) as i32;
                self.uclip_x1 = (w[0xA] & 0x1FFF) as i32;
                self.uclip_y1 = (w[0xB] & 0x1FFF) as i32;
                0
            }
            0x9 => {
                self.sys_clip_x = (w[0xA] & 0x1FFF) as i32;
                self.sys_clip_y = (w[0xB] & 0x1FFF) as i32;
                0
            }
            0xA => {
                self.local_x = sxt((w[0x6] & 0x7FF) as i32, 11);
                self.local_y = sxt((w[0x7] & 0x7FF) as i32, 11);
                0
            }
            _ => 0, // illegal: the list walker ends the draw
        };
        cycles
    }

    /// `AdjustDrawTiming` — scale a span's pixel cycles by the FB/VRAM
    /// refresh overhead through the fractional accumulator.
    fn adjust(&mut self, cycles: u64) -> u64 {
        self.dta += (cycles as u32) * if self.bpp8 { 24 } else { 48 };
        let extra = self.dta >> 8;
        self.dta &= 0xFF;
        cycles + extra as u64
    }

    /// Per-plot cost of one pixel step: 6 when the framebuffer is read back
    /// (MSB-on or half-background colour calc), else 1 (`PlotPixel`).
    fn ppc(mode: u16) -> u64 {
        if mode & 0x8000 != 0 || mode & 0x0001 != 0 { 6 } else { 1 }
    }

    fn polygon_cost(&mut self, w: &[u16; 16]) -> u64 {
        let mode = w[0x2];
        let cost: u64 = if mode & 0x4 != 0 { 4 } else { 0 }; // gouraud table
        let mut p = [(0i32, 0i32); 4];
        for (i, v) in p.iter_mut().enumerate() {
            v.0 = sxt(w[0x6 + (i << 1)] as i32, 13) + self.local_x;
            v.1 = sxt(w[0x7 + (i << 1)] as i32, 13) + self.local_y;
        }
        cost + self.quad_cost(mode, p)
    }

    fn sprite_cost(&mut self, w: &[u16; 16], format: SpriteFormat) -> u64 {
        let mode = w[0x2];
        let cm = (mode >> 3) & 0x7;
        let mut cost: u64 = if mode & 0x4 != 0 { 4 } else { 0 }; // gouraud table
        if cm == 1 {
            cost += 16; // CLUT load
        }
        let cw = (((w[0x5] >> 8) & 0x3F) << 3) as i32;
        let ch = (w[0x5] & 0xFF) as i32;
        let mut p = [(0i32, 0i32); 4];
        match format {
            SpriteFormat::Distorted => {
                for (i, v) in p.iter_mut().enumerate() {
                    v.0 = sxt(w[0x6 + (i << 1)] as i32, 13) + self.local_x;
                    v.1 = sxt(w[0x7 + (i << 1)] as i32, 13) + self.local_y;
                }
            }
            SpriteFormat::Normal => {
                let x0 = sxt(w[0x6] as i32, 13) + self.local_x;
                let y0 = sxt(w[0x7] as i32, 13) + self.local_y;
                p[0] = (x0, y0);
                p[1] = (x0 + (cw.max(1) - 1), y0);
                p[2] = (p[1].0, y0 + (ch.max(1) - 1));
                p[3] = (x0, p[2].1);
            }
            SpriteFormat::Scaled => {
                let zp = (w[0] >> 8) & 0xF;
                let zp_x = sxt(w[0x6] as i32, 13);
                let zp_y = sxt(w[0x7] as i32, 13);
                let disp_w = sxt(w[0x8] as i32, 13);
                let disp_h = sxt(w[0x9] as i32, 13);
                let alt_x = sxt(w[0xA] as i32, 13);
                let alt_y = sxt(w[0xB] as i32, 13);
                for v in p.iter_mut() {
                    *v = (zp_x, zp_y);
                }
                match zp >> 2 {
                    0x0 => {
                        p[2].1 = alt_y;
                        p[3].1 = alt_y;
                    }
                    0x1 => {
                        p[2].1 += disp_h;
                        p[3].1 += disp_h;
                    }
                    0x2 => {
                        p[0].1 -= disp_h >> 1;
                        p[1].1 -= disp_h >> 1;
                        p[2].1 += (disp_h + 1) >> 1;
                        p[3].1 += (disp_h + 1) >> 1;
                    }
                    _ => {
                        p[0].1 -= disp_h;
                        p[1].1 -= disp_h;
                    }
                }
                match zp & 0x3 {
                    0x0 => {
                        p[1].0 = alt_x;
                        p[2].0 = alt_x;
                    }
                    0x1 => {
                        p[1].0 += disp_w;
                        p[2].0 += disp_w;
                    }
                    0x2 => {
                        p[0].0 -= disp_w >> 1;
                        p[1].0 += (disp_w + 1) >> 1;
                        p[2].0 += (disp_w + 1) >> 1;
                        p[3].0 -= disp_w >> 1;
                    }
                    _ => {
                        p[0].0 -= disp_w;
                        p[3].0 -= disp_w;
                    }
                }
                for v in p.iter_mut() {
                    v.0 += self.local_x;
                    v.1 += self.local_y;
                }
            }
        }
        cost + self.quad_cost(mode, p)
    }

    /// Shared polygon/sprite cost: step the two edges over `dmax + 1` spans,
    /// charging each span's setup and (unless pre-clipped away on a non-final
    /// span) its anti-aliased pixel walk (`PolygonResumeBase` /
    /// `SpriteResumeBase`).
    fn quad_cost(&mut self, mode: u16, p: [(i32, i32); 4]) -> u64 {
        let mut dmax = sxt(p[3].0 - p[0].0, 13).abs();
        dmax = dmax.max(sxt(p[3].1 - p[0].1, 13).abs());
        dmax = dmax.max(sxt(p[2].0 - p[1].0, 13).abs());
        dmax = dmax.max(sxt(p[2].1 - p[1].1, 13).abs());
        dmax &= 0xFFF;

        let mut e0 = EdgeStepper::setup(p[0], p[3], dmax);
        let mut e1 = EdgeStepper::setup(p[1], p[2], dmax);
        let ppc = Self::ppc(mode);
        let mut cost: u64 = 0;
        for iter in (0..=dmax).rev() {
            let (clip_cost, clipped, walk) =
                self.setup_line_impl(true, mode, e0.vertex(), e1.vertex());
            cost += clip_cost;
            // A pre-clipped span draws nothing — unless it is the final span,
            // which the hardware always rasterises (as the collapsed point).
            if !clipped || iter == 0 {
                let pixels = self.walk_span(true, mode, &walk, ppc);
                cost += self.adjust(pixels);
            }
            e0.step();
            e1.step();
        }
        cost
    }

    fn line_cost(&mut self, w: &[u16; 16]) -> u64 {
        let mode = w[0x2];
        let num_lines = if w[0] & 0x1 != 0 { 4 } else { 1 };
        let ppc = Self::ppc(mode);
        let mut cost: u64 = 1; // CMD_Line setup
        for iter in 0..num_lines {
            if mode & 0x4 != 0 {
                cost += 2; // per-segment gouraud endpoints
            }
            let p0 = (
                sxt((w[0x6 + ((iter << 1) & 0x7)] & 0x1FFF) as i32, 13) + self.local_x,
                sxt((w[0x7 + ((iter << 1) & 0x7)] & 0x1FFF) as i32, 13) + self.local_y,
            );
            let p1 = (
                sxt((w[0x6 + (((iter << 1) + 2) & 0x7)] & 0x1FFF) as i32, 13) + self.local_x,
                sxt((w[0x7 + (((iter << 1) + 2) & 0x7)] & 0x1FFF) as i32, 13) + self.local_y,
            );
            // Line commands always rasterise the (possibly point-collapsed)
            // span — RESUME_Line ignores SetupDrawLine's clipped return.
            let (clip_cost, _, walk) = self.setup_line_impl(false, mode, p0, p1);
            cost += clip_cost;
            let pixels = self.walk_span(false, mode, &walk, ppc);
            cost += self.adjust(pixels);
        }
        cost
    }

    /// `SetupDrawLine`: charge the span setup (+4 pre-clip, +8 base), apply
    /// the pre-clip point-collapse / horizontal swap, and build the walk
    /// state. `clipped` (the middle return) marks a fully pre-clipped span —
    /// polygons/sprites skip those spans' pixel pass unless final; line
    /// commands rasterise regardless.
    fn setup_line_impl(
        &self,
        aa: bool,
        mode: u16,
        p0in: (i32, i32),
        p1in: (i32, i32),
    ) -> (u64, bool, LineWalk) {
        let pcd = mode & 0x800 != 0;
        let user_clip_en = mode & 0x400 != 0;
        let user_clip_mode = mode & 0x200 != 0;

        let mut p0 = (p0in.0 & 0x1FFF, p0in.1 & 0x1FFF);
        let mut p1 = (p1in.0 & 0x1FFF, p1in.1 & 0x1FFF);
        let mut cost: u64 = 0;
        let mut clipped = false;

        if !pcd {
            cost += 4;
            let swapped = if user_clip_en && !user_clip_mode {
                // Pre-clip against the user window, ignoring the system clip.
                clipped |= (((self.uclip_x1 - p0.0) & (self.uclip_x1 - p1.0))
                    | ((p0.0 - self.uclip_x0) & (p1.0 - self.uclip_x0)))
                    & 0x1000
                    != 0;
                clipped |= (((self.uclip_y1 - p0.1) & (self.uclip_y1 - p1.1))
                    | ((p0.1 - self.uclip_y0) & (p1.1 - self.uclip_y0)))
                    & 0x1000
                    != 0;
                p0.1 == p1.1 && (p0.0 < self.uclip_x0 || p0.0 > self.uclip_x1)
            } else {
                clipped |= (((self.sys_clip_x - p0.0) & (self.sys_clip_x - p1.0))
                    | (p0.0 & p1.0))
                    & 0x1000
                    != 0;
                clipped |= (((self.sys_clip_y - p0.1) & (self.sys_clip_y - p1.1))
                    | (p0.1 & p1.1))
                    & 0x1000
                    != 0;
                p0.1 == p1.1 && p0.0 > self.sys_clip_x
            };
            // The hardware reduces a clipped span to a point.
            if clipped {
                p1 = p0;
            } else if swapped {
                core::mem::swap(&mut p0, &mut p1);
            }
        }
        cost += 8;

        let dx = sxt(p1.0 - p0.0, 13);
        let dy = sxt(p1.1 - p0.1, 13);
        let abs_dx = dx.abs();
        let abs_dy = dy.abs();
        let max_adxdy = abs_dx.max(abs_dy);
        let x_inc: i32 = if dx >= 0 { 1 } else { -1 };
        let y_inc: i32 = if dy >= 0 { 1 } else { -1 };
        let lid_x_inc = (x_inc & 0x7FF) as u32;
        let lid_y_inc = ((y_inc & 0x7FF) as u32) << 16;

        let mut xy = ((p0.0 & 0x7FF) as u32) + (((p0.1 & 0x7FF) as u32) << 16);
        let term_xy = ((p1.0 & 0x7FF) as u32) + (((p1.1 & 0x7FF) as u32) << 16);

        let (mut error, error_inc, error_adj, mut error_cmp, xy_inc);
        if abs_dy > abs_dx {
            error_inc = 2 * abs_dx;
            error_adj = -(2 * abs_dy);
            error = (abs_dy - 2 * abs_dy) - 1;
            error_cmp = 0i32;
            if dy < 0 && !aa {
                error_cmp -= 1;
            }
            error -= error_inc;
            xy = xy.wrapping_add(0x0800_0000u32.wrapping_sub(lid_y_inc)) & 0x07FF_07FF;
            xy_inc = [lid_y_inc, lid_x_inc];
        } else {
            error_inc = 2 * abs_dy;
            error_adj = -(2 * abs_dx);
            error = (abs_dx - 2 * abs_dx) - 1;
            error_cmp = 0i32;
            if dx < 0 && !aa {
                error_cmp -= 1;
            }
            error -= error_inc;
            xy = xy.wrapping_add(0x800u32.wrapping_sub(lid_x_inc)) & 0x07FF_07FF;
            xy_inc = [lid_x_inc, lid_y_inc];
        }
        if aa {
            error += 1;
            error_cmp += 1;
        }

        // Anti-aliasing fill-in pixel offset (sign-arithmetic port).
        let (aa_x_inc, aa_y_inc) = if abs_dy > abs_dx {
            if y_inc < 0 {
                (x_inc >> 31, -(x_inc >> 31))
            } else {
                (-(!x_inc >> 31), !x_inc >> 31)
            }
        } else if x_inc < 0 {
            (-(!y_inc >> 31), -(!y_inc >> 31))
        } else {
            (y_inc >> 31, y_inc >> 31)
        };
        let aa_xy_inc = ((aa_x_inc & 0x7FF) as u32) + (((aa_y_inc & 0x7FF) as u32) << 16);

        let walk = LineWalk {
            xy,
            term_xy,
            xy_inc,
            aa_xy_inc,
            error: fx(error),
            error_inc: fx(error_inc),
            error_adj: fx(error_adj),
            error_cmp: fx(error_cmp) as i32,
            max_adxdy,
        };
        (cost, clipped, walk)
    }

    /// The `DrawLine` inner loop as a pixel-step counter: returns the span's
    /// pixel cycles (`plots × ppc`), honouring the AA fill-in plot and the
    /// `drawn_ac` early exit (the hardware stops a span once it leaves the
    /// clip window after having been inside it).
    fn walk_span(&self, aa: bool, mode: u16, w: &LineWalk, ppc: u64) -> u64 {
        let user_clip_en = mode & 0x400 != 0;
        let user_clip_mode = mode & 0x200 != 0;
        let clipo =
            (((self.sys_clip_y & 0x3FF) as u32) << 16) | ((self.sys_clip_x & 0x3FF) as u32);
        let uclipo0 =
            (((self.uclip_y0 & 0x3FF) as u32) << 16) | ((self.uclip_x0 & 0x3FF) as u32);
        let uclipo1 =
            (((self.uclip_y1 & 0x3FF) as u32) << 16) | ((self.uclip_x1 & 0x3FF) as u32);

        let mut xy = w.xy;
        let mut error = w.error;
        let mut drawn_ac = true;
        let mut plots: u64 = 0;

        // PBODY: charge one plot; `false` = the span terminated early.
        let pbody = |pxy: u32, drawn_ac: &mut bool, plots: &mut u64| -> bool {
            let clipped = if user_clip_en && !user_clip_mode {
                (uclipo1.wrapping_sub(pxy) | pxy.wrapping_sub(uclipo0)) & 0x8000_8000 != 0
            } else {
                clipo.wrapping_sub(pxy) & 0x8000_8000 != 0
            };
            if clipped && !*drawn_ac {
                return false;
            }
            *drawn_ac &= clipped;
            *plots += 1;
            true
        };

        for _ in 0..=w.max_adxdy {
            xy = xy.wrapping_add(w.xy_inc[0]) & 0x07FF_07FF;
            error = error.wrapping_add(w.error_inc);
            if error as i32 >= w.error_cmp {
                error = error.wrapping_add(w.error_adj);
                if aa {
                    let aa_xy = xy.wrapping_add(w.aa_xy_inc) & 0x07FF_07FF;
                    if !pbody(aa_xy, &mut drawn_ac, &mut plots) {
                        return plots * ppc;
                    }
                }
                xy = xy.wrapping_add(w.xy_inc[1]) & 0x07FF_07FF;
            }
            if !pbody(xy, &mut drawn_ac, &mut plots) {
                return plots * ppc;
            }
            if xy == w.term_xy {
                break;
            }
        }
        plots * ppc
    }
}

#[derive(Clone, Copy)]
enum SpriteFormat {
    Normal,
    Scaled,
    Distorted,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write one 16-word command at `pos` (in 0x20-byte slots).
    fn put_cmd(vram: &mut Vram, pos: u32, words: &[u16]) {
        for (i, &w) in words.iter().enumerate() {
            vram.write16(pos * 0x20 + (i as u32) * 2, w);
        }
    }

    fn sys_clip_cmd(x: u16, y: u16) -> [u16; 12] {
        let mut w = [0u16; 12];
        w[0] = 0x0009;
        w[0xA] = x;
        w[0xB] = y;
        w
    }

    /// An axis-aligned polygon A=(x0,y0) B=(x1,y0) C=(x1,y1) D=(x0,y1).
    fn polygon_cmd(pmod: u16, x0: u16, y0: u16, x1: u16, y1: u16) -> [u16; 14] {
        let mut w = [0u16; 14];
        w[0] = 0x0004;
        w[2] = pmod;
        w[0x6] = x0;
        w[0x7] = y0;
        w[0x8] = x1;
        w[0x9] = y0;
        w[0xA] = x1;
        w[0xB] = y1;
        w[0xC] = x0;
        w[0xD] = y1;
        w
    }

    #[test]
    fn adjust_scales_by_48_over_256_in_16bpp_and_24_in_8bpp() {
        let mut t = DrawTiming::new();
        assert_eq!(t.adjust(256), 256 + 48);
        let mut t8 = DrawTiming::new();
        t8.set_bpp8(true);
        assert_eq!(t8.adjust(256), 256 + 24);
        // The fractional residue accumulates across calls (Mednafen
        // DTACounter): 10 cycles ×48 = 480 → +1 with 224 carried.
        let mut t = DrawTiming::new();
        assert_eq!(t.adjust(10), 11);
        assert_eq!(t.adjust(10), 12, "carry pushes the second call over");
    }

    #[test]
    fn per_pixel_cost_is_6_for_rmw_modes() {
        assert_eq!(DrawTiming::ppc(0x0000), 1, "plain replace");
        assert_eq!(DrawTiming::ppc(0x8000), 6, "MSB-on reads the framebuffer");
        assert_eq!(DrawTiming::ppc(0x0001), 6, "half-background colour calc");
        assert_eq!(DrawTiming::ppc(0x0004), 1, "gouraud alone is write-only");
    }

    #[test]
    fn end_and_skip_commands_charge_the_fetch_only() {
        let mut vram = Vram::new();
        put_cmd(&mut vram, 0, &[0x8000]);
        put_cmd(&mut vram, 1, &[0x4004]); // skip-flagged polygon
        let mut t = DrawTiming::new();
        assert_eq!(t.command(&vram, 0), 16);
        assert_eq!(t.command(&vram, 0x20), 16);
    }

    #[test]
    fn skip_flagged_clip_command_does_not_move_the_clip_state() {
        let mut vram = Vram::new();
        let mut clip = sys_clip_cmd(319, 223);
        clip[0] |= 0x4000; // skip flag
        put_cmd(&mut vram, 0, &clip);
        put_cmd(&mut vram, 1, &polygon_cmd(0, 10, 10, 110, 109));
        let mut t = DrawTiming::new();
        t.command(&vram, 0);
        let clipped = t.command(&vram, 0x20);
        // With the clip never opened (still 0×0), every span pre-clips: only
        // the final span rasterises, as a collapsed point.
        let mut t2 = DrawTiming::new();
        let mut vram2 = Vram::new();
        put_cmd(&mut vram2, 0, &sys_clip_cmd(319, 223));
        put_cmd(&mut vram2, 1, &polygon_cmd(0, 10, 10, 110, 109));
        t2.command(&vram2, 0);
        let open = t2.command(&vram2, 0x20);
        assert!(
            clipped < open / 4,
            "pre-clipped polygon ({clipped}) must cost far less than the open one ({open})"
        );
    }

    #[test]
    fn polygon_cost_matches_the_hand_computed_reference_model() {
        // 100×100-px axis-aligned polygon fully inside the clip, 16 bpp,
        // plain replace: dmax = 99 → 100 spans; each span is 101 plots ×1 cy
        // (no AA fill-ins on a horizontal span), AdjustDrawTiming ×(1+48/256)
        // accumulating fractionally; span setup = 4 (pre-clip) + 8.
        let mut vram = Vram::new();
        put_cmd(&mut vram, 0, &sys_clip_cmd(319, 223));
        put_cmd(&mut vram, 1, &polygon_cmd(0, 10, 10, 110, 109));
        let mut t = DrawTiming::new();
        assert_eq!(t.command(&vram, 0), 16);
        // 16 (fetch) + 100×12 (span setup) + 100×101 (plots)
        //   + floor-accumulated 101×100×48/256 = 1893 (refresh overhead)
        assert_eq!(t.command(&vram, 0x20), 16 + 1200 + 10100 + 1893);
    }

    #[test]
    fn msb_on_polygon_costs_6x_per_pixel() {
        let mut vram = Vram::new();
        put_cmd(&mut vram, 0, &sys_clip_cmd(319, 223));
        put_cmd(&mut vram, 1, &polygon_cmd(0x8000, 10, 10, 110, 109));
        let mut t = DrawTiming::new();
        t.command(&vram, 0);
        // Plots 10100×6 = 60600, overhead floor(60600×48/256) = 11362.
        assert_eq!(t.command(&vram, 0x20), 16 + 1200 + 60600 + 11362);
    }

    #[test]
    fn line_command_walks_one_span_without_aa() {
        let mut vram = Vram::new();
        put_cmd(&mut vram, 0, &sys_clip_cmd(319, 223));
        // Line (type 6) from (0,0) to (100,0): 101 plots, setup 1 + 12.
        let mut w = [0u16; 14];
        w[0] = 0x0006;
        w[0x8] = 100; // XB
        put_cmd(&mut vram, 1, &w);
        let mut t = DrawTiming::new();
        t.command(&vram, 0);
        // 16 + 1 (CMD_Line) + 12 (span setup) + 101 + floor(101×48/256)=18
        assert_eq!(t.command(&vram, 0x20), 16 + 1 + 12 + 101 + 18);
    }

    #[test]
    fn user_clip_command_updates_the_user_window() {
        let mut vram = Vram::new();
        let mut w = [0u16; 12];
        w[0] = 0x0008;
        w[0x6] = 5;
        w[0x7] = 6;
        w[0xA] = 50;
        w[0xB] = 60;
        put_cmd(&mut vram, 0, &w);
        let mut t = DrawTiming::new();
        assert_eq!(t.command(&vram, 0), 16);
        assert_eq!(
            (t.uclip_x0, t.uclip_y0, t.uclip_x1, t.uclip_y1),
            (5, 6, 50, 60)
        );
    }
}
