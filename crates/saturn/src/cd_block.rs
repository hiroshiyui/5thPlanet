//! Minimal CD-block command interface at `0x0589_0000..0x0589_FFFF`.
//!
//! **NOT a CD-block emulation.** The Saturn CD-block is itself a complete
//! subsystem — an SH-1 running CD-ROM firmware that handles disc reading,
//! sub-Q, error correction, audio CD playback. The real thing lands in a
//! later milestone. For M4 we model *just enough* of the host-interface
//! register protocol that BIOS init detects "a CD-block is present, no
//! disc inserted" and proceeds toward the splash instead of hanging.
//!
//! Register layout (host interface; offsets relative to `0x0589_0000`,
//! matching the Saturn CD-block / SCSP manual and Yabause's `cs2.c`).
//! Each 16-bit register occupies a 4-byte slot; a 16-bit access to either
//! halfword of the slot hits the same register, and a 32-bit read returns
//! the value duplicated in both halves.
//!
//! ```text
//!   0x0008  HIRQ        Host IRQ status     (write-AND-to-clear; 16-bit)
//!   0x000C  HIRQ_MASK   Host IRQ mask
//!   0x0018  CR1         Command/response register 1
//!   0x001C  CR2         Command/response register 2
//!   0x0020  CR3         Command/response register 3
//!   0x0024  CR4         Command/response register 4  (write triggers exec)
//!   0x8000  DATA        Data-transfer FIFO (no disc → reads 0)
//! ```
//!
//! On power-on the CD-block presents the ASCII identity `"CDBLOCK"` in
//! CR1..CR4 (`CR1=0x0043 'C'`, `CR2=0x4442 "DB"`, `CR3=0x4C4F "LO"`,
//! `CR4=0x434B "CK"`) and `HIRQ=0xFFFF`; the BIOS reads this signature to
//! detect the subsystem. Thereafter the host drives commands by writing
//! CR1..CR4 — writing CR4 latches the command (`CR1 >> 8`) and the block
//! processes it, writing a response back into CR1..CR4 and setting
//! `HIRQ.CMOK`. With no disc the status is always `NODISC`.

pub const CD_BLOCK_BASE: u32 = 0x0589_0000;
pub const CD_BLOCK_END: u32 = 0x0589_FFFF;

/// Offset of the data-transfer FIFO within the region.
const DATA_FIFO: u32 = 0x8000;

// HIRQ status bits (cs2.c).
const HIRQ_CMOK: u16 = 0x0001; // command dispatch OK / ready for next
const HIRQ_DRDY: u16 = 0x0002; // data transfer ready
const HIRQ_DCHG: u16 = 0x0020; // disc changed
const HIRQ_ESEL: u16 = 0x0040; // soft-reset / selector settings done
const HIRQ_EHST: u16 = 0x0080; // host I/O done
const HIRQ_SCDQ: u16 = 0x0400; // subcode Q decode done

// CD status codes (cs2.c).
const STAT_PAUSE: u8 = 0x01; // drive ready, disc present, not playing
const STAT_PERI: u8 = 0x20; // OR'd in for periodic (unsolicited) reports

/// SH-2 cycles between unsolicited periodic status reports. The CD-block
/// firmware emits a status report roughly once per sector period; with no
/// disc Yabause uses `_periodictiming = 50000` against a ×3 clock, i.e.
/// ~16 667 SH-2 cycles. The BIOS relies on these reports appearing (the
/// CR registers transitioning from the power-on signature to a live
/// status report) to confirm the CD-block firmware is running.
const PERIODIC_CYCLES: u64 = 16_667;

#[derive(Clone, Debug)]
pub struct CdBlock {
    pub hirq: u16,
    pub hirq_mask: u16,
    pub cr1: u16,
    pub cr2: u16,
    pub cr3: u16,
    pub cr4: u16,

    // CD status report fields (see `cd_report`). With no disc inserted
    // these stay at their power-on "nothing" values.
    status: u8,
    options: u8,
    repcnt: u8,
    ctrladdr: u8,
    track: u8,
    index: u8,
    fad: u32,
    disk_changed: bool,

