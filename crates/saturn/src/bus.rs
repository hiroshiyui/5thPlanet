//! Saturn-wide memory bus seen by both SH-2 cores.
//!
//! [`SaturnBus`] implements `sh2::Bus`. It dispatches every access by
//! the top 8 bits of the address, after the CPU has already stripped
//! the cached/cache-through indicator. Anything outside the modeled
//! region map reads as 0 and ignores writes (open-bus behaviour).
//!
//! Memory map (physical, after CPU `classify()`):
//!
//! ```text
//!   0x0000_0000..0x000F_FFFF   BIOS ROM (mirrored)
//!   0x0010_0000..0x0017_FFFF   SMPC / system registers (stub)
//!   0x0018_0000..0x001F_FFFF   Backup RAM (32 KiB, mirrored)
//!   0x0020_0000..0x002F_FFFF   Low work RAM (1 MiB)
//!   0x0040_0000..0x004F_FFFF   Sound area (stub)
//!   0x0500_0000..0x05FF_FFFF   A-Bus + B-Bus (stub for M2; VDP1/2/SCSP
//!                              get subdivided in M3+)
//!   0x0600_0000..0x06FF_FFFF   High work RAM (1 MiB)
//!   everything else            open bus (0 on read, drop writes)
//! ```
//!
//! Wait-state numbers are conservative defaults; software can later
//! override them via the SH-2 `BSC` registers (out of M2 scope).

use sh2::bus::{AccessKind, Bus};

use crate::cartridge::Cartridge;
use crate::cd_block::{CD_BLOCK_BASE, CD_BLOCK_END, CdBlock};

/// CD-block data-transfer port — the 32-bit alias the SCU DMA reads sector
/// data from (`src == 0x0581_8000`, special-cased in the SCU). Distinct from
/// the register/FIFO window at [`CD_BLOCK_BASE`].
const CD_DATA_PORT: u32 = 0x0581_8000;
const CD_DATA_PORT_END: u32 = 0x0581_8003;
use crate::memory::{BackupRam, BiosRom, Ram, StubRegisterBank};
use crate::scsp::Scsp;
use crate::scu::{SCU_BASE, SCU_END, Scu};
use crate::smpc::Smpc;
use crate::vdp1::Vdp1;
use crate::vdp2::Vdp2;

pub const BIOS_BASE: u32 = 0x0000_0000;
pub const BIOS_END: u32 = 0x000F_FFFF;
pub const SMPC_BASE: u32 = 0x0010_0000;
pub const SMPC_END: u32 = 0x0017_FFFF;
pub const BACKUP_BASE: u32 = 0x0018_0000;
pub const BACKUP_END: u32 = 0x001F_FFFF;
pub const LOW_WRAM_BASE: u32 = 0x0020_0000;
pub const LOW_WRAM_END: u32 = 0x002F_FFFF;
pub const SOUND_BASE: u32 = 0x0040_0000;
pub const SOUND_END: u32 = 0x004F_FFFF;
/// SCSP sound RAM: 512 KiB at 0x05A0_0000, mirrored through the 1 MiB window,
/// shared between the SH-2 and the hosted sound 68k (which sees it at 0).
pub const SCSP_RAM_BASE: u32 = 0x05A0_0000;
pub const SCSP_RAM_END: u32 = 0x05AF_FFFF;
/// SCSP control + slot + DSP registers at 0x05B0_0000 (mirrored).
pub const SCSP_REGS_BASE: u32 = 0x05B0_0000;
pub const SCSP_REGS_END: u32 = 0x05BF_FFFF;
pub const ABUS_BBUS_BASE: u32 = 0x0500_0000;
pub const ABUS_BBUS_END: u32 = 0x05FF_FFFF;
/// Inter-CPU FRT input-capture (FTI) trigger regions: a 16-bit write to the
/// first pulses the slave SH-2's FTI, the second the master's (Yabause
/// `SSH2/MSH2InputCaptureWriteWord`; Saturn hardware wires the cores' FTI here).
pub const SLAVE_FTI_BASE: u32 = 0x0100_0000;
pub const SLAVE_FTI_END: u32 = 0x017F_FFFF;
pub const MASTER_FTI_BASE: u32 = 0x0180_0000;
pub const MASTER_FTI_END: u32 = 0x01FF_FFFF;
pub const HIGH_WRAM_BASE: u32 = 0x0600_0000;
pub const HIGH_WRAM_END: u32 = 0x06FF_FFFF;

