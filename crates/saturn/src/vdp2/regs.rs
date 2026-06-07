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
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Vdp2Regs {
    #[serde(with = "serde_big_array::BigArray")]
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
    /// Active display size `(width, height)` in pixels, decoded from TVMD.
    /// HRESO (bits 2..0) selects the horizontal dot count — 320 / 352 / 640 /
    /// 704 (bit 2 picks the 31 kHz "exclusive monitor" variants, same pixel
    /// widths). VRESO (bits 5..4) selects 224 / 240 / 256 lines. LSMD (bits
    /// 7..6) == 3 is double-density interlace, which doubles the displayed line
    /// count. Hi-res (640/704) games — e.g. Doukyuusei ~if~ at 640×224 — need
    /// this so the renderer produces the correct width instead of a fixed 320.
    pub fn screen_dims(&self) -> (usize, usize) {
        let width = match self.h_resolution() & 0b11 {
            0 => 320,
            1 => 352,
            2 => 640,
            _ => 704,
        };
        let base_h = match self.v_resolution() {
            0 => 224,
            1 => 240,
            _ => 256,
        };
        let height = if (self.tvmd() >> 6) & 0b11 == 3 {
            base_h * 2 // double-density interlace
        } else {
            base_h
        };
        (width, height)
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

    /// Snap `(x, y)` to the mosaic block origin for a background layer, per
    /// MZCTL (0x022): bits 0–3 enable NBG0–3 mosaic, bit 4 RBG0; the block is
    /// `MZSZH+1` (bits 11:8) wide and `MZSZV+1` (bits 15:12) tall, so every dot
    /// in a block shows the colour of the block's top-left dot. `enable_bit`
    /// selects the layer (`1 << n` for NBG, `0x10` for RBG0); when that bit is
    /// clear the coordinate is returned unchanged (mosaic off → no-op).
    /// (*VDP2 User's Manual*, MZCTL.)
    pub fn mosaic_coord(&self, enable_bit: u16, x: u32, y: u32) -> (u32, u32) {
        let mzctl = self.read16(0x022);
        if mzctl & enable_bit == 0 {
            return (x, y);
        }
        let szh = (((mzctl >> 8) & 0xF) + 1) as u32;
        let szv = (((mzctl >> 12) & 0xF) + 1) as u32;
        (x - x % szh, y - y % szv)
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

    /// "Transparent-pen as solid" for NBG`n` (BGON NxTPON, bits 8..11). When
    /// set, palette code 0 is drawn as the opaque colour `CRAM[offset]` instead
    /// of being treated as transparent — the BIOS splash sets this on NBG3 so
    /// the metal's code-0 dots fill with silver rather than showing the
    /// backdrop through them.
    pub fn nbg_transparent_pen_solid(&self, n: usize) -> bool {
        self.bgon() & (1 << (8 + n)) != 0
    }

    /// "Transparent-pen as solid" for rotation `which` (R0TPON bit 12).
    pub fn rbg_transparent_pen_solid(&self, which: usize) -> bool {
        let bit = if which == 0 { 12 } else { 8 };
        self.bgon() & (1 << bit) != 0
    }

    /// Back-screen table: `(VRAM word address, per-line-colour mode)`. The
    /// backdrop colour is read as RGB555 from VRAM at this word address — the
    /// real Saturn backdrop, not CRAM[0]. BKTAU (0x0AC) bits 2..0 are the high
    /// word-address bits and bit 15 (BKCLMD) selects per-line colour (the table
    /// advances one word per scanline → gradients); BKTAL (0x0AE) is the low 16
    /// bits. (VDP2 manual §back screen; Mednafen `vdp2_render.cpp` `BKTA`.)
    pub fn back_screen(&self) -> (u32, bool) {
        let upper = self.read16(0x0AC);
        let lower = self.read16(0x0AE);
        let addr = (((upper as u32 & 0x0007) << 16) | lower as u32) & 0x7FFFF;
        (addr, upper & 0x8000 != 0)
    }

    /// Line-colour-screen table: `(VRAM word address, per-line mode)`. LCTAU
    /// (0x0A8) bits 2..0 + bit 15 (per-line), LCTAL (0x0AA) low 16. The colour is
    /// RGB555 masked to the low 11 bits on hardware (`& 0x07FF`); inserted into
    /// the colour calculation of layers selected by [`Self::line_colour_enable`].
    pub fn line_colour_screen(&self) -> (u32, bool) {
        let upper = self.read16(0x0A8);
        let lower = self.read16(0x0AA);
        let addr = (((upper as u32 & 0x0007) << 16) | lower as u32) & 0x7FFFF;
        (addr, upper & 0x8000 != 0)
    }

    /// Per-layer line-colour-screen insertion enable (LNCLEN, 0x0E8): bit 0
    /// NBG0/RBG1, 1 NBG1, 2 NBG2, 3 NBG3, 4 RBG0, 5 sprite (Mednafen
    /// `LineColorEn = V & 0x3F`).
    pub fn line_colour_enable(&self) -> u8 {
        (self.read16(0x0E8) & 0x3F) as u8
    }

    /// Shadow-receive enable for a screen (SDCTL 0x0E2, bits 0..5): NBG0..3 =
    /// 0..3, RBG0 = 4, back screen = 5 (RBG1 shares NBG0's bit 0). A sprite
    /// (MSB) shadow only darkens screens with their bit set — Mednafen
    /// `(SDCTL >> n) & 1` → `PIX_SHADEN`. (VDP2 manual §shadow, SDCTL.)
    pub fn shadow_enabled(&self, screen_bit: u8) -> bool {
        self.read16(0x0E2) & (1 << screen_bit) != 0
    }

    /// Per-screen colour-offset enable (CLOFEN, 0x110, bits 0..6): 0 NBG0/RBG1,
    /// 1 NBG1, 2 NBG2, 3 NBG3, 4 RBG0, 5 sprite, 6 back screen. When a screen's
    /// bit is set its final colour gets the selected RGB offset added — this is
    /// how games do fade-to-black / fade-to-white / tint transitions. (Mednafen
    /// `ColorOffsEn = V & 0x7F`; VDP2 manual §colour offset, CLOFEN.)
    pub fn color_offset_enable(&self) -> u8 {
        (self.read16(0x110) & 0x7F) as u8
    }

    /// Per-screen colour-offset select (CLOFSL, 0x112): same bit layout as
    /// CLOFEN; 0 picks offset set A (COAR/COAG/COAB), 1 picks set B
    /// (COBR/COBG/COBB). (Mednafen `ColorOffsSel = V & 0x7F`.)
    pub fn color_offset_select(&self) -> u8 {
        (self.read16(0x112) & 0x7F) as u8
    }

    /// The signed per-channel colour offset for set `ab` (0 = A, 1 = B) as
    /// `(r, g, b)`, each a 9-bit two's-complement value in -256..=255 added to
    /// the 8-bit RGB channel (then clamped 0..=255). Set A = COAR/COAG/COAB
    /// (0x114/0x116/0x118), set B = COBR/COBG/COBB (0x11A/0x11C/0x11E).
    /// (Mednafen `ColorOffs`, `sign_x_to_s32(9, V)`.)
    pub fn color_offset(&self, ab: usize) -> (i32, i32, i32) {
        let base = if ab == 0 { 0x114 } else { 0x11A };
        let sx9 = |off: u32| {
            let v = (self.read16(off) & 0x1FF) as i32;
            if v & 0x100 != 0 { v - 0x200 } else { v }
        };
        (sx9(base), sx9(base + 2), sx9(base + 4))
    }

    // ---- Special function: priority / colour-calc (C4 — registers modelled) ----
    //
    // The special-function machinery lets a layer raise/lower a *per-dot* effect
    // keyed on the dot's palette code: a colour code whose bit in the selected
    // SFCODE matches gets a priority-LSB or colour-calc adjustment (Mednafen
    // `MakeSFCodeLUT`). The registers below are decoded; **the per-dot
    // application is not yet wired** — it needs the sampler to carry each dot's
    // palette code out to the compositor, a sizeable change to a dormant feature
    // (SFPRMD/SFCCMD default 0). Deferred to avoid mis-rendering on an
    // unvalidatable per-dot path. (VDP2 manual §special priority / colour calc.)

    /// Special-function priority mode for `layer` (SFPRMD 0x0EA, 2 bits each):
    /// NBG0 0..1, NBG1 2..3, NBG2 4..5, NBG3 6..7, RBG0 8..9. 0 = off (priority
    /// is the register value), 1 = per-character special bit, 2/3 = per-dot
    /// SFCODE match.
    pub fn special_priority_mode(&self, layer: usize) -> u8 {
        ((self.read16(0x0EA) >> (layer * 2)) & 0x3) as u8
    }

    /// Special-function colour-calculation mode for `layer` (SFCCMD 0x0EE, 2
    /// bits each, same layout as SFPRMD).
    pub fn special_color_calc_mode(&self, layer: usize) -> u8 {
        ((self.read16(0x0EE) >> (layer * 2)) & 0x3) as u8
    }

    /// The 8-bit special-function code selected for `layer`: SFSEL (0x024, one
    /// bit per layer) picks SFCODE-A (low byte) or SFCODE-B (high byte) of
    /// SFCODE (0x026). A dot triggers the special function when bit
    /// `(palette_code >> 1) & 7` of this code is set (Mednafen `MakeSFCodeLUT`).
    pub fn special_function_code(&self, layer: usize) -> u8 {
        let sel = (self.read16(0x024) >> layer) & 1;
        (self.read16(0x026) >> (sel * 8)) as u8
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

    /// 8-bit fractional scroll `(x, y)` for NBG0/NBG1 (SCXDN/SCYDN, fraction in
    /// the high byte). NBG2/3 have no fraction. Combined with [`Self::nbg_scroll`]
    /// these give a 16.8 source position; the fraction only changes the sampled
    /// pixel once whole-layer/line zoom advances the accumulator across a dot.
    /// (Mednafen `XScrollF`/`YScrollF = (V >> 8) & 0xFF`.)
    pub fn nbg_scroll_frac(&self, n: usize) -> (u8, u8) {
        let (xo, yo) = if n == 0 { (0x072, 0x076) } else { (0x082, 0x086) };
        (
            ((self.read16(xo) >> 8) & 0xFF) as u8,
            ((self.read16(yo) >> 8) & 0xFF) as u8,
        )
    }

    /// Whole-layer per-dot coordinate increment `(x, y)` for NBG0/NBG1 as 16.16
    /// fixed point (ZMXN/ZMYN reduction/zoom; `0x1_0000` = 1:1). The hardware
    /// register is 3 integer bits (`ZMxIN`) + an 8-bit fraction (`ZMxDN`, high
    /// byte) — Mednafen's 11-bit `XCoordInc`/`YCoordInc` (`.8`), here shifted to
    /// `.16` to compose with the line-zoom table. A larger increment reduces the
    /// layer (each screen dot steps more source pixels). Only NBG0/1 support it.
    pub fn nbg_coord_inc(&self, n: usize) -> (u32, u32) {
        let (xi, yi) = if n == 0 { (0x078, 0x07C) } else { (0x088, 0x08C) };
        let inc = |int_off: u32, frac_off: u32| {
            let int = (self.read16(int_off) & 0x7) as u32;
            let frac = ((self.read16(frac_off) >> 8) & 0xFF) as u32;
            (int << 16) | (frac << 8)
        };
        (inc(xi, xi + 2), inc(yi, yi + 2))
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
    /// SPWINEN (SPCTL bit 4): when set, bit 15 of each VDP1 framebuffer pixel is
    /// the **sprite-window** flag (rather than the shadow / RGB-mode bit), which
    /// gates layers whose WCTL enables the sprite window. (Mednafen `SpriteWinEn
    /// = SPCTL & 0x10`; the window bit `sd = (src >> 15) & 1`.)
    pub fn sprite_window_enabled(&self) -> bool {
        self.spctl() & 0x10 != 0
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
    fn mosaic_coord_snaps_to_block_origin_when_enabled() {
        let mut r = Vdp2Regs::new();
        // No mosaic configured → coordinate unchanged for every layer.
        assert_eq!(r.mosaic_coord(1 << 0, 7, 5), (7, 5));

        // MZCTL: enable NBG0 (bit 0), 4×2 blocks (MZSZH=3 → width 4, MZSZV=1 → height 2).
        r.write16(0x022, (1 << 0) | (3 << 8) | (1 << 12));
        assert_eq!(r.mosaic_coord(1 << 0, 0, 0), (0, 0));
        assert_eq!(r.mosaic_coord(1 << 0, 3, 1), (0, 0), "snaps to the 4×2 block origin");
        assert_eq!(r.mosaic_coord(1 << 0, 5, 3), (4, 2), "next block over");
        // A layer whose enable bit is clear is untouched (NBG1, RBG0).
        assert_eq!(r.mosaic_coord(1 << 1, 5, 3), (5, 3));
        assert_eq!(r.mosaic_coord(0x10, 5, 3), (5, 3));
    }

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
    fn screen_dims_decode_every_resolution_code() {
        let mut r = Vdp2Regs::new();
        // HRESO (bits 2..0): only the low 2 bits select the width family.
        for (hres, w) in [(0u16, 320usize), (1, 352), (2, 640), (3, 704)] {
            r.write16(0x000, hres);
            assert_eq!(r.screen_dims().0, w, "HRESO {hres} → width {w}");
        }
        // HRESO bit 2 (the 31 kHz "exclusive monitor" variants) keeps the same
        // pixel width as the low 2 bits select.
        r.write16(0x000, 0b100); // hres 4 → low 2 bits = 0 → 320
        assert_eq!(r.screen_dims().0, 320);
        r.write16(0x000, 0b110); // hres 6 → low 2 bits = 2 → 640
        assert_eq!(r.screen_dims().0, 640);

        // VRESO (bits 5..4): 224 / 240 / 256 (code 3 also 256).
        for (vres, h) in [(0u16, 224usize), (1, 240), (2, 256), (3, 256)] {
            r.write16(0x000, vres << 4);
            assert_eq!(r.screen_dims().1, h, "VRESO {vres} → height {h}");
        }
    }

    #[test]
    fn screen_dims_double_density_interlace_doubles_height() {
        let mut r = Vdp2Regs::new();
        // LSMD (bits 7..6) == 3 → double-density interlace doubles the line count.
        r.write16(0x000, 0b11 << 6); // interlace (LSMD=3), vres 224
        assert_eq!(r.screen_dims(), (320, 448), "224 → 448 in double-density");
        // Doukyuusei ~if~: 640×224 hi-res, non-interlaced.
        r.write16(0x000, 0x8000 | 0b010); // DISP + hres 640
        assert_eq!(r.screen_dims(), (640, 224));
    }

    #[test]
    fn cram_offset_selects_the_bank_per_layer() {
        let mut r = Vdp2Regs::new();
        // CRAOFA (0x0E4): 3 bits per layer, N0..N3 at bits 2:0/6:4/10:8/14:12.
        r.write16(0x0E4, (1 << 0) | (3 << 4) | (5 << 8) | (7 << 12));
        assert_eq!(r.nbg_color_ram_offset(0), 1 << 8, "N0 offset 1 → CRAM 0x100");
        assert_eq!(r.nbg_color_ram_offset(1), 3 << 8, "N1 offset 3 → CRAM 0x300");
        assert_eq!(r.nbg_color_ram_offset(2), 5 << 8);
        assert_eq!(r.nbg_color_ram_offset(3), 7 << 8);
        // CRAOFB (0x0E6): RBG0 at bits 2:0; RBG1 reuses NBG0's offset.
        r.write16(0x0E6, 2);
        assert_eq!(r.rbg_color_ram_offset(0), 2 << 8, "RBG0 offset 2 → CRAM 0x200");
        assert_eq!(r.rbg_color_ram_offset(1), 1 << 8, "RBG1 shares NBG0's offset");
    }

    #[test]
    fn transparent_pen_solid_bits_decode() {
        let mut r = Vdp2Regs::new();
        // BGON NxTPON bits 8..11 — code-0 dots drawn solid rather than transparent.
        r.write16(0x020, (1 << 8) | (1 << 11)); // N0TPON + N3TPON
        assert!(r.nbg_transparent_pen_solid(0));
        assert!(!r.nbg_transparent_pen_solid(1));
        assert!(!r.nbg_transparent_pen_solid(2));
        assert!(r.nbg_transparent_pen_solid(3));
        // R0TPON is bit 12; RBG1 falls back to bit 8 (N0TPON).
        r.write16(0x020, (1 << 12) | (1 << 8));
        assert!(r.rbg_transparent_pen_solid(0), "R0TPON bit 12");
        assert!(r.rbg_transparent_pen_solid(1), "RBG1 reuses N0TPON bit 8");
    }

    #[test]
    fn nbg_color_mode_decodes_per_layer_widths() {
        let mut r = Vdp2Regs::new();
        // CHCTLA: N0CHCN bits 6..4 (3 bits), N1CHCN bits 13..12 (2 bits).
        r.write16(0x028, (0b100 << 4) | (0b10 << 12)); // N0 = 4 (16M RGB), N1 = 2
        assert_eq!(r.nbg_color_mode(0), 4);
        assert_eq!(r.nbg_color_mode(1), 2);
        // CHCTLB: N2CHCN bit 1, N3CHCN bit 5 (each a single 16/256 bit).
        r.write16(0x02A, (1 << 1) | (1 << 5));
        assert_eq!(r.nbg_color_mode(2), 1);
        assert_eq!(r.nbg_color_mode(3), 1);
    }

    #[test]
    fn nbg_priority_decode_per_layer() {
        let mut r = Vdp2Regs::new();
        // PRINA (0x0F8): N0 bits 2..0, N1 bits 10..8. PRINB (0x0FA): N2, N3.
        r.write16(0x0F8, 3 | (5 << 8));
        r.write16(0x0FA, 2 | (7 << 8));
        assert_eq!(r.nbg_priority(0), 3);
        assert_eq!(r.nbg_priority(1), 5);
        assert_eq!(r.nbg_priority(2), 2);
        assert_eq!(r.nbg_priority(3), 7);
    }

    #[test]
    fn nbg_char_size_and_bitmap_size_decode() {
        let mut r = Vdp2Regs::new();
        // CHCTLA: N0CHSZ bit 0, N1CHSZ bit 8. CHCTLB: N2CHSZ bit 0, N3CHSZ bit 4.
        r.write16(0x028, 0x0001 | 0x0100);
        r.write16(0x02A, 0x0001 | 0x0010);
        assert!(r.nbg_char_size_2x2(0));
        assert!(r.nbg_char_size_2x2(1));
        assert!(r.nbg_char_size_2x2(2));
        assert!(r.nbg_char_size_2x2(3));
        // NBG1 bitmap enable (bit 9) + size (bits 11..10).
        r.write16(0x028, 0x0200 | (0b10 << 10));
        assert!(r.nbg_bitmap_enabled(1));
        assert_eq!(r.nbg_bitmap_size(1), 2);
        // NBG2/3 are cell-only → never bitmap.
        assert!(!r.nbg_bitmap_enabled(2));
        assert_eq!(r.nbg_bitmap_size(2), 0);
    }

    #[test]
    fn pattern_name_control_fields_decode() {
        let mut r = Vdp2Regs::new();
        // PNCN0 at 0x030: PNB bit 15, CNSM bit 14, SPLT bits 7..5, SPCN bits 4..0.
        r.write16(0x030, 0x8000 | 0x4000 | (0b101 << 5) | 0b10011);
        assert!(r.nbg_pn_one_word(0), "PNB → 1-word entries");
        assert!(r.nbg_pn_cnsm(0));
        assert_eq!(r.nbg_pn_splt(0), 0b101);
        assert_eq!(r.nbg_pn_spcn(0), 0b10011);
    }

    #[test]
    fn plane_size_and_plane_page_compose() {
        let mut r = Vdp2Regs::new();
        // PLSZ (0x03A): NBG0 in bits 1..0.
        r.write16(0x03A, 0b11); // 2×2
        assert_eq!(r.nbg_plane_size(0), 0b11);
        // MPOFN N0MP = 2; MPCDN0 plane C (low byte of 0x042) = 9.
        r.write16(0x03C, 0x0002); // map offset 2
        r.write16(0x042, 9); // MPCDN0: plane C in low byte
        // plane page = (map_offset << 6) | MPxx = (2 << 6) | 9 = 137.
        assert_eq!(r.nbg_plane_page(0, 2), (2 << 6) | 9);
    }

    #[test]
    fn sprite_control_fields_decode() {
        let mut r = Vdp2Regs::new();
        // SPCTL (0x0E0): SPTYPE bits 3..0, SPCLMD bit 5, SPCCN bits 10..8,
        // SPCCCS bits 13..12.
        r.write16(0x0E0, 0x0007 | 0x0020 | (0b101 << 8) | (0b10 << 12));
        assert_eq!(r.sprite_type(), 7);
        assert!(r.sprite_rgb_mode());
        assert_eq!(r.sprite_cc_condition(), 0b101);
        assert_eq!(r.sprite_cc_mode(), 0b10);
        // Sprite priority registers PRISA..PRISD (0x0F0..) two 3-bit fields each.
        r.write16(0x0F0, 4 | (6 << 8)); // S0 = 4, S1 = 6
        assert_eq!(r.sprite_priority(0), 4);
        assert_eq!(r.sprite_priority(1), 6);
        // CCRSA (0x100) — colour-calc ratios, 5-bit fields.
        r.write16(0x100, 0x1F | (0x0A << 8));
        assert_eq!(r.sprite_color_calc_ratio(0), 0x1F);
        assert_eq!(r.sprite_color_calc_ratio(1), 0x0A);
    }

    #[test]
    fn color_calc_descriptor_keys_on_ccctl_enable_bits() {
        let mut r = Vdp2Regs::new();
        // CCCTL (0x0EC): per-NBG enable bits 0..3, CCMD (additive) bit 8.
        r.write16(0x0EC, 0x0001 | 0x0100); // enable N0, additive mode
        r.write16(0x108, 0x000C); // CCRNA: N0 ratio = 12
        assert!(r.color_calc_add_mode());
        assert_eq!(r.nbg_color_calc_ratio(0), 12);
        assert_eq!(r.nbg_color_calc(0), Some((12, true)), "enabled → (ratio, add)");
        assert_eq!(r.nbg_color_calc(1), None, "N1 disabled → None");
        // RBG0 uses CCCTL bit 4 + CCRR (0x10C).
        r.write16(0x0EC, 0x0010 | 0x0100);
        r.write16(0x10C, 0x0007);
        assert_eq!(r.rbg_color_calc(0), Some((7, true)));
    }

    #[test]
    fn window_rect_and_line_table_decode() {
        let mut r = Vdp2Regs::new();
        // W0: WPSX0 0x0C0 (>>1), WPSY0 0x0C2, WPEX0 0x0C4 (>>1), WPEY0 0x0C6.
        r.write16(0x0C0, 0x0040); // sx raw 0x40 → 0x20
        r.write16(0x0C2, 0x0030); // sy
        r.write16(0x0C4, 0x0280); // ex raw 0x280 → 0x140
        r.write16(0x0C6, 0x00E0); // ey
        assert_eq!(r.window_rect(0), (0x20, 0x140, 0x30, 0xE0));
        // LWTA0 (0x0D8): line-window enable bit 15 + table address (word units).
        r.write16(0x0D8, 0x8000 | 0x0002);
        r.write16(0x0DA, 0x0000);
        assert!(r.window_line_enabled(0));
        // address = ((hi & 7) << 16 | (lo & 0xFFFE)) << 1 = (0x2_0000) << 1.
        assert_eq!(r.window_line_table(0), 0x2_0000 << 1);
    }

    #[test]
    fn rotation_fields_decode() {
        let mut r = Vdp2Regs::new();
        // BGON: R0ON bit 4, R1ON bit 5.
        r.write16(0x020, (1 << 4) | (1 << 5));
        assert!(r.rbg_enabled(0));
        assert!(r.rbg_enabled(1));
        // MPOFR (0x03E): RA bits 1..0, RB bits 5..4.
        r.write16(0x03E, 0x0001 | (0x2 << 4));
        assert_eq!(r.rbg_map_offset(0), 1);
        assert_eq!(r.rbg_map_offset(1), 2);
        assert_eq!(r.rbg_bitmap_base(0), 0x2_0000);
        // CHCTLB: R0CHCN bits 14..12, R0BMEN bit 9, R0CHSZ bit 8.
        r.write16(0x02A, (0b011 << 12) | (1 << 9) | (1 << 8));
        assert_eq!(r.rbg_color_mode(), 3);
        assert!(r.rbg_bitmap_enabled());
        assert!(r.rbg_char_size_2x2());
        // PRIR (0x0FC): RBG0 priority bits 2..0.
        r.write16(0x0FC, 6);
        assert_eq!(r.rbg_priority(0), 6);
    }

    #[test]
    fn rotation_coeff_and_plane_size_decode() {
        let mut r = Vdp2Regs::new();
        // KTCTL (0x0B4): RAKTE bit 0, RAKDBS bit 1, RAKMD bits 3..2.
        r.write16(0x0B4, 0x0001 | 0x0002 | (0b10 << 2));
        assert!(r.rbg_coeff_enabled(0));
        assert!(r.rbg_coeff_size_word(0));
        assert_eq!(r.rbg_coeff_mode(0), 0b10);
        // PLSZ rotation: RA bits 9..8, screen-over RAOVR bits 11..10.
        r.write16(0x03A, (0b11 << 8) | (0b10 << 10));
        assert_eq!(r.rbg_plane_size(0), 0b11);
        assert_eq!(r.rbg_screen_over(0), 0b10);
    }

    #[test]
    fn line_scroll_control_decode() {
        let mut r = Vdp2Regs::new();
        // SCRCTL (0x09A): N0 in bits 5..0 — VCSC bit0, LSCX bit1, LSCY bit2,
        // LZMX bit3, LSS bits 5..4.
        r.write16(0x09A, 0b101111); // VCSC|LSCX|LSCY|LZMX, LSS=0b10 → 4 lines
        assert!(r.nbg_vcell_scroll(0));
        assert!(r.nbg_line_scroll_x(0));
        assert!(r.nbg_line_scroll_y(0));
        assert!(r.nbg_line_zoom_x(0));
        assert_eq!(r.nbg_line_scroll_interval(0), 4);
        // Line-scroll table address (word units, <<1 to bytes).
        r.write16(0x0A0, 0x0001); // hi
        r.write16(0x0A2, 0x0000); // lo
        assert_eq!(r.nbg_line_scroll_table(0), 0x1_0000 << 1);
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

    #[test]
    fn back_and_line_colour_screen_addresses_decode() {
        let mut r = Vdp2Regs::new();
        // BKTAU bits 2..0 are the high word-address bits; bit 15 is per-line.
        r.write16(0x0AC, 0x8003); // BKCLMD + high bits 3
        r.write16(0x0AE, 0x1234); // low 16
        assert_eq!(r.back_screen(), (0x3_1234, true));
        r.write16(0x0AC, 0x0000);
        assert_eq!(r.back_screen(), (0x1234, false));
        // Line-colour table + enable register.
        r.write16(0x0A8, 0x0001);
        r.write16(0x0AA, 0x5678);
        assert_eq!(r.line_colour_screen(), (0x1_5678, false));
        r.write16(0x0E8, 0x00FF); // LNCLEN masked to 6 bits
        assert_eq!(r.line_colour_enable(), 0x3F);
    }

    #[test]
    fn shadow_enable_bits_decode_per_screen() {
        let mut r = Vdp2Regs::new();
        r.write16(0x0E2, 0b11_0001); // NBG0 (0), RBG0 (4), back (5)
        assert!(r.shadow_enabled(0));
        assert!(!r.shadow_enabled(1));
        assert!(r.shadow_enabled(4));
        assert!(r.shadow_enabled(5));
    }

    #[test]
    fn special_function_registers_decode() {
        let mut r = Vdp2Regs::new();
        // SFPRMD: NBG0 = mode 2, NBG3 = mode 1, RBG0 = mode 3.
        r.write16(0x0EA, 2 | (1 << 6) | (3 << 8));
        assert_eq!(r.special_priority_mode(0), 2);
        assert_eq!(r.special_priority_mode(3), 1);
        assert_eq!(r.special_priority_mode(4), 3); // RBG0 in bits 8..9
        // SFCCMD: NBG1 = mode 2.
        r.write16(0x0EE, 2 << 2);
        assert_eq!(r.special_color_calc_mode(1), 2);
        // SFSEL picks code A/B; SFCODE-A = 0x12, SFCODE-B = 0x34.
        r.write16(0x026, 0x3412);
        r.write16(0x024, 0b0010); // layer 1 → code B
        assert_eq!(r.special_function_code(0), 0x12);
        assert_eq!(r.special_function_code(1), 0x34);
    }
}