    /// Global cycle of the last periodic status report (see `tick`).
    last_periodic: u64,
    /// A command's response sits in CR1..CR4 awaiting a host read; periodic
    /// reports are suppressed until then so they don't clobber it. Set when
    /// the host writes CR1 (begins a command), cleared when it reads CR4
    /// (consumes the response) — matching cs2.c's `_command` flag.
    command_pending: bool,
}

impl Default for CdBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl CdBlock {
    pub fn new() -> Self {
        Self {
            hirq: 0xFFFF,
            hirq_mask: 0xFFFF,
            // Power-on identity string "CDBLOCK" — the BIOS reads CR1..CR4
            // to confirm the CD subsystem is present.
            cr1: (0 << 8) | b'C' as u16,
            cr2: ((b'D' as u16) << 8) | b'B' as u16,
            cr3: ((b'L' as u16) << 8) | b'O' as u16,
            cr4: ((b'C' as u16) << 8) | b'K' as u16,
            // Model a disc present and ready in the tray — what every
            // emulator's "no CD image" dummy drive reports so BIOS init
            // proceeds to the splash (Yabause's dummy core returns
            // "disc present, spinning" → status PAUSE). Real no-disc and
            // CD-image handling land with the full CD-block in a later
            // milestone. Report fields match cs2.c's `Cs2Reset` for a
            // present disc: FAD 150, ctrl/addr 0x41, track 1, index 1.
            status: STAT_PAUSE,
            options: 0x00,
            repcnt: 0x00,
            ctrladdr: 0x41,
            track: 0x01,
            index: 0x01,
            fad: 150,
            disk_changed: true,
            last_periodic: 0,
            command_pending: false,
        }
    }

    /// Map an access offset to its register slot (each register occupies a
    /// 4-byte slot; both halfwords alias the same register).
    fn slot(offset: u32) -> u32 {
        offset & 0xFFFC
    }

    pub fn read16(&mut self, offset: u32) -> u16 {
        if offset & 0xFFFF >= DATA_FIFO {
            return 0; // no disc → no data
        }
        match Self::slot(offset & 0xFFFF) {
            0x0008 => self.hirq,
            0x000C => self.hirq_mask,
            0x0018 => self.cr1,
            0x001C => self.cr2,
            0x0020 => self.cr3,
            0x0024 => {
                // Reading CR4 consumes a command response; periodic
                // reports may resume (cs2.c clears `_command` here).
                self.command_pending = false;
                self.cr4
            }
            _ => 0,
        }
    }

    pub fn write16(&mut self, offset: u32, val: u16) {
        if offset & 0xFFFF >= DATA_FIFO {
            return;
        }
        match Self::slot(offset & 0xFFFF) {
            // HIRQ is write-AND-to-clear: a written 0 bit clears the flag,
            // a written 1 bit leaves it untouched (cs2.c: `HIRQ &= val`).
            0x0008 => self.hirq &= val,
            0x000C => self.hirq_mask = val,
            0x0018 => {
                // Writing CR1 begins a command and ends any periodic
                // (PERI) reporting state; periodic reports are suppressed
                // until the host reads the response (CR4).
                self.status &= !STAT_PERI;
                self.command_pending = true;
                self.cr1 = val;
            }
            0x001C => self.cr2 = val,
            0x0020 => self.cr3 = val,
            0x0024 => {
                // CR4 is the last register written; latch and execute.
                self.cr4 = val;
                self.execute();
            }
            _ => {}
        }
    }

