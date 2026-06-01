//! SH7604 BSC module — register storage only for M1.
//!
//! Real BSC behaviour is not required by the SH-2 core itself; future
//! milestones may add it if a Saturn game exercises a feature beyond
//! reading and writing the configuration registers. For now reads and
//! writes round-trip verbatim so software setup code doesn't trap.

/// Generic 16-byte register bank covering the BSC address span.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Bsc {
    pub raw: [u8; 32],
    /// Set for the **slave** SH-2. `BCR1` (`0xFFFFFFE0`) bit 15 is the SH7604
    /// MASTER/slave bit (read-only, a hardware/pin property, not software): the
    /// master reads 0, the slave reads 1. The Saturn BIOS cold-start reads it
    /// to branch — the slave skips the work-RAM init the master does at cold
    /// boot. Without modelling it, an `SSHON`-released slave re-inits WRAM and
    /// clobbers the running game. The host (Saturn) sets this after each reset.
    pub is_slave: bool,
}

impl Bsc {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read8(&self, offset: u32) -> u8 {
        let o = (offset as usize) & 0x1F;
        // `BCR1` bit 15 (the MASTER/slave bit) lands in bit 7 of this byte for a
        // longword read of `0xFFFFFFE0`; force it read-only per master/slave.
        if o == 0x02 {
            (self.raw[o] & 0x7F) | ((self.is_slave as u8) << 7)
        } else {
            self.raw[o]
        }
    }

    pub fn write8(&mut self, offset: u32, val: u8) {
        self.raw[(offset as usize) & 0x1F] = val;
    }
}
