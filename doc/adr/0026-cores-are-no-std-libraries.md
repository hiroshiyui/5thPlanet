# 0026. Processor cores are `no_std`, library-shaped, dependency-free

- **Status:** Accepted
- **Date:** 2026-06-29 (retroactively recorded; the decision dates to M1 / M5)

## Context

The Saturn has several programmable processors, and each CPU core — the SH-2
(SH7604), the MC68EC000, the SCU-DSP — is in principle reusable outside this
emulator and must be testable without standing up the whole machine.

[ADR-0006](0006-scu-dsp-standalone-crate.md) already split the SCU-DSP into its
own crate (rather than a `saturn` module) for exactly that independence, and
[ADR-0007](0007-forbid-unsafe-code.md) forbids `unsafe` workspace-wide. But
neither captures the rule that a core carries **no `std` / no I/O** dependency. A
core that pulled in `std` (for collections, `env`, files, threads) would couple
its correctness tests to a host environment, bloat the dependency graph for any
library reuse, and blur the trust boundary that the bus-stall contract
([ADR-0003](0003-bus-returns-stall-cycles.md)) rests on — the core is pure compute
that only ever touches the outside world through a trait seam.

## Decision

We will keep the standalone processor cores **`no_std` and library-shaped — pure
compute with no I/O**:

- `sh2` and `m68k` are `#![no_std]` + `extern crate alloc`; `scu_dsp` is
  `#![no_std]` *without* `alloc` (fixed arrays only). **None may use `std`.**
- **No `println!` / `eprintln!` in a core** — tracing belongs in a dedicated
  `debug.rs` (already the `sh2` convention).
- The cores stay **dependency-free by default.** `Serialize` / `Deserialize`
  derives are gated behind an **optional `serde` feature, off by default**, that
  the `saturn` crate turns on when it needs save states
  ([ADR-0018](0018-save-state-design.md)) — a standalone consumer of `sh2` /
  `m68k` / `scu_dsp` pulls in nothing.
- The only I/O boundary a core exposes is its **trait seam** (the `Bus` trait for
  `sh2`, ADR-0003); the host owns all I/O.

A reviewer who sees `use std::…`, a non-optional external dependency, or a
`println!` added to a core crate should treat it as violating this ADR.

## Consequences

- **Easier:** each core unit-tests hermetically (a flat in-memory `Bus` / harness,
  no SDL, no files); the cores are reusable as plain libraries; the dependency
  graph stays tiny; the trust boundary stays sharp.
- **Cost accepted:** some ergonomics are given up — no `std::collections` (use
  `alloc` or fixed arrays), no ad-hoc `println!` debugging (route through
  `debug.rs`), and the serde-feature plumbing (`#[cfg_attr(feature = "serde",
  derive(...))]`, `serde-big-array` for arrays > 32, and a hand-rolled flat-tuple
  codec for `scu_dsp`'s 2-D data RAM since it has no `alloc`).
- **Invariant:** any future core added to the workspace inherits this — `no_std`,
  library-shaped, `serde`-optional.

## Alternatives considered

- **`std` cores.** Rejected: couples tests to the host, bloats the graph for reuse,
  and weakens the `Bus` trust boundary — for no real gain, since `alloc` + the test
  harness already cover every need.
- **Always-on `serde` derives.** Rejected: forces `serde` onto every standalone
  consumer; the feature gate costs only a few `cfg_attr`s and keeps the cores clean.
- **Keep the cores as `saturn` modules** (no separate crates). Rejected for
  `scu_dsp` already by ADR-0006; the same reasoning — independent testability, a
  clean boundary — generalizes to all the cores.
