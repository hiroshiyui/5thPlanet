# 0027. Regression correctness is pinned to deterministic golden fingerprints

- **Status:** Accepted
- **Date:** 2026-06-29 (retroactively recorded; the methodology dates to M4 / M8)

## Context

The core is **deterministic**: single-threaded, no `rand`, the RTC seeded once and
then cycle-driven, stepped by the deadline scheduler
([ADR-0004](0004-deterministic-deadline-scheduler.md)). Given a fixed input a run
reproduces bit-for-bit. That determinism is the precondition for cheap, exact
regression guards.

Two things need guarding against *silent* regression: (a) emulation **output** —
does a change still produce the same boot splash / rendered frame / CPU+memory
state? — and (b) save-state **compatibility** — can an old, or wrong-media, state
file be loaded without corrupting the machine? A naive guard (commit a full
per-register dump, or a whole-framebuffer image, per test) is large, noisy, and
tempts a contributor to *auto-update* the golden on every diff — which silently
erodes the guard it was meant to be.

## Decision

We will pin regression correctness to **deterministic golden fingerprints, not
full dumps**:

- A run's CPU+memory state is fingerprinted with **FNV-1a** (`harness::state_digest`)
  — a stable, platform-independent 64-bit hash. The `bios_boot` test hashes only the
  **active framebuffer region** to a committed golden
  (`tests/golden/bios_splash.hash`, currently `0x0B1BA6E5180766F7`), so it is
  independent of the MAX-buffer dimensions. Per-game render guards pin a non-black
  **pixel count** (e.g. Doukyuusei 143341, VF2 53440).
- **A golden is re-baselined only for an *intended*, understood output change** — in
  its own commit, with the reason stated — **never** auto-updated to turn a red bar
  green. A golden that moves *unexpectedly* is a **finding**, not a chore.
- Save states carry a **magic (`5PSS`) + `u32` version (currently 12) header plus
  FNV-1a BIOS / disc fingerprints**, and a load **rejects** a wrong-magic,
  wrong-version, or wrong-media file rather than corrupting the machine
  ([ADR-0018](0018-save-state-design.md)). The round-trip test asserts determinism:
  snapshot, run both the original and the reloaded machine forward by the same
  budget, assert identical re-snapshots.

A reviewer who sees a golden silently re-baselined with no stated intended-change
reason, or a save-state load that skips the version / fingerprint check, should
treat it as violating this ADR.

## Consequences

- **Easier:** a regression surfaces as a single changed hash / count, tiny in the
  diff; "did this change pixels?" has a yes/no answer; cross-version and cross-media
  save loads fail *safe*.
- **Two halves of one program:** this is the *prevention* half of accuracy work; the
  cross-emulator oracle diff ([ADR-0017](0017-reference-oracle-policy.md)) is the
  *detection* half (find where we diverge from Mednafen) — a golden then locks the
  fix in.
- **Cost accepted:** a golden hash tells you *that* the output changed, not *why* —
  diagnosing a moved golden still needs the instruments (`sdbg`, the trace
  harnesses). A deliberate pixel-visible change requires a human play-test sign-off
  **and** a re-baseline commit (e.g. the colour-offset C7 splash re-baseline).
- **Invariant:** every new subsystem with deterministic output gets a golden or
  fingerprint guard; any save-state format change bumps `VERSION`.

## Alternatives considered

- **Full per-register / full-framebuffer goldens.** Rejected: large, noisy, and they
  invite auto-updating, which erodes the guard.
- **No version / fingerprint header on save states** (trust the file). Rejected: a
  stale or wrong-media load silently corrupts the machine; the header makes it fail
  loudly.
- **Re-baseline goldens automatically on any diff.** Rejected: that defeats the
  entire point of a golden — a regression must be a deliberate human decision, not a
  test-runner convenience.
