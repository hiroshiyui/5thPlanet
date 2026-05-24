//! Video Display Processor 2 (VDP2) — Saturn's background generator.
//!
//! The chip composites four NBG (normal background) layers, two RBG
//! (rotation background) layers, and the sprite layer from VDP1 into
//! the final video output. M3 implements the *minimum* needed for
//! the BIOS splash: register surface, VRAM, CRAM, and (in task #6) a
//! renderer for one NBG layer in bitmap or 4-cell-tile mode.
//!
//! Memory map (post-`classify` physical addresses):
//!
//! ```text
//!   0x05E0_0000..0x05E7_FFFF   VRAM     (512 KiB, 4 banks)
//!   0x05F0_0000..0x05F0_0FFF   CRAM     (4 KiB)
//!   0x05F8_0000..0x05F8_01FF   Registers (~50 named, mostly 16-bit)
//! ```
//!
//! M3 task #5 (this file) lands the storage; task #6 fills in
//! `render_frame()`.

pub mod cram;
pub mod regs;
pub mod renderer;
pub mod vram;

pub use cram::Cram;
pub use regs::Vdp2Regs;
pub use renderer::{FRAME_HEIGHT, FRAME_WIDTH, FRAMEBUFFER_BYTES, render_frame};
pub use vram::Vram;

pub const VRAM_BASE: u32 = 0x05E0_0000;
pub const VRAM_END: u32 = 0x05E7_FFFF;
pub const CRAM_BASE: u32 = 0x05F0_0000;
pub const CRAM_END: u32 = 0x05F0_0FFF;
pub const REGS_BASE: u32 = 0x05F8_0000;
pub const REGS_END: u32 = 0x05F8_01FF;

#[derive(Clone, Debug)]
pub struct Vdp2 {
    pub regs: Vdp2Regs,
    pub vram: Vram,
    pub cram: Cram,
}

impl Default for Vdp2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Vdp2 {
    pub fn new() -> Self {
        Self {
            regs: Vdp2Regs::new(),
            vram: Vram::new(),
            cram: Cram::new(),
        }
    }

    /// True iff `addr` lies in any VDP2-owned address window. Used by
    /// the bus dispatch to decide whether the access routes here.
    #[inline]
    pub fn owns(addr: u32) -> bool {
        matches!(
            addr,
            VRAM_BASE..=VRAM_END | CRAM_BASE..=CRAM_END | REGS_BASE..=REGS_END
        )
    }

    pub fn read8(&self, addr: u32) -> u8 {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.read8(addr - VRAM_BASE),
            CRAM_BASE..=CRAM_END => self.cram.read8(addr - CRAM_BASE),
            REGS_BASE..=REGS_END => self.regs.read8(addr - REGS_BASE),
            _ => 0,
        }
    }
    pub fn read16(&self, addr: u32) -> u16 {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.read16(addr - VRAM_BASE),
            CRAM_BASE..=CRAM_END => self.cram.read16(addr - CRAM_BASE),
            REGS_BASE..=REGS_END => self.regs.read16(addr - REGS_BASE),
            _ => 0,
        }
    }
    pub fn read32(&self, addr: u32) -> u32 {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.read32(addr - VRAM_BASE),
            CRAM_BASE..=CRAM_END => self.cram.read32(addr - CRAM_BASE),
            REGS_BASE..=REGS_END => self.regs.read32(addr - REGS_BASE),
            _ => 0,
        }
    }
    pub fn write8(&mut self, addr: u32, val: u8) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write8(addr - VRAM_BASE, val),
            CRAM_BASE..=CRAM_END => self.cram.write8(addr - CRAM_BASE, val),
            REGS_BASE..=REGS_END => self.regs.write8(addr - REGS_BASE, val),
            _ => {}
        }
    }
    pub fn write16(&mut self, addr: u32, val: u16) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write16(addr - VRAM_BASE, val),
            CRAM_BASE..=CRAM_END => self.cram.write16(addr - CRAM_BASE, val),
            REGS_BASE..=REGS_END => self.regs.write16(addr - REGS_BASE, val),
            _ => {}
        }
    }
    pub fn write32(&mut self, addr: u32, val: u32) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write32(addr - VRAM_BASE, val),
            CRAM_BASE..=CRAM_END => self.cram.write32(addr - CRAM_BASE, val),
            REGS_BASE..=REGS_END => self.regs.write32(addr - REGS_BASE, val),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_includes_all_three_windows() {
        assert!(Vdp2::owns(VRAM_BASE));
        assert!(Vdp2::owns(VRAM_END));
        assert!(Vdp2::owns(CRAM_BASE));
        assert!(Vdp2::owns(REGS_BASE));
        assert!(!Vdp2::owns(VRAM_BASE - 1));
        assert!(!Vdp2::owns(0x0500_0000)); // A-bus area, not VDP2
    }

    #[test]
    fn aggregate_dispatch_routes_each_window_correctly() {
        let mut v = Vdp2::new();
        v.write32(VRAM_BASE + 0x100, 0xDEAD_BEEF);
        v.write16(CRAM_BASE + 0x10, 0xCAFE);
        v.write16(REGS_BASE + 0x000, 0x8000); // TVMD.DISP
        assert_eq!(v.read32(VRAM_BASE + 0x100), 0xDEAD_BEEF);
        assert_eq!(v.read16(CRAM_BASE + 0x10), 0xCAFE);
        assert!(v.regs.display_enabled());
    }
}