/// SH7604 BSC external-bus timing state (M12 task #8) — a faithful port of
/// Mednafen's per-access model (`sh7095.inc` `BSC_BusRead`/`BSC_BusWrite` +
/// `ss.cpp` `BusRW_DB_CS0`). One instance lives in the bus and is shared by
/// **both** SH-2s, so CPU↔CPU bus arbitration emerges from the shared
/// timestamp exactly as from Mednafen's `SH7095_mem_timestamp` (at our
/// per-instruction `cycle` granularity).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BusTiming {
    /// When the external bus becomes free (global cycles). Each access first
    /// raises it to the accessing CPU's current cycle, then pays its cost on
    /// top — an access never starts while the bus is still busy.
    mem_ts: u64,
    /// Completion time of the most recent CPU store. The SH-2 write buffer:
    /// a store stalls the CPU only while the *previous* store is still on
    /// the bus (Mednafen `write_finish_timestamp` → `MA_until`).
    write_finish: u64,
    /// High-WRAM SDRAM busy-until: a write occupies the SDRAM array 2 cycles
    /// past the bus handoff; the next CS3 access waits for it
    /// (Mednafen `BSC.sdram_finish_time`).
    sdram_finish: u64,
    /// B-bus (SCSP/VDP1/VDP2) write-completion time (Mednafen scu.inc
    /// `BBus_SH2_WriteFinishTS`): an SH-2 B-bus write costs the CPU only +2
    /// cycles while the device-side completion lands here — and only the
    /// *next B-bus access* (read or write) waits it out. CS0/CS3 traffic in
    /// between is unaffected; this is the exact deferred-write serialization
    /// the flat per-access write totals approximated (M12 #8 residual).
    bbus_write_finish: u64,
    /// Last access bookkeeping for the bus-turnaround penalty: +1 on a read
    /// issued in the same cycle as a previous access to a *different* CS
    /// region, +1 on a write issued in the same cycle as a previous *read*.
    last_time: u64,
    last_addr: u32,
    last_was_read: bool,
    /// Incremental-charge bookkeeping: the bus returns per-access stalls that
    /// the CPU *sums* into one instruction, while the model's truth is "the
    /// CPU lands at `mem_ts`". So each access returns only the delta beyond
    /// what this instruction (identified by its `cycle`) already paid —
    /// multiple accesses (a line fill's 4 beats, RTE's two pops) total
    /// exactly `mem_ts − cycle`, never more.
    cycle_seen: u64,
    charged: u64,
}

impl Default for BusTiming {
    fn default() -> Self {
        Self {
            mem_ts: 0,
            write_finish: 0,
            sdram_finish: 0,
            bbus_write_finish: 0,
            // "No previous access": a sentinel that can never equal `mem_ts`,
            // so a fresh bus's first access pays no turnaround penalty.
            last_time: u64::MAX,
            last_addr: 0,
            last_was_read: false,
            cycle_seen: u64::MAX,
            charged: 0,
        }
    }
}

impl BusTiming {
    /// Charge the CPU up to `target` on the global timeline, returning only
    /// the not-yet-paid portion for the instruction at `now`.
    #[inline]
    fn pay(&mut self, now: u64, target: u64) -> u32 {
        if self.cycle_seen != now {
            self.cycle_seen = now;
            self.charged = 0;
        }
        let due = target.saturating_sub(now);
        let inc = due.saturating_sub(self.charged);
        self.charged += inc;
        inc as u32
    }
}

/// CS0 (`< 0x0200_0000`) cost **per 16-bit transaction** — the CS0 bus is
/// 16 bits wide, so a 32-bit access pays twice (Mednafen `BusRW_DB_CS0`).
#[inline]
fn cs0_half_cost(addr: u32) -> u64 {
    match addr {
        BIOS_BASE..=BIOS_END => 8,
        SMPC_BASE..=SMPC_END => 0,
        BACKUP_BASE..=BACKUP_END => 8,
        // Low work RAM (the full 2 MiB hardware window incl. the
        // revision-dependent upper mirror).
        0x0020_0000..=0x003F_FFFF => 7,
        // The inter-CPU FRT-trigger window.
        SLAVE_FTI_BASE..=MASTER_FTI_END => 8,
        // Unknown/unmapped CS0 (incl. the 0x0040_0000 stub region).
        _ => 4,
    }
}

/// Number of 16-bit bus transactions a CS0 / A-bus access of `width` bytes
/// performs.
#[inline]
fn halves(width: u32) -> u64 {
    if width == 4 { 2 } else { 1 }
}

/// B-bus per-region costs (Mednafen `scu.inc BBusRW_DB`):
/// `(read_per_half, write_finish_first, write_finish_second)`. A B-bus read
/// is always two 16-bit halves regardless of access width; a write's
/// device-side completion costs land on `BusTiming::bbus_write_finish`
/// (first half always, second only for a 32-bit access).
#[inline]
fn bbus_cost(addr: u32) -> (u64, u64, u64) {
    match addr {
        // SCSP (the +24/half read and +17 first-half write finish are the
        // M11 VF2 SFX-wedge values).
        0x05A0_0000..=0x05BF_FFFF => (24, 17, 13),
        // VDP1: the register window's second write half is free.
        0x05C0_0000..=0x05D7_FFFF => (14, 9, u64::from(addr & 0x10_0000 == 0)),
        // VDP2.
        0x05E0_0000..=0x05FB_FFFF => (20, 3, 1),
        // Unknown B-bus: only the +2 CPU write prologue applies.
        _ => (0, 0, 0),
    }
}

