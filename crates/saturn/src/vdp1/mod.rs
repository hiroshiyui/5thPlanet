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
//! Still to come (M5 task #2): gouraud shading, double-buffer swap, and
//! cycle-accurate draw-end timing.

pub mod command;
pub mod framebuffer;
pub mod plotter;
pub mod regs;
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
pub struct Vdp1 {
    pub vram: Vram,
    pub fb: Framebuffer,
    pub regs: Vdp1Regs,
    /// Set when a plot finishes; the Saturn aggregate drains this and
    /// raises the SCU sprite-draw-end interrupt (drain-at-aggregate).
    draw_end_pending: bool,
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
            regs: Vdp1Regs::new(),
            draw_end_pending: false,
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

    /// React to a control-register write. `PTMR` (0x04) kicks the
    /// plotter when its mode bits are set; `ENDR` (0x0C) force-terminates
    /// the current draw, which (since our plot is synchronous) simply
    /// raises the draw-end flag for a waiting poll.
    fn after_reg_write(&mut self, off: u32) {
        match off {
            0x04 if self.regs.ptmr() & 0x03 != 0 => self.process_list(),
            0x0C => self.regs.cef_set(),
            _ => {}
        }
    }

    /// Run the VDP1 command list now: clear the draw-end flag, walk the
    /// table in VRAM rendering into the frame buffer, then latch COPR and
    /// raise the draw-end flag. The draw is synchronous (timing-exact
    /// draw-end + the SCU sprite-end interrupt are the next increment).
    pub fn process_list(&mut self) {
        let prev_copr = self.regs.read16(0x14);
        self.erase_framebuffer();
        self.regs.cef_clear();
        let Vdp1 { vram, fb, regs, .. } = self;
        let mut plotter = Plotter::new(&*vram, fb);
        let result = plotter.process_list();
        regs.set_command_addrs(prev_copr, result.copr);
        regs.cef_set();
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
    fn aggregate_dispatch_routes_each_window() {
        let mut v = Vdp1::new();
        v.write32(VRAM_BASE + 0x100, 0xDEAD_BEEF);
        v.write16(FB_BASE + 0x40, 0x7FFF);
        v.write16(REGS_BASE + 0x00, 0x0003); // TVMR
        assert_eq!(v.read32(VRAM_BASE + 0x100), 0xDEAD_BEEF);
        assert_eq!(v.read16(FB_BASE + 0x40), 0x7FFF);
        assert_eq!(v.read16(REGS_BASE + 0x00), 0x0003);
    }

    #[test]
    fn plot_trigger_via_aggregate_sets_draw_end() {
        let mut v = Vdp1::new();
        v.write16(REGS_BASE + 0x04, 0x0001); // PTMR
        // EDSR (0x10) CEF bit must be set.
        assert_eq!(v.read16(REGS_BASE + 0x10) & 0x0002, 0x0002);
    }
}
