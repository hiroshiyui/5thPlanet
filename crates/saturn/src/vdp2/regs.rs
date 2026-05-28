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

    /// Colour-RAM address offset for NBG`n` (CRAOFA, 0x0E4 — 3 bits per layer,
    /// N0..N3 at bits 2:0 / 6:4 / 10:8 / 14:12). The value selects a CRAM bank
    /// by supplying the high bits of the palette index (`offset × 0x100`); a
    /// layer's effective colour is `CRAM[(offset << 8) | dot]`. The BIOS splash
    /// puts NBG3's silver palette at offset 3 (CRAM 0x300+), so ignoring this
    /// read the dark CRAM 0x000+ bank instead.
    pub fn nbg_color_ram_offset(&self, n: usize) -> usize {
        (((self.read16(0x0E4) >> (n * 4)) & 0x7) as usize) << 8
    }

    /// Colour-RAM address offset for rotation `which` (CRAOFB, 0x0E6 — RBG0 at
    /// bits 2:0). RBG1 (rare) reuses NBG0's offset.
    pub fn rbg_color_ram_offset(&self, which: usize) -> usize {
        if which == 0 {
            ((self.read16(0x0E6) & 0x7) as usize) << 8
        } else {
            self.nbg_color_ram_offset(0)
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

    /// Pattern-name control word for NBG`n` (PNCN0..3 at 0x030 + n·2).
    pub fn nbg_pncn(&self, n: usize) -> u16 {
        self.read16(0x030 + n as u32 * 2)
    }
    /// Pattern-name data size: true = 1-word entries, false = 2-word (PNB).
    pub fn nbg_pn_one_word(&self, n: usize) -> bool {
        self.nbg_pncn(n) & 0x8000 != 0
    }
    /// Character-number supplement mode (CNSM): in 1-word mode, true selects
    /// a 12-bit char number with no flip, false a 10-bit number + 2 flip bits.
    pub fn nbg_pn_cnsm(&self, n: usize) -> bool {
        self.nbg_pncn(n) & 0x4000 != 0
    }
    /// Supplementary character bits (SPCN, 1-word mode).
    pub fn nbg_pn_spcn(&self, n: usize) -> u32 {
        (self.nbg_pncn(n) & 0x1F) as u32
    }
    /// Supplementary palette bits (SPLT, 1-word 16-colour mode).
    pub fn nbg_pn_splt(&self, n: usize) -> u32 {
        ((self.nbg_pncn(n) >> 5) & 0x7) as u32
    }

    /// The 9-bit plane number for NBG`n` plane `plane` (0=A,1=B,2=C,3=D),
    /// i.e. `(map_offset << 6) | MPxx`. MPABNn (A/B) at 0x040 + n·4, MPCDNn
    /// (C/D) at 0x042 + n·4.
    pub fn nbg_plane_page(&self, n: usize, plane: usize) -> u32 {
        let base = 0x040 + n as u32 * 4;
        let (reg, shift) = match plane {
            0 => (base, 0),
            1 => (base, 8),
            2 => (base + 2, 0),
            _ => (base + 2, 8),
        };
        let mp = ((self.read16(reg) >> shift) & 0x3F) as u32;
        (self.nbg_map_offset(n) << 6) | mp
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

    // ---- Line scroll (NBG0/NBG1 only) ----
    //
    // SCRCTL (0x09A) enables per-line horizontal/vertical scroll and selects
    // the line-scroll interval (LSS): every 1/2/4/8 lines. The table lives in
    // VRAM at LSTAn (NBG0 0x0A0/0x0A2, NBG1 0x0A4/0x0A6), word-addressed.

    pub fn scrctl(&self) -> u16 {
        self.read16(0x09A)
    }
    /// SCRCTL field block for NBG`n` (n = 0/1): N0 in bits 5..0, N1 in 13..8.
    fn scrctl_bits(&self, n: usize) -> u16 {
        if n == 0 {
            self.scrctl() & 0x3F
        } else {
            (self.scrctl() >> 8) & 0x3F
        }
    }
    /// Horizontal line-scroll enable for NBG`n` (LSCX).
    pub fn nbg_line_scroll_x(&self, n: usize) -> bool {
        self.scrctl_bits(n) & 0x02 != 0
    }
    /// Vertical line-scroll enable for NBG`n` (LSCY).
    pub fn nbg_line_scroll_y(&self, n: usize) -> bool {
        self.scrctl_bits(n) & 0x04 != 0
    }
    /// Horizontal line-zoom enable for NBG`n` (LZMX) — its longword occupies a
    /// table slot even though the renderer doesn't apply the zoom yet.
    pub fn nbg_line_zoom_x(&self, n: usize) -> bool {
        self.scrctl_bits(n) & 0x08 != 0
    }
    /// Line-scroll interval in lines (LSS): 1, 2, 4 or 8.
    pub fn nbg_line_scroll_interval(&self, n: usize) -> u32 {
        1 << ((self.scrctl_bits(n) >> 4) & 0x3)
    }
    /// Byte address in VDP2 VRAM of NBG`n`'s line-scroll table. The register
    /// pair holds the address divided by 2 (word units).
    pub fn nbg_line_scroll_table(&self, n: usize) -> u32 {
        let (hi, lo) = if n == 0 {
            (0x0A0, 0x0A2)
        } else {
            (0x0A4, 0x0A6)
        };
        let addr = (((self.read16(hi) & 0x7) as u32) << 16) | self.read16(lo) as u32;
        addr << 1
    }
    /// Vertical-cell-scroll enable for NBG`n` (SCRCTL N0VCSC/N1VCSC).
    pub fn nbg_vcell_scroll(&self, n: usize) -> bool {
        self.scrctl_bits(n) & 0x01 != 0
    }
    /// Byte address in VDP2 VRAM of the (shared) vertical-cell-scroll table
    /// (VCSTAU/VCSTAL at 0x09C/0x09E), word-addressed.
    pub fn vcell_scroll_table(&self) -> u32 {
        let addr = (((self.read16(0x09C) & 0x7) as u32) << 16) | self.read16(0x09E) as u32;
        addr << 1
    }

    // ---- Sprite-layer (VDP1 framebuffer) control ----
    //
    // VDP2 reads the VDP1 frame buffer as the sprite layer. SPCTL (0x0E0)
    // selects the sprite-data *type* (0..15), which fixes how each 16-bit
    // frame-buffer word splits into priority bits / colour-calc bits /
    // colour code; SPCLMD (bit 5) enables RGB direct-colour for words with
    // the MSB set. The chosen priority bits index the eight sprite-priority
    // registers PRISA..PRISD (S0PRIN..S7PRIN).

    /// SPCTL (0x0E0) — sprite control.
    pub fn spctl(&self) -> u16 {
        self.read16(0x0E0)
    }
    /// Sprite data type (SPTYPE, SPCTL bits 3..0).
    pub fn sprite_type(&self) -> usize {
        (self.spctl() & 0xF) as usize
    }
    /// SPCLMD (SPCTL bit 5): when set, sprite words with the MSB set are
    /// RGB direct colour rather than a palette code.
    pub fn sprite_rgb_mode(&self) -> bool {
        self.spctl() & 0x20 != 0
    }
    /// Sprite priority register `i` (0..7) — PRISA/PRISB/PRISC/PRISD at
    /// 0x0F0/0x0F2/0x0F4/0x0F6, two 3-bit fields each (even = bits 2..0,
    /// odd = bits 10..8). Priority 0 means the sprite dot is not shown.
    pub fn sprite_priority(&self, i: usize) -> u8 {
        let reg = 0x0F0 + (i as u32 / 2) * 2;
        let shift = (i & 1) * 8;
        ((self.read16(reg) >> shift) & 0x7) as u8
    }
    /// Sprite colour-calculation ratio register `i` (0..7) — CCRSA..CCRSD at
    /// 0x100/0x102/0x104/0x106, two 5-bit fields each.
    pub fn sprite_color_calc_ratio(&self, i: usize) -> u8 {
        let reg = 0x100 + (i as u32 / 2) * 2;
        let shift = (i & 1) * 8;
        ((self.read16(reg) >> shift) & 0x1F) as u8
    }
    /// SPCCN (SPCTL bits 10..8) — the sprite priority compared against for
    /// colour-calc enable, per SPCCCS mode.
    pub fn sprite_cc_condition(&self) -> u8 {
        ((self.spctl() >> 8) & 0x7) as u8
    }
    /// SPCCCS (SPCTL bits 13..12) — colour-calc condition mode: 0 = priority
    /// ≤ SPCCN, 1 = == , 2 = ≥ , 3 = always (MSB-controlled).
    pub fn sprite_cc_mode(&self) -> u8 {
        ((self.spctl() >> 12) & 0x3) as u8
    }

    // ---- Colour calculation (CCCTL 0x0EC + ratio registers) ----

    pub fn ccctl(&self) -> u16 {
        self.read16(0x0EC)
    }
    /// CCMD (CCCTL bit 8): false = ratio (alpha) blend, true = additive blend.
    pub fn color_calc_add_mode(&self) -> bool {
        self.ccctl() & 0x100 != 0
    }
    /// SPCCEN (CCCTL bit 6) — master enable for sprite colour calculation.
    pub fn sprite_color_calc_enabled(&self) -> bool {
        self.ccctl() & 0x40 != 0
    }
    /// Colour-calc ratio (0..31) for NBG`n`: CCRNA (0x108) holds N0/N1,
    /// CCRNB (0x10A) holds N2/N3, low/high 5-bit fields.
    pub fn nbg_color_calc_ratio(&self, n: usize) -> u8 {
        let (reg, shift) = match n {
            0 => (0x108, 0),
            1 => (0x108, 8),
            2 => (0x10A, 0),
            _ => (0x10A, 8),
        };
        ((self.read16(reg) >> shift) & 0x1F) as u8
    }
    /// Colour-calc descriptor `(ratio, add_mode)` for NBG`n` if CCCTL enables
    /// it (bits 0..3 = N0..N3), else `None`.
    pub fn nbg_color_calc(&self, n: usize) -> Option<(u8, bool)> {
        (self.ccctl() & (1 << n) != 0)
            .then(|| (self.nbg_color_calc_ratio(n), self.color_calc_add_mode()))
    }
    /// Colour-calc descriptor for RBG`which`: RBG0 uses CCCTL bit 4 + CCRR
    /// (0x10C); RBG1 shares NBG0's bit 0 + ratio.
    pub fn rbg_color_calc(&self, which: usize) -> Option<(u8, bool)> {
        if which == 0 {
            (self.ccctl() & 0x10 != 0).then(|| {
                (
                    (self.read16(0x10C) & 0x1F) as u8,
                    self.color_calc_add_mode(),
                )
            })
        } else {
            self.nbg_color_calc(0)
        }
    }

    // ---- Windows (W0/W1 rectangles + per-layer WCTL control bytes) ----

    /// Window `w` (0/1) rectangle `(start_x, end_x, start_y, end_y)` in
    /// low-res screen dots. Horizontal coordinates are stored at half-dot
    /// resolution (`>>1`) in normal mode; vertical is per-line. With a line
    /// window enabled the horizontal pair is overridden per scanline from the
    /// line-window table; hi-res scaling is a later refinement.
    pub fn window_rect(&self, w: usize) -> (u32, u32, u32, u32) {
        let base = if w == 0 { 0x0C0 } else { 0x0C8 };
        let sx = ((self.read16(base) & 0x3FE) >> 1) as u32;
        let sy = (self.read16(base + 2) & 0x3FF) as u32;
        let ex = ((self.read16(base + 4) & 0x3FE) >> 1) as u32;
        let ey = (self.read16(base + 6) & 0x3FF) as u32;
        (sx, ex, sy, ey)
    }
    /// Whether window `w` uses a per-line window table (LWTAnU bit 15).
    pub fn window_line_enabled(&self, w: usize) -> bool {
        let reg = if w == 0 { 0x0D8 } else { 0x0DC };
        self.read16(reg) & 0x8000 != 0
    }
    /// Byte address in VDP2 VRAM of window `w`'s line-window table (LWTAn).
    /// The register pair holds the address divided by 2 (word units).
    pub fn window_line_table(&self, w: usize) -> u32 {
        let (hi, lo) = if w == 0 {
            (0x0D8, 0x0DA)
        } else {
            (0x0DC, 0x0DE)
        };
        let addr = (((self.read16(hi) & 0x7) as u32) << 16) | (self.read16(lo) & 0xFFFE) as u32;
        addr << 1
    }
    /// Per-layer window-control byte (W0/W1 enable+area+logic bits). N0/N1 in
    /// WCTLA (0x0D0), N2/N3 in WCTLB (0x0D2), RBG0/sprite in WCTLC (0x0D4).
    pub fn nbg_window_control(&self, n: usize) -> u8 {
        let (reg, shift) = match n {
            0 => (0x0D0, 0),
            1 => (0x0D0, 8),
            2 => (0x0D2, 0),
            _ => (0x0D2, 8),
        };
        ((self.read16(reg) >> shift) & 0xFF) as u8
    }
    pub fn rbg0_window_control(&self) -> u8 {
        (self.read16(0x0D4) & 0xFF) as u8
    }
    pub fn sprite_window_control(&self) -> u8 {
        ((self.read16(0x0D4) >> 8) & 0xFF) as u8
    }

    // ---- Rotation backgrounds (RBG0 / RBG1) ----
    //
    // RBG0 (BGON bit 4) uses rotation parameter set A, its colour/map/priority
    // from the R0* fields (CHCTLB, MPOFR.RAMP, PRIR). RBG1 (BGON bit 5) uses
    // parameter set B and MPOFR.RBMP; its priority shares the NBG0 slot
    // (N0PRIN). The parameter-table address comes from RPTAU/RPTAL.

    /// RBG`which` (0/1) enable — BGON bits 4 (R0ON) / 5 (R1ON).
    pub fn rbg_enabled(&self, which: usize) -> bool {
        self.bgon() & (0x10 << which) != 0
    }
    /// RBG`which` priority: RBG0 from PRIR (R0PRIN, 0x0FC bits 2..0); RBG1
    /// shares NBG0's priority (PRINA bits 2..0). 0 = not shown.
    pub fn rbg_priority(&self, which: usize) -> u8 {
        if which == 0 {
            (self.read16(0x0FC) & 0x7) as u8
        } else {
            (self.read16(0x0F8) & 0x7) as u8
        }
    }
    /// RBG colour number (CHCTLB R0CHCN, bits 14..12): 0=16, 1=256, 2=2048,
    /// 3=32K RGB, 4=16M RGB. Both rotation backgrounds use this field here.
    pub fn rbg_color_mode(&self) -> u8 {
        ((self.chctlb() >> 12) & 0x7) as u8
    }
    /// RBG bitmap-format enable (CHCTLB R0BMEN, bit 9).
    pub fn rbg_bitmap_enabled(&self) -> bool {
        self.chctlb() & 0x0200 != 0
    }
    /// RBG bitmap size (CHCTLB R0BMSZ, bit 10): 0 = 512×256, 1 = 512×512.
    pub fn rbg_bitmap_size(&self) -> u8 {
        ((self.chctlb() >> 10) & 0x1) as u8
    }
    /// Rotation map offset (MPOFR at 0x03E): RA in bits 1..0, RB in bits 5..4.
    pub fn rbg_map_offset(&self, which: usize) -> u32 {
        let shift = if which == 0 { 0 } else { 4 };
        ((self.read16(0x03E) >> shift) & 0x3) as u32
    }
    /// RBG bitmap base: `map_offset × 0x20000` bytes.
    pub fn rbg_bitmap_base(&self, which: usize) -> u32 {
        self.rbg_map_offset(which) * 0x2_0000
    }
    /// Rotation parameter-table byte address: `(RPTAU << 16 | RPTAL) << 1`.
    pub fn rotation_table_addr(&self) -> u32 {
        (((self.read16(0x0BC) & 0x7) as u32) << 16 | self.read16(0x0BE) as u32) << 1
    }
    /// Rotation plane-A map number (low 6 bits): RA from MPABRA (0x050), RB
    /// from MPABRB (0x060).
    pub fn rbg_plane_a_map(&self, which: usize) -> u16 {
        self.read16(if which == 0 { 0x050 } else { 0x060 })
    }

    /// Rotation pattern-name control (PNCR, 0x038) — same field layout as the
    /// NBG `PNCN` registers.
    pub fn rbg_pncr(&self) -> u16 {
        self.read16(0x038)
    }
    pub fn rbg_pn_one_word(&self) -> bool {
        self.rbg_pncr() & 0x8000 != 0
    }
    pub fn rbg_pn_cnsm(&self) -> bool {
        self.rbg_pncr() & 0x4000 != 0
    }
    pub fn rbg_pn_spcn(&self) -> u32 {
        (self.rbg_pncr() & 0x1F) as u32
    }
    pub fn rbg_pn_splt(&self) -> u32 {
        ((self.rbg_pncr() >> 5) & 0x7) as u32
    }
    /// Rotation character size (CHCTLB R0CHSZ, bit 8): false = 8×8, true = 16×16.
    pub fn rbg_char_size_2x2(&self) -> bool {
        self.chctlb() & 0x0100 != 0
    }
    /// Rotation plane size (PLSZ): RA in bits 9..8, RB in bits 13..12.
    /// 0 = 1×1 page, 1 = 2×1, 3 = 2×2.
    pub fn rbg_plane_size(&self, which: usize) -> u8 {
        let shift = if which == 0 { 8 } else { 12 };
        ((self.read16(0x03A) >> shift) & 0x3) as u8
    }
    /// Rotation screen-over mode (PLSZ RAOVR bits 11..10 / RBOVR 15..14):
    /// 0 = repeat the field, 1 = screen-over pattern, 2 = transparent outside
    /// the field, 3 = transparent outside a 512×512 area.
    pub fn rbg_screen_over(&self, which: usize) -> u8 {
        let shift = if which == 0 { 10 } else { 14 };
        ((self.read16(0x03A) >> shift) & 0x3) as u8
    }
    /// The 9-bit map number for rotation `which`, plane index `plane` (0..15 =
    /// A..P, row-major over the 4×4 plane grid). RA map registers are
    /// MPABRA..MPOPRA (0x050..0x05E), RB MPABRB..MPOPRB (0x060..0x06E), two
    /// 6-bit numbers per 16-bit register.
    pub fn rbg_plane_number(&self, which: usize, plane: usize) -> u32 {
        let base = if which == 0 { 0x050 } else { 0x060 };
        let reg = base + (plane as u32 / 2) * 2;
        let shift = (plane & 1) * 8;
        let mp = ((self.read16(reg) >> shift) & 0x3F) as u32;
        (self.rbg_map_offset(which) << 6) | mp
    }

    // ---- Rotation coefficient table (KTCTL 0x0B4, KTAOF 0x0B6) ----

    /// Coefficient-table enable for rotation `which` (RAKTE / RBKTE).
    pub fn rbg_coeff_enabled(&self, which: usize) -> bool {
        self.read16(0x0B4) & (1 << if which == 0 { 0 } else { 8 }) != 0
    }
    /// Coefficient data size: false = longword (24-bit), true = word (15-bit).
    pub fn rbg_coeff_size_word(&self, which: usize) -> bool {
        self.read16(0x0B4) & (1 << if which == 0 { 1 } else { 9 }) != 0
    }
    /// Coefficient use mode (RAKMD/RBKMD): 0 = kx & ky, 1 = kx, 2 = ky,
    /// 3 = Xp.
    pub fn rbg_coeff_mode(&self, which: usize) -> u8 {
        let shift = if which == 0 { 2 } else { 10 };
        ((self.read16(0x0B4) >> shift) & 0x3) as u8
    }
    /// Byte address in VRAM of the coefficient table (KTAOF address offset ×
    /// the per-size bank). Assumes the table is in VRAM (RAMCTL.CRKTE = 0).
    pub fn rbg_coeff_table_base(&self, which: usize) -> u32 {
        let ktaof = self.read16(0x0B6);
        let aos = (if which == 0 { ktaof } else { ktaof >> 8 } & 0x7) as u32;
        if self.rbg_coeff_size_word(which) {
            aos * 0x2_0000
        } else {
            (aos & 0x3) * 0x4_0000
        }
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
