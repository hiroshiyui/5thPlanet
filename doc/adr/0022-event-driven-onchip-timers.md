# 0022. Event-driven SH-2 on-chip FRT/WDT timers + INTC (lazy materialize)

- **Status:** Accepted
- **Date:** 2026-06-20

## Context

The SH-2 on-chip free-running timer (FRT), watchdog timer (WDT), and interrupt
controller (INTC) were advanced *every instruction*: `Cpu::step` called
`advance_timers` + `refresh_interrupts` on each retire. Correct, but expensive —
profiling put that machinery at **~11% of self-time** in poll-heavy scenes, where
a tight `ICF`/status spin loop ticks the timer and re-scans the INTC millions of
times for no state change. Mednafen instead models these lazily: the timer is a
monotone clock divider materialized only when its value is needed, and the
interrupt output is recomputed only when an input changes. This is task A1 of the
M13 fidelity/perf backlog, and the per-access bus model
([0021](0021-per-access-bsc-bus-timing.md)) is the other half of the same
per-instruction overhead.

## Decision

We will port Mednafen's lazy model into `crates/sh2/src/onchip/`
(`d2f2b0e`/`ef6bf19`/`c643fce`/`e6b3d72`):

- **Lazy FRT/WDT.** `Cpu::step` no longer ticks per instruction; a per-step gate
  `if now >= onchip.timer_next_ts()` materializes FRC/WTCNT only at the next
  scheduled edge (`frt_wdt_update` advances by `(now>>shift)-(lastts>>shift)` —
  our monotone `u64` cycle never rebases, so it *is* Mednafen's `ClockDivider`;
  `frt_wdt_recalc_net` recomputes the edge). A timer-register read/write catches
  the counters up on demand (`timer_sync_pre`/`_post`).
- **Recalc-on-change INTC.** `refresh_interrupts` runs only when an input changes
  — from `frt_wdt_update` on a flag set, after every on-chip register write, after
  a DMAC transfer-end, and in `fti_input_capture` — behind a signature gate that
  keeps it idempotent, so over-calling is free.
- The gate sits at **end-of-step**, keeping the interrupt phase identical to the
  per-instruction model. Ported in **four golden-invariant-by-construction
  stages**, each verified bit-identical (the `bios_boot` golden
  `0x0B1BA6E5180766F7` is unchanged and both playable games were play-tested).
  Save-state v10: `OnChip::lastts` (serialized timer phase) and `next_ts` **must**
  serialize (the lazy FRC field would otherwise diverge on load).

## Consequences

- **Easier:** poll-scene per-instruction timer/INTC overhead **~11% → ~1.3%**,
  with zero behavioral change (golden-invariant). [0002](0002-accuracy-over-performance.md)
  forbids trading accuracy *for* speed; this trades neither — it's the same
  output, computed only when it can change.
- **Gotchas (load-bearing):** `release_slave` must `reset_timer_epoch` after the
  cycle resync (a stale `lastts` vs the jumped cycle over-ticks/hangs); the
  end-of-step gate placement is what preserves the interrupt phase; `next_ts` must
  be in the save state. CKS=3 (external FTCI clock) freezes the FRT.
- **Cost we accept:** more subtle state than the per-instruction model (the lazy
  phase + the full set of recalc triggers must stay complete) — mitigated by
  porting Mednafen's proven model rather than inventing a dirty-flag scheme.

## Alternatives considered

- **Keep ticking per instruction.** Correct, but the measured ~11% overhead lands
  exactly in the menu/poll scenes where responsiveness matters.
- **A hand-rolled dirty-flag recalc.** Considered and warned off as fragile (easy
  to miss a trigger and desync the INTC); porting Mednafen's lazy model made
  recalc-on-change robust instead of brittle.
- **Dispatch peripherals mid-batch to react sooner** (the related SMPC attempt,
  `b65cd18`). Rejected and reverted (`4d0c67f`): breaking the SH-2 batch
  re-anchored `run_frame`'s grid and black-screened *Doukyuusei*. Events — not
  batch breaks — are the right tool, and this ADR's staged template is how A1's
  other edges (VDP1 draw-end, SCU Timer-0, FTI) were converted.
