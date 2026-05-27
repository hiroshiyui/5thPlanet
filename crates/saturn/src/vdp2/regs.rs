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
//!   0x03A  PLSZ     Plane Size              (per-background plane size)
//!   0x03C  MPOFN    Map Offset NBG          (NBG0..3 high 2 addr bits)
//!   0x03E  MPOFR    Map Offset RBG (rotation)
//!   0x040..0x04E    Map address registers (MPABNn/MPCDNn per-bg)
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

    /// NBG0 bitmap enable — CHCTLA bit 1 (`N0BMEN`). 0 = cell/tile
    /// format, 1 = bitmap format. (VDP2 manual, CHCTLA bit layout:
    /// bit0 N0CHSZ, bit1 N0BMEN, bits3..2 N0BMSZ, bits6..4 N0CHCN.)
    pub fn nbg0_bitmap_enabled(&self) -> bool {
        self.chctla() & 0x0002 != 0
    }
    /// NBG0 bitmap size — CHCTLA bits 3..2 (`N0BMSZ`): 0 = 512×256,
    /// 1 = 512×512, 2 = 1024×256, 3 = 1024×512.
    pub fn nbg0_bitmap_size(&self) -> u8 {
        ((self.chctla() >> 2) & 0x3) as u8
    }

    // ---- Generalized per-NBG accessors (n = 0..3) ----
    //
    // The pattern-name-table address of each plane is
    // `((map_offset << 6) | plane_number) × plane_size`. MPOFN supplies the
    // upper 2 bits per background; the per-plane MPABNn / MPCDNn registers
    // supply the lower 6. Register offsets and bit fields follow the VDP2
    // User's Manual (cross-checked against MAME's `saturn_v.cpp`).

    /// Background-enable bit for NBG`n` (BGON bits 0..3).
    pub fn nbg_enabled(&self, n: usize) -> bool {
        self.bgon() & (1 << n) != 0
    }

    /// Priority number for NBG`n` (PRINA: N0 2..0 / N1 10..8;
    /// PRINB: N2 2..0 / N3 10..8). Priority 0 means the layer is not shown.
    pub fn nbg_priority(&self, n: usize) -> u8 {
        let (reg, shift) = match n {
            0 => (0x0F8, 0),
            1 => (0x0F8, 8),
            2 => (0x0FA, 0),
            _ => (0x0FA, 8),
        };
        ((self.read16(reg) >> shift) & 0x7) as u8
    }

    /// Character colour number for NBG`n`: 0=16-colour (4bpp),
    /// 1=256-colour (8bpp), 2=2048-colour, 3=32K-colour RGB, 4=16M RGB.
    /// NBG2/3 only encode bit 0 (16 vs 256 colour).
    pub fn nbg_color_mode(&self, n: usize) -> u8 {
        match n {
            0 => ((self.chctla() >> 4) & 0x7) as u8,
            1 => ((self.chctla() >> 12) & 0x3) as u8,
            2 => ((self.chctlb() >> 1) & 0x1) as u8,
            _ => ((self.chctlb() >> 5) & 0x1) as u8,
        }
    }

    /// Cell size for NBG`n`: 0 = 1×1 cell (8×8 px), 1 = 2×2 cells (16×16 px).
    pub fn nbg_char_size_2x2(&self, n: usize) -> bool {
        let bit = match n {
            0 => self.chctla() & 0x0001,
            1 => self.chctla() & 0x0100,
            2 => self.chctlb() & 0x0001,
            _ => self.chctlb() & 0x0010,
        };
        bit != 0
    }

    /// Bitmap-format enable for NBG0/1 (NBG2/3 are cell-only → false).
    pub fn nbg_bitmap_enabled(&self, n: usize) -> bool {
        match n {
            0 => self.chctla() & 0x0002 != 0,
            1 => self.chctla() & 0x0200 != 0,
            _ => false,
        }
    }

    /// Bitmap size code for NBG0/1 (CHCTLA `N0BMSZ`/`N1BMSZ`):
    /// 0 = 512×256, 1 = 512×512, 2 = 1024×256, 3 = 1024×512.
    pub fn nbg_bitmap_size(&self, n: usize) -> u8 {
        match n {
            0 => ((self.chctla() >> 2) & 0x3) as u8,
            1 => ((self.chctla() >> 10) & 0x3) as u8,
            _ => 0,
        }
    }

    /// Plane size for NBG`n` (PLSZ at 0x03A, 2 bits each): 0 = 1×1 plane,
    /// 1 = 2×1, 2 = (reserved), 3 = 2×2 planes of pages.
    pub fn nbg_plane_size(&self, n: usize) -> u8 {
        ((self.read16(0x03A) >> (n * 2)) & 0x3) as u8
    }

    /// MPOFN (0x03C) — map offset (high 2 bits of each plane address).
    pub fn mpofn(&self) -> u16 {
        self.read16(0x03C)
    }
    /// Map offset for NBG`n` (MPOFN: 2 bits each, N0 1..0 … N3 13..12).
    pub fn nbg_map_offset(&self, n: usize) -> u32 {
        ((self.mpofn() >> (n * 4)) & 0x3) as u32
    }

    /// MPABN`n` plane-A map number (bits 5..0). Register base 0x040, +4/bg.
    pub fn nbg_plane_a_number(&self, n: usize) -> u32 {
        let mpab = self.read16(0x040 + (n as u32) * 4);
        (self.nbg_map_offset(n) << 6) | (mpab & 0x3F) as u32
    }

    /// Bitmap base for NBG`n`: `map_offset × 0x20000` bytes.
    pub fn nbg_bitmap_base(&self, n: usize) -> u32 {
        self.nbg_map_offset(n) * 0x2_0000
    }

    /// Pattern-name-table base (plane A) for NBG`n`, in bytes. Assumes the
    /// common 0x2000-byte page (64×64 cells, 1-word entries); larger plane
    /// sizes are a later refinement.
    pub fn nbg_pattern_table_base(&self, n: usize) -> u32 {
        self.nbg_plane_a_number(n) * 0x2000
    }

    /// Integer scroll (x, y) for NBG`n`. NBG0/1 carry an ignored fractional
    /// part; NBG2/3 are integer-only. Offsets per the VDP2 scroll register
    /// block (NBG0 0x70/0x74, NBG1 0x80/0x84, NBG2 0x90/0x92, NBG3 0x94/0x96).
    pub fn nbg_scroll(&self, n: usize) -> (u32, u32) {
        let (xo, yo) = match n {
            0 => (0x070, 0x074),
            1 => (0x080, 0x084),
            2 => (0x090, 0x092),
            _ => (0x094, 0x096),
        };
        (
            (self.read16(xo) & 0x07FF) as u32,
            (self.read16(yo) & 0x07FF) as u32,
        )
    }

    // ---- NBG0 wrappers (kept for existing callers/tests) ----

    pub fn nbg0_map_offset(&self) -> u32 {
        self.nbg_map_offset(0)
    }
    pub fn nbg0_plane_a_number(&self) -> u32 {
        self.nbg_plane_a_number(0)
    }
    pub fn nbg0_bitmap_base(&self) -> u32 {
        self.nbg_bitmap_base(0)
    }
    pub fn nbg0_pattern_table_base(&self) -> u32 {
        self.nbg_pattern_table_base(0)
    }
    pub fn nbg0_scroll_x(&self) -> u32 {
        self.nbg_scroll(0).0
    }
    pub fn nbg0_scroll_y(&self) -> u32 {
        self.nbg_scroll(0).1
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

    #[test]
    fn chctla_bitmap_enable_is_bit_one() {
        let mut r = Vdp2Regs::new();
        assert!(!r.nbg0_bitmap_enabled());
        r.write16(0x028, 0x0002); // N0BMEN
        assert!(r.nbg0_bitmap_enabled());
        // Bit 2 is the low N0BMSZ bit, NOT enable.
        r.write16(0x028, 0x0004);
        assert!(!r.nbg0_bitmap_enabled());
        assert_eq!(r.nbg0_bitmap_size(), 1);
    }

    #[test]
    fn map_offset_and_plane_compose_pattern_table_base() {
        let mut r = Vdp2Regs::new();
        // N0MP = 1 (high 2 bits), N0MPA = 5 (low 6 bits).
        r.write16(0x03C, 0x0001); // MPOFN.N0MP = 1
        r.write16(0x040, 0x0005); // MPABN0.N0MPA = 5
        assert_eq!(r.nbg0_map_offset(), 1);
        // plane number = (1 << 6) | 5 = 69.
        assert_eq!(r.nbg0_plane_a_number(), 69);
        assert_eq!(r.nbg0_pattern_table_base(), 69 * 0x2000);
        // Bitmap base keys only on the map offset.
        assert_eq!(r.nbg0_bitmap_base(), 0x2_0000);
    }

    #[test]
    fn zero_map_registers_keep_bases_at_origin() {
        let r = Vdp2Regs::new();
        assert_eq!(r.nbg0_bitmap_base(), 0);
        assert_eq!(r.nbg0_pattern_table_base(), 0);
    }

    #[test]
    fn scroll_integer_parts_decode() {
        let mut r = Vdp2Regs::new();
        r.write16(0x070, 0x0040); // SCXIN0 = 64
        r.write16(0x074, 0x0010); // SCYIN0 = 16
        assert_eq!(r.nbg0_scroll_x(), 64);
        assert_eq!(r.nbg0_scroll_y(), 16);
        // Only the low 11 bits are the integer part.
        r.write16(0x070, 0xF801);
        assert_eq!(r.nbg0_scroll_x(), 1);
    }
}
