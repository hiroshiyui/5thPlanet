//! VDP1 frame buffer — 256 KiB at `0x05C8_0000..=0x05CB_FFFF`.
//!
//! VDP1 double-buffers: the plotter draws into the back buffer while
//! the display reads the front, swapping at the rate FBCR programs.
//! M4 models only the visible window as one flat 256 KiB region —
//! enough that CPU reads/writes round-trip. There is no plotter and
//! no buffer swap yet (VDP1 rendering is M5), so reads return whatever
//! the CPU last wrote, defaulting to zero (transparent).

pub const FRAMEBUFFER_BYTES: usize = 256 * 1024;

/// Pixel stride and height of the 16-bit (RGB555) frame buffer in the
/// default TVM=0 configuration: 512 × 256 × 2 bytes = 256 KiB. The
/// plotter addresses pixels as `(y * STRIDE + x) * 2`, matching the
/// hardware layout VDP2 later reads back as the sprite layer.
pub const FB_STRIDE: i32 = 512;
pub const FB_HEIGHT: i32 = 256;

#[derive(Clone, Debug)]
pub struct Framebuffer {
    bytes: Vec<u8>,
}

impl Default for Framebuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framebuffer {
    pub fn new() -> Self {
        Self {
            bytes: vec![0u8; FRAMEBUFFER_BYTES],
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    fn idx(&self, offset: u32) -> usize {
        (offset as usize) % self.bytes.len()
    }

    /// Read the 16-bit pixel at `(x, y)` in the 512×256 RGB555 buffer.
    /// Callers must keep `0 <= x < 512` and `0 <= y < 256`.
    pub fn pixel(&self, x: i32, y: i32) -> u16 {
        self.read16(((y * FB_STRIDE + x) as u32) * 2)
    }

    /// Write the 16-bit pixel at `(x, y)`. Bounds are the caller's
    /// responsibility (the plotter clips before calling).
    pub fn set_pixel(&mut self, x: i32, y: i32, val: u16) {
        self.write16(((y * FB_STRIDE + x) as u32) * 2, val);
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
    fn round_trip_and_default_zero() {
        let mut fb = Framebuffer::new();
        assert_eq!(fb.read32(0), 0);
        fb.write16(0x40, 0x7FFF);
        assert_eq!(fb.read16(0x40), 0x7FFF);
    }

    #[test]
    fn mirrors_within_256_kib_window() {
        let mut fb = Framebuffer::new();
        fb.write32(0x80, 0xDEAD_BEEF);
        assert_eq!(fb.read32(0x80 + FRAMEBUFFER_BYTES as u32), 0xDEAD_BEEF);
    }
}
