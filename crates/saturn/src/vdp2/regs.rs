//! VDP2 register bank — 512 bytes at `0x05F8_0000..=0x05F8_01FF`.
//!
//! There are ~50 named registers, almost all 16-bit, controlling
//! display mode, background enables, character/bitmap modes, plane
//! sizes, map offsets, scroll values, priorities, and special
//! effects. Most are register storage with no behavioural side
//! effect — the renderer reads them each frame to decide what to
//! draw. Only the master `TVMD.DISP` bit (15) is treated specially
//! in M3: when clear, the renderer must emit a blank frame and skip
//! VBlank-IN generation.
//!
//! Implementation strategy: the underlying storage is a flat 512-byte
//! buffer, with named accessors for the registers the renderer cares
//! about. Adding accessors as the renderer grows is cheap; per-field
//! decomposition with 50+ struct fields would just be ceremony.
//!
//! Register map (selected; see *VDP2 User's Manual* for the full set):
//!
//! ```text
//!   0x000  TVMD     TV Mode                 (15=DISP, master enable)
//!   0x002  EXTEN    External Signal Enable
//!   0x004  TVSTAT   TV Status               (read-only: HBLANK, VBLANK)
//!   0x006  VRSIZE   VRAM Size / Version     (typically reads 0x0000)
//!   0x008  HCNT     H counter               (read-only)
//!   0x00A  VCNT     V counter               (read-only)
//!   0x00E  RAMCTL   RAM Control             (VRAM bank + CRAM mode)
//!   0x020  BGON     Background On           (bits 0..3 = NBG0..3 enable)
//!   0x028  CHCTLA   Character Control A     (NBG0/1 mode + bpp)
//!   0x02A  CHCTLB   Character Control B     (NBG2/3 + RBG0)
//!   0x02C  BMPNA    Bitmap Palette NBG0/1
//!   0x03C  PLSZ     Plane Size              (per-background plane size)
//!   0x03E  MPOFN    Map Offset NBG          (NBG0..3 high addr nibbles)
//!   0x040..0x05E    Map address registers (per-plane per-bg)
//!   0x070..0x07E    Scroll register integers for NBG0..3
//!   0x080..0x09E    Scroll register fractions
//!   0x0F0..0x0FE    Per-background priority numbers
//! ```

const REG_BYTES: usize = 0x200;

#[derive(Clone, Debug)]
pub struct Vdp2Regs {
    raw: [u8; REG_BYTES],
}

impl Default for Vdp2Regs {
    fn default() -> Self {
        Self::new()
    }
}

impl Vdp2Regs {
    pub fn new() -> Self {
        Self {
            raw: [0; REG_BYTES],
        }
    }

    /// Whole register window — exposed so the renderer can read what
    /// it needs without going through 50 named accessors.
    pub fn raw(&self) -> &[u8; REG_BYTES] {
        &self.raw
    }

    fn idx(&self, offset: u32) -> usize {
        (offset as usize) % REG_BYTES
    }

    pub fn read8(&self, offset: u32) -> u8 {
        self.raw[self.idx(offset)]
    }
    pub fn read16(&self, offset: u32) -> u16 {
        u16::from_be_bytes([self.read8(offset), self.read8(offset.wrapping_add(1))])
    }
    pub fn read32(&self, offset: u32) -> u32 {
        u32::from_be_bytes([
            self.read8(offset),
            self.read8(offset.wrapping_add(1)),
            self.read8(offset.wrapping_add(2)),
            self.read8(offset.wrapping_add(3)),
        ])
    }
    pub fn write8(&mut self, offset: u32, val: u8) {
        let i = self.idx(offset);
        self.raw[i] = val;
    }
    pub fn write16(&mut self, offset: u32, val: u16) {
        let i = self.idx(offset);
        let b = val.to_be_bytes();
        self.raw[i] = b[0];
        self.raw[(i + 1) % REG_BYTES] = b[1];
    }
    pub fn write32(&mut self, offset: u32, val: u32) {
        let i = self.idx(offset);
        let b = val.to_be_bytes();
        self.raw[i] = b[0];
        self.raw[(i + 1) % REG_BYTES] = b[1];
        self.raw[(i + 2) % REG_BYTES] = b[2];
        self.raw[(i + 3) % REG_BYTES] = b[3];
    }

