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

## Common tracing principles

- **Classify the failure before explaining it.** Decide whether the symptom is
  frontend pacing, audio queueing, render, input, or core emulation. For movie
  stalls, sample frame progress, CD FAD, buffer occupancy, audio queue state, and
  SH-2 PCs before blaming the CD player or renderer.
- **Turn "random" into replayable.** Intermittent title bugs usually come from
  RTC seed, input timing, savestate timing, or interrupt timing. Capture input or
  a pre-failure savestate, then prove repeated headless runs reach the same bad
  state.
- **Trace transitions, not just final hangs.** The final park point is often a
  downstream victim. Find the last successful event and the first missing event:
  completed DMA without `EndDataXfer`, key-on without samples, VDP command list
  published but stale, and so on.
- **Compare good and bad timelines.** A lucky run is evidence. Diff CD commands,
  DMA completions, SCU interrupts, SH-2 PCs, buffer state, and key registers until
  the first semantic divergence appears.
- **Instrument narrowly.** Prefer env-gated probes, watchpoints, breakpoints, and
  per-subsystem traces over broad logging. Remove temporary probes once the root
  is understood.
- **Instrument both ends of a mapping, not just the input — and never gate the
  probe on the value you're validating.** For a coordinate/address transform
  (screen→source, logical→physical) log the *output* next to the input. A probe
  that prints only the source `sy` (or filters on `sy == N`) cannot reveal
  `sy ≠ y`; it will happily confirm a faithful mapping that isn't. (The BIOS
  Memory Manager half-screen doubling hid behind exactly this — see below.)
- **Follow ownership boundaries.** A full CD buffer does not prove the CD block is
  wrong; repeated audio does not prove the frontend is wrong; blank graphics do
  not prove VDP rendering is wrong. Verify whether the subsystem is the cause or
  only the place where the earlier error becomes visible.
- **Anchor fixes in hardware invariants.** Examples: interrupts are not accepted
  in delay slots, SCU `IST` is write-0-clear, cache-through aliases must fold to
  physical addresses, CCR access width matters, and DMA completion timing is part
  of the contract.
- **Rerun the original repro.** A unit test validates the invariant; the captured
  title repro validates that the user-visible failure is actually gone.
- **Document ruled-out hypotheses.** Keep the negative evidence with the case:
  frontend pacing, cache coherency, read-pump behavior, HIRQ latch semantics,
  DMA halt behavior, or any other plausible path already disproven.

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
- **VDP2 per-layer coordinate math (scroll / zoom / line-scroll).** Each NBG/RBG
  layer maps a screen dot to a source dot through whole-layer scroll + fractional
  zoom (`ZMxN` coord-increment) + per-line scroll + per-column vertical cell
  scroll. Read **every** control register — including **SCRCTL (line scroll
  X/Y/zoom)** — before declaring the renderer faithful; one missed register sends
  the whole layer to the wrong source row. Vertical line scroll (LSCY) supplies
  the line's **source-Y base**, not an additive offset over the display line. A
  symptom of "content repeats every N lines / an exact half-screen" is a ~2×
  vertical-sampling-rate error (line-scroll/zoom), *not* VRAM duplication — verify
  the sampling before chasing the bytes. (BIOS Memory Manager — see below.)
- **Interrupt-acceptance timing.** The Saturn aggregate forwards SCU/CD interrupts
  to the master once per instruction; that forward must honour the SH-2 rules the
  core already enforces — above all, **never accept an interrupt inside a branch
  delay slot** (`!cpu.next_is_delay_slot()`; leave the edge pending to the next
  boundary). And acceptance consumes only the emulator's internal fresh-assertion
  edge: the SCU `IST` latch is **guest-cleared, not auto-cleared on vector fetch**
  (the ISR reads IST to identify the source). (Sangokushi V's movie stall lived
  here — see below.)
- **SMPC INTBACK acquisition modes (status vs peripheral).** INTBACK is two
  independent, separately-gated fetches: the **status** phase runs only if
  `IREG0 & 0xF` (RTC/region/SMEM, OREG0 = `0x80`-style status byte), and the
  **peripheral** phase runs if `IREG1 & 0x8`. The `SR_NPE` "more data, await
  CONTINUE" bit is set **only inside the status phase**, so the peripheral
  phase's continue-handshake is required **only when status was also returned**.
  A **peripheral-only INTBACK** (`IREG0 & 0xF == 0`, `IREG1 & 0x8`) returns the
  pad report **directly in OREG0.. with no CONTINUE** — many games poll the pad
  this way every frame. Returning the status phase unconditionally (OREG0 =
  `0x80`) and always arming the staged CONTINUE makes such a game read "no
  controller" and ignore all input. (Panzer Dragoon Zwei lived here; `SAT_SMPCLOG`
  shows the per-frame `IREG0=00 IREG1=08 COMREG=10` poll.)
