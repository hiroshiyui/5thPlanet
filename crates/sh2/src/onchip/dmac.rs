//! SH7604 on-chip DMA controller (DMAC). Two channels (0 and 1).
//!
//! Per-channel register map (channel 0 at offsets shown, channel 1 at +0x10):
//!
//! ```text
//!   SAR0  (FFFFFF80, 32-bit)  source address
//!   DAR0  (FFFFFF84, 32-bit)  destination address
//!   TCR0  (FFFFFF88, 32-bit)  transfer count (low 24 bits valid)
//!   CHCR0 (FFFFFF8C, 32-bit)  channel control
//! ```
//!
//! Plus the per-controller registers:
//!
//! ```text
//!   VCRDMA0 (FFFFFFA0)        ch0 interrupt vector
//!   VCRDMA1 (FFFFFFA8)        ch1 interrupt vector
//!   DMAOR   (FFFFFFB0)        DMA operation register (DME enable, NMIF, AE)
//! ```
//!
//! This module stores the registers and decodes the control bits; the actual
//! transfer engine lives in [`crate::interpreter::Cpu`] (`run_dma`), which has
//! the bus handle and runs any enabled channel to completion after each
//! instruction step (M13 D1 — modelled on Mednafen `DMA_DoTransfer`).
//! Cycle-stealing / burst bus arbitration is a later timing refinement.

/// One DMAC channel's registers (SAR/DAR/TCR/CHCR).
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Channel {
    pub sar: u32,
    pub dar: u32,
    pub tcr: u32,
    pub chcr: u32,
}

/// The SH7604 on-chip DMA controller: two [`Channel`]s. See the module header
/// for the per-channel register map.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Dmac {
    pub channels: [Channel; 2],
    pub dmaor: u32,
    /// Per-channel "TE set by hardware but not yet read" shadow (Mednafen
    /// `CHCRM`, bit 1). CHCR.TE is **clear-only**: software can never set it,
    /// and a write of 0 clears it only once a CHCR read has observed it (the
    /// read clears this shadow). Set alongside TE at transfer end.
    pub chcrm: [u32; 2],
    /// DMAOR counterpart (Mednafen `DMAORM`): bit 2 shadows a hardware-set AE
    /// until a DMAOR read. NMIF (bit 1) is set without a shadow (Mednafen
    /// `SetNMI`), so it is clearable by any write of 0.
    pub dmaorm: u32,
}

impl Dmac {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hardware-latch CHCR.TE (transfer end) on channel `ch`, protecting it
    /// from a write-0 clear until software reads CHCR (Mednafen
    /// `DMACH[ch].CHCRM |= 2`).
    pub fn latch_te(&mut self, ch: usize) {
        self.channels[ch].chcr |= 0b10;
        self.chcrm[ch] |= 0b10;
    }

    /// Hardware-latch DMAOR.AE (address error), protected like TE (Mednafen
    /// `DMAORM |= 4`).
    pub fn latch_ae(&mut self) {
        self.dmaor |= 0b100;
        self.dmaorm |= 0b100;
    }

    /// Software write of CHCR (Mednafen `OnChipRegWrite` 0x8C/0x9C):
    /// `CHCR = (V & ~2) | (CHCR & (V | CHCRM) & 2)` — TE can only be cleared,
    /// and only by writing 0 after a read observed it.
    pub fn write_chcr(&mut self, ch: usize, v: u32) {
        let c = &mut self.channels[ch];
        c.chcr = (v & !2) | (c.chcr & (v | self.chcrm[ch]) & 2);
    }

    /// Software write of DMAOR (Mednafen `OnChipRegWrite` 0xB0):
    /// `DMAOR = (V & 9) | (DMAOR & (V | DMAORM) & 6)` — DME/PR are plain
    /// read/write; NMIF/AE are clear-only (write-0-after-read-1).
    pub fn write_dmaor(&mut self, v: u32) {
        self.dmaor = (v & 0b1001) | (self.dmaor & (v | self.dmaorm) & 0b0110);
    }

