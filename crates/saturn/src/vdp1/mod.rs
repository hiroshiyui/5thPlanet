//! Video Display Processor 1 (VDP1) — Saturn's sprite/polygon engine.
//!
//! VDP1 walks a command table in VRAM and rasterises textured sprites,
//! polygons and lines into a frame buffer that VDP2 then composites as
//! the sprite layer. The list walker and pixel pipeline live in
//! [`plotter`]; [`command`] decodes one command-table entry. A `PTMR`
//! write runs the list synchronously, latching the draw-end status and
//! flagging the SCU sprite-draw-end interrupt for the aggregate to
//! forward.
//!
//! Memory map (post-`classify` physical addresses):
//!
//! ```text
//!   0x05C0_0000..0x05C7_FFFF   VRAM         (512 KiB — command table + chars)
//!   0x05C8_0000..0x05CB_FFFF   Frame buffer (256 KiB — 512×256 RGB555)
//!   0x05D0_0000..0x05D0_0017   Registers    (11 × 16-bit)
//! ```
//!
//! Double buffering: the plotter draws into the *draw* buffer ([`Vdp1::fb`])
//! while VDP2 composites the *display* buffer ([`Vdp1::display_fb`]); `FBCR`
//! swaps them at the frame boundary (automatic 1-cycle mode or a manual
//! change). A plot kicked through `PTMR` takes a modelled number of cycles
//! proportional to its commands + dots, so `EDSR.CEF` reads busy and the SCU
//! sprite-draw-end interrupt fires only once that duration elapses.

pub mod command;
pub mod framebuffer;
pub mod plotter;
pub mod regs;
pub mod timing;
pub mod vram;

pub use command::Command;
pub use framebuffer::Framebuffer;
pub use plotter::Plotter;
pub use regs::Vdp1Regs;
pub use vram::Vram;

pub const VRAM_BASE: u32 = 0x05C0_0000;
pub const VRAM_END: u32 = 0x05C7_FFFF;
pub const FB_BASE: u32 = 0x05C8_0000;
pub const FB_END: u32 = 0x05CB_FFFF;
pub const REGS_BASE: u32 = 0x05D0_0000;
pub const REGS_END: u32 = 0x05D0_0017;

#[derive(Clone, Debug)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Vdp1 {
    pub vram: Vram,
    /// Draw buffer — the plotter renders here.
    pub fb: Framebuffer,
    /// Display buffer — VDP2 composites this as the sprite layer.
    display: Framebuffer,
    pub regs: Vdp1Regs,
    /// Set when a plot finishes; the Saturn aggregate drains this and
    /// raises the SCU sprite-draw-end interrupt (drain-at-aggregate).
    draw_end_pending: bool,
    /// Current global cycle, refreshed by the bus on each VDP1 access so a
    /// `PTMR`-kicked plot can schedule its completion.
    now: u64,
    /// When a timed plot is in flight, the global cycle it completes at.
    busy_until: Option<u64>,
    /// Cycle of the last SH-2 access that incurred a draw-slowdown stall — the
    /// cap reference for [`Self::draw_slowdown`] (Mednafen `LastRWTS`).
    last_rw_ts: u64,
    /// Opt-in gate for [`Self::draw_slowdown`]. In the reference the RW
    /// slowdown is a **per-game hack** (Mednafen `db.cpp`
    /// `HORRIBLEHACK_VDP1RWDRAWSLOWDOWN` — applied to e.g. Virtua Fighter 1,
    /// but *not* VF2 nor the BIOS), so the oracle's default is off and ours
    /// must match or the master's draw-loop timing diverges from the
    /// trace-diff reference. Host configuration, not machine state.
    #[serde(skip)]
    rw_slowdown: bool,
    /// Debug-only: lifetime
    /// `(total VDP1 accesses, accesses-while-drawing, stall hits, stall cycles)`.
    #[serde(skip)]
    dbg_slowdown: (u32, u32, u32, u64),
    /// Debug-only: lifetime `(begin_plot calls, summed duration, max duration)`.
    #[serde(skip)]
    dbg_plots: (u32, u64, u64, u32, u32),
    /// Persistent draw-cycle state (clip/local registers + refresh-overhead
    /// residue) for the duration model — see [`timing::DrawTiming`].
    timing: timing::DrawTiming,
    /// Debug-only: per-frame plot accumulator `(plots, max command_count,
    /// max pixels, max duration)` since the last [`Self::dbg_take_frame`].
    /// The audio-CD BGM probe drains this once per `run_frame` to get a
    /// per-frame VDP1 command-count/duration series (A6 + M12 #6: the
    /// command-list and draw-span comparison vs mednaref `SS_VDP1DRAW`).
    #[serde(skip)]
    dbg_frame: (u32, u32, u32, u64),
}

