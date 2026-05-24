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
//! M1 stores the registers and exposes [`Dmac::run_channel`] for an
//! immediate-mode synchronous transfer; autonomous cycle-stealing /
//! burst-mode triggering by external sources arrives in M2 alongside the
//! Saturn bus arbitration.

#[derive(Clone, Debug, Default)]
pub struct Channel {
    pub sar: u32,
    pub dar: u32,
    pub tcr: u32,
    pub chcr: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Dmac {
    pub channels: [Channel; 2],
    pub dmaor: u32,
}

impl Dmac {
    pub fn new() -> Self {
        Self::default()
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
