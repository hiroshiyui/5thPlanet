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

use crate::disc::{Disc, FAD_OFFSET};

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

// CD status codes — high byte of CR1 (cs2.c / MAME `CD_STAT_*`).
const STAT_PAUSE: u8 = 0x01; // drive ready, disc present, not playing
const STAT_PERI: u8 = 0x20; // OR'd in for periodic (unsolicited) reports

// 16-bit CR1 status bits that live above the status byte (MAME `CD_STAT_*`).
const STAT_TRANS: u16 = 0x4000; // data-transfer request pending

// Further status bytes (high byte of the 16-bit status word, MAME `CD_STAT_*`).
const STAT_REJECT: u16 = 0xFF00; // CR1 reject marker for malformed requests
const STAT_SEEK: u8 = 0x04; // drive seeking
const STAT_PLAY: u8 = 0x03; // read/playback in progress

// HIRQ playback-complete bit.
const HIRQ_PEND: u16 = 0x0010; // CD playback / read range completed

// HIRQ bits used by the buffer/filter/partition + filesystem engine.
#[allow(dead_code)] // M7 phase 4 (filesystem)
const HIRQ_EFLS: u16 = 0x0200; // file-system processing complete

// Buffer/filter/partition engine sizes (MAME `saturn_cd_hle`): a shared pool of
// 200 sector blocks, and 24 filter/partition selectors.
const MAX_BLOCKS: usize = 200;
const MAX_FILTERS: usize = 24;
/// "No filter / device disconnected" sentinel (MAME's `cddevicenum == 0xff`).
const NO_FILTER: u8 = 0xFF;

/// One buffered sector in the 200-block pool. `size < 0` marks the slot free;
/// otherwise it holds `size` bytes of user data plus the sector's disc
/// coordinates and subheader fields (used by filtering).
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug, Default)]
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
#[derive(Clone, Debug, Default)]
struct Partition {
    blocks: Vec<usize>,
}