/// CS1/CS2 A-bus access cost (per-16-bit-transaction, from Mednafen
/// `scu.inc ABusRW_DB`; the cartridge window's from the live ASR0
/// strobe/wait fields). B-bus accesses never reach here — the SH-2 path
/// goes through [`bbus_cost`] and the DMA path has its own arm.
#[inline]
fn cs12_cost(addr: u32, width: u32, write: bool, asr0: u32) -> u64 {
    match addr {
        // A-bus CS0/CS1: the cartridge window. Cost is programmed in ASR0
        // (high half = CS0 below 0x0400_0000, low half = CS1 above):
        // 5 + inter-cycle waits (bits 7:4) + the read/write strobe wait.
        0x0200_0000..=0x04FF_FFFF => {
            let cfg = if addr & 0x0400_0000 != 0 { asr0 & 0xFFFF } else { asr0 >> 16 };
            let strobe = (cfg >> (13 + write as u32)) & 1;
            u64::from(5 + ((cfg >> 4) & 0xF) + strobe) * halves(width)
        }
        // A-bus dummy region: no CPU-visible cost (Mednafen charges only the
        // DMA timelines here).
        0x0500_0000..=0x057F_FFFF => 0,
        // A-bus CS2: the CD-block. +8 per 16-bit transaction.
        0x0580_0000..=0x058F_FFFF => 8 * halves(width),
        _ => 0,
    }
}

// Not `Clone`: the CD-block holds a `Box<dyn SectorSource>` (an image or a
// live drive) that isn't cloneable, and nothing clones the bus anyway.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SaturnBus {
    pub bios: BiosRom,
    pub smpc: Smpc,
    /// Internal battery-backed backup RAM (32 KiB, odd-byte packed).
    pub backup: BackupRam,
    pub low_wram: Ram,
    pub sound: StubRegisterBank,
    pub scu: Scu,
    pub vdp1: Vdp1,
    pub vdp2: Vdp2,
    pub cd_block: CdBlock,
    pub scsp: Scsp,
    /// Rear expansion connector: Extension RAM / backup / ROM cart, or an
    /// empty slot (the default). Mapped at `0x0200_0000..0x04FF_FFFF`.
    pub cartridge: Cartridge,
    pub abus_bbus: StubRegisterBank,
    pub high_wram: Ram,
    /// Per-access external-bus timing (M12 task #8); shared by both CPUs.
    pub timing: BusTiming,
    /// Current global cycle, refreshed by the scheduler before each CPU
    /// step (see `Sh2Entity::step`). Lets time-varying peripheral reads —
    /// notably the SMPC `SF` flag's INTBACK completion — resolve at the
    /// exact instruction that reads them, rather than at a coarse drain
    /// boundary.
    pub cycle: u64,
    /// PC of the CPU currently stepping, refreshed alongside `cycle`. Debug-only
    /// (used by the `SAT_WWATCH` write-watchpoint to name the writing instruction);
    /// `#[serde(skip)]` so it never affects save-state determinism.
    #[serde(skip)]
    pub step_pc: u32,
    /// Which core is currently stepping (`false` = master, `true` = slave),
    /// set by `step_cpus`. Debug-only (lets `SAT_FTILOG` / write-watches name
    /// the *issuing* core); `#[serde(skip)]`, never affects determinism.
    #[serde(skip)]
    pub cur_is_slave: bool,
    /// Pending FRT input-capture (FTI) triggers from inter-CPU signalling: a
    /// 16-bit write to `0x0100_0000..0x017F_FFFF` pulses the *slave*'s FTI,
    /// `0x0180_0000..0x01FF_FFFF` the *master*'s. The bus can't reach the cores,
    /// so it flags here and `Saturn::drain_input_capture` applies it (the
    /// SMPC/SCU drain-at-aggregate pattern). `#[serde(skip)]` — transient signal.
    #[serde(skip)]
    pub slave_input_capture: bool,
    #[serde(skip)]
    pub master_input_capture: bool,
    /// Debug-only programmatic write-watchpoint (the `sdbg` debugger's `bw`):
    /// `(addr, optional value)`. When a bus write matches, [`watch_hit`] records
    /// `(addr, written value, writing-instruction PC)` — the *first* match only,
    /// so the run can stop at the originating store. `#[serde(skip)]` — debug
    /// state, never part of a save state.
    #[serde(skip)]
    pub watch: Option<(u32, Option<u32>)>,
    #[serde(skip)]
    pub watch_hit: Option<(u32, u32, u32)>,
    /// Raster-jitter probe (M13 A1 evidence-first): when `raster_probe_on`, a
    /// read of VCNT/TVSTAT stashes `(reg_offset, stored_value)` here for the
    /// aggregate (`step_cpus`) to compare against the cycle-exact value — the
    /// "bus flags, aggregate drains" pattern, like `slave_input_capture`. Gated
    /// by the flag so a normal run pays only one predictable branch per read.
    /// `#[serde(skip)]` — observer-only, never part of a save state.
    #[serde(skip)]
    pub raster_probe_on: bool,
    /// `(reg_offset, stored_value, reading_pc, read_cycle)` of the last
    /// VCNT/TVSTAT read while the probe is on.
    #[serde(skip)]
    pub last_raster_read: Option<(u32, u16, u32, u64)>,
}

