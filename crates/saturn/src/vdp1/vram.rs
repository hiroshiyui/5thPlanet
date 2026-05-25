//! VDP1 VRAM — 512 KiB at `0x05C0_0000..=0x05C7_FFFF`.
//!
//! Holds the command table (the sprite/polygon display list) plus
//! character and gouraud-shading data. M4 only needs the storage to
//! round-trip: BIOS init code stages a display list here even though
//! the VDP1 plotter (M5) doesn't yet consume it. Addressing folds
//! modulo size so accesses past the end mirror back to the start,
//! matching the VDP2 VRAM convention in this crate.

pub const VRAM_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug)]
pub struct Vram {
    bytes: Vec<u8>,
}

impl Default for Vram {
    fn default() -> Self {
        Self::new()
    }
}

impl Vram {
    pub fn new() -> Self {
        Self {
            bytes: vec![0u8; VRAM_BYTES],
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.bytes
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_byte_word_long() {
        let mut v = Vram::new();
        v.write32(0x10000, 0xAABB_CCDD);
        assert_eq!(v.read32(0x10000), 0xAABB_CCDD);
        assert_eq!(v.read8(0x10003), 0xDD);
        assert_eq!(v.read16(0x10000), 0xAABB);
    }

    #[test]
    fn mirrors_within_512_kib_window() {
        let mut v = Vram::new();
        v.write32(0x100, 0x1234_5678);
        assert_eq!(v.read32(0x8_0100), 0x1234_5678);
    }
}
