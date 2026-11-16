//! Event-driven scheduler for Saturn-side chips.
//!
//! Each [`SchedEntity`] reports a `next_deadline()` — the global cycle
//! at which it next wants to run. The [`Scheduler`] always advances the
//! entity with the smallest deadline by one step, and the entity is then
//! free to push its deadline forward by whatever work that step cost.
//! Calling [`Scheduler::run_for`] keeps doing this until the global
//! clock has reached the requested horizon.
//!
//! **Determinism contract**: ties in deadline are broken by entity
//! insertion order, so the same start state and the same call yields
//! the same sequence of steps across runs and across machines. The
//! deterministic-replay test asserts this.
//!
//! The live entities are the master and slave SH-2 cores plus the CD-block
//! periodic timer. The linear scan over entities is O(n) per step; with n=3
//! that's fine. If n ever grows past a handful, swap the scan for a
//! `BinaryHeap` keyed on `(deadline, insertion_order)`.
//!
//! **Saturn stepping (Phase-2 Mednafen alignment).** The live machine no
//! longer steps the two SH-2s by this most-behind rule: [`Saturn::step_cpus`]
//! (`system.rs`) drives them in Mednafen's **master-leads-slave** order — the
//! master executes one instruction, then the slave catches up to the master's
//! timestamp — because the most-behind rule could let the *slave* lead and
//! diverged timing-sensitive inter-CPU handoffs. This generic
//! [`run_for`](Scheduler::run_for) / [`pick_behind`](Scheduler::pick_behind) is
//! retained for the CD-block timer it still steps and for the determinism unit
//! test (`tests/scheduler.rs`); the live SH-2 ordering lives in `step_cpus`.

/// Anything the [`Scheduler`] can run. Implementations carry their own
/// local cycle counter; [`next_deadline`] reports it, and [`step`]
/// advances the entity (and, by side effect, the counter).
pub trait SchedEntity {
    /// External state the entity needs to make progress. For real chips
    /// this is the Saturn bus; for tests it can be `()`.
    type Context;

    /// Global cycle at which this entity is next due to run. Must be
    /// monotonically non-decreasing across [`step`] calls.
    fn next_deadline(&self) -> u64;

    /// Advance one unit of work. Implementations must push their
    /// internal cycle counter forward (so [`next_deadline`] returns a
    /// larger value), otherwise [`Scheduler::run_for`] will loop forever.
    fn step(&mut self, ctx: &mut Self::Context);
}

/// Generic deterministic scheduler: holds a set of [`SchedEntity`]s and steps
/// the most-behind one each turn (ties break by insertion order). The live
/// SH-2 pair is driven by [`crate::system::Saturn::step_cpus`] instead; this
/// still runs the CD-block timer and backs the determinism test.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Scheduler<E: SchedEntity> {
    entities: Vec<E>,
}

impl<E: SchedEntity> Default for Scheduler<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: SchedEntity> Scheduler<E> {
    pub fn new() -> Self {
        Self {
            entities: Vec::new(),
        }
    }

    pub fn add(&mut self, entity: E) -> EntityId {
        let id = EntityId(self.entities.len());
        self.entities.push(entity);
        id
    }

    pub fn entity(&self, id: EntityId) -> &E {
        &self.entities[id.0]
    }
    pub fn entity_mut(&mut self, id: EntityId) -> &mut E {
        &mut self.entities[id.0]
    }
    pub fn entities(&self) -> &[E] {
        &self.entities
    }

    /// Global cycle: the smallest `next_deadline` across all entities.
    /// An empty scheduler is "at" cycle 0.
    pub fn now(&self) -> u64 {
        self.entities
            .iter()
            .map(|e| e.next_deadline())
            .min()
            .unwrap_or(0)
    }

    /// Advance the global clock by at least `cycles` by repeatedly
    /// stepping the most-behind entity. Returns when `now() >= start + cycles`.
    pub fn run_for(&mut self, cycles: u64, ctx: &mut E::Context) {
        if self.entities.is_empty() || cycles == 0 {
            return;
        }
        let target = self.now().saturating_add(cycles);
        while self.now() < target {
            let idx = self.pick_behind();
            let before = self.entities[idx].next_deadline();
            self.entities[idx].step(ctx);
            debug_assert!(
                self.entities[idx].next_deadline() > before,
                "SchedEntity::step must advance next_deadline()",
            );
        }
    }

    /// Index of the most-behind entity, with ties resolved to the lowest
    /// insertion order. Centralised here so the determinism contract
    /// has exactly one definition.
    fn pick_behind(&self) -> usize {
        let mut best = 0;
        let mut best_dl = self.entities[0].next_deadline();
        for (i, e) in self.entities.iter().enumerate().skip(1) {
            let dl = e.next_deadline();
            if dl < best_dl {
                best = i;
                best_dl = dl;
            }
        }
        best
    }
}

