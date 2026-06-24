# Debugging playbook

How we diagnose emulation bugs in 5thPlanet — especially the boot/render
blockers that stop a commercial title. This is the *workflow* that ties together
the instruments catalogued in CLAUDE.md's **Developer tools** section and the
behavioral-oracle policy in **ADR-0017**. The CLAUDE.md "Project conventions"
bullet on LLE debugging is the one-paragraph version; this is the long form.

## Core principle: in LLE, the game is never wrong — we are

A commercial Saturn title is real SH-2 code that runs correctly on hardware and
on the reference emulators (Mednafen / MAME). So when it misbehaves here, the
defect is an **emulation fidelity gap, not a game-logic puzzle**.

> **Trace _to_ the divergence, then audit the emulation — do not trace _through_
> the game's logic past it.** The instant a trace shows *wrong data coming out of
> correct-looking code*, stop chasing the symptom downstream and pivot to
> auditing the hardware feature at that point.

The failure mode to avoid: spending dozens of probes following the game's
*consumption* of bad data deep into its own state machine. That finds the
symptom; it rarely finds the root. (Real example: a ~30-probe trace through
Sangokushi V's menu-render logic reached only "a display-object state count is 6
instead of 0" — while the root was a dropped cache purge two layers up.)

## The workflow

1. **Reproduce deterministically.** A bug you can't replay you can't bisect.
   - Record/replay input movies (`jupiter` `SAT_INPUT_REC` → `sdbg replay`):
     they capture the RTC seed + per-frame pad so an input-gated screen
     reproduces frame-for-frame. Pass the same cartridge (`--cart`) — state must
     match exactly or determinism breaks.
   - Save states pin a moment for repeated inspection.
   - Sanity-check: same master PC / cycle every run.

2. **Localize the divergence.** Find the first place *our* state differs from the
   oracle — do not start by reading game logic.
   - `sdbg` (the gdb-style REPL over the core): breakpoints, register-guarded
     (`b <addr> <regidx> <val>`), single-step, SH-2 + 68k disasm, memory
     search/probe, CD/SCSP state.
   - **Master-PC trace-diff vs Mednafen** (`sdbg tdiff`): run the full system
     against a Mednafen `SS_PCTRACE` dump; it stops at the first divergent PC and
     rewinds to capture registers + call-chain there.
   - The **cross-emulator signal "oscilloscope"** (`tools/scope_diff.py`):
     samples sound-RAM channels on a shared timebase on *both* emulators and
     reports the first divergent row per channel — the generalization of all the
     one-off trace probes.
   - **Write/read watches** (`SAT_WWATCH` + `SAT_WVAL` / `SAT_RWATCH`): catch who
     writes a value to an address, tagged with the `AccessKind` (CPU store vs
     SCU-DMA vs DMAC vs DSP-DMA) + cycle + PC. The "silently-dropped transfer"
     instrument.

3. **Audit the emulation at the divergence.** Now read *our* handler for the
   feature involved and diff it against the matching reference handler
   (`mednaref/src/ss/*.inc` / `*.cpp`). The reference is observed as a spec,
   never copied (ADR-0017). The question is always: *what hardware behavior are
   we failing to reproduce that makes this correct game code produce wrong
   results?*

4. **Reuse the title's signature failure mode.** Once a game reveals one class of
   bug, check that class *first* on its next blocker before any deep RE — a title
   that trips on one fidelity gap usually trips on the same family again.

## The cardinal rule: instruments must not perturb the core

Every probe is **observer-only and golden-safe** — an env-gated read or a
`#[serde(skip)]` debug field, never a behavioral change. The guards are the
`bios_boot` golden hash and the per-title render goldens
(`crates/saturn/tests/trace_boot.rs`); if an instrument moves them, it has
changed the core and is wrong. *Extend the instrument, don't perturb the core.*

## Fertile gap areas (where blockers cluster)

- **Cache coherency.** The two SH-2 I-caches are not hardware-coherent; software
  keeps them in sync with explicit purges. Verify the purge paths fire for
  *every* access width and trigger: `CCR.CP` via byte **and** word writes, the
  associative-purge space, and the reset purge. (Both Sangokushi V blockers lived
  here.)
- **Sub-word access to on-chip registers.** A byte register read/written as a
  16-/32-bit access has non-obvious byte positioning ("jankiness") — confirm the
  routing handles all widths, not just the common one.
- **Bus-pointer alias folding.** Games program DMA / descriptor pointers through
  SH-2 cache-through aliases (`0x2xxx_xxxx` for `0x0xxx_xxxx`); every such pointer
  must be folded to its physical address before a region-matched bus access, or
  the transfer reads open bus and moves nothing.
- **Inter-CPU handshakes.** The master/slave SH-2s wake each other via the FRT
  input-capture pin and relocate each other via SMPC `SSHON` / `SSHOFF`. Timing
  and cache state around these handoffs are a recurring blocker source.

## Case studies (scrubbed)

- **Sangokushi V — two cache purges.** Signature failure = SH-2 cache purges we
  weren't honoring. (1) `Cpu::reset` didn't purge the cache, so an SSHON-re-reset
  slave ran stale code and never relocated → blank menu (`35ce7e8`). (2) A 16-bit
  `MOV.W @CCR` cache-purge fell through (only byte-CCR reached the cache) → stale
  display-list → blank menu buttons (`6215aab`). The second was found by
  *recognizing the cache-coherency class*, after a long symptom-trace did not.
- **Doukyuusei ~if~ — dropped DMA.** The record-select menu was empty because an
  SCU indirect-DMA descriptor-table base pointer was read through an unfolded
  cache-through alias → empty descriptors → the menu-background DMA moved nothing.
  The "control-flow-skip / under-driven" symptom pointed at game logic; the root
  was a silently-dropped bus transfer.
- **Virtua Fighter 2 — audio fidelity.** Silent SFX traced to SH-2→SCSP B-bus
  wait-states being charged 0 (vs the reference's read +48 / write +17), which
  let the game's sound-submit spin-timeout expire before the 68k driver's wake,
  latching a permanent "sound wedged" flag. Found by diffing the timing model
  against the oracle, not by reading the driver.

---

See also: CLAUDE.md (Developer tools + the per-crate gotchas), ADR-0017 (the
behavioral-oracle policy), [`wip-compatibility-titles.md`](wip-compatibility-titles.md),
and [`compatible-game-titles.md`](compatible-game-titles.md).
