# 0006. SCU-DSP is a standalone crate, not a `saturn` module

- **Status:** Accepted
- **Date:** 2026-05-25

## Context

The SCU embeds a 32-bit microcoded vector DSP: its own VLIW-ish ISA, 256
words of program RAM, four banks of 64 × 32-bit data RAM, a multiplier,
and an ALU. In implementation effort it's comparable to a sizeable
fraction of the SH-2 core — a whole instruction set with a decoder, an
interpreter, and per-opcode tests. The Saturn BIOS doesn't load DSP
microcode on the boot path, but many 3D games hand matrix math to it, so
we'll need it eventually even though nothing drives it during early
milestones.

The question was where it lives: a module inside `crates/saturn` (next to
the SCU it physically belongs to), or its own workspace crate.

## Decision

We will build the SCU-DSP as its **own workspace crate, `scu_dsp`,
structured exactly like `sh2`** (`regs` / `isa` / `decoder` /
`interpreter`, with per-opcode integration tests). It stands alone with
no dependency on `saturn`. The SCU host wires it in later — writing
microcode through the SCU register window, starting it, and forwarding
its end-flag to the SCU INTC — *when a concrete target microcode program
surfaces*; until then the crate is exercised only by its own tests.

This mirrors the treatment of the SH-2 ([the core is its own map-agnostic
crate](0003-bus-returns-stall-cycles.md)): each substantial processor
core is an independently-testable unit.

## Consequences

- The DSP is testable in isolation, the same way `sh2` is — per-opcode
  unit tests against a known-good model, no Saturn system needed.
- Future host integration is a **wire-up exercise, not a redesign**: the
  ISA/decoder/interpreter already exist; the SCU only needs to feed it
  microcode and read its end state.
- The crate structure is symmetric with `sh2`, so a contributor who knows
  one knows the shape of the other.
- Cost: a crate with **no in-tree consumer yet** (host glue is deferred,
  see `scu_dsp/src/lib.rs`). It must be kept building and tested so it
  doesn't rot before the SCU host needs it.

## Alternatives considered

- **A module inside `crates/saturn`.** Rejected: couples the DSP to the
  bus/system prematurely, makes isolated testing awkward, and breaks the
  "each major core is its own crate" symmetry with `sh2`. There's no
  benefit to co-locating it before the host actually drives it.
- **Defer it entirely until a game needs it.** Rejected: standing it up
  parallel to `sh2` while that pattern is fresh is cheap; retrofitting a
  DSP into a mature `saturn` later would be a redesign, exactly what the
  one-chip-at-a-time discipline avoids.
