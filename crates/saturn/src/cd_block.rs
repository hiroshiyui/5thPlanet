//! Minimal CD-block presence stub at `0x0589_8000..0x0589_FFFF`.
//!
//! **NOT a CD-block emulation.** The Saturn CD-block is itself a
//! complete subsystem — an SH-1 running CD-ROM firmware that handles
//! disc reading, sub-Q, error correction, audio CD playback. M5+ will
//! land that. For M4 we provide *just enough* of the register-level
//! interface that BIOS init code doesn't hang waiting on reads from a
//! missing peripheral.
//!
//! Register layout (from the Saturn CD-block / SCSI interface manual):
//!
//! ```text
//!   0x0000  HIRQ        Host IRQ Status      (W1C; 16-bit)
//!   0x000C  HIRQ_MASK   Host IRQ Mask        (16-bit)
//!   0x0018  CR1         Command Register 1   (high word of cmd / response)
//!   0x001C  CR2         Command Register 2
//!   0x0020  CR3         Command Register 3
//!   0x0024  CR4         Command Register 4
//! ```
//!
//! HIRQ bits: 0 = CMOK (command processed), 1 = DRDY (data ready),
//! 2 = CSCT (sector ready), 3 = BFUL, 4 = PEND, 5 = DCHG (disc
//! change), 6 = ESEL, 7 = EHST, …
//!
//! M4 initial state: HIRQ = 0x0007 (CMOK | DRDY | DCHG) — "command
//! ready to accept, data path ready, recent disc change". CR4 holds
//! a "drive present, no disc" status code that the BIOS read sees.

pub const CD_BLOCK_BASE: u32 = 0x0589_8000;
pub const CD_BLOCK_END: u32 = 0x0589_FFFF;

/// HIRQ.CMOK — command processed. Set whenever a CR write is observed.
const HIRQ_CMOK: u16 = 0x0001;
const HIRQ_DRDY: u16 = 0x0002;
const HIRQ_DCHG: u16 = 0x0020;

#[derive(Clone, Debug)]
pub struct CdBlock {
    pub hirq: u16,
    pub hirq_mask: u16,
    pub cr1: u16,
    pub cr2: u16,
    pub cr3: u16,
    pub cr4: u16,
}

impl Default for CdBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl CdBlock {
    pub fn new() -> Self {
        Self {
            hirq: HIRQ_CMOK | HIRQ_DRDY | HIRQ_DCHG,
            hirq_mask: 0xFFFF,
            cr1: 0x0000,
            // "Drive busy" status in low byte initially; once first
            // command is processed BIOS sees CMOK and reads CR1..4.
            cr2: 0x0000,
            cr3: 0x0000,
            // CR4 reports a "drive present, no disc" status code that
            // BIOS reads to know there's a drive but no media. The
            // exact value isn't critical — BIOS just needs a non-zero
            // word here to register the drive's existence.
            cr4: 0x0400,
        }
    }

    pub fn read16(&self, offset: u32) -> u16 {
        match offset & 0xFFFF {
            0x0000 => self.hirq,
            0x000C => self.hirq_mask,
            0x0018 => self.cr1,
            0x001C => self.cr2,
            0x0020 => self.cr3,
            0x0024 => self.cr4,
            _ => 0,
        }
    }

    pub fn write16(&mut self, offset: u32, val: u16) {
        match offset & 0xFFFF {
            // HIRQ is write-1-to-clear: software writes the bits it
            // wants to acknowledge.
            0x0000 => self.hirq &= !val,
            0x000C => self.hirq_mask = val,
            0x0018 => {
                self.cr1 = val;
                self.command_received();
            }
            0x001C => self.cr2 = val,
            0x0020 => self.cr3 = val,
            0x0024 => {
                // CR4 is the last register written in a command
                // sequence; latch CMOK on it.
                self.cr4 = val;
                self.command_received();
            }
            _ => {}
        }
    }

    pub fn read8(&self, offset: u32) -> u8 {
        let w = self.read16(offset & !1);
        if offset & 1 == 0 {
            (w >> 8) as u8
        } else {
            w as u8
        }
    }

    pub fn write8(&mut self, offset: u32, val: u8) {
        let aligned = offset & !1;
        let cur = self.read16(aligned);
        let new = if offset & 1 == 0 {
            (cur & 0x00FF) | ((val as u16) << 8)
        } else {
            (cur & 0xFF00) | val as u16
        };
        self.write16(aligned, new);
    }

    pub fn read32(&self, offset: u32) -> u32 {
        ((self.read16(offset) as u32) << 16) | self.read16(offset + 2) as u32
    }

    pub fn write32(&mut self, offset: u32, val: u32) {
        self.write16(offset, (val >> 16) as u16);
        self.write16(offset + 2, val as u16);
    }

    /// Latch HIRQ.CMOK after a command write. The real CD-block would
    /// process the command on its SH-1 and only set CMOK after; for
    /// M4 we ack synchronously since we don't actually do anything
    /// with the command bytes.
    fn command_received(&mut self) {
        self.hirq |= HIRQ_CMOK;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_hirq_signals_cmok_drdy_dchg() {
        let c = CdBlock::new();
        assert_eq!(c.hirq, HIRQ_CMOK | HIRQ_DRDY | HIRQ_DCHG);
    }

    #[test]
    fn cr_writes_set_cmok_in_hirq() {
        let mut c = CdBlock::new();
        c.hirq = 0; // pretend host has W1C'd everything
        c.write16(0x0018, 0x1234);
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK);
    }

    #[test]
    fn hirq_is_write_one_to_clear() {
        let mut c = CdBlock::new();
        c.hirq = HIRQ_CMOK | HIRQ_DRDY | HIRQ_DCHG;
        c.write16(0x0000, HIRQ_CMOK);
        assert_eq!(c.hirq, HIRQ_DRDY | HIRQ_DCHG);
        // Writing 0 doesn't clear anything.
        c.write16(0x0000, 0);
        assert_eq!(c.hirq, HIRQ_DRDY | HIRQ_DCHG);
    }

    #[test]
    fn cr_registers_round_trip() {
        let mut c = CdBlock::new();
        c.write16(0x0018, 0xAAAA);
        c.write16(0x001C, 0xBBBB);
        c.write16(0x0020, 0xCCCC);
        c.write16(0x0024, 0xDDDD);
        assert_eq!(c.read16(0x0018), 0xAAAA);
        assert_eq!(c.read16(0x001C), 0xBBBB);
        assert_eq!(c.read16(0x0020), 0xCCCC);
        assert_eq!(c.read16(0x0024), 0xDDDD);
    }

    #[test]
    fn byte_reads_split_word_correctly() {
        let mut c = CdBlock::new();
        c.write16(0x0018, 0x1234);
        assert_eq!(c.read8(0x0018), 0x12);
        assert_eq!(c.read8(0x0019), 0x34);
    }

    #[test]
    fn unmapped_offsets_read_zero() {
        let c = CdBlock::new();
        assert_eq!(c.read16(0x1000), 0);
        assert_eq!(c.read32(0x2000), 0);
    }
}
