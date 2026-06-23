//! Top-level CPU state and instruction dispatch.
//!
//! Task #4 lands the full SH-2 opcode set. Cycle counts are the base values
//! from *SH-1/SH-2 Software Manual* Appendix A; pipeline interlocks that
//! refine them land in task #5. Exception handling (TRAPA stack frame, RTE
//! pop, slot-illegal / address-error vectoring) gets its full plumbing in
//! task #7 — what's here is enough for software TRAPA/RTE round-trips on a
//! well-behaved fixture.

use crate::bus::{AccessKind, Bus};
use crate::cache::{self, Cache};
use crate::isa::Op;
use crate::onchip::OnChip;
use crate::pipeline::Pipeline;
use crate::regs::{Registers, Sr};

/// Decode an SH-2 logical address into (physical Saturn-bus address,
/// cacheable?). Cached and cache-through regions alias the same physical
/// memory; the cacheable flag tells the CPU whether to consult its cache.
/// Addresses outside both regions pass through unmodified — those are
/// SH-2 control areas the bus typically returns open-bus for.
#[inline]
const fn classify(addr: u32) -> (u32, bool) {
    match addr {
        0x0000_0000..=0x1FFF_FFFF => (addr, true),
        0x2000_0000..=0x3FFF_FFFF => (addr & 0x1FFF_FFFF, false),
        _ => (addr, false),
    }
}

/// True for the SH7604 **associative-purge** address space — regions 2 and 5
/// (`0x4000_0000..0x5FFF_FFFF` and `0xA000_0000..0xBFFF_FFFF`). An access here
/// invalidates the matching cache line by address ([`Cache::assoc_purge`]) and
/// does **not** reach the external bus; reads return open bus (`!0`). This is
/// how software drops a single stale line for cross-master coherency. (*SH7604
/// Hardware Manual* §8; Mednafen `sh7095.inc` region 2/5.)
#[inline]
const fn is_assoc_purge(addr: u32) -> bool {
    matches!(addr >> 29, 2 | 5)
}

/// True for any address in the on-chip DIVU register block (`FFFFFF00..1F`).
/// A read of any of these stalls the CPU until the hardware divider retires
/// (M13 D1; Mednafen `divide_finish_timestamp`).
#[inline]
const fn is_divu_reg(addr: u32) -> bool {
    matches!(addr & 0x1FF, 0x100..=0x11F)
}

/// SH7604 Cache Control Register. 8-bit, byte-accessed. It lives in the
/// on-chip address window but controls [`Cache`], not [`OnChip`], so the
/// memory path routes it here explicitly rather than letting the generic
/// `OnChip::owns` dispatch swallow it. (*SH7604 Hardware Manual* §8, CCR.)
const CCR_ADDR: u32 = 0xFFFF_FE92;

/// FRT free-running-timer control/status register (*SH7604 HW Manual* §11). Bit 7
/// (ICF) is the inter-CPU input-capture flag a sibling SH-2's FTI pulse sets and
/// the other polls/clears; [`Cpu::dbg_ftcsr`] traces accesses to it.
const FTCSR_ADDR: u32 = 0xFFFF_FE11;

/// Debug-only tally of `Data` reads whose **physical** address lands in
/// `[lo, hi)`, bucketed by how the cache treated them. Used to test whether a
/// shared region (e.g. SCSP sound RAM, written by the 68k) is read by this
/// SH-2 through **cacheable** addresses — where a stale resident line can hide
/// the 68k's fresh value (`hit`) — versus **cache-through**/cache-off paths
/// that always reach RAM (`through`/`bypass`). `miss` is a cacheable read that
/// went to RAM this time but left the line resident for future (possibly
/// stale) hits. Observer-only; never serialized.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadWatch {
    pub lo: u32,
    pub hi: u32,
    /// Cache-through (`0x2000_0000` alias) — always fresh from RAM.
    pub through: u64,
    /// Served from a resident cache line — **staleness-prone**.
    pub hit: u64,
    /// Cacheable miss — fetched fresh now, but installs a line.
    pub miss: u64,
    /// Cacheable address but the cache was disabled — fresh from RAM.
    pub bypass: u64,
}

/// One Hitachi SH-2 (SH7604) core.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cpu {
    pub regs: Registers,
    pub pipeline: Pipeline,
    pub cache: Cache,
    /// SH7604 on-chip peripherals (FFFFFE00..FFFFFFFF). Memory accesses
    /// to that range are routed here by [`Cpu::mem_read32`] et al. before
    /// reaching the external [`Bus`].
    pub onchip: OnChip,
    /// When `Some`, the next instruction is a delay-slot fetch; after it
    /// executes PC is overwritten with the contained target.
    pub(crate) pending_branch: Option<u32>,
    /// True only while the slot instruction itself is executing.
    pub(crate) in_delay_slot: bool,
    /// The destination register of the most recently retired load. The
    /// very next instruction that reads it pays a 1-cycle stall, then
    /// this is cleared regardless.
    pub(crate) load_dest_pending: Option<u8>,
    /// Debug only: the last CPU *exception* taken as `(vector, faulting PC)` —
    /// illegal (4) / slot-illegal (6) / address-error (9/10) / TRAPA, but not
    /// hardware interrupts. Used to diagnose spurious faults; not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub last_fault: Option<(u8, u32)>,
    /// Debug only: the raw 16-bit word the fetch returned for the most recent
    /// *general-illegal* (vector 4) instruction. Lets a spurious illegal be
    /// compared against the external-memory word (a mismatch = stale I-cache).
    #[cfg_attr(feature = "serde", serde(skip))]
    pub last_illegal_word: Option<u16>,
    /// Debug only: when set, every instruction-fetch cache *hit* is compared
    /// against true backing memory ([`Bus::peek16`]); the first mismatch is
    /// recorded in [`Self::dbg_stale_fetch`]. The stale-instruction-cache
    /// coherency-hang detector (e.g. Sangokushi V). Off by default — the
    /// per-fetch `peek16` is a debug cost. Not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_detect_stale: bool,
    /// Debug only: the first stale instruction-fetch caught while
    /// [`Self::dbg_detect_stale`] is set, as `(phys_addr, cached, memory,
    /// cycle)`. Not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_stale_fetch: Option<(u32, u16, u16, u64)>,
    /// Debug only: count of stale-detector comparisons actually performed
    /// (cache hits where [`Bus::peek16`] returned a value). Confirms the
    /// detector ran rather than silently skipping. Not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_stale_checks: u64,
    /// Debug only: extra stall cycles charged on every instruction-fetch cache
    /// hit. A timing-probe knob to slow the master (or slave) without changing
    /// any cache value/content — used to test timing-sensitivity hypotheses
    /// (e.g. the Sangokushi V inter-CPU race). 0 = no effect (default). Not
    /// serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_slow_fetch: u32,
    /// Debug only: when `Some`, tallies `Data` reads to a physical range by
    /// cache treatment — see [`ReadWatch`]. Set via [`Cpu::enable_read_watch`].
    #[cfg_attr(feature = "serde", serde(skip))]
    pub read_watch: Option<ReadWatch>,
    /// Debug only: register-write watch — `Some((idx, val))` logs the executing
    /// PC each time `R[idx]` *transitions to* `val`. Only active once the cycle
    /// reaches [`Self::dbg_regwatch_after`], so a long run stays full-speed until
    /// the window of interest. Beats the bp stack-heuristic / line-granular
    /// read-watch confounds when hunting where a value is computed into a
    /// register. Off by default; not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_regwatch: Option<(u8, u32)>,
    /// Debug only: only check [`Self::dbg_regwatch`] once `pipeline.cycles >=`
    /// this (0 = always). Keeps the per-instruction cost off the hot path until
    /// the target window. Not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_regwatch_after: u64,
    /// Debug only: whether `R[idx] == val` held after the previous instruction —
    /// the transition-edge detector for [`Self::dbg_regwatch`]. Not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_regwatch_armed: bool,
    /// Debug only: transitions [`Self::dbg_regwatch`] fired, as
    /// `(executing_pc, cycle)`. Capped to avoid unbounded growth. Not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_regwatch_log: alloc::vec::Vec<(u32, u64)>,
    /// Debug only: when set, record every byte access to FTCSR ([`FTCSR_ADDR`]) —
    /// the FRT status reg whose ICF bit carries the inter-CPU FTI done-flag. For
    /// hunting where the master detects/clears the slave's "decode done" pulse
    /// (the FILM-player ping-pong re-arm). Off by default; not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_ftcsr: bool,
    /// Debug only: FTCSR byte accesses captured while [`Self::dbg_ftcsr`], as
    /// `(pc, value, is_write, cycle)` (pc is the post-fetch PC, ≈ access instr
    /// + 2). Capped to avoid unbounded growth. Not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_ftcsr_log: alloc::vec::Vec<(u32, u8, bool, u64)>,
    /// Debug only: when >0, push the executing PC into [`Self::dbg_pc_log`] each
    /// step and decrement. Armed (once) by the first FTCSR *write* while
    /// [`Self::dbg_ftcsr`] — captures the path right after the master clears the
    /// inter-CPU done-flag (the streaming round-2 decision). Not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_pc_capture: usize,
    /// Debug only: PCs captured under [`Self::dbg_pc_capture`]. Not serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub dbg_pc_log: alloc::vec::Vec<u32>,
    /// Pre-decoded instruction table: `decode(w)` for every 16-bit word `w`.
    /// Turns the per-instruction decode (a match + operand-field extraction +
    /// `Op` construction, ~7% of run time) into one indexed load. Pure function
    /// of the word, so it's identical for every CPU and bit-identical to calling
    /// `decode` — derived state, rebuilt on construction/load, never serialized.
    /// ~0.5 MiB; built once in [`Self::new`].
    #[cfg_attr(feature = "serde", serde(skip, default = "build_decode_lut"))]
    decode_lut: alloc::boxed::Box<[Op]>,
}

