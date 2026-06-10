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
//! `CR4=0x434B "CK"`) and `HIRQ=0x0000` (all flags clear; MAME's
//! `hirqreg = 0`); the BIOS reads this signature to detect the subsystem.
//! Thereafter the host drives commands by writing all four of CR1..CR4 —
//! the block processes the command (`CR1 >> 8`), writes a response back
//! into CR1..CR4, and sets `HIRQ.CMOK`. With a present dummy disc the
//! status is `PAUSE`.

use std::collections::VecDeque;

use crate::disc::{FAD_OFFSET, SectorSource};

pub const CD_BLOCK_BASE: u32 = 0x0589_0000;
pub const CD_BLOCK_END: u32 = 0x0589_FFFF;

/// Offset of the data-transfer FIFO within the region.
const DATA_FIFO: u32 = 0x8000;

// HIRQ status bits (cs2.c).
const HIRQ_CMOK: u16 = 0x0001; // command dispatch OK / ready for next
const HIRQ_DRDY: u16 = 0x0002; // data transfer ready
const HIRQ_CSCT: u16 = 0x0004; // finished reading one sector
const HIRQ_BFUL: u16 = 0x0008; // CD buffer full
const HIRQ_DCHG: u16 = 0x0020; // disc change / tray open
const HIRQ_ESEL: u16 = 0x0040; // soft-reset / selector settings done
const HIRQ_EHST: u16 = 0x0080; // host I/O done
const HIRQ_SCDQ: u16 = 0x0400; // subcode Q decode done
// MPEG decode-end / "no MPEG card" bit. The real CD-block sets it at power-on
// and holds it (Mednafen `cdb.cpp` sets it from reset and it appears in every
// HIRQ the BIOS reads). We have no MPEG card, so it stays set permanently —
// kept across host HIRQ writes (the host never clears it without an MPEG card).
const HIRQ_MPED: u16 = 0x0800;

// CD status codes — high byte of CR1 (cs2.c / MAME `CD_STAT_*`).
const STAT_PAUSE: u8 = 0x01; // drive ready, disc present, not playing
const STAT_NODISC: u8 = 0x07; // door closed, no disc present
const STAT_PERI: u8 = 0x20; // OR'd in for periodic (unsolicited) reports

// 16-bit CR1 status bits that live above the status byte (MAME `CD_STAT_*`).
const STAT_TRANS: u16 = 0x4000; // data-transfer request pending

// Further status bytes (high byte of the 16-bit status word, MAME `CD_STAT_*`).
const STAT_REJECT: u16 = 0xFF00; // CR1 reject marker for malformed requests
const STAT_SEEK: u8 = 0x04; // drive seeking
const STAT_PLAY: u8 = 0x03; // read/playback in progress
const STAT_STANDBY: u8 = 0x02; // drive stopped (MAME CD_STAT_STANDBY)
const STAT_BUSY: u8 = 0x00; // command accepted, drive transitioning (Mednafen STATUS_BUSY)

// HIRQ playback-complete bit.
const HIRQ_PEND: u16 = 0x0010; // CD playback / read range completed

// HIRQ bits used by the buffer/filter/partition + filesystem engine.
#[allow(dead_code)] // M7 phase 4 (filesystem)
const HIRQ_EFLS: u16 = 0x0200; // file-system processing complete
const HIRQ_ECPY: u16 = 0x0100; // end of copy/move (also set at CD-block reset)

// Buffer/filter/partition engine sizes (MAME `saturn_cd_hle`): a shared pool of
// 200 sector blocks, and 24 filter/partition selectors.
const MAX_BLOCKS: usize = 200;
const MAX_FILTERS: usize = 24;
/// "No filter / device disconnected" sentinel (MAME's `cddevicenum == 0xff`).
const NO_FILTER: u8 = 0xFF;

/// One buffered sector in the 200-block pool. `size < 0` marks the slot free;
/// otherwise it holds `size` bytes of user data plus the sector's disc
/// coordinates and subheader fields (used by filtering).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Block {
    size: i32,
    fad: i32,
    data: Vec<u8>,
    chan: u8,
    fnum: u8,
    subm: u8,
    cinf: u8,
}

impl Block {
    /// A free pool slot.
    fn free() -> Self {
        Block {
            size: -1,
            fad: 0,
            data: Vec::new(),
            chan: 0,
            fnum: 0,
            subm: 0,
            cinf: 0,
        }
    }
}

/// A sector-selection filter (MAME `filterT`): FAD-range and subheader-condition
/// matching, plus the true/false partition each matched sector routes to.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Filter {
    mode: u8,
    chan: u8,
    smmask: u8,
    cimask: u8,
    fid: u8,
    smval: u8,
    cival: u8,
    condtrue: u8,
    condfalse: u8,
    fad: u32,
    range: u32,
}

/// A partition (output buffer): an ordered list of pool-block indices. Unlike
/// MAME's fixed array + null-defragment, we keep it compact in a `Vec`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Partition {
    blocks: Vec<usize>,
}

/// In-flight 32-bit sector-data transfer (Get / Get-and-Delete Sector Data):
/// streams `num` blocks of partition `part` starting at index `pos`, tracking
/// the current block (`sect`) and byte offset within it (`offs`).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Xfer32 {
    delete: bool,
    part: usize,
    pos: usize,
    num: usize,
    sect: usize,
    offs: usize,
}

/// SH-2 master clock (Hz) — sectors stream at 75×speed of these.
const MASTER_HZ: u64 = 28_636_400;

/// One ISO9660 directory record (MAME `direntryT`, fields we use).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct DirEntry {
    firstfad: u32,
    length: u32,
    flags: u8,
    file_unit_size: u8,
    interleave_gap_size: u8,
    #[allow(dead_code)] // retained for Get File Info / debugging
    name: Vec<u8>,
}

/// Little-endian u32 from a byte slice at `o` (0 if out of range).
fn le32(b: &[u8], o: usize) -> u32 {
    match b.get(o..o + 4) {
        Some(s) => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        None => 0,
    }
}

/// SH-2 master cycles between periodic CD status reports. The CD-block
/// firmware emits one report per interval; with no disc playing that interval
/// is ~16.67 ms (Yabause `_periodictiming = 50000` against its µs×3 clock →
/// 50000/3 ≈ 16667 µs). At the 28.6364 MHz SH-2 master clock that is
/// 16667 µs × 28.6364 ≈ 477_273 cycles. [`CdBlock::tick`] carries the
/// remainder across intervals, so the long-run cadence averages exactly this.
///
/// REVIEW(magic): reference-derived, not from a hardware datasheet — the
/// real CD-block (SH-1) firmware period isn't published, so the ~16.67 ms
/// comes from Yabause. It's *deliberately independent* of the video frame
/// (the CD clock is separate); a previous value (476_932) duplicated the old
/// `system::CYCLES_PER_FRAME` and silently went stale when that was
/// corrected to 479_151 — don't re-tie it to the frame length.
const PERIODIC_CYCLES: u64 = 477_273;

/// The CD-block's internal clock: 44.1 kHz × 256 — the unit Mednafen counts its
/// drive and periodic timers in (`cdb.cpp`).
const CD_CLOCK_HZ: u64 = 44_100 * 256;

/// SH-2 master cycles between periodic reports **while the drive is actively
/// reading** (PLAY). Mednafen reloads its periodic counter to `17712` of the
/// [`CD_CLOCK_HZ`] clock on every sector tick in PLAY/PAUSE (`cdb.cpp` ~2373),
/// so the periodic fires roughly **once per sector** (≈75–150 Hz) during a read
/// rather than the idle ~60 Hz ([`PERIODIC_CYCLES`]). Found via a Mednafen
/// dev-build CD trace-diff (M11 timing alignment): ours fired the flat idle
/// cadence regardless of drive state, so during a game's CD-heavy load our
/// periodic/`SCDQ` reports were far sparser than the reference's. `17712 /
/// (44100×256) × 28.6364 MHz ≈ 44_927` master cycles.
const ACTIVE_PERIODIC_CYCLES: u64 = 17_712 * MASTER_HZ / CD_CLOCK_HZ;

/// Convert a count expressed in [`CD_CLOCK_HZ`] units (the unit Mednafen's
/// `cdb.cpp` keeps `DriveCounter`/`PeriodicIdleCounter` in) to SH-2 master
/// cycles, the unit our [`CdBlock::tick`] advances by. `n / CD_CLOCK_HZ`
/// seconds × `MASTER_HZ` cycles/second.
const fn cd2m(n: u64) -> u64 {
    n * MASTER_HZ / CD_CLOCK_HZ
}

/// Drive-phase machine timing (master cycles), ported from Mednafen `cdb.cpp`
/// (`Drive_Run`/`StartSeek`/`SeekStart*`). The reference keeps these in
/// [`CD_CLOCK_HZ`] units; [`cd2m`] converts.
///
/// `SeekCPIUpdateDelay = 500` (cdb.cpp:1903): the short delay before the
/// command-issued seek begins.
const SEEK_CPI_DELAY_CYC: u64 = cd2m(500);
/// `SeekStart2` schedules `256000 - delay_sub` (cdb.cpp:1966) before the seek
/// time itself is computed in `SeekStart3`.
const SEEKSTART2_CYC: u64 = cd2m(256_000);
/// Idle periodic-report interval — `PeriodicIdleCounter_Reload` (cdb.cpp:606,
/// `187065`), ≈16.6 ms ≈ 60 Hz.
const PERIODIC_IDLE_CYC: i64 = cd2m(187_065) as i64;
/// Active (PLAY/PAUSE) periodic-report interval — `17712` (cdb.cpp:2373),
/// fires roughly once per sector.
const PERIODIC_ACTIVE_CYC: i64 = ACTIVE_PERIODIC_CYCLES as i64;
/// Recognition spin-up time for a disc present at power-on/insert — Mednafen
/// `DRIVEPHASE_STARTUP` holds `STATUS_BUSY` for `1 * 44100 * 256` CD clocks
/// (`cdb.cpp:2175`), i.e. exactly one second of the CD clock, before it reads
/// the TOC and settles to PAUSE. `cd2m(CD_CLOCK_HZ)` is that one second in
/// master cycles. The BIOS plays its boot animation + sets up the menu sounds
/// during this window; reporting PAUSE immediately skips the whole sequence.
const STARTUP_CYC: i64 = cd2m(CD_CLOCK_HZ) as i64;

/// One unit of read/seek time at single speed: `(44100×256) / 75` per sector,
/// halved at 2× (cdb.cpp:2291/2398, `(44100*256)/((subq&0x40)?150:75)`).
fn sector_cyc(speed: u32) -> u64 {
    MASTER_HZ / (75 * speed.max(1) as u64)
}

/// The drive's coarse operating phase — Mednafen `DrivePhase` (`cdb.cpp:585`),
/// reduced to the subset our FAD-addressed (no analog/subchannel-Q) disc model
/// needs. The host-visible *status code* (`STATUS_*`) and HIRQ-edge sequence a
/// game's GFS server polls are driven by these transitions, so the phase set
/// and its timing mirror the reference even though the seek internals are
/// simplified (we address sectors directly rather than chasing the pickup via
/// subchannel Q).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum DrivePhase {
    /// Drive idle/stopped (post-Init PAUSE, or stopped via Seek-to-0).
    Idle,
    /// Power-on/insert recognition spin-up (Mednafen `DRIVEPHASE_STARTUP`,
    /// `cdb.cpp:2182`): a disc present at reset reports `STATUS_BUSY` for ~1 s
    /// (`1*44100*256` CD clocks) while the pickup spins up and the TOC is read,
    /// then settles to PAUSE. The BIOS fills this window with its boot
    /// animation + menu-sound setup, so a drive that reports PAUSE *immediately*
    /// (our old `insert_disc`) skips both. A host-level Init does **not** abort
    /// it — the physical pickup keeps spinning up regardless of the buffer/filter
    /// reset the Init command performs.
    Startup,
    /// A seek was just commanded: compute the target, report `STATUS_BUSY`,
    /// then schedule the seek-time (Mednafen `SEEK_START1/2/3`).
    SeekStart,
    /// Seeking to the target FAD: reports `STATUS_SEEK` until the seek time
    /// elapses, then enters `Play`.
    Seek,
    /// Reading/playing the range `[cur_play_start, cur_play_end)`: reports
    /// `STATUS_PLAY`, buffers one data sector per tick via the read-ahead
    /// pipeline (`sec_prebuf`), raising `CSCT` per buffered sector.
    Play,
    /// Range complete (or buffer-full): a `PauseCounter`-delayed transition to
    /// `STATUS_PAUSE`, at which point `play_end_irq` (`PEND`/`EFLS`) fires.
    Pause,
    /// The pickup settle between `SeekStart` and `Seek` (Mednafen `SeekStart2`
    /// → `SEEK_START3`, cdb.cpp:1957): the geometry is already resolved but the
    /// radial seek hasn't begun, and the host sees **`STATUS_BUSY`** for the
    /// whole `SEEKSTART2_CYC` (256000-clock) window — only then does the drive
    /// report `STATUS_SEEK`. VF2's intro probes 1-sector Plays and reads this
    /// intermediate status into its next command word (`0x2000 | CR1`), so
    /// reporting SEEK during the settle leaked `0x04` into the command byte
    /// (`0x24`, not a real command) and derailed it. Declared last so existing
    /// bincode save states (index-encoded) keep their phase indices.
    SeekSettle,
}

// Not `Clone`: holds a `Box<dyn SectorSource>` (image or live drive) that
// isn't cloneable; nothing clones a CdBlock.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
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
    repcnt: u8,
    ctrladdr: u8,
    track: u8,
    index: u8,
    fad: u32,
    disk_changed: bool,
    /// "Current head position is on a CD-ROM (data) track" status bit (CR1 low
    /// byte bit 7). **Read-based, like Mednafen** (`CurPosInfo.is_cdrom`,
    /// cdb.cpp:2312/2324): set true only when the read pump actually reads a
    /// *data* sector during PLAY, cleared for audio and at Init/insert. It is
    /// NOT a position lookup — during recognition (before any PLAY) the real
    /// drive reports `is_cdrom = 0`, and the BIOS recognition state machine
    /// branches on that: a premature `1` (our old `track_at_fad`-based value)
    /// made the loader restart its cleanup loop (AbortFile) and give up to the
    /// CD player instead of proceeding to GetToc → auth → Play → ReadFile.
    is_cdrom: bool,

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

    /// The inserted disc image, if any. `None` is the power-on "no disc"
    /// state the existing no-disc command subset already models.
    ///
    /// `#[serde(skip)]`: the sector source is external media (an image, maybe
    /// hundreds of MB, or a live drive), so save states reference it rather
    /// than embedding it. The *logical* playback position (FAD, status,
    /// partitions) lives in the fields above and is serialized; `load_state`
    /// re-grafts the live source.
    #[serde(skip)]
    disc: Option<Box<dyn SectorSource>>,

    /// Host-readable data staged by a command (Get TOC for now), streamed out
    /// 16-bit big-endian through the data FIFO at `0x8000`. `xfer_pos` is the
    /// byte cursor. Phase 3 generalises this to sector data + the SCU-DMA port.
    xfer: Vec<u8>,
    xfer_pos: usize,
    /// Running count of bytes the host has read from the data port since the
    /// current transfer was staged (mirrors MAME `xferdnum`). Incremented by
    /// both the 16-bit FIFO path (TOC/file-info) and the 32-bit sector path;
    /// reset when a command stages a transfer; reported back, in words, by End
    /// Data Transfer (`0x06`) so the BIOS can confirm how much it actually got.
    xfer_done: usize,
    /// Persistent data-transfer-request (TRANS, `0x4000`) status bit. Set when a
    /// command stages host-readable data (Get TOC / Get Sector Data); cleared by
    /// End Data Transfer (`0x06`). Mirrors MAME's `cd_stat & CD_STAT_TRANS`: the
    /// BIOS polls status to know a transfer is pending and reads the transferred
    /// word count back from End Data Transfer to confirm it — so the bit must
    /// persist across status polls, not just appear in the staging command's CR1.
    transfer_request: bool,

    /// Decoded CD-DA (Red Book) samples awaiting mix into the SCSP output —
    /// interleaved 16-bit stereo at 44.1 kHz. The read pump fills this while an
    /// **audio** track plays (M10); `Saturn::take_audio` drains and mixes it.
    cd_audio: VecDeque<i16>,
    /// CD-DA jitter-buffer priming flag: the read pump fills [`Self::cd_audio`]
    /// in sector bursts on the scheduler's batch cadence, but the host drains it
    /// smoothly per audio frame. Stay un-primed (mix silence, keep buffering)
    /// until a pre-roll cushion has built, then drain; re-arm on a dry buffer.
    /// Without this the steady drain outruns the bursty fill and CDDA stutters.
    cd_audio_primed: bool,

    // ---- buffer/filter/partition engine (M7 phase 2) ----
    /// Shared 200-block sector pool; free slots have `size < 0`.
    blocks: Vec<Block>,
    /// Count of free pool slots (mirrors MAME `freeblocks`).
    free_blocks: i32,
    /// Buffer-full latch (mirrors MAME `buffull`).
    buf_full: bool,
    /// 24 sector filters.
    filters: Vec<Filter>,
    /// 24 output partitions (one selectable per filter index).
    partitions: Vec<Partition>,
    /// Filter the CD drive's output connects to (`0xFF` = disconnected).
    cd_device_filter: u8,
    /// Last partition a sector was delivered to (Get Last Buffer destination).
    last_buffer: u8,
    /// Sector data length the next read stores / the host transfers (Set Sector
    /// Length; default 2048). Read by the M7-phase-3 read pump / transfer.
    #[allow(dead_code)]
    sectlenin: u32,
    #[allow(dead_code)]
    sectlenout: u32,
    /// Result of the last Calculate Actual Data Size, in 16-bit words.
    calcsize: u32,

    // ---- read pump + data transfer (M7 phase 3) ----
    /// FAD the read pump is currently at.
    cd_curfad: u32,
    /// Sectors left to read in the active PLAY; `< 0` means idle.
    fadstoplay: i64,
    /// Read speed multiplier (1× or 2×; default 2×).
    cd_speed: u32,
    /// Cycles accumulated toward the next sector read.
    sector_accum: u64,
    /// At least one sector has been buffered since the last empty.
    sectorstore: bool,
    /// Working sector being filtered, and whether it carries a Mode-2 subheader.
    curblock: Block,
    curblock_mode2: bool,
    /// Active 32-bit sector-data transfer, if any.
    xfer32: Option<Xfer32>,

    // ---- drive-phase machine (Mednafen `cdb.cpp` `Drive_Run`) ----
    /// Coarse drive phase driving the host-visible status sequence + HIRQ
    /// edge timing (see [`DrivePhase`]).
    drive_phase: DrivePhase,
    /// Countdown (master cycles) to the next [`DrivePhase`] advance — Mednafen
    /// `DriveCounter`. Signed: the `tick` loop processes phases while it is
    /// `<= 0`, then reschedules.
    drive_counter: i64,
    /// Countdown (master cycles) to the next periodic report — Mednafen
    /// `PeriodicIdleCounter`. Reloads to [`PERIODIC_IDLE_CYC`] idle,
    /// [`PERIODIC_ACTIVE_CYC`] in PLAY/PAUSE.
    periodic_idle: i64,
    /// The drive head's next-to-read FAD — Mednafen `CurSector`. The buffered
    /// sector lags this by one (the read-ahead pipeline), and `cd_curfad`
    /// (= `CurPosInfo.fad`) reports the sector currently read *ahead*.
    drive_sector: u32,
    /// Play range start/end as the command gave them — Mednafen
    /// `CurPlayStart`/`CurPlayEnd` (bit `0x800000` = FAD addressing).
    cur_play_start: u32,
    cur_play_end: u32,
    /// Commanded repeat count and the running repeat counter — Mednafen
    /// `CurPlayRepeat`/`PlayRepeatCounter` (bit 0x80 = "repeated, no fresh
    /// sector since").
    cur_play_repeat: u8,
    play_repeat_counter: u8,
    /// HIRQ to raise when the play range completes — `HIRQ_PEND` for Play,
    /// `HIRQ_EFLS` for Read File (Mednafen `PlayEndIRQType`, cdb.cpp:2830/3920).
    play_end_irq: u16,
    /// Mednafen `PauseCounter`: sequences the `PLAY → BUSY → PAUSE` end
    /// transition so the end IRQ fires a couple periodics *after* the last
    /// sector, with status already PAUSE (cdb.cpp:450 FIXME / 2414-2444).
    pause_counter: i32,
    /// The read-ahead sector's FAD and whether one is loaded — Mednafen
    /// `SecPreBuf`/`SecPreBuf_In`. We store only the FAD (not the bytes): our
    /// [`SectorSource`] reads are synchronous and deterministic, so deferring
    /// the actual read+filter to the process step is observably identical to
    /// Mednafen reading the bytes ahead, and reuses [`read_filtered_sector`].
    sec_prebuf_fad: u32,
    sec_prebuf_in: bool,
    sec_prebuf_audio: bool,
    /// A sector was buffered since the last status downgrade — Mednafen
    /// `PlaySectorProcessed` (drives the SEEK/BUSY→PLAY status refresh).
    play_sector_processed: bool,

    // ---- ISO9660 filesystem (M7 phase 4) ----
    /// Root directory record (from the primary volume descriptor).
    curroot: DirEntry,
    /// Entries of the current directory.
    curdir: Vec<DirEntry>,
    /// Number of entries in the current directory.
    numfiles: u32,
    /// Index of the first non-directory entry (Get File Scope).
    firstfile: u32,

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

    /// Debug-only command-history ring (M11 boot trace): when `cmd_log_on` is
    /// set, [`dispatch`](Self::dispatch) appends `(cmd, cr_in, [cr1..4 out],
    /// hirq, status)` per command (bounded to the last 512). Not part of the
    /// machine state — `#[serde(skip)]` so the save-state determinism contract
    /// is untouched — and recording is off by default so normal runs pay
    /// nothing. Read in `tests/trace_boot.rs` to diff our recognition CD command
    /// stream against Mednafen's `cdb.cpp`.
    #[serde(skip)]
    pub cmd_log: Vec<CmdTrace>,
    #[serde(skip)]
    pub cmd_log_on: bool,
    /// Master PC that issued the in-flight command, set by the bus from its
    /// `step_pc` just before a CD register write so [`execute`](Self::execute)
    /// can record the caller in [`cmd_log`](Self::cmd_log). Debug-only.
    #[serde(skip)]
    pub caller_pc: u32,

    /// Debug-only HIRQ-edge log (M11 CD-timing alignment, plan Phase 1): when
    /// `hirq_log_on`, records `(old, new, cause)` on every HIRQ change —
    /// including the *between-command* read-pump (CSCT/PEND) and host-W1C edges
    /// that the per-command [`cmd_log`](Self::cmd_log) misses — so our HIRQ
    /// *timeline* can be diffed against Mednafen's `cdb.cpp` to pin the exact
    /// bit/edge whose timing the game's GFS server reads. `cause` is the command
    /// byte (0x00..=0xFF), or [`HIRQ_CAUSE_READPUMP`]/[`HIRQ_CAUSE_W1C`].
    /// `#[serde(skip)]` (debug-only — save-state determinism untouched).
    #[serde(skip)]
    pub hirq_log: Vec<(u16, u16, u32)>,
    #[serde(skip)]
    pub hirq_log_on: bool,
    #[serde(skip)]
    last_hirq: u16,
}

