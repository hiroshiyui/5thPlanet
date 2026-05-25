//! Video Display Processor 1 (VDP1) — Saturn's sprite/polygon engine.
//!
//! VDP1 draws textured/gouraud-shaded quads and lines into a frame
//! buffer that VDP2 then composites as the sprite layer. **M4 stubs
//! the address space only — there is no plotter.** The renderer for
//! sprites and polygons is M5. M4 needs just enough that BIOS init
//! code, which stages a display list and pokes the control registers
//! during power-on, sees defined storage and a draw-end handshake
//! instead of open bus.
//!
//! Memory map (post-`classify` physical addresses):
//!
//! ```text
//!   0x05C0_0000..0x05C7_FFFF   VRAM         (512 KiB — command table + chars)
//!   0x05C8_0000..0x05CB_FFFF   Frame buffer (256 KiB — visible window)
//!   0x05D0_0000..0x05D0_0017   Registers    (11 × 16-bit)
//! ```
//!
//! See [`regs`] for the draw-end handshake — the one behaviour beyond
//! flat storage that this stub models.

pub mod framebuffer;
pub mod regs;
pub mod vram;

pub use framebuffer::Framebuffer;
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
        }
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
            REGS_BASE..=REGS_END => self.regs.write8(addr - REGS_BASE, val),
            _ => {}
        }
    }
    pub fn write16(&mut self, addr: u32, val: u16) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write16(addr - VRAM_BASE, val),
            FB_BASE..=FB_END => self.fb.write16(addr - FB_BASE, val),
            REGS_BASE..=REGS_END => self.regs.write16(addr - REGS_BASE, val),
            _ => {}
        }
    }
    pub fn write32(&mut self, addr: u32, val: u32) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write32(addr - VRAM_BASE, val),
            FB_BASE..=FB_END => self.fb.write32(addr - FB_BASE, val),
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
