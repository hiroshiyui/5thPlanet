# 0005. Queue a side effect, drain it at the aggregate

- **Status:** Accepted
- **Date:** 2026-05-25

## Context

Several peripherals need to cause **system-wide** side effects that reach
beyond themselves: SMPC must release/halt the slave SH-2, fire an NMI on
the master, and complete INTBACK; the SCU must run a DMA transfer across
the whole bus and raise interrupts. But each peripheral lives *inside*
`SaturnBus`, which lives inside `Saturn`. A method that already holds
`&mut self.bus.smpc` cannot also borrow `&mut self.bus` (to move bytes) or
`&mut self.scheduler` (to release a CPU) — Rust's borrow checker forbids
the overlapping `&mut`, and [ADR-0007](0007-forbid-unsafe-code.md) forbids
papering over it with `unsafe` aliasing.

We need a way for a peripheral to *request* a cross-cutting effect without
holding a borrow that conflicts with applying it.

## Decision

We will use a **queue-and-drain** pattern: a peripheral records its intent
in a plain field and returns; the `Saturn` aggregate later **pops** that
intent (which releases the peripheral borrow) and applies the effect with
unconflicted access to the bus and scheduler. Draining happens between
scheduler batches in `run_for` (and in `run_frame`).

Concretely:

- **SMPC** — a COMREG write sets `pending: Option<Command>` (the command latch
  only — SF is software-set / hardware-cleared, so the write itself does not
  raise it); `drain_smpc` calls `take_pending()`, performs the effect (release
  slave, raise NMI, schedule INTBACK), then `mark_command_done()` clears SF
  (which a polling guest had pre-written to 1).
  INTBACK additionally records `intback_complete_at` and is finished by a
  later drain once its execution time elapses.
- **SCU** — a triggering `D*EN` write records a `DmaRequest`;
  `drain_scu_dma` calls `take_pending_dma()`, moves the bytes via the bus,
  then `finish_dma()` writes back final state.

A sibling pattern handles the cases where one *method* genuinely needs two
disjoint fields at once (e.g. `reset()` touching both the bus and a
scheduler entity): **destructure `self`** into per-field bindings so the
borrows are provably disjoint, instead of going through a `&mut self`
accessor that borrows the whole struct.

## Consequences

- Borrows stay clean and checked at compile time — no `RefCell` runtime
  borrow panics, no `unsafe` aliasing.
- Effects apply at **well-defined drain points** (between 256-cycle
  scheduler batches), giving a single, predictable place where
  peripheral state changes the rest of the system.
- That batching introduces a small latency: an effect applies at the next
  drain, up to `SMPC_POLL_QUANTUM` (256) cycles later. Usually harmless,
  but it *does* interact with timing — the INTBACK SF-clear timing showed
  that drain granularity matters, which is why INTBACK uses an explicit
  completion deadline rather than firing on the next drain.
- Cost: a layer of indirection, and the drain order at the aggregate is a
  place that must be kept correct (e.g. complete a due INTBACK before
  processing newly-queued commands).

## Alternatives considered

- **`RefCell` / `Cell` interior mutability** to dodge the borrow checker.
  Rejected: trades a compile error for a possible runtime panic and hides
  the actual data-flow.
- **Callbacks / channels** between peripheral and aggregate. Rejected:
  heavier than needed and risks nondeterministic ordering, which would
  fight [ADR-0004](0004-deterministic-deadline-scheduler.md).
- **`unsafe` raw-pointer aliasing.** Rejected outright by
  [ADR-0007](0007-forbid-unsafe-code.md).
