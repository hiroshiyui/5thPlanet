# 0002. Accuracy over performance (no JIT/dynarec)

- **Status:** Accepted
- **Date:** 2026-05-25

## Context

The SEGA Saturn is one of the hardest fifth-generation consoles to
emulate: eight processors (2× SH-2 SH7604, MC68EC000, VDP1, VDP2, SCU +
SCU-DSP, SCSP M68k + SCSP-DSP, SH-1 CD-block) share buses with timing
dependencies that cross chip boundaries. Much Saturn software — and the
BIOS itself — depends on precise inter-chip timing; "close enough"
emulation manifests as games that hang, glitch, or desync in ways that
are extremely hard to diagnose after the fact.

A common alternative is to chase speed first (dynamic recompilation,
high-level emulation of subsystems, approximate per-instruction cycle
counts) and patch accuracy problems as games surface them. That path
trades a correct foundation for runtime performance.

## Decision

We will treat **fidelity as the primary goal and performance as
explicitly subordinate to it.** Concretely:

- The SH-2 core is **cycle-accurate** per instruction, modeling the
  5-stage pipeline interlocks and bus stalls (see `crates/sh2`). Every
  issue cost is traceable to a manual entry — we do not invent cycle
  counts.
- The `Bus` trait returns `(value, stall_cycles)` so wait-state timing
  composes across chips rather than being approximated.
- We will **never** introduce a JIT, a dynamic recompiler, or an
  "approximate cycle" shortcut. A reviewer may reject any change that
  trades correctness for speed on these grounds, citing this ADR.
- The system is built **one chip at a time** across milestones, each
  validated before the next is added, so timing bugs are caught against
  a known-good foundation instead of compounding.

## Consequences

- **Runtime is slow.** The emulator runs far below real-time in debug
  builds; this is accepted. Optimization, if ever pursued, must preserve
  observable timing exactly.
- **The foundation is verifiable.** Because behavior is deterministic and
  cycle-grounded, we can cross-check it instruction-by-instruction
  against a reference emulator — which is exactly how the M4 BIOS-boot
  bugs were pinpointed (the SH-2 core/cache/SMPC/SCU/bus were proven
  bit-exact for millions of instructions; see the M4 #5 notes in
  `doc/roadmap.md`).
- **Some milestones are slow to land** (e.g. cycle-accurate timing makes
  even "boot the BIOS" a deep effort), and that's the deliberate cost.
- This decision is the project's design axis; it constrains every later
  ADR. A decision to relax it would supersede this one and should be
  argued explicitly.

## Alternatives considered

- **Dynarec / JIT for speed.** Rejected: it makes precise, composable
  inter-chip timing far harder and undermines the verifiability that the
  one-chip-at-a-time approach depends on. Out of scope permanently, not
  just "for now."
- **High-level emulation of subsystems** (e.g. HLE BIOS, approximate
  SCU/SMPC). Rejected for the core boot path: the bugs we needed to find
  lived precisely in the low-level timing HLE would paper over.
