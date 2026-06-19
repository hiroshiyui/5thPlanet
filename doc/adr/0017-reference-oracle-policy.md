# 0017. Reference emulators are local, never-committed behavioural oracles — no code derived

- **Status:** Accepted
- **Date:** 2026-06-19 (retroactively recorded; the policy is long-standing)

## Context

"Accuracy-first" ([ADR-0002](0002-accuracy-over-performance.md)) is meaningful
only against a reference. The Saturn's tightly-coupled timing has many behaviours
the public hardware manuals do not pin down (CD-block conventions, exact
interrupt phasing, SCSP envelope/monitor edges), and the project's core debugging
technique is an **LLE↔LLE master-PC trace-diff**: run the same real BIOS on ours
and on a reference, and the first genuine divergence is the bug. That requires
the references to be available locally and runnable headless.

The references are existing emulators under copyleft licences:

- **Mednafen / Beetle Saturn** (`mednaref/`) — the game-level accuracy oracle
  (it boots the commercial library); authoritative for M11+.
- **MAME** (`mameref/`) — the low-level / early-boot oracle; authoritative for
  CPU/bus/peripheral mechanics.
- **Yabause** (`yabref/`) — a secondary opinion.

Using their *source* — copying, porting, or vendoring it — would entangle this
project's licensing and dissolve the "independent, clean implementation" stance
that makes it a from-scratch accuracy port rather than a fork.

## Decision

We will treat the reference emulators strictly as **local, gitignored,
never-committed behavioural oracles.** We observe their *behaviour* — PC traces
(`SS_PCTRACE`), command traces (`SS_CDTRACE`), register/memory dumps, and the
cross-emulator signal "oscilloscope" (`Scsp::enable_scope` + `tools/scope_diff.py`
against a matching mednaref hook) — and we implement against the **hardware
documentation** plus those observations. We do **not** copy, derive, translate,
or vendor any emulator source, and the reference trees never enter the repo.

Our code cites the *manual section* or the *reference's observed behaviour*
("matches Mednafen `cdb.cpp` recognition order"), not its source lines. Where two
oracles disagree (the standing MAME-vs-Mednafen CD-block tension), we choose per
the documented divergences (see system-architecture §9, Part C.1) and keep both
the MAME-shaped `bios_boot` golden and the Mednafen game-boot path green.

## Consequences

- **Easier / legally clean:** the project stays an independent implementation with
  no copyleft entanglement; it can be licensed on its own terms. The trace-diff
  and scope-diff methodologies are sanctioned first-class tools, and the
  instrumentation on both sides (ours env-gated, theirs via the documented
  `SS_*` hooks) is the blessed way to chase a divergence.
- **Cost we accept:** contributors must obtain and build the references
  themselves (they are not shipped), and must work around headless quirks
  (`SDL_VIDEODRIVER=dummy`, `-force_module ss` for audio-only discs, the ~8 s
  command-timeout that favours `fflush`ing command traces over buffered PC
  traces). Any behaviour learned from an oracle must be **re-justified** against
  hardware docs before it becomes our code — observation, not transcription.
- **Two-oracle discipline:** MAME and Mednafen genuinely disagree (power-on/reset
  HIRQ, DCHG stickiness, `is_cdrom` semantics); the policy forces an explicit,
  recorded choice rather than silently following whichever was open.

## Alternatives considered

- **Vendor or commit a reference** for reproducible diffing. Rejected: copyleft
  entanglement, and it invites copy-paste of reference logic, forfeiting the
  clean-implementation stance.
- **Derive/port the hard subsystems from a reference** (e.g. translate Mednafen's
  CD-block). Rejected for the same licensing reason; we instead *model* the
  observable behaviour and cite it.
- **Hardware docs only, no emulator oracle.** Rejected: the manuals leave the
  coupled-timing gaps unspecified, and the subtle M11 bugs (FTCSR write-0-clear,
  the `DCHG`-at-`Init` re-raise, the SCU indirect-DMA alias) were findable *only*
  by diffing a real PC/command stream against an LLE reference.
