# 0007. Workspace-wide `unsafe_code = "forbid"`

- **Status:** Accepted
- **Date:** 2026-05-25

## Context

An emulator is overwhelmingly pure computation over owned buffers — there
is no inherent need for `unsafe` in the core. A memory-safety bug here
would be especially nasty: it wouldn't look like a crash, it would look
like an *emulation inaccuracy* (a wrong byte, a corrupted register), and
chasing it would be indistinguishable from chasing a timing bug until far
too late. Rust's compile-time memory safety is a primary reason to use it
for this project, and that benefit only holds if `unsafe` doesn't quietly
creep in "just here, for speed/convenience".

The one place that touches a C library — the SDL2 frontend — is reached
through the `sdl2` crate's safe bindings, not raw FFI, so even the I/O
boundary doesn't need `unsafe`.

## Decision

We will set **`unsafe_code = "forbid"` workspace-wide**
(`[workspace.lints.rust]` in the root `Cargo.toml`, inherited by every
crate). `forbid` is deliberately stronger than `deny`: a local
`#[allow(unsafe_code)]` cannot silently re-enable it. Introducing any
`unsafe` therefore requires changing the **workspace lint policy itself**
— a visible, reviewable `Cargo.toml` change with a written justification,
which reviewers treat as a Critical-severity decision until argued.

## Consequences

- The compiler guarantees memory safety across the entire codebase; a
  whole class of "is this an emulation bug or a UB bug?" investigations is
  ruled out by construction.
- It forces safe alternatives where `unsafe` would have been the lazy
  path. Concrete cases already hit: a trace test could not use
  `std::env::set_var` (an `unsafe` fn in edition 2024) and passes the env
  var in externally instead; the SDL2 frontend drains unrecognized event
  codes through the safe API rather than reaching for FFI.
- Cost: occasionally more verbose or indirect code, and some
  micro-optimizations (raw pointer tricks, uninitialized buffers) are off
  the table. This is acceptable — and consistent — given
  [ADR-0002](0002-accuracy-over-performance.md) already subordinates
  performance to correctness.
- If a future need is genuinely unavoidable (e.g. a hot path that profiles
  as a real bottleneck, or an FFI with no safe wrapper), it gets its own
  ADR superseding the relevant scope of this one — not a quiet `allow`.

## Alternatives considered

- **`deny` instead of `forbid`.** Rejected: `deny` can be overridden by a
  local `#[allow(unsafe_code)]`, so `unsafe` could slip in without a
  policy-level change. `forbid` makes any `unsafe` a deliberate, visible
  event — which is the whole point.
- **Allow `unsafe` freely (the Rust default).** Rejected: discards the
  main safety advantage of using Rust for an emulator, where UB hides as
  inaccuracy.
