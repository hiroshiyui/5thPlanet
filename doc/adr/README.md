# Architecture Decision Records

This directory holds **Architecture Decision Records (ADRs)** — short
documents that capture a significant architectural or design decision,
the context that forced it, and the consequences we accept by making it.

5thPlanet is an accuracy-first emulator built one chip at a time, and a
lot of its design is load-bearing in non-obvious ways (the `Bus` stall
contract, the queue-and-drain pattern, why the SCU-DSP is a separate
crate, why we keep a local Yabause build around). `CLAUDE.md` documents
*what* the architecture is; ADRs record *why* it is that way, so a future
contributor (or a future us) doesn't relitigate a settled choice or
quietly undo it.

## Format

We use Michael Nygard's lightweight format — see
[`template.md`](template.md). Each ADR has:

- **Status** — `Proposed` → `Accepted` → (later) `Superseded by NNNN` /
  `Deprecated`.
- **Context** — the forces at play; what made a decision necessary.
- **Decision** — what we chose, stated in active voice ("We will …").
- **Consequences** — what becomes easier and what becomes harder.
- **Alternatives considered** — options weighed and why they lost.

## Conventions

- One file per decision: `NNNN-kebab-case-title.md`, `NNNN` zero-padded
  and monotonically increasing. Never renumber.
- ADRs are **append-only**: once `Accepted`, don't rewrite the decision.
  If it changes, write a *new* ADR that supersedes the old one and flip
  the old one's status to `Superseded by NNNN`.
- Keep them short (a screen or two). Link to code, `CLAUDE.md` sections,
  and `doc/glossary.md` terms rather than duplicating them.
- A new ADR lands in its own `docs(adr): …` commit.

## Index

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-accuracy-over-performance.md) | Accuracy over performance (no JIT/dynarec) | Accepted |
| [0003](0003-bus-returns-stall-cycles.md) | `Bus` returns stall cycles; host owns wait-state math | Accepted |
| [0004](0004-deterministic-deadline-scheduler.md) | Event-driven scheduler with deterministic "smallest deadline wins" | Accepted |
| [0005](0005-queue-and-drain-side-effects.md) | Queue a side effect, drain it at the aggregate | Accepted |
| [0006](0006-scu-dsp-standalone-crate.md) | SCU-DSP is a standalone crate, not a `saturn` module | Accepted |
| [0007](0007-forbid-unsafe-code.md) | Workspace-wide `unsafe_code = "forbid"` | Accepted |
| [0008](0008-frontend-osd-software-composite.md) | Hand-rolled, software-composited frontend OSD | Accepted |
| [0009](0009-physdisc-libcdio-ffi-crate.md) | Live physical-disc reads via a feature-gated libcdio FFI crate | Accepted |
| [0010](0010-hle-direct-boot.md) | Optional HLE direct boot (load the 1st-read program, bypass the BIOS CD loader) | Accepted |

### Decisions worth recording (backlog)

Significant choices already made in code/`CLAUDE.md` that are good
candidates for retroactive ADRs:

- Yabause as a reference oracle for cross-verification (no code derived).