/// In-flight 32-bit sector-data transfer (Get / Get-and-Delete Sector Data):
/// streams `num` blocks of partition `part` starting at index `pos`, tracking
/// the current block (`sect`) and byte offset within it (`offs`).
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug, Default)]
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

    /// The inserted disc image, if any. `None` is the power-on "no disc"
    /// state the existing no-disc command subset already models.
    disc: Option<Disc>,

    /// Host-readable data staged by a command (Get TOC for now), streamed out
    /// 16-bit big-endian through the data FIFO at `0x8000`. `xfer_pos` is the
    /// byte cursor. Phase 3 generalises this to sector data + the SCU-DMA port.
    xfer: Vec<u8>,
    xfer_pos: usize,

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
            hirq: 0x0000,
            hirq_mask: 0xFFFF,
            // Power-on identity string "CDBLOCK" — the BIOS reads CR1..CR4
            // to confirm the CD subsystem is present.
            cr1: b'C' as u16, // high byte 0, low byte 'C'
            cr2: ((b'D' as u16) << 8) | b'B' as u16,
            cr3: ((b'L' as u16) << 8) | b'O' as u16,
            cr4: ((b'C' as u16) << 8) | b'K' as u16,
            // No CD image present. Status is `PAUSE` (MAME resets `cd_stat`
            // to PAUSE even with no image), but the disc *geometry* is all
            // zero: MAME's `cr_standard_return` returns CR2=CR3=CR4=0 when
            // `!cdrom_image->exists()`. (Yabause's `DummyCD` instead reports
            // a present disc with FAD 150 / ctrl-addr 0x41 / track 1 — the
            // earlier model here; MAME is now the primary reference.) Real
            // disc-image geometry lands with the full CD-block in M6.
            status: STAT_PAUSE,
            options: 0x00,
            repcnt: 0x00,
            ctrladdr: 0x00,
            track: 0x00,
            index: 0x00,
            fad: 0,
            disk_changed: true,
            disc: None,
            xfer: Vec::new(),
            xfer_pos: 0,
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
            curroot: DirEntry::default(),
            curdir: Vec::new(),
            numfiles: 0,
            firstfile: 0,
            command_pending: false,
            cr_written: 0,
            periodic_accum: 0,
            host_initialized: false,
        }
    }

    /// Insert (or replace) a disc. The drive returns to PAUSE at the start of
    /// track 1 (FAD 150), the geometry the status reports now carry, and a
    /// disc-change is flagged (`HIRQ.DCHG`) so the BIOS re-reads the TOC.
    pub fn insert_disc(&mut self, disc: Disc) {
        self.ctrladdr = disc
            .track_at_fad(FAD_OFFSET)
            .map_or(0x41, |t| t.ctrl_addr());
        self.track = disc.first_track();
        self.index = 1;
        self.fad = FAD_OFFSET;
        self.status = STAT_PAUSE;
        self.disc = Some(disc);
        self.disk_changed = true;
        self.hirq |= HIRQ_DCHG;
    }

    /// Whether a disc is present.
    pub fn has_disc(&self) -> bool {
        self.disc.is_some()
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
                // Recompute the buffer/disc-state flags on read and latch
                // them, matching MAME's `hirq_r`: DCHG ("disc change / tray
                // open") is **always cleared**, and BFUL/CSCT are set from
                // the buffer state — both clear in our no-disc model (no data
                // buffered, no sector stored). (This replaces the earlier
                // Yabause-derived DCHG *re-assert*, which is the opposite and
                // left CMOK/DCHG bits set that derailed the BIOS — see the
                // MAME reference diff at BIOS 0x4216.)
                self.hirq &= !(HIRQ_DCHG | HIRQ_BFUL | HIRQ_CSCT);
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

    /// Write a standard CD status report into CR1..CR4 (cs2.c `doCDReport`).
    fn cd_report(&mut self) {
        self.cr1 = ((self.status as u16) << 8)
            | (((self.options & 0xF) as u16) << 4)
            | (self.repcnt & 0xF) as u16;
        self.cr2 = ((self.ctrladdr as u16) << 8) | self.track as u16;
        self.cr3 = ((self.index as u16) << 8) | ((self.fad >> 16) & 0xFF) as u16;
        self.cr4 = self.fad as u16;
    }

    /// The 16-bit status word (status code in the high byte) — MAME `cd_stat`.
    fn cd_stat(&self) -> u16 {
        (self.status as u16) << 8
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
            // Store `sectlenin` bytes: the 2048 user payload for the common
            // case, else a leading slice of the full on-disc sector.
            let data = if len == 2048 {
                match disc.read_sector(fad) {
                    Some(s) => s.to_vec(),
                    None => return false,
                }
            } else {
                match disc.read_full_sector(fad) {
                    Some(s) => s[..len.min(s.len())].to_vec(),
                    None => return false,
                }
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

    /// Advance the read pump one sector (MAME `cd_playdata`): a seek completes
    /// to PLAY; in PLAY each tick reads one filtered sector until the range is
    /// exhausted, then pauses and raises `PEND`.
    fn play_data(&mut self) {
        match self.status {
            STAT_SEEK => self.status = STAT_PLAY,
            STAT_PLAY if self.fadstoplay > 0 => {
                let fad = self.cd_curfad;
                if self.read_filtered_sector(fad) {
                    self.cd_curfad += 1;
                    self.fadstoplay -= 1;
                    self.hirq |= HIRQ_CSCT;
                    self.sectorstore = true;
                    if self.fadstoplay == 0 {
                        self.status = STAT_PAUSE;
                        self.hirq |= HIRQ_PEND;
                    }
                }
            }
            _ => {}
        }
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
                for cfad in 166..200u32 {
                    let Some(sect) = disc.read_sector(cfad) else { break };
                    if sect.len() >= 6 && &sect[1..6] == b"CD001" {
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
            for i in 0..nsect as u32 {
                match disc.read_sector(fad + i) {
                    Some(s) => buf.extend_from_slice(s),
                    None => buf.resize(buf.len() + 2048, 0),
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
                name: buf.get(pos + 33..pos + 33 + namelen).unwrap_or(&[]).to_vec(),
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
                // Get hardware info: status, CD/MPEG version, drive rev
                // (MAME `cmd_get_hw_info`). MAME does *not* touch the
                // disc-changed state here, so we don't either.
                // REVIEW(magic): CR2/CR4 (0x0201, 0x0400) are MAME's literal
                // hardware-info bytes (CD-block version / drive revision),
                // not from a datasheet. The BIOS doesn't gate boot on them
                // (it just reads them); revisit if a game checks the revision.
                self.cr1 = (self.status as u16) << 8;
                self.cr2 = 0x0201; // MPEG card present / CD version
                self.cr3 = 0x0000; // MPEG not authenticated
                self.cr4 = 0x0400; // drive info / revision
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
                }
                self.cr1 = STAT_TRANS | ((self.status as u16) << 8);
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
                self.cr4 = self.partitions.get(buf).map_or(0, |p| p.blocks.len() as u16);
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
                // Play disc: start = (CR1&0xFF)<<16|CR2, end = (CR3&0xFF)<<16|CR4.
                // Bit 0x800000 selects FAD addressing (the BIOS/game read path);
                // an end without it plays to the lead-out.
                let start = ((self.cr1 as u32 & 0xFF) << 16) | self.cr2 as u32;
                let end = ((self.cr3 as u32 & 0xFF) << 16) | self.cr4 as u32;
                self.status = STAT_PLAY;
                if start & 0x80_0000 != 0 && start != 0xFF_FFFF {
                    self.cd_curfad = start & 0x0F_FFFF;
                }
                if end & 0x80_0000 != 0 {
                    if end != 0xFF_FFFF {
                        self.fadstoplay = (end & 0x0F_FFFF) as i64;
                    }
                } else if let Some(d) = &self.disc {
                    self.fadstoplay = d.lead_out_fad().saturating_sub(self.cd_curfad) as i64;
                }
                self.sectorstore = false;
                self.cd_report();
                self.hirq |= HIRQ_CMOK;
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
                if bufnum >= MAX_FILTERS || (sectnum != 0xFFFF && avail < sectnum) {
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
                if bufnum >= MAX_FILTERS || avail == 0 {
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
                self.cd_device_filter = if (f as usize) < MAX_FILTERS { f } else { NO_FILTER };
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
                    self.cr1 = self.cd_stat() | STAT_TRANS;
                    self.cr2 = 6; // 6 words for a single file
                } else {
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
                    self.cd_curfad = e.firstfad + file_offset;
                    self.fadstoplay = nsect.saturating_sub(file_offset) as i64;
                    self.status = STAT_PLAY;
                    self.cd_device_filter = if (file_filter as usize) < MAX_FILTERS {
                        file_filter
                    } else {
                        NO_FILTER
                    };
                    self.sectorstore = false;
                }
                self.cd_report();
                self.hirq |= HIRQ_CMOK | HIRQ_EHST;
            }
            0x75 => {
                // Abort file: stop any read / transfer, return to PAUSE.
                self.fadstoplay = -1;
                self.status = STAT_PAUSE;
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
                    self.hirq = 0x07C5;
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
        // Read pump: one sector per 1/(75×speed) second while a disc is in.
        if self.disc.is_some() {
            self.sector_accum += cycles;
            let per = MASTER_HZ / (75 * self.cd_speed.max(1) as u64);
            while self.sector_accum >= per {
                self.sector_accum -= per;
                self.play_data();
            }
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
        c.hirq = HIRQ_CMOK | HIRQ_DRDY | HIRQ_DCHG;
        // Clear CMOK by writing a word with CMOK = 0, others = 1.
        c.write16(0x0008, !HIRQ_CMOK);
        assert_eq!(c.hirq, HIRQ_DRDY | HIRQ_DCHG);
        // Writing all-ones clears nothing.
        c.write16(0x0008, 0xFFFF);
        assert_eq!(c.hirq, HIRQ_DRDY | HIRQ_DCHG);
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
        // No-disc PAUSE report (MAME `cr_standard_return`, no image): status
        // PAUSE in CR1, zero geometry in CR2..CR4.
        assert_eq!(c.read16(0x0018), 0x0100);
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

    use crate::disc::Disc;

    /// A 4-sector raw-ISO disc (one Mode-1 data track from FAD 150).
    fn iso_disc() -> Disc {
        Disc::from_iso(vec![0u8; 2048 * 4])
    }

    #[test]
    fn insert_disc_flags_change_and_reports_real_geometry() {
        let mut c = CdBlock::new();
        c.hirq = 0;
        c.insert_disc(iso_disc());
        assert!(c.has_disc());
        assert_eq!(c.hirq & HIRQ_DCHG, HIRQ_DCHG, "disc change flagged");
        // Get Status (cmd 0x00) now reports track 1 / data (0x41) / FAD 150.
        c.write16(0x0018, 0x0000);
        c.write16(0x001C, 0x0000);
        c.write16(0x0020, 0x0000);
        c.write16(0x0024, 0x0000);
        assert_eq!(c.read16(0x0018), 0x0100, "PAUSE status");
        assert_eq!(c.read16(0x001C), 0x4101, "ctrl/adr 0x41, track 1");
        assert_eq!(c.read16(0x0020), 0x0100, "index 1, FAD hi 0");
        assert_eq!(c.read16(0x0024), 0x0096, "FAD 150");
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
        // Pump two sectors (75×2 Hz) plus slack.
        let per = MASTER_HZ / (75 * 2);
        c.tick(per * 2 + 100);
        assert_eq!(c.partitions[0].blocks.len(), 2, "two sectors buffered");
        assert_eq!(c.status, STAT_PAUSE, "paused after the read range");
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
        c.tick(MASTER_HZ / (75 * 2) + 100);
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
    fn data_port_alias_routes_through_the_bus() {
        use crate::Saturn;
        use sh2::bus::{AccessKind, Bus};
        let mut sat = Saturn::with_blank_bios();
        sat.insert_disc(data_disc());
        let cd = &mut sat.bus.cd_block;
        play(cd, 150, 1);
        cd.tick(MASTER_HZ / (75 * 2) + 100);
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
        c.tick(MASTER_HZ / (75 * 2) + 100);
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
        assert_eq!(c.hirq, 0x07C5, "authentication HIRQ pattern");
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
}
