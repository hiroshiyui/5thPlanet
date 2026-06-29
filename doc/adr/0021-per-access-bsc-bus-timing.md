# 0021. Per-access BSC bus-timing model (faithful Mednafen port)

- **Status:** Accepted — except the **both-CPU DMA-halt** sub-decision, which was
  superseded by [0025](0025-scu-dma-no-cpu-halt.md) (the SCU DMA now halts neither
  SH-2; the rest of this model stands).
- **Date:** 2026-06-12

## Context

Whole-system cycle accuracy (M12) requires charging the Saturn's bus the way the
hardware does. The two SH-2s share one external bus, and cost depends on the
region and access width: CS0 is a **16-bit** bus (low WRAM, BIOS, backup RAM,
SMPC), CS3 is **32-bit SDRAM** (high WRAM), the A-bus is the cartridge, the B-bus
is VDP1/VDP2/SCSP, and the C-bus is the SCU↔SDRAM DMA path.

[0003](0003-bus-returns-stall-cycles.md) settled that the `Bus` impl owns the
wait-state math (the CPU just accumulates the returned stalls); *what that math
is* was, until M12, a coarse per-region approximation. That approximation was
wrong in load-bearing ways — most sharply, it charged the SCSP B-bus read **0**
cycles, which let VF2's sound-submit spin-timeout expire before the 68k driver's
IRQ-masked wake, latching a permanent "sound wedged" flag and silently dropping
every SFX. The SCU and SH7604 manuals leave the coupled inter-chip timing
under-specified, so the reference of last resort is Mednafen's `BSC_BusRead/Write`
+ `scu.inc` ([0017](0017-reference-oracle-policy.md)).

## Decision

We will port Mednafen's BSC bus-timing model faithfully into `SaturnBus`
(`BusTiming` + `SaturnBus::charge`, `crates/saturn/src/bus.rs`; `57cbfe5`+`006187a`):

- A **shared bus timestamp** both CPUs sync to — CPU↔CPU arbitration *emerges*
  from it (cf. Mednafen `SH7095_mem_timestamp`; pairs with the master-leads-slave
  stepping of [0016](0016-master-leads-slave-cpu-stepping.md)).
- **CS0 16-bit** per-transaction costs (low WRAM +7, BIOS +8, backup +8, SMPC 0,
  FTI window +8, unknown +4; a 32-bit access pays twice); **CS3 SDRAM** (read +7,
  write +2 with a 2-cycle array-busy window; cache line-fills are one burst —
  `AccessKind::LineFill` beats are free); the **SH-2 write buffer** (a lone store
  returns 0 stall; only write-after-write backlogs stall); **bus turnaround +1**;
  A-bus cost from the live ASR0 wait/strobe fields; CD CS2 +8 per 16-bit.
- **B-bus exact deferred-write model** (`scu.inc BBusRW_DB`; `6973ce8`): a write
  hands off in **+2** CPU cycles and posts its device-side completion (SCSP
  +17/+13, VDP1 +9/+1, VDP2 +3/+1 per first/second 16-bit half) on
  `bbus_write_finish`, which only the *next* B-bus access waits out; a B-bus
  **read** is always two 16-bit halves regardless of width (VDP1 +28, VDP2 +40,
  **SCSP +48** — the muted-SFX fix).
- Stalls returned **incrementally** (`BusTiming::pay`) so one instruction's
  several accesses total exactly `mem_ts − cycle`. `AccessKind::Dma` is cost-only
  on the DMA engines' own timeline at Mednafen's `dma_time_thing` values, and a
  **C-bus-endpoint SCU DMA halts both SH-2s** for its paced duration
  (`RecalcDMAHalt`/`SetExtHalt`; `a101f15`) while a pure A↔B transfer halts
  neither. ⚠ **Superseded by [0025](0025-scu-dma-no-cpu-halt.md):** the SCU DMA
  now halts *neither* SH-2 — `drain_dma` copies synchronously and charges 0
  CPU-halt cost. Serialized since save-state v6 (v9 adds `bbus_write_finish`).

## Consequences

- **Easier / fixed:** whole-system phase to within ~1% of the oracle; the
  permanent-SFX-mute bug (the B-bus SCSP read cost was load-bearing); correct DMA
  pacing + the both-CPU halt (the halt was later removed for cycle-accounting
  correctness — see [0025](0025-scu-dma-no-cpu-halt.md)).
- **Cost we accept:** per-access charging is part of the per-instruction fidelity
  overhead, notable in poll-heavy scenes (the timer half of that cost is addressed
  in [0022](0022-event-driven-onchip-timers.md)). Save-state format bumps (v6, v9).
- **Invariant:** bus costs trace to Mednafen `BSC_*` / `scu.inc`, not invented
  numbers — the same discipline as cycle counts tracing to the SH-2 manual
  ([0002](0002-accuracy-over-performance.md)).

## Alternatives considered

- **Coarse per-region waits (the pre-M12 model).** Rejected: too inexact — it
  missed the SCSP read cost (muted SFX) and mispriced DMA writes up to 11×.
- **Model VRAM contention.** Deliberately dropped: the oracle models none, so
  adding it would diverge *from* the reference rather than toward the hardware.