- **CD-block command decode — MAME model vs Mednafen oracle.** Some CD-block
  command handlers were first written against MAME `saturn_cd_hle.cpp`, but the
  project oracle is Mednafen `cdb.cpp` (LLE↔LLE). For position/timing-sensitive
  commands the two models genuinely differ — e.g. **Seek (0x11)**: MAME
  `cmd_seek_disc` keys FAD-vs-track on `CR1 & 0x80`; Mednafen `COMMAND_SEEK` takes
  a single value `((CR1&0xFF)<<16)|CR2` and resolves FAD-vs-track by the
  `0x800000` marker bit, running a *timed* BUSY→SEEK→PAUSE that updates
  `cd_curfad`. A "stale head position / instant completion" symptom ⇒ diff the
  command body against `cdb.cpp`, not the MAME source. (Panzer Dragoon Zwei lived
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
- **BIOS Memory Manager — line scroll is a base, not an offset (and a near-miss).**
  A BIOS screen rendered doubled: two copies of the menu stacked in one 320×224
  frame. A long trace *wrongly cleared the renderer* — it ruled out char-mode,
  scroll, zoom, window (WCTLA), and interlace (TVMD), but **never read SCRCTL**
  (the per-layer line-scroll control; `0x0E0E` here → vertical line scroll on),
  and its bitmap probe printed the *source* `sy` while filtering on `sy == 168`,
  so "`sy=168 → py=168`" was misread as "`sy = y`, faithful" when it was really
  screen y=84 → sy=168. That sent the investigation chasing "who writes the VRAM
  twice" (write-watch on the bitmap bank) when the VRAM was fine. Root (Codex):
  VDP2 **vertical line scroll (LSCY) supplies the line's source-Y base**, but the
  renderer added it on top of the screen-line counter, sampling `source_y ≈ 2*y`
  and wrapping the 256-line bitmap at 128 → the half-screen repeat. Fix:
  `y_phase = lscy ? y % interval : y` (only the residual phase within the
  line-scroll interval advances by the Y coord-increment, per Mednafen
  `CurYScrollIF`); regression `nbg0_line_scroll_y_replaces_the_screen_line_base`.
  **Lessons:** enumerate *all* per-layer VDP2 control registers (line-scroll
  included) before declaring the renderer faithful; instrument the screen
  coordinate next to the computed source coordinate, and don't gate the probe on
  the value you're validating; a half-screen repeat is a vertical-sampling-rate
  bug, not VRAM duplication.
- **Panzer Dragoon Zwei — input dead (peripheral-only INTBACK).** The game
  reached its title (via the Seek fix below) but accepted **no** controller input
  — START did nothing, no button skipped the FMV — while **VF2 read input fine**.
  Classified to the SMPC INTBACK path with a new `SAT_SMPCLOG` access logger
  (observer-only): PDZ polled `IREG0=00, IREG1=08, COMREG=10` every frame, then
  read **only OREG0** and re-issued, never writing the CONTINUE bit. Our handler
  *always* ran the status phase (OREG0 = `0x80`) and armed the staged CONTINUE,
  so PDZ saw `0x80` where it expected the `0xF1` port byte and gave up. The oracle
  (`smpc.cpp:1217/1250`) gates the status phase on `IREG0 & 0xF` and sets
  `SR_NPE` only there, so a peripheral-only INTBACK returns the pad directly in
  OREG0.. with no handshake. Fix: honour the `IREG0 & 0xF` gate — skip the status
  phase, fill OREG from byte 0, no CONTINUE. Verified by the title advancing to
  the main menu (NEW GAME / OPTIONS) on START, with VF2 + SAN5 input unchanged.
  **Lessons:** "VF2 works, this game doesn't" on a *shared* peripheral path means
  the two games exercise *different sub-modes* of it — log the actual register
  traffic of both and diff; a one-off `SAT_SMPCLOG` was the decisive instrument.
  The symptom subsystem (input) was right, but the bug was an unhandled INTBACK
  acquisition mode, not the pad-report format (which VF2 proved correct).
- **Panzer Dragoon Zwei — a MAME-derived CD command in a Mednafen world.** The
  opening FMV played, then the game bailed to the BIOS CD player. Three competing
  agents (Mednafen oracle-diff / sdbg verdict-trace / CD-status register-audit)
  converged: the post-FMV teardown issues `Seek 1100,0200`, and our CD **Seek
  (0x11) handler was a port of MAME `cmd_seek_disc`** (FAD-vs-track keyed on
  `CR1 & 0x80`, track from `CR2 >> 8`) — a *different model* from the oracle's
  `COMMAND_SEEK` (cdb.cpp:2851), where the seek parameter is a single value
  `((CR1&0xFF)<<16)|CR2` (`0`=Stop, `0xFFFFFF`=Pause, else a seek whose
  FAD-vs-track addressing is the `0x800000` marker bit, resolved in `SeekStart1`).
  PDZ's param `0x000200` (marker clear = track 2 / index 0) hit the bogus track
  arm, which set `track` but **left `cd_curfad` at the stale FMV head FAD** and
  **completed instantly** (no timed BUSY→SEEK→PAUSE). The BIOS disc-validity
  service (ROM `0x3BA6`: OK iff `status != 0xFF && (status & 0x20)`) then sampled
  an unsettled drive → verdict 1 → CD-player relaunch. Fix: route the real-seek
  case through `start_seek(cmd_sp, 0x800000, 0, 0)` so the phase machine runs and
  `cd_curfad` settles at the target (a bare seek's `cur_play_end = 0x800000` makes
  `check_end_met` true on the first sector → PAUSE). Verified by the rendered
  title screen, not a pixel count. It surfaced only now because the FMV itself
  began playing only after the Sangokushi V fixes, and no prior game issued a
  plain *track-form* Seek (BIOS/VF2 set the `0x800000` marker, which the buggy
  `CR1 & 0x80` test happened to satisfy). **Lessons:** when a single chip command
  is documented as a MAME port but the project oracle is Mednafen (LLE↔LLE), diff
  the command body against `cdb.cpp` directly — MAME and Mednafen are genuinely
  different models, not the same logic differently spelled; a "stale position /
  instant completion" symptom on a position-sensitive command points at the
  addressing decode + the missing timed phase machine.

---

See also: CLAUDE.md (Developer tools + the per-crate gotchas), ADR-0017 (the
behavioral-oracle policy), [`boot-blocker-investigations.md`](boot-blocker-investigations.md),
and [`compatible-game-titles.md`](compatible-game-titles.md).
