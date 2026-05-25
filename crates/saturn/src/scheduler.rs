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

#[derive(Clone, Debug)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct EntityId(pub(crate) usize);

// ---- Concrete entity: SH-2 wrapped for the Saturn bus context. -----------

/// Schedulable wrapper around an `sh2::Cpu`. The entity's `next_deadline`
/// is the CPU's `pipeline.cycles` when running, or `u64::MAX` when
/// halted — that way the scheduler's "most-behind wins" rule naturally
/// skips a halted CPU (it's "infinitely ahead") without any special-
/// casing in the scheduler. The slave SH-2 lives in this halted state
/// from power-on until SMPC's `SETSL` command releases it.
#[derive(Clone, Debug)]
pub struct Sh2Entity {
    pub cpu: sh2::Cpu,
    halted: bool,
}

impl Sh2Entity {
    pub fn new(cpu: sh2::Cpu) -> Self {
        Self { cpu, halted: false }
    }

    /// Construct already-halted — what the slave starts as on power-on.
    pub fn new_halted(cpu: sh2::Cpu) -> Self {
        Self { cpu, halted: true }
    }

    pub fn is_halted(&self) -> bool {
        self.halted
    }

    pub fn set_halted(&mut self, halted: bool) {
        self.halted = halted;
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
            self.cpu.step(ctx);
        }
    }
}