    // ---- Named accessors for renderer-critical registers ----

    pub fn tvmd(&self) -> u16 {
        self.read16(0x000)
    }
    /// Master display enable — DISP bit of TVMD. When clear the
    /// renderer must produce a blank frame.
    pub fn display_enabled(&self) -> bool {
        self.tvmd() & 0x8000 != 0
    }
    /// Horizontal resolution code — TVMD bits 2..0.
    pub fn h_resolution(&self) -> u8 {
        (self.tvmd() & 0b111) as u8
    }
    /// Vertical resolution code — TVMD bits 6..4.
    pub fn v_resolution(&self) -> u8 {
        ((self.tvmd() >> 4) & 0b11) as u8
    }
    pub fn ramctl(&self) -> u16 {
        self.read16(0x00E)
    }
    /// CRAM-mode bits 13..12 of RAMCTL: 0 = mode 0 (1024×16 RGB555),
    /// 1 = mode 1 (2048×16), 2 = mode 2 (1024×32 RGB888).
    pub fn cram_mode(&self) -> u8 {
        ((self.ramctl() >> 12) & 0b11) as u8
    }
    pub fn bgon(&self) -> u16 {
        self.read16(0x020)
    }
    pub fn nbg0_enabled(&self) -> bool {
        self.bgon() & 1 != 0
    }
    pub fn nbg1_enabled(&self) -> bool {
        self.bgon() & 2 != 0
    }
    pub fn nbg2_enabled(&self) -> bool {
        self.bgon() & 4 != 0
    }
    pub fn nbg3_enabled(&self) -> bool {
        self.bgon() & 8 != 0
    }
    pub fn chctla(&self) -> u16 {
        self.read16(0x028)
    }
    pub fn chctlb(&self) -> u16 {
        self.read16(0x02A)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tvmd_round_trip_and_display_bit_decode() {
        let mut r = Vdp2Regs::new();
        assert!(!r.display_enabled());
        r.write16(0x000, 0x8000); // DISP
        assert!(r.display_enabled());
        r.write16(0x000, 0x0000);
        assert!(!r.display_enabled());
    }

    #[test]
    fn resolution_bits_decode() {
        let mut r = Vdp2Regs::new();
        r.write16(0x000, 0x0001 | (0b10 << 4)); // hres=1, vres=2
        assert_eq!(r.h_resolution(), 1);
        assert_eq!(r.v_resolution(), 2);
    }

    #[test]
    fn bgon_per_layer_enables_decode() {
        let mut r = Vdp2Regs::new();
        r.write16(0x020, 0b0101);
        assert!(r.nbg0_enabled());
        assert!(!r.nbg1_enabled());
        assert!(r.nbg2_enabled());
        assert!(!r.nbg3_enabled());
    }

    #[test]
    fn ramctl_cram_mode_decode() {
        let mut r = Vdp2Regs::new();
        r.write16(0x00E, 2 << 12);
        assert_eq!(r.cram_mode(), 2);
    }

    #[test]
    fn write32_then_read16_halves() {
        let mut r = Vdp2Regs::new();
        r.write32(0x028, 0xAABB_CCDD);
        assert_eq!(r.read16(0x028), 0xAABB);
        assert_eq!(r.read16(0x02A), 0xCCDD);
        assert_eq!(r.chctla(), 0xAABB);
        assert_eq!(r.chctlb(), 0xCCDD);
    }

    #[test]
    fn offsets_past_window_mirror() {
        let mut r = Vdp2Regs::new();
        r.write16(0x004, 0x1234);
        assert_eq!(r.read16(0x004 + 0x200), 0x1234);
    }
}
