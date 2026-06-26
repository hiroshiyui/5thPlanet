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

1. **Reproduce deterministically.** A bug you can't replay you can't bisect — and
   an "intermittent" one usually *can* be made deterministic.
   - **"Random" across sessions ≠ non-deterministic.** The core is deterministic
     given (RTC seed + pad stream): single-threaded, no `rand`, the SMPC RTC
     seeded once then cycle-driven. So an interactively-random failure is just
     between-session variation in the RTC seed + human input timing — capture one
     *failing* session and it reproduces every time (a recording of a *good*
     session always passes; you need a recording of a *bad* one).
   - Record/replay input movies (`jupiter` `SAT_INPUT_REC` → `sdbg replay`):
     they capture the RTC seed + per-frame pad so an input-gated screen
     reproduces frame-for-frame. Pass the same cartridge (`--cart`) — state must
     match exactly or determinism breaks.
   - **Snapshot just before the failure** (`sdbg save`) for a fast
     load-and-run-forward repro; sanity-check it re-runs identically N× (same
     master PC / cycle / state). A fix must clear the *timing-independent root*
     against this repro, not just make this one snapshot pass.
   - **Classify before tracing** — is the symptom downstream of the bug? Separate
     a frontend / pacing / render stall from a core / game-state stall (e.g.
     `SAT_MOVIE_PROBE`: frames still advancing while CD state freezes ⇒ core, not
     pacing) so you trace the right subsystem.
   - **Timing-sensitive bisection: use the snapshot, not input-replay.** Replaying
     one recording across code versions *desyncs* — a timing change navigates the
     menus differently and never reaches the bug. Loading the same pre-failure
     snapshot on each variant (revert the suspect commit + rebuild) pins the
     identical state, so only the code difference shows.

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
- **Interrupt-acceptance timing.** The Saturn aggregate forwards SCU/CD interrupts
  to the master once per instruction; that forward must honour the SH-2 rules the
  core already enforces — above all, **never accept an interrupt inside a branch
  delay slot** (`!cpu.next_is_delay_slot()`; leave the edge pending to the next
  boundary). And acceptance consumes only the emulator's internal fresh-assertion
  edge: the SCU `IST` latch is **guest-cleared, not auto-cleared on vector fetch**
  (the ISR reads IST to identify the source). (Sangokushi V's movie stall lived
  here — see below.)

## Case studies (scrubbed)

- **Sangokushi V — two cache purges.** Signature failure = SH-2 cache purges we
  weren't honoring. (1) `Cpu::reset` didn't purge the cache, so an SSHON-re-reset
  slave ran stale code and never relocated → blank menu (`35ce7e8`). (2) A 16-bit
  `MOV.W @CCR` cache-purge fell through (only byte-CCR reached the cache) → stale
  display-list → blank menu buttons (`6215aab`). The second was found by
  *recognizing the cache-coherency class*, after a long symptom-trace did not.
- **Sangokushi V — interrupt in a delay slot.** Its scenario-opening movie stalled
  *intermittently* (interactively "random"). Made deterministic via
  record→replay→snapshot; classified core-not-pacing (`SAT_MOVIE_PROBE` showed
  frames advancing while the CD buffer froze full, `free=0 parts=[0:200]`);
  snapshot-bisected to clear the cache / BFUL / DMA-halt suspects. Forward-tracing
  `SAT_CD_XFER_TRACE` / `SAT_SCU_INT_TRACE` good-vs-bad sectors found a
  `Level0DmaEnd` delivered at `0600094C` — the `nop` delay slot after an `rte`: the
  aggregate was forwarding SCU interrupts before *every* instruction, including
  delay slots, corrupting the CD-DMA completion (no `EndDataXfer` → buffer fills →
  freeze). Fix: gate the forward on `!next_is_delay_slot()`. The full-CD-buffer
  symptom was the downstream victim; the root was one interrupt landing one
  instruction early. **Lesson: a "random" timing bug is still a fixed,
  deterministic instance once captured — and the symptom subsystem is rarely the
  buggy one.**
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