impl SaturnBus {
    /// Pop the last-noted raster-register read `(reg_offset, stored_value)` (M13
    /// A1 jitter probe). The aggregate drains this after each instruction.
    pub fn take_raster_read(&mut self) -> Option<(u32, u16, u32, u64)> {
        self.last_raster_read.take()
    }

    /// Extra SH-2 stall for an access to VDP1 VRAM/FB while the plotter is
    /// drawing — the SH-2↔VDP1 VRAM bus contention (M12 #6). 0 elsewhere. Added
    /// on top of [`waits_for`] so graphics-drawing code can't outrun the
    /// reference; see [`crate::vdp1::Vdp1::draw_slowdown`].
    #[inline]
    fn vdp1_draw_stall(&mut self, addr: u32, write: bool) -> u32 {
        if Vdp1::owns(addr) {
            self.vdp1.draw_slowdown(addr, self.cycle, write)
        } else {
            0
        }
    }

    /// Debug-only: record the first bus write matching the programmatic
    /// write-watchpoint (`bw`). `val` is the size-appropriate written value;
    /// `step_pc` is the PC of the storing instruction (refreshed per step).
    fn note_write(&mut self, addr: u32, val: u32) {
        if self.watch_hit.is_none()
            && let Some((waddr, wval)) = self.watch
            && addr == waddr
            && wval.is_none_or(|v| v == val)
        {
            self.watch_hit = Some((addr, val, self.step_pc));
        }
    }
    /// Construct a bus with the supplied BIOS image. RAM regions are
    /// freshly allocated and zeroed.
    pub fn new(bios: Vec<u8>) -> Self {
        Self {
            bios: BiosRom::new(bios),
            smpc: Smpc::new(),
            backup: BackupRam::new(),
            low_wram: Ram::new(1024 * 1024),
            sound: StubRegisterBank::new("SOUND"),
            scu: Scu::new(),
            vdp1: Vdp1::new(),
            vdp2: Vdp2::new(),
            cd_block: CdBlock::new(),
            scsp: Scsp::new(),
            cartridge: Cartridge::None,
            abus_bbus: StubRegisterBank::new("A/B-BUS"),
            high_wram: Ram::new(1024 * 1024),
            timing: BusTiming::default(),
            cycle: 0,
            step_pc: 0,
            cur_is_slave: false,
            slave_input_capture: false,
            master_input_capture: false,
            watch: None,
            watch_hit: None,
            raster_probe_on: false,
            last_raster_read: None,
        }
    }

    /// Construct with a placeholder all-zero BIOS image — useful for
    /// bus-routing unit tests that don't need real boot code.
    pub fn with_blank_bios() -> Self {
        Self::new(vec![0u8; 512 * 1024])
    }
}

/// Debug (`SAT_FTILOG`): trace inter-CPU FRT input-capture (FTI) wake pulses.
/// The env var is read once and cached, so the per-write check on the FTI path
/// is a single atomic load (not a process-global env lookup) when unset.
#[inline]
fn ftilog() -> bool {
    use std::sync::OnceLock;
    static FTILOG: OnceLock<bool> = OnceLock::new();
    *FTILOG.get_or_init(|| std::env::var_os("SAT_FTILOG").is_some())
}

/// Debug write-watchpoint (boot-divergence investigation): when `SAT_WWATCH=0xADDR`
/// is set, log any write whose byte span covers `ADDR`, with width, value, access
/// kind and cycle. No-op (one cheap env check, cached) when unset.
#[inline]
fn write_watch(addr: u32, size: u32, val: u32, k: AccessKind, cycle: u64, pc: u32) {
    use std::sync::OnceLock;
    static W: OnceLock<Option<u32>> = OnceLock::new();
    let w = *W.get_or_init(|| {
        std::env::var("SAT_WWATCH")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
    });
    // Value-match mode (SAT_WVAL=0xVALUE): log any write of that 32-bit value,
    // regardless of address — finds *where* a known datum lands (e.g. the
    // IP.BIN "SEGA" word 0x53454741 to locate the IP.BIN's WRAM destination).
    static WV: OnceLock<Option<u32>> = OnceLock::new();
    let wv = *WV.get_or_init(|| {
        std::env::var("SAT_WVAL")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
    });
    if let Some(tv) = wv
        && val == tv
    {
        eprintln!(
            "WVAL {tv:08X}: w{}@{addr:08X} {k:?} cyc={cycle} pc={pc:08X}",
            size * 8
        );
    }
    if let Some(t) = w {
        // High work RAM (1 MiB) mirrors across its 16 MiB window, so a write to
        // any mirror of `t` hits the same byte. Compare folded offsets there.
        let fold = |a: u32| {
            if (HIGH_WRAM_BASE..=HIGH_WRAM_END).contains(&a) {
                HIGH_WRAM_BASE + ((a - HIGH_WRAM_BASE) % 0x10_0000)
            } else {
                a
            }
        };
        let (fa, ft) = (fold(addr), fold(t));
        // Optional window (SAT_WWATCH_WIN bytes) so a memset-style clear loop
        // near the target is visible, not just the exact word.
        static WIN: OnceLock<u32> = OnceLock::new();
        let win = *WIN.get_or_init(|| {
            std::env::var("SAT_WWATCH_WIN")
                .ok()
                .and_then(|s| {
                    let s = s.trim();
                    // Accept both hex ("0x40") and decimal ("64").
                    match s.strip_prefix("0x") {
                        Some(h) => u32::from_str_radix(h, 16).ok(),
                        None => s.parse().ok(),
                    }
                })
                .unwrap_or(0)
        });
        if fa.wrapping_add(size) > ft.saturating_sub(win) && fa < ft.wrapping_add(win.max(1)) {
            eprintln!(
                "WWATCH {t:08X}: w{}@{addr:08X} val={val:08X} {k:?} cyc={cycle} pc={pc:08X}",
                size * 8
            );
        }
    }
}