/// Build the 65536-entry pre-decode table (see [`Cpu::decode_lut`]).
fn build_decode_lut() -> alloc::boxed::Box<[Op]> {
    (0..=u16::MAX)
        .map(crate::decoder::decode)
        .collect::<alloc::vec::Vec<Op>>()
        .into_boxed_slice()
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// A fresh CPU in the SH7604 cold-reset state (registers at their power-on
    /// values; the first `step` fetches PC/SP from the vector table).
    pub fn new() -> Self {
        Self {
            regs: Registers::new_at_reset(),
            pipeline: Pipeline::new(),
            cache: Cache::new(),
            onchip: OnChip::new(),
            pending_branch: None,
            in_delay_slot: false,
            load_dest_pending: None,
            last_fault: None,
            last_illegal_word: None,
            dbg_detect_stale: false,
            dbg_stale_fetch: None,
            dbg_stale_checks: 0,
            dbg_slow_fetch: 0,
            read_watch: None,
            dbg_regwatch: None,
            dbg_regwatch_after: 0,
            dbg_regwatch_armed: false,
            dbg_regwatch_log: alloc::vec::Vec::new(),
            dbg_ftcsr: false,
            dbg_ftcsr_log: alloc::vec::Vec::new(),
            dbg_pc_capture: 0,
            dbg_pc_log: alloc::vec::Vec::new(),
            decode_lut: build_decode_lut(),
        }
    }

    /// Debug: start tallying `Data` reads whose physical address is in
    /// `[lo, hi)` by cache treatment (see [`ReadWatch`]). Resets the counters.
    pub fn enable_read_watch(&mut self, lo: u32, hi: u32) {
        self.read_watch = Some(ReadWatch {
            lo,
            hi,
            ..Default::default()
        });
    }

    /// Debug: record one watched read into the active [`ReadWatch`], if the
    /// physical address is in range. `bucket` selects the counter (0 = through,
    /// 1 = hit, 2 = miss, 3 = bypass).
    #[inline]
    fn note_watched_read(&mut self, phys: u32, kind: AccessKind, bucket: u8) {
        if !matches!(kind, AccessKind::Data) {
            return;
        }
        if let Some(w) = &mut self.read_watch
            && phys >= w.lo
            && phys < w.hi
        {
            match bucket {
                0 => w.through += 1,
                1 => w.hit += 1,
                2 => w.miss += 1,
                _ => w.bypass += 1,
            }
        }
    }

    /// Power-on reset. Loads PC and R15 from the reset vector.
    pub fn reset(&mut self, bus: &mut impl Bus) {
        self.regs = Registers::new_at_reset();
        self.pipeline = Pipeline::new();
        self.pending_branch = None;
        self.in_delay_slot = false;
        self.load_dest_pending = None;
        let (pc, _) = self.mem_read32(0x0000_0000, AccessKind::Data, bus);
        let (sp, _) = self.mem_read32(0x0000_0004, AccessKind::Data, bus);
        self.regs.pc = pc;
        self.regs.r[15] = sp;
    }

    /// Mark this core as the **slave** SH-2: `BCR1` bit 15 (the SH7604
    /// master/slave bit) then reads 1, so the Saturn BIOS cold-start takes the
    /// slave path and does **not** re-initialize work RAM (which would clobber
    /// the running game when the slave is `SSHON`-released). A hardware/pin
    /// property — it survives [`reset`], so the host sets it once.
    pub fn set_bsc_slave(&mut self, is_slave: bool) {
        self.onchip.bsc.is_slave = is_slave;
    }

    /// True when the next [`step`] will execute a branch delay slot
    /// (a branch is pending). Used by trace tooling to match reference
    /// emulators that execute the delay slot inside the branch handler.
    pub fn next_is_delay_slot(&self) -> bool {
        self.pending_branch.is_some()
    }

    /// Apply an inter-CPU FRT input-capture (FTI) edge, materializing the lazy
    /// FRC up to this CPU's current cycle first so FICR latches the *current*
    /// counter, not a stale event-time value (Stage B). Returns whether the
    /// input-capture interrupt is enabled (TIER.ICIE). The host calls this on
    /// the *target* CPU (the sibling pulses its FTI); it reads the cycle from
    /// the target's own `pipeline.cycles`. The set ICF arms ICI on the next
    /// `refresh_interrupts` (per-instruction in Stage B).
    pub fn fti_input_capture(&mut self) -> bool {
        self.onchip.frt_wdt_update(self.pipeline.cycles);
        let icie = self.onchip.frt.input_capture();
        self.onchip.refresh_interrupts(); // recalc-on-change: ICF may arm ICI
        icie
    }

    /// Fetch + decode + execute one instruction, then advance the on-chip
    /// time-driven peripherals by the cycles consumed: the FRT and WDT
    /// counters tick, any enabled DMAC channel runs, and the level-triggered
    /// on-chip interrupt pending bits are refreshed so they're visible at the
    /// next instruction boundary. Returns the total cycles.
    pub fn step(&mut self, bus: &mut impl Bus) -> u32 {
        // Register-write watch (debug): if `R[idx]` transitions to the target
        // value within the cycle window, log the executing PC. Off-path when
        // disabled (one `is_some` check); `instr_pc` is captured only when armed.
        let instr_pc = if self.dbg_regwatch.is_some() { self.regs.pc } else { 0 };
        if self.dbg_pc_capture > 0 {
            self.dbg_pc_log.push(self.regs.pc);
            self.dbg_pc_capture -= 1;
        }
        let cost = self.step_instruction(bus);
        if let Some((idx, val)) = self.dbg_regwatch
            && self.pipeline.cycles >= self.dbg_regwatch_after
        {
            let now_match = self.regs.r[(idx & 0x0F) as usize] == val;
            if now_match && !self.dbg_regwatch_armed && self.dbg_regwatch_log.len() < 4096 {
                self.dbg_regwatch_log.push((instr_pc, self.pipeline.cycles));
            }
            self.dbg_regwatch_armed = now_match;
        }
        // Event-scheduled FRT/WDT (Stage B): only materialize when this
        // instruction reached the next scheduled timer edge — otherwise the
        // per-instruction cost is a single (well-predicted) compare. Gating at
        // end-of-step (rather than top-of-step) keeps the materialize cycle and
        // the interrupt phase identical to the per-instruction model, so the
        // event gate is behaviour-neutral; register reads/writes catch the
        // counters up on demand via `timer_sync_pre`/`_post`. A timer-register
        // write may have pulled `next_ts` in, so re-check after `run_dma`.
        let now = self.pipeline.cycles;
        if now >= self.onchip.timer_next_ts() {
            self.onchip.frt_wdt_update(now);
            self.onchip.frt_wdt_recalc_net(now);
        }
        self.run_dma(bus);
        // Recalc-on-change (Stage C): the INTC is no longer re-armed every
        // instruction. Its inputs change only at timer events (handled in
        // frt_wdt_update), on-chip register writes (mem_write*), DMAC
        // transfer-end (run_dma_channel), and FTI capture (fti_input_capture) —
        // each calls refresh_interrupts itself.
        cost
    }

    /// The bare instruction step: interrupt boundary → fetch → decode →
    /// interlock → execute → cycle-accumulate. [`Cpu::step`] wraps this to
    /// drive the on-chip peripherals.
    fn step_instruction(&mut self, bus: &mut impl Bus) -> u32 {
        // ---- Interrupt boundary: check pending INTC sources first ----
        // SH-2 only accepts interrupts at instruction boundaries (never
        // inside a delay slot — that's a hardware invariant).
        if self.pending_branch.is_none()
            && let Some((src, level)) = self.onchip.intc.next_pending(self.regs.sr.imask())
        {
            let vector = self.onchip.intc.vector_for(src);
            self.onchip.intc.acknowledge(src);
            // External (level-triggered) sources stay asserted until the
            // line drops; on-chip sources are edge-triggered and we just
            // cleared the bit above. The Saturn-side glue will re-raise
            // External(level) on every step while IRL is held.
            let cost = self.take_exception(vector, Some(level), bus);
            self.pipeline.advance(cost);
            return cost;
        }

        let instr_pc = self.regs.pc;
        let (word, fetch_stall) = self.mem_read16(instr_pc, AccessKind::Fetch, bus);
        self.regs.pc = instr_pc.wrapping_add(2);
        // Pre-decoded table lookup (bit-identical to `decode(word)`; see
        // `decode_lut`) — `word` indexes the full u16 space, always in bounds.
        let op = self.decode_lut[word as usize];

        // ---- Pre-dispatch interlocks ----
        let mut interlock_stall = 0u32;
        if let Some(loaded) = self.load_dest_pending.take()
            && op.reads_reg(loaded)
        {
            interlock_stall += 1;
        }
        if op.reads_mac() {
            interlock_stall += self.pipeline.stall_for_mac();
        }

        let was_pending = self.pending_branch.is_some();
        self.in_delay_slot = was_pending;

        // ---- Slot-illegal instruction ----
        // A delay-slot containing a branch / SR-mutating op / PC-fetching
        // op raises vector 6. The pushed PC is the *branch* address
        // (instr_pc - 2), so RTE restarts the branch with consistent state.
        if was_pending && op.is_illegal_in_slot() {
            self.pending_branch = None;
            self.in_delay_slot = false;
            self.regs.pc = instr_pc; // un-advance: re-fetch the slot on return
            let cost = self.take_exception(6, None, bus);
            self.pipeline.advance(interlock_stall + fetch_stall + cost);
            return interlock_stall + fetch_stall + cost;
        }

        // ---- General illegal instruction ----
        if matches!(op, Op::Illegal(_)) {
            self.last_illegal_word = Some(word);
            self.regs.pc = instr_pc; // RTE returns to the offending op
            let cost = self.take_exception(4, None, bus);
            self.pipeline.advance(interlock_stall + fetch_stall + cost);
            return interlock_stall + fetch_stall + cost;
        }

        let exec_cycles = self.execute(op, instr_pc, bus);

        if was_pending && let Some(target) = self.pending_branch.take() {
            self.regs.pc = target;
        }
        self.in_delay_slot = false;

        // ---- Post-dispatch scoreboard updates ----
        if let Some(rn) = op.load_dest() {
            self.load_dest_pending = Some(rn);
        }
        if let Some(lat) = op.multiply_latency() {
            self.pipeline
                .schedule_mac(lat + exec_cycles + interlock_stall);
        }

        let total = interlock_stall + fetch_stall + exec_cycles;
        self.pipeline.advance(total);
        total
    }

    /// Push SR then PC on the stack and vector through `VBR + vector*4`.
    /// Returns the bus-stall cycles incurred; the caller adds the fixed
    /// 5-cycle exception overhead. If `set_imask` is `Some(lvl)` the SR
    /// interrupt mask is raised to it after the push (interrupt entry).
    fn take_exception(&mut self, vector: u8, set_imask: Option<u8>, bus: &mut impl Bus) -> u32 {
        // Debug: record CPU exceptions (set_imask None) — not interrupts.
        if set_imask.is_none() {
            self.last_fault = Some((vector, self.regs.pc));
        }
        let mut sp = self.regs.r[15];
        sp = sp.wrapping_sub(4);
        let s1 = self.mem_write32(sp, self.regs.sr.0, AccessKind::Data, bus);
        sp = sp.wrapping_sub(4);
        let s2 = self.mem_write32(sp, self.regs.pc, AccessKind::Data, bus);
        self.regs.r[15] = sp;
        if let Some(lvl) = set_imask {
            self.regs.sr.set_imask(lvl);
        }
        let vec_addr = self.regs.vbr.wrapping_add((vector as u32) << 2);
        let (target, s3) = self.mem_read32(vec_addr, AccessKind::Data, bus);
        self.regs.pc = target;
        // Reset interlocks; the handler executes from a fresh pipeline
        // state, just like after a branch.
        self.pending_branch = None;
        self.load_dest_pending = None;
        5 + s1 + s2 + s3
    }

    /// PC base for PC-relative addressing (`MOV.W/L @(disp,PC)`, `MOVA`):
    /// normally the instruction's own address + 4 (the SH-2 pipeline PC). In
    /// the delay slot of a *taken* branch the hardware has already redirected
    /// PC, so the base is the branch destination + 2 (SH-2 manual, MOVA note:
    /// "PC = branch destination + 2"; Mednafen `UCDelayBranch` sets
    /// `PC = target` before the slot executes). A not-taken conditional's
    /// slot has no pending branch and uses the normal base.
    fn pcrel_base(&self, instr_pc: u32) -> u32 {
        match (self.in_delay_slot, self.pending_branch) {
            (true, Some(target)) => target.wrapping_add(2),
            _ => instr_pc.wrapping_add(4),
        }
    }

    fn execute(&mut self, op: Op, instr_pc: u32, bus: &mut impl Bus) -> u32 {
        use Op::*;
        match op {
            // ============================================================
            // System control
            // ============================================================
            Nop => 1,
            Clrt => {
                self.regs.sr.set_t(false);
                1
            }
            Sett => {
                self.regs.sr.set_t(true);
                1
            }
            Clrmac => {
                self.regs.mach = 0;
                self.regs.macl = 0;
                1
            }
            // SLEEP halts the CPU until an interrupt or NMI. For M1 we model
            // it as a 3-cycle NOP — wake-up plumbing arrives with task #7.
            Sleep => 3,

            // ============================================================
            // Data transfer
            // ============================================================
            MovI { rn, imm } => {
                self.regs.r[rn as usize] = imm as i32 as u32;
                1
            }
            MovWPcRel { rn, disp } => {
                let addr = self.pcrel_base(instr_pc).wrapping_add((disp as u32) << 1);
                let (val, s) = self.mem_read16(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i16 as i32 as u32;
                1 + s
            }
            MovLPcRel { rn, disp } => {
                let addr = (self.pcrel_base(instr_pc) & !3).wrapping_add((disp as u32) << 2);
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val;
                1 + s
            }
            MovRR { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize];
                1
            }

            MovBS { rn, rm } => {
                let addr = self.regs.r[rn as usize];
                let val = self.regs.r[rm as usize] as u8;
                let s = self.mem_write8(addr, val, AccessKind::Data, bus);
                1 + s
            }
            MovWS { rn, rm } => {
                let addr = self.regs.r[rn as usize];
                let val = self.regs.r[rm as usize] as u16;
                let s = self.mem_write16(addr, val, AccessKind::Data, bus);
                1 + s
            }
            MovLS { rn, rm } => {
                let addr = self.regs.r[rn as usize];
                let val = self.regs.r[rm as usize];
                let s = self.mem_write32(addr, val, AccessKind::Data, bus);
                1 + s
            }
            MovBL { rn, rm } => {
                let (val, s) = self.mem_read8(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i8 as i32 as u32;
                1 + s
            }
            MovWL { rn, rm } => {
                let (val, s) = self.mem_read16(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i16 as i32 as u32;
                1 + s
            }
            MovLL { rn, rm } => {
                let (val, s) = self.mem_read32(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val;
                1 + s
            }
            MovBM { rn, rm } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(1);
                let s =
                    self.mem_write8(addr, self.regs.r[rm as usize] as u8, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            MovWM { rn, rm } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(2);
                let s =
                    self.mem_write16(addr, self.regs.r[rm as usize] as u16, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            MovLM { rn, rm } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            MovBP { rn, rm } => {
                let (val, s) = self.mem_read8(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i8 as i32 as u32;
                if rn != rm {
                    self.regs.r[rm as usize] = self.regs.r[rm as usize].wrapping_add(1);
                }
                1 + s
            }
            MovWP { rn, rm } => {
                let (val, s) = self.mem_read16(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i16 as i32 as u32;
                if rn != rm {
                    self.regs.r[rm as usize] = self.regs.r[rm as usize].wrapping_add(2);
                }
                1 + s
            }
            MovLP { rn, rm } => {
                let (val, s) = self.mem_read32(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val;
                if rn != rm {
                    self.regs.r[rm as usize] = self.regs.r[rm as usize].wrapping_add(4);
                }
                1 + s
            }

            MovBS0 { rn, disp } => {
                let addr = self.regs.r[rn as usize].wrapping_add(disp as u32);
                let s = self.mem_write8(addr, self.regs.r[0] as u8, AccessKind::Data, bus);
                1 + s
            }
            MovWS0 { rn, disp } => {
                let addr = self.regs.r[rn as usize].wrapping_add((disp as u32) << 1);
                let s = self.mem_write16(addr, self.regs.r[0] as u16, AccessKind::Data, bus);
                1 + s
            }
            MovLS4 { rn, rm, disp } => {
                let addr = self.regs.r[rn as usize].wrapping_add((disp as u32) << 2);
                let s = self.mem_write32(addr, self.regs.r[rm as usize], AccessKind::Data, bus);
                1 + s
            }
            MovBL0 { rm, disp } => {
                let addr = self.regs.r[rm as usize].wrapping_add(disp as u32);
                let (val, s) = self.mem_read8(addr, AccessKind::Data, bus);
                self.regs.r[0] = val as i8 as i32 as u32;
                1 + s
            }
            MovWL0 { rm, disp } => {
                let addr = self.regs.r[rm as usize].wrapping_add((disp as u32) << 1);
                let (val, s) = self.mem_read16(addr, AccessKind::Data, bus);
                self.regs.r[0] = val as i16 as i32 as u32;
                1 + s
            }
            MovLL4 { rn, rm, disp } => {
                let addr = self.regs.r[rm as usize].wrapping_add((disp as u32) << 2);
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val;
                1 + s
            }

            MovBSX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rn as usize]);
                let s =
                    self.mem_write8(addr, self.regs.r[rm as usize] as u8, AccessKind::Data, bus);
                1 + s
            }
            MovWSX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rn as usize]);
                let s =
                    self.mem_write16(addr, self.regs.r[rm as usize] as u16, AccessKind::Data, bus);
                1 + s
            }
            MovLSX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rn as usize]);
                let s = self.mem_write32(addr, self.regs.r[rm as usize], AccessKind::Data, bus);
                1 + s
            }
            MovBLX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rm as usize]);
                let (val, s) = self.mem_read8(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i8 as i32 as u32;
                1 + s
            }
            MovWLX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rm as usize]);
                let (val, s) = self.mem_read16(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i16 as i32 as u32;
                1 + s
            }
            MovLLX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rm as usize]);
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val;
                1 + s
            }

            MovBSG { disp } => {
                let addr = self.regs.gbr.wrapping_add(disp as u32);
                let s = self.mem_write8(addr, self.regs.r[0] as u8, AccessKind::Data, bus);
                1 + s
            }
            MovWSG { disp } => {
                let addr = self.regs.gbr.wrapping_add((disp as u32) << 1);
                let s = self.mem_write16(addr, self.regs.r[0] as u16, AccessKind::Data, bus);
                1 + s
            }
            MovLSG { disp } => {
                let addr = self.regs.gbr.wrapping_add((disp as u32) << 2);
                let s = self.mem_write32(addr, self.regs.r[0], AccessKind::Data, bus);
                1 + s
            }
            MovBLG { disp } => {
                let addr = self.regs.gbr.wrapping_add(disp as u32);
                let (val, s) = self.mem_read8(addr, AccessKind::Data, bus);
                self.regs.r[0] = val as i8 as i32 as u32;
                1 + s
            }
            MovWLG { disp } => {
                let addr = self.regs.gbr.wrapping_add((disp as u32) << 1);
                let (val, s) = self.mem_read16(addr, AccessKind::Data, bus);
                self.regs.r[0] = val as i16 as i32 as u32;
                1 + s
            }
            MovLLG { disp } => {
                let addr = self.regs.gbr.wrapping_add((disp as u32) << 2);
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[0] = val;
                1 + s
            }

            Mova { disp } => {
                self.regs.r[0] = (self.pcrel_base(instr_pc) & !3).wrapping_add((disp as u32) << 2);
                1
            }
            Movt { rn } => {
                self.regs.r[rn as usize] = self.regs.sr.t() as u32;
                1
            }
            SwapB { rn, rm } => {
                let m = self.regs.r[rm as usize];
                self.regs.r[rn as usize] =
                    (m & 0xFFFF_0000) | ((m & 0xFF) << 8) | ((m & 0xFF00) >> 8);
                1
            }
            SwapW { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize].rotate_left(16);
                1
            }
            Xtrct { rn, rm } => {
                let m = self.regs.r[rm as usize];
                let n = self.regs.r[rn as usize];
                self.regs.r[rn as usize] = ((m & 0xFFFF) << 16) | ((n >> 16) & 0xFFFF);
                1
            }

            // ============================================================
            // Arithmetic
            // ============================================================
            Add { rn, rm } => {
                self.regs.r[rn as usize] =
                    self.regs.r[rn as usize].wrapping_add(self.regs.r[rm as usize]);
                1
            }
            AddI { rn, imm } => {
                self.regs.r[rn as usize] = self.regs.r[rn as usize].wrapping_add(imm as i32 as u32);
                1
            }
            Addc { rn, rm } => {
                let t_in = self.regs.sr.t() as u32;
                let (s1, c1) = self.regs.r[rn as usize].overflowing_add(self.regs.r[rm as usize]);
                let (s2, c2) = s1.overflowing_add(t_in);
                self.regs.r[rn as usize] = s2;
                self.regs.sr.set_t(c1 || c2);
                1
            }
            Addv { rn, rm } => {
                let (s, ov) = (self.regs.r[rn as usize] as i32)
                    .overflowing_add(self.regs.r[rm as usize] as i32);
                self.regs.r[rn as usize] = s as u32;
                self.regs.sr.set_t(ov);
                1
            }
            Sub { rn, rm } => {
                self.regs.r[rn as usize] =
                    self.regs.r[rn as usize].wrapping_sub(self.regs.r[rm as usize]);
                1
            }
            Subc { rn, rm } => {
                let t_in = self.regs.sr.t() as u32;
                let (s1, b1) = self.regs.r[rn as usize].overflowing_sub(self.regs.r[rm as usize]);
                let (s2, b2) = s1.overflowing_sub(t_in);
                self.regs.r[rn as usize] = s2;
                self.regs.sr.set_t(b1 || b2);
                1
            }
            Subv { rn, rm } => {
                let (s, ov) = (self.regs.r[rn as usize] as i32)
                    .overflowing_sub(self.regs.r[rm as usize] as i32);
                self.regs.r[rn as usize] = s as u32;
                self.regs.sr.set_t(ov);
                1
            }
            Neg { rn, rm } => {
                self.regs.r[rn as usize] = 0u32.wrapping_sub(self.regs.r[rm as usize]);
                1
            }
            Negc { rn, rm } => {
                let t_in = self.regs.sr.t() as u32;
                let (s1, b1) = 0u32.overflowing_sub(self.regs.r[rm as usize]);
                let (s2, b2) = s1.overflowing_sub(t_in);
                self.regs.r[rn as usize] = s2;
                self.regs.sr.set_t(b1 || b2);
                1
            }
            Dt { rn } => {
                let v = self.regs.r[rn as usize].wrapping_sub(1);
                self.regs.r[rn as usize] = v;
                self.regs.sr.set_t(v == 0);
                1
            }

            CmpEqI { imm } => {
                self.regs.sr.set_t(self.regs.r[0] == (imm as i32 as u32));
                1
            }
            CmpEq { rn, rm } => {
                self.regs
                    .sr
                    .set_t(self.regs.r[rn as usize] == self.regs.r[rm as usize]);
                1
            }
            CmpHs { rn, rm } => {
                self.regs
                    .sr
                    .set_t(self.regs.r[rn as usize] >= self.regs.r[rm as usize]);
                1
            }
            CmpGe { rn, rm } => {
                self.regs
                    .sr
                    .set_t((self.regs.r[rn as usize] as i32) >= (self.regs.r[rm as usize] as i32));
                1
            }
            CmpHi { rn, rm } => {
                self.regs
                    .sr
                    .set_t(self.regs.r[rn as usize] > self.regs.r[rm as usize]);
                1
            }
            CmpGt { rn, rm } => {
                self.regs
                    .sr
                    .set_t((self.regs.r[rn as usize] as i32) > (self.regs.r[rm as usize] as i32));
                1
            }
            CmpPl { rn } => {
                self.regs.sr.set_t((self.regs.r[rn as usize] as i32) > 0);
                1
            }
            CmpPz { rn } => {
                self.regs.sr.set_t((self.regs.r[rn as usize] as i32) >= 0);
                1
            }
            CmpStr { rn, rm } => {
                // T = any byte of (Rn ^ Rm) is zero — i.e. any matching byte.
                let x = self.regs.r[rn as usize] ^ self.regs.r[rm as usize];
                let any_zero_byte = (x & 0xFF00_0000) == 0
                    || (x & 0x00FF_0000) == 0
                    || (x & 0x0000_FF00) == 0
                    || (x & 0x0000_00FF) == 0;
                self.regs.sr.set_t(any_zero_byte);
                1
            }

            ExtsB { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize] as i8 as i32 as u32;
                1
            }
            ExtsW { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize] as i16 as i32 as u32;
                1
            }
            ExtuB { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize] & 0xFF;
                1
            }
            ExtuW { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize] & 0xFFFF;
                1
            }

            // ---- Multiplies ----
            MulL { rn, rm } => {
                self.regs.macl = self.regs.r[rn as usize].wrapping_mul(self.regs.r[rm as usize]);
                // Manual: 2–4 cycles. Use 2 as base; pipeline interlock model
                // (task #5) will refine this with MAC-ready scoreboard.
                2
            }
            MulsW { rn, rm } => {
                let m = self.regs.r[rm as usize] as i16 as i32;
                let n = self.regs.r[rn as usize] as i16 as i32;
                self.regs.macl = (m.wrapping_mul(n)) as u32;
                1
            }
            MuluW { rn, rm } => {
                let m = (self.regs.r[rm as usize] as u16) as u32;
                let n = (self.regs.r[rn as usize] as u16) as u32;
                self.regs.macl = m.wrapping_mul(n);
                1
            }
            DmulsL { rn, rm } => {
                let prod = (self.regs.r[rn as usize] as i32 as i64)
                    .wrapping_mul(self.regs.r[rm as usize] as i32 as i64);
                self.regs.macl = prod as u32;
                self.regs.mach = (prod >> 32) as u32;
                2
            }
            DmuluL { rn, rm } => {
                let prod =
                    (self.regs.r[rn as usize] as u64).wrapping_mul(self.regs.r[rm as usize] as u64);
                self.regs.macl = prod as u32;
                self.regs.mach = (prod >> 32) as u32;
                2
            }
            MacL { rn, rm } => self.exec_mac_l(rn, rm, bus),
            MacW { rn, rm } => self.exec_mac_w(rn, rm, bus),

            // ---- Division ----
            Div0u => {
                self.regs.sr.set_m(false);
                self.regs.sr.set_q(false);
                self.regs.sr.set_t(false);
                1
            }
            Div0s { rn, rm } => {
                let m = (self.regs.r[rm as usize] as i32) < 0;
                let q = (self.regs.r[rn as usize] as i32) < 0;
                self.regs.sr.set_m(m);
                self.regs.sr.set_q(q);
                self.regs.sr.set_t(m != q);
                1
            }
            Div1 { rn, rm } => {
                self.exec_div1(rn, rm);
                1
            }

            // ============================================================
            // Logical
            // ============================================================
            And { rn, rm } => {
                self.regs.r[rn as usize] &= self.regs.r[rm as usize];
                1
            }
            AndI { imm } => {
                self.regs.r[0] &= imm as u32;
                1
            }
            AndBG { imm } => self.exec_logical_bg(imm, bus, |a, b| a & b),
            Or { rn, rm } => {
                self.regs.r[rn as usize] |= self.regs.r[rm as usize];
                1
            }
            OrI { imm } => {
                self.regs.r[0] |= imm as u32;
                1
            }
            OrBG { imm } => self.exec_logical_bg(imm, bus, |a, b| a | b),
            Xor { rn, rm } => {
                self.regs.r[rn as usize] ^= self.regs.r[rm as usize];
                1
            }
            XorI { imm } => {
                self.regs.r[0] ^= imm as u32;
                1
            }
            XorBG { imm } => self.exec_logical_bg(imm, bus, |a, b| a ^ b),
            Not { rn, rm } => {
                self.regs.r[rn as usize] = !self.regs.r[rm as usize];
                1
            }
            Tst { rn, rm } => {
                self.regs
                    .sr
                    .set_t((self.regs.r[rn as usize] & self.regs.r[rm as usize]) == 0);
                1
            }
            TstI { imm } => {
                self.regs.sr.set_t((self.regs.r[0] & imm as u32) == 0);
                1
            }
            TstBG { imm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.gbr);
                let (val, s) = self.mem_read8(addr, AccessKind::Data, bus);
                self.regs.sr.set_t((val & imm) == 0);
                3 + s
            }
            Tas { rn } => {
                let addr = self.regs.r[rn as usize];
                let (val, sr) = self.mem_read8(addr, AccessKind::Data, bus);
                self.regs.sr.set_t(val == 0);
                let sw = self.mem_write8(addr, val | 0x80, AccessKind::Data, bus);
                4 + sr + sw
            }

            // ============================================================
            // Shifts / rotates
            // ============================================================
            Shll { rn } | Shal { rn } => {
                let v = self.regs.r[rn as usize];
                self.regs.sr.set_t(v & 0x8000_0000 != 0);
                self.regs.r[rn as usize] = v << 1;
                1
            }
            Shlr { rn } => {
                let v = self.regs.r[rn as usize];
                self.regs.sr.set_t(v & 1 != 0);
                self.regs.r[rn as usize] = v >> 1;
                1
            }
            Shar { rn } => {
                let v = self.regs.r[rn as usize];
                self.regs.sr.set_t(v & 1 != 0);
                self.regs.r[rn as usize] = ((v as i32) >> 1) as u32;
                1
            }
            Rotl { rn } => {
                let v = self.regs.r[rn as usize];
                self.regs.sr.set_t(v & 0x8000_0000 != 0);
                self.regs.r[rn as usize] = v.rotate_left(1);
                1
            }
            Rotr { rn } => {
                let v = self.regs.r[rn as usize];
                self.regs.sr.set_t(v & 1 != 0);
                self.regs.r[rn as usize] = v.rotate_right(1);
                1
            }
            Rotcl { rn } => {
                let v = self.regs.r[rn as usize];
                let new_msb = v & 0x8000_0000 != 0;
                self.regs.r[rn as usize] = (v << 1) | (self.regs.sr.t() as u32);
                self.regs.sr.set_t(new_msb);
                1
            }
            Rotcr { rn } => {
                let v = self.regs.r[rn as usize];
                let new_lsb = v & 1 != 0;
                self.regs.r[rn as usize] = ((self.regs.sr.t() as u32) << 31) | (v >> 1);
                self.regs.sr.set_t(new_lsb);
                1
            }
            Shll2 { rn } => {
                self.regs.r[rn as usize] <<= 2;
                1
            }
            Shlr2 { rn } => {
                self.regs.r[rn as usize] >>= 2;
                1
            }
            Shll8 { rn } => {
                self.regs.r[rn as usize] <<= 8;
                1
            }
            Shlr8 { rn } => {
                self.regs.r[rn as usize] >>= 8;
                1
            }
            Shll16 { rn } => {
                self.regs.r[rn as usize] <<= 16;
                1
            }
            Shlr16 { rn } => {
                self.regs.r[rn as usize] >>= 16;
                1
            }

            // ============================================================
            // Branches
            // ============================================================
            Bra { disp } => {
                self.pending_branch = Some(
                    instr_pc
                        .wrapping_add(4)
                        .wrapping_add(((disp as i32) << 1) as u32),
                );
                2
            }
            Bsr { disp } => {
                self.regs.pr = instr_pc.wrapping_add(4);
                self.pending_branch = Some(
                    instr_pc
                        .wrapping_add(4)
                        .wrapping_add(((disp as i32) << 1) as u32),
                );
                2
            }
            Braf { rm } => {
                self.pending_branch = Some(
                    instr_pc
                        .wrapping_add(4)
                        .wrapping_add(self.regs.r[rm as usize]),
                );
                2
            }
            Bsrf { rm } => {
                self.regs.pr = instr_pc.wrapping_add(4);
                self.pending_branch = Some(
                    instr_pc
                        .wrapping_add(4)
                        .wrapping_add(self.regs.r[rm as usize]),
                );
                2
            }
            Jmp { rm } => {
                self.pending_branch = Some(self.regs.r[rm as usize]);
                2
            }
            Jsr { rm } => {
                self.regs.pr = instr_pc.wrapping_add(4);
                self.pending_branch = Some(self.regs.r[rm as usize]);
                2
            }
            Rts => {
                self.pending_branch = Some(self.regs.pr);
                2
            }
            Bt { disp } => {
                if self.regs.sr.t() {
                    self.regs.pc = instr_pc
                        .wrapping_add(4)
                        .wrapping_add(((disp as i32) << 1) as u32);
                    3
                } else {
                    1
                }
            }
            Bf { disp } => {
                if !self.regs.sr.t() {
                    self.regs.pc = instr_pc
                        .wrapping_add(4)
                        .wrapping_add(((disp as i32) << 1) as u32);
                    3
                } else {
                    1
                }
            }
            BtS { disp } => {
                if self.regs.sr.t() {
                    self.pending_branch = Some(
                        instr_pc
                            .wrapping_add(4)
                            .wrapping_add(((disp as i32) << 1) as u32),
                    );
                    2
                } else {
                    1
                }
            }
            BfS { disp } => {
                if !self.regs.sr.t() {
                    self.pending_branch = Some(
                        instr_pc
                            .wrapping_add(4)
                            .wrapping_add(((disp as i32) << 1) as u32),
                    );
                    2
                } else {
                    1
                }
            }

            // ============================================================
            // Control-register transfer
            // ============================================================
            LdcSr { rm } => {
                self.regs.sr.0 = self.regs.r[rm as usize] & Sr::WRITE_MASK;
                1
            }
            LdcGbr { rm } => {
                self.regs.gbr = self.regs.r[rm as usize];
                1
            }
            LdcVbr { rm } => {
                self.regs.vbr = self.regs.r[rm as usize];
                1
            }
            LdcLSr { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.sr.0 = val & Sr::WRITE_MASK;
                3 + s
            }
            LdcLGbr { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.gbr = val;
                3 + s
            }
            LdcLVbr { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.vbr = val;
                3 + s
            }
            StcSr { rn } => {
                self.regs.r[rn as usize] = self.regs.sr.0;
                1
            }
            StcGbr { rn } => {
                self.regs.r[rn as usize] = self.regs.gbr;
                1
            }
            StcVbr { rn } => {
                self.regs.r[rn as usize] = self.regs.vbr;
                1
            }
            StcLSr { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.sr.0, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                2 + s
            }
            StcLGbr { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.gbr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                2 + s
            }
            StcLVbr { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.vbr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                2 + s
            }
            LdsMach { rm } => {
                self.regs.mach = self.regs.r[rm as usize];
                1
            }
            LdsMacl { rm } => {
                self.regs.macl = self.regs.r[rm as usize];
                1
            }
            LdsPr { rm } => {
                self.regs.pr = self.regs.r[rm as usize];
                1
            }
            LdsLMach { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.mach = val;
                1 + s
            }
            LdsLMacl { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.macl = val;
                1 + s
            }
            LdsLPr { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.pr = val;
                1 + s
            }
            StsMach { rn } => {
                self.regs.r[rn as usize] = self.regs.mach;
                1
            }
            StsMacl { rn } => {
                self.regs.r[rn as usize] = self.regs.macl;
                1
            }
            StsPr { rn } => {
                self.regs.r[rn as usize] = self.regs.pr;
                1
            }
            StsLMach { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.mach, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            StsLMacl { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.macl, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            StsLPr { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.pr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }

            // ============================================================
            // Exception primitives
            // ============================================================
            Trapa { imm } => {
                // self.regs.pc is already instr_pc + 2 (advanced in step()),
                // so take_exception pushes the correct resume address.
                let cost = self.take_exception(imm, None, bus);
                3 + cost
            }
            Rte => {
                // Pop PC then SR. RTE has a delay slot; the slot's PC is the
                // instruction after RTE, but the architecture says the slot
                // executes *before* the popped PC takes effect, so we mirror
                // the branch path: set pending_branch to the popped PC.
                let sp = self.regs.r[15];
                let (new_pc, s1) = self.mem_read32(sp, AccessKind::Data, bus);
                let (new_sr, s2) = self.mem_read32(sp.wrapping_add(4), AccessKind::Data, bus);
                self.regs.r[15] = sp.wrapping_add(8);
                self.regs.sr.0 = new_sr & Sr::WRITE_MASK;
                self.pending_branch = Some(new_pc);
                4 + s1 + s2
            }

            Illegal(_) => {
                // Intercepted in step() before reaching execute(): vector 4
                // is taken immediately. Reaching this arm means the
                // step-level guard regressed.
                unreachable!("Op::Illegal must be handled in step(), not execute()");
            }
        }
    }

    // ----------------------------------------------------------------------
    // Memory routing
    // ----------------------------------------------------------------------
    //
    // Every CPU memory access goes through these helpers. SH7604 address
    // space is partitioned by the top 3 bits:
    //
    //   0x00000000..0x1FFFFFFF  cached area      → probe cache, miss-fills line
    //   0x20000000..0x3FFFFFFF  cache-through    → bypass cache, present masked addr
    //   0xFFFFFE00..0xFFFFFFFF  on-chip peripherals (intercepted before bus)
    //   anything else           pass through to bus untouched (control regions)
    //
    // The cache uses the masked physical address (low 29 bits) for tag
    // matching so a cached and cache-through access to the same physical
    // memory see the same line storage.

    /// Materialize the lazy FRT/WDT counters up to the current cycle before a
    /// timer-register access (Stage B). The event-scheduled timer only advances
    /// FRC/WTCNT at its `next_ts` edge otherwise, so a register read/write must
    /// catch them up to the access cycle (Mednafen calls `FRT_WDT_Update` at the
    /// top of every FRT/WDT MMIO handler). No-op for non-timer on-chip registers.
    #[inline]
    fn timer_sync_pre(&mut self, addr: u32) {
        if OnChip::owns_timer(addr) {
            self.onchip.frt_wdt_update(self.pipeline.cycles);
        }
    }

    /// Recompute the next FRT/WDT event after a timer-register access — a write
    /// may have changed a counter / OCR / prescaler / enable, moving the edge
    /// (Mednafen `FRT_WDT_Recalc_NET`). No-op otherwise.
    #[inline]
    fn timer_sync_post(&mut self, addr: u32) {
        if OnChip::owns_timer(addr) {
            self.onchip.frt_wdt_recalc_net(self.pipeline.cycles);
        }
    }

    #[inline]
    pub(crate) fn mem_read8(
        &mut self,
        addr: u32,
        kind: AccessKind,
        bus: &mut impl Bus,
    ) -> (u8, u32) {
        if addr == CCR_ADDR {
            return (self.cache.ccr(), 0);
        }
        if OnChip::owns(addr) {
            let stall = if is_divu_reg(addr) { self.pipeline.stall_for_divide() } else { 0 };
            self.timer_sync_pre(addr);
            let v = self.onchip.read8(addr);
            self.timer_sync_post(addr);
            if self.dbg_ftcsr && addr == FTCSR_ADDR && self.dbg_ftcsr_log.len() < 16384 {
                self.dbg_ftcsr_log.push((self.regs.pc, v, false, self.pipeline.cycles));
            }
            return (v, stall);
        }
        if is_assoc_purge(addr) {
            self.cache.assoc_purge(addr);
            return (!0, 0);
        }
        let (phys, cacheable) = classify(addr);
        if cacheable {
            match self.cache.probe(phys, matches!(kind, AccessKind::Fetch)) {
                cache::Probe::Hit(set, way) => {
                    self.note_watched_read(phys, kind, 1);
                    let v = cache::extract_u8(self.cache.line_at(set, way), phys);
                    // peek16 the containing halfword, extract the byte.
                    if self.dbg_detect_stale
                        && self.dbg_stale_fetch.is_none()
                        && let Some(hw) = bus.peek16(phys & !1)
                    {
                        self.dbg_stale_checks += 1;
                        let mem = (hw >> (if phys & 1 == 0 { 8 } else { 0 })) as u8;
                        if mem != v {
                            self.dbg_stale_fetch =
                                Some((phys, v as u16, mem as u16, self.pipeline.cycles));
                        }
                    }
                    return (v, 0);
                }
                cache::Probe::Miss => {
                    self.note_watched_read(phys, kind, 2);
                    let (line, stall) = self.fill_line(phys, kind, bus);
                    return (cache::extract_u8(&line, phys), stall);
                }
                cache::Probe::Bypass => self.note_watched_read(phys, kind, 3),
            }
        } else {
            self.note_watched_read(phys, kind, 0);
        }
        bus.read8(phys, kind)
    }

    #[inline]
    pub(crate) fn mem_read16(
        &mut self,
        addr: u32,
        kind: AccessKind,
        bus: &mut impl Bus,
    ) -> (u16, u32) {
        if OnChip::owns(addr) {
            let stall = if is_divu_reg(addr) { self.pipeline.stall_for_divide() } else { 0 };
            self.timer_sync_pre(addr);
            let v = self.onchip.read16(addr);
            self.timer_sync_post(addr);
            return (v, stall);
        }
        if is_assoc_purge(addr) {
            self.cache.assoc_purge(addr);
            return (!0, 0);
        }
        let (phys, cacheable) = classify(addr);
        if cacheable {
            match self.cache.probe(phys, matches!(kind, AccessKind::Fetch)) {
                cache::Probe::Hit(set, way) => {
                    self.note_watched_read(phys, kind, 1);
                    let v = cache::extract_u16(self.cache.line_at(set, way), phys);
                    // Compare the cache-hit (fetch *or* data) against true
                    // memory: a mismatch is a stale cache line (the
                    // coherency-hang smoking gun).
                    if self.dbg_detect_stale
                        && self.dbg_stale_fetch.is_none()
                        && let Some(mem) = bus.peek16(phys)
                    {
                        self.dbg_stale_checks += 1;
                        if mem != v {
                            self.dbg_stale_fetch = Some((phys, v, mem, self.pipeline.cycles));
                        }
                    }
                    let extra = if matches!(kind, AccessKind::Fetch) { self.dbg_slow_fetch } else { 0 };
                    return (v, extra);
                }
                cache::Probe::Miss => {
                    self.note_watched_read(phys, kind, 2);
                    let (line, stall) = self.fill_line(phys, kind, bus);
                    return (cache::extract_u16(&line, phys), stall);
                }
                cache::Probe::Bypass => self.note_watched_read(phys, kind, 3),
            }
        } else {
            self.note_watched_read(phys, kind, 0);
        }
        bus.read16(phys, kind)
    }

    #[inline]
    pub(crate) fn mem_read32(
        &mut self,
        addr: u32,
        kind: AccessKind,
        bus: &mut impl Bus,
    ) -> (u32, u32) {
        if OnChip::owns(addr) {
            let stall = if is_divu_reg(addr) { self.pipeline.stall_for_divide() } else { 0 };
            self.timer_sync_pre(addr);
            let v = self.onchip.read32(addr);
            self.timer_sync_post(addr);
            return (v, stall);
        }
        if is_assoc_purge(addr) {
            self.cache.assoc_purge(addr);
            return (!0, 0);
        }
        let (phys, cacheable) = classify(addr);
        if cacheable {
            match self.cache.probe(phys, matches!(kind, AccessKind::Fetch)) {
                cache::Probe::Hit(set, way) => {
                    self.note_watched_read(phys, kind, 1);
                    let v = cache::extract_u32(self.cache.line_at(set, way), phys);
                    if self.dbg_detect_stale
                        && self.dbg_stale_fetch.is_none()
                        && let (Some(hi), Some(lo)) =
                            (bus.peek16(phys), bus.peek16(phys.wrapping_add(2)))
                    {
                        self.dbg_stale_checks += 1;
                        let mem = ((hi as u32) << 16) | lo as u32;
                        if mem != v {
                            self.dbg_stale_fetch =
                                Some((phys, v as u16, mem as u16, self.pipeline.cycles));
                        }
                    }
                    return (v, 0);
                }
                cache::Probe::Miss => {
                    self.note_watched_read(phys, kind, 2);
                    let (line, stall) = self.fill_line(phys, kind, bus);
                    return (cache::extract_u32(&line, phys), stall);
                }
                cache::Probe::Bypass => self.note_watched_read(phys, kind, 3),
            }
        } else {
            self.note_watched_read(phys, kind, 0);
        }
        bus.read32(phys, kind)
    }

    #[inline]
    pub(crate) fn mem_write8(
        &mut self,
        addr: u32,
        val: u8,
        kind: AccessKind,
        bus: &mut impl Bus,
    ) -> u32 {
        if addr == CCR_ADDR {
            self.cache.set_ccr(val);
            return 0;
        }
        if OnChip::owns(addr) {
            self.timer_sync_pre(addr);
            self.onchip.write8(addr, val);
            self.timer_sync_post(addr);
            // Recalc-on-change: any on-chip write may have touched an INTC
            // input (TIER/FTCSR/DVCR/CHCR/IPR…); re-arm now (signature-gated).
            self.onchip.refresh_interrupts();
            if let Some(lat) = self.onchip.divu.take_pending_latency() {
                self.pipeline.schedule_divide(lat);
            }
            if self.dbg_ftcsr && addr == FTCSR_ADDR {
                if self.dbg_ftcsr_log.len() < 16384 {
                    self.dbg_ftcsr_log.push((self.regs.pc, val, true, self.pipeline.cycles));
                }
                // Arm the post-clear PC trace once: the first FTCSR write is the
                // master ack/clear of the slave's round-1 done — capture what it
                // does next (the round-2 streaming decision).
                if self.dbg_pc_log.is_empty() {
                    self.dbg_pc_capture = 4000;
                }
            }
            return 0;
        }
        if is_assoc_purge(addr) {
            self.cache.assoc_purge(addr);
            return 0;
        }
        let (phys, cacheable) = classify(addr);
        if cacheable {
            // Write-through: update the cached line if resident, then
            // always reach the bus.
            self.cache.write_through_u8(phys, val);
        }
        bus.write8(phys, val, kind)
    }

    #[inline]
    pub(crate) fn mem_write16(
        &mut self,
        addr: u32,
        val: u16,
        kind: AccessKind,
        bus: &mut impl Bus,
    ) -> u32 {
        if OnChip::owns(addr) {
            self.timer_sync_pre(addr);
            self.onchip.write16(addr, val);
            self.timer_sync_post(addr);
            self.onchip.refresh_interrupts(); // recalc-on-change (see mem_write8)
            if let Some(lat) = self.onchip.divu.take_pending_latency() {
                self.pipeline.schedule_divide(lat);
            }
            return 0;
        }
        if is_assoc_purge(addr) {
            self.cache.assoc_purge(addr);
            return 0;
        }
        let (phys, cacheable) = classify(addr);
        if cacheable {
            self.cache.write_through_u16(phys, val);
        }
        bus.write16(phys, val, kind)
    }

    #[inline]
    pub(crate) fn mem_write32(
        &mut self,
        addr: u32,
        val: u32,
        kind: AccessKind,
        bus: &mut impl Bus,
    ) -> u32 {
        if OnChip::owns(addr) {
            self.timer_sync_pre(addr);
            self.onchip.write32(addr, val);
            self.timer_sync_post(addr);
            self.onchip.refresh_interrupts(); // recalc-on-change (see mem_write8)
            if let Some(lat) = self.onchip.divu.take_pending_latency() {
                self.pipeline.schedule_divide(lat);
            }
            return 0;
        }
        if is_assoc_purge(addr) {
            self.cache.assoc_purge(addr);
            return 0;
        }
        let (phys, cacheable) = classify(addr);
        if cacheable {
            self.cache.write_through_u32(phys, val);
        }
        bus.write32(phys, val, kind)
    }

    // ---- on-chip DMAC transfer engine ------------------------------------
    //
    // DMA accesses go straight to the external bus (bypassing the cache, as
    // the SH7604 DMAC does), after stripping the SH-2 cache-region bits via
    // `classify`. `AccessKind::Dma` lets the bus account DMA arbitration.

    fn dma_read(&mut self, addr: u32, size: u32, bus: &mut impl Bus) -> u32 {
        let (phys, _) = classify(addr);
        match size {
            0 => bus.read8(phys, AccessKind::Dma).0 as u32,
            1 => bus.read16(phys, AccessKind::Dma).0 as u32,
            _ => bus.read32(phys, AccessKind::Dma).0,
        }
    }
    fn dma_write(&mut self, addr: u32, val: u32, size: u32, bus: &mut impl Bus) {
        let (phys, _) = classify(addr);
        match size {
            0 => {
                bus.write8(phys, val as u8, AccessKind::Dma);
            }
            1 => {
                bus.write16(phys, val as u16, AccessKind::Dma);
            }
            _ => {
                bus.write32(phys, val, AccessKind::Dma);
            }
        }
    }

    /// Run any enabled on-chip DMAC channel to completion. Mirrors the SH7604
    /// DMAC `DMA_DoTransfer` (Mednafen `sh7095.inc`): a channel runs when the
    /// master enable is on with no fault (DMAOR `DME & !NMIF & !AE`, i.e.
    /// `DMAOR & 0x07 == 0x01`) and the channel's CHCR.DE is set with CHCR.TE
    /// clear. It copies TCR units of the CHCR.TS size from SAR to DAR honouring
    /// the SM/DM address modes, writes back the final SAR/DAR + residual TCR,
    /// and latches TE on completion (which raises the channel interrupt when
    /// CHCR.IE is set — surfaced by [`OnChip::refresh_interrupts`]).
    ///
    /// **Request mode (CHCR.AR) is intentionally not gated** — Mednafen's
    /// `DMA_RunCond` runs any enabled channel, modelling auto-request only (the
    /// Saturn wires no DREQ source to the SH-2 DMAC). Synchronous block
    /// transfer; cycle-stealing/burst bus timing is a later refinement, as for
    /// the SCU DMA.
    fn run_dma(&mut self, bus: &mut impl Bus) {
        for ch in 0..2 {
            // Re-checked per channel: a fault (AE) raised by channel 0 clears the
            // master run condition, so channel 1 does not start (DMAOR: DME set
            // AND NMIF/AE fault bits clear — Mednafen DMA_RecalcRunning).
            if !self.onchip.dmac.enabled() {
                return;
            }
            self.run_dma_channel(ch, bus);
        }
    }

    fn run_dma_channel(&mut self, ch: usize, bus: &mut impl Bus) {
        let chcr = self.onchip.dmac.channels[ch].chcr;
        // Run condition (Mednafen DMA_RunCond): DE (bit 0) set, TE (bit 1) clear.
        if chcr & 0b11 != 0b01 {
            return;
        }
        let ts = (chcr >> 10) & 3; // transfer size: 0 byte / 1 word / 2 long / 3 block
        let sm = (chcr >> 12) & 3; // source address mode
        let dm = (chcr >> 14) & 3; // destination address mode

        const AM: u32 = 0x07FF_FFFF; // 27-bit external address mask
        // Per-mode address delta (Mednafen `ainc` table): 0 fixed, 1 +unit,
        // 2/3 −unit (mode 3 is "setting prohibited" but the hardware, like
        // mode 2, decrements). The unit follows the access size.
        let unit: u32 = match ts {
            0 => 1,
            1 => 2,
            _ => 4, // long *and* the per-longword stride of block mode
        };
        let ainc = |mode: u32| -> u32 {
            match mode {
                1 => unit,
                2 | 3 => unit.wrapping_neg(),
                _ => 0,
            }
        };
        let align = match ts {
            0 => 0,
            1 => 1,
            _ => 3,
        };

        // SAR/DAR accumulate at full 32-bit width (masked to 27 bits only at the
        // access), matching Mednafen — they are written back unmasked.
        let mut sar = self.onchip.dmac.channels[ch].sar;
        let mut dar = self.onchip.dmac.channels[ch].dar;
        let mut tcr = self.onchip.dmac.channels[ch].tcr & 0x00FF_FFFF;
        if tcr == 0 {
            tcr = 0x0100_0000; // a TCR of 0 means the maximum count (16M)
        }
        let mut fault = false; // a misaligned access sets AE and aborts the channel

        loop {
            if ts == 3 {
                // 16-byte block: four longwords. A misaligned base sets AE but
                // the current block still completes (Mednafen). The source reads
                // sar, sar+4, sar+8, sar+12 then advances by 16 (SM ignored); the
                // destination is written per-longword, advancing by DM each time.
                if (sar | dar) & 3 != 0 {
                    self.onchip.dmac.dmaor |= 4; // AE
                    fault = true;
                }
                let mut buf = [0u32; 4];
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = self.dma_read(sar.wrapping_add((i as u32) << 2) & AM & !3, 2, bus);
                }
                sar = sar.wrapping_add(0x10);
                for v in buf {
                    self.dma_write(dar & AM & !3, v, 2, bus);
                    dar = dar.wrapping_add(ainc(dm));
                    tcr = tcr.wrapping_sub(1) & 0x00FF_FFFF;
                    if tcr == 0 {
                        break;
                    }
                }
            } else {
                if (sar | dar) & align != 0 {
                    self.onchip.dmac.dmaor |= 4; // AE: misaligned word/long address
                    fault = true;
                }
                let v = self.dma_read(sar & AM & !align, ts, bus);
                self.dma_write(dar & AM & !align, v, ts, bus);
                sar = sar.wrapping_add(ainc(sm));
                dar = dar.wrapping_add(ainc(dm));
                tcr = tcr.wrapping_sub(1) & 0x00FF_FFFF;
            }
            if tcr == 0 || fault {
                break;
            }
        }

        // Completion: TE latches only when the count reaches zero (a fault aborts
        // with TCR still non-zero, leaving TE clear). SAR/DAR/TCR write back.
        if tcr == 0 {
            self.onchip.dmac.channels[ch].chcr |= 0b10; // TE
            // Recalc-on-change (Stage C): TE just latched; if CHCR.IE is set the
            // transfer-end interrupt is now pending. Re-arm the INTC here rather
            // than via a per-instruction refresh.
            self.onchip.refresh_interrupts();
        }
        let c = &mut self.onchip.dmac.channels[ch];
        c.sar = sar;
        c.dar = dar;
        c.tcr = tcr;
    }

    /// Cache line-fill for a confirmed miss (the `mem_read*` hit/bypass cases
    /// are handled inline via [`cache::Cache::probe`]). Fetches the full
    /// 16-byte line aligned to `phys & !0xF` via four sequential `bus.read32`
    /// calls (the SH7604 burst is four 32-bit beats), installs it, and returns
    /// the populated line plus the accumulated bus stall.
    fn fill_line(
        &mut self,
        phys: u32,
        kind: AccessKind,
        bus: &mut impl Bus,
    ) -> ([u8; cache::LINE_BYTES], u32) {
        let base = phys & !0xF;
        let mut line = [0u8; cache::LINE_BYTES];
        let mut stall = 0u32;
        for chunk in 0..4u32 {
            // The fill is one bus burst: the first beat carries the access's
            // own kind (and pays the full first-access cost); the remaining
            // three are `LineFill` continuation beats, which an SDRAM host
            // charges nothing for (SH7604 burst read — Mednafen `BurstHax`).
            let beat_kind = if chunk == 0 { kind } else { AccessKind::LineFill };
            let (val, s) = bus.read32(base + chunk * 4, beat_kind);
            let off = (chunk * 4) as usize;
            line[off..off + 4].copy_from_slice(&val.to_be_bytes());
            stall += s;
        }
        self.cache.install(phys, line);
        (line, stall)
    }

    // ----------------------------------------------------------------------
    // Helpers
    // ----------------------------------------------------------------------

    /// AND.B/OR.B/XOR.B #imm,@(R0,GBR). 3 cycles plus bus stalls.
    fn exec_logical_bg(&mut self, imm: u8, bus: &mut impl Bus, f: fn(u8, u8) -> u8) -> u32 {
        let addr = self.regs.r[0].wrapping_add(self.regs.gbr);
        let (val, sr) = self.mem_read8(addr, AccessKind::Data, bus);
        let sw = self.mem_write8(addr, f(val, imm), AccessKind::Data, bus);
        3 + sr + sw
    }

    /// Non-restoring division step. Faithful port of the SH-2 software
    /// manual algorithm (§6, DIV1). Operates on Rn (dividend high half)
    /// using Rm as the divisor.
    fn exec_div1(&mut self, rn: u8, rm: u8) {
        let old_q = self.regs.sr.q();
        let m = self.regs.sr.m();
        let t_in = self.regs.sr.t();
        let divisor = self.regs.r[rm as usize];

        let new_q = self.regs.r[rn as usize] & 0x8000_0000 != 0;
        let shifted = (self.regs.r[rn as usize] << 1) | (t_in as u32);

        let (result, q) = if !old_q {
            if !m {
                let r = shifted.wrapping_sub(divisor);
                let tmp1 = r > shifted;
                let q = if new_q { !tmp1 } else { tmp1 };
                (r, q)
            } else {
                let r = shifted.wrapping_add(divisor);
                let tmp1 = r < shifted;
                let q = if new_q { tmp1 } else { !tmp1 };
                (r, q)
            }
        } else if !m {
            let r = shifted.wrapping_add(divisor);
            let tmp1 = r < shifted;
            let q = if new_q { !tmp1 } else { tmp1 };
            (r, q)
        } else {
            let r = shifted.wrapping_sub(divisor);
            let tmp1 = r > shifted;
            let q = if new_q { tmp1 } else { !tmp1 };
            (r, q)
        };

        self.regs.r[rn as usize] = result;
        self.regs.sr.set_q(q);
        self.regs.sr.set_t(q == m);
    }

    /// MAC.L @Rm+,@Rn+. Signed 32×32 multiply, 64-bit accumulate into
    /// MACH:MACL. S-bit saturation is implemented to the 48-bit signed
    /// range (per SH7604 manual); rare in practice but exercised by some
    /// DSP-heavy code paths.
    fn exec_mac_l(&mut self, rn: u8, rm: u8, bus: &mut impl Bus) -> u32 {
        let addr_m = self.regs.r[rm as usize];
        let (sm, s1) = self.mem_read32(addr_m, AccessKind::Data, bus);
        self.regs.r[rm as usize] = addr_m.wrapping_add(4);

        let addr_n = self.regs.r[rn as usize];
        let (sn, s2) = self.mem_read32(addr_n, AccessKind::Data, bus);
        self.regs.r[rn as usize] = addr_n.wrapping_add(4);

        let prod = (sm as i32 as i64).wrapping_mul(sn as i32 as i64);
        let acc = ((self.regs.mach as u64) << 32) | (self.regs.macl as u64);
        let sum = (acc as i64).wrapping_add(prod);

        let final_sum = if self.regs.sr.s() {
            const MAX: i64 = (1i64 << 47) - 1;
            const MIN: i64 = -(1i64 << 47);
            sum.clamp(MIN, MAX)
        } else {
            sum
        };
        self.regs.macl = final_sum as u32;
        self.regs.mach = (final_sum >> 32) as u32;
        3 + s1 + s2
    }

    /// MAC.W @Rm+,@Rn+. Signed 16×16 multiply. With S=0 the 32-bit product
    /// is added to MACH:MACL as a 64-bit signed value. With S=1 it
    /// accumulates into MACL only with 32-bit saturation (MACH retains the
    /// "overflow occurred" indicator in bit 0, per the SH7604 manual).
    fn exec_mac_w(&mut self, rn: u8, rm: u8, bus: &mut impl Bus) -> u32 {
        let addr_m = self.regs.r[rm as usize];
        let (sm, s1) = self.mem_read16(addr_m, AccessKind::Data, bus);
        self.regs.r[rm as usize] = addr_m.wrapping_add(2);

        let addr_n = self.regs.r[rn as usize];
        let (sn, s2) = self.mem_read16(addr_n, AccessKind::Data, bus);
        self.regs.r[rn as usize] = addr_n.wrapping_add(2);

        let prod = (sm as i16 as i32).wrapping_mul(sn as i16 as i32);

        if !self.regs.sr.s() {
            let acc = ((self.regs.mach as u64) << 32) | (self.regs.macl as u64);
            let sum = (acc as i64).wrapping_add(prod as i64);
            self.regs.macl = sum as u32;
            self.regs.mach = (sum >> 32) as u32;
        } else {
            let (sum, ov) = (self.regs.macl as i32).overflowing_add(prod);
            if ov {
                // Saturate and set the overflow flag in MACH bit 0.
                self.regs.macl = if prod < 0 {
                    i32::MIN as u32
                } else {
                    i32::MAX as u32
                };
                self.regs.mach |= 1;
            } else {
                self.regs.macl = sum as u32;
            }
        }
        3 + s1 + s2
    }
}