impl Default for Vdp1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Vdp1 {
    pub fn new() -> Self {
        Self {
            vram: Vram::new(),
            fb: Framebuffer::new(),
            display: Framebuffer::new(),
            regs: Vdp1Regs::new(),
            draw_end_pending: false,
            now: 0,
            busy_until: None,
            last_rw_ts: 0,
            rw_slowdown: false,
            timing: timing::DrawTiming::new(),
            dbg_slowdown: (0, 0, 0, 0),
            dbg_plots: (0, 0, 0, 0, 0),
            dbg_frame: (0, 0, 0, 0),
        }
    }

    /// The extra SH-2 stall cycles for an access to VDP1 VRAM/FB while the
    /// plotter is **drawing** — the SH-2↔VDP1 VRAM bus contention. A faithful
    /// port of Mednafen `vdp1.cpp` `Write_/Read_CheckDrawSlowdown`
    /// (`HORRIBLEHACK_VDP1RWDRAWSLOWDOWN`): while drawing, an access costs up to
    /// `count` cycles, *capped by the gap since the last slowed access* so the
    /// SH-2 is throttled to the draw rate, not stalled unboundedly. `count` is
    /// 25 cy for a VRAM/FB write (22 for a register write), 41 for a VRAM read
    /// (44 for an FB read); register reads aren't slowed. Without this, ours'
    /// 0-wait VDP1 VRAM lets graphics-drawing code outrun the reference (M12 #6).
    pub fn draw_slowdown(&mut self, addr: u32, now: u64, write: bool) -> u32 {
        self.dbg_slowdown.0 += 1;
        if !self.rw_slowdown || self.busy_until.is_none() || now <= self.last_rw_ts {
            return 0;
        }
        self.dbg_slowdown.1 += 1;
        let a = addr & 0x1F_FFFF; // offset within the 2 MB VDP1 window
        let count: u32 = if write {
            if a & 0x10_0000 != 0 { 22 } else { 25 } // regs : VRAM/FB
        } else if a & 0x10_0000 != 0 {
            return 0; // register reads aren't slowed
        } else if a & 0x8_0000 != 0 {
            44 // FB read
        } else {
            41 // VRAM read
        };
        let stall = count.min((now - self.last_rw_ts) as u32);
        self.last_rw_ts = now;
        if stall != 0 {
            self.dbg_slowdown.2 += 1;
            self.dbg_slowdown.3 += stall as u64;
        }
        stall
    }

    /// Enable/disable the per-game SH-2↔VDP1 RW draw-slowdown (see the
    /// `rw_slowdown` field — off by default, matching the oracle's
    /// per-game-hack gating).
    pub fn set_rw_slowdown(&mut self, on: bool) {
        self.rw_slowdown = on;
    }

    /// Debug-only: lifetime
    /// `(total VDP1 accesses, accesses-while-drawing, stall hits, stall cycles)`
    /// accumulated by [`Self::draw_slowdown`]. Used by the BGM trigger-timing
    /// probe (M12 #6) to confirm the slowdown actually fires while the master
    /// copies the boot animation into VDP1 VRAM.
    pub fn dbg_slowdown(&self) -> (u32, u32, u32, u64) {
        self.dbg_slowdown
    }

    /// Debug-only: lifetime `(begin_plot calls, summed duration, max duration)`.
    pub fn dbg_plots(&self) -> (u32, u64, u64, u32, u32) {
        self.dbg_plots
    }

    /// The buffer VDP2 reads as the sprite layer (the front/display buffer).
    pub fn display_fb(&self) -> &Framebuffer {
        &self.display
    }

    /// Refresh the cycle hint and complete any in-flight plot that is now due.
    /// The bus calls this on every VDP1 access (mirroring SMPC's INTBACK
    /// settle), so CPU polling of `EDSR` advances the draw clock.
    pub fn tick(&mut self, now: u64) {
        self.now = now;
        self.settle(now);
    }

    /// Complete a timed plot once `now` reaches its scheduled end: latch
    /// `EDSR.CEF` and flag the SCU sprite-draw-end interrupt.
    pub fn settle(&mut self, now: u64) {
        if let Some(end) = self.busy_until
            && now >= end
        {
            self.busy_until = None;
            self.regs.cef_set();
            self.draw_end_pending = true;
        }
    }