/// Debug read-watchpoint (handshake/poll investigation): when `SAT_RWATCH=0xADDR`
/// is set, log any read whose byte span covers `[ADDR, ADDR+SAT_RWATCH_WIN)`,
/// with width, value, access kind, cycle and PC. Mirrors [`write_watch`] but for
/// reads — finds where the master polls a status word a peripheral/68k writes
/// (e.g. a sound-driver ready signature in sound RAM). No-op when unset. Note the
/// SCSP 68k reads via its own bus, so this isolates *main-CPU* (+ SCU-DMA) reads.
#[inline]
fn read_watch(addr: u32, size: u32, val: u32, k: AccessKind, cycle: u64, pc: u32) {
    use std::sync::OnceLock;
    static R: OnceLock<Option<u32>> = OnceLock::new();
    let r = *R.get_or_init(|| {
        std::env::var("SAT_RWATCH")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
    });
    let Some(t) = r else { return };
    static WIN: OnceLock<u32> = OnceLock::new();
    let win = *WIN.get_or_init(|| {
        std::env::var("SAT_RWATCH_WIN")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4)
    });
    if addr.wrapping_add(size) > t && addr < t.wrapping_add(win.max(1)) {
        eprintln!(
            "RWATCH {t:08X}: r{}@{addr:08X} val={val:08X} {k:?} cyc={cycle} pc={pc:08X}",
            size * 8
        );
    }
}