/// [`CdBlock::hirq_log`] cause: the read pump (`play_data`) changed HIRQ.
pub const HIRQ_CAUSE_READPUMP: u32 = 0x100;
/// [`CdBlock::hirq_log`] cause: the host write-1-to-clear changed HIRQ.
pub const HIRQ_CAUSE_W1C: u32 = 0x101;

/// One [`CdBlock::cmd_log`] entry — a detailed CD host-command record for the
/// M11 boot trace: the master-SH-2 PC that issued the command, the command
/// byte, the CR1..4 the host wrote and the CR1..4 the block returned, the HIRQ
/// before→after the command, and the resulting drive status. Debug-only.
#[derive(Clone, Copy, Debug, Default)]
pub struct CmdTrace {
    /// Master PC that wrote the command trigger (CR4), via the bus `step_pc`.
    pub caller_pc: u32,
    pub cmd: u8,
    pub cr_in: [u16; 4],
    pub cr_out: [u16; 4],
    pub hirq_in: u16,
    pub hirq_out: u16,
    pub status: u8,
}

impl Default for CdBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl CdBlock {
    pub fn new() -> Self {
        Self {
            // Power-on HIRQ is all-clear (MAME's `hirqreg = 0`): CMOK and the
            // rest are set only by events (commands, periodics). The BIOS
            // ORs HIRQ into a WRAM accumulator and tests CMOK (bit 0) early
            // in boot — a spuriously-set CMOK derails it.
            hirq: HIRQ_MPED, // no MPEG card → MPED set from power-on, held
            hirq_mask: 0xFFFF,
            // Power-on identity string "CDBLOCK" — the BIOS reads CR1..CR4
            // to confirm the CD subsystem is present.
            cr1: b'C' as u16, // high byte 0, low byte 'C'
            cr2: ((b'D' as u16) << 8) | b'B' as u16,
            cr3: ((b'L' as u16) << 8) | b'O' as u16,
            cr4: ((b'C' as u16) << 8) | b'K' as u16,
            // No CD image present → status `NODISC` (door closed, empty
            // drive), matching MAME's reset (`saturn_cd_hle.cpp`: it sets
            // `cd_stat = CD_STAT_PAUSE` but immediately overrides to
            // `CD_STAT_NODISC` when `!m_cdrom_image->exists()`). The disc
            // *geometry* is all zero: MAME's `cr_standard_return` returns
            // CR2=CR3=CR4=0 with no image. `insert_disc` switches this to
            // PAUSE with real geometry. (Verified against MAME v0.287 with
            // both the USA v1.00 and JP v1.01 BIOS; the splash boot path is
            // unaffected by PAUSE-vs-NODISC, but the host-visible status is.)
            status: STAT_NODISC,
            repcnt: 0x00,
            ctrladdr: 0x00,
            track: 0x00,
            index: 0x00,
            fad: 0,
            disk_changed: true,
            is_cdrom: false,
            disc: None,
            xfer: Vec::new(),
            xfer_pos: 0,
            xfer_done: 0,
            transfer_request: false,
            cd_audio: VecDeque::new(),
            cd_audio_primed: false,
            blocks: vec![Block::free(); MAX_BLOCKS],
            free_blocks: MAX_BLOCKS as i32,
            buf_full: false,
            filters: vec![Filter::default(); MAX_FILTERS],
            partitions: vec![Partition::default(); MAX_FILTERS],
            cd_device_filter: NO_FILTER,
            last_buffer: NO_FILTER,
            sectlenin: 2048,
            sectlenout: 2048,
            calcsize: 0,
            cd_curfad: FAD_OFFSET,
            fadstoplay: -1,
            cd_speed: 2,
            sector_accum: 0,
            sectorstore: false,
            curblock: Block::free(),
            curblock_mode2: false,
            xfer32: None,
            drive_phase: DrivePhase::Idle,
            drive_counter: 0,
            periodic_idle: PERIODIC_IDLE_CYC,
            drive_sector: FAD_OFFSET,
            cur_play_start: 0,
            cur_play_end: 0,
            cur_play_repeat: 0,
            play_repeat_counter: 0,
            play_end_irq: 0,
            pause_counter: 0,
            sec_prebuf_fad: FAD_OFFSET,
            sec_prebuf_in: false,
            sec_prebuf_audio: false,
            play_sector_processed: false,
            curroot: DirEntry::default(),
            curdir: Vec::new(),
            numfiles: 0,
            firstfile: 0,
            command_pending: false,
            cr_written: 0,
            periodic_accum: 0,
            host_initialized: false,
            cmd_log: Vec::new(),
            cmd_log_on: false,
            caller_pc: 0,
            hirq_log: Vec::new(),
            hirq_log_on: false,
            last_hirq: HIRQ_MPED, // matches the power-on `hirq` above
        }
    }

    /// Whether the CD-block is currently asserting its SCU external interrupt
    /// ([`crate::scu::Source::Cd`]): `(HIRQ & HIRQ_Mask) != 0`, exactly
    /// Mednafen's `RecalcIRQOut` condition (`cdb.cpp`). The Saturn aggregate
    /// samples this each master instruction and feeds it to
    /// [`Scu::set_cd_int`](crate::scu::Scu::set_cd_int) — the CD-block can't
    /// reach the SCU from inside the bus, so the level is sampled at the top.
    pub fn irq_active(&self) -> bool {
        (self.hirq & self.hirq_mask) != 0
    }

    /// Debug-only: true while the drive is still in recognition spin-up
    /// ([`DrivePhase::Startup`]). Lets a boot-timing probe stamp the frame the
    /// drive settles (recognition complete), to localize where ours' boot
    /// diverges in frame-count from the reference (M12 BGM-trigger-tick chase).
    pub fn dbg_in_startup(&self) -> bool {
        matches!(self.drive_phase, DrivePhase::Startup)
    }

    /// Record a HIRQ change to [`hirq_log`](Self::hirq_log) (debug-only; no-op
    /// unless `hirq_log_on`). Called after every site that mutates `hirq`.
    fn note_hirq(&mut self, cause: u32) {
        if self.hirq_log_on && self.hirq != self.last_hirq {
            if self.hirq_log.len() < 16384 {
                self.hirq_log.push((self.last_hirq, self.hirq, cause));
            }
            self.last_hirq = self.hirq;
        }
    }

