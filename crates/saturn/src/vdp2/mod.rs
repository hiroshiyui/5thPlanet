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
pub mod rotation;
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
#[derive(serde::Serialize, serde::Deserialize)]
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

    /// `TVSTAT.VBLANK` (bit 3) reads 1 whenever the display is disabled
    /// (`TVMD.DISP=0`), not only during the raster vblank period — matching the
    /// reference (Mednafen `InternalVB = !DisplayOn`, true at power-on). The
    /// bit *stored* by [`Saturn::update_video_timing`] is the pure raster
    /// vblank (so the VBlank-IN/OUT interrupt edges are unaffected); this
    /// display-off term is OR'd in only on the **bus-facing** read. Without it,
    /// the BIOS — which polls TVSTAT.VBLANK while the display is still off,
    /// before enabling it — would spin forever during boot.
    #[inline]
    fn tvstat_vblank_off(&self) -> bool {
        !self.regs.display_enabled()
    }

    pub fn read8(&self, addr: u32) -> u8 {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.read8(addr - VRAM_BASE),
            CRAM_BASE..=CRAM_END => self.cram.read8(addr - CRAM_BASE),
            REGS_BASE..=REGS_END => {
                let v = self.regs.read8(addr - REGS_BASE);
                // TVSTAT low byte (offset 0x005) holds VBLANK (bit 3).
                if addr - REGS_BASE == 0x005 && self.tvstat_vblank_off() {
                    v | 0x08
                } else {
                    v
                }
            }
            _ => 0,
        }
    }
    pub fn read16(&self, addr: u32) -> u16 {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.read16(addr - VRAM_BASE),
            CRAM_BASE..=CRAM_END => self.cram.read16(addr - CRAM_BASE),
            REGS_BASE..=REGS_END => {
                let v = self.regs.read16(addr - REGS_BASE);
                if addr - REGS_BASE == 0x004 && self.tvstat_vblank_off() {
                    v | 0x0008
                } else {
                    v
                }
            }
            _ => 0,
        }
    }
    pub fn read32(&self, addr: u32) -> u32 {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.read32(addr - VRAM_BASE),
            CRAM_BASE..=CRAM_END => self.cram.read32(addr - CRAM_BASE),
            REGS_BASE..=REGS_END => {
                let v = self.regs.read32(addr - REGS_BASE);
                // A 32-bit read at 0x004 puts TVSTAT in the upper half-word
                // (big-endian), so VBLANK is bit 19.
                if addr - REGS_BASE == 0x004 && self.tvstat_vblank_off() {
                    v | 0x0008_0000
                } else {
                    v
                }
            }
            _ => 0,
        }
    }
    /// Whether a register byte offset is a **read-only status register** the
    /// bus must not write: TVSTAT (0x004–0x005), HCNT (0x008–0x009), VCNT
    /// (0x00A–0x00B). On hardware these reflect the live raster and ignore CPU
    /// writes; `update_video_timing` maintains TVSTAT/VCNT/HCNT via `regs`
    /// directly. Without this guard a game's bulk VDP2 register init writes 0
    /// into TVSTAT, wiping the VBLANK edge-state stored there — which re-fired
    /// the VBlank-IN edge every scheduler batch and flooded the master with
    /// interrupts (VF2 hung its startup interrupt handler on that flood).
    const fn reg_readonly(off: u32) -> bool {
        // `off` is already 0..=0x1FF (REGS_BASE..=REGS_END window).
        matches!(off, 0x004 | 0x005 | 0x008 | 0x009 | 0x00A | 0x00B)
    }
    fn write_reg8(&mut self, off: u32, val: u8) {
        if !Self::reg_readonly(off) {
            self.regs.write8(off, val);
        }
    }
    pub fn write8(&mut self, addr: u32, val: u8) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write8(addr - VRAM_BASE, val),
            CRAM_BASE..=CRAM_END => self.cram.write8(addr - CRAM_BASE, val),
            REGS_BASE..=REGS_END => self.write_reg8(addr - REGS_BASE, val),
            _ => {}
        }
    }
    pub fn write16(&mut self, addr: u32, val: u16) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write16(addr - VRAM_BASE, val),
            CRAM_BASE..=CRAM_END => self.cram.write16(addr - CRAM_BASE, val),
            REGS_BASE..=REGS_END => {
                let off = addr - REGS_BASE;
                let b = val.to_be_bytes();
                self.write_reg8(off, b[0]);
                self.write_reg8(off.wrapping_add(1), b[1]);
            }
            _ => {}
        }
    }
    pub fn write32(&mut self, addr: u32, val: u32) {
        match addr {
            VRAM_BASE..=VRAM_END => self.vram.write32(addr - VRAM_BASE, val),
            CRAM_BASE..=CRAM_END => self.cram.write32(addr - CRAM_BASE, val),
            REGS_BASE..=REGS_END => {
                let off = addr - REGS_BASE;
                for (k, &byte) in val.to_be_bytes().iter().enumerate() {
                    self.write_reg8(off.wrapping_add(k as u32), byte);
                }
            }
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
    fn tvstat_vblank_reads_set_while_display_off() {
        let mut v = Vdp2::new();
        // Display off (TVMD.DISP=0): TVSTAT.VBLANK (bit 3) reads 1 on the bus
        // regardless of the stored raster bit — matches the reference.
        v.write16(REGS_BASE, 0x0000); // DISP = 0
        assert_eq!(v.read16(REGS_BASE + 0x004) & 0x0008, 0x0008);
        assert_eq!(v.read8(REGS_BASE + 0x005) & 0x08, 0x08);
        assert_eq!(v.read32(REGS_BASE + 0x004) & 0x0008_0000, 0x0008_0000);
        // Display on: VBLANK reflects only the stored raster bit (0 here).
        v.write16(REGS_BASE, 0x8000); // DISP = 1
        assert_eq!(v.read16(REGS_BASE + 0x004) & 0x0008, 0x0000);
    }

    #[test]
    fn bus_writes_to_readonly_status_registers_are_ignored() {
        let mut v = Vdp2::new();
        v.write16(REGS_BASE, 0x8000); // display on (so TVSTAT has no override)
        // Seed the status registers the way `update_video_timing` would.
        v.regs.write16(0x004, 0x0008); // TVSTAT.VBLANK
        v.regs.write16(0x008, 0x1234); // HCNT
        v.regs.write16(0x00A, 0x0056); // VCNT
        // A game's bulk register init writing 0 must NOT clobber them.
        v.write16(REGS_BASE + 0x004, 0x0000);
        v.write16(REGS_BASE + 0x008, 0x0000);
        v.write16(REGS_BASE + 0x00A, 0x0000);
        v.write32(REGS_BASE + 0x004, 0x0000_0000); // also covers 0x006 VRSIZE (RW)
        assert_eq!(v.regs.read16(0x004), 0x0008, "TVSTAT preserved");
        assert_eq!(v.regs.read16(0x008), 0x1234, "HCNT preserved");
        assert_eq!(v.regs.read16(0x00A), 0x0056, "VCNT preserved");
        // A writable register (TVMD, 0x000) still takes bus writes.
        v.write16(REGS_BASE, 0x81C3);
        assert_eq!(v.regs.read16(0x000), 0x81C3, "TVMD writable");
    }

    #[test]
    fn aggregate_dispatch_routes_each_window_correctly() {
        let mut v = Vdp2::new();
        v.write32(VRAM_BASE + 0x100, 0xDEAD_BEEF);
        v.write16(CRAM_BASE + 0x10, 0xCAFE);
        v.write16(REGS_BASE, 0x8000); // TVMD.DISP (offset 0x000)
        assert_eq!(v.read32(VRAM_BASE + 0x100), 0xDEAD_BEEF);
        assert_eq!(v.read16(CRAM_BASE + 0x10), 0xCAFE);
        assert!(v.regs.display_enabled());
    }
}
