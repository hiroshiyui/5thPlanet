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
//! For M2 the only entities are the master and slave SH-2 cores. The
//! linear scan over entities is O(n) per step; with n=2 that's fine.
//! Once VDP/SCU/SCSP arrive in M3+ and n grows past a handful, swap
//! the scan for a `BinaryHeap` keyed on `(deadline, insertion_order)`.

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

    /// Like [`run_for`](Self::run_for), but invokes `before_step(entity, id)`
    /// immediately before each entity is stepped. Used by the full-system
    /// boot tracer to record the master SH-2's PC *in scheduler order* (with
    /// the slave and CD-block interleaved), which `run_frame` produces but a
    /// master-only single-step trace cannot. Not on the hot path.
    pub fn run_for_traced<F>(&mut self, cycles: u64, ctx: &mut E::Context, mut before_step: F)
    where
        F: FnMut(&E, EntityId),
    {
        if self.entities.is_empty() || cycles == 0 {
            return;
        }
        let target = self.now().saturating_add(cycles);
        while self.now() < target {
            let idx = self.pick_behind();
            before_step(&self.entities[idx], EntityId(idx));
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

/// Schedulable wrapper around an `sh2::Cpu`. The entity's `next_deadline`
/// is the CPU's `pipeline.cycles` when running, or `u64::MAX` when
/// halted — that way the scheduler's "most-behind wins" rule naturally
/// skips a halted CPU (it's "infinitely ahead") without any special-
/// casing in the scheduler. The slave SH-2 lives in this halted state
/// from power-on until SMPC's `SETSL` command releases it.
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
    /// Debug-only full-speed breakpoint: when set and the master reaches this
    /// PC, [`bp_hit`] captures R0..R15 plus 96 bytes of code at the PC (so a
    /// transient work-RAM routine can be disassembled at the instant it runs).
    #[serde(skip)]
    bp: Option<u32>,
    #[serde(skip)]
    bp_hit: Option<([u32; 16], Vec<u16>)>,
    /// When set, the master's BIOS SYS-call entry addresses are intercepted and
    /// run by [`crate::bios_hle`] instead of executing BIOS code (the cold HLE
    /// direct boot, ADR-0011). `#[serde(skip)]` — save-state across an HLE boot
    /// is out of scope, so a restored state resumes with LLE dispatch.
    #[serde(skip)]
    hle_sys_active: bool,
}

impl Sh2Entity {
    pub fn new(cpu: sh2::Cpu) -> Self {
        Self {
            cpu,
            halted: false,
            pc_trace: None,
            bp: None,
            bp_hit: None,
            hle_sys_active: false,
        }
    }

    /// Construct already-halted — what the slave starts as on power-on.
    pub fn new_halted(cpu: sh2::Cpu) -> Self {
        Self {
            cpu,
            halted: true,
            pc_trace: None,
            bp: None,
            bp_hit: None,
            hle_sys_active: false,
        }
    }

    /// Enable HLE BIOS SYS-call dispatch on this core (the cold HLE direct boot).
    pub fn set_hle_sys(&mut self, on: bool) {
        self.hle_sys_active = on;
    }

    /// Begin recording a full-speed PC trace (debug; see [`pc_trace`]).
    pub fn enable_pc_trace(&mut self) {
        self.pc_trace = Some(std::collections::VecDeque::new());
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

    /// Arm a full-speed breakpoint at `pc` (debug; see [`bp`]).
    pub fn set_bp(&mut self, pc: u32) {
        self.bp = Some(pc);
        self.bp_hit = None;
    }

    /// Take the captured (registers, code-words) from a breakpoint hit, if any.
    pub fn take_bp_hit(&mut self) -> Option<([u32; 16], Vec<u16>)> {
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
            // HLE BIOS SYS-call dispatch (cold HLE direct boot, ADR-0011):
            // intercept a call landing on a SYS entry address and run the host
            // implementation in place of BIOS code, then return to the caller.
            // Checked before fetch so the BIOS routine never executes.
            if self.hle_sys_active
                && !self.cpu.next_is_delay_slot()
                && crate::bios_hle::is_sys_addr(self.cpu.regs.pc)
            {
                ctx.cycle = self.cpu.pipeline.cycles;
                crate::bios_hle::dispatch(&mut self.cpu, ctx);
                return;
            }
            // Publish the current global cycle to the bus so time-varying
            // peripheral reads (SMPC SF INTBACK completion) resolve at the
            // exact instruction that reads them.
            ctx.cycle = self.cpu.pipeline.cycles;
            self.cpu.step(ctx);
            if let Some(bp) = self.bp
                && self.cpu.regs.pc == bp
                && self.bp_hit.is_none()
            {
                use sh2::bus::{AccessKind, Bus};
                let mut code = Vec::with_capacity(48);
                for i in 0..48u32 {
                    code.push(ctx.read16(bp + i * 2, AccessKind::Data).0);
                }
                self.bp_hit = Some((self.cpu.regs.r, code));
            }
            if let Some(trace) = &mut self.pc_trace {
                let pc = self.cpu.regs.pc;
                let idle = (0x0000_02B0..=0x0000_02B6).contains(&pc);
                // Freeze the buffer once execution reaches the work-RAM shell
                // region (0x0602_0000+): the boot give-up branch is the tail
                // just before that entry, and the shell's own loop would
                // otherwise flood the window and evict it.
                let frozen = trace
                    .back()
                    .is_some_and(|&b| (0x0602_0000..0x0605_0000).contains(&b));
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
/// behaviour. The CD-block is not (yet) a CPU — the real SH-1 + CD-ROM
/// firmware lands in a later milestone — so for now its only time-varying
/// behaviour is the periodic status report it emits roughly once per frame.
///
/// Modelling it as a scheduler entity (rather than a single poke at the
/// VBlank edge) ticks [`CdBlock::tick`](crate::cd_block::CdBlock::tick) on a
/// sub-frame granularity, so the report lands at the cycle-exact point
/// *within* the frame that the reference produces it — which the BIOS's
/// phase-sensitive CD-firmware liveness poll depends on. The entity itself
/// owns no CD state: it holds only its tick cadence and reaches the CD-block
/// through the shared bus context. When the full CD-block arrives, a proper
/// SH-1 core entity replaces this timer.
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

/// The Saturn scheduler runs a heterogeneous set of entities — today the
/// two SH-2 cores plus the CD-block periodic timer; as more time-driven
/// chips land (SCSP M68k, the real CD-block SH-1) they join here. Wrapping
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