    /// Master enable (DMAOR.DME, bit 0) AND no fault bits set.
    pub fn enabled(&self) -> bool {
        self.dmaor & 0b0111 == 0b0001
    }

    /// Per-channel enable (CHCR.DE, bit 0).
    pub fn channel_enabled(&self, ch: usize) -> bool {
        self.channels[ch].chcr & 1 != 0
    }

    /// Transfer size encoded in CHCR.TS (bits 11..10):
    /// 00=byte, 01=word(16), 10=long(32), 11=16-byte block.
    pub fn channel_size_bytes(&self, ch: usize) -> u32 {
        match (self.channels[ch].chcr >> 10) & 0b11 {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => 16,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_requires_dme_and_no_fault_bits() {
        let mut d = Dmac::new();
        assert!(!d.enabled(), "DME clear");
        d.write_dmaor(0b0001);
        assert!(d.enabled(), "DME set, no fault");
        d.latch_ae();
        assert!(!d.enabled(), "AE blocks the master enable");
        d.dmaor = 0b0011; // NMIF (set by the NMI path, not software)
        assert!(!d.enabled(), "NMIF blocks the master enable");
        d.dmaor = 0b1001; // PR is not a fault bit
        assert!(d.enabled(), "PR does not block");
    }

    #[test]
    fn channel_enable_and_transfer_size_decode() {
        let mut d = Dmac::new();
        assert!(!d.channel_enabled(0));
        d.write_chcr(1, 1);
        assert!(d.channel_enabled(1));
        assert!(!d.channel_enabled(0));
        for (ts, bytes) in [(0u32, 1u32), (1, 2), (2, 4), (3, 16)] {
            d.write_chcr(0, ts << 10);
            assert_eq!(d.channel_size_bytes(0), bytes, "TS={ts}");
        }
    }

    #[test]
    fn chcr_te_is_clear_only_after_a_read() {
        // Mednafen: CHCR = (V & ~2) | (CHCR & (V | CHCRM) & 2).
        let mut d = Dmac::new();
        d.write_chcr(0, 0b11); // software tries to set TE
        assert_eq!(d.channels[0].chcr & 2, 0, "TE not software-settable");
        d.latch_te(0);
        assert_eq!(d.channels[0].chcr & 2, 2, "hardware sets TE");
        d.write_chcr(0, 0b01); // write 0 to TE WITHOUT having read it
        assert_eq!(d.channels[0].chcr & 2, 2, "unread TE survives a write-0");
        d.chcrm[0] = 0; // what a CHCR read does
        d.write_chcr(0, 0b11); // writing 1 keeps it
        assert_eq!(d.channels[0].chcr & 2, 2, "write-1 keeps TE");
        d.write_chcr(0, 0b01); // write 0 after the read clears it
        assert_eq!(d.channels[0].chcr & 2, 0, "read-then-write-0 clears TE");
        assert_eq!(d.channels[0].chcr & 1, 1, "other bits written normally");
    }

    #[test]
    fn dmaor_nmif_ae_are_clear_only() {
        // Mednafen: DMAOR = (V & 9) | (DMAOR & (V | DMAORM) & 6).
        let mut d = Dmac::new();
        d.write_dmaor(0b1111);
        assert_eq!(d.dmaor, 0b1001, "NMIF/AE not software-settable");
        d.latch_ae();
        d.write_dmaor(0b0001);
        assert_eq!(d.dmaor & 0b100, 0b100, "unread AE survives a write-0");
        d.dmaorm = 0; // a DMAOR read
        d.write_dmaor(0b0001);
        assert_eq!(d.dmaor, 0b0001, "read-then-write-0 clears AE");
        // NMIF has no protection shadow: any write of 0 clears it.
        d.dmaor |= 0b010;
        d.write_dmaor(0b0001);
        assert_eq!(d.dmaor & 0b010, 0, "NMIF cleared by write-0");
    }
}
