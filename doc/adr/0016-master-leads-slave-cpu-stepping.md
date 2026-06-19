# 0016. The live SH-2 pair is stepped master-leads-slave, not by the generic scheduler

- **Status:** Accepted
- **Date:** 2026-06-19 (retroactively recorded; the decision dates to M11 / the Mednafen-alignment Phase 2 work)

## Context

[ADR-0004](0004-deterministic-deadline-scheduler.md) established the
generic, event-driven scheduler: each `SchedEntity` reports `next_deadline()`,
the **most-behind entity steps**, and ties break by insertion order. That rule is
correct and deterministic for loosely-coupled entities.

It is the wrong rule for the **two SH-2s specifically.** Master and slave are
sub-instruction coupled — they share work-RAM with no hardware cache coherency,
wake each other through the FRT input-capture pin on a *specific* instruction
(§ inter-CPU FTI), and the master takes SCU interrupts at instruction boundaries.
Under "most-behind wins," the **slave can lead the master**, which reorders those
handoffs relative to real hardware and, decisively, relative to the reference
oracle.

The reference oracle (Mednafen / Beetle Saturn, LLE) steps the pair in a fixed
order: `CPU[0].Step()` advances the master one instruction, then
`RunSlaveUntil(master_timestamp)` runs the slave up to the master's new
timestamp (`ss.cpp`). The whole debugging methodology is an **LLE↔LLE master-PC
trace-diff** ([ADR-0017](0017-reference-oracle-policy.md)); for two PC streams to
be comparable, ours must step the CPUs in the *same* order. Separately, SCU
interrupts must be evaluated **per master instruction** — the SCU IRL is a level
the master samples every cycle, so an interrupt must land at the exact
instruction `SR.imask` drops below its level, not once per scheduler batch.

This was not theoretical: the earlier between-batch `drain_scu_intc` forwarded
interrupts at batch boundaries (wrong instruction), and a most-behind slave-leads
ordering diverged timing-sensitive WRAM handoffs — both produced trace
divergences against Mednafen during M11 game boot.

## Decision

We will step the **live SH-2 pair via `Saturn::step_cpus` in Mednafen's
master-leads-slave order** (master one instruction → slave runs until it catches
up to the master's timestamp; "Phase 2A"), and **sample the SCU interrupt line
per master instruction** inside `step_cpus` ("Phase 2B"), presenting the SCU's
fixed `0x40 + index` vector at the exact instruction the mask drops.

The generic `Scheduler` of ADR-0004 is **retained** — it still drives the
CD-block timer and backs the determinism unit test — but the **live CPU pair is
not stepped by its most-behind rule.** Batches are clamped to the next scheduled
peripheral-event edge (VBlank-IN/-OUT, pending INTBACK), capped by
`SMPC_POLL_QUANTUM`, so interrupt assertion and the raster registers settle at
the cycle-exact point the reference produces them.

This **refines, and does not supersede,** ADR-0004. A reviewer should treat any
diff that routes the live SH-2 pair back through `Scheduler::pick_behind`, or that
moves SCU-interrupt evaluation back to a between-batch drain, as a violation of
this ADR.

## Consequences

- **Easier:** the LLE↔Mednafen PC-trace-diff aligns — same stepping order means
  the two master PC streams are directly comparable until a genuine bug, which is
  how M11's CD/DCHG, FTCSR, and menu-DMA divergences were localized. Interrupts
  land cycle-exactly, fixing the class of bugs where a handler ran a few
  instructions early or late.
- **Cost we accept:** two stepping models coexist (the generic deadline scheduler
  *and* `step_cpus`), so a contributor must know the live pair is special and not
  "unify" them. The batch edge-clamp adds a small amount of scheduling logic
  (`cycles_to_next_event`) on the hot path.
- **Bounded:** the change is to *ordering and interrupt-sampling granularity*, not
  to instruction semantics — each SH-2 still executes via the same `sh2` core, so
  determinism and the save-state round-trip are preserved.

## Alternatives considered

- **Use the generic most-behind scheduler for the CPUs too** (the original
  ADR-0004 rule, unmodified). Rejected: it permits the slave to lead, reordering
  inter-CPU WRAM/FTI handoffs away from hardware and from Mednafen, and the
  trace-diff cannot align when the stepping order itself differs.
- **Keep per-batch interrupt draining** (`drain_scu_intc`). Rejected: it forwards
  the interrupt at a batch boundary rather than the instruction where the level
  beats the mask, shifting handler entry by up to a batch — visible as a trace
  divergence.
- **Invent our own stepping order and accept trace-diff drift.** Rejected: there
  is exactly one LLE oracle whose PC stream we can diff against; matching its
  order is the entire point. A novel order would forfeit the methodology that
  finds these bugs.
