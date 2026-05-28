//! System Manager and Peripheral Control (SMPC) — Saturn-side power
//! manager, sub-CPU controller, and peripheral I/O front-end.
//!
//! Lives in the `0x0010_0000..=0x0017_FFFF` region. Software talks to
//! it through a register bank at *odd* byte offsets (every other byte
//! is reserved). The general flow for a command is:
//!
//! 1. Software writes any required inputs to IREG0..IREG6.
//! 2. Software writes the command code to COMREG.
//! 3. SMPC sets SF (status flag) to 1 to indicate "busy".
//! 4. The host (Saturn aggregate) picks up the queued command via
//!    [`Smpc::take_pending`] and performs the side effect — releasing
//!    the slave CPU, hold the slave, etc. — then clears SF to 0.
//! 5. Software polls SF until it reads 0 and then inspects OREG0..31
//!    and SR for the response.
//!
//! M3 implements the register surface plus the slave-control commands
//! (`SSHON` / `SSHOFF`); other commands are recognised, queued, and
//! treated as immediate no-ops so BIOS init code doesn't deadlock.
//! `INTBACK` peripheral-data return is a placeholder that reports "no
//! controller connected" — full peripheral protocol arrives in M4
//! alongside the SDL2 keyboard binding.
//!
//! Register layout (offsets from `SMPC_BASE = 0x0010_0000`):
//!
//! ```text
//!   0x01  IREG0           0x21  OREG0
//!   0x03  IREG1           0x23  OREG1
//!   ...                    ...
//!   0x0D  IREG6           0x5F  OREG31
//!   0x1F  COMREG          0x61  SR    status register
//!                         0x63  SF    status flag (busy when 1)
//! ```

/// COMREG command codes that this module recognises. Discriminants
/// match the hardware-defined byte values so `cmd as u8` round-trips
/// with [`Command::from_raw`]. Unknown codes are recorded into
/// `last_unknown_command` for trace tooling and otherwise ignored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum Command {
    /// MSHON — request master SH-2 on. No-op for us (master is always on).
    MshOn = 0x00,
    /// SSHON — release slave SH-2 from halt.
    SshOn = 0x02,
    /// SSHOFF — halt the slave SH-2.
    SshOff = 0x03,
    /// SNDON / SNDOFF — sound subsystem control. M3 no-op.
    SndOn = 0x06,
    SndOff = 0x07,
    /// CDON / CDOFF — CD block control. M3 no-op.
    CdOn = 0x08,
    CdOff = 0x09,
    /// SYSRES — system reset. M3 no-op; M4+ will route to Saturn::reset().
    SysRes = 0x0D,
    /// CKCHG352 / CKCHG320 — clock change (PAL / NTSC). M3 no-op.
    CkChg352 = 0x0E,
    CkChg320 = 0x0F,
    /// INTBACK — peripheral data fetch. M4 returns "no controller"
    /// (see [`crate::system::Saturn`] INTBACK handling).
    IntBack = 0x10,
    /// SETTIME — set the RTC from IREG0..6 (see `system` command handling).
    SetTime = 0x16,
    /// SETSMEM — write the four SMPC-backup-memory (SMEM) bytes from IREG0..3.
    SetSMem = 0x17,
    /// NMIREQ — assert NMI to master SH-2. Routed via INTC.
    NmiReq = 0x18,
    /// RESENAB / RESDISA — reset button enable/disable. M3 no-op.
    ResEnab = 0x19,
    ResDisa = 0x1A,
}