impl SaturnBus {
    /// Per-access external-bus cost (M12 task #8): the BSC state machine.
    ///
    /// A CPU access (`Fetch`/`Data`) raises the shared bus timestamp to the
    /// caller's cycle, pays the turnaround penalty and the region cost on the
    /// bus timeline, and returns the CPU-visible stall: a **read** waits
    /// until the data arrives (`mem_ts − now`); a **write** goes through the
    /// SH-2 write buffer and stalls only while the *previous* store is still
    /// on the bus. `LineFill` continuation beats ride the SDRAM burst (free
    /// on CS3, full price elsewhere; no turnaround/bookkeeping — Mednafen
    /// `BurstHax`). `Dma` accesses are cost-only — the DMA engines keep their
    /// own timeline (Mednafen `SH2DMAHax`) and must not disturb the CPU bus
    /// state (CS3 DMA: read 6 / write 3).
    fn charge(&mut self, addr: u32, width: u32, write: bool, kind: AccessKind) -> u32 {
        // SH-2↔VDP1-VRAM draw contention (M12 #6) rides on top of the base
        // cost; it is 0 outside the VDP1 window.
        let draw = u64::from(self.vdp1_draw_stall(addr, write));
        if kind == AccessKind::Dma {
            // DMA-timeline costs (Mednafen scu.inc `dma_time_thing`): the SCU
            // paces itself much cheaper than the SH-2 path — B-bus VDP1/VDP2
            // cost 1 per 16-bit access and the SCSP 13 (read or write); a
            // C-bus SDRAM read costs 6 per 32-bit word and a C-bus write is
            // free (`DMA_Write` WriteBus==2 charges nothing).
            let c = match addr {
                0x0600_0000..=0x07FF_FFFF => {
                    if write {
                        0
                    } else {
                        6
                    }
                }
                0x05A0_0000..=0x05BF_FFFF => 13 * halves(width),
                0x05C0_0000..=0x05FB_FFFF => halves(width),
                a if a < 0x0200_0000 => cs0_half_cost(a) * halves(width),
                a => cs12_cost(a, width, write, self.scu.asr0),
            };
            return (c + draw) as u32;
        }
        let burst = kind == AccessKind::LineFill;
        let now = self.cycle;
        let asr0 = self.scu.asr0;
        let t = &mut self.timing;
        if t.mem_ts < now {
            t.mem_ts = now;
        }
        if !burst {
            // Bus turnaround: one dead cycle when the bus switches CS region
            // between back-to-back reads, or direction read→write.
            let same_cycle = t.mem_ts == t.last_time;
            if write {
                t.mem_ts += u64::from(same_cycle && t.last_was_read);
            } else {
                t.mem_ts += u64::from(same_cycle && ((t.last_addr ^ addr) & 0x0600_0000) != 0);
            }
        }
        match addr {
            a if a < 0x0200_0000 => {
                t.mem_ts += cs0_half_cost(a) * halves(width);
            }
            // CS3: high work RAM, 32-bit synchronous DRAM. Any width is one
            // SDRAM operation; a write completes in the array 2 cycles after
            // the bus handoff and blocks the next CS3 access until then.
            0x0600_0000..=0x07FF_FFFF => {
                if !burst {
                    if t.mem_ts < t.sdram_finish {
                        t.mem_ts = t.sdram_finish;
                    }
                    if write {
                        t.mem_ts += 2;
                        t.sdram_finish = t.mem_ts + 2;
                    } else {
                        t.mem_ts += 7;
                    }
                }
            }
            // B-bus: SCSP / VDP1 / VDP2 (Mednafen scu.inc BBusRW_DB). Every
            // access first waits out the previous B-bus write's device-side
            // completion; a write then costs the CPU only +2 and posts its
            // own completion on `bbus_write_finish`; a read is always two
            // 16-bit halves at the region's per-half latency.
            a @ 0x05A0_0000..=0x05FB_FFFF => {
                if t.mem_ts < t.bbus_write_finish {
                    t.mem_ts = t.bbus_write_finish;
                }
                let (rd_half, wf_first, wf_second) = bbus_cost(a);
                if write {
                    t.mem_ts += 2;
                    t.bbus_write_finish =
                        t.mem_ts + wf_first + if width == 4 { wf_second } else { 0 };
                } else {
                    t.mem_ts += rd_half * 2;
                }
                t.mem_ts += draw;
            }
            a => {
                t.mem_ts += cs12_cost(a, width, write, asr0) + draw;
            }
        }
        if !burst {
            t.last_addr = addr;
            t.last_was_read = !write;
            t.last_time = t.mem_ts;
        }
        // A read stalls the CPU until the data arrives; a write only until
        // the *previous* store cleared the write buffer (its own bus time
        // runs in the background — the SH-2 write buffer). `pay` makes
        // repeated charges within one instruction incremental.
        if write {
            let target = t.write_finish; // the previous store's completion
            t.write_finish = t.mem_ts;
            t.pay(now, target)
        } else {
            t.pay(now, t.mem_ts)
        }
    }
}