/// Opaque handle into a [`Scheduler`] returned by [`Scheduler::add`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct EntityId(pub(crate) usize);

// ---- Concrete entity: SH-2 wrapped for the Saturn bus context. -----------

/// A captured breakpoint hit: the `pc` it fired at (so a caller with several
/// breakpoints armed knows *which* one), R0..R15, PR, GBR, 48 code words at the
/// bp PC, and the probe value — the bus (raw-WRAM) read of the configured
/// [`Sh2Entity::set_bp_probe`] address at the hit cycle (0 if unset). Both the
/// code words and the probe are captured with side-effect-free peeks (see
/// [`crate::SaturnBus::peek32`]): an unpeekable address (a register window)
/// reads 0.
#[derive(Clone, Debug)]
pub struct BpHit {
    pub pc: u32,
    pub regs: [u32; 16],
    pub pr: u32,
    pub gbr: u32,
    pub cycle: u64,
    pub code: Vec<u16>,
    pub probe: u32,
}

/// Schedulable wrapper around an `sh2::Cpu`. The entity's `next_deadline`
/// is the CPU's `pipeline.cycles` when running, or `u64::MAX` when
/// halted — that way the scheduler's "most-behind wins" rule naturally
/// skips a halted CPU (it's "infinitely ahead") without any special-
/// casing in the scheduler. The slave SH-2 lives in this halted state
/// from power-on until SMPC's `SSHON` command releases it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Sh2Entity {
    pub cpu: sh2::Cpu,
    halted: bool,
    /// Debug-only full-speed PC trace (M11 boot-give-up investigation). When
    /// `Some`, every executed instruction's PC is appended (consecutive
    /// duplicates and the BIOS idle poll `0x2B0..0x2B6` filtered) up to a cap,
    /// keeping the most recent window. `#[serde(skip)]` so it never affects
    /// save-state determinism; only set on the master via [`enable_pc_trace`].
    #[serde(skip)]
    pc_trace: Option<std::collections::VecDeque<u32>>,
    /// Half-open PC range `[lo, hi)` that, once recorded, freezes the trace
    /// ring (so the give-up tail isn't evicted by the destination's own loop).
    /// Defaults to the work-RAM region the BIOS gives up into; override via
    /// [`set_trace_freeze`]. `#[serde(skip)]` like the trace itself.
    #[serde(skip)]
    trace_freeze: (u32, u32),
    /// Debug-only full-speed breakpoints: each `(pc, guard)` fires when the CPU
    /// reaches `pc` (and, if `guard` is `Some((idx, val))`, only when
    /// `regs.r[idx] == val` — to stop at a shared routine on the *one* call
    /// carrying a specific argument). The first one reached in a step captures
    /// [`bp_hit`] (R0..R15 + PR + GBR + 96 bytes of code at the PC, so a
    /// transient work-RAM routine can be disassembled — and its caller found via
    /// PR — at the instant it runs). Several may be armed at once.
    #[serde(skip)]
    bps: Vec<(u32, Option<(usize, u32)>)>,
    #[serde(skip)]
    bp_hit: Option<BpHit>,
    /// Optional memory probe: when set, a breakpoint hit also captures the
    /// 32-bit value at this address peeked from the **backing memory** (raw
    /// WRAM/ROM, *not* the CPU cache), with no bus-timing or peripheral side
    /// effects. Lets a debugger compare what a CPU loaded (via its cache /
    /// cache-through) against the true backing memory at the exact bp cycle —
    /// the decisive test for a cache-coherency vs torn-write bug. `#[serde(skip)]`.
    #[serde(skip)]
    bp_probe: Option<u32>,
}

impl Sh2Entity {
    pub fn new(cpu: sh2::Cpu) -> Self {
        Self {
            cpu,
            halted: false,
            pc_trace: None,
            trace_freeze: (0x0602_0000, 0x0605_0000),
            bps: Vec::new(),
            bp_hit: None,
            bp_probe: None,
        }
    }

    /// Begin recording a full-speed PC trace (debug; see [`pc_trace`]).
    pub fn enable_pc_trace(&mut self) {
        self.pc_trace = Some(std::collections::VecDeque::new());
    }

    /// Override the PC range `[lo, hi)` that freezes the trace ring (see
    /// [`trace_freeze`]).
    pub fn set_trace_freeze(&mut self, lo: u32, hi: u32) {
        self.trace_freeze = (lo, hi);
    }

