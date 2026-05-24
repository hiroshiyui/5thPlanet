//! SH7604 UBC module — register storage only for M1.
//!
//! Real UBC behaviour is not required by the SH-2 core itself; future
//! milestones may add it if a Saturn game exercises a feature beyond
//! reading and writing the configuration registers. For now reads and
//! writes round-trip verbatim so software setup code doesn't trap.

/// Generic 16-byte register bank covering the UBC address span.
#[derive(Clone, Debug, Default)]
pub struct Ubc {
    pub raw: [u8; 32],
}

impl Ubc {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read8(&self, offset: u32) -> u8 {
        self.raw[(offset as usize) & 0x1F]
    }

    pub fn write8(&mut self, offset: u32, val: u8) {
        self.raw[(offset as usize) & 0x1F] = val;
    }
}