    /// Whether a plot is still in progress (draw-end not yet latched).
    pub fn is_drawing(&self) -> bool {
        self.busy_until.is_some()
    }

    /// The global cycle an in-flight plot completes at (latches `EDSR.CEF` and
    /// raises the SCU sprite-draw-end interrupt), or `None` when idle. The
    /// Saturn aggregate feeds this into the run-loop's next-event edge so the
    /// draw-end lands at its exact cycle rather than the next batch boundary
    /// (event-driven scheduling, M13 A1).
    pub fn draw_end_cycle(&self) -> Option<u64> {
        self.busy_until
    }

    /// `FBCR`/`PTMR`-driven frame-buffer change, called at the VBlank boundary
    /// with the current global cycle `now`.
    ///
    /// Two coupled behaviours, mirroring MAME `saturn_v.cpp`'s VBlank handler:
    ///
    /// * **Buffer swap** — automatic 1-cycle mode (FCM=0) swaps every frame;
    ///   manual mode (FCM=1) swaps only when the one-shot change trigger FCT is
    ///   set. A swap marks the frame buffers "changed".
    /// * **Automatic draw** (`PTMR` PTM bits = `0b10`) — VDP1 re-renders the
    ///   whole command list into the (freshly swapped-in) draw buffer *every
    ///   frame the buffers change*, with no new `PTMR` write. This is what
    ///   animates the BIOS splash: drawing once per frame into the back buffer
    ///   keeps both buffers populated, so the displayed sprite layer holds
    ///   steady instead of strobing present/absent every other frame. (The
    ///   one-shot PTM=`0b01` "draw by request" mode plots on the `PTMR` write
    ///   instead — see [`Self::after_reg_write`].)
    pub fn frame_change(&mut self, now: u64) {
        self.now = now;
        let fbcr = self.regs.read16(0x02);
        let manual = fbcr & 0x02 != 0; // FCM
        let trigger = fbcr & 0x01 != 0; // FCT
        let changed = !manual || trigger;
        if changed {
            core::mem::swap(&mut self.fb, &mut self.display);
            self.regs.write16(0x02, fbcr & !0x01); // clear the one-shot FCT
        }
        // Automatic-draw mode: redraw the list into the back buffer each time
        // the buffers change (MAME gates `vdp1_process_list` on the swap).
        if changed && self.regs.ptmr() & 0x03 == 0x02 {
            self.begin_plot();
        }
    }

    /// Pop the pending draw-end notification. The aggregate calls this
    /// each drain and forwards it to `Scu::raise(SpriteDrawEnd)`.
    pub fn take_draw_end(&mut self) -> bool {
        core::mem::take(&mut self.draw_end_pending)
    }

    /// True iff `addr` lies in any VDP1-owned address window. Used by
    /// the bus dispatch to decide whether the access routes here.
    #[inline]
    pub fn owns(addr: u32) -> bool {
        matches!(
            addr,
            VRAM_BASE..=VRAM_END | FB_BASE..=FB_END | REGS_BASE..=REGS_END
        )
    }