    pub fn read8(&mut self, offset: u32) -> u8 {
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

    pub fn read32(&mut self, offset: u32) -> u32 {
        ((self.read16(offset) as u32) << 16) | self.read16(offset + 2) as u32
    }

    pub fn write32(&mut self, offset: u32, val: u32) {
        self.write16(offset, (val >> 16) as u16);
        self.write16(offset + 2, val as u16);
    }

    /// Write a standard CD status report into CR1..CR4 (cs2.c `doCDReport`).
    fn cd_report(&mut self) {
        self.cr1 = ((self.status as u16) << 8)
            | (((self.options & 0xF) as u16) << 4)
            | (self.repcnt & 0xF) as u16;
        self.cr2 = ((self.ctrladdr as u16) << 8) | self.track as u16;
        self.cr3 = ((self.index as u16) << 8) | ((self.fad >> 16) & 0xFF) as u16;
        self.cr4 = self.fad as u16;
    }

    /// Process the command latched in CR1..CR4. Real hardware runs this on
    /// the SH-1 after a timing delay then raises `HIRQ.CMOK`; we execute
    /// synchronously and set CMOK immediately, which is observationally
    /// equivalent for the BIOS (it polls HIRQ for CMOK after issuing).
    ///
    /// Only the commands BIOS init issues with no disc present are
    /// modelled; everything else falls back to a plain status report,
    /// which is what most CD-block commands return.
    fn execute(&mut self) {
        let command = (self.cr1 >> 8) as u8;
        // Clear CMOK while "processing" (cs2.c clears it at entry).
        self.hirq &= !HIRQ_CMOK;

        match command {
            0x00 => {
                // Get CD status.
                self.cd_report();
                self.hirq |= HIRQ_CMOK;
            }
            0x01 => {
                // Get hardware info: status, CD/MPEG version, drive rev.
                self.cr1 = (self.status as u16) << 8;
                self.cr2 = 0x0201; // MPEG card present / CD version
                self.cr3 = 0x0000; // MPEG not authenticated
                self.cr4 = 0x0400; // drive info / revision
                self.hirq |= HIRQ_CMOK;
            }
            0x02 => {
                // Get TOC: no disc → empty, but answer so the host moves on.
                self.cr1 = (self.status as u16) << 8;
                self.cr2 = 0x00CC;
                self.cr3 = 0x0000;
                self.cr4 = 0x0000;
                self.hirq |= HIRQ_CMOK | HIRQ_DRDY;
            }
            0x03 => {
                // Get session info.
                self.status = STAT_PAUSE;
                self.cr1 = (self.status as u16) << 8;
                self.cr2 = 0x0000;
                self.cr3 = 0xFFFF;
                self.cr4 = 0xFFFF;
                self.hirq |= HIRQ_CMOK;
            }
            0x04 => {
                // Initialize CD system: software/selector reset.
                self.cd_report();
                let mut h = self.hirq & 0xFFE5;
                if self.disk_changed {
                    h |= HIRQ_DCHG;
                } else {
                    h &= !HIRQ_DCHG;
                }
                self.hirq = h | HIRQ_CMOK | HIRQ_ESEL;
            }
            0x06 => {
                // End data transfer: no transfer pending → 0xFF count.
                self.cr1 = ((self.status as u16) << 8) | 0x00FF;
                self.cr2 = 0xFFFF;
                self.cr3 = 0x0000;
                self.cr4 = 0x0000;
                self.hirq |= HIRQ_CMOK | HIRQ_EHST;
            }
            _ => {
                // Default: most commands answer with a status report.
                self.cd_report();
                self.hirq |= HIRQ_CMOK;
            }
        }
    }

    /// Advance the CD-block's free-running clock to global cycle `now` and,
    /// once a periodic interval has elapsed with no command response
    /// outstanding, emit an unsolicited periodic status report: status
    /// gains the `PERI` flag, CR1..CR4 are refreshed via `doCDReport`, and
    /// `HIRQ.SCDQ` is raised. The BIOS watches CR1..CR4 transition from the
    /// power-on signature to a live status report to confirm the CD-block
    /// firmware is running. Called from the Saturn run loop between batches.
    pub fn tick(&mut self, now: u64) {
        if self.command_pending {
            // Don't clobber a command response the host hasn't read yet;
            // hold the periodic clock so the first report lands a full
            // interval after the response is consumed.
            self.last_periodic = now;
            return;
        }
        if now.wrapping_sub(self.last_periodic) >= PERIODIC_CYCLES {
            self.last_periodic = now;
            self.status |= STAT_PERI;
            self.cd_report();
            self.hirq |= HIRQ_SCDQ;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_on_presents_cdblock_signature() {
        let mut c = CdBlock::new();
        // "CDBLOCK" across CR1..CR4 (CR1 high byte is the status = 0).
        assert_eq!(c.read16(0x0018), (0 << 8) | b'C' as u16);
        assert_eq!(c.read16(0x001C), ((b'D' as u16) << 8) | b'B' as u16);
        assert_eq!(c.read16(0x0020), ((b'L' as u16) << 8) | b'O' as u16);
        assert_eq!(c.read16(0x0024), ((b'C' as u16) << 8) | b'K' as u16);
    }

    #[test]
    fn registers_alias_both_halfwords_of_their_slot() {
        let mut c = CdBlock::new();
        // CR1 is reachable at both 0x18 and 0x1A; HIRQ at 0x08 and 0x0A.
        assert_eq!(c.read16(0x0018), c.read16(0x001A));
        assert_eq!(c.read16(0x0008), c.read16(0x000A));
    }

    #[test]
    fn read32_duplicates_the_register_in_both_halves() {
        let mut c = CdBlock::new();
        let cr1 = c.read16(0x0018) as u32;
        assert_eq!(c.read32(0x0018), (cr1 << 16) | cr1);
    }

    #[test]
    fn hirq_is_write_and_to_clear() {
        let mut c = CdBlock::new();
        c.hirq = HIRQ_CMOK | HIRQ_DRDY | HIRQ_DCHG;
        // Clear CMOK by writing a word with CMOK = 0, others = 1.
        c.write16(0x0008, !HIRQ_CMOK);
        assert_eq!(c.hirq, HIRQ_DRDY | HIRQ_DCHG);
        // Writing all-ones clears nothing.
        c.write16(0x0008, 0xFFFF);
        assert_eq!(c.hirq, HIRQ_DRDY | HIRQ_DCHG);
    }

    #[test]
    fn get_status_command_returns_disc_present_report_and_cmok() {
        let mut c = CdBlock::new();
        c.hirq = 0;
        // Command 0x00 (Get Status): write CR1 high byte = 0x00, then CR4.
        c.write16(0x0018, 0x0000);
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000); // triggers execute
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK);
        // Disc-present PAUSE report (FAD 150, ctrl/addr 0x41, track 1,
        // index 1): CR1=0x0100, CR2=0x4101, CR3=0x0100, CR4=0x0096.
        assert_eq!(c.read16(0x0018), 0x0100);
        assert_eq!(c.read16(0x001C), 0x4101);
        assert_eq!(c.read16(0x0020), 0x0100);
        assert_eq!(c.read16(0x0024), 0x0096);
    }

    #[test]
    fn periodic_tick_refreshes_cr_with_a_peri_status_report() {
        let mut c = CdBlock::new();
        // No command pending; advance past the periodic interval.
        c.tick(PERIODIC_CYCLES + 1);
        // PERI (0x20) is OR'd into the status byte of CR1; SCDQ is raised.
        assert_eq!(c.read16(0x0018) >> 8, (STAT_PAUSE | STAT_PERI) as u16);
        assert_eq!(c.hirq & HIRQ_SCDQ, HIRQ_SCDQ);
    }

    #[test]
    fn get_hardware_info_reports_drive_revision() {
        let mut c = CdBlock::new();
        c.write16(0x0018, 0x0100); // command 0x01 in high byte
        c.write16(0x0024, 0x0000); // trigger
        assert_eq!(c.read16(0x001C), 0x0201);
        assert_eq!(c.read16(0x0024), 0x0400);
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK);
    }

    #[test]
    fn initialize_cd_system_sets_esel_and_cmok() {
        let mut c = CdBlock::new();
        c.write16(0x0018, 0x0400); // command 0x04
        c.write16(0x0024, 0x0000);
        assert_eq!(c.hirq & (HIRQ_CMOK | HIRQ_ESEL), HIRQ_CMOK | HIRQ_ESEL);
    }

    #[test]
    fn data_fifo_region_reads_zero() {
        let mut c = CdBlock::new();
        assert_eq!(c.read16(0x8000), 0);
        assert_eq!(c.read32(0x9000), 0);
    }
}
