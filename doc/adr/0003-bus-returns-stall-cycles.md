# 0003. The `Bus` trait returns stall cycles; the host owns wait-state math

- **Status:** Accepted
- **Date:** 2026-05-25

## Context

[ADR-0002](0002-accuracy-over-performance.md) commits us to cycle-accurate
timing, which means every external memory access must account for the
wait states the hardware would incur. But *how many* cycles an access
stalls is not a property of the SH-2 — it's a property of **what's on the
other end of the bus**: BIOS ROM, work RAM, a peripheral register, all at
different speeds, ultimately configurable through the SH7604 BSC. On the
Saturn those numbers differ per region (ROM ~10, low WRAM ~3, high WRAM
~1, …); on a test fixture they're zero.

The SH-2 core (`crates/sh2`) is deliberately standalone — `no_std`, no
I/O, reusable by the M1 unit tests, the ROM regression harness, and the
full Saturn system alike. If the core knew the Saturn memory map and its
wait states, it would stop being a reusable SH-2 and become "the Saturn's
SH-2", and the timing model would be baked in one place instead of
composing across chips.

So we need a boundary that lets the CPU stay ignorant of the memory map
while still accumulating accurate per-access timing.

## Decision

We will make the `sh2::bus::Bus` trait the **sole trust boundary** between
the core and its host, and have **every access return the cycles the CPU
should stall**, with the host computing those cycles:

```rust
fn read8 (&mut self, addr: u32, kind: AccessKind) -> (u8,  u32); // (value, stall)
fn read16(&mut self, addr: u32, kind: AccessKind) -> (u16, u32);
fn read32(&mut self, addr: u32, kind: AccessKind) -> (u32, u32);
fn write8 (&mut self, addr: u32, val: u8,  kind: AccessKind) -> u32; // stall
fn write16(&mut self, addr: u32, val: u16, kind: AccessKind) -> u32;
fn write32(&mut self, addr: u32, val: u32, kind: AccessKind) -> u32;
```

- Reads return `(value, stall_cycles)`; writes return `stall_cycles`. The
  CPU adds the stall to its per-instruction cycle total and otherwise
  doesn't interpret it.
- `AccessKind` (`Fetch` / `Data` / `Dma`) is passed on every access so the
  host can distinguish opcode fetch from data and CPU-driven from
  DMA-driven traffic (for cache decisions and future bus arbitration)
  without the core deciding policy.
- The **host owns all wait-state math.** `saturn::SaturnBus` computes the
  stall per region; a future refinement keys it on real BSC register
  values. The test `harness::MemBus` returns 0.

This keeps the *core's* timing model to what's intrinsic (issue costs +
pipeline interlocks; see [ADR-0002]) and makes wait states a composable
property of the host.

## Consequences

- **The SH-2 core stays reusable and map-agnostic.** The same `Cpu` plugs
  into a flat test RAM or the full Saturn bus with no changes — exactly
  what let the reference-diff run the core against a known-good trace.
- **Timing composes.** Cache miss line-fills sum the four `read32` bus
  stalls; DMA can be charged distinctly via `AccessKind::Dma`; per-region
  Saturn waits live in one place (`SaturnBus`) instead of being smeared
  through the CPU.
- The core does **not** route every access through `Bus`: on-chip
  peripherals (`0xFFFFFE00+`), the cache, and the CCR are handled inside
  `Cpu::mem_*` before the bus is consulted (see `CLAUDE.md`). `Bus` is the
  *external* boundary, not "every load/store".
- Costs we accept: a small per-access tuple/return overhead, and the
  obligation that each host get its wait-state numbers right — a wrong
  stall count is a silent timing bug, not a crash. Saturn's current
  numbers are BSC defaults, explicitly a later-refinement hook.

## Alternatives considered

- **CPU hardcodes Saturn wait states.** Rejected: couples the core to one
  memory map, breaks `no_std` reusability, and centralizes timing in the
  wrong layer.
- **A separate post-hoc timing pass over executed accesses.** Rejected:
  not composable with cache misses / interlocks and forces a second model
  of the access stream; returning the stall inline keeps one source of
  truth per access.
- **Return only the value; host pushes stalls out-of-band.** Rejected: the
  stall belongs to the specific access, so coupling it to the return value
  is the simplest correct shape.