impl Command {
    /// Decode the raw COMREG byte into a known command, or `None`.
    pub fn from_raw(code: u8) -> Option<Self> {
        Some(match code {
            0x00 => Self::MshOn,
            0x02 => Self::SshOn,
            0x03 => Self::SshOff,
            0x06 => Self::SndOn,
            0x07 => Self::SndOff,
            0x08 => Self::CdOn,
            0x09 => Self::CdOff,
            0x0D => Self::SysRes,
            0x0E => Self::CkChg352,
            0x0F => Self::CkChg320,
            0x10 => Self::IntBack,
            0x16 => Self::SetTime,
            0x17 => Self::SetSMem,
            0x18 => Self::NmiReq,
            0x19 => Self::ResEnab,
            0x1A => Self::ResDisa,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Smpc {
    pub ireg: [u8; 7],
    pub oreg: [u8; 32],
    pub comreg: u8,
    pub sr: u8,
    pub sf: u8,
    pub pdr1: u8,
    pub pdr2: u8,
    pub ddr1: u8,
    pub ddr2: u8,
    pub iosel: u8,
    pub exle: u8,
    /// Last COMREG byte that didn't decode to a known command. Set on
    /// write; never cleared automatically. Useful for trace tooling.
    pub last_unknown_command: Option<u8>,
    /// Command queued by a write to COMREG, waiting for the Saturn
    /// aggregate to process it. SF stays at 1 while this is `Some`.
    pending: Option<Command>,
    /// INTBACK has a non-trivial execution time: the SMPC keeps SF busy for
    /// a request-dependent interval (see `system::intback_busy_us`) before it
    /// fills OREG, clears SF, and raises its interrupt. While a dequeued
    /// INTBACK is still "executing", this holds the global cycle at which it
    /// completes; the Saturn aggregate finishes it once `now()` passes it.
    /// Clearing SF immediately makes the BIOS's SF-poll return too early and
    /// derail the boot, so the delay must be non-zero. (The exact duration is
    /// reference-derived and unverified — see the `REVIEW(magic)` on
    /// `intback_busy_us`.)
    pub intback_complete_at: Option<u64>,
    /// INTBACK staged-peripheral protocol state (matches MAME `smpc.cpp`).
    /// 0 = not in an INTBACK peripheral sequence; non-zero = a peripheral
    /// transfer phase is in progress (the value drives the next `SR`). The
    /// status phase sets it to `(IREG1 & 8) >> 3`; each CONTINUE advances it
    /// (1 → 2 → 0), and BREAK clears it.
    pub intback_stage: u8,
    /// Pad mode echoed back in the peripheral-phase `SR` (`IREG0 >> 4`).
    pub pmode: u8,
    /// Set when the host writes IREG0 with the CONTINUE bit (0x80) during an
    /// INTBACK peripheral sequence; the Saturn aggregate drains it to run the
    /// next peripheral phase. Mirrors MAME scheduling `intback_continue_request`.
    intback_continue: bool,
    /// Port-1 digital-pad state as a *pressed* mask (1 = held), using the
    /// [`pad`] bit positions. The INTBACK peripheral phase reports the
    /// active-low inverse; default 0 = nothing pressed. The frontend sets it.
    pub pad1: u16,
    /// Real-time clock value, in seconds since the Unix epoch, as of
    /// `rtc_set_cycle`. The live time is this plus the emulated seconds
    /// elapsed since (see [`Smpc::rtc_oreg`]). Set by the `SETTIME` command or
    /// [`Smpc::set_rtc_unix`]; defaults to a fixed date so the core is
    /// deterministic (the frontend can inject the host clock).
    rtc_secs: u64,
    /// Global cycle at which `rtc_secs` was last synced.
    rtc_set_cycle: u64,
    /// Area (region) code reported in INTBACK OREG9. 0x04 = North America
    /// (NTSC); the BIOS halts on a mismatch with its build region.
    pub region: u8,
    /// SMPC backup memory (`SMEM`) — 4 bytes echoed in INTBACK OREG12..15 and
    /// written by the `SETSMEM` command.
    pub smem: [u8; 4],
}

/// Standard digital-pad button bits for [`Smpc::pad1`] (1 = pressed). The high
/// byte is the SMPC's first data byte, the low byte the second (active-low on
/// the wire — the report inverts this mask).
pub mod pad {
    pub const B: u16 = 1 << 15;
    pub const C: u16 = 1 << 14;
    pub const A: u16 = 1 << 13;
    pub const START: u16 = 1 << 12;
    pub const UP: u16 = 1 << 11;
    pub const DOWN: u16 = 1 << 10;
    pub const LEFT: u16 = 1 << 9;
    pub const RIGHT: u16 = 1 << 8;
    pub const L: u16 = 1 << 7;
    pub const Z: u16 = 1 << 6;
    pub const Y: u16 = 1 << 5;
    pub const X: u16 = 1 << 4;
    pub const R: u16 = 1 << 3;
}

/// Region code for INTBACK OREG9 (SMPC manual area-code table).
pub mod region {
    pub const JAPAN: u8 = 0x01;
    pub const ASIA_NTSC: u8 = 0x02;
    pub const NORTH_AMERICA: u8 = 0x04;
    pub const EUROPE_PAL: u8 = 0x0C;
}

/// Days from the proleptic-Gregorian civil date to 1970-01-01 (Howard
/// Hinnant's `days_from_civil`). Valid across the RTC's whole range.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil `(year, month, day)` for `z` days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[inline]
fn to_bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}
#[inline]
fn from_bcd(v: u8) -> u32 {
    ((v >> 4) as u32) * 10 + (v & 0x0F) as u32
}

/// Encode `unix_secs` as the seven INTBACK RTC bytes (OREG1..7): year-hi/lo
/// BCD, `(weekday<<4)|month` (weekday 0=Sun, month 1..12 in the low nibble),
/// then day/hour/minute/second BCD.
fn rtc_bytes(unix_secs: u64) -> [u8; 7] {
    let days = (unix_secs / 86_400) as i64;
    let tod = unix_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    // 1970-01-01 was a Thursday (4, with Sunday = 0).
    let weekday = (days + 4).rem_euclid(7) as u8;
    [
        to_bcd((y / 100) as u32),
        to_bcd((y % 100) as u32),
        (weekday << 4) | (m as u8 & 0x0F),
        to_bcd(d as u32),
        to_bcd((tod / 3600) as u32),
        to_bcd((tod % 3600 / 60) as u32),
        to_bcd((tod % 60) as u32),
    ]
}

impl Smpc {
    pub fn new() -> Self {
        Self {
            region: region::NORTH_AMERICA,
            // Deterministic default clock: 1996-01-01 00:00:00. The frontend
            // overrides it with the host time via `set_rtc_unix`.
            rtc_secs: days_from_civil(1996, 1, 1) as u64 * 86_400,
            ..Default::default()
        }
    }

    /// Set the RTC to `unix_secs` (seconds since 1970-01-01), syncing the
    /// advance origin to the current global cycle `now`.
    pub fn set_rtc_unix(&mut self, unix_secs: u64, now: u64) {
        self.rtc_secs = unix_secs;
        self.rtc_set_cycle = now;
    }

    /// Set the RTC from the seven `SETTIME` IREG bytes (same layout as the
    /// INTBACK RTC bytes), syncing the advance origin to `now`.
    pub fn set_rtc_bcd(&mut self, t: [u8; 7], now: u64) {
        let year = from_bcd(t[0]) as i64 * 100 + from_bcd(t[1]) as i64;
        let month = (t[2] & 0x0F) as i64;
        let day = from_bcd(t[3]) as i64;
        // A nonsense date (e.g. an all-zero IREG → year 0) yields negative
        // days; clamp so garbage input can't overflow rather than panic.
        let days = days_from_civil(year, month.max(1), day.max(1)).max(0) as u64;
        let secs = days * 86_400
            + from_bcd(t[4]) as u64 * 3600
            + from_bcd(t[5]) as u64 * 60
            + from_bcd(t[6]) as u64;
        self.set_rtc_unix(secs, now);
    }

    /// The current RTC as the seven INTBACK OREG bytes, advancing the stored
    /// value by the emulated seconds elapsed since it was set
    /// (`cycles_per_second` is the master-clock rate).
    pub fn rtc_oreg(&self, now: u64, cycles_per_second: u64) -> [u8; 7] {
        let elapsed = now.saturating_sub(self.rtc_set_cycle) / cycles_per_second.max(1);
        rtc_bytes(self.rtc_secs + elapsed)
    }

    pub fn read8(&self, offset: u32) -> u8 {
        match offset {
            0x01 => self.ireg[0],
            0x03 => self.ireg[1],
            0x05 => self.ireg[2],
            0x07 => self.ireg[3],
            0x09 => self.ireg[4],
            0x0B => self.ireg[5],
            0x0D => self.ireg[6],
            0x1F => self.comreg,
            o if (0x21..=0x5F).contains(&o) && (o & 1) == 1 => self.oreg[((o - 0x21) / 2) as usize],
            0x61 => self.sr,
            0x63 => self.sf,
            0x75 => self.pdr1,
            0x77 => self.pdr2,
            0x79 => self.ddr1,
            0x7B => self.ddr2,
            0x7D => self.iosel,
            0x7F => self.exle,
            _ => 0,
        }
    }

    pub fn write8(&mut self, offset: u32, val: u8) {
        match offset {
            0x01 => {
                self.ireg[0] = val;
                // During an INTBACK peripheral sequence, a write to IREG0
                // is the host's CONTINUE/BREAK request (MAME `ireg_w`,
                // `offset == 1`): bit 0x40 = BREAK (end now), bit 0x80 =
                // CONTINUE (fetch the next peripheral phase).
                if self.intback_stage != 0 {
                    if val & 0x40 != 0 {
                        self.sr &= 0x0F; // BREAK: ack, end the sequence
                        self.intback_stage = 0;
                    } else if val & 0x80 != 0 {
                        self.intback_continue = true;
                        self.sf = 1; // busy until the phase completes
                    }
                }
            }
            0x03 => self.ireg[1] = val,
            0x05 => self.ireg[2] = val,
            0x07 => self.ireg[3] = val,
            0x09 => self.ireg[4] = val,
            0x0B => self.ireg[5] = val,
            0x0D => self.ireg[6] = val,
            0x1F => {
                self.comreg = val;
                self.queue_command(val);
            }
            o if (0x21..=0x5F).contains(&o) && (o & 1) == 1 => {
                self.oreg[((o - 0x21) / 2) as usize] = val;
            }
            0x61 => self.sr = val,
            0x63 => self.sf = val,
            0x75 => self.pdr1 = val,
            0x77 => self.pdr2 = val,
            0x79 => self.ddr1 = val,
            0x7B => self.ddr2 = val,
            0x7D => self.iosel = val,
            0x7F => self.exle = val,
            _ => {}
        }
    }

    pub fn read16(&self, offset: u32) -> u16 {
        ((self.read8(offset) as u16) << 8) | self.read8(offset + 1) as u16
    }
    pub fn write16(&mut self, offset: u32, val: u16) {
        self.write8(offset, (val >> 8) as u8);
        self.write8(offset + 1, val as u8);
    }
    pub fn read32(&self, offset: u32) -> u32 {
        ((self.read8(offset) as u32) << 24)
            | ((self.read8(offset + 1) as u32) << 16)
            | ((self.read8(offset + 2) as u32) << 8)
            | self.read8(offset + 3) as u32
    }
    pub fn write32(&mut self, offset: u32, val: u32) {
        self.write8(offset, (val >> 24) as u8);
        self.write8(offset + 1, (val >> 16) as u8);
        self.write8(offset + 2, (val >> 8) as u8);
        self.write8(offset + 3, val as u8);
    }

    /// Called from the COMREG write path: decode + enqueue + raise SF.
    fn queue_command(&mut self, raw: u8) {
        match Command::from_raw(raw) {
            Some(cmd) => {
                self.pending = Some(cmd);
                self.sf = 1;
            }
            None => {
                self.last_unknown_command = Some(raw);
            }
        }
    }

    /// Pop a pending INTBACK CONTINUE request (set when the host wrote the
    /// CONTINUE bit to IREG0 mid-sequence). The Saturn aggregate runs the
    /// next peripheral phase when this returns `true`.
    pub fn take_intback_continue(&mut self) -> bool {
        core::mem::take(&mut self.intback_continue)
    }

    /// Pop the queued command, if any. The caller is expected to apply
    /// its side effect and then either let SF drop naturally on the
    /// next [`mark_command_done`] or call it explicitly.
    pub fn take_pending(&mut self) -> Option<Command> {
        self.pending.take()
    }

    /// Signal that the last queued command has finished — clears SF so
    /// software polling sees "not busy". `INTBACK` may also want to
    /// populate OREG0..31 first; the caller does that before calling.
    pub fn mark_command_done(&mut self) {
        self.sf = 0;
    }

    /// Resolve a pending INTBACK against the current global cycle: once
    /// the command's execution time has elapsed (`intback_complete_at`),
    /// drop SF so the BIOS's poll loop sees "done". Called on every SMPC
    /// access so SF clears at the *exact* instruction that reads it,
    /// rather than at a coarse drain boundary — the BIOS polls SF tightly
    /// and a late clear desyncs it from the raster (Yabause reference-diff
    /// at the 0x1D64 SF poll). OREG and the SMPC interrupt are filled when
    /// the command is dequeued, so the response is ready before SF drops.
    pub fn settle_intback(&mut self, cycle: u64) {
        if let Some(done_at) = self.intback_complete_at
            && cycle >= done_at
        {
            self.intback_complete_at = None;
            self.sf = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_day_conversion_round_trips() {
        for &(y, m, d) in &[(1970, 1, 1), (1996, 1, 1), (2001, 9, 11), (2099, 12, 31)] {
            let z = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(z), (y, m, d), "{y}-{m}-{d}");
        }
        // 1970-01-01 is day 0 and a Thursday (weekday 4, Sunday = 0).
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn rtc_bytes_encode_a_known_timestamp() {
        // 2001-09-11 13:46:00.
        let secs = days_from_civil(2001, 9, 11) as u64 * 86_400 + 13 * 3600 + 46 * 60;
        let b = rtc_bytes(secs);
        assert_eq!(b[0], 0x20, "year hi");
        assert_eq!(b[1], 0x01, "year lo");
        assert_eq!(b[2] & 0x0F, 0x09, "month");
        assert_eq!(b[2] >> 4, 2, "Tuesday");
        assert_eq!(b[3], 0x11, "day");
        assert_eq!(b[4], 0x13, "hour");
        assert_eq!(b[5], 0x46, "minute");
        assert_eq!(b[6], 0x00, "second");
    }

    #[test]
    fn rtc_advances_with_emulated_time() {
        let mut s = Smpc::new();
        s.set_rtc_unix(0, 0); // 1970-01-01 00:00:00 at cycle 0
        let cps = 1000; // 1000 cycles per second for the test
        assert_eq!(s.rtc_oreg(0, cps)[6], 0x00, "0 s");
        assert_eq!(s.rtc_oreg(5 * 1000, cps)[6], 0x05, "5 s later → :05");
        assert_eq!(s.rtc_oreg(90 * 1000, cps)[5], 0x01, "90 s later → minute 1");
    }

    #[test]
    fn settime_bcd_round_trips_through_the_rtc() {
        let mut s = Smpc::new();
        // year 2010, month 12, day 25, 06:30:15.
        s.set_rtc_bcd([0x20, 0x10, 0x0C, 0x25, 0x06, 0x30, 0x15], 0);
        let b = s.rtc_oreg(0, 1000);
        assert_eq!([b[0], b[1]], [0x20, 0x10], "year");
        assert_eq!(b[2] & 0x0F, 0x0C, "month 12");
        assert_eq!(b[3], 0x25, "day 25");
        assert_eq!([b[4], b[5], b[6]], [0x06, 0x30, 0x15], "time");
    }

    #[test]
    fn ireg_round_trip() {
        let mut s = Smpc::new();
        s.write8(0x01, 0xAB);
        s.write8(0x07, 0xCD);
        assert_eq!(s.read8(0x01), 0xAB);
        assert_eq!(s.read8(0x07), 0xCD);
        // Even-offset slot is reserved → reads as 0.
        assert_eq!(s.read8(0x02), 0);
    }

    #[test]
    fn oreg_round_trip_via_byte_and_word_access() {
        let mut s = Smpc::new();
        s.write8(0x21, 0x11);
        s.write8(0x23, 0x22);
        s.write8(0x5F, 0xFF);
        assert_eq!(s.read8(0x21), 0x11);
        assert_eq!(s.read8(0x23), 0x22);
        assert_eq!(s.read8(0x5F), 0xFF);
        assert_eq!(s.oreg[0], 0x11);
        assert_eq!(s.oreg[1], 0x22);
        assert_eq!(s.oreg[31], 0xFF);
    }

    #[test]
    fn known_comreg_writes_queue_pending_command_and_raise_sf() {
        let mut s = Smpc::new();
        s.write8(0x1F, 0x02); // SSHON
        assert_eq!(s.sf, 1, "SF goes busy on command queue");
        assert_eq!(s.take_pending(), Some(Command::SshOn));
        assert!(s.take_pending().is_none(), "pending is one-shot");
    }

    #[test]
    fn unknown_comreg_writes_are_recorded_not_queued() {
        let mut s = Smpc::new();
        s.write8(0x1F, 0xFE);
        assert!(s.take_pending().is_none());
        assert_eq!(s.last_unknown_command, Some(0xFE));
        assert_eq!(s.sf, 0, "SF stays 0 for unknown commands");
    }

    #[test]
    fn mark_command_done_drops_sf() {
        let mut s = Smpc::new();
        s.write8(0x1F, 0x16); // SETTIME
        assert_eq!(s.sf, 1);
        let _ = s.take_pending();
        s.mark_command_done();
        assert_eq!(s.sf, 0);
    }

    #[test]
    fn command_decode_covers_all_m3_codes() {
        let cases = [
            (0x00, Command::MshOn),
            (0x02, Command::SshOn),
            (0x03, Command::SshOff),
            (0x10, Command::IntBack),
            (0x16, Command::SetTime),
            (0x17, Command::SetSMem),
            (0x18, Command::NmiReq),
            (0x19, Command::ResEnab),
            (0x1A, Command::ResDisa),
        ];
        for (raw, expected) in cases {
            assert_eq!(Command::from_raw(raw), Some(expected));
        }
        assert_eq!(Command::from_raw(0xFF), None);
    }
}
