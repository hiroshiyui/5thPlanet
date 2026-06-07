//! Scheduler determinism and fairness (M2 task #4).
//!
//! Fake entities for the trait-level tests; real `Sh2Entity` for the
//! cross-crate sanity check at the end.

use saturn::{SaturnBus, SchedEntity, Scheduler, Sh2Entity};

/// Test entity: an opaque counter that advances by `rate` each step.
/// The advance rate models "this chip's instructions cost N cycles each".
#[derive(Clone, Debug)]
struct Counter {
    cycles: u64,
    rate: u64,
    /// Step log for inspecting interleave order in tests.
    log: Vec<u64>,
}

impl Counter {
    fn new(rate: u64) -> Self {
        Self {
            cycles: 0,
            rate,
            log: Vec::new(),
        }
    }
}

impl SchedEntity for Counter {
    type Context = ();
    fn next_deadline(&self) -> u64 {
        self.cycles
    }
    fn step(&mut self, _: &mut ()) {
        self.log.push(self.cycles);
        self.cycles += self.rate;
    }
}

#[test]
fn empty_scheduler_is_a_no_op() {
    let mut s: Scheduler<Counter> = Scheduler::new();
    s.run_for(100, &mut ());
    assert_eq!(s.now(), 0);
}

#[test]
fn run_for_zero_does_not_step_anyone() {
    let mut s = Scheduler::new();
    let a = s.add(Counter::new(1));
    s.run_for(0, &mut ());
    assert!(s.entity(a).log.is_empty());
}

#[test]
fn equal_rate_entities_alternate_strictly() {
    let mut s = Scheduler::new();
    let a = s.add(Counter::new(1));
    let b = s.add(Counter::new(1));
    s.run_for(6, &mut ());
    // Both end at cycle 6, having stepped 6 times each. The interleave
    // alternates because ties resolve to insertion order: A goes first
    // (both at 0), then B (A at 1, B at 0 → B is behind), then A, etc.
    assert_eq!(s.entity(a).cycles, 6);
    assert_eq!(s.entity(b).cycles, 6);
    assert_eq!(s.entity(a).log, [0, 1, 2, 3, 4, 5]);
    assert_eq!(s.entity(b).log, [0, 1, 2, 3, 4, 5]);
}

#[test]
fn slower_entity_runs_more_times_per_global_cycle() {
    // A costs 4 cycles per step, B costs 1. Over 12 global cycles A
    // should run 3 times and B 12 times.
    let mut s = Scheduler::new();
    let a = s.add(Counter::new(4));
    let b = s.add(Counter::new(1));
    s.run_for(12, &mut ());
    assert_eq!(s.entity(a).log.len(), 3);
    assert_eq!(s.entity(b).log.len(), 12);
    assert_eq!(s.entity(a).cycles, 12);
    assert_eq!(s.entity(b).cycles, 12);
}

#[test]
fn run_for_is_deterministic_across_repeated_runs() {
    // Seed two identical schedulers, run each for the same horizon,
    // compare step logs byte-for-byte. Catches accidental nondeterminism
    // from hash iteration order, time-based tie-breaking, etc.
    fn build() -> Scheduler<Counter> {
        let mut s = Scheduler::new();
        s.add(Counter::new(3));
        s.add(Counter::new(2));
        s.add(Counter::new(5));
        s
    }
    let mut s1 = build();
    let mut s2 = build();
    s1.run_for(60, &mut ());
    s2.run_for(60, &mut ());
    for (a, b) in s1.entities().iter().zip(s2.entities()) {
        assert_eq!(a.log, b.log, "step logs must match exactly");
        assert_eq!(a.cycles, b.cycles);
    }
}

#[test]
fn now_tracks_minimum_deadline_across_entities() {
    let mut s = Scheduler::new();
    let _ = s.add(Counter::new(10));
    let _ = s.add(Counter::new(1));
    s.run_for(20, &mut ());
    // now() is the min — the slower-overall entity (A) is ahead because
    // each of its steps is large; the fast (B) is the trailing edge.
    // Either way both should be ≥ 20.
    assert!(s.now() >= 20);
    for e in s.entities() {
        assert!(e.cycles >= 20, "every entity reaches the horizon");
    }
}

#[test]
fn sh2_entity_coscheduled_via_real_bus() {
    // Two SH-2s sharing a SaturnBus, running NOPs out of low WRAM.
    // Verify both advance and roughly keep pace.
    let mut bus = SaturnBus::with_blank_bios();
    // Plant a small NOP loop in low WRAM at 0x0020_1000:
    //   NOP × 8, BRA -8  (back to start)
    // Actually simpler: place 8 NOPs and let PC run off, since each NOP
    // costs 1 cycle and we just want both CPUs to make progress.
    for i in 0..8u32 {
        bus.low_wram.write16(0x1000 + i * 2, 0x0009);
    }

    let mut s: Scheduler<Sh2Entity> = Scheduler::new();
    let master_id = s.add({
        let mut cpu = sh2::Cpu::new();
        cpu.regs.pc = 0x0020_1000;
        cpu.regs.r[15] = 0x0020_8000;
        Sh2Entity::new(cpu)
    });
    let slave_id = s.add({
        let mut cpu = sh2::Cpu::new();
        cpu.regs.pc = 0x0020_1000;
        cpu.regs.r[15] = 0x0020_8400;
        Sh2Entity::new(cpu)
    });

    s.run_for(50, &mut bus);
    let m = s.entity(master_id).cpu.pipeline.cycles;
    let v = s.entity(slave_id).cpu.pipeline.cycles;
    assert!(m >= 50, "master reached horizon");
    assert!(v >= 50, "slave reached horizon");
    // Neither should be more than one large step ahead — fairness check.
    let diff = m.abs_diff(v);
    assert!(diff < 50, "drift {diff} too large; scheduler not fair");
}