    pub fn read8(&self, addr: u32) -> u8 {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.read8(addr - VRAM_BASE),
            FB_BASE..=FB_END => self.fb.read8(addr - FB_BASE),
            REGS_BASE..=REGS_END => self.regs.read8(addr - REGS_BASE),
            _ => 0,
        }
    }
    pub fn read16(&self, addr: u32) -> u16 {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.read16(addr - VRAM_BASE),
            FB_BASE..=FB_END => self.fb.read16(addr - FB_BASE),
            REGS_BASE..=REGS_END => self.regs.read16(addr - REGS_BASE),
            _ => 0,
        }
    }
    pub fn read32(&self, addr: u32) -> u32 {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.read32(addr - VRAM_BASE),
            FB_BASE..=FB_END => self.fb.read32(addr - FB_BASE),
            REGS_BASE..=REGS_END => self.regs.read32(addr - REGS_BASE),
            _ => 0,
        }
    }
    pub fn write8(&mut self, addr: u32, val: u8) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write8(addr - VRAM_BASE, val),
            FB_BASE..=FB_END => self.fb.write8(addr - FB_BASE, val),
            REGS_BASE..=REGS_END => {
                let off = addr - REGS_BASE;
                self.regs.write8(off, val);
                self.after_reg_write(off & 0xFE);
            }
            _ => {}
        }
    }
    pub fn write16(&mut self, addr: u32, val: u16) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write16(addr - VRAM_BASE, val),
            FB_BASE..=FB_END => self.fb.write16(addr - FB_BASE, val),
            REGS_BASE..=REGS_END => {
                let off = addr - REGS_BASE;
                self.regs.write16(off, val);
                self.after_reg_write(off & 0xFF);
            }
            _ => {}
        }
    }
    pub fn write32(&mut self, addr: u32, val: u32) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write32(addr - VRAM_BASE, val),
            FB_BASE..=FB_END => self.fb.write32(addr - FB_BASE, val),
            REGS_BASE..=REGS_END => {
                let off = addr - REGS_BASE;
                self.regs.write32(off, val);
                self.after_reg_write(off & 0xFF);
                self.after_reg_write((off & 0xFF) + 2);
            }
            _ => {}
        }
    }

    /// React to a control-register write. `PTMR` (0x04) kicks a one-shot plot
    /// only in "draw by request" mode (PTM = `0b01`); automatic-draw mode
    /// (PTM = `0b10`) draws at the frame change instead (see
    /// [`Self::frame_change`]), not on the register write. `ENDR` (0x0C)
    /// force-terminates the current draw, completing it immediately.
    fn after_reg_write(&mut self, off: u32) {
        match off {
            0x04 if self.regs.ptmr() & 0x03 == 0x01 => self.begin_plot(),
            0x0C => self.finish_draw(),
            _ => {}
        }
    }

    /// Render the command list into the draw buffer and latch the command
    /// addresses, returning the plot result (used to size the draw duration).
    fn render_list(&mut self) -> plotter::PlotResult {
        let prev_copr = self.regs.read16(0x14);
        self.erase_framebuffer();
        self.regs.cef_clear();
        let Vdp1 { vram, fb, regs, timing, .. } = self;
        // TVMR bit 0 selects the 8 bits/pixel frame buffer; FBCR bits 3/2
        // (DIE/DIL) select double-interlace plotting. Latch the pixel layout
        // onto the draw buffer so the swap publishes it to the VDP2 sprite
        // layer (see `Framebuffer::hires8`).
        let bpp8 = regs.read16(0x00) & 0x1 != 0;
        let fbcr = regs.read16(0x02);
        fb.set_hires8(bpp8);
        let mut plotter = Plotter::new(&*vram, fb, bpp8, fbcr & 0x8 != 0, fbcr & 0x4 != 0);
        let result = plotter.process_list(timing);
        regs.set_command_addrs(prev_copr, result.copr);
        result
    }

    /// Run the command list and complete the draw immediately. Convenience
    /// path for direct callers/tests that don't model draw timing.
    pub fn process_list(&mut self) {
        self.render_list();
        self.finish_draw();
    }

    /// Kick a timed plot: render now (the pixels are ready) but defer the
    /// draw-end — `EDSR.CEF` and the SCU interrupt land after a duration
    /// proportional to the plot's commands + dots. The bus refreshes `now`
    /// before this runs (see [`Self::tick`]).
    pub fn begin_plot(&mut self) {
        let r = self.render_list();
        // Duration from the Mednafen-faithful draw-cycle walk (M12 task #6):
        // the boot animation runs ~26k cycles, the BIOS CD-player panel
        // ~240k (≈ half a frame) — matching the oracle's DrawingActive spans
        // so EDSR.CEF and the sprite-draw-end interrupt land when its do.
        let duration = r.cycles;
        self.busy_until = Some(self.now + duration);
        self.dbg_plots.0 += 1;
        self.dbg_plots.1 += duration;
        if duration > self.dbg_plots.2 {
            self.dbg_plots.2 = duration;
        }
        if r.command_count > self.dbg_plots.3 {
            self.dbg_plots.3 = r.command_count;
        }
        if r.pixels > self.dbg_plots.4 {
            self.dbg_plots.4 = r.pixels;
        }
        self.dbg_frame.0 += 1;
        if r.command_count > self.dbg_frame.1 {
            self.dbg_frame.1 = r.command_count;
        }
        if r.pixels > self.dbg_frame.2 {
            self.dbg_frame.2 = r.pixels;
        }
        if duration > self.dbg_frame.3 {
            self.dbg_frame.3 = duration;
        }
    }

    /// Debug-only: drain the per-frame plot accumulator `(plots, max
    /// command_count, max pixels, max duration)` and reset it. The BGM probe
    /// calls this once per `run_frame` to build a per-frame VDP1
    /// command-count/duration series (A6 + M12 #6).
    pub fn dbg_take_frame(&mut self) -> (u32, u32, u32, u64) {
        core::mem::take(&mut self.dbg_frame)
    }

    /// Latch draw-end now (plot finished or force-terminated via ENDR).
    fn finish_draw(&mut self) {
        self.busy_until = None;
        self.regs.cef_set();
        self.draw_end_pending = true;
    }

    /// Clear the erase region (EWLR..EWRR) to the erase colour (EWDR).
    ///
    /// On hardware the erase happens at the frame-buffer swap for the
    /// buffer about to be drawn. We run a single buffer (swap is a later
    /// increment), so the equivalent observable behaviour is to clear
    /// the region right before plotting. EWLR/EWRR carry the rectangle
    /// (X in 8-pixel units, Y in lines); a zero EWRR — the power-on and
    /// test default — leaves the buffer untouched.
    fn erase_framebuffer(&mut self) {
        let ewdr = self.regs.read16(0x06);
        let ewlr = self.regs.read16(0x08);
        let ewrr = self.regs.read16(0x0A);
        let x1 = ((ewlr >> 9) & 0x3F) as i32 * 8;
        let y1 = (ewlr & 0x1FF) as i32;
        let x3 = ((ewrr >> 9) & 0x7F) as i32 * 8;
        let y3 = (ewrr & 0x1FF) as i32 + 1;
        for y in y1..y3.min(framebuffer::FB_HEIGHT) {
            for x in x1..x3.min(framebuffer::FB_STRIDE) {
                self.fb.set_pixel(x, y, ewdr);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_includes_all_three_windows() {
        assert!(Vdp1::owns(VRAM_BASE));
        assert!(Vdp1::owns(VRAM_END));
        assert!(Vdp1::owns(FB_BASE));
        assert!(Vdp1::owns(FB_END));
        assert!(Vdp1::owns(REGS_BASE));
        assert!(Vdp1::owns(REGS_END));
        assert!(!Vdp1::owns(VRAM_BASE - 1));
        assert!(!Vdp1::owns(0x05E0_0000)); // VDP2 VRAM, not VDP1
    }

    #[test]
    fn draw_end_cycle_exposes_the_in_flight_plot_completion() {
        let mut v = Vdp1::new();
        assert_eq!(v.draw_end_cycle(), None, "idle drive has no draw-end");
        // Kick a timed plot at cycle 1000 (empty list → minimal duration); the
        // completion cycle is now visible to the run-loop's next-event edge.
        v.tick(1000);
        v.begin_plot();
        let end = v.draw_end_cycle().expect("a plot is in flight");
        assert!(end >= 1000, "draw-end is at/after the kick cycle");
        // Settling at the completion cycle latches draw-end and clears it.
        v.settle(end);
        assert_eq!(v.draw_end_cycle(), None, "completed plot is no longer pending");
        assert!(v.take_draw_end(), "the sprite-draw-end notification fired");
    }

    #[test]
    fn aggregate_dispatch_routes_each_window() {
        let mut v = Vdp1::new();
        v.write32(VRAM_BASE + 0x100, 0xDEAD_BEEF);
        v.write16(FB_BASE + 0x40, 0x7FFF);
        v.write16(REGS_BASE, 0x0003); // TVMR
        assert_eq!(v.read32(VRAM_BASE + 0x100), 0xDEAD_BEEF);
        assert_eq!(v.read16(FB_BASE + 0x40), 0x7FFF);
        assert_eq!(v.read16(REGS_BASE), 0x0003);
    }

    #[test]
    fn plot_trigger_via_aggregate_sets_draw_end() {
        let mut v = Vdp1::new();
        v.write16(REGS_BASE + 0x04, 0x0001); // PTMR — kicks a timed plot
        assert!(v.is_drawing(), "plot in progress");
        assert_eq!(v.read16(REGS_BASE + 0x10) & 0x0002, 0, "CEF not yet set");
        v.settle(u64::MAX);
        assert_eq!(v.read16(REGS_BASE + 0x10) & 0x0002, 0x0002, "CEF latched");
    }

    #[test]
    fn frame_change_swaps_draw_and_display_buffers() {
        let mut v = Vdp1::new();
        // Draw a marker into the draw buffer; the display buffer is still blank.
        v.fb.set_pixel(3, 3, 0x7FFF);
        assert_eq!(v.display_fb().pixel(3, 3), 0, "display blank before swap");
        // FBCR power-on = 0 → automatic 1-cycle mode → swap every frame.
        v.frame_change(0);
        assert_eq!(
            v.display_fb().pixel(3, 3),
            0x7FFF,
            "swap exposes the drawn buffer"
        );
    }

    #[test]
    fn manual_mode_swaps_only_when_the_change_trigger_is_set() {
        let mut v = Vdp1::new();
        v.fb.set_pixel(1, 1, 0x1234);
        v.write16(REGS_BASE + 0x02, 0x0002); // FBCR: FCM=1 (manual), FCT=0
        v.frame_change(0);
        assert_eq!(v.display_fb().pixel(1, 1), 0, "manual mode: no swap");
        v.write16(REGS_BASE + 0x02, 0x0003); // FCM=1, FCT=1 → change now
        v.frame_change(0);
        assert_eq!(v.display_fb().pixel(1, 1), 0x1234, "FCT triggered the swap");
        assert_eq!(v.read16(REGS_BASE + 0x02) & 0x0001, 0, "FCT is one-shot");
    }

    #[test]
    fn erase_honours_the_ewlr_upper_left_origin() {
        // EWLR carries the upper-left corner: X in 8-pixel units (bits 14-9),
        // Y in lines (bits 8-0). With X1=1 (=8px), the erase rectangle starts
        // at column 8, leaving column 0..7 untouched.
        let mut v = Vdp1::new();
        v.fb.set_pixel(0, 0, 0x7FFF); // outside (x < 8)
        v.fb.set_pixel(8, 0, 0x7FFF); // inside the erase rect
        v.regs.write16(0x06, 0x1234); // EWDR fill colour
        v.regs.write16(0x08, 1 << 9); // EWLR: X1=1 (→8px), Y1=0
        v.regs.write16(0x0A, (4 << 9) | 3); // EWRR: X3=4 (→32px), Y3=3
        v.erase_framebuffer();
        assert_eq!(v.fb.pixel(0, 0), 0x7FFF, "left of the erase X-origin kept");
        assert_eq!(v.fb.pixel(8, 0), 0x1234, "inside the erase rect filled");
        assert_eq!(v.fb.pixel(31, 3), 0x1234, "far corner of erase rect filled");
    }

    #[test]
    fn ptmr_byte_write_kicks_a_one_shot_plot() {
        // A byte write to PTMR (0x04) must still drive the plot trigger: write8
        // re-aligns the offset (& 0xFE) when calling after_reg_write.
        let mut v = Vdp1::new();
        v.write8(REGS_BASE + 0x05, 0x01); // low byte of PTMR = PTM 0b01
        assert!(v.is_drawing(), "byte PTMR write kicked the one-shot plot");
    }

    #[test]
    fn endr_force_terminates_an_in_flight_draw() {
        // ENDR (0x0C) completes the current draw immediately — CEF latches and
        // the drive is no longer busy without waiting out the duration.
        let mut v = Vdp1::new();
        v.write16(REGS_BASE + 0x04, 0x0001); // PTMR one-shot → in flight
        assert!(v.is_drawing());
        assert_eq!(v.read16(REGS_BASE + 0x10) & regs::EDSR_CEF, 0, "CEF not yet set");
        v.write16(REGS_BASE + 0x0C, 0x0001); // ENDR
        assert!(!v.is_drawing(), "ENDR finished the draw");
        assert_eq!(v.read16(REGS_BASE + 0x10) & regs::EDSR_CEF, regs::EDSR_CEF, "CEF latched");
    }

    #[test]
    fn automatic_draw_redraws_at_frame_change_not_on_ptmr_write() {
        // PTM = 0b10 (automatic draw): the BIOS splash mode. A write to PTMR
        // must NOT kick a plot — VDP1 re-renders the list at each frame change
        // instead (with FBCR=0 automatic mode), which keeps both buffers
        // populated so the displayed sprite layer doesn't strobe.
        let mut v = Vdp1::new();
        v.write16(REGS_BASE + 0x04, 0x0002); // PTMR: PTM = automatic
        assert!(
            !v.is_drawing(),
            "automatic-draw mode: the PTMR write itself must not plot"
        );
        v.frame_change(100); // FBCR=0 (auto) → swap + redraw
        assert!(
            v.is_drawing(),
            "automatic-draw mode: the frame change re-renders the list"
        );
    }
}