    /// Drain the recorded PC trace (most-recent window), if enabled.
    pub fn take_pc_trace(&mut self) -> Vec<u32> {
        self.pc_trace
            .as_mut()
            .map(|d| d.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn is_halted(&self) -> bool {
        self.halted
    }

    pub fn set_halted(&mut self, halted: bool) {
        self.halted = halted;
    }

    /// Arm a single full-speed breakpoint at `pc` (debug; see [`bps`]),
    /// replacing any previously-armed set. Convenience for the common one-bp
    /// case; use [`set_bps`] to arm several.
    pub fn set_bp(&mut self, pc: u32) {
        self.bps = vec![(pc, None)];
        self.bp_hit = None;
    }

    /// Arm a single register-guarded breakpoint: fires at `pc` only when
    /// `regs.r[idx] == val` (replaces the armed set).
    pub fn set_bp_cond(&mut self, pc: u32, idx: usize, val: u32) {
        self.bps = vec![(pc, Some((idx, val)))];
        self.bp_hit = None;
    }

    /// Arm a *set* of breakpoints `(pc, optional (reg, val) guard)`, replacing
    /// any previously armed. The first one reached captures [`bp_hit`]; the
    /// hit's `pc` field says which fired.
    pub fn set_bps(&mut self, bps: Vec<(u32, Option<(usize, u32)>)>) {
        self.bps = bps;
        self.bp_hit = None;
    }

    /// Set (or clear) the breakpoint memory probe — the 32-bit address whose
    /// bus value is captured alongside the registers on the next bp hit (see
    /// [`bp_probe`]). Independent of [`set_bp`]/[`set_bp_cond`].
    pub fn set_bp_probe(&mut self, addr: Option<u32>) {
        self.bp_probe = addr;
    }

    /// Take the captured (registers, PR, GBR, code-words, probe-value) from a
    /// breakpoint hit, if any. The probe value is the bus (raw-WRAM) read of
    /// [`bp_probe`] at the hit cycle, or 0 if no probe was set.
    pub fn take_bp_hit(&mut self) -> Option<BpHit> {
        self.bp_hit.take()
    }
}

impl SchedEntity for Sh2Entity {
    type Context = crate::SaturnBus;

    fn next_deadline(&self) -> u64 {
        if self.halted {
            u64::MAX
        } else {
            self.cpu.pipeline.cycles
        }
    }

    fn step(&mut self, ctx: &mut Self::Context) {
        if !self.halted {
            // Publish the current global cycle to the bus so time-varying
            // peripheral reads (SMPC SF INTBACK completion) resolve at the
            // exact instruction that reads them.
            ctx.cycle = self.cpu.pipeline.cycles;
            ctx.step_pc = self.cpu.regs.pc;
            self.cpu.step(ctx);
            // First armed breakpoint whose PC (and optional register guard)
            // matches the just-executed instruction. `find_map` returns the
            // matching PC by value, releasing the `self.bps` borrow before the
            // capture (which needs `&mut ctx`).
            let pc = self.cpu.regs.pc;
            let bp = self.bps.iter().find_map(|&(p, guard)| {
                (p == pc && guard.is_none_or(|(idx, val)| self.cpu.regs.r[idx] == val)).then_some(p)
            });
            if let Some(bp) = bp
                && self.bp_hit.is_none()
            {
                // Observer-only: peek (no BusTiming charge, no watchpoint, no
                // FIFO pop), so arming a breakpoint never perturbs the run.
                let code = (0..48u32)
                    .map(|i| ctx.peek16(bp.wrapping_add(i * 2)).unwrap_or(0))
                    .collect();
                // Probe the backing memory (raw WRAM, no CPU cache) at the
                // exact bp cycle — for cache/torn-write diagnosis.
                let probe = self.bp_probe.and_then(|a| ctx.peek32(a)).unwrap_or(0);
                self.bp_hit = Some(BpHit {
                    pc: bp,
                    regs: self.cpu.regs.r,
                    pr: self.cpu.regs.pr,
                    gbr: self.cpu.regs.gbr,
                    cycle: self.cpu.pipeline.cycles,
                    code,
                    probe,
                });
            }
            if let Some(trace) = &mut self.pc_trace {
                let pc = self.cpu.regs.pc;
                let idle = (0x0000_02B0..=0x0000_02B6).contains(&pc);
                // Freeze the buffer once execution reaches the work-RAM shell
                // region (0x0602_0000+): the boot give-up branch is the tail
                // just before that entry, and the shell's own loop would
                // otherwise flood the window and evict it.
                let (flo, fhi) = self.trace_freeze;
                let frozen = trace.back().is_some_and(|&b| (flo..fhi).contains(&b));
                if !idle && !frozen && trace.back() != Some(&pc) {
                    trace.push_back(pc);
                    if trace.len() > 32768 {
                        trace.pop_front();
                    }
                }
            }
        }
    }
}

// ---- Concrete entity: CD-block periodic-firmware timer. ------------------

/// Schedulable timer that drives the CD-block's periodic firmware
/// behaviour. The CD-block is high-level-emulated, not an SH-1 core (its
/// firmware is undumped — ADR-0015), so its time-varying behaviour (the
/// periodic status report, the drive/read pump) advances from this timer.
///
/// Modelling it as a scheduler entity (rather than a single poke at the
/// VBlank edge) ticks [`CdBlock::tick`](crate::cd_block::CdBlock::tick) on a
/// sub-frame granularity, so the report lands at the cycle-exact point
/// *within* the frame that the reference produces it — which the BIOS's
/// phase-sensitive CD-firmware liveness poll depends on. The entity itself
/// owns no CD state: it holds only its tick cadence and reaches the CD-block
/// through the shared bus context.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CdBlockEntity {
    /// Global cycle of the next tick.
    next: u64,
    /// Cycles between ticks (the sub-frame granularity, e.g. one scanline).
    interval: u64,
}

impl CdBlockEntity {
    /// `interval` is the tick granularity in SH-2 master cycles. Finer
    /// intervals place the periodic report more precisely within the frame;
    /// the CD-block's own accumulator carries the remainder, so the report
    /// cadence is independent of this value (only its phase resolution is).
    pub fn new(interval: u64) -> Self {
        Self {
            next: interval,
            interval,
        }
    }

