# 0025. The SCU DMA halts neither SH-2 (synchronous copy at the trigger)

- **Status:** Accepted
- **Date:** 2026-06-29 (retroactively recorded; the decision dates to 2026-06-26,
  `64237d7`)

## Context

[ADR-0021](0021-per-access-bsc-bus-timing.md) ported Mednafen's BSC bus-timing
model. One of its sub-decisions (Decision bullet 4 / `a101f15`) was that a
**C-bus-endpoint SCU DMA halts both SH-2s** for its paced duration
(`RecalcDMAHalt` / `SetExtHalt`). At the time that both-CPU halt was the dominant
whole-system phase lever — it pulled the boot sequence's tick count from +182 to
−47 against the oracle's 4497.

But our SCU DMA engine does **not** keep a DMA "time-running". Following the
queue-and-drain pattern ([ADR-0005](0005-queue-and-drain-side-effects.md)),
`drain_dma` copies the *whole* transfer synchronously at the trigger point and
raises DMA-end immediately. There is no interval on our timeline during which a
CPU is genuinely halted by an in-flight DMA — so charging the per-access transfer
time as a CPU stall **double-counts** cycles the SH-2s were never actually halted
for.

Mednafen can charge the halt because it runs a genuinely *timed* DMA engine that
halts the C-bus while the transfer is active and force-finishes it the moment a
CPU touches the A/B bus (`CheckForceDMAFinish`). We model the synchronous engine,
not the timed one, so importing only its halt-charge — without the timed transfer
it accounts for — is unsound.

## Decision

We will charge the SCU DMA **zero CPU-halt cost**. `drain_dma` copies the transfer
synchronously and returns **0** halt cycles, and the DMA-end interrupt is raised at
the trigger point (the documented hand-off boundary). `AccessKind::Dma` stays
cost-only on the **DMA engine's own** timeline (Mednafen's `dma_time_thing`
values), never as an SH-2 stall.

This **supersedes the both-CPU-halt sub-decision of ADR-0021**
(`RecalcDMAHalt` / `SetExtHalt`, `a101f15`); the rest of ADR-0021's BSC model
stands unchanged. A reviewer who sees a diff re-introducing a CPU-halt charge for
a synchronous `drain_dma` should treat it as violating this ADR.

## Consequences

- **Fixed / correct:** the SH-2 cycle accounting no longer double-charges cycles
  the CPUs were never halted for; the DMA-end-at-trigger boundary is the
  documented hand-off (it also keeps the synchronous queue-and-drain DMA path
  self-consistent).
- **Cost we accept:** this **regresses whole-system phase** versus the oracle — the
  both-CPU halt had been pulling the tick count toward Mednafen's 4497, and removing
  it gives that back. We take the phase regression in exchange for cycle-accounting
  correctness; the residual ~1 % phase gap is a known stop-rule item (the
  recognition handshake + a 68k-gated poll loop), not an active target.
- **Follow-up:** a faithful *timed* SCU DMA engine (modelling Mednafen's active C-bus
  halt + `CheckForceDMAFinish` force-finish) would recover the phase *without*
  double-counting — but it is a substantial rewrite and is not pulled until a game
  needs it.
- **Save-state:** unaffected — the change is a charge value, not a serialized field
  (ADR-0021's v6/v9 layout is untouched).

## Alternatives considered

- **Keep the both-CPU halt** (`a101f15`). Rejected: it is sound only for a *timed*
  DMA engine; on our synchronous `drain_dma` it charges halt cycles for an interval
  that does not exist on our timeline, double-counting against the SH-2s.
- **Model the full timed DMA engine now** (Mednafen's active-halt + force-finish).
  Deferred: it is the correct long-term fix and would recover the phase, but it is a
  substantial engine rewrite, and no current target needs the ~1 % phase it buys.
- **Halt only the triggering CPU, not both.** Rejected: still charges a halt for a
  transfer that completes instantaneously on our timeline — wrong for the same
  reason, just by less.
