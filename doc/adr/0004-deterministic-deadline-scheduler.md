# 0004. Event-driven scheduler with deterministic "smallest deadline wins"

- **Status:** Accepted
- **Date:** 2026-05-25

## Context

The Saturn runs many chips on a shared timeline — two SH-2s today, and
VDP/SCU/SCSP entities later — and [ADR-0002](0002-accuracy-over-performance.md)
requires their interleaving to be **cycle-accurate**. Two things follow
from that:

1. We must decide, at each step, which chip is most "behind" and advance
   it, rather than running one chip to completion and approximating the
   others.
2. The result must be **deterministic**: the ROM regression hashes, the
   dual-SH-2 tests, and the Yabause reference-diff all assume that the
   same start state plus the same `run_for(N)` yields the *exact* same
   sequence of steps every run and on every machine. Any nondeterminism
   (thread scheduling, hash-map iteration order, tie-breaks that depend
   on address) would make those checks meaningless.

## Decision

We will use an **event-driven scheduler** (`saturn::scheduler`) in which
each `SchedEntity` reports a `next_deadline()` — the global cycle it next
wants to run at — and the scheduler always advances the entity with the
**smallest deadline**, then lets it push its deadline forward by the work
that step cost. `Scheduler::run_for(n)` repeats this until the clock
reaches the horizon.

The determinism contract is one rule, defined in exactly one place
(`pick_behind`): **ties in deadline resolve to insertion order** (the
lowest-index entity wins; the strict `dl < best_dl` comparison never
displaces an equal-deadline incumbent). The master SH-2 is added first,
the slave second, so the master deterministically wins ties.

A halted entity (e.g. the slave before SMPC `SSHON`) reports
`next_deadline() == u64::MAX`, so "smallest deadline wins" naturally skips
it without any special-casing.

## Consequences

- **Deterministic replay** holds: identical state + `run_for(N)` gives
  identical results, which the scheduler tests assert and which the ROM
  hashes and reference-diff depend on.
- Adding chips is uniform — they implement `SchedEntity` and slot in; the
  halted-as-`u64::MAX` trick keeps the scheduler body branch-free of
  per-chip state.
- **The tie-break rule is load-bearing.** Any change to entity insertion
  order, or to `pick_behind`, silently re-times the whole system — it
  must preserve insertion-order tie resolution.
- Today the scheduler does an O(n) linear scan, fine for n = 2. When the
  entity count grows past a handful, swap it for a `BinaryHeap` keyed on
  `(deadline, insertion_order)` — the key must keep the same tie rule so
  determinism is preserved.

## Alternatives considered

- **Round-robin / fixed time-slice.** Rejected: not deadline-driven, so
  it can't keep chips with different step costs cycle-aligned; coarser
  and less accurate than the project's axis allows.
- **One OS thread per chip.** Rejected: nondeterministic by construction,
  which would break every hash-based and trace-based verification.
- **Unspecified tie-breaking.** Rejected: leaving ties to `HashMap` order
  or address would reintroduce nondeterminism; the contract exists
  precisely to forbid that.