    /// Insert (or replace) a disc. The drive returns to PAUSE at the start of
    /// track 1 (FAD 150) with the geometry the status reports now carry.
    ///
    /// A disc-change (`HIRQ.DCHG`) is flagged **only for a runtime swap** — i.e.
    /// once the host has engaged the block (`host_initialized`). At *cold boot*
    /// the disc is simply present from the BIOS's point of view; raising DCHG
    /// then makes the BIOS treat the boot disc as a hot-swap and drop into its
    /// CD control panel ("Start Application") instead of auto-booting the game.
    /// MAME's HLE likewise shows no DCHG in its cold-boot HIRQ trace.
    pub fn insert_disc<S: SectorSource + 'static>(&mut self, source: S) {
        self.ctrladdr = source
            .track_at_fad(FAD_OFFSET)
            .map_or(0x41, |t| t.ctrl_addr);
        self.track = source.first_track();
        self.index = 1;
        self.fad = FAD_OFFSET;
        // Mednafen `DRIVEPHASE_STARTUP`: a disc present at power-on/insert spins
        // up reporting `STATUS_BUSY` for ~1 s (recognition) before the TOC is
        // read and the drive settles to PAUSE. The BIOS plays its boot animation
        // and sets up the menu sounds during that window — reporting PAUSE
        // immediately (the old behaviour) made the BIOS perceive a
        // ready/door-already-closed drive and jump straight to the static logo
        // with no animation and no sound. See [`DrivePhase::Startup`].
        self.status = STAT_BUSY;
        // A freshly-loaded disc has not been read yet → `is_cdrom` clears until
        // the read pump actually reads a data sector (Mednafen semantics).
        self.is_cdrom = false;
        self.disc = Some(Box::new(source));
        self.cd_curfad = FAD_OFFSET;
        self.drive_sector = FAD_OFFSET;
        self.drive_startup();
        // A disc appearing is always a disc-change: the FIRST Init (cold boot
        // *or* a runtime swap) reports DCHG one-shot, matching Mednafen, which
        // treats a power-on disc as a change (its recognition HIRQ carries
        // DCHG). For a runtime swap (host already engaged) also assert DCHG now,
        // since the BIOS/game isn't about to issue an Init.
        self.disk_changed = true;
        // Mednafen's CD block reset-completes with the full reset HIRQ
        // (cdb.cpp:4075, CMOK|DCHG|ESEL|EHST|MPED|ECPY|EFLS = 0x0BE1) when a
        // disc is present — that's the value the BIOS reads before its first
        // disc-recognition command, so it goes straight to GetHwInfo. With only
        // MPED set, the BIOS saw a "not ready" block, issued an extra
        // Init(SW-reset)+GetStatus, and the recognition state machine desynced:
        // it then looped AbortFile and gave up to the CD player. Setting the
        // full reset HIRQ on a disc-present boot makes recognition proceed
        // (GetToc → auth → GetDiscRegion → ChangeDir), matching Mednafen.
        //
        // Gated on disc presence (insert_disc): the no-disc splash keeps the
        // MPED-only power-on HIRQ, so the bios_boot golden is unaffected —
        // setting this HIRQ at cold *power-on* (no disc) breaks the splash.
        self.hirq =
            HIRQ_CMOK | HIRQ_DCHG | HIRQ_ESEL | HIRQ_EHST | HIRQ_MPED | HIRQ_ECPY | HIRQ_EFLS;
    }

    /// Whether a disc is present.
    pub fn has_disc(&self) -> bool {
        self.disc.is_some()
    }

    /// Eject the disc: the inverse of [`insert_disc`]. The drive returns to the
    /// empty-tray `NODISC` state with zeroed geometry, and a disc-change is
    /// flagged (`HIRQ.DCHG`) so the BIOS/game notices the media left.
    pub fn eject(&mut self) {
        self.disc = None;
        self.drive_idle();
        self.status = STAT_NODISC;
        self.ctrladdr = 0;
        self.track = 0;
        self.index = 0;
        self.fad = 0;
        self.disk_changed = true;
        self.hirq |= HIRQ_DCHG;
    }

    /// Borrow the inserted sector source, if any — used to fingerprint the
    /// media for save-state validation (the source is `#[serde(skip)]`'d).
    pub fn disc(&self) -> Option<&dyn SectorSource> {
        self.disc.as_deref()
    }

    /// Move the sector source out (leaving the drive disc-less). Used by
    /// `Saturn::load_state` to re-graft the live source onto a decoded state,
    /// which never carries the (skipped) media itself.
    pub fn take_disc(&mut self) -> Option<Box<dyn SectorSource>> {
        self.disc.take()
    }

    /// Re-attach a source moved out by [`take_disc`] without disturbing the
    /// already-restored logical state (status/FAD/partitions). Unlike
    /// [`insert_disc`], it does *not* reset the drive or raise `DCHG`.
    pub fn restore_disc(&mut self, disc: Option<Box<dyn SectorSource>>) {
        self.disc = disc;
    }

    /// Map an access offset to its register slot (each register occupies a
    /// 4-byte slot; both halfwords alias the same register).
    fn slot(offset: u32) -> u32 {
        offset & 0xFFFC
    }

    pub fn read16(&mut self, offset: u32) -> u16 {
        if offset & 0xFFFF >= DATA_FIFO {
            // Data FIFO (16-bit): stream the staged TOC / file-info buffer.
            // 32-bit sector-data transfers go through `read32` / the data port.
            return self.read_fifo16();
        }
        match Self::slot(offset & 0xFFFF) {
            0x0008 => {
                // DCHG ("disc change / tray open") is **W1C, NOT auto-cleared
                // on read** — Mednafen keeps it set in the HIRQ until the host
                // acknowledges by writing HIRQ (its disc-recognition loop reads
                // HIRQ many times and the stored value retains DCHG=0x20; the
                // value the BIOS branches on is e.g. 0x0FE1, with DCHG). MAME's
                // `hirq_r` clears DCHG on read, but that left our recognition
                // HIRQ shadow ([0x060003A4]) at 0x0EC1 — missing DCHG — and the
                // BIOS took its give-up (AbortFile-loop) path instead of
                // proceeding to GetToc → auth → Play. (Same W1C reasoning as
                // CSCT below.)
                //
                // CSCT ("1 sector read complete") is likewise W1C — it stays set
                // after the read pump raises it until the host writes HIRQ.
                // Clearing it on read made our loader never see "read complete".
                self.hirq &= !HIRQ_BFUL;
                // Debug: env-gated HIRQ read-watch (logs each *changed* value
                // the host reads), to see which HIRQ state the BIOS CD-boot
                // loader branches on at the post-IP.BIN read-file-vs-re-recognize
                // decision. Deduped so the constant status-poll doesn't flood.
                #[cfg(not(test))]
                if std::env::var_os("CD_RWATCH").is_some() {
                    use std::sync::atomic::{AtomicU16, Ordering};
                    static LAST: AtomicU16 = AtomicU16::new(0xFFFF);
                    if LAST.swap(self.hirq, Ordering::Relaxed) != self.hirq {
                        eprintln!("HIRQrd {:04X}", self.hirq);
                    }
                }
                self.hirq
            }
            0x000C => self.hirq_mask,
            0x0018 => {
                #[cfg(not(test))]
                if std::env::var_os("CD_RWATCH").is_some() {
                    use std::sync::atomic::{AtomicU16, Ordering};
                    static LAST: AtomicU16 = AtomicU16::new(0xFFFF);
                    if LAST.swap(self.cr1, Ordering::Relaxed) != self.cr1 {
                        eprintln!("CR1rd {:04X}", self.cr1);
                    }
                }
                self.cr1
            }
            0x001C => self.cr2,
            0x0020 => self.cr3,
            0x0024 => {
                // Reading CR4 consumes a command response; periodic
                // reports may resume (cs2.c clears `_command` here).
                #[cfg(not(test))]
                if std::env::var_os("CD_RWATCH").is_some() {
                    use std::sync::atomic::{AtomicU16, Ordering};
                    static LAST: AtomicU16 = AtomicU16::new(0xFFFF);
                    if LAST.swap(self.cr4, Ordering::Relaxed) != self.cr4 {
                        eprintln!("CR4rd {:04X}", self.cr4);
                    }
                }
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
            // MPED is held set regardless (no MPEG card — it's never cleared
            // without one), matching Mednafen where MPED persists from reset.
            0x0008 => {
                // When the host clears DCHG it has *acknowledged* the disc
                // change, so drop the internal `disk_changed` latch too.
                // Otherwise the next Init re-raises DCHG (see the 0x04 handler)
                // and the BIOS perceives a fresh disc swap — it then loops
                // recognition instead of booting the disc it already read.
                // Mednafen clears DCHG once during recognition and never
                // re-raises it at Init; this matches that (verified by the CD
                // command/HIRQ trace-diff: ours' Init was the only command
                // leaving DCHG set where Mednafen's left it clear).
                if self.hirq & HIRQ_DCHG != 0 && val & HIRQ_DCHG == 0 {
                    self.disk_changed = false;
                }
                self.hirq = (self.hirq & val) | HIRQ_MPED;
                self.note_hirq(HIRQ_CAUSE_W1C);
            }
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
        // The data port (FIFO) carries 32-bit sector-data transfers.
        if offset & 0xFFFF >= DATA_FIFO {
            return self.read_data_port32();
        }
        ((self.read16(offset) as u32) << 16) | self.read16(offset + 2) as u32
    }

    /// Read the CD data-transfer port (the SCU-DMA alias at `0x0581_8000`),
    /// one 32-bit big-endian word of the active sector-data transfer.
    pub fn read_data_port(&mut self) -> u32 {
        self.read_data_port32()
    }

    pub fn write32(&mut self, offset: u32, val: u32) {
        self.write16(offset, (val >> 16) as u16);
        self.write16(offset + 2, val as u16);
    }

    /// Debug: the drive head's current FAD (the read-pump position).
    pub fn curfad(&self) -> u32 {
        self.cd_curfad
    }

    /// Debug: a snapshot of CD-block state for the interactive debugger —
    /// `(status, cur_fad, fad_to_play, free_blocks, per-partition block counts)`.
    pub fn debug_state(&self) -> (u8, u32, i64, i32, Vec<usize>) {
        (
            self.status,
            self.cd_curfad,
            self.fadstoplay,
            self.free_blocks,
            self.partitions.iter().map(|p| p.blocks.len()).collect(),
        )
    }

    /// Write a standard CD status report into CR1..CR4 (cs2.c `doCDReport`).
    fn cd_report(&mut self) {
        // The reported FAD is the drive's *current* head position, which the
        // read pump advances (`cd_curfad`) — not the static `fad` set only at
        // insert/eject. Reporting the stale `fad` left the BIOS boot loader
        // seeing the head never move past the IP.BIN start (150) after a read,
        // so it rejected the disc; cs2.c reports the live `cd_curfad`.
        let fad = if self.disc.is_some() {
            self.cd_curfad
        } else {
            self.fad
        };
        // CR1 low byte (MAME / Mednafen `Results[0]`, cdb.cpp: `(is_cdrom << 7)
        // | (repcount & 0x7F)`): bit 7 = "the current head position is on a
        // CD-ROM (data) track" (`is_cdrom`), bits 6-0 = the periodic repeat
        // count. The BIOS CD-boot loader checks this is-cdrom bit in the status
        // report after reading IP.BIN to confirm it is positioned on a data
        // track before reading the 1st-read file. **Read-based** (`self.is_cdrom`,
        // set by the read pump on a data-sector read) — NOT a `track_at_fad`
        // position lookup, which reported `1` during recognition (before any
        // PLAY) where the real drive / Mednafen report `0`, derailing the BIOS
        // recognition into the give-up loop.
        self.cr1 = self.cd_stat() | ((self.is_cdrom as u16) << 7) | (self.repcnt & 0x7F) as u16;
        self.cr2 = ((self.ctrladdr as u16) << 8) | self.track as u16;
        self.cr3 = ((self.index as u16) << 8) | ((fad >> 16) & 0xFF) as u16;
        self.cr4 = fad as u16;
    }

    /// The 16-bit status word (status code in the high byte) — MAME `cd_stat`.
    fn cd_stat(&self) -> u16 {
        let mut s = (self.status as u16) << 8;
        if self.transfer_request {
            s |= STAT_TRANS;
        }
        s
    }

    /// Read one sector at `fad` into the working block and route it through the
    /// connected filter into a partition (MAME `cd_read_filtered_sector`).
    fn read_filtered_sector(&mut self, fad: u32) -> bool {
        if self.cd_device_filter == NO_FILTER || self.buf_full {
            return false;
        }
        let len = self.sectlenin as usize;
        let (data, sub) = {
            let Some(disc) = self.disc.as_ref() else {
                return false;
            };
            let mut buf = [0u8; 2352];
            // Store `sectlenin` bytes: the 2048 user payload for the common
            // case, else a leading slice of the full on-disc sector.
            let data = if len == 2048 {
                if !disc.read_sector(fad, &mut buf[..2048]) {
                    return false;
                }
                buf[..2048].to_vec()
            } else {
                let n = disc.read_full_sector(fad, &mut buf);
                if n == 0 {
                    return false;
                }
                buf[..len.min(n)].to_vec()
            };
            (data, disc.subheader(fad))
        };
        let (chan, fnum, subm, cinf) = sub.unwrap_or((0, 0, 0, 0));
        self.curblock = Block {
            size: len as i32,
            fad: fad as i32,
            data,
            chan,
            fnum,
            subm,
            cinf,
        };
        self.curblock_mode2 = sub.is_some();
        self.filter_data()
    }

    /// Whether the working sector matches filter `f` (FAD range + Mode-2
    /// subheader conditions, with the reverse-conditions bit).
    fn filter_match(&self, f: &Filter) -> bool {
        let mut m = true;
        if f.mode & 0x40 != 0 {
            let fad = self.curblock.fad as u32;
            if fad < f.fad || fad > f.fad.wrapping_add(f.range) {
                m = false;
            }
        }
        if self.curblock_mode2 {
            if f.mode & 0x01 != 0 && self.curblock.fnum != f.fid {
                m = false;
            }
            if f.mode & 0x02 != 0 && self.curblock.chan != f.chan {
                m = false;
            }
            if f.mode & 0x04 != 0 && (self.curblock.subm & f.smmask) != f.smval {
                m = false;
            }
            if f.mode & 0x08 != 0 && (self.curblock.cinf & f.cimask) != f.cival {
                m = false;
            }
            if f.mode & 0x10 != 0 {
                m = !m;
            }
        }
        m
    }

    /// Route the working sector to a partition via the filter chain: a match
    /// goes to the filter's true-connector partition; a miss chases the
    /// false-connector (up to two hops) before the sector is dropped
    /// (MAME `cd_filterdata`).
    fn filter_data(&mut self) -> bool {
        let mut fidx = self.cd_device_filter as usize;
        if fidx >= MAX_FILTERS {
            return false;
        }
        let mut last = self.filters[fidx].condtrue;
        let mut keepgoing = 2;
        loop {
            let f = self.filters[fidx].clone();
            if self.filter_match(&f) {
                break;
            }
            last = f.condfalse;
            if last == NO_FILTER || keepgoing == 0 {
                return false;
            }
            fidx = last as usize;
            if fidx >= MAX_FILTERS {
                return false;
            }
            keepgoing -= 1;
        }
        let part = last as usize;
        if part >= MAX_FILTERS {
            return false;
        }
        self.last_buffer = last;
        let Some(b) = self.alloc_block() else {
            return false;
        };
        self.blocks[b].fad = self.curblock.fad;
        self.blocks[b].data = self.curblock.data.clone();
        self.blocks[b].chan = self.curblock.chan;
        self.blocks[b].fnum = self.curblock.fnum;
        self.blocks[b].subm = self.curblock.subm;
        self.blocks[b].cinf = self.curblock.cinf;
        self.partitions[part].blocks.push(b);
        true
    }

    /// Read one sector at `cd_curfad` immediately (the pre-drive-phase-machine
    /// read pump). Retained as a **test-only** direct-read helper for the
    /// sector-decode unit tests (CDDA decode, `is_cdrom` latch); production now
    /// reads through the phased [`drive_run`](Self::drive_run) pipeline.
    #[cfg(test)]
    fn play_data(&mut self) {
        // Match on the drive-state bits only: `STAT_PERI` (0x20) is OR'd into
        // `status` by the unsolicited periodic report between commands, so an
        // exact `match self.status` would miss the PLAY/SEEK arms whenever a
        // periodic report has fired since the last CR1 write — stalling the read
        // pump (the BIOS would wait forever for sector data after Play).
        match self.status & !STAT_PERI {
            STAT_SEEK => self.status = STAT_PLAY | (self.status & STAT_PERI),
            STAT_PLAY if self.fadstoplay > 0 => {
                let fad = self.cd_curfad;
                let is_audio = self
                    .disc
                    .as_ref()
                    .and_then(|d| d.track_at_fad(fad))
                    .is_some_and(|t| t.is_audio);
                if is_audio {
                    // CDDA: stream the sector to the audio mixer, don't buffer it
                    // as data (no CSCT / sector store — the host isn't reading).
                    // An audio sector clears `is_cdrom` (Mednafen cdb.cpp:2312).
                    self.is_cdrom = false;
                    if self.read_cd_audio_sector(fad) {
                        self.cd_curfad += 1;
                        self.fadstoplay -= 1;
                        // PLAY active periodic cadence: a sector read schedules
                        // the next periodic ~one sector hence (Mednafen's
                        // per-sector reset; see ACTIVE_PERIODIC_CYCLES).
                        self.periodic_accum =
                            PERIODIC_CYCLES.saturating_sub(ACTIVE_PERIODIC_CYCLES);
                        if self.fadstoplay == 0 {
                            self.status = STAT_PAUSE;
                            self.hirq |= HIRQ_PEND;
                        }
                    }
                } else {
                    // Reading a data sector sets `is_cdrom` (Mednafen
                    // cdb.cpp:2324) — the head is confirmed on a CD-ROM track,
                    // whether or not a filter buffers the sector.
                    self.is_cdrom = true;
                    if self.read_filtered_sector(fad) {
                        self.cd_curfad += 1;
                        self.fadstoplay -= 1;
                        // PLAY active periodic cadence (see audio branch above).
                        self.periodic_accum =
                            PERIODIC_CYCLES.saturating_sub(ACTIVE_PERIODIC_CYCLES);
                        self.hirq |= HIRQ_CSCT;
                        self.sectorstore = true;
                        if self.fadstoplay == 0 {
                            self.status = STAT_PAUSE;
                            self.hirq |= HIRQ_PEND;
                        }
                    }
                }
            }
            _ => {}
        }
        self.note_hirq(HIRQ_CAUSE_READPUMP);
    }

    // ===== drive-phase machine (Mednafen `cdb.cpp` `Drive_Run` port) =====
    //
    // Replaces the immediate `STAT_PLAY → STAT_PAUSE` read pump with the
    // reference's phased model so the *host-visible status sequence and HIRQ
    // edge timing* a game's GFS server polls match Mednafen:
    //   Play/Seek → BUSY → SEEK → PLAY → (read-ahead, CSCT/sector) → end_met
    //            → BUSY → PauseCounter delay → PAUSE + PEND/EFLS.
    // The seek internals are simplified for our FAD-addressed disc (no analog
    // pickup / subchannel-Q chase), but the phases, their timing, and the edges
    // they emit are faithful.

    /// Begin a seek (Mednafen `StartSeek`, cdb.cpp:1984). Records the play range
    /// and the end-of-range IRQ type, clears any pending read-ahead, reports
    /// `STATUS_BUSY`, and enters the seek-startup phase. `target`/`end` carry
    /// bit `0x800000` for FAD addressing.
    fn start_seek(&mut self, target: u32, end: u32, repeat: u8, play_end_irq: u16) {
        self.start_seek_impl(target, end, repeat, play_end_irq, false);
    }

    /// `no_pickup_change` is Play-mode bit 7 (Mednafen `StartSeek`'s
    /// `no_pickup_change`): re-arm the play range *without* moving the pickup —
    /// the read-ahead is kept, and a drive already in `Play` just continues
    /// (only the range/IRQ changes); otherwise the seek skips the geometry
    /// resolve (`SeekStart1`) so the settle targets the current head position.
    fn start_seek_impl(
        &mut self,
        target: u32,
        end: u32,
        repeat: u8,
        play_end_irq: u16,
        no_pickup_change: bool,
    ) {
        if self.disc.is_none() {
            return;
        }
        // Debug (SAT_CDSEEKLOG): log every seek/read/play target — the CD
        // "index" (which section/FAD the game reads). Observer-only.
        if std::env::var("SAT_CDSEEKLOG").is_ok() {
            let (sfad, efad) = (target & 0x7F_FFFF, end & 0x7F_FFFF);
            eprintln!(
                "CDSEEK fad={sfad} (0x{sfad:06X}) lba={} count={} end_fad={efad} repeat={repeat} irq={play_end_irq:04X} npc={no_pickup_change}",
                sfad as i64 - 150,
                efad.wrapping_sub(sfad)
            );
        }
        self.play_repeat_counter = 0;
        if !no_pickup_change {
            self.sec_prebuf_in = false; // ClearPendingSec
        }
        self.cur_play_start = target;
        self.cur_play_end = end;
        self.cur_play_repeat = repeat;
        self.play_end_irq = play_end_irq;
        if no_pickup_change && self.drive_phase == DrivePhase::Play {
            // Already playing: keep reading from the current position with the
            // new range (Mednafen cdb.cpp:2025 returns before the phase reset).
            return;
        }
        self.status = STAT_BUSY;
        if no_pickup_change {
            // Skip SeekStart1 (the geometry resolve): the settle + seek target
            // the current head position (Mednafen `DRIVEPHASE_SEEK_START2`).
            self.drive_phase = DrivePhase::SeekSettle;
            self.drive_counter = (SEEKSTART2_CYC - SEEK_CPI_DELAY_CYC) as i64;
        } else {
            self.drive_phase = DrivePhase::SeekStart;
            self.drive_counter = SEEK_CPI_DELAY_CYC as i64;
        }
        self.periodic_idle = PERIODIC_IDLE_CYC;
    }

    /// Demo/debug hook: start CD-DA (Red Book) playback of `sectors` sectors
    /// from FAD `fad`, exactly as a host **Play** command would (it calls the
    /// same [`Self::start_seek`]) — the drive seeks, reaches [`DrivePhase::Play`],
    /// and the periodic read pump streams the audio track to the CD-DA mixer that
    /// [`crate::system::Saturn::take_audio`] sums into the output. This lets a
    /// demo *drive the running machine* to play an audio disc without the BIOS
    /// issuing Play itself (the LLE-68k trigger wall). Observer-only: it reuses
    /// the real play path and adds no new core behaviour.
    pub fn dbg_play_cdda(&mut self, fad: u32, sectors: u32) {
        self.start_seek(0x80_0000 | fad, 0x80_0000 | (fad + sectors), 0, HIRQ_PEND);
    }

    /// Demo/debug: find the first Red Book **audio** track on the disc and start
    /// playing it as CD-DA (see [`Self::dbg_play_cdda`]); returns whether one was
    /// found. The frontend's "play CD audio" key calls this so an audio disc
    /// plays through the live SCSP-mixed output without the BIOS issuing Play
    /// (the LLE-68k trigger wall). Walks the tracks by FAD to the lead-out.
    pub fn dbg_play_first_audio_track(&mut self) -> bool {
        let found = {
            let Some(disc) = self.disc.as_ref() else {
                return false;
            };
            let lead_out = disc.lead_out_fad();
            let mut fad = FAD_OFFSET;
            let mut found = None;
            while fad < lead_out {
                let Some(t) = disc.track_at_fad(fad) else {
                    break;
                };
                if t.is_audio {
                    found = Some((t.start_fad, t.length));
                    break;
                }
                fad = t.start_fad + t.length.max(1);
            }
            found
        };
        match found {
            Some((start, len)) => {
                self.dbg_play_cdda(start, len);
                true
            }
            None => false,
        }
    }

    /// Park the drive: cancel any active play/seek and return the phase machine
    /// to idle (no read range). Called by Init / Abort File / Seek-stop / disc
    /// insert+eject so the host-visible state is consistent with those commands'
    /// "drive stopped/paused" semantics. The caller sets the status code.
    fn drive_idle(&mut self) {
        self.drive_phase = DrivePhase::Idle;
        self.sec_prebuf_in = false;
        self.cur_play_end = 0;
        self.play_end_irq = 0;
        self.pause_counter = 0;
        self.play_repeat_counter = 0;
        self.fadstoplay = -1;
        self.drive_counter = 0;
        self.periodic_idle = PERIODIC_IDLE_CYC;
    }

    /// Begin the recognition spin-up (Mednafen `DRIVEPHASE_STARTUP`): park the
    /// read pipeline like [`drive_idle`] but enter [`DrivePhase::Startup`] for
    /// [`STARTUP_CYC`] (~1 s) instead of going idle. The caller sets
    /// `status = STAT_BUSY`. Periodic reports are suppressed during recognition
    /// (Mednafen parks `PeriodicIdleCounter` at "never"), so the host keeps
    /// seeing BUSY until the spin-up completes in [`Self::drive_run`].
    fn drive_startup(&mut self) {
        self.drive_phase = DrivePhase::Startup;
        self.sec_prebuf_in = false;
        self.cur_play_end = 0;
        self.play_end_irq = 0;
        self.pause_counter = 0;
        self.play_repeat_counter = 0;
        self.fadstoplay = -1;
        self.drive_counter = STARTUP_CYC;
        self.periodic_idle = i64::MAX;
    }

    /// Resolve the seek target and enter the pickup settle (Mednafen
    /// `SeekStart1`+`SeekStart2`, cdb.cpp:1905/1957). Sets the head geometry
    /// and holds **`STATUS_BUSY`** for the `SEEKSTART2_CYC` settle — the host
    /// must not see `STATUS_SEEK` until the radial seek proper begins in
    /// [`Self::seek_settle_done`] (the `SEEK_START3` stage). Reporting SEEK
    /// during the settle leaked status `0x04` into VF2's `0x2000 | CR1`
    /// command builder (→ the bogus command `0x24`) where Mednafen's BUSY
    /// (`0x00`) keeps it a clean GetSubcodeQ.
    fn seek_start(&mut self) {
        // SeekStart1: resolve the target FAD. Data reads use FAD addressing
        // (bit 0x800000); the track/index form seeks to the commanded track's
        // *start* (Mednafen `SeekStart1` else-branch, `150 +
        // toc.tracks[track_target].lba`; the index is approximated to 1 — we
        // don't model sub-indices). VF2's character-select BGM is
        // `Play(track 14 idx 1 → track 14 idx 99, repeat ∞)` — a CD-DA track
        // on loop; the old "approximate to the disc start" played *data*
        // sectors from FAD 150 instead of the music.
        let fad_target = if self.cur_play_start & 0x80_0000 != 0 {
            (self.cur_play_start & 0x7F_FFFF).max(FAD_OFFSET)
        } else {
            let tt = ((self.cur_play_start >> 8) & 0xFF) as u8;
            self.track_start_fad(tt).unwrap_or(FAD_OFFSET)
        };
        if let Some(t) = self.disc.as_ref().and_then(|d| d.track_at_fad(fad_target)) {
            self.ctrladdr = t.ctrl_addr;
            self.track = t.number;
            self.index = 1;
        }
        self.cd_curfad = fad_target;
        // SeekStart2 (cdb.cpp:1957): BUSY through the settle. The physical
        // pickup (`drive_sector`) stays put until the settle completes — it is
        // the seek-distance origin read by `seek_settle_done`. (Mednafen
        // subtracts the already-elapsed `SeekCPIUpdateDelay` from the settle.)
        self.is_cdrom = false;
        self.repcnt = self.play_repeat_counter & 0x0F;
        self.status = STAT_BUSY;
        self.drive_phase = DrivePhase::SeekSettle;
        self.drive_counter += (SEEKSTART2_CYC - SEEK_CPI_DELAY_CYC) as i64;
    }

    /// The settle elapsed: schedule the radial seek time and report
    /// `STATUS_SEEK` (Mednafen `SEEK_START3`, cdb.cpp:2218). Seek time =
    /// fixed 12·(44100·256)/150 plus a per-FAD-delta term (cdb.cpp:2227).
    fn seek_settle_done(&mut self) {
        // Visible stopped geometry uses the 0xFFFF_FFFF sentinel, but the
        // physical pickup remains at `drive_sector`. Using the sentinel as the
        // next seek's origin turns a stop-then-Play sequence into a multi-day
        // seek.
        let prev = self.drive_sector;
        let fad_target = self.cd_curfad;
        let delta = fad_target.abs_diff(prev) as u64;
        let seek_cyc = cd2m(12 * CD_CLOCK_HZ / 150) + cd2m(delta * 27);
        self.status = STAT_SEEK;
        self.drive_phase = DrivePhase::Seek;
        self.drive_sector = fad_target;
        self.drive_counter += seek_cyc as i64;
    }

    /// Start FAD of `track` (clamped to the disc's track range) from the TOC,
    /// for the Play command's track/index addressing form.
    fn track_start_fad(&self, track: u8) -> Option<u32> {
        let d = self.disc.as_ref()?;
        let t = track.clamp(d.first_track(), d.last_track());
        let toc = d.toc();
        let i = (t as usize - 1) * 4;
        Some(((toc[i + 1] as u32) << 16) | ((toc[i + 2] as u32) << 8) | toc[i + 3] as u32)
    }

    /// Track number under the play head, for the track-form end checks
    /// (Mednafen reads it from the per-sector subchannel Q; we resolve it
    /// from the FAD). Past the lead-out (or with no position) → 0xAA.
    fn head_track(&self) -> u8 {
        self.disc
            .as_ref()
            .and_then(|d| d.track_at_fad(self.cd_curfad))
            .map_or(0xAA, |t| t.number)
    }

    /// Whether the play head has reached the end of the commanded range
    /// (Mednafen `CheckEndMet`, cdb.cpp:2054): the lead-out, the FAD-form
    /// bounds, or — in the track/index form — the head crossing past the end
    /// track (or before the start track).
    fn check_end_met(&self) -> bool {
        let mut end_met = self.track == 0xAA;
        if self.cur_play_end != 0 {
            if self.cur_play_end & 0x80_0000 != 0 {
                end_met |= self.cd_curfad >= (self.cur_play_end & 0x7F_FFFF);
            } else if let Some(d) = self.disc.as_ref() {
                let end_track = (((self.cur_play_end >> 8) & 0xFF) as u8)
                    .clamp(d.first_track(), d.last_track());
                // Our index is always 1, so only the track number can exceed
                // the bound (Mednafen also compares `idx > end_index`).
                end_met |= self.head_track() > end_track;
            }
        }
        if self.cur_play_start & 0x80_0000 != 0 {
            end_met |= self.cd_curfad < (self.cur_play_start & 0x7F_FFFF);
        } else if let Some(d) = self.disc.as_ref() {
            let start_track =
                (((self.cur_play_start >> 8) & 0xFF) as u8).clamp(d.first_track(), d.last_track());
            end_met |= self.head_track() < start_track;
        }
        end_met
    }

    /// One `DRIVEPHASE_PLAY` step (Mednafen cdb.cpp:2304): process the
    /// previously read-ahead sector (buffer a data sector → `CSCT`, or stream a
    /// CDDA sector), then read the next sector ahead (`SecPreBuf`). The buffered
    /// sector lags `cd_curfad` (= `CurPosInfo.fad`, the read-ahead position) by
    /// one, so the final out-of-range sector is discarded by the periodic
    /// `end_met` check before it is ever buffered.
    fn drive_play_tick(&mut self) {
        if self.sec_prebuf_in {
            let fad = self.sec_prebuf_fad;
            if self.sec_prebuf_audio {
                // CDDA → mixer; clears is_cdrom (cdb.cpp:2319).
                self.is_cdrom = false;
                self.read_cd_audio_sector(fad);
                self.sec_prebuf_in = false;
                self.play_sector_processed = true;
                self.drive_sector = self.drive_sector.wrapping_add(1);
            } else {
                // Data sector confirms is_cdrom (cdb.cpp:2324); buffer it only
                // if the pool has room, else hold (buffer-full backpressure —
                // the periodic moves us to a Pause).
                self.is_cdrom = true;
                if self.free_blocks > 0 {
                    self.read_filtered_sector(fad);
                    self.sectorstore = true;
                    self.hirq |= HIRQ_CSCT;
                    if self.free_blocks <= 0 {
                        self.hirq |= HIRQ_BFUL;
                    }
                    self.sec_prebuf_in = false;
                    self.play_sector_processed = true;
                    self.drive_sector = self.drive_sector.wrapping_add(1);
                }
            }
        }
        // PLAY/PAUSE periodic cadence: once per sector (cdb.cpp:2373).
        self.periodic_idle = PERIODIC_ACTIVE_CYC;
        // Read the next sector ahead (only if the prior one was consumed).
        if !self.sec_prebuf_in {
            let fad = self.drive_sector;
            // Refresh the reported geometry as the head advances (Mednafen
            // updates `CurPosInfo.tno/ctrl_adr` from the per-sector subchannel
            // Q) — the host polls the report's track byte, and the track-form
            // end check follows the head across track boundaries.
            if let Some(t) = self.disc.as_ref().and_then(|d| d.track_at_fad(fad)) {
                self.sec_prebuf_audio = t.is_audio;
                self.ctrladdr = t.ctrl_addr;
                self.track = t.number;
                self.index = 1;
            } else {
                self.sec_prebuf_audio = false;
            }
            self.sec_prebuf_fad = fad;
            self.sec_prebuf_in = true;
            self.cd_curfad = fad; // CurPosInfo.fad = CurSector (cdb.cpp:2389)
            self.fadstoplay = (self.cur_play_end & 0x7F_FFFF) as i64 - fad as i64;
        }
        self.drive_counter += sector_cyc(self.cd_speed) as i64;
    }

    /// The per-`PeriodicIdleCounter` work (Mednafen cdb.cpp:2403): resolve the
    /// PLAY/PAUSE end-of-range and buffer-full transitions, then emit the
    /// unsolicited status report (`SCDQ`). Sequenced so the end IRQ
    /// (`PEND`/`EFLS`) fires a couple periodics *after* the last sector with the
    /// status already `PAUSE` (cdb.cpp:450 FIXME).
    fn drive_periodic(&mut self) {
        self.periodic_idle = PERIODIC_IDLE_CYC;
        if self.sec_prebuf_in && matches!(self.drive_phase, DrivePhase::Play | DrivePhase::Pause) {
            let end_met = self.check_end_met();
            if self.drive_phase == DrivePhase::Pause {
                self.sec_prebuf_in = false;
                if self.pause_counter == 1 {
                    self.status = STAT_PAUSE;
                    self.fadstoplay = -1;
                    // Don't fire if we've repeated with no fresh sector since.
                    if end_met && self.play_end_irq != 0 && (self.play_repeat_counter & 0x80) == 0 {
                        self.hirq |= self.play_end_irq;
                    }
                    self.play_end_irq = 0;
                    self.pause_counter = -1;
                } else if self.pause_counter == -1 {
                    self.status = STAT_PAUSE;
                    self.fadstoplay = -1;
                    if !end_met && self.free_blocks > 0 {
                        // Resume from a buffer-full pause: continue reading from
                        // where we stopped (drive_sector), not a fresh seek.
                        self.status = STAT_BUSY;
                        self.drive_phase = DrivePhase::Play;
                        self.drive_counter = SEEK_CPI_DELAY_CYC as i64;
                    }
                } else {
                    self.pause_counter += 1;
                }
            } else if end_met {
                self.sec_prebuf_in = false;
                if self.play_repeat_counter >= self.cur_play_repeat {
                    self.drive_sector = self.cd_curfad;
                    self.status = STAT_BUSY;
                    self.drive_phase = DrivePhase::Pause;
                    self.pause_counter = if self.play_end_irq != 0 { 0 } else { 1 };
                } else {
                    if self.play_repeat_counter < 0x0E {
                        self.play_repeat_counter += 1;
                    }
                    self.play_repeat_counter |= 0x80;
                    // Replay the range from the start (cdb.cpp:2468 SeekStart1/2).
                    self.drive_phase = DrivePhase::SeekStart;
                    self.status = STAT_BUSY;
                    self.drive_counter = SEEK_CPI_DELAY_CYC as i64;
                }
            } else if self.free_blocks <= 0 {
                // Buffer-full pause (cdb.cpp:2472).
                self.sec_prebuf_in = false;
                self.status = STAT_BUSY;
                self.drive_phase = DrivePhase::Pause;
                self.pause_counter = 0;
            } else {
                self.play_repeat_counter &= !0x80;
                if self.play_sector_processed {
                    self.status = STAT_PLAY;
                    self.play_sector_processed = false;
                }
            }
        }
        // Emit the unsolicited periodic report (SCDQ), unless a command response
        // is still unread (cs2.c `_command` / Mednafen `ResultsRead`) **or the
        // host is mid-way through composing a command** (`cr_written != 0`).
        // The hardware keeps host-written command words and block-written
        // results in separate register files (Mednafen `CTR.CD[]` vs
        // `Results[]`; MAME gates on `cmd_pending`), so a periodic can never
        // corrupt a half-written command. Our shared CR1–4 must emulate that:
        // without this guard the report clobbered VF2's GetSubcodeQ between
        // its CR1 and CR4 writes, dispatching the report's own status byte as
        // a bogus command (0x21 = PAUSE|PERI, 0x24 = SEEK|PERI).
        if self.host_initialized && !self.command_pending && self.cr_written == 0 {
            self.status |= STAT_PERI;
            self.cd_report();
        }
        self.hirq |= HIRQ_SCDQ;
    }

    /// Advance the drive-phase machine by `cycles` master cycles (the
    /// disc-present path of [`tick`]; Mednafen `Drive_Run`, cdb.cpp:2134).
    ///
    /// Event-stepped so it is **independent of the caller's tick granularity**:
    /// each `DriveCounter`/`PeriodicIdleCounter` crossing is processed in order,
    /// whether the caller advances 256 cycles or a whole frame at once. (The
    /// reference relies on being called in fine slices from its scheduler; a
    /// coarse single advance would otherwise fire the periodic only once and
    /// stall the multi-periodic `PauseCounter` end sequence.)
    fn drive_run(&mut self, cycles: u64) {
        let mut rem = cycles as i64;
        loop {
            // Process every drive-phase advance that is currently due.
            let mut guard = 0;
            while self.drive_counter <= 0 && guard < 100_000 {
                guard += 1;
                match self.drive_phase {
                    DrivePhase::Idle => self.drive_counter += PERIODIC_IDLE_CYC,
                    DrivePhase::Startup => {
                        // Recognition spin-up complete (Mednafen `STARTUP` →
                        // `TranslateTOC` + `StartSeek(150)`, cdb.cpp:2183). Our
                        // FAD-addressed disc model already holds the TOC and is
                        // positioned at track 1, so we settle straight to a ready
                        // PAUSE and resume idle periodic reports — the host has
                        // seen BUSY for the whole ~1 s window.
                        self.status = STAT_PAUSE | (self.status & STAT_PERI);
                        self.drive_phase = DrivePhase::Idle;
                        self.cd_curfad = FAD_OFFSET;
                        self.drive_sector = FAD_OFFSET;
                        self.drive_counter += PERIODIC_IDLE_CYC;
                        self.periodic_idle = PERIODIC_IDLE_CYC;
                    }
                    DrivePhase::SeekStart => self.seek_start(),
                    DrivePhase::SeekSettle => self.seek_settle_done(),
                    DrivePhase::Seek => {
                        self.play_sector_processed = false;
                        self.sec_prebuf_in = false;
                        self.drive_phase = DrivePhase::Play;
                        self.drive_counter += sector_cyc(self.cd_speed) as i64;
                    }
                    DrivePhase::Play => self.drive_play_tick(),
                    DrivePhase::Pause => {
                        // PAUSE keeps reading the next sector ahead (cdb.cpp:2375)
                        // so the periodic's PauseCounter sequence (which is gated
                        // on a pending read-ahead) advances and eventually fires
                        // the end IRQ with the status already PAUSE.
                        self.periodic_idle = PERIODIC_ACTIVE_CYC;
                        if !self.sec_prebuf_in {
                            self.sec_prebuf_fad = self.drive_sector;
                            self.cd_curfad = self.drive_sector;
                            self.sec_prebuf_in = true;
                        }
                        self.drive_counter += sector_cyc(self.cd_speed) as i64;
                    }
                }
            }
            // Process a due periodic report (reloads `periodic_idle` positive).
            if self.periodic_idle <= 0 {
                self.drive_periodic();
            }
            if rem <= 0 {
                break;
            }
            // Advance to the next scheduled event, bounded by what's left.
            let next = self.drive_counter.min(self.periodic_idle).max(1);
            let adv = next.min(rem);
            self.drive_counter -= adv;
            self.periodic_idle -= adv;
            rem -= adv;
        }
        self.note_hirq(HIRQ_CAUSE_READPUMP);
    }

    /// Decode one Red Book audio sector at `fad` into the CD-DA FIFO: 2352 raw
    /// bytes = 588 interleaved 16-bit little-endian stereo frames. Capped at a
    /// few seconds so the buffer can't grow unbounded if the host stops draining
    /// audio, while still absorbing the read pump's sector-burst fills (the
    /// scheduler ticks the drive in batches, so several sectors can decode in one
    /// step; a tight cap would drop them and the audio would gap — see
    /// [`Self::take_cd_audio`]'s jitter buffer).
    fn read_cd_audio_sector(&mut self, fad: u32) -> bool {
        let Some(disc) = self.disc.as_ref() else {
            return false;
        };
        let mut buf = [0u8; 2352];
        if disc.read_full_sector(fad, &mut buf) < 2352 {
            return false;
        }
        for frame in buf.chunks_exact(2) {
            self.cd_audio
                .push_back(i16::from_le_bytes([frame[0], frame[1]]));
        }
        const CAP: usize = 44_100 * 2 * 5; // ~5 s of stereo samples
        while self.cd_audio.len() > CAP {
            self.cd_audio.pop_front();
        }
        true
    }

    /// Drain up to `n` decoded CD-DA samples (interleaved stereo) **raw** —
    /// exactly what's buffered, padded with silence if short. The low-level
    /// drain used by the decode tests and the byte-exact CDDA demo. Realtime
    /// playback should use [`Self::take_cd_audio_buffered`] instead.
    pub fn take_cd_audio(&mut self, n: usize) -> Vec<i16> {
        let take = n.min(self.cd_audio.len());
        let mut v: Vec<i16> = self.cd_audio.drain(..take).collect();
        v.resize(n, 0);
        v
    }

    /// Drain `n` CD-DA samples for **realtime playback** (`Saturn::take_audio`),
    /// through a **pre-roll jitter buffer**: the read pump fills [`Self::cd_audio`]
    /// in sector bursts on the scheduler's batch cadence, but the host pulls a
    /// smooth `n` every audio frame. Draining 1:1 from a near-empty buffer would
    /// pad silence on every fill gap → audible stutter. Instead we hold off (mix
    /// silence, keep buffering) until a ~0.5 s cushion exists, then drain.
    ///
    /// Re-priming happens **only when the buffer is fully drained** — a genuine
    /// stall or end-of-track — not on a sub-frame near-miss. Re-priming on every
    /// dip-below-`n` (the first cut) re-buffered a fresh half-second of silence
    /// whenever the cushion was still settling against the seek startup + bursty
    /// fill, which stuttered at the *start* of playback. A near-miss now just
    /// pads the shortfall once; only a true empty re-arms the cushion.
    pub fn take_cd_audio_buffered(&mut self, n: usize) -> Vec<i16> {
        const PREROLL: usize = 44_100; // ~0.5 s of interleaved stereo
        if !self.cd_audio_primed {
            if self.cd_audio.len() < PREROLL.max(n) {
                return vec![0i16; n];
            }
            self.cd_audio_primed = true;
        }
        let out = self.take_cd_audio(n);
        if self.cd_audio.is_empty() {
            self.cd_audio_primed = false; // fully drained → re-buffer before resuming
        }
        out
    }

    /// Remove `num` blocks from partition `buf` starting at `ofs`, freeing them.
    fn delete_partition_sectors(&mut self, buf: usize, ofs: usize, num: usize) {
        let end = (ofs + num).min(self.partitions[buf].blocks.len());
        if ofs > end {
            return;
        }
        let removed: Vec<usize> = self.partitions[buf].blocks.drain(ofs..end).collect();
        for b in removed {
            self.free_block(b);
        }
    }

    /// One 32-bit big-endian word from the active sector-data transfer (the
    /// data port at `0x..18000` / FIFO offset `0x8000`). When the blocks are
    /// drained, a Get-and-Delete frees them. Falls back to the 16-bit TOC
    /// stream (as two words) when no 32-bit transfer is active.
    fn read_data_port32(&mut self) -> u32 {
        let Some(x) = self.xfer32.clone() else {
            return ((self.read_fifo16() as u32) << 16) | self.read_fifo16() as u32;
        };
        if x.sect >= x.num {
            if x.delete {
                self.delete_partition_sectors(x.part, x.pos, x.num);
            }
            self.xfer32 = None;
            return 0xFFFF_FFFF;
        }
        let bi = self.partitions[x.part].blocks[x.pos + x.sect];
        let size = self.blocks[bi].size.max(0) as usize;
        let o = x.offs;
        let d = &self.blocks[bi].data;
        let rv = ((*d.get(o).unwrap_or(&0) as u32) << 24)
            | ((*d.get(o + 1).unwrap_or(&0) as u32) << 16)
            | ((*d.get(o + 2).unwrap_or(&0) as u32) << 8)
            | (*d.get(o + 3).unwrap_or(&0) as u32);
        if let Some(xm) = self.xfer32.as_mut() {
            xm.offs += 4;
            if xm.offs >= size {
                xm.offs = 0;
                xm.sect += 1;
            }
        }
        self.xfer_done += 4;
        rv
    }

    /// One 16-bit big-endian word from the staged TOC/file-info buffer.
    fn read_fifo16(&mut self) -> u16 {
        let p = self.xfer_pos;
        let word = match (self.xfer.get(p), self.xfer.get(p + 1)) {
            (Some(&hi), Some(&lo)) => ((hi as u16) << 8) | lo as u16,
            _ => 0,
        };
        if p < self.xfer.len() {
            self.xfer_pos = (p + 2).min(self.xfer.len());
            self.xfer_done += 2;
        }
        word
    }

    /// Load a directory (MAME `read_new_dir`). `0xFFFFFF` finds the primary
    /// volume descriptor (FAD 166..200), parses the root record, and reads the
    /// root directory; otherwise it reads the sub-directory at entry `fileno`.
    fn read_new_dir(&mut self, fileno: u32) {
        if fileno == 0xFF_FFFF {
            let mut pvd = None;
            if let Some(disc) = self.disc.as_ref() {
                let mut sect = [0u8; 2048];
                for cfad in 166..200u32 {
                    if !disc.read_sector(cfad, &mut sect) {
                        break;
                    }
                    if &sect[1..6] == b"CD001" {
                        match sect[0] {
                            1 => {
                                pvd = Some(sect.to_vec());
                                break;
                            }
                            0xFF => break,
                            _ => {}
                        }
                    }
                }
            }
            let Some(sect) = pvd else { return };
            // Root directory record sits at offset 156 in the PVD.
            self.curroot = DirEntry {
                firstfad: le32(&sect, 158) + FAD_OFFSET,
                length: le32(&sect, 166),
                flags: *sect.get(181).unwrap_or(&0),
                ..Default::default()
            };
            self.make_dir_current(self.curroot.firstfad);
        } else if let Some(e) = self.curdir.get(fileno as usize) {
            let fad = e.firstfad;
            self.make_dir_current(fad);
        }
    }

    /// Parse the directory at `fad` into `curdir` (MAME `make_dir_current`):
    /// variable-length records, jumping the 0-padded gap at each 0x800 sector
    /// boundary; `firstfile` is the first non-directory entry.
    fn make_dir_current(&mut self, fad: u32) {
        let dirlen = self.curroot.length.max(2048) as usize;
        let nsect = dirlen.div_ceil(2048);
        let mut buf: Vec<u8> = Vec::with_capacity(nsect * 2048);
        if let Some(disc) = self.disc.as_ref() {
            let mut sect = [0u8; 2048];
            for i in 0..nsect as u32 {
                if disc.read_sector(fad + i, &mut sect) {
                    buf.extend_from_slice(&sect);
                } else {
                    buf.resize(buf.len() + 2048, 0);
                }
            }
        }
        let mut entries: Vec<DirEntry> = Vec::new();
        let mut pos = 0usize;
        let mut sector_number = 0usize;
        while pos < buf.len() {
            let rec = buf[pos] as usize;
            if rec == 0 {
                if sector_number < self.curroot.length as usize {
                    sector_number += 0x800;
                    pos = sector_number;
                    continue;
                }
                break;
            }
            if pos + 33 > buf.len() {
                break;
            }
            let namelen = buf[pos + 32] as usize;
            entries.push(DirEntry {
                firstfad: le32(&buf, pos + 2) + FAD_OFFSET,
                length: le32(&buf, pos + 10),
                file_unit_size: buf[pos + 26],
                interleave_gap_size: buf[pos + 27],
                flags: buf[pos + 25],
                name: buf
                    .get(pos + 33..pos + 33 + namelen)
                    .unwrap_or(&[])
                    .to_vec(),
            });
            pos += rec;
        }
        self.numfiles = entries.len() as u32;
        self.firstfile = entries
            .iter()
            .position(|e| e.flags & 0x02 == 0)
            .unwrap_or(0) as u32;
        self.curdir = entries;
    }

    /// Allocate a free pool block (its `size` set to `sectlenin`), returning the
    /// index, or `None` (latching buffer-full) when the pool is exhausted.
    fn alloc_block(&mut self) -> Option<usize> {
        for i in 0..self.blocks.len() {
            if self.blocks[i].size < 0 {
                self.free_blocks -= 1;
                if self.free_blocks <= 0 {
                    self.buf_full = true;
                }
                self.blocks[i].size = self.sectlenin as i32;
                return Some(i);
            }
        }
        self.buf_full = true;
        None
    }

    /// Return a pool block to the free list (clearing the buffer-full latch).
    fn free_block(&mut self, idx: usize) {
        self.blocks[idx].size = -1;
        self.blocks[idx].data = Vec::new();
        self.free_blocks += 1;
        self.buf_full = false;
        self.hirq &= !HIRQ_BFUL;
    }

    /// Free every block held by partition `p` and empty it.
    fn clear_partition(&mut self, p: usize) {
        let idxs = core::mem::take(&mut self.partitions[p].blocks);
        for b in idxs {
            self.free_block(b);
        }
    }

    /// Reset Selector (cmd 0x48): clear a single partition (CR1 low = 0) or,
    /// per CR1 flag bits, reset filter conditions / all filters / all
    /// partitions (MAME `cmd_reset_selector`).
    fn reset_selector(&mut self) {
        let cr1 = self.cr1;
        if cr1 & 0xFF == 0x00 {
            let buf = (self.cr3 >> 8) as usize;
            if buf < MAX_FILTERS {
                self.clear_partition(buf);
            }
            self.cd_report();
            self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            return;
        }
        if cr1 & 0x80 != 0 {
            for f in &mut self.filters {
                f.condfalse = 0;
            }
        }
        if cr1 & 0x40 != 0 {
            for f in &mut self.filters {
                f.condtrue = 0;
            }
        }
        if cr1 & 0x10 != 0 {
            for f in &mut self.filters {
                *f = Filter {
                    range: 0xFFFF_FFFF,
                    ..Filter::default()
                };
            }
        }
        if cr1 & 0x04 != 0 {
            for p in 0..MAX_FILTERS {
                self.clear_partition(p);
            }
            self.buf_full = false;
        }
        self.cd_report();
        self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
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
        let cr_in = [self.cr1, self.cr2, self.cr3, self.cr4];
        // The host has engaged the block; unsolicited periodic reports may
        // now run (the signature no longer needs holding — see
        // `host_initialized`). The response that follows sits in CR1..CR4
        // until the host reads CR4, so guard it from periodic clobbering.
        self.host_initialized = true;
        self.command_pending = true;
        let hirq_in = self.hirq; // HIRQ the host saw before issuing this command
        // Clear CMOK while "processing" (cs2.c clears it at entry).
        self.hirq &= !HIRQ_CMOK;

        self.dispatch(command, cr_in, hirq_in);
        if std::env::var_os("CD_TRACE").is_some() {
            eprintln!(
                "CD {cmd:02X} in={i0:04X},{i1:04X},{i2:04X},{i3:04X} \
                 out={o0:04X},{o1:04X},{o2:04X},{o3:04X} hirq={h:04X} stat={s:02X}",
                cmd = command,
                i0 = cr_in[0],
                i1 = cr_in[1],
                i2 = cr_in[2],
                i3 = cr_in[3],
                o0 = self.cr1,
                o1 = self.cr2,
                o2 = self.cr3,
                o3 = self.cr4,
                h = self.hirq,
                s = self.status,
            );
        }
    }

    /// Decode and run one host command. `cr_in` is the CR1..CR4 the host wrote;
    /// `hirq_in` is the HIRQ the host saw just before issuing it.
    fn dispatch(&mut self, command: u8, cr_in: [u16; 4], hirq_in: u16) {
        self.dispatch_inner(command, cr_in);
        self.note_hirq(command as u32);
        // M11 boot-trace ring (off by default; see `cmd_log`). Collapse a run of
        // consecutive GetStatus(0x00) polls from the same caller into a single
        // entry (the stall spins on it) so the ring keeps the meaningful command
        // sequence instead of flooding with identical polls.
        if self.cmd_log_on {
            let collapse = command == 0x00
                && self
                    .cmd_log
                    .last()
                    .is_some_and(|e| e.cmd == 0x00 && e.caller_pc == self.caller_pc);
            if !collapse {
                if self.cmd_log.len() >= 1024 {
                    self.cmd_log.remove(0);
                }
                self.cmd_log.push(CmdTrace {
                    caller_pc: self.caller_pc,
                    cmd: command,
                    cr_in,
                    cr_out: [self.cr1, self.cr2, self.cr3, self.cr4],
                    hirq_in,
                    hirq_out: self.hirq,
                    status: self.status,
                });
            }
        }
    }

    fn dispatch_inner(&mut self, command: u8, _cr_in: [u16; 4]) {
        match command {
            0x00 => {
                // Get CD status.
                self.cd_report();
                self.hirq |= HIRQ_CMOK;
            }
            0x01 => {
                // Get hardware info: status + CD-block hardware flags/version,
                // MPEG version, drive info (Mednafen `cdb.cpp` GET_HWINFO:
                // CR2=0x0002, CR3=0x0000, CR4=0x0600). MAME uses different
                // literal bytes (0x0201/0x0400) whose CR2 high byte (0x02) the
                // BIOS reads as "MPEG card present" — that sent our boot down
                // the MPEG-auth-probe path (E0/E1/E2 with CR2=1) and made it
                // loop disc recognition ~4× instead of proceeding once. With
                // no MPEG card we must report 0x0002/0x0600 like the reference.
                self.cr1 = (self.status as u16) << 8;
                self.cr2 = 0x0002; // hardware flags (no MPEG) / version
                self.cr3 = 0x0000; // MPEG version (none)
                self.cr4 = 0x0600; // drive info / revision
                self.hirq |= HIRQ_CMOK;
            }
            0x02 => {
                // Get TOC (MAME `cmd_get_toc`): status becomes TRANS|PAUSE
                // (we don't track the TRANS status bit separately, so set it
                // directly in CR1); CR2 = TOC length in words = 102*2 = 0xCC.
                // With a disc, stage the real 408-byte TOC for the host to read
                // through the data FIFO.
                if let Some(d) = &self.disc {
                    self.xfer = d.toc().to_vec();
                    self.xfer_pos = 0;
                    self.xfer_done = 0;
                    self.transfer_request = true;
                }
                self.cr1 = self.cd_stat() | STAT_TRANS;
                self.cr2 = 0x00CC;
                self.cr3 = 0x0000;
                self.cr4 = 0x0000;
                self.hirq |= HIRQ_CMOK | HIRQ_DRDY;
            }
            0x03 => {
                // Get session info (MAME `cmd_get_session_info`). CR1 low byte
                // selects which session; the BIOS reads CR3 (session count in
                // the high byte) and CR4. With a disc, session 0 ("total / disc
                // end") returns the lead-out FAD; otherwise the disc start.
                // (MAME warns CR4 must be > 1 and < 100 or the BIOS rejects the
                // no-disc default — hence CR3=0x0100, CR4=0 there.)
                let which = (self.cr1 & 0xFF) as u8;
                self.status = STAT_PAUSE;
                self.cr1 = (self.status as u16) << 8;
                self.cr2 = 0x0000;
                match (&self.disc, which) {
                    (Some(d), 0) => {
                        let lo = d.lead_out_fad();
                        self.cr3 = 0x0100 | ((lo >> 16) & 0xFF) as u16;
                        self.cr4 = lo as u16;
                    }
                    _ => {
                        self.cr3 = 0x0100;
                        self.cr4 = 0x0000;
                    }
                }
                self.hirq |= HIRQ_CMOK;
            }
            0x04 => {
                // Initialize CD system (MAME `cmd_init_cdsystem`): clears
                // DRDY/BFUL/PEND from HIRQ (`& 0xFFE5`). The disc-change is a
                // *one-shot*: the FIRST Init after an insert reports DCHG and
                // acknowledges it (`disk_changed = false`); later Inits clear
                // DCHG. Re-raising DCHG on every Init (the old behaviour, since
                // `disk_changed` was never cleared) made the BIOS perceive a
                // continuous disc swap and park in its CD control panel instead
                // of auto-booting a recognised game disc.
                //
                // With a disc present (not NODISC), Init returns the drive to
                // PAUSE at the start of track 1 and clears the play range — so
                // a prior Seek that stopped the drive (STANDBY) is undone and
                // the next probe pass finds a ready drive (MAME `cmd_init_cdsystem`).
                // During recognition spin-up (`DrivePhase::Startup`) a host-level
                // Init resets the buffer/filter engine but must NOT park the
                // physical pickup — the drive keeps reporting BUSY until spin-up
                // completes (Mednafen leaves the `DrivePhase` running across the
                // Init). Unconditionally parking it here is precisely what
                // cancelled the BUSY window and skipped the boot animation when an
                // earlier "report BUSY" attempt was tried (and then reverted).
                if self.disc.is_some() && self.drive_phase != DrivePhase::Startup {
                    self.drive_idle();
                    self.status = STAT_PAUSE;
                    self.cd_curfad = FAD_OFFSET;
                    self.drive_sector = FAD_OFFSET;
                    // Init re-seats the head at track 1 with nothing read yet →
                    // `is_cdrom` clears (Mednafen cdb.cpp:4051) until the next
                    // PLAY actually reads a data sector.
                    self.is_cdrom = false;
                }
                self.buf_full = false;
                self.cd_speed = if self.cr1 & 0x10 != 0 { 1 } else { 2 };
                self.cd_report();
                let mut h = self.hirq & 0xFFE5;
                if self.disk_changed {
                    h |= HIRQ_DCHG;
                    self.disk_changed = false;
                } else {
                    h &= !HIRQ_DCHG;
                }
                // Mednafen's CD-block reset raises the full "end of <op>" set —
                // CMOK|DCHG|ESEL|EHST|MPED|ECPY|EFLS = 0x0BE1 (cdb.cpp:4075) —
                // and the BIOS disc-recognition loop polls HIRQ into its shadow
                // [0x060003A4] and waits for that mask. We previously raised only
                // CMOK|ESEL (+DCHG), so the shadow topped out at 0x0EE1 (missing
                // **ECPY 0x100**, which ours otherwise sets only on auth) — the
                // wait never completed and recognition looped/gave up. Raise the
                // same set Mednafen's reset does so recognition can proceed.
                self.hirq = h | HIRQ_CMOK | HIRQ_ESEL | HIRQ_EHST | HIRQ_ECPY | HIRQ_EFLS;
            }
            0x06 => {
                // End data transfer (MAME `cmd_end_data_transfer`): clear the
                // TRANS status bit and report the number of *bytes* the host
                // read back, as a 24-bit count split across CR1 (MSB) / CR2
                // (low 16 bits, in words). `xfer_pos` is the FIFO/DMA byte
                // cursor advanced by the host's reads. When nothing was
                // transferred, return the 0xFF / 0xFFFF "no xfer" sentinel.
                // The BIOS reads this count to confirm a staged transfer (e.g.
                // the TOC) actually completed before it proceeds with boot.
                let xferd = self.xfer_done as u32;
                self.transfer_request = false;
                if xferd != 0 {
                    self.cr1 = self.cd_stat() | ((xferd >> 17) & 0xFF) as u16;
                    self.cr2 = ((xferd >> 1) & 0xFFFF) as u16;
                } else {
                    self.cr1 = self.cd_stat() | 0x00FF;
                    self.cr2 = 0xFFFF;
                }
                self.cr3 = 0x0000;
                self.cr4 = 0x0000;
                // A Get-and-Delete transfer (0x63) frees the sectors it covered
                // when the transfer ends (MAME `cmd_end_data_transfer`,
                // `XFERTYPE32_GETDELETESECTOR`): the host reads exactly the
                // requested sector count via the data port and never over-reads,
                // so the lazy free in `read_data_port32` — which only fires on a
                // read *past* the end — does not run. Free them here instead.
                // Without this the transferred sectors (e.g. the 16 IP.BIN
                // sectors) linger in the partition and are prepended to the next
                // file read, shifting the loaded 1st-read program that many
                // sectors so the BIOS jumps into stale IP.BIN data and crashes.
                if let Some(x) = self.xfer32.take()
                    && x.delete
                {
                    self.delete_partition_sectors(x.part, x.pos, x.num);
                }
                self.xfer.clear();
                self.xfer_pos = 0;
                self.xfer_done = 0;
                self.xfer32 = None;
                self.hirq |= HIRQ_CMOK | HIRQ_EHST;
            }
            0x30 => {
                // Set CD device connection: CR3 high byte = filter (0xFF=none).
                self.cd_device_filter = (self.cr3 >> 8) as u8;
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            }
            0x31 => {
                // Get CD device connection.
                self.cr1 = self.cd_stat();
                self.cr2 = 0;
                self.cr3 = (self.cd_device_filter as u16) << 8;
                self.cr4 = 0;
                self.hirq |= HIRQ_CMOK;
            }
            0x32 => {
                // Get last buffer destination.
                self.cr1 = self.cd_stat();
                self.cr2 = 0;
                self.cr3 = (self.last_buffer as u16) << 8;
                self.cr4 = 0;
                self.hirq |= HIRQ_CMOK;
            }
            0x40 => {
                // Set filter range: FAD0 = (CR1&0xFF)<<16|CR2,
                // range = (CR3&0xFF)<<16|CR4; CR3 high = filter #.
                let f = ((self.cr3 >> 8) & 0xFF) as usize;
                if f < MAX_FILTERS {
                    self.filters[f].fad = ((self.cr1 as u32 & 0xFF) << 16) | self.cr2 as u32;
                    self.filters[f].range = ((self.cr3 as u32 & 0xFF) << 16) | self.cr4 as u32;
                }
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            }
            0x42 => {
                // Set filter subheader conditions.
                let f = ((self.cr3 >> 8) & 0xFF) as usize;
                if f < MAX_FILTERS {
                    let fl = &mut self.filters[f];
                    fl.chan = self.cr1 as u8;
                    fl.smmask = (self.cr2 >> 8) as u8;
                    fl.cimask = self.cr2 as u8;
                    fl.fid = self.cr3 as u8;
                    fl.smval = (self.cr4 >> 8) as u8;
                    fl.cival = self.cr4 as u8;
                }
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            }
            0x43 => {
                // Get filter subheader conditions.
                let f = (((self.cr3 >> 8) & 0xFF) as usize).min(MAX_FILTERS - 1);
                let fl = self.filters[f].clone();
                self.cr1 = self.cd_stat() | fl.chan as u16;
                self.cr2 = ((fl.smmask as u16) << 8) | fl.cimask as u16;
                self.cr3 = fl.fid as u16;
                self.cr4 = ((fl.smval as u16) << 8) | fl.cival as u16;
                self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            }
            0x44 => {
                // Set filter mode (CR1 low; bit 7 = re-initialise the filter).
                let f = ((self.cr3 >> 8) & 0xFF) as usize;
                let mode = self.cr1 as u8;
                if f < MAX_FILTERS {
                    if mode & 0x80 != 0 {
                        self.filters[f] = Filter::default();
                    } else {
                        self.filters[f].mode = mode;
                    }
                }
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            }
            0x45 => {
                // Get filter mode.
                let f = (((self.cr3 >> 8) & 0xFF) as usize).min(MAX_FILTERS - 1);
                self.cr1 = self.cd_stat() | self.filters[f].mode as u16;
                self.cr2 = 0;
                self.cr3 = 0;
                self.cr4 = 0;
                self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            }
            0x46 => {
                // Set filter connection: CR1 bit0=true cond, bit1=false cond.
                let f = ((self.cr3 >> 8) & 0xFF) as usize;
                if f < MAX_FILTERS {
                    if self.cr1 & 1 != 0 {
                        self.filters[f].condtrue = (self.cr2 >> 8) as u8;
                    }
                    if self.cr1 & 2 != 0 {
                        self.filters[f].condfalse = self.cr2 as u8;
                    }
                }
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            }
            0x48 => self.reset_selector(),
            0x50 => {
                // Get buffer size: free blocks, max block size words, total.
                self.cr1 = self.cd_stat();
                self.cr2 = self.free_blocks.clamp(0, MAX_BLOCKS as i32) as u16;
                self.cr3 = 0x1800;
                self.cr4 = MAX_BLOCKS as u16;
                self.hirq |= HIRQ_CMOK;
            }
            0x51 => {
                // Get buffer partition sector number (CR4 = block count).
                let buf = (self.cr3 >> 8) as usize;
                self.cr1 = self.cd_stat();
                self.cr2 = 0;
                self.cr3 = 0;
                self.cr4 = self
                    .partitions
                    .get(buf)
                    .map_or(0, |p| p.blocks.len() as u16);
                self.hirq |= HIRQ_CMOK;
            }
            0x52 => {
                // Calculate actual data size (in words) over a sector range.
                let buf = (self.cr3 >> 8) as usize;
                let offs = self.cr2 as usize;
                let num = self.cr4 as usize;
                self.calcsize = 0;
                if let Some(p) = self.partitions.get(buf) {
                    let idxs: Vec<usize> = p.blocks.clone();
                    for i in 0..num {
                        if let Some(&b) = idxs.get(offs + i) {
                            self.calcsize += (self.blocks[b].size.max(0) as u32) / 2;
                        }
                    }
                }
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            }
            0x53 => {
                // Get actual data size (result of the last 0x52).
                self.cr1 = self.cd_stat() | ((self.calcsize >> 16) & 0xFF) as u16;
                self.cr2 = self.calcsize as u16;
                self.cr3 = 0;
                self.cr4 = 0;
                self.hirq |= HIRQ_CMOK;
            }
            0x54 => {
                // Get sector information for one buffered sector.
                let offs = (self.cr2 & 0xFF) as usize;
                let buf = (self.cr3 >> 8) as usize;
                let blk = self
                    .partitions
                    .get(buf)
                    .and_then(|p| p.blocks.get(offs).copied());
                match blk {
                    Some(b) => {
                        let (fad, fnum, chan, subm, cinf) = {
                            let bl = &self.blocks[b];
                            (bl.fad, bl.fnum, bl.chan, bl.subm, bl.cinf)
                        };
                        self.cr1 = self.cd_stat() | ((fad >> 16) & 0xFF) as u16;
                        self.cr2 = (fad & 0xFFFF) as u16;
                        self.cr3 = ((fnum as u16) << 8) | chan as u16;
                        self.cr4 = ((subm as u16) << 8) | cinf as u16;
                    }
                    None => self.cr1 |= STAT_REJECT,
                }
                self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            }
            0x10 => {
                // Play disc (Mednafen `COMMAND_PLAY`, cdb.cpp:2802): start =
                // (CR1&0xFF)<<16|CR2, end = (CR3&0xFF)<<16|CR4, play-mode = CR3>>8.
                // Bit 0x800000 = FAD addressing; when both start and end are FAD,
                // the end field is a *sector count* added to the start
                // (cdb.cpp:2813). A lone 0xFFFFFF reuses the prior position.
                let mut start = ((self.cr1 as u32 & 0xFF) << 16) | self.cr2 as u32;
                let mut end = ((self.cr3 as u32 & 0xFF) << 16) | self.cr4 as u32;
                let pm = (self.cr3 >> 8) as u8;
                if start == 0xFF_FFFF {
                    start = self.cur_play_start;
                }
                if end == 0xFF_FFFF {
                    end = self.cur_play_end;
                } else if (start & 0x80_0000) != 0 && (end & 0x80_0000) != 0 {
                    end = 0x80_0000 | ((start.wrapping_add(end)) & 0x7F_FFFF);
                }
                // Mixed FAD/track addressing forms are rejected with the play
                // state untouched (Mednafen cdb.cpp:2830: `((psp ^ pep) &
                // 0x800000) && pep != 0` → `CDStatusResults(true)`). VF2's
                // character-select BGM setup issues such a Play; accepting it
                // derailed the drive onto a stale range.
                if (start ^ end) & 0x80_0000 != 0 && end != 0 {
                    if std::env::var("SAT_CDSEEKLOG").is_ok() {
                        eprintln!("CDPLAY-REJECT start={start:06X} end={end:06X}");
                    }
                    self.cd_report();
                    self.cr1 = STAT_REJECT;
                    self.hirq |= HIRQ_CMOK;
                    return;
                }
                let repeat = if pm & 0x70 == 0 {
                    pm & 0x0F
                } else {
                    self.cur_play_repeat
                };
                self.sectorstore = false;
                // BUSY now (even with play-mode 0x80); the seek machine drives
                // BUSY → SEEK → PLAY and raises PEND at the range end. Mode
                // bit 7 = "no pickup change" (see `start_seek_impl`).
                self.status = STAT_BUSY;
                self.cd_report();
                self.start_seek_impl(start, end, repeat, HIRQ_PEND, pm & 0x80 != 0);
                self.hirq |= HIRQ_CMOK;
            }
            0x11 => {
                // Disc seek (MAME `cmd_seek_disc`). CR1 bit 0x80 = FAD seek
                // (0xFFFFFF = pause in place); otherwise a track seek where the
                // track is CR2's high byte. A track-0 / invalid seek is the
                // drive-stop idiom → STANDBY: the BIOS issues it to halt the
                // drive between boot probe passes and waits for STANDBY before
                // continuing, so leaving the status at PAUSE (the old default
                // handler) stalled the boot loop and dropped to the CD shell.
                //
                // A Seek cancels any in-flight play/read and parks the phase
                // machine; it positions the head and pauses (no read range).
                self.drive_idle();
                if self.cr1 & 0x80 != 0 {
                    let temp = ((self.cr1 as u32 & 0xFF) << 16) | self.cr2 as u32;
                    if temp == 0xFF_FFFF {
                        self.status = STAT_PAUSE;
                    } else {
                        self.cd_curfad = ((self.cr1 as u32 & 0x7F) << 16) | self.cr2 as u32;
                        self.status = STAT_PAUSE;
                    }
                } else if self.cr2 >> 8 != 0 {
                    self.status = STAT_PAUSE;
                    self.track = (self.cr2 >> 8) as u8;
                } else {
                    // Stop (Seek target 0): the drive halts. Match Mednafen's
                    // STOP geometry (cdb.cpp:2846) — every position field goes
                    // to the "no position" sentinel (0xFF / FAD 0xFFFFFF) and
                    // the repeat count to 0x7F — so the BIOS's post-stop
                    // GetStatus poll sees the expected stopped geometry
                    // (CR2=0xFFFF, CR3=0xFFFF, CR4=0xFFFF) and proceeds, rather
                    // than reading stale track/ctrl-adr fields and looping.
                    self.status = STAT_STANDBY;
                    self.cd_curfad = 0xFFFF_FFFF;
                    self.track = 0xFF;
                    self.ctrladdr = 0xFF;
                    self.index = 0xFF;
                    self.repcnt = 0x7F;
                }
                self.cd_report();
                self.hirq |= HIRQ_CMOK;
            }
            0x20 => {
                // Get Subcode (Mednafen cdb.cpp `COMMAND_GET_SUBCODE`): CR1
                // low byte = type (0 = channel Q, 1 = channels R–W); stages
                // the subcode bytes for a FIFO transfer like GetToc, with the
                // word count in CR2 and the DTREQ bit (our `STAT_TRANS`) in
                // the status. VF2's intro polls the Q channel while its
                // 1-sector probe Plays seek. Types >= 2 are rejected.
                match self.cr1 & 0xFF {
                    0 => {
                        // Q: 10 bytes / 5 words — [ctrl/adr, tno, idx,
                        // rel-FAD (3 bytes), 0, abs-FAD (3 bytes)] from the
                        // current head geometry, binary not BCD (Mednafen
                        // `SubCodeQBuf`, cdb.cpp:2525).
                        let fad = self.cd_curfad;
                        let rel = self
                            .disc
                            .as_ref()
                            .and_then(|d| d.track_at_fad(fad))
                            .map_or(0, |t| fad.wrapping_sub(t.start_fad));
                        self.xfer = vec![
                            self.ctrladdr,
                            self.track,
                            self.index,
                            (rel >> 16) as u8,
                            (rel >> 8) as u8,
                            rel as u8,
                            0,
                            (fad >> 16) as u8,
                            (fad >> 8) as u8,
                            fad as u8,
                        ];
                        self.xfer_pos = 0;
                        self.xfer_done = 0;
                        self.transfer_request = true;
                        self.cr1 = self.cd_stat();
                        self.cr2 = 0x0005;
                        self.cr3 = 0x0000;
                        self.cr4 = 0x0000;
                        self.hirq |= HIRQ_CMOK | HIRQ_DRDY;
                    }
                    1 => {
                        // R–W: 24 bytes / 12 words; content unimplemented in
                        // the reference too (Mednafen fills 0xFF).
                        self.xfer = vec![0xFF; 24];
                        self.xfer_pos = 0;
                        self.xfer_done = 0;
                        self.transfer_request = true;
                        self.cr1 = self.cd_stat();
                        self.cr2 = 0x000C;
                        self.cr3 = 0x0000;
                        self.cr4 = 0x0000;
                        self.hirq |= HIRQ_CMOK | HIRQ_DRDY;
                    }
                    _ => {
                        self.cd_report();
                        self.cr1 = STAT_REJECT;
                        self.hirq |= HIRQ_CMOK;
                    }
                }
            }
            0x60 => {
                // Set sector length (CR1 low = input code, CR2 high = output).
                let len = |code: u16| match code {
                    0 => 2048,
                    1 => 2336,
                    2 => 2340,
                    3 => 2352,
                    _ => 0,
                };
                let lin = len(self.cr1 & 0xFF);
                if lin != 0 {
                    self.sectlenin = lin;
                }
                let lout = len((self.cr2 >> 8) & 0xFF);
                if lout != 0 {
                    self.sectlenout = lout;
                }
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_ESEL;
            }
            0x61 | 0x63 => {
                // Get (and optionally delete) sector data: set up a 32-bit
                // transfer over a partition's blocks; the host reads the data
                // port. CR4 = count (0xFFFF = all from offset), CR2 = offset.
                let delete = command == 0x63;
                let bufnum = (self.cr3 >> 8) as usize;
                let mut sectnum = self.cr4 as usize;
                let sectofs = self.cr2 as usize;
                let avail = self.partitions.get(bufnum).map_or(0, |p| p.blocks.len());
                // Reject the whole request if the buffer is invalid or the
                // [offset, offset+count) range escapes the partition — the
                // transfer indexes `blocks[pos + sect]` directly, so an
                // offset-aware bound is required (count-only let a non-zero
                // offset over-read past the end). 0xFFFF = "all from offset".
                if bufnum >= MAX_FILTERS
                    || sectofs > avail
                    || (sectnum != 0xFFFF && sectofs + sectnum > avail)
                {
                    self.cr1 = STAT_REJECT;
                    self.cr2 = 0;
                    self.cr3 = 0;
                    self.cr4 = 0;
                    self.hirq |= HIRQ_CMOK | HIRQ_EHST;
                } else {
                    if sectnum == 0xFFFF {
                        sectnum = avail.saturating_sub(sectofs);
                    }
                    self.xfer32 = Some(Xfer32 {
                        delete,
                        part: bufnum,
                        pos: sectofs,
                        num: sectnum,
                        sect: 0,
                        offs: 0,
                    });
                    self.xfer_done = 0;
                    self.transfer_request = true;
                    self.cd_report();
                    self.cr1 |= STAT_TRANS;
                    self.hirq |= HIRQ_CMOK | HIRQ_EHST | HIRQ_DRDY;
                }
            }
            0x62 => {
                // Delete sector data: free a range of a partition's blocks.
                let bufnum = (self.cr3 >> 8) as usize;
                let mut sectnum = self.cr4 as usize;
                let sectofs = self.cr2 as usize;
                let avail = self.partitions.get(bufnum).map_or(0, |p| p.blocks.len());
                // Same offset-aware bound as 0x61/0x63 (the delete itself clamps,
                // but reject an out-of-range request like the hardware rather
                // than silently truncating it).
                if bufnum >= MAX_FILTERS
                    || avail == 0
                    || sectofs > avail
                    || (sectnum != 0xFFFF && sectofs + sectnum > avail)
                {
                    self.cr1 = STAT_REJECT;
                    self.cr2 = 0;
                    self.cr3 = 0;
                    self.cr4 = 0;
                    self.hirq |= HIRQ_CMOK | HIRQ_EHST;
                } else {
                    if sectnum == 0xFFFF {
                        sectnum = avail.saturating_sub(sectofs);
                    }
                    self.delete_partition_sectors(bufnum, sectofs, sectnum);
                    self.cd_report();
                    self.hirq |= HIRQ_CMOK | HIRQ_EHST;
                }
            }
            0x70 => {
                // Change directory: CR3 low + CR4 = file id (0xFFFFFF = root).
                let temp = ((self.cr3 as u32 & 0xFF) << 16) | self.cr4 as u32;
                self.read_new_dir(temp);
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_EFLS;
            }
            0x71 => {
                // Read directory: just (re)connect the filter for the read.
                let f = (self.cr3 >> 8) as u8;
                self.cd_device_filter = if (f as usize) < MAX_FILTERS {
                    f
                } else {
                    NO_FILTER
                };
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_EFLS;
            }
            0x72 => {
                // Get file-system scope: file count + first file id.
                self.cr1 = self.cd_stat();
                self.cr2 = self.numfiles as u16;
                self.cr3 = 0x0100;
                self.cr4 = self.firstfile as u16;
                self.hirq |= HIRQ_CMOK | HIRQ_EFLS;
            }
            0x73 => {
                // Get target file info: stage a 12-byte record for the host to
                // read through the FIFO (FAD, length, gap/unit size, id, flags).
                let temp = ((self.cr3 as u32 & 0xFF) << 16) | self.cr4 as u32;
                if temp != 0xFF_FFFF {
                    if let Some(e) = self.curdir.get(temp as usize) {
                        let mut f = vec![0u8; 12];
                        f[0..4].copy_from_slice(&e.firstfad.to_be_bytes());
                        f[4..8].copy_from_slice(&e.length.to_be_bytes());
                        f[8] = e.interleave_gap_size;
                        f[9] = e.file_unit_size;
                        f[10] = temp as u8;
                        f[11] = e.flags;
                        self.xfer = f;
                        self.xfer_pos = 0;
                    }
                    self.xfer_done = 0;
                    self.transfer_request = true;
                    self.cr1 = self.cd_stat() | STAT_TRANS;
                    self.cr2 = 6; // 6 words for a single file
                } else {
                    self.xfer_done = 0;
                    self.transfer_request = true;
                    self.cr1 = self.cd_stat() | STAT_TRANS;
                    self.cr2 = 0x5F4; // all entries (whole-directory form)
                }
                self.cr3 = 0;
                self.cr4 = 0;
                self.hirq |= HIRQ_CMOK | HIRQ_DRDY;
            }
            0x74 => {
                // Read file: start the read pump over a file's sectors.
                let file_offset = ((self.cr1 as u32 & 0xFF) << 8) | (self.cr2 as u32 & 0xFF);
                let file_filter = (self.cr3 >> 8) as u8;
                let file_id = ((self.cr3 as u32 & 0xFF) << 16) | self.cr4 as u32;
                if let Some(e) = self.curdir.get(file_id as usize) {
                    let nsect = e.length.div_ceil(self.sectlenin.max(1));
                    let start_fad = e.firstfad + file_offset;
                    let sec_count = nsect.saturating_sub(file_offset);
                    self.cd_device_filter = if (file_filter as usize) < MAX_FILTERS {
                        file_filter
                    } else {
                        NO_FILTER
                    };
                    self.sectorstore = false;
                    // Read File ends the range with EFLS, not PEND (Mednafen
                    // `COMMAND_READ_FILE`, cdb.cpp:3920).
                    self.status = STAT_BUSY;
                    self.cd_report();
                    self.start_seek(
                        0x80_0000 | (start_fad & 0x7F_FFFF),
                        0x80_0000 | (start_fad.wrapping_add(sec_count) & 0x7F_FFFF),
                        0,
                        HIRQ_EFLS,
                    );
                } else {
                    self.cd_report();
                }
                self.hirq |= HIRQ_CMOK | HIRQ_EHST;
            }
            0x75 => {
                // Abort file: stop any read / transfer, return the drive to idle.
                // With a disc that's PAUSE; with an empty tray the drive stays
                // NODISC — AbortFile is a buffer/transfer abort, not a physical
                // drive operation, so it must not fabricate a disc-present status.
                // (The old unconditional `STAT_PAUSE` clobbered NODISC→PAUSE on
                // the no-disc CD-player panel, so the BIOS perceived a disc and
                // never settled to the live "no disc" idle state. Same disc-guard
                // as the auth handler below.)
                self.drive_idle();
                if self.disc.is_some() {
                    self.status = STAT_PAUSE;
                }
                self.xfer32 = None;
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_EFLS;
            }
            0xE0 => {
                // Check copy protection (authentication). A genuine Saturn data
                // disc succeeds: raise MAME's auth HIRQ pattern 0x07C5
                // (CMOK|CSCT|ESEL|EHST|ECPY|EFLS|SCDQ — ECPY = auth done) so the
                // BIOS proceeds to read the IP and boot. MPEG card / no disc
                // just acknowledge.
                let mpeg = self.cr2 == 0x0001;
                if self.disc.is_some() {
                    self.status = STAT_PAUSE;
                }
                if !mpeg && self.disc.is_some() {
                    self.sectorstore = true;
                    self.hirq = 0x07C5 | HIRQ_MPED; // 0x0FC5 (MPED held, no MPEG card)
                } else {
                    self.hirq |= HIRQ_CMOK;
                }
                self.cd_report();
            }
            0xE1 => {
                // Get disc region: 4 = Saturn data disc, 2 = MPEG, 0 = no CD.
                // The BIOS gates booting on this being a Saturn disc.
                let mpeg = self.cr2 == 0x0001;
                self.cr1 = self.cd_stat();
                self.cr2 = if mpeg {
                    0x0002
                } else if self.disc.is_some() {
                    0x0004
                } else {
                    0x0000
                };
                self.cr3 = 0;
                self.cr4 = 0;
                self.hirq |= HIRQ_CMOK;
            }
            0x67 => {
                // Get copy/move error: report "no error" — CR1 = status,
                // CR2=CR3=CR4=0 (Mednafen `cdb.cpp` returns 0x0100,0,0,0). The
                // default status report's non-zero geometry (CR2-4 = ctrl/track
                // /index/FAD) was read by the BIOS recognition code as a copy
                // error, making it loop recognition (re-Init / GetCopyError ~6×)
                // instead of proceeding once.
                self.cr1 = self.cd_stat();
                self.cr2 = 0;
                self.cr3 = 0;
                self.cr4 = 0;
                self.hirq |= HIRQ_CMOK;
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
        // Disc present: drive the faithful `cdb.cpp` phase machine (seek →
        // play with the read-ahead pipeline, status sequence, and the
        // periodic/SCDQ + end-of-range IRQ timing the host polls).
        if self.disc.is_some() {
            self.drive_run(cycles);
            return;
        }
        // No disc: keep the bespoke idle periodic-liveness cadence the no-disc
        // BIOS splash boot depends on (the `bios_boot` golden path) — the phase
        // machine only governs disc reads.
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
        // with a command (see `host_initialized`); never clobber an unread
        // command response (`command_pending`, cs2.c's `_command`); and never
        // clobber a command the host is mid-way through composing
        // (`cr_written != 0` — see the matching guard in `drive_periodic`).
        if !self.host_initialized || self.command_pending || self.cr_written != 0 {
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
        assert_eq!(c.read16(0x0018), b'C' as u16);
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
        c.hirq = HIRQ_CMOK | HIRQ_DRDY | HIRQ_DCHG | HIRQ_MPED;
        // Clear CMOK by writing a word with CMOK = 0, others = 1.
        c.write16(0x0008, !HIRQ_CMOK);
        assert_eq!(c.hirq, HIRQ_DRDY | HIRQ_DCHG | HIRQ_MPED);
        // Writing all-ones clears nothing.
        c.write16(0x0008, 0xFFFF);
        assert_eq!(c.hirq, HIRQ_DRDY | HIRQ_DCHG | HIRQ_MPED);
        // MPED is held even if the host writes a 0 to it (no MPEG card).
        c.write16(0x0008, !HIRQ_MPED);
        assert_eq!(c.hirq & HIRQ_MPED, HIRQ_MPED, "MPED stays set (no MPEG)");
    }

    #[test]
    fn get_status_command_returns_no_disc_report_and_cmok() {
        let mut c = CdBlock::new();
        c.hirq = 0;
        // Command 0x00 (Get Status): write CR1 high byte = 0x00, then CR2-4.
        c.write16(0x0018, 0x0000);
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000); // triggers execute
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK);
        // No-disc NODISC report (MAME `cr_standard_return`, no image): status
        // NODISC (0x07) in CR1, zero geometry in CR2..CR4.
        assert_eq!(c.read16(0x0018), 0x0700);
        assert_eq!(c.read16(0x001C), 0x0000);
        assert_eq!(c.read16(0x0020), 0x0000);
        assert_eq!(c.read16(0x0024), 0x0000);
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
        assert_eq!(c.read16(0x0018), b'C' as u16);
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
        assert_eq!(c.read16(0x0018) >> 8, (STAT_NODISC | STAT_PERI) as u16);
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
        assert_eq!(c.read16(0x0018) >> 8, (STAT_NODISC | STAT_PERI) as u16);
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
        assert_eq!(fine.read16(0x0018) >> 8, (STAT_NODISC | STAT_PERI) as u16);
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
        assert_eq!(c.read16(0x0018) >> 8, (STAT_NODISC | STAT_PERI) as u16);
    }

    #[test]
    fn get_hardware_info_reports_drive_revision() {
        let mut c = CdBlock::new();
        c.write16(0x0018, 0x0100); // command 0x01 in high byte
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000); // CR4 completes the set → trigger
        assert_eq!(c.read16(0x001C), 0x0002); // CR2: hw flags (no MPEG) / version
        assert_eq!(c.read16(0x0024), 0x0600); // CR4: drive info (Mednafen value)
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

    use crate::disc::Disc;

    /// A 4-sector raw-ISO disc (one Mode-1 data track from FAD 150).
    fn iso_disc() -> Disc {
        Disc::from_iso(vec![0u8; 2048 * 4])
    }

    /// A 2-sector Red Book audio disc, with known PCM in the first frame.
    fn audio_disc() -> Disc {
        let mut bin = vec![0u8; 2352 * 2];
        bin[0..2].copy_from_slice(&0x1234i16.to_le_bytes()); // L, frame 0
        bin[2..4].copy_from_slice(&(-2i16).to_le_bytes()); // R, frame 0
        Disc::from_cue(
            "FILE \"a.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
            |_| Some(bin.clone()),
        )
        .expect("audio cue")
    }

    #[test]
    fn cdda_audio_track_decodes_to_stereo_pcm() {
        let mut c = CdBlock::new();
        c.insert_disc(audio_disc());
        // Play the audio track from FAD 150; one play_data tick = one sector.
        c.status = STAT_PLAY;
        c.cd_curfad = FAD_OFFSET;
        c.fadstoplay = 2;
        c.play_data();
        let pcm = c.take_cd_audio(1176); // 588 interleaved stereo frames
        assert_eq!(pcm.len(), 1176);
        assert_eq!(pcm[0], 0x1234, "left, frame 0");
        assert_eq!(pcm[1], -2, "right, frame 0");
        // CDDA is mixed to audio, not buffered as data for the host to read.
        assert!(!c.sectorstore);
    }

    #[test]
    fn take_cd_audio_pads_with_silence_when_empty() {
        let mut c = CdBlock::new();
        assert_eq!(c.take_cd_audio(8), vec![0i16; 8]);
    }

    /// Demonstration (manual): our CD-block decodes the REAL Doukyuusei disc's
    /// Track 2 (Red Book audio — the "this disc is a SEGA Saturn game" warning)
    /// to the exact PCM you hear from
    /// `aplay -f S16_LE -r 44100 -c 2 "…(Track 2).bin"`. This proves the
    /// CDDA→SCSP path produces real, faithful sound **today** — the drive plays
    /// the disc bit-for-bit; the only thing missing for the in-panel experience
    /// is the BIOS issuing the Play command (the LLE-68k/trigger wall). Dumps our
    /// own output to `/tmp/ours_track2.pcm` so it can be played back and compared:
    ///   aplay -f S16_LE -r 44100 -c 2 /tmp/ours_track2.pcm
    #[test]
    #[ignore = "manual: decode the real disc's CD-DA track (needs roms/)"]
    fn cdda_plays_real_disc_track2_audio() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cue_name = "Doukyuusei - if (Japan) (1M, 2M).cue";
        let Ok(cue) = std::fs::read_to_string(root.join("roms").join(cue_name)) else {
            println!("no roms/{cue_name}; skipped");
            return;
        };
        let disc = Disc::from_cue(&cue, |name| std::fs::read(root.join("roms").join(name)).ok())
            .expect("parse the Doukyuusei cue");
        let audio = disc
            .tracks()
            .iter()
            .find(|t| matches!(t.mode, crate::disc::TrackMode::Audio))
            .expect("the disc has a Red Book audio track");
        let start_fad = audio.start_fad;
        println!(
            "audio track #{} at FAD {start_fad}, {} sectors",
            audio.number, audio.length
        );

        // Decode the whole warning track (75 sectors ≈ 1 s each). Expected PCM
        // straight off the disc image (the same bytes aplay played).
        let sectors = audio.length;
        let mut expect: Vec<i16> = Vec::with_capacity(sectors as usize * 1176);
        for i in 0..sectors {
            let sec = disc
                .read_full_sector(start_fad + i)
                .expect("audio sector present");
            expect.extend(sec.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
        }

        // Drive the CD-block's read pump exactly as a Play command would: one
        // audio sector per `play_data`, draining each to our CD-DA mixer output
        // as we go (so the ~1 s FIFO cap never drops a sector).
        let mut c = CdBlock::new();
        c.insert_disc(disc);
        c.status = STAT_PLAY;
        c.cd_curfad = start_fad;
        c.fadstoplay = sectors as i64;
        let mut pcm: Vec<i16> = Vec::with_capacity(sectors as usize * 1176);
        for _ in 0..sectors {
            c.play_data();
            pcm.extend(c.take_cd_audio(1176));
        }

        let peak = pcm.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0);
        assert!(peak > 1000, "CD-DA output is real audio (peak {peak}), not silence");
        assert_eq!(pcm, expect, "our CD-DA decode is byte-identical to the disc");

        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let _ = std::fs::write("/tmp/ours_track2.pcm", &bytes);
        println!(
            "decoded {} CD-DA samples, peak {peak} — faithful. wrote /tmp/ours_track2.pcm\n  \
             play it: aplay -f S16_LE -r 44100 -c 2 /tmp/ours_track2.pcm",
            pcm.len()
        );
    }

    #[test]
    fn insert_disc_flags_change_and_reports_real_geometry() {
        let mut c = CdBlock::new();
        c.hirq = 0;
        // A *runtime* swap (host already engaged) flags the disc change; at cold
        // boot the disc is just present (no DCHG) so the BIOS auto-boots it.
        c.host_initialized = true;
        c.insert_disc(iso_disc());
        assert!(c.has_disc());
        assert_eq!(c.hirq & HIRQ_DCHG, HIRQ_DCHG, "disc change flagged");
        // Get Status (cmd 0x00) now reports track 1 / data (0x41) / FAD 150.
        c.write16(0x0018, 0x0000);
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000);
        // CR1 = BUSY status (0x00) in the high byte: a freshly-inserted disc is
        // in the recognition spin-up (Mednafen `DRIVEPHASE_STARTUP`), reporting
        // STATUS_BUSY for ~1 s while the pickup spins up and reads the TOC,
        // *before* it settles to PAUSE — the BIOS plays its boot animation
        // during this window, which reporting PAUSE immediately (the old
        // behaviour this assertion used to lock in) skipped entirely. The
        // `is_cdrom` bit (0x80, low byte) is **read-based** (Mednafen
        // `CurPosInfo.is_cdrom`): a freshly-inserted disc has read no data
        // sector yet, so it is 0 here.
        assert_eq!(
            c.read16(0x0018),
            0x0000,
            "BUSY status (recognition spin-up), is_cdrom=0 (no read yet)"
        );
        assert_eq!(c.read16(0x001C), 0x4101, "ctrl/adr 0x41, track 1");
        assert_eq!(c.read16(0x0020), 0x0100, "index 1, FAD hi 0");
        assert_eq!(c.read16(0x0024), 0x0096, "FAD 150");
        // Once the read pump reads a data sector, `is_cdrom` latches to 1 and a
        // subsequent status report carries the bit (low byte 0x80).
        c.cd_curfad = FAD_OFFSET;
        c.fadstoplay = 1;
        c.status = STAT_PLAY;
        c.play_data();
        assert!(c.is_cdrom, "is_cdrom set after reading a data sector");
        c.write16(0x0018, 0x0000);
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000);
        assert_eq!(
            c.read16(0x0018) & 0x0080,
            0x0080,
            "is_cdrom bit now reported"
        );
    }

    #[test]
    fn get_toc_streams_the_toc_through_the_fifo() {
        let mut c = CdBlock::new();
        c.insert_disc(iso_disc());
        // Get TOC (cmd 0x02).
        c.write16(0x0018, 0x0200);
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000);
        assert_eq!(c.read16(0x001C), 0x00CC, "TOC length = 102 words");
        assert_eq!(c.hirq & HIRQ_DRDY, HIRQ_DRDY, "data ready");
        // The data FIFO streams the TOC: track 1 = 0x41,0x00,0x00,0x96.
        assert_eq!(c.read16(0x8000), 0x4100); // ctrl/adr + FAD hi
        assert_eq!(c.read16(0x8000), 0x0096); // FAD lo
        // Entry 99 (first track) begins at byte 396 = word 198.
        for _ in 2..198 {
            let _ = c.read16(0x8000);
        }
        assert_eq!(c.read16(0x8000), 0x4101, "first-track meta: ctrl 0x41, #1");
    }

    #[test]
    fn get_session_returns_the_lead_out_fad() {
        let mut c = CdBlock::new();
        c.insert_disc(iso_disc()); // lead-out FAD = 150 + 4 = 154
        // Get Session, session 0 (total / disc end): CR1 = 0x0300.
        c.write16(0x0018, 0x0300);
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000);
        assert_eq!(c.read16(0x0020), 0x0100, "1 session, lead-out FAD hi 0");
        assert_eq!(c.read16(0x0024), 154, "lead-out FAD 154");
    }

    /// Issue a full 4-CR command (high byte of CR1 = command) and run it.
    fn cmd(c: &mut CdBlock, cr1: u16, cr2: u16, cr3: u16, cr4: u16) {
        c.write16(0x0018, cr1);
        c.write16(0x001C, cr2);
        c.write16(0x0020, cr3);
        c.write16(0x0024, cr4);
    }

    /// Parse the real VF2 disc's ISO9660 root directory and dump it, so we can
    /// confirm the FS parser finds the 1st-read file the CD-boot loader expects.
    #[test]
    #[ignore = "manual: dump the real VF2 disc's ISO9660 root directory"]
    fn vf2_iso9660_root_dir_dump() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let Ok(cue) = std::fs::read_to_string(root.join("roms/vf2_full.cue")) else {
            println!("no roms/vf2_full.cue; skipped");
            return;
        };
        let disc = match Disc::from_cue(&cue, |name| {
            std::fs::read(root.join("roms").join(name)).ok()
        }) {
            Ok(d) => d,
            Err(e) => {
                println!("cue parse failed: {e}");
                return;
            }
        };
        let mut c = CdBlock::new();
        c.insert_disc(disc);
        cmd(&mut c, 0x7000, 0x0000, 0x00FF, 0xFFFF); // ChangeDir -> root
        println!(
            "curroot: firstfad=0x{:X} length={} flags=0x{:02X}",
            c.curroot.firstfad, c.curroot.length, c.curroot.flags
        );
        println!("numfiles={} firstfile={}", c.numfiles, c.firstfile);
        for (i, e) in c.curdir.iter().enumerate() {
            println!(
                "  [{i:>2}] name={:<16?} firstfad=0x{:06X} len={:>8} flags=0x{:02X}",
                String::from_utf8_lossy(&e.name),
                e.firstfad,
                e.length,
                e.flags
            );
        }
    }

    #[test]
    fn set_and_get_filter_range_round_trips() {
        let mut c = CdBlock::new();
        // Set Filter Range (0x40) on filter 2: FAD0 = 0x012345, range = 0x000678.
        cmd(&mut c, 0x4001, 0x2345, 0x0200, 0x0678);
        assert_eq!(c.filters[2].fad, 0x01_2345);
        assert_eq!(c.filters[2].range, 0x00_0678);
        assert_eq!(c.hirq & HIRQ_ESEL, HIRQ_ESEL);
    }

    #[test]
    fn set_and_get_filter_subheader_and_mode() {
        let mut c = CdBlock::new();
        // Set Filter Subheader Conditions (0x42) on filter 1.
        cmd(&mut c, 0x4205, 0x1122, 0x0133, 0x4455);
        assert_eq!(c.filters[1].chan, 0x05);
        assert_eq!(c.filters[1].smmask, 0x11);
        assert_eq!(c.filters[1].cimask, 0x22);
        assert_eq!(c.filters[1].fid, 0x33);
        assert_eq!(c.filters[1].smval, 0x44);
        assert_eq!(c.filters[1].cival, 0x55);
        // Get Filter Subheader Conditions (0x43) reads them back.
        cmd(&mut c, 0x4300, 0x0000, 0x0100, 0x0000);
        assert_eq!(c.read16(0x0018) & 0xFF, 0x05); // chan in CR1 low
        assert_eq!(c.read16(0x001C), 0x1122); // smmask/cimask
        // Set Filter Mode (0x44): mode 0x07.
        cmd(&mut c, 0x4407, 0x0000, 0x0100, 0x0000);
        assert_eq!(c.filters[1].mode, 0x07);
        // Get Filter Mode (0x45).
        cmd(&mut c, 0x4500, 0x0000, 0x0100, 0x0000);
        assert_eq!(c.read16(0x0018) & 0xFF, 0x07);
    }

    #[test]
    fn cd_device_connection_round_trips() {
        let mut c = CdBlock::new();
        // Set CD Device Connection (0x30): connect drive to filter 3 (CR3 hi).
        cmd(&mut c, 0x3000, 0x0000, 0x0300, 0x0000);
        assert_eq!(c.cd_device_filter, 3);
        // Get CD Device Connection (0x31): filter # in CR3 high byte.
        cmd(&mut c, 0x3100, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.read16(0x0020) >> 8, 3);
    }

    #[test]
    fn get_buffer_size_reports_the_full_pool_when_idle() {
        let mut c = CdBlock::new();
        cmd(&mut c, 0x5000, 0x0000, 0x0000, 0x0000); // Get Buffer Size
        assert_eq!(c.read16(0x001C), MAX_BLOCKS as u16, "all blocks free");
        assert_eq!(c.read16(0x0024), MAX_BLOCKS as u16, "total blocks");
    }

    #[test]
    fn reset_selector_clears_filters_and_get_sector_info_rejects_when_empty() {
        let mut c = CdBlock::new();
        c.filters[0].fad = 0x1234;
        // Reset Selector (0x48) with CR1 bit 4: reset filter conditions.
        cmd(&mut c, 0x4810, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.filters[0].fad, 0, "filter FAD reset");
        assert_eq!(c.filters[0].range, 0xFFFF_FFFF, "filter range reset to all");
        // Get Sector Information (0x54) on an empty partition → REJECT in CR1.
        cmd(&mut c, 0x5400, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.read16(0x0018) & STAT_REJECT, STAT_REJECT);
    }

    /// A 4-sector ISO with recognisable longwords at the start of sectors 0/1.
    fn data_disc() -> Disc {
        let mut img = vec![0u8; 2048 * 4];
        img[0..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // FAD 150
        img[2048..2052].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]); // FAD 151
        Disc::from_iso(img)
    }

    /// Connect the drive to filter 0 (which, with default conditions, routes
    /// every sector to partition 0) and play `count` sectors from `fad`.
    fn play(c: &mut CdBlock, fad: u32, count: u32) {
        cmd(c, 0x3000, 0, 0x0000, 0); // Set CD device connection → filter 0
        let start = 0x80_0000 | fad;
        let end = 0x80_0000 | count;
        cmd(
            c,
            0x1000 | ((start >> 16) & 0xFF) as u16,
            (start & 0xFFFF) as u16,
            ((end >> 16) & 0xFF) as u16,
            (end & 0xFFFF) as u16,
        );
    }

    /// Run the drive-phase machine well past the faithful seek + read +
    /// PauseCounter latency so a test can assert the post-read end state. (The
    /// phased model no longer reads instantly on Play — it seeks first, reads
    /// via the one-sector read-ahead pipeline, then delays the end IRQ a couple
    /// periodics; `drive_run` is granularity-independent, so one big advance
    /// fires the whole internal event sequence.)
    fn pump(c: &mut CdBlock) {
        c.tick(12_000_000);
    }

    #[test]
    fn set_sector_length_decodes_size_codes() {
        let mut c = CdBlock::new();
        cmd(&mut c, 0x6003, 0x0300, 0x0000, 0x0000); // in=2352(3), out=2352(3)
        assert_eq!(c.sectlenin, 2352);
        assert_eq!(c.sectlenout, 2352);
        cmd(&mut c, 0x6000, 0x0000, 0x0000, 0x0000); // both back to 2048(0)
        assert_eq!(c.sectlenin, 2048);
    }

    #[test]
    fn play_pumps_sectors_into_a_partition_then_streams_the_data_port() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        play(&mut c, 150, 2);
        pump(&mut c);
        assert_eq!(c.partitions[0].blocks.len(), 2, "two sectors buffered");
        // Mask the periodic (PERI) flag the unsolicited report ORs in over the
        // pump — the drive-state bits are PAUSE.
        assert_eq!(
            c.status & !STAT_PERI,
            STAT_PAUSE,
            "paused after the read range"
        );
        assert_eq!(c.hirq & HIRQ_PEND, HIRQ_PEND, "PEND on range complete");
        // Get Sector Data: partition 0, offset 0, 2 sectors.
        cmd(&mut c, 0x6100, 0x0000, 0x0000, 0x0002);
        assert_eq!(c.hirq & HIRQ_DRDY, HIRQ_DRDY, "data ready");
        // Stream the 32-bit data port: sector 0's first longword, then sector 1.
        assert_eq!(c.read32(0x8000), 0xDEAD_BEEF);
        for _ in 1..512 {
            let _ = c.read32(0x8000); // rest of sector 0 (2048 B = 512 words)
        }
        assert_eq!(c.read32(0x8000), 0x1234_5678, "second sector");
    }

    #[test]
    fn get_and_delete_sector_data_frees_the_blocks_when_drained() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        play(&mut c, 150, 1);
        pump(&mut c);
        assert_eq!(c.partitions[0].blocks.len(), 1);
        let free_before = c.free_blocks;
        // Get-and-delete (0x63): 1 sector from partition 0.
        cmd(&mut c, 0x6300, 0x0000, 0x0000, 0x0001);
        // Drain the sector (512 longwords) then one more read to hit the end.
        for _ in 0..512 {
            let _ = c.read32(0x8000);
        }
        let _ = c.read32(0x8000); // past end → frees the blocks
        assert!(c.partitions[0].blocks.is_empty(), "partition emptied");
        assert_eq!(c.free_blocks, free_before + 1, "block returned to the pool");
    }

    #[test]
    fn end_data_transfer_frees_a_get_and_delete_that_was_not_over_read() {
        // Regression: the host reads exactly the sector count and never over-
        // reads the data port, so the lazy free in `read_data_port32` doesn't
        // fire — `0x06 EndDataTransfer` must free the Get-and-Delete blocks.
        // Without it the sectors linger and are prepended to the next read,
        // shifting a loaded 1st-read program (the VF2 boot crash).
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        play(&mut c, 150, 1);
        pump(&mut c);
        assert_eq!(c.partitions[0].blocks.len(), 1);
        let free_before = c.free_blocks;
        cmd(&mut c, 0x6300, 0x0000, 0x0000, 0x0001); // get-and-delete 1 sector
        for _ in 0..512 {
            let _ = c.read32(0x8000); // drain exactly one sector, no over-read
        }
        assert_eq!(
            c.partitions[0].blocks.len(),
            1,
            "still buffered before EndXfer"
        );
        cmd(&mut c, 0x0600, 0x0000, 0x0000, 0x0000); // End data transfer
        assert!(c.partitions[0].blocks.is_empty(), "EndXfer freed the block");
        assert_eq!(c.free_blocks, free_before + 1, "block returned to the pool");
    }

    #[test]
    fn data_port_alias_routes_through_the_bus() {
        use crate::Saturn;
        use sh2::bus::{AccessKind, Bus};
        let mut sat = Saturn::with_blank_bios();
        sat.insert_disc(data_disc());
        let cd = &mut sat.bus.cd_block;
        play(cd, 150, 1);
        pump(cd);
        cmd(cd, 0x6100, 0x0000, 0x0000, 0x0001); // Get Sector Data
        // The SCU-DMA data-port alias at 0x0581_8000 streams the same bytes.
        let (w, _) = sat.bus.read32(0x0581_8000, AccessKind::Data);
        assert_eq!(w, 0xDEAD_BEEF);
    }

    /// A minimal ISO9660 disc: PVD at FAD 166 → root dir at FAD 167 with
    /// `.`, `..`, and one file `X` (FAD 170, 2048 B = 0xCAFEBABE…).
    fn fs_disc() -> Disc {
        let mut img = vec![0u8; 2048 * 21];
        let put = |img: &mut [u8], off: usize, v: u32| {
            img[off..off + 4].copy_from_slice(&v.to_le_bytes());
        };
        // Primary volume descriptor at sector 16.
        let pvd = 16 * 2048;
        img[pvd] = 1;
        img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        let r = pvd + 156; // root directory record
        img[r] = 34;
        put(&mut img, r + 2, 17); // root dir LBA
        put(&mut img, r + 10, 2048); // root dir length
        img[r + 25] = 0x02; // directory
        img[r + 32] = 1;
        // Root directory at sector 17: ".", "..", file "X".
        let d = 17 * 2048;
        img[d] = 34;
        put(&mut img, d + 2, 17);
        put(&mut img, d + 10, 2048);
        img[d + 25] = 0x02;
        img[d + 32] = 1;
        img[d + 34] = 34;
        put(&mut img, d + 36, 17);
        put(&mut img, d + 44, 2048);
        img[d + 59] = 0x02;
        img[d + 66] = 1;
        img[d + 67] = 0x01;
        img[d + 68] = 34;
        put(&mut img, d + 70, 20); // file LBA 20 → FAD 170
        put(&mut img, d + 78, 2048); // file length
        img[d + 93] = 0x00; // not a directory
        img[d + 100] = 1;
        img[d + 101] = b'X';
        // File content at sector 20.
        img[20 * 2048..20 * 2048 + 4].copy_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);
        Disc::from_iso(img)
    }

    #[test]
    fn iso9660_change_dir_lists_files_and_read_file_streams_content() {
        let mut c = CdBlock::new();
        c.insert_disc(fs_disc());
        // Change directory to root (file id 0xFFFFFF).
        cmd(&mut c, 0x7000, 0x0000, 0x00FF, 0xFFFF);
        assert_eq!(c.numfiles, 3, ". / .. / one file");
        assert_eq!(c.firstfile, 2, "first non-directory entry");
        // Get file-system scope.
        cmd(&mut c, 0x7200, 0, 0, 0);
        assert_eq!(c.read16(0x001C), 3); // CR2 = file count
        assert_eq!(c.read16(0x0024), 2); // CR4 = first file id
        // Get file info for file id 2: FAD 170 (0xAA), length 2048 (0x0800).
        cmd(&mut c, 0x7300, 0x0000, 0x0000, 0x0002);
        assert_eq!(c.read16(0x8000), 0x0000); // FAD hi
        assert_eq!(c.read16(0x8000), 0x00AA); // FAD lo = 170
        assert_eq!(c.read16(0x8000), 0x0000); // length hi
        assert_eq!(c.read16(0x8000), 0x0800); // length lo = 2048
        // Read file id 2 via filter 0 → partition 0; pump one sector.
        cmd(&mut c, 0x7400, 0x0000, 0x0000, 0x0002);
        pump(&mut c);
        assert_eq!(c.partitions[0].blocks.len(), 1, "file sector buffered");
        cmd(&mut c, 0x6100, 0x0000, 0x0000, 0x0001); // Get Sector Data
        assert_eq!(c.read32(0x8000), 0xCAFE_BABE, "file content streamed");
    }

    #[test]
    fn authentication_and_disc_region() {
        let mut c = CdBlock::new();
        // No disc → region 0 (no CD).
        cmd(&mut c, 0xE100, 0x0000, 0, 0);
        assert_eq!(c.read16(0x001C), 0x0000, "no disc → region 0");
        c.insert_disc(iso_disc());
        // Check copy protection (0xE0): the auth HIRQ pattern incl. ECPY (0x100).
        cmd(&mut c, 0xE000, 0x0000, 0, 0);
        assert_eq!(
            c.hirq, 0x0FC5,
            "authentication HIRQ pattern (0x07C5 | MPED)"
        );
        assert_ne!(c.hirq & 0x0100, 0, "ECPY (authentication done)");
        // Get disc region (0xE1): 4 = Saturn data disc.
        cmd(&mut c, 0xE100, 0x0000, 0, 0);
        assert_eq!(c.read16(0x001C), 0x0004, "Saturn data-disc region");
    }

    #[test]
    fn no_disc_commands_unchanged() {
        // Without a disc, Get Status still returns the no-disc report.
        let mut c = CdBlock::new();
        c.write16(0x0018, 0x0000);
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000);
        assert_eq!(c.read16(0x001C), 0x0000);
        assert_eq!(c.read16(0x0024), 0x0000);
    }

    #[test]
    fn get_sector_data_rejects_offset_plus_count_past_partition() {
        // Regression: 0x61 validated count alone (`avail < count`), so a
        // non-zero offset with `offset + count > avail` slipped through and the
        // transfer then indexed `blocks[pos + sect]` out of bounds. Now reject.
        let mut c = CdBlock::new();
        c.partitions[0].blocks = vec![0, 1, 2]; // 3 sectors available
        c.hirq = 0;
        c.write16(0x0018, 0x6100); // CR1: command 0x61 (Get Sector Data)
        c.write16(0x001C, 2); // CR2: offset = 2
        c.write16(0x0020, 0x0000); // CR3: buffer 0
        c.write16(0x0024, 3); // CR4: count = 3 → 2+3 > 3 → reject (was OOB)
        assert_eq!(
            c.cr1, STAT_REJECT,
            "offset+count past the partition rejected"
        );
        assert!(c.xfer32.is_none(), "no transfer armed on reject");
    }

    #[test]
    fn get_sector_data_ffff_count_is_remaining_from_offset() {
        // 0xFFFF count = "all sectors from the offset"; the armed transfer
        // length must be `avail - offset`, not `avail`.
        let mut c = CdBlock::new();
        c.partitions[0].blocks = vec![0, 1, 2, 3, 4]; // 5 sectors available
        c.write16(0x0018, 0x6100);
        c.write16(0x001C, 2); // offset = 2
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0xFFFF); // count = all-remaining
        let x = c.xfer32.as_ref().expect("transfer armed");
        assert_eq!(x.pos, 2);
        assert_eq!(x.num, 3, "0xFFFF count = avail (5) - offset (2)");
        assert!(c.cr1 & STAT_TRANS != 0, "transfer-pending status set");
    }

    // ===== Seek disc (0x11) — all three branches =====

    #[test]
    fn seek_disc_fad_form_positions_the_head_and_pauses() {
        let mut c = CdBlock::new();
        c.insert_disc(iso_disc());
        c.drive_phase = DrivePhase::Idle; // skip the recognition spin-up
        // CR1 bit 0x80 set + a real FAD (0x000200): seek to that FAD, status PAUSE.
        cmd(&mut c, 0x1180, 0x0200, 0x0000, 0x0000);
        assert_eq!(c.status & !STAT_PERI, STAT_PAUSE, "FAD seek pauses");
        assert_eq!(c.cd_curfad, 0x0200, "head positioned at the requested FAD");
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK);
    }

    #[test]
    fn seek_disc_fad_ffffff_is_pause_in_place() {
        let mut c = CdBlock::new();
        c.insert_disc(iso_disc());
        c.drive_phase = DrivePhase::Idle;
        c.cd_curfad = 0x0345; // an existing head position to be preserved
        // CR1 bit 0x80, FAD = 0xFFFFFF: pause in place — head FAD unchanged.
        cmd(&mut c, 0x11FF, 0xFFFF, 0x0000, 0x0000);
        assert_eq!(c.status & !STAT_PERI, STAT_PAUSE, "pause in place");
        assert_eq!(c.cd_curfad, 0x0345, "head position preserved (no re-seek)");
    }

    #[test]
    fn seek_disc_track_form_sets_track_and_pauses() {
        let mut c = CdBlock::new();
        c.insert_disc(iso_disc());
        c.drive_phase = DrivePhase::Idle;
        // CR1 bit 0x80 clear, CR2 high byte = track 5: track seek → PAUSE.
        cmd(&mut c, 0x1100, 0x0500, 0x0000, 0x0000);
        assert_eq!(c.status & !STAT_PERI, STAT_PAUSE, "track seek pauses");
        assert_eq!(c.track, 5, "track set from CR2 high byte");
    }

    #[test]
    fn seek_disc_stop_form_goes_standby_with_stopped_geometry() {
        let mut c = CdBlock::new();
        c.insert_disc(iso_disc());
        c.drive_phase = DrivePhase::Idle;
        // CR1 bit 0x80 clear, CR2 = 0 (track 0): Stop → STANDBY with the
        // "no position" sentinels (Mednafen STOP geometry, cdb.cpp:2846).
        cmd(&mut c, 0x1100, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.status & !STAT_PERI, STAT_STANDBY, "Stop → STANDBY");
        assert_eq!(c.cd_curfad, 0xFFFF_FFFF, "head FAD sentinel");
        assert_eq!(c.track, 0xFF);
        assert_eq!(c.ctrladdr, 0xFF);
        assert_eq!(c.index, 0xFF);
        assert_eq!(c.repcnt, 0x7F);
        // The status report carries the stopped geometry (CR2/3/4 = 0xFF…).
        assert_eq!(c.read16(0x001C), 0xFFFF, "CR2 ctrl/track sentinel");
        assert_eq!(c.read16(0x0020), 0xFFFF, "CR3 index/FAD-hi sentinel");
        assert_eq!(c.read16(0x0024), 0xFFFF, "CR4 FAD-lo sentinel");
    }

    // ===== Get last buffer destination (0x32) =====

    #[test]
    fn get_last_buffer_destination_reports_the_last_routed_partition() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        // A play routes sectors through filter 0 → partition 0, latching
        // `last_buffer` to that partition.
        play(&mut c, 150, 1);
        pump(&mut c);
        assert_eq!(c.last_buffer, 0, "filtered sector latched partition 0");
        // Get Last Buffer Destination (0x32): partition # in CR3 high byte.
        cmd(&mut c, 0x3200, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.read16(0x0020) >> 8, 0, "last buffer in CR3 high byte");
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK);
    }

    // ===== Set filter connection (0x46) =====

    #[test]
    fn set_filter_connection_sets_true_and_false_conditions() {
        let mut c = CdBlock::new();
        // CR1 bit0 = set true cond, bit1 = set false cond. CR2 high = true
        // partition (7), CR2 low = false partition (9), filter # = CR3 high (2).
        cmd(&mut c, 0x4603, 0x0709, 0x0200, 0x0000);
        assert_eq!(c.filters[2].condtrue, 7, "true connector set");
        assert_eq!(c.filters[2].condfalse, 9, "false connector set");
        assert_eq!(c.hirq & HIRQ_ESEL, HIRQ_ESEL);
        // Bit0 only: update the true connector, leave the false one alone.
        cmd(&mut c, 0x4601, 0x1100, 0x0200, 0x0000);
        assert_eq!(c.filters[2].condtrue, 0x11, "true connector updated");
        assert_eq!(c.filters[2].condfalse, 9, "false connector untouched");
        // Bit1 only: update the false connector, leave the true one alone.
        cmd(&mut c, 0x4602, 0x0022, 0x0200, 0x0000);
        assert_eq!(c.filters[2].condtrue, 0x11, "true connector untouched");
        assert_eq!(c.filters[2].condfalse, 0x22, "false connector updated");
    }

    // ===== Get buffer partition sector number (0x51) =====

    #[test]
    fn get_buffer_partition_sector_number_reports_block_count() {
        let mut c = CdBlock::new();
        c.partitions[3].blocks = vec![0, 1, 2, 3]; // 4 blocks in partition 3
        // Get Buffer Partition Sector Number (0x51): partition = CR3 high byte.
        cmd(&mut c, 0x5100, 0x0000, 0x0300, 0x0000);
        assert_eq!(c.read16(0x0024), 4, "CR4 = partition block count");
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK);
        // An out-of-range partition reports 0.
        cmd(&mut c, 0x5100, 0x0000, 0xFF00, 0x0000);
        assert_eq!(c.read16(0x0024), 0, "invalid partition → 0 count");
    }

    // ===== Calculate (0x52) + Get (0x53) actual data size =====

    #[test]
    fn calculate_and_get_actual_data_size_sums_block_sizes_in_words() {
        let mut c = CdBlock::new();
        // Three blocks of known byte sizes in partition 1.
        c.blocks[10].size = 2048;
        c.blocks[11].size = 2048;
        c.blocks[12].size = 1024;
        c.partitions[1].blocks = vec![10, 11, 12];
        // Calculate Actual Data Size (0x52): partition = CR3 high (1),
        // offset = CR2 (0), count = CR4 (3). Result is in 16-bit words.
        cmd(&mut c, 0x5200, 0x0000, 0x0100, 0x0003);
        // (2048 + 2048 + 1024) / 2 = 2560 words.
        assert_eq!(c.calcsize, 2560);
        assert_eq!(c.hirq & HIRQ_ESEL, HIRQ_ESEL);
        // Get Actual Data Size (0x53): the word count splits CR1(MSB)/CR2(low).
        cmd(&mut c, 0x5300, 0x0000, 0x0000, 0x0000);
        // 2560 words fits in 16 bits, so the size-hi byte (CR1 low) is 0.
        assert_eq!(c.read16(0x0018) & 0xFF, 0, "CR1 size hi");
        assert_eq!(c.read16(0x001C), 2560, "CR2 size lo (words)");
        // A sub-range (offset 1, count 1) sums just that one block.
        cmd(&mut c, 0x5200, 0x0001, 0x0100, 0x0001);
        assert_eq!(c.calcsize, 1024, "block 11 only = 2048/2 words");
    }

    // ===== Get copy/move error (0x67) =====

    #[test]
    fn get_copy_move_error_reports_no_error() {
        let mut c = CdBlock::new();
        c.insert_disc(iso_disc());
        // Even with real disc geometry present, Get Copy/Move Error (0x67)
        // returns "no error": CR2..CR4 all zero (Mednafen 0x0100,0,0,0).
        cmd(&mut c, 0x6700, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.read16(0x001C), 0x0000, "CR2 = 0 (no error)");
        assert_eq!(c.read16(0x0020), 0x0000, "CR3 = 0");
        assert_eq!(c.read16(0x0024), 0x0000, "CR4 = 0");
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK);
    }

    // ===== Read directory (0x71) =====

    #[test]
    fn read_directory_connects_the_filter_and_rejects_invalid() {
        let mut c = CdBlock::new();
        // Read Directory (0x71): connect the read filter from CR3 high byte (5).
        cmd(&mut c, 0x7100, 0x0000, 0x0500, 0x0000);
        assert_eq!(c.cd_device_filter, 5, "filter connected for the dir read");
        assert_eq!(c.hirq & HIRQ_EFLS, HIRQ_EFLS);
        // An out-of-range filter index → disconnected (NO_FILTER).
        cmd(&mut c, 0x7100, 0x0000, 0xFF00, 0x0000);
        assert_eq!(c.cd_device_filter, NO_FILTER, "invalid filter → disconnected");
    }

    // ===== Abort file (0x75) — with and without a disc =====

    #[test]
    fn abort_file_with_disc_pauses_and_clears_xfer() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        play(&mut c, 150, 2);
        pump(&mut c);
        // Arm a 32-bit transfer so we can confirm Abort File clears it.
        cmd(&mut c, 0x6100, 0x0000, 0x0000, 0x0002); // Get Sector Data
        assert!(c.xfer32.is_some(), "transfer armed before abort");
        cmd(&mut c, 0x7500, 0x0000, 0x0000, 0x0000); // Abort File
        assert_eq!(c.status & !STAT_PERI, STAT_PAUSE, "disc present → PAUSE");
        assert!(c.xfer32.is_none(), "xfer32 cleared by Abort File");
        assert_eq!(c.fadstoplay, -1, "play range parked (drive idle)");
        assert_eq!(c.hirq & HIRQ_EFLS, HIRQ_EFLS);
    }

    #[test]
    fn abort_file_without_disc_keeps_nodisc() {
        let mut c = CdBlock::new();
        // No disc: Abort File must NOT fabricate a disc-present PAUSE status —
        // it is a buffer/transfer abort, not a physical drive op.
        cmd(&mut c, 0x7500, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.status & !STAT_PERI, STAT_NODISC, "no disc → NODISC kept");
        assert!(c.xfer32.is_none());
        assert_eq!(c.hirq & (HIRQ_CMOK | HIRQ_EFLS), HIRQ_CMOK | HIRQ_EFLS);
    }

    // ===== Default arm — unimplemented command =====

    #[test]
    fn unimplemented_command_returns_a_status_report_and_cmok() {
        let mut c = CdBlock::new();
        c.hirq = 0;
        // 0x21 is not a real command → the default arm: a plain status report
        // + CMOK. (0x20 Get Subcode is now implemented; see the subcode tests.)
        cmd(&mut c, 0x2100, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK, "default arm sets CMOK");
        // No disc → the report is the NODISC status, zero geometry.
        assert_eq!(c.read16(0x0018), 0x0700, "CR1 = NODISC status report");
        assert_eq!(c.read16(0x001C), 0x0000);
        assert_eq!(c.read16(0x0020), 0x0000);
        assert_eq!(c.read16(0x0024), 0x0000);
    }

    // ===== Play (0x10) variants =====

    #[test]
    fn play_with_repeat_mode_arms_the_repeat_count() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        // Play FAD 150, 2 sectors, play-mode = 0x03 (repeat 3, top bits 0) in
        // CR3 high byte. start/end both FAD (bit 0x80 in CR1/CR3 low byte).
        // CR1 = 0x10 | 0x80 (FAD), CR2 = 150; CR3 = (0x03<<8)|0x80, CR4 = 2.
        cmd(&mut c, 0x1080, 150, 0x0380, 0x0002);
        assert_eq!(c.cur_play_repeat, 0x03, "repeat field decoded from play-mode");
        // The end field is start + count when both are FAD-addressed.
        assert_eq!(c.cur_play_end & 0x7F_FFFF, 150 + 2, "end = start + count");
        assert_eq!(c.hirq & HIRQ_CMOK, HIRQ_CMOK);
    }

    #[test]
    fn play_ffffff_reuses_the_prior_play_position() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        // First Play sets a range.
        cmd(&mut c, 0x1080, 150, 0x0080, 0x0002); // start 150, +2 sectors
        let prior_start = c.cur_play_start;
        let prior_end = c.cur_play_end;
        // A lone 0xFFFFFF start/end reuses the prior position (cdb.cpp:2813).
        cmd(&mut c, 0x10FF, 0xFFFF, 0x00FF, 0xFFFF);
        assert_eq!(c.cur_play_start, prior_start, "start reused");
        assert_eq!(c.cur_play_end, prior_end, "end reused");
    }

    #[test]
    fn play_after_seek_stop_uses_physical_pickup_as_seek_origin() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        c.drive_phase = DrivePhase::Idle;
        c.drive_sector = 150;

        cmd(&mut c, 0x1100, 0x0000, 0x0000, 0x0000); // Seek-to-0 stops drive
        assert_eq!(c.cd_curfad, 0xFFFF_FFFF, "stopped geometry is visible");
        assert_eq!(c.drive_sector, 150, "physical pickup position is retained");

        play(&mut c, 150, 2);
        pump(&mut c);
        assert_eq!(c.partitions[0].blocks.len(), 2, "Play completes after stop");
        assert_eq!(c.status & !STAT_PERI, STAT_PAUSE);
        assert_eq!(c.track, 1, "seek refreshes target track geometry");
        assert_eq!(c.index, 1, "seek refreshes target index geometry");
    }

    /// A Play's seek must report `STATUS_BUSY` through the whole `SeekStart2`
    /// settle (`SEEKSTART2_CYC`, 256000 CD clocks) and only then `STATUS_SEEK`
    /// — matching Mednafen, where `SEEK_START1/2/3` are all BUSY and only the
    /// `SEEK` phase reports SEEK. VF2's intro probes 1-sector Plays and builds
    /// its next command as `0x2000 | <status report CR1>`; SEEK (0x04) leaking
    /// in during the settle produced the bogus command `0x24` and derailed it.
    #[test]
    fn seek_holds_busy_through_the_seekstart2_settle_before_reporting_seek() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        c.drive_phase = DrivePhase::Idle;
        c.drive_sector = 150;

        play(&mut c, 151, 1);
        assert_eq!(c.status & !STAT_PERI, STAT_BUSY, "BUSY at Play accept");
        c.tick(cd2m(128_000));
        assert_eq!(c.status & !STAT_PERI, STAT_BUSY, "still BUSY mid-settle");
        // Past the settle, inside the radial seek proper.
        c.tick(cd2m(256_000));
        assert_eq!(c.status & !STAT_PERI, STAT_SEEK, "SEEK only after the settle");
        // The split phases don't break end-to-end completion.
        pump(&mut c);
        assert_eq!(c.partitions[0].blocks.len(), 1, "sector buffered");
        assert_eq!(c.status & !STAT_PERI, STAT_PAUSE);
    }

    /// The unsolicited periodic report must not clobber a command the host is
    /// mid-way through composing (`cr_written != 0`). The hardware keeps
    /// host-written command words and block-written results in separate
    /// register files (Mednafen `CTR.CD[]` vs `Results[]`); our shared CR1–4
    /// emulated the report into a half-written command, so VF2's GetSubcodeQ
    /// (CR1=0x2000 written, CR4 still pending) dispatched as the report's own
    /// status byte — the bogus command 0x24 (`SEEK|PERI`) / 0x21 (`PAUSE|PERI`).
    #[test]
    fn periodic_report_does_not_clobber_a_half_composed_command() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        c.drive_phase = DrivePhase::Idle;
        cmd(&mut c, 0x0000, 0x0000, 0x0000, 0x0000); // engage (GetStatus)
        let _ = c.read16(0x0024); // consume response (clears command_pending)

        c.write16(0x0018, 0x2000); // CR1 of a GetSubcodeQ — command half-composed
        c.tick(cd2m(500_000)); // several periodic reports elapse
        assert_eq!(c.cr1, 0x2000, "periodic must not overwrite the pending CR1");

        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000); // CR4 completes + dispatches the command
        assert_eq!(c.cr2, 0x0005, "the intended GetSubcodeQ executed (5 words)");
        assert_eq!(c.cr1 & STAT_TRANS, STAT_TRANS, "subcode staged for transfer");
    }

    /// Get Subcode (0x20) type 0 stages the 10-byte Q channel — [ctrl/adr,
    /// tno, idx, rel-FAD, 0, abs-FAD] — for FIFO reads, with CR2 = 5 words and
    /// the DTREQ/TRANS bit set (Mednafen `COMMAND_GET_SUBCODE`).
    #[test]
    fn get_subcode_q_stages_the_head_position_for_fifo_read() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        play(&mut c, 150, 2);
        pump(&mut c);
        let fad = c.cd_curfad;
        let rel = fad - 150;

        cmd(&mut c, 0x2000, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.cr1 & STAT_TRANS, STAT_TRANS, "DTREQ in the status");
        assert_eq!(c.cr2, 0x0005, "5 words of subcode Q");
        assert_eq!(c.hirq & HIRQ_DRDY, HIRQ_DRDY, "data ready");
        assert_eq!(c.read16(0x8000), 0x4101, "ctrl/adr 0x41, track 1");
        assert_eq!(c.read16(0x8000), 0x0100 | ((rel >> 16) & 0xFF) as u16, "index 1 + rel hi");
        assert_eq!(c.read16(0x8000), (rel & 0xFFFF) as u16, "rel-FAD");
        assert_eq!(c.read16(0x8000), ((fad >> 16) & 0xFF) as u16, "abs hi");
        assert_eq!(c.read16(0x8000), (fad & 0xFFFF) as u16, "abs-FAD");
        // End Data Transfer reports the 5 words read back.
        cmd(&mut c, 0x0600, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.cr2, 5, "host read 5 words");

        // Type >= 2 is rejected (Mednafen `CDStatusResults(true)`).
        cmd(&mut c, 0x2002, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.cr1, STAT_REJECT, "subcode type >= 2 rejected");
    }

    /// The Play track/index addressing form (no FAD flag) seeks to the
    /// commanded track's start and — with repeat ∞ — loops it, streaming an
    /// audio track to the CD-DA mixer (Mednafen `SeekStart1` else-branch +
    /// `CheckEndMet` track comparison). VF2's character-select BGM is
    /// `Play(track 14 idx 1 → track 14 idx 99, mode 0x0F)`; approximating the
    /// form to the disc start played data sectors instead of the music.
    #[test]
    fn play_track_index_form_seeks_to_the_track_and_loops_its_audio() {
        let mut bin = vec![0u8; 2352 * 8];
        bin[2352 * 4..2352 * 4 + 2].copy_from_slice(&0x4321i16.to_le_bytes());
        let disc = Disc::from_cue(
            "FILE \"a.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 00:00:04\n",
            |_| Some(bin.clone()),
        )
        .expect("cue");
        let mut c = CdBlock::new();
        c.insert_disc(disc);
        c.drive_phase = DrivePhase::Idle;
        c.drive_sector = 150;
        // Start = track 2 index 1, end = track 2 index 99, play mode 0x0F.
        cmd(&mut c, 0x1000, 0x0201, 0x0F00, 0x0263);
        assert_ne!(c.cr1, STAT_REJECT, "consistent track forms accepted");
        pump(&mut c);
        assert!(c.cd_curfad >= 154, "head in the audio track (FAD {})", c.cd_curfad);
        assert_ne!(c.status & !STAT_PERI, STAT_PAUSE, "repeat ∞ never pauses");
        let pcm = c.take_cd_audio(2);
        assert_eq!(pcm[0], 0x4321, "track 2 PCM streamed to the CD-DA mixer");
    }

    /// A Play mixing FAD and track addressing forms is rejected with the
    /// play state untouched (Mednafen cdb.cpp:2830: `((psp ^ pep) & 0x800000)
    /// && pep != 0` → `CDStatusResults(true)`).
    #[test]
    fn play_with_mixed_fad_and_track_forms_is_rejected() {
        let mut c = CdBlock::new();
        c.insert_disc(data_disc());
        c.drive_phase = DrivePhase::Idle;
        let prev_end = c.cur_play_end;
        cmd(&mut c, 0x1080, 0x0E01, 0x0F00, 0x0E63); // FAD start, track end
        assert_eq!(c.cr1, STAT_REJECT, "mixed forms rejected");
        assert_eq!(c.cur_play_end, prev_end, "play state untouched");
        assert_eq!(c.drive_phase, DrivePhase::Idle, "no seek started");
    }

    // ===== File-system error/edge branches (0x70 / 0x73 / 0x74) =====

    #[test]
    fn get_file_info_whole_directory_form() {
        let mut c = CdBlock::new();
        c.insert_disc(fs_disc());
        cmd(&mut c, 0x7000, 0x0000, 0x00FF, 0xFFFF); // ChangeDir → root
        // Get File Info (0x73) with id 0xFFFFFF = whole-directory form: CR2 is
        // the all-entries word count (0x5F4), TRANS set, no per-file FAD staged.
        cmd(&mut c, 0x7300, 0x0000, 0x00FF, 0xFFFF);
        assert_eq!(c.read16(0x001C), 0x05F4, "whole-directory word count");
        assert!(c.transfer_request, "transfer pending");
        assert_eq!(c.hirq & HIRQ_DRDY, HIRQ_DRDY);
    }

    #[test]
    fn get_file_info_for_a_valid_id_stages_a_12_byte_record() {
        let mut c = CdBlock::new();
        c.insert_disc(fs_disc());
        cmd(&mut c, 0x7000, 0x0000, 0x00FF, 0xFFFF); // ChangeDir → root
        // Get File Info for id 2 (the file "X" at FAD 170, length 2048).
        cmd(&mut c, 0x7300, 0x0000, 0x0000, 0x0002);
        assert_eq!(c.read16(0x001C), 6, "CR2 = 6 words for a single file");
        // The 12-byte record streams big-endian through the FIFO: FAD, length,
        // gap/unit, id, flags.
        assert_eq!(c.read16(0x8000), 0x0000, "FAD hi");
        assert_eq!(c.read16(0x8000), 0x00AA, "FAD lo = 170");
        assert_eq!(c.read16(0x8000), 0x0000, "length hi");
        assert_eq!(c.read16(0x8000), 0x0800, "length lo = 2048");
        let _gap_unit = c.read16(0x8000); // gap/unit size bytes
        assert_eq!(c.read16(0x8000) >> 8, 2, "id byte = 2");
    }

    #[test]
    fn read_file_with_invalid_id_just_reports_status() {
        let mut c = CdBlock::new();
        c.insert_disc(fs_disc());
        c.drive_phase = DrivePhase::Idle;
        cmd(&mut c, 0x7000, 0x0000, 0x00FF, 0xFFFF); // ChangeDir → root (3 entries)
        // Read File (0x74) with id 99 (out of range): the else branch just
        // emits a status report and does NOT start a read (no seek armed).
        cmd(&mut c, 0x7400, 0x0000, 0x0000, 0x0063);
        assert_eq!(c.fadstoplay, -1, "no read range armed for an invalid id");
        assert_eq!(c.hirq & (HIRQ_CMOK | HIRQ_EHST), HIRQ_CMOK | HIRQ_EHST);
    }

    // ===== 16-bit data FIFO read path (read_fifo16) =====

    #[test]
    fn data_fifo_streams_a_staged_transfer_word_by_word_big_endian() {
        let mut c = CdBlock::new();
        c.insert_disc(fs_disc());
        cmd(&mut c, 0x7000, 0x0000, 0x00FF, 0xFFFF); // ChangeDir → root
        // Stage the 12-byte Get-File-Info record for file id 2.
        cmd(&mut c, 0x7300, 0x0000, 0x0000, 0x0002);
        // The first staged byte pair is the big-endian FAD high half = 0x0000,
        // then the low half 0x00AA. Verify the 16-bit FIFO returns each pair
        // MSB-first (vs. the 32-bit data-port path tested elsewhere).
        assert_eq!(c.read16(0x8000), 0x0000); // bytes 0..1 (FAD hi)
        assert_eq!(c.read16(0x8000), 0x00AA); // bytes 2..3 (FAD lo = 170)
        // The byte cursor advances by two per read; xfer_done tracks it.
        assert_eq!(c.xfer_pos, 4, "two 16-bit reads consumed four bytes");
        assert_eq!(c.xfer_done, 4);
        // Reads past the staged buffer return 0 and don't run off the end.
        for _ in 0..20 {
            let _ = c.read16(0x8000);
        }
        assert_eq!(c.xfer_pos, c.xfer.len(), "cursor clamps at the buffer end");
        assert_eq!(c.read16(0x8000), 0x0000, "past-end reads return 0");
    }

    // ===== take_cd_audio_buffered pre-roll path =====

    #[test]
    fn take_cd_audio_buffered_holds_silence_until_preroll_then_drains() {
        let mut c = CdBlock::new();
        c.insert_disc(audio_disc());
        // Decode several audio sectors directly into the CD-DA FIFO (one sector
        // = 1176 samples; the pre-roll cushion is 44_100 samples ≈ 38 sectors).
        c.status = STAT_PLAY;
        c.cd_curfad = FAD_OFFSET;
        // Each play_data reads one sector and advances; loop the 2-sector disc.
        for _ in 0..50 {
            c.cd_curfad = FAD_OFFSET; // re-read sector 0 each time to fill
            c.fadstoplay = 1;
            c.status = STAT_PLAY;
            c.play_data();
        }
        assert!(
            c.cd_audio.len() >= 44_100,
            "buffered past the pre-roll cushion"
        );
        // Before priming, the very first buffered drain must already pass the
        // cushion (we filled > PREROLL), so it returns real samples, not silence.
        let out = c.take_cd_audio_buffered(1176);
        assert!(c.cd_audio_primed, "primed once the cushion was reached");
        assert!(out.iter().any(|&s| s != 0), "drained real PCM, not silence");
    }

    #[test]
    fn take_cd_audio_buffered_returns_silence_below_preroll() {
        let mut c = CdBlock::new();
        // An empty (or barely-filled) FIFO stays un-primed and pads silence.
        let out = c.take_cd_audio_buffered(64);
        assert_eq!(out, vec![0i16; 64], "no cushion yet → silence");
        assert!(!c.cd_audio_primed, "still un-primed below the pre-roll");
    }

    // ===== dbg_play_cdda / dbg_play_first_audio_track debug hooks =====

    #[test]
    fn dbg_play_cdda_arms_a_play_over_the_given_range() {
        let mut c = CdBlock::new();
        c.insert_disc(audio_disc());
        // dbg_play_cdda(fad, sectors): drives the real start_seek Play machinery.
        c.dbg_play_cdda(FAD_OFFSET, 2);
        assert_eq!(
            c.cur_play_start, 0x80_0000 | FAD_OFFSET,
            "play start armed (FAD-addressed)"
        );
        assert_eq!(c.cur_play_end, 0x80_0000 | (FAD_OFFSET + 2), "play end armed");
        assert_eq!(c.play_end_irq, HIRQ_PEND, "PEND at range end");
        assert_eq!(c.drive_phase, DrivePhase::SeekStart, "seek started");
    }

    #[test]
    fn dbg_play_first_audio_track_finds_and_plays_the_audio_track() {
        let mut c = CdBlock::new();
        c.insert_disc(audio_disc());
        assert!(c.dbg_play_first_audio_track(), "found the audio track");
        // It armed a Play over track 1 (FAD 150, 2 sectors) via dbg_play_cdda.
        assert_eq!(c.cur_play_start, 0x80_0000 | FAD_OFFSET);
        assert_eq!(c.drive_phase, DrivePhase::SeekStart, "seek started");
        // Running the drive past seek streams CDDA into the audio FIFO.
        pump(&mut c);
        assert!(!c.cd_audio.is_empty(), "CDDA decoded to the audio FIFO");
        // A data-only disc has no audio track → returns false, arms nothing.
        let mut d = CdBlock::new();
        d.insert_disc(iso_disc());
        assert!(!d.dbg_play_first_audio_track(), "no audio track on a data disc");
    }

    // ===== reset_selector variants =====

    #[test]
    fn reset_selector_single_partition_clears_just_that_buffer() {
        let mut c = CdBlock::new();
        // Two partitions hold blocks; CR1 low == 0 clears the one in CR3 high.
        c.blocks[5].size = 2048;
        c.blocks[6].size = 2048;
        c.partitions[2].blocks = vec![5];
        c.partitions[3].blocks = vec![6];
        c.free_blocks = MAX_BLOCKS as i32 - 2;
        // Reset Selector (0x48), CR1 low = 0 → clear partition CR3 high = 2.
        cmd(&mut c, 0x4800, 0x0000, 0x0200, 0x0000);
        assert!(c.partitions[2].blocks.is_empty(), "partition 2 cleared");
        assert_eq!(c.partitions[3].blocks, vec![6], "partition 3 untouched");
        assert_eq!(c.free_blocks, MAX_BLOCKS as i32 - 1, "one block freed");
        assert_eq!(c.hirq & (HIRQ_CMOK | HIRQ_ESEL), HIRQ_CMOK | HIRQ_ESEL);
    }

    #[test]
    fn reset_selector_all_partitions_and_connectors() {
        let mut c = CdBlock::new();
        c.filters[0].condtrue = 7;
        c.filters[0].condfalse = 9;
        c.blocks[1].size = 2048;
        c.partitions[0].blocks = vec![1];
        c.free_blocks = MAX_BLOCKS as i32 - 1;
        // CR1 bit 0x80 (false conds) | 0x40 (true conds) | 0x04 (all partitions).
        cmd(&mut c, 0x48C4, 0x0000, 0x0000, 0x0000);
        assert_eq!(c.filters[0].condtrue, 0, "true connector reset");
        assert_eq!(c.filters[0].condfalse, 0, "false connector reset");
        assert!(c.partitions[0].blocks.is_empty(), "all partitions cleared");
        assert_eq!(c.free_blocks, MAX_BLOCKS as i32, "all blocks freed");
        assert!(!c.buf_full, "buffer-full latch cleared");
    }

    // ===== Get Sector Data / Delete reject branches (invalid buffer) =====

    #[test]
    fn get_sector_data_rejects_an_invalid_buffer_number() {
        let mut c = CdBlock::new();
        c.hirq = 0;
        // Buffer 0xFF >= MAX_FILTERS → reject, no transfer armed.
        cmd(&mut c, 0x6100, 0x0000, 0xFF00, 0x0001);
        assert_eq!(c.cr1, STAT_REJECT, "invalid buffer rejected");
        assert!(c.xfer32.is_none(), "no transfer armed");
        assert_eq!(c.hirq & (HIRQ_CMOK | HIRQ_EHST), HIRQ_CMOK | HIRQ_EHST);
    }

    #[test]
    fn delete_sector_data_rejects_an_empty_or_invalid_partition() {
        let mut c = CdBlock::new();
        c.hirq = 0;
        // Delete (0x62) on an empty partition (avail == 0) → reject.
        cmd(&mut c, 0x6200, 0x0000, 0x0000, 0x0001);
        assert_eq!(c.cr1, STAT_REJECT, "empty partition rejected");
        // A valid range frees the requested blocks.
        c.blocks[2].size = 2048;
        c.blocks[3].size = 2048;
        c.partitions[1].blocks = vec![2, 3];
        c.free_blocks = MAX_BLOCKS as i32 - 2;
        cmd(&mut c, 0x6200, 0x0000, 0x0100, 0x0001); // delete 1 from offset 0
        assert_eq!(c.partitions[1].blocks, vec![3], "one block deleted");
        assert_eq!(c.free_blocks, MAX_BLOCKS as i32 - 1, "freed one block");
        assert_eq!(c.hirq & (HIRQ_CMOK | HIRQ_EHST), HIRQ_CMOK | HIRQ_EHST);
    }

    // ===== byte-access (read8/write8) paths + HIRQ-mask register =====

    #[test]
    fn byte_access_aliases_the_even_and_odd_halves_of_a_register() {
        let mut c = CdBlock::new();
        // Power-on CR2 = "DB" → high byte 'D' (offset 0x1C), low byte 'B' (0x1D).
        assert_eq!(c.read8(0x001C), b'D');
        assert_eq!(c.read8(0x001D), b'B');
        // write8 composes the two halves of a 16-bit register write. Write the
        // HIRQ-mask register (0x0C) one byte at a time and read it back.
        c.write8(0x000C, 0x12); // high byte
        c.write8(0x000D, 0x34); // low byte
        assert_eq!(c.hirq_mask, 0x1234, "byte writes compose the 16-bit value");
        assert_eq!(c.read8(0x000C), 0x12);
        assert_eq!(c.read8(0x000D), 0x34);
    }

    #[test]
    fn hirq_mask_register_round_trips_and_gates_irq_active() {
        let mut c = CdBlock::new();
        c.hirq = HIRQ_CMOK;
        // The HIRQ-mask register at 0x0C is a plain read/write latch.
        c.write16(0x000C, 0x0001); // unmask only CMOK
        assert_eq!(c.read16(0x000C), 0x0001);
        assert!(c.irq_active(), "(hirq & mask) != 0 with CMOK unmasked");
        c.write16(0x000C, 0x0000); // mask everything
        assert!(!c.irq_active(), "masking all bits drops the IRQ level");
    }

    // ===== read8 of CR4 consumes the command response =====

    #[test]
    fn read8_of_cr4_consumes_the_pending_command_response() {
        let mut c = CdBlock::new();
        // Issue Get Status; the response sits pending until CR4 is read.
        cmd(&mut c, 0x0000, 0x0000, 0x0000, 0x0000);
        assert!(c.command_pending, "response pending after a command");
        // A byte read of either half of the CR4 slot routes through read16 and
        // clears command_pending (consumes the response).
        let _ = c.read8(0x0024);
        assert!(!c.command_pending, "reading CR4 consumed the response");
    }
}
