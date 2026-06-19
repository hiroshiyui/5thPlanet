# 0015. The CD-block is high-level-emulated — the one LLE exception

- **Status:** Accepted
- **Date:** 2026-06-19 (retroactively recorded; the decision dates to M7)

## Context

[ADR-0002](0002-accuracy-over-performance.md) sets the charter: every real chip
is emulated **low-level** (LLE), cycle by cycle — no JIT, no dynarec, no
approximate-cycle shortcut. The Saturn has eight programmable processors, and one
of them is the **SH-1** inside the CD-block, the CD-ROM controller.

The SH-1 cannot be LLE'd, for two independent reasons:

- **Its firmware is undumped.** The CD-ROM controller program lives in on-die
  mask ROM; there is no image to load and run instruction-by-instruction, and no
  legal/known route to one. LLE needs code to execute; there is none.
- **Half its job has no digital ground truth.** A large part of the SH-1's work
  is driving the **analog servo** — spinning the disc at the right CLV velocity
  and steering the optical pickup. That behaviour is mechanical/analog; there is
  no observable *digital* signal to be cycle-accurate *against*. Even with a
  firmware dump, "cycle-accurate" would be undefined for the servo half.

Every Saturn emulator — MAME (`saturn_cd_hle.cpp`), Yabause, Mednafen
(`cdb.cpp`) — high-level-emulates the CD-block for exactly these reasons. The
observable contract is the **host command interface** (HIRQ/CR1–4, the data
ports) plus the block/filter/partition buffer engine and the CD-ROM filesystem —
all of which *are* digital and well-specified.

## Decision

We will **high-level-emulate the CD-block**: model the host-visible command
interface (HIRQ, CR1–4, the 16-bit FIFO + 32-bit SCU-DMA data port), the
200-block pool + 24 filters/partitions, the 75 Hz read pump, the ISO9660
filesystem, disc authentication, and the drive state machine — reading sectors
from a disc image or a live drive (`disc::SectorSource`). We will **not** emulate
the SH-1 core or the servo.

This is the **single, named exception** to ADR-0002's LLE rule. The model is
shaped to MAME `saturn_cd_hle.cpp` and cross-checked against Mednafen
`cdb.cpp`; where timing is not derivable from silicon (e.g. the `DrivePhase::
Startup` ~1 s recognition spin-up, the recognition handshake), we match the
reference's model, not hypothetical hardware. Any *other* "let's HLE subsystem
X" proposal must be argued as its own ADR and clear a high bar — ADR-0010/0011
(HLE direct boot) were tried and removed, and ADR-0012 (HLE the SCSP sound
driver) was rejected, precisely because those subsystems *can* be LLE'd and the
LLE↔Mednafen trace-diff methodology depends on it.

## Consequences

- **Easier / possible at all:** M7 exists. An undumped on-die firmware cannot be
  LLE'd; HLE is the only way to model the CD-block. The model is compact,
  `no_std`-friendly in spirit, and unit-testable (`cd_block.rs` + the
  `cd_block.rs` integration tests) without any silicon reference.
- **Cost we accept:** CD-block *timing* is reconstructed from reference behaviour
  rather than derived from the SH-1's instruction stream. The discipline is to
  model only **host-observable** behaviour and to match the reference where the
  reference is itself a model. Subtle CD-timing fidelity (recognition handshake
  length, periodic-report cadence) is "as faithful as the oracle," not "as
  faithful as hardware."
- **Boundary stays bright:** LLE is the default everywhere else (both SH-2s, the
  68k, both DSPs, VDP1/2 engines). The CD-block is the one carve-out, and naming
  it here keeps the next contributor from either (a) trying to LLE an undumped
  chip, or (b) citing this ADR to justify HLE-ing a chip that *can* be run for
  real.

## Alternatives considered

- **LLE the SH-1.** Impossible: the firmware is undumped on-die mask ROM. And the
  servo half has no digital ground truth even with a dump — "cycle-accurate"
  would be undefined for it. This is not a cost trade-off; it is a hard wall.
- **Skip the CD-block entirely (direct-load programs, bypass the BIOS CD path).**
  That is ADR-0010/0011 (HLE direct boot), which were **removed** — the reference
  oracle is itself LLE, so a valid PC-trace-diff needs the real BIOS driving a
  modelled CD-block, not a bypass.
- **Model less (just enough to read sectors).** Insufficient: the BIOS drives the
  real host protocol (recognition, authentication, the filter/partition engine,
  the filesystem commands); a thinner model desyncs the BIOS recognition state
  machine and never boots a game.
