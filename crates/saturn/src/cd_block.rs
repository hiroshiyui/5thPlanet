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
#[allow(dead_code)] // M7 phase 3 (read pump / seek)
const STAT_SEEK: u8 = 0x04; // drive seeking

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
            cr1: (0 << 8) | b'C' as u16,
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
            // Data FIFO: stream the staged transfer buffer (e.g. the TOC) as
            // 16-bit big-endian words, advancing the cursor. Empty / past-end
            // reads return 0. Phase 3 adds sector data + the SCU-DMA port.
            let p = self.xfer_pos;
            let word = match (self.xfer.get(p), self.xfer.get(p + 1)) {
                (Some(&hi), Some(&lo)) => ((hi as u16) << 8) | lo as u16,
                _ => 0,
            };
            if p < self.xfer.len() {
                self.xfer_pos = (p + 2).min(self.xfer.len());
            }
            return word;
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

    /// The 16-bit status word (status code in the high byte) — MAME `cd_stat`.
    fn cd_stat(&self) -> u16 {
        (self.status as u16) << 8
    }

    /// Allocate a free pool block (its `size` set to `sectlenin`), returning the
    /// index, or `None` (latching buffer-full) when the pool is exhausted.
    /// Used by the M7-phase-3 read pump.
    #[allow(dead_code)]
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