    /// Re-anchor the next tick one interval after global cycle `now` — used by
    /// a system reset, which keeps the global clock running (time stays
    /// monotonic) instead of restarting it at 0.
    pub fn rebase(&mut self, now: u64) {
        self.next = now + self.interval;
    }
}

impl SchedEntity for CdBlockEntity {
    type Context = crate::SaturnBus;

    fn next_deadline(&self) -> u64 {
        self.next
    }

    fn step(&mut self, ctx: &mut Self::Context) {
        ctx.cd_block.tick(self.interval);
        self.next += self.interval;
    }
}

// ---- Heterogeneous Saturn entity. ----------------------------------------

/// The Saturn scheduler runs a heterogeneous set of entities — the two
/// SH-2 cores plus the CD-block periodic timer (the SCSP 68k is run from the
/// aggregate's batch loop instead). Wrapping
/// them in one enum keeps [`Scheduler`] a single generic over one entity
/// type, so the determinism contract (ties broken by insertion order) has
/// exactly one definition. Dispatch is a thin match.
///
/// The `Sh2` variant is far larger than `CdBlock` (it embeds a whole
/// `sh2::Cpu`), but it's also the hot one — the scheduler steps the CPUs
/// millions of times per second — and there are only a handful of entities,
/// so boxing the SH-2 to equalise variant size would add an indirection on
/// the hottest path to save a few KB. Not worth it; the lint is allowed.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum SaturnEntity {
    Sh2(Sh2Entity),
    CdBlock(CdBlockEntity),
}

impl SaturnEntity {
    /// Borrow the wrapped SH-2 entity. Panics if this entity is not an
    /// SH-2: callers hold an [`EntityId`] obtained when they added a
    /// known-SH-2 entity, so the variant is fixed at the call site even
    /// though the entity `Vec` is heterogeneous.
    pub fn sh2(&self) -> &Sh2Entity {
        match self {
            SaturnEntity::Sh2(e) => e,
            _ => panic!("scheduler entity is not an SH-2"),
        }
    }

    pub fn sh2_mut(&mut self) -> &mut Sh2Entity {
        match self {
            SaturnEntity::Sh2(e) => e,
            _ => panic!("scheduler entity is not an SH-2"),
        }
    }

    /// Borrow the wrapped CD-block timer entity. Panics if this entity is not
    /// the CD-block timer (see [`Self::sh2`] for why that is a caller bug).
    pub fn cd_block_mut(&mut self) -> &mut CdBlockEntity {
        match self {
            SaturnEntity::CdBlock(e) => e,
            _ => panic!("scheduler entity is not the CD-block timer"),
        }
    }
}

impl SchedEntity for SaturnEntity {
    type Context = crate::SaturnBus;

    fn next_deadline(&self) -> u64 {
        match self {
            SaturnEntity::Sh2(e) => e.next_deadline(),
            SaturnEntity::CdBlock(e) => e.next_deadline(),
        }
    }

    fn step(&mut self, ctx: &mut Self::Context) {
        match self {
            SaturnEntity::Sh2(e) => e.step(ctx),
            SaturnEntity::CdBlock(e) => e.step(ctx),
        }
    }
}