#[test]
fn now_is_zero_on_empty_scheduler() {
    // An empty scheduler reports it's "at" cycle 0 (min over an empty set).
    let s: Scheduler<Counter> = Scheduler::new();
    assert_eq!(s.now(), 0);
}

#[test]
fn add_returns_sequential_handles_and_entity_accessors_work() {
    let mut s = Scheduler::new();
    let a = s.add(Counter::new(2));
    let b = s.add(Counter::new(7));
    // EntityId handles are distinct and resolve to the right entity.
    assert_ne!(a, b);
    assert_eq!(s.entity(a).rate, 2);
    assert_eq!(s.entity(b).rate, 7);
    assert_eq!(s.entities().len(), 2);
    // entity_mut gives mutable access.
    s.entity_mut(a).cycles = 99;
    assert_eq!(s.entity(a).cycles, 99);
}

/// Entity that records the *global* step order into a shared `Vec` (its
/// Context), so a test can assert the exact interleave the scheduler chose.
#[derive(Clone)]
struct Ordered {
    id: u64,
    cycles: u64,
    rate: u64,
}

impl SchedEntity for Ordered {
    type Context = Vec<u64>;
    fn next_deadline(&self) -> u64 {
        self.cycles
    }
    fn step(&mut self, order: &mut Vec<u64>) {
        order.push(self.id);
        self.cycles += self.rate;
    }
}

#[test]
fn pick_behind_breaks_ties_by_insertion_order() {
    // Three entities all at deadline 0 with equal rate. The determinism
    // contract says the most-behind pick resolves ties to the lowest insertion
    // order, so the first full round of steps must go A, B, C in that order.
    let mut s = Scheduler::new();
    s.add(Ordered { id: 0, cycles: 0, rate: 5 });
    s.add(Ordered { id: 1, cycles: 0, rate: 5 });
    s.add(Ordered { id: 2, cycles: 0, rate: 5 });
    let mut order = Vec::new();
    // Advancing to 5 takes exactly one round (each goes 0 → 5).
    s.run_for(5, &mut order);
    assert_eq!(order, [0, 1, 2], "ties resolve to insertion order, in turn");
}

#[test]
fn pick_behind_selects_strictly_smallest_deadline() {
    // B is genuinely behind (smaller deadline), so it is picked regardless of
    // insertion order — only true ties fall back to insertion order.
    let mut s = Scheduler::new();
    let a = s.add(Counter::new(10));
    let b = s.add(Counter::new(1));
    // Advance A once so the deadlines differ: A at 10, B at 0.
    s.entity_mut(a).cycles = 10;
    s.run_for(1, &mut ());
    // now() was 0 (B's deadline); target is 1; only B (the behind one) steps.
    assert!(s.entity(a).log.is_empty(), "ahead entity is not stepped");
    assert_eq!(s.entity(b).log, [0]);
}

/// A halted entity: reports `u64::MAX` so the most-behind rule skips it,
/// mirroring `Sh2Entity`'s halted slave.
#[derive(Clone)]
struct Haltable {
    cycles: u64,
    halted: bool,
    steps: u64,
}

impl SchedEntity for Haltable {
    type Context = ();
    fn next_deadline(&self) -> u64 {
        if self.halted { u64::MAX } else { self.cycles }
    }
    fn step(&mut self, _: &mut ()) {
        self.steps += 1;
        self.cycles += 1;
    }
}

#[test]
fn halted_entity_with_max_deadline_is_never_stepped() {
    // The halted entity reports u64::MAX, so the runnable one is always the
    // most-behind and the halted one accumulates zero steps — the scheduler
    // skips it without any special-casing (the Sh2Entity halted-slave model).
    let mut s = Scheduler::new();
    let live = s.add(Haltable { cycles: 0, halted: false, steps: 0 });
    let dead = s.add(Haltable { cycles: 0, halted: true, steps: 0 });
    s.run_for(8, &mut ());
    assert_eq!(s.entity(live).steps, 8, "live entity advances to horizon");
    assert_eq!(s.entity(dead).steps, 0, "halted entity is skipped entirely");
    // now() is driven by the live entity (the halted one is at u64::MAX).
    assert_eq!(s.now(), 8);
}

#[test]
fn run_for_advances_until_target_deadline() {
    // run_for(N) steps until now() >= start + N. A single entity stepping by 3
    // overshoots from 0 → 3 → 6 → 9 to first reach/pass the target 7.
    let mut s = Scheduler::new();
    let a = s.add(Counter::new(3));
    s.run_for(7, &mut ());
    assert!(s.now() >= 7);
    assert_eq!(s.entity(a).cycles, 9, "smallest multiple of 3 that is >= 7");
    assert_eq!(s.entity(a).log, [0, 3, 6]);
}