impl Bus for SaturnBus {
    fn read8(&mut self, addr: u32, k: AccessKind) -> (u8, u32) {
        let v = match addr {
            BIOS_BASE..=BIOS_END => self.bios.read8(addr - BIOS_BASE),
            SMPC_BASE..=SMPC_END => {
                self.smpc.settle_intback(self.cycle);
                self.smpc.read8(addr - SMPC_BASE)
            }
            BACKUP_BASE..=BACKUP_END => self.backup.read8(addr - BACKUP_BASE),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.read8(addr - LOW_WRAM_BASE),
            SOUND_BASE..=SOUND_END => self.sound.read8(addr - SOUND_BASE),
            a if Cartridge::owns(a) => self.cartridge.read8(a),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.read8(addr - CD_BLOCK_BASE),
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.read8(a)
            }
            a if Vdp2::owns(a) => self.vdp2.read8(a),
            SCU_BASE..=SCU_END => self.scu.read8(addr - SCU_BASE),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.read8(addr - SCSP_RAM_BASE),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.read8(addr - SCSP_REGS_BASE),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.read8(addr - ABUS_BBUS_BASE),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.read8(addr - HIGH_WRAM_BASE),
            _ => 0,
        };
        read_watch(addr, 1, v as u32, k, self.cycle, self.step_pc);
        (v, self.charge(addr, 1, false, k))
    }

    fn read16(&mut self, addr: u32, k: AccessKind) -> (u16, u32) {
        let v = match addr {
            BIOS_BASE..=BIOS_END => self.bios.read16(addr - BIOS_BASE),
            SMPC_BASE..=SMPC_END => {
                self.smpc.settle_intback(self.cycle);
                self.smpc.read16(addr - SMPC_BASE)
            }
            BACKUP_BASE..=BACKUP_END => self.backup.read16(addr - BACKUP_BASE),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.read16(addr - LOW_WRAM_BASE),
            SOUND_BASE..=SOUND_END => self.sound.read16(addr - SOUND_BASE),
            a if Cartridge::owns(a) => self.cartridge.read16(a),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.read16(addr - CD_BLOCK_BASE),
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.read16(a)
            }
            a if Vdp2::owns(a) => self.vdp2.read16(a),
            SCU_BASE..=SCU_END => self.scu.read16(addr - SCU_BASE),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.read16(addr - SCSP_RAM_BASE),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.read16(addr - SCSP_REGS_BASE),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.read16(addr - ABUS_BBUS_BASE),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.read16(addr - HIGH_WRAM_BASE),
            _ => 0,
        };
        read_watch(addr, 2, v as u32, k, self.cycle, self.step_pc);
        // Raster-jitter probe: note a VCNT (0x05F8_000A) / TVSTAT (0x05F8_0004)
        // read so the aggregate can compare the stored value to the cycle-exact
        // one. Canonical addresses only (games read those); off by default.
        if self.raster_probe_on && (addr == 0x05F8_0004 || addr == 0x05F8_000A) {
            self.last_raster_read =
                Some((addr - crate::vdp2::REGS_BASE, v, self.step_pc, self.cycle));
        }
        (v, self.charge(addr, 2, false, k))
    }

    /// Side-effect-free peek of the cacheable backing-memory regions (RAM/ROM
    /// where code lives) for the cache stale-fetch detector. Other regions
    /// (registers, open bus) return `None` — there's no stable value to
    /// compare a cached line against. Touches no timing/watch state.
    fn peek16(&self, addr: u32) -> Option<u16> {
        match addr {
            BIOS_BASE..=BIOS_END => Some(self.bios.read16(addr - BIOS_BASE)),
            LOW_WRAM_BASE..=LOW_WRAM_END => Some(self.low_wram.read16(addr - LOW_WRAM_BASE)),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => Some(self.high_wram.read16(addr - HIGH_WRAM_BASE)),
            _ => None,
        }
    }

    fn read32(&mut self, addr: u32, k: AccessKind) -> (u32, u32) {
        let v = match addr {
            BIOS_BASE..=BIOS_END => self.bios.read32(addr - BIOS_BASE),
            SMPC_BASE..=SMPC_END => {
                self.smpc.settle_intback(self.cycle);
                self.smpc.read32(addr - SMPC_BASE)
            }
            BACKUP_BASE..=BACKUP_END => self.backup.read32(addr - BACKUP_BASE),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.read32(addr - LOW_WRAM_BASE),
            SOUND_BASE..=SOUND_END => self.sound.read32(addr - SOUND_BASE),
            a if Cartridge::owns(a) => self.cartridge.read32(a),
            // CD data-transfer port alias (the SCU-DMA path; see saturn_scu).
            CD_DATA_PORT..=CD_DATA_PORT_END => self.cd_block.read_data_port(),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.read32(addr - CD_BLOCK_BASE),
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.read32(a)
            }
            a if Vdp2::owns(a) => self.vdp2.read32(a),
            SCU_BASE..=SCU_END => self.scu.read32(addr - SCU_BASE),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.read32(addr - SCSP_RAM_BASE),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.read32(addr - SCSP_REGS_BASE),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.read32(addr - ABUS_BBUS_BASE),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.read32(addr - HIGH_WRAM_BASE),
            _ => 0,
        };
        read_watch(addr, 4, v, k, self.cycle, self.step_pc);
        (v, self.charge(addr, 4, false, k))
    }

    fn write8(&mut self, addr: u32, val: u8, k: AccessKind) -> u32 {
        write_watch(addr, 1, val as u32, k, self.cycle, self.step_pc);
        self.note_write(addr, val as u32);
        match addr {
            BIOS_BASE..=BIOS_END => self.bios.write_ignored(),
            SMPC_BASE..=SMPC_END => self.smpc.write8(addr - SMPC_BASE, val),
            BACKUP_BASE..=BACKUP_END => self.backup.write8(addr - BACKUP_BASE, val),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.write8(addr - LOW_WRAM_BASE, val),
            SOUND_BASE..=SOUND_END => self.sound.write8(addr - SOUND_BASE, val),
            a if Cartridge::owns(a) => self.cartridge.write8(a, val),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.write8(addr - CD_BLOCK_BASE, val),
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.write8(a, val)
            }
            a if Vdp2::owns(a) => self.vdp2.write8(a, val),
            SCU_BASE..=SCU_END => self.scu.write8(addr - SCU_BASE, val),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.write8(addr - SCSP_RAM_BASE, val),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.write8(addr - SCSP_REGS_BASE, val),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.write8(addr - ABUS_BBUS_BASE, val),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.write8(addr - HIGH_WRAM_BASE, val),
            // Inter-CPU FTI trigger. Hardware wires the cores' input-capture pin
            // to the bus, so a write of *any* width pulses it — not just the
            // 16-bit form (`write16`). Mirror that here so a `MOV.B` wake isn't
            // silently dropped (a latent lost-wakeup).
            SLAVE_FTI_BASE..=SLAVE_FTI_END => {
                self.slave_input_capture = true;
                if ftilog() {
                    eprintln!("FTI->slave addr={addr:08X} val={val:02X} by={} pc={:08X} cyc={} w8", if self.cur_is_slave { "SLAVE" } else { "MASTER" }, self.step_pc, self.cycle);
                }
            }
            MASTER_FTI_BASE..=MASTER_FTI_END => {
                self.master_input_capture = true;
                if ftilog() {
                    eprintln!("FTI->master addr={addr:08X} val={val:02X} by={} pc={:08X} cyc={} w8", if self.cur_is_slave { "SLAVE" } else { "MASTER" }, self.step_pc, self.cycle);
                }
            }
            _ => {}
        }
        self.charge(addr, 1, true, k)
    }

    fn write16(&mut self, addr: u32, val: u16, k: AccessKind) -> u32 {
        write_watch(addr, 2, val as u32, k, self.cycle, self.step_pc);
        self.note_write(addr, val as u32);
        match addr {
            BIOS_BASE..=BIOS_END => self.bios.write_ignored(),
            SMPC_BASE..=SMPC_END => self.smpc.write16(addr - SMPC_BASE, val),
            BACKUP_BASE..=BACKUP_END => self.backup.write16(addr - BACKUP_BASE, val),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.write16(addr - LOW_WRAM_BASE, val),
            SOUND_BASE..=SOUND_END => self.sound.write16(addr - SOUND_BASE, val),
            a if Cartridge::owns(a) => self.cartridge.write16(a, val),
            CD_BLOCK_BASE..=CD_BLOCK_END => {
                // Record the issuing master PC so the CD command trace can name
                // the loader code that drove each command (debug-only).
                self.cd_block.caller_pc = self.step_pc;
                self.cd_block.write16(addr - CD_BLOCK_BASE, val)
            }
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.write16(a, val)
            }
            a if Vdp2::owns(a) => self.vdp2.write16(a, val),
            SCU_BASE..=SCU_END => self.scu.write16(addr - SCU_BASE, val),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.write16(addr - SCSP_RAM_BASE, val),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.write16(addr - SCSP_REGS_BASE, val),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.write16(addr - ABUS_BBUS_BASE, val),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.write16(addr - HIGH_WRAM_BASE, val),
            // Inter-CPU FRT input-capture (FTI) trigger: a 16-bit write to this
            // region pulses the *other* SH-2's free-running-timer input capture
            // (the Saturn slave/master "wake" signal). Drained at the aggregate.
            SLAVE_FTI_BASE..=SLAVE_FTI_END => {
                self.slave_input_capture = true;
                if ftilog() {
                    eprintln!("FTI->slave addr={addr:08X} val={val:04X} by={} pc={:08X} cyc={}", if self.cur_is_slave { "SLAVE" } else { "MASTER" }, self.step_pc, self.cycle);
                }
            }
            MASTER_FTI_BASE..=MASTER_FTI_END => {
                self.master_input_capture = true;
                if ftilog() {
                    eprintln!("FTI->master addr={addr:08X} val={val:04X} by={} pc={:08X} cyc={}", if self.cur_is_slave { "SLAVE" } else { "MASTER" }, self.step_pc, self.cycle);
                }
            }
            _ => {}
        }
        self.charge(addr, 2, true, k)
    }

    fn write32(&mut self, addr: u32, val: u32, k: AccessKind) -> u32 {
        write_watch(addr, 4, val, k, self.cycle, self.step_pc);
        self.note_write(addr, val);
        match addr {
            BIOS_BASE..=BIOS_END => self.bios.write_ignored(),
            SMPC_BASE..=SMPC_END => self.smpc.write32(addr - SMPC_BASE, val),
            BACKUP_BASE..=BACKUP_END => self.backup.write32(addr - BACKUP_BASE, val),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.write32(addr - LOW_WRAM_BASE, val),
            SOUND_BASE..=SOUND_END => self.sound.write32(addr - SOUND_BASE, val),
            a if Cartridge::owns(a) => self.cartridge.write32(a, val),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.write32(addr - CD_BLOCK_BASE, val),
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.write32(a, val)
            }
            a if Vdp2::owns(a) => self.vdp2.write32(a, val),
            SCU_BASE..=SCU_END => self.scu.write32(addr - SCU_BASE, val),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.write32(addr - SCSP_RAM_BASE, val),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.write32(addr - SCSP_REGS_BASE, val),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.write32(addr - ABUS_BBUS_BASE, val),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.write32(addr - HIGH_WRAM_BASE, val),
            // Inter-CPU FTI trigger — any-width write pulses it (see `write8`).
            SLAVE_FTI_BASE..=SLAVE_FTI_END => {
                self.slave_input_capture = true;
                if ftilog() {
                    eprintln!("FTI->slave addr={addr:08X} val={val:08X} by={} pc={:08X} cyc={} w32", if self.cur_is_slave { "SLAVE" } else { "MASTER" }, self.step_pc, self.cycle);
                }
            }
            MASTER_FTI_BASE..=MASTER_FTI_END => {
                self.master_input_capture = true;
                if ftilog() {
                    eprintln!("FTI->master addr={addr:08X} val={val:08X} by={} pc={:08X} cyc={} w32", if self.cur_is_slave { "SLAVE" } else { "MASTER" }, self.step_pc, self.cycle);
                }
            }
            _ => {}
        }
        self.charge(addr, 4, true, k)
    }
}
