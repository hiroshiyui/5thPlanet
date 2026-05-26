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
const HIRQ_BFUL: u16 = 0x0008; // CD buffer full
const HIRQ_DCHG: u16 = 0x0020; // disc changed
const HIRQ_ESEL: u16 = 0x0040; // soft-reset / selector settings done
const HIRQ_EHST: u16 = 0x0080; // host I/O done
const HIRQ_SCDQ: u16 = 0x0400; // subcode Q decode done

// CD status codes (cs2.c).
const STAT_PAUSE: u8 = 0x01; // drive ready, disc present, not playing
const STAT_PERI: u8 = 0x20; // OR'd in for periodic (unsolicited) reports

/// SH-2 master cycles between periodic status reports. The CD-block
/// firmware emits one report per periodic interval; with no disc playing
/// that interval is ~16.67 ms — Yabause's `_periodictiming = 50000` against
/// a µs×3 clock (50000/3 ≈ 16667 µs), i.e. one 60 Hz frame. At the 28.6 MHz
/// SH-2 master clock that is ≈476 932 cycles (matching the run loop's
/// `CYCLES_PER_FRAME`). [`CdBlock::tick`] carries the remainder across
/// intervals, so the long-run cadence averages exactly this many cycles
/// per report regardless of the sub-frame tick granularity it's driven at.
const PERIODIC_CYCLES: u64 = 476_932;

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

    /// A command's response sits in CR1..CR4 awaiting a host read; periodic
    /// reports are suppressed until then so they don't clobber it. Set when
    /// a command executes (response ready), cleared when the host reads CR4
    /// (consumes the response) — matching cs2.c's `_command` flag.
    command_pending: bool,

    /// Which of CR1..CR4 the host has written since the last command
    /// dispatch (bit 0 = CR1 … bit 3 = CR4). A command executes only once
    /// **all four** are written (`0xF`), matching MAME's HLE
    /// (`cmd_pending == 0xf`). Executing on a lone CR4 write — as we did
    /// before — falsely processes partial register pokes as commands,
    /// clobbering the power-on signature the BIOS later checks.
    cr_written: u8,

    /// Free-running accumulator (SH-2 master cycles) toward the next
    /// periodic report, advanced by [`tick`](Self::tick). Mirrors cs2.c's
    /// `_periodiccycles`: each interval crossing fires one report and the
    /// overshoot is carried forward, keeping the average cadence exact.
    periodic_accum: u64,

    /// Set once the host has issued its first command. Until then the
    /// power-on `"CDBLOCK"` signature is held in CR1..CR4 and **no**
    /// unsolicited periodic report is emitted — the BIOS reads that
    /// signature (well into boot, ~frame 19) to confirm the CD-block is
    /// present, and a periodic clobbering CR1..CR4 first derails it.
    /// Matches MAME's HLE CD block, whose `sh1_command_cb` only touches
    /// CR1..CR4 once a full command is queued (`cmd_pending == 0xf`); it
    /// emits no unsolicited periodics. (This overrides the earlier
    /// Yabause-derived "periodic from power-on" behaviour, which is what
    /// broke the signature check — see the MAME reference diff.)
    host_initialized: bool,
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
            command_pending: false,
            cr_written: 0,
            periodic_accum: 0,
            host_initialized: false,
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
            0x0008 => {
                // The CD-block recomputes the buffer/disc-state flags
                // whenever HIRQ is read and latches them (cs2.c). With no
                // data buffered, BFUL ("buffer full") is always clear.
                // DCHG ("disc changed") is re-asserted from the drive's
                // disc-changed state, so a software write-1-to-clear of
                // DCHG only sticks until the next read — without this the
                // BIOS's "wait for disc-change to clear" poll exits a frame
                // early and diverges from the reference.
                self.hirq &= !HIRQ_BFUL;
                if self.disk_changed {
                    self.hirq |= HIRQ_DCHG;
                }
                self.hirq
            }
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
                // (PERI) reporting state (matches MAME cr1_w).
                self.status &= !STAT_PERI;
                self.cr1 = val;
                self.cr_written |= 1;
            }
            0x001C => {
                self.cr2 = val;
                self.cr_written |= 2;
            }
            0x0020 => {
                self.cr3 = val;
                self.cr_written |= 4;
            }
            0x0024 => {
                // CR4 is conventionally the last register written. Only
                // dispatch once the host has written *all four* CRs
                // (`cr_written == 0xF`) — a lone CR4 poke is not a command.
                self.cr4 = val;
                self.cr_written |= 8;
                if self.cr_written == 0x0F {
                    self.cr_written = 0;
                    self.execute();
                }
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
        // The host has engaged the block; unsolicited periodic reports may
        // now run (the signature no longer needs holding — see
        // `host_initialized`). The response that follows sits in CR1..CR4
        // until the host reads CR4, so guard it from periodic clobbering.
        self.host_initialized = true;
        self.command_pending = true;
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
                // Acknowledges the disc-change (cs2.c clears isdiskchanged
                // here when a disc is present), so DCHG stops being
                // re-asserted on subsequent HIRQ reads.
                self.disk_changed = false;
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

    /// Advance the CD-block's internal clock by `cycles` SH-2 master cycles,
    /// emitting one periodic status report for each periodic interval that
    /// elapses (carrying the overshoot forward — see [`PERIODIC_CYCLES`]).
    ///
    /// This mirrors Yabause's `Cs2Exec`, which the reference drives every
    /// scanline from its main loop and which fires a report when its
    /// `_periodiccycles` accumulator crosses `_periodictiming`. Driving this
    /// *sub-frame* — as a scheduler entity ticking on a scanline granularity
    /// — rather than once at the VBlank edge lands the report at the
    /// cycle-exact point *within* the frame that the reference produces it.
    /// The BIOS's CD-firmware liveness poll is phase-sensitive to exactly
    /// when, inside the frame, CR1..CR4 flip to a live `PERI` status report,
    /// so the sub-frame phase — not just the once-per-frame cadence — has to
    /// match for the boot to track the reference.
    ///
    /// (Yabause's companion `_statuscycles` drive-status poll, which can flip
    /// a no-disc/open drive to PAUSE and flag a disc change, is a no-op for
    /// our always-present dummy disc — status is already PAUSE — so it is not
    /// modelled here. It returns when the real CD-block / disc swapping does.)
    pub fn tick(&mut self, cycles: u64) {
        self.periodic_accum += cycles;
        while self.periodic_accum >= PERIODIC_CYCLES {
            self.periodic_accum -= PERIODIC_CYCLES;
            self.emit_periodic();
        }
    }

    /// Emit one unsolicited periodic status report: the status gains the
    /// `PERI` flag, CR1..CR4 are refreshed via `doCDReport`, and `HIRQ.SCDQ`
    /// is raised. The BIOS watches CR1..CR4 transition from the power-on
    /// signature to a live status report to confirm the CD-block firmware is
    /// running. Suppressed while a command response is still unread so it
    /// doesn't clobber CR1..CR4 — matching cs2.c, which still decrements its
    /// periodic accumulator (the cadence keeps ticking) but returns before
    /// the report when `_command` is set.
    fn emit_periodic(&mut self) {
        // Hold the power-on signature until the host has engaged the block
        // with a command (see `host_initialized`); and never clobber an
        // unread command response (`command_pending`, cs2.c's `_command`).
        if !self.host_initialized || self.command_pending {
            return;
        }
        self.status |= STAT_PERI;
        self.cd_report();
        self.hirq |= HIRQ_SCDQ;
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
    fn signature_held_until_first_command() {
        // The power-on "CDBLOCK" signature must survive many periodic
        // intervals — no unsolicited periodic clobbers CR1..CR4 before the
        // host engages the block (the BIOS reads the signature ~frame 19 to
        // confirm the CD subsystem; a periodic there derails boot). Matches
        // MAME's HLE CD block, which emits no unsolicited periodics.
        let mut c = CdBlock::new();
        for _ in 0..10 {
            c.tick(PERIODIC_CYCLES);
        }
        assert_eq!(c.read16(0x0018), (0 << 8) | b'C' as u16);
        assert_eq!(c.read16(0x001C), ((b'D' as u16) << 8) | b'B' as u16);
        assert_eq!(c.read16(0x0020), ((b'L' as u16) << 8) | b'O' as u16);
        assert_eq!(c.read16(0x0024), ((b'C' as u16) << 8) | b'K' as u16);
        // Status never gained the PERI flag — no periodic report ran.
        assert_eq!(
            c.read16(0x0018) >> 8,
            0,
            "CR1 status byte still 0 (no PERI)"
        );
    }

    /// Engage the block with a Get Status command and consume the response,
    /// leaving `host_initialized` set so periodics may run.
    fn activated() -> CdBlock {
        let mut c = CdBlock::new();
        // A command requires all four CRs written (command 0x00 = Get Status).
        c.write16(0x0018, 0x0000);
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000); // CR4 completes the set → execute
        let _ = c.read16(0x0024); // consume response (clears command_pending)
        c
    }

    #[test]
    fn periodic_fires_after_the_first_command() {
        let mut c = activated();
        c.tick(PERIODIC_CYCLES);
        // PERI (0x20) is OR'd into the status byte of CR1; SCDQ is raised.
        assert_eq!(c.read16(0x0018) >> 8, (STAT_PAUSE | STAT_PERI) as u16);
        assert_eq!(c.hirq & HIRQ_SCDQ, HIRQ_SCDQ);
    }

    #[test]
    fn periodic_report_only_fires_once_the_interval_elapses() {
        let mut c = activated();
        let cr1_cmd = c.read16(0x0018); // command status report (no PERI yet)
        // A partial interval accumulates but emits nothing yet.
        c.tick(PERIODIC_CYCLES - 1);
        assert_eq!(c.read16(0x0018), cr1_cmd);
        // One more cycle crosses the interval; the report lands. The
        // accumulator carries the overshoot forward (cadence stays exact).
        c.tick(1);
        assert_eq!(c.read16(0x0018) >> 8, (STAT_PAUSE | STAT_PERI) as u16);
    }

    #[test]
    fn periodic_cadence_is_independent_of_tick_granularity() {
        // Ticking one interval in many small sub-frame steps fires exactly
        // one report — the accumulator, not the call count, drives cadence.
        let mut fine = activated();
        let step = PERIODIC_CYCLES / 263; // ~one scanline
        let mut acc = 0;
        while acc < PERIODIC_CYCLES {
            fine.tick(step);
            acc += step;
        }
        // Exactly one PERI report so far (status byte has PERI, SCDQ set).
        assert_eq!(fine.read16(0x0018) >> 8, (STAT_PAUSE | STAT_PERI) as u16);
        assert_eq!(fine.hirq & HIRQ_SCDQ, HIRQ_SCDQ);
    }

    #[test]
    fn periodic_tick_is_suppressed_while_a_command_response_is_unread() {
        let mut c = CdBlock::new();
        // Issue a command (CR1 write sets command_pending); the response
        // must not be clobbered by a periodic report until CR4 is read.
        c.write16(0x0018, 0x0000);
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000); // execute Get Status → response in CR1..4
        let cr1_after_cmd = c.read16(0x0018);
        c.tick(PERIODIC_CYCLES); // should be suppressed (CR4 not yet read)
        assert_eq!(
            c.read16(0x0018),
            cr1_after_cmd,
            "response held until CR4 read"
        );
        // Read CR4 (consumes response), then a periodic may land.
        let _ = c.read16(0x0024);
        c.tick(PERIODIC_CYCLES);
        assert_eq!(c.read16(0x0018) >> 8, (STAT_PAUSE | STAT_PERI) as u16);
    }

    #[test]
    fn get_hardware_info_reports_drive_revision() {
        let mut c = CdBlock::new();
        c.write16(0x0018, 0x0100); // command 0x01 in high byte
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000); // CR4 completes the set → trigger
        assert_eq!(c.read16(0x001C), 0x0201);
        assert_eq!(c.read16(0x0024), 0x0400);
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK);
    }

    #[test]
    fn initialize_cd_system_sets_esel_and_cmok() {
        let mut c = CdBlock::new();
        c.write16(0x0018, 0x0400); // command 0x04
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000); // CR4 completes the set
        assert_eq!(c.hirq & (HIRQ_CMOK | HIRQ_ESEL), HIRQ_CMOK | HIRQ_ESEL);
    }

    #[test]
    fn data_fifo_region_reads_zero() {
        let mut c = CdBlock::new();
        assert_eq!(c.read16(0x8000), 0);
        assert_eq!(c.read32(0x9000), 0);
    }
}
