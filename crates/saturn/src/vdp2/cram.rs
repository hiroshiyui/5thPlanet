//! VDP2 Color RAM (CRAM) — 4 KiB at `0x05F0_0000..=0x05F0_0FFF`.
//!
//! Mirrors within the 4 KiB region via offset folding. The Saturn
//! supports three CRAM "modes" selectable by `RAMCTL`:
//!
//! - **Mode 0**: 1024 × 16-bit entries (RGB555 + transparency bit)
//! - **Mode 1**: 2048 × 16-bit entries (same RGB555 encoding, more entries)
//! - **Mode 2**: 1024 × 32-bit entries (true 8-bit-per-channel RGB888)
//!
//! [`Cram::color_rgb888`] decodes any of them given the current RAMCTL.CRMD;
//! the renderer passes the live mode. ([`Cram::color_rgb888_mode0`] remains
//! the fast RGB555 path the mode-aware lookup delegates to for modes 0/1.)

const CRAM_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug)]
pub struct Cram {
    bytes: Vec<u8>,
}

impl Default for Cram {
    fn default() -> Self {
        Self::new()
    }
}

impl Cram {
    pub fn new() -> Self {
        Self {
            bytes: vec![0u8; CRAM_BYTES],
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    fn idx(&self, offset: u32) -> usize {
        (offset as usize) % self.bytes.len()
    }

    pub fn read8(&self, offset: u32) -> u8 {
        self.bytes[self.idx(offset)]
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
        self.bytes[i] = val;
    }
    pub fn write16(&mut self, offset: u32, val: u16) {
        let i = self.idx(offset);
        let n = self.bytes.len();
        let b = val.to_be_bytes();
        self.bytes[i] = b[0];
        self.bytes[(i + 1) % n] = b[1];
    }
    pub fn write32(&mut self, offset: u32, val: u32) {
        let i = self.idx(offset);
        let n = self.bytes.len();
        let b = val.to_be_bytes();
        self.bytes[i] = b[0];
        self.bytes[(i + 1) % n] = b[1];
        self.bytes[(i + 2) % n] = b[2];
        self.bytes[(i + 3) % n] = b[3];
    }

    /// Look up a Mode-0 palette entry (16-bit RGB555 + T bit) and
    /// expand it to an RGB888 triple. The T bit is dropped — sprite
    /// priority is the compositor's concern, not the palette's.
    pub fn color_rgb888_mode0(&self, index: usize) -> (u8, u8, u8) {
        let off = (index * 2) % self.bytes.len();
        let entry = u16::from_be_bytes([self.bytes[off], self.bytes[off + 1]]);
        let r5 = entry & 0x1F;
        let g5 = (entry >> 5) & 0x1F;
        let b5 = (entry >> 10) & 0x1F;
        (expand5to8(r5), expand5to8(g5), expand5to8(b5))
    }

    /// Look up a palette entry honouring the current CRAM mode (RAMCTL.CRMD):
    ///
    /// - **0 / 1** — 16-bit RGB555 entries (`index × 2`); modes differ only in
    ///   entry count (1024 vs 2048), which is the caller's index range.
    /// - **2 / 3** — 32-bit RGB888 entries (`index × 4`, stored `0x00BBGGRR`),
    ///   giving true 8-bit-per-channel colour.
    pub fn color_rgb888(&self, index: usize, cram_mode: u8) -> (u8, u8, u8) {
        match cram_mode {
            2 | 3 => {
                let off = (index * 4) % self.bytes.len();
                let w = self.read32(off as u32);
                (
                    (w & 0xFF) as u8,
                    ((w >> 8) & 0xFF) as u8,
                    ((w >> 16) & 0xFF) as u8,
                )
            }
            _ => self.color_rgb888_mode0(index),
        }
    }
}

/// Expand a 15-bit RGB555 value (the low 15 bits of an entry / a direct
/// 16bpp dot) to an RGB888 triple. The top bit is ignored.
#[inline]
pub fn rgb555_to_888(entry: u16) -> (u8, u8, u8) {
    (
        expand5to8(entry & 0x1F),
        expand5to8((entry >> 5) & 0x1F),
        expand5to8((entry >> 10) & 0x1F),
    )
}

/// Expand a 5-bit colour channel to 8 bits, replicating the high
/// bits into the low ones so 0x1F maps to 0xFF instead of 0xF8.
#[inline]
fn expand5to8(v: u16) -> u8 {
    let v = v & 0x1F;
    ((v << 3) | (v >> 2)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_byte_word_long() {
        let mut c = Cram::new();
        c.write32(0x10, 0xDEAD_BEEF);
        assert_eq!(c.read32(0x10), 0xDEAD_BEEF);
        assert_eq!(c.read8(0x10), 0xDE);
        assert_eq!(c.read16(0x12), 0xBEEF);
    }

    #[test]
    fn mirrors_within_4kib_window() {
        let mut c = Cram::new();
        c.write16(0x0080, 0xCAFE);
        assert_eq!(c.read16(0x1080), 0xCAFE, "0x1080 = 0x0080 + 4 KiB");
    }

    #[test]
    fn mode0_color_lookup_expands_5_to_8_bits() {
        let mut c = Cram::new();
        // R=31, G=0, B=0 → 0x001F (high bit T=0)
        c.write16(0, 0x001F);
        assert_eq!(c.color_rgb888_mode0(0), (0xFF, 0x00, 0x00));

        // R=0, G=0, B=31 → 0x7C00
        c.write16(2, 0x7C00);
        assert_eq!(c.color_rgb888_mode0(1), (0x00, 0x00, 0xFF));

        // R=16, G=16, B=16 → all 0x10 → 0x4210
        c.write16(4, 0x4210);
        // 0x10 = 0b10000 → 0b10000_100 = 0x84
        assert_eq!(c.color_rgb888_mode0(2), (0x84, 0x84, 0x84));
    }

    #[test]
    fn mode0_lookup_ignores_transparency_bit() {
        let mut c = Cram::new();
        // Set T bit + R=31; the T bit shouldn't bleed into the colour.
        c.write16(0, 0x801F);
        assert_eq!(c.color_rgb888_mode0(0), (0xFF, 0x00, 0x00));
    }

    #[test]
    fn mode2_lookup_reads_32bit_rgb888_entries() {
        let mut c = Cram::new();
        // Entry 1 (index 1 → byte 4): 0x00BBGGRR with R=0x12, G=0x34, B=0x56.
        c.write32(4, 0x0056_3412);
        assert_eq!(c.color_rgb888(1, 2), (0x12, 0x34, 0x56));
        // Modes 0/1 still take the 16-bit RGB555 path.
        c.write16(0, 0x001F);
        assert_eq!(c.color_rgb888(0, 0), (0xFF, 0x00, 0x00));
        assert_eq!(c.color_rgb888(0, 1), (0xFF, 0x00, 0x00));
    }
}
