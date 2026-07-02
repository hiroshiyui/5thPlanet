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
- **VDP2 colour / CRAM-bank decode (per colour-depth, per pattern-name width).**
  "Wrong colours, right shape" on one layer = a palette/CRAM-bank decode bug, not
  geometry or a dropped transfer. The palette→CRAM-index math differs by colour
  depth AND pattern-name width: 16-colour (4bpp) uses the full palette field
  `<<4`; **256-colour (8bpp) selects the bank from palette bits [6:4] ONLY** (a
  256-entry palette spans CRAM addr [7:0]) → `(palette & 0x70) << 4`; and a
  1-word PN pre-extracts a 3-bit bank while a 2-word PN carries the full 7-bit
  field — so a branch that's correct for one width/depth silently over- or
  under-shifts another. Confirm the bug is in the *decode* (not the source) by
  reconstructing the tile from VRAM + the intended CRAM bank, and isolate the
  layer with the env-gated `SAT_NO_NBG*`/`SAT_NO_SPRITE` suppressors + sdbg
  `vdp2regs`/`cram`. A non-zero 8bpp palette bank is an easy test gap. (GN98's
  team-flag previews — see below.)
- **VDP1 double-interlace (DIE) deinterlace.** A foreground layer that *jitters*
  (oscillates frame-to-frame on a strict 2-frame period, ~tens-of-k px toggling
  while the non-black count barely moves) with a steady background = an interlace
  deinterlace gap, not a swap-timing or content bug. In `FBCR.DIE=1` mode VDP1
  rasterizes the even/odd fields into its two framebuffers on alternating frames
  (the game flips `DIL` each frame), so the two buffers legitimately hold opposite
  fields; the compositor must **weave** them (display line y ← the field-`(y&1)`
  buffer, even field → rows 0,2,4…, odd → 1,3,5…) into one full-height image, not
  line-double whichever field is current. Classify with the `SAT_NO_SPRITE` /
  `SAT_NO_NBG*` suppressors (which layer?), `SAT_VDP1LOG` (does the game submit
  identical commands each frame? → our display, not its content), and sdbg
  `vdp1regs` + `SAT_VDP1FB` (DIE/DIL, per-frame swap/draw). **Pixel-visible →
  needs user motion play-test sign-off** (a static-frame combing preview
  over-predicts motion artifacts). (GN98 menu — see below; cf.
  `p5-per-field-interlace-reverted`.)
- **Interrupt-acceptance timing.** The Saturn aggregate forwards SCU/CD interrupts
  to the master once per instruction; that forward must honour the SH-2 rules the
  core already enforces — above all, **never accept an interrupt inside a branch
  delay slot** (`!cpu.next_is_delay_slot()`; leave the edge pending to the next
  boundary). And acceptance consumes only the emulator's internal fresh-assertion
  edge: the SCU `IST` latch is **guest-cleared, not auto-cleared on vector fetch**
  (the ISR reads IST to identify the source). (Sangokushi V's movie stall lived
  here — see below.)
- **VDP status-flag observability vs the ISR that reads it.** A game's VBlank
  handler often gates on a VDP flag that a frame-boundary event also mutates —
  e.g. **VDP1 EDSR.CEF** (draw-end). The mutation (the VDP1 frame-buffer swap +
  automatic-draw restart, which *clears* CEF for the next draw) must land at the
  raster point the reference uses, relative to the interrupt: Mednafen `SetHBVB`
  defers it to the **first HBLANK after leaving VBLANK** — *after* the VBlank-OUT
  ISR has read CEF, not on the VBlank-OUT edge itself. Doing it on the edge
  clears the flag before the ISR observes it, every frame. When a flag-gated
  loop never advances, check *who clears the flag and when* relative to the
  handler — and instrument the flag's value **at the read PC**, not just at the
  edge. (Greatest Nine '98's "Now Loading" lived here — see below.)
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
- **SMPC SF (status flag) is software-set / hardware-cleared — a COMREG write
  must NOT set it.** On real hardware and in Mednafen (`smpc.cpp`: `SMPC_Write`
  case `0x0F` is just `PendingCommand = V`), a COMREG write only *latches* the
  command; SF is set **only** by the guest writing the SF port (the "pre-write"
  idiom), and the SMPC only ever *clears* SF, at command completion. A guest
  that wants to poll for completion sets SF=1 first; a fast fire-and-forget
  command (SNDON/SNDOFF/SSHON/…) skips the pre-write and SF stays 0. Spuriously
  raising SF on the COMREG write makes such a command read back "busy" — and a
  guest that does a **read-once-or-spin** SF check (a `bt .` self-loop that never
  re-reads, not a re-reading poll) latches forever. (Greatest Nine '98 issues
  SNDOFF with no pre-write, then the `0x06004A7E bt -2` self-loop; `SAT_SMPCLOG`
  shows `COMREG <- 07` immediately followed by `SF -> 01`.) Note SF *busy
  durations* are a separate faithfulness axis — Mednafen models 92 base + per-
  command SMPC clocks, cleared cycle-exactly; ours clears at the next
  between-batch drain (fine for re-reading polls).
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

Each case distils one blocker to symptom → trace → root → lesson. The **CASE#N**
IDs are stable references — cite them from commit messages, the roadmap, or the
forensic records below; append new cases at the end rather than renumbering.

- **CASE#1: Wachenröder — white 3D-battle floor (the unread KTCTL register).** The RBG0
  rotating floor washed near-white. Confirmed it was RBG0 (layer isolation), and
  that *disabling* its additive colour-calc fixed it — but the contradiction was
  that Mednafen's identical additive blend should wash too. Resolution: the
  per-dot line-colour CRAM index for a rotation layer comes from the **coefficient
  word's top byte (bits 30:24) when KTCTL bit 4/12 is set** (Mednafen
  `vdp2_render.cpp:1989` `LB.lc=(coeff>>24)&0x7F`), *not* the LCTA table — and we
  only ever read LCTA (all-zero here → CRAM[0]=light-grey → additive wash). The
  floor coeff `0x34333323` → index 52 → CRAM[52]=dark. Fix: a `CoeffCtx.line_colour`
  bit + `Dot.lc` carrying the per-dot index into the compositor's line-colour
  partner; regression `rbg0_coefficient_table_supplies_the_line_colour_index`;
  golden-safe. **Lessons:** (1) *the trap was an unread control register* — KTCTL
  was never in the register dump, so a faithful blend looked like a bug elsewhere
  (cf. SCRCTL in the BIOS-Memory-Manager miss); enumerate **every** control reg of
  the suspect layer, coefficient-table control included. (2) `sdbg poke` to the
  SH-2 cache-through alias (`0x25F8xxxx`) is **silently dropped** — pokes must use
  the real bus base (`0x05F8xxxx`); always re-dump the reg to confirm a poke landed
  before trusting a poke-based negative. (3) A full-screen wash makes the non-black
  pixel count useless — byte-diff PPMs (`cmp -l`). (4) Three fan-out agents proposed
  three different roots; all but the right one were falsified by live register/poke
  data — adjudicate every agent claim against the oracle + live state, and the user's
  Mednafen screenshot (RBG0 texture *is* drawn, just dark) was the decisive oracle.
- **CASE#2: Sangokushi V — two cache purges.** Signature failure = SH-2 cache purges we
  weren't honoring. (1) `Cpu::reset` didn't purge the cache, so an SSHON-re-reset
  slave ran stale code and never relocated → blank menu (`35ce7e8`). (2) A 16-bit
  `MOV.W @CCR` cache-purge fell through (only byte-CCR reached the cache) → stale
  display-list → blank menu buttons (`6215aab`). The second was found by
  *recognizing the cache-coherency class*, after a long symptom-trace did not.
- **CASE#3: Sangokushi V — interrupt in a delay slot.** Its scenario-opening movie stalled
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
  buggy one.** (Full forensic record — the deterministic-repro recipe and the
  ruled-out evidence table — in *Boot-blocker case files* below.)
- **CASE#4: Greatest Nine '98 — VDP1 draw-end flag cleared before the ISR read it**
  (`9b91689`). Wedged forever at "Now Loading": its CD-loader's VBlank-OUT ISR
  gates on **EDSR.CEF** (VDP1 draw-end). We cleared CEF (via `vdp1.frame_change`
  → `render_list`) on the **VBlank-OUT edge** in `update_video_timing`, but that
  edge's own ISR runs *at* VBlank-OUT — so it read CEF=0 every frame and the
  loader spun (both SH-2s deadlocked; the slave's FTI/ICF poll + the
  un-installed worker callback were **downstream victims**). Mednafen `SetHBVB`
  defers the swap + CEF-clear to the **first HBLANK after leaving VBLANK**
  (`vbcdpending` consumed on the next HBLANK edge); ported as moving
  `frame_change` to the first active scanline (`prev_line==0 && line==1`).
  Golden-safe (same displayed buffer at render time). Found by a 3-agent fan-out
  that converged on the CEF mechanism; a first fix (move to the VBlank-OUT edge)
  "should have worked" but a **windowed EDSR probe** showed the ISR runs at
  VBlank-OUT — a new race against the same ISR — so it took one more line of
  deferral. **Lesson: a 2-CPU flag-wait deadlock — trace UPSTREAM to what stops
  driving the flag (the FTI deadlock was the victim); and when a fix "should
  work" but doesn't, instrument the value AT THE READ SITE.**
- **CASE#5: Greatest Nine '98 — SMPC SF spuriously set on a COMREG write** (`e1a7401`,
  the second GN98 blocker). After the CEF fix, GN98 wedged in a black-screen
  self-loop at `0x06004A7E` (a read-once-or-spin SMPC **SF** check). Root: our
  `queue_command` set `sf=1` on every COMREG write, but SF is software-managed
  (Mednafen `SMPC_Write 0x0F` is just `PendingCommand = V`); GN98 issues SNDOFF
  with no SF pre-write, so the spurious busy made its one-shot check spin
  forever. Fix: drop the `sf=1` from `queue_command`. **GN98 now boots to its
  title.** Found by a 3-agent fan-out (SF-lifecycle + oracle + path-trace) — and
  a cautionary note: one agent ran on the already-fixed tree and concluded "the
  premise is stale," a **concurrency confound** when agents both edit and
  observe; trust the agent that *made* the change + the oracle model, and re-run
  the repro yourself. (See the "SMPC SF is software-set" fertile-gap class above.)
- **CASE#6: Greatest Nine '98 — 8bpp 2-word pattern-name palette over-shift** (`ff6e7a4`,
  an in-game render bug, GN98 now fully playable). The team-select menu's two
  LARGE preview flags (NBG1, 8bpp/256-colour, 2-word PN) rendered as scrambled
  rainbow palettes; the small grid flags (NBG3, 4bpp) were fine — **wrong colours,
  right shape**. Root: `sample_pattern_cell` used the full palette field as
  `<< 8`; for a 2-word PN that field is the full 7 bits, so 0x10 → bank 0x1000,
  which folds (`% 0x1000` in CRAM mode 0) back to bank 0 (a garbage palette). In
  256-colour mode only palette bits [6:4] select the bank → `(palette & 0x70) << 4`
  (bit-identical to Mednafen `vdp2_render.cpp:443` `(palno<<4) & ~((1<<bpp)-1)`).
  Fix: normalise the 1-word 8bpp bank into [6:4] so the CRAM-index path is
  width-uniform. **3-agent fan-out: layer-id (the `SAT_NO_NBG*` toggles proved
  NBG1, not a sprite), source (reconstructed the clean logo from intact VRAM ⇒
  render bug not DMA), oracle (Mednafen renders it correctly; cited the formula)
  — all three independently verified the fix renders the Lions logo.** Lesson:
  "wrong colours, right shape" = a palette/CRAM-bank decode bug; the layer's
  registers were all correct — the bug was the per-depth/per-PN-width CRAM-index
  math. (See the "VDP2 colour / CRAM-bank decode" fertile-gap class above.)
- **CASE#7: Greatest Nine '98 — VDP1 double-interlace field strobe** (`33ccf8a` gated →
  `b1bb3ce` default-on, user-verified). The main menu's foreground (title + menu
  bars) jittered every frame; the VDP2 background was steady. Classified as a
  *temporal* render bug: `fbdump` of consecutive frames showed a strict 2-frame
  toggle (~71412 px change, non-black count barely moving), `SAT_NO_SPRITE`
  localised it to the VDP1 sprite layer, and `SAT_VDP1LOG` showed the game
  submits *identical* commands each frame (→ our display, not its content). Root:
  GN98 runs VDP1 in `FBCR.DIE=1` double-interlace and flips `DIL` each frame, so
  VDP1 rasterizes the even/odd fields into its two framebuffers alternately; our
  compositor line-doubled only the front buffer, so the displayed field
  alternated → strobe. Fix: weave the two fields per display line in
  `render_line` (field-`(y&1)` buffer), matching Mednafen `vdp2.cpp:329`. **My
  first hypothesis — the recent `9b91689` CEF swap-timing fix caused it — was
  FALSIFIED by a revert test (both swap edges strobe); a 4-agent fan-out then
  proved it PRE-EXISTING (bisect to v0.16.0), with the mechanism + oracle agents
  converging on the deinterlace gap.** Lessons: (1) a strict 2-frame toggle with
  a near-constant non-black count = a field/deinterlace bug, not present/absent
  flicker; (2) it's pixel-visible → I gated it, showed before/after, and got the
  user's **motion** play-test before defaulting on (the user found VF2 *smoother*,
  not combed — a static-frame combing preview over-predicts motion). (See the
  "VDP1 double-interlace (DIE) deinterlace" fertile-gap class above; cf. the
  reverted P5 per-field *rendering*, a different, broader mechanism.)
- **CASE#8: Doukyuusei ~if~ — dropped DMA.** The record-select menu was empty because an
  SCU indirect-DMA descriptor-table base pointer was read through an unfolded
  cache-through alias → empty descriptors → the menu-background DMA moved nothing.
  The "control-flow-skip / under-driven" symptom pointed at game logic; the root
  was a silently-dropped bus transfer.
- **CASE#9: Virtua Fighter 2 — audio fidelity.** Silent SFX traced to SH-2→SCSP B-bus
  wait-states being charged 0 (vs the reference's read +48 / write +17), which
  let the game's sound-submit spin-timeout expire before the 68k driver's wake,
  latching a permanent "sound wedged" flag. Found by diffing the timing model
  against the oracle, not by reading the driver.
- **CASE#10: BIOS CD-player BGM silence — an m68k decode bug, not a timing gap**
  (`32662f7`, resolved 2026-06-06). The BIOS CD-player's BGM was silent, and
  because it surfaced during the M12 whole-system cycle-accuracy push the natural
  suspicion was a timing gap. Root: `ADDA.L` / `SUBA.L Dn,An` were mis-decoded as
  `ADDX` / `SUBX` — the two share the bit-8 + bits-5:4==`00` encoding, and the
  `op_addsub` dispatch guard failed to exclude opmode `0b11`, so the address
  register never accumulated and the sound driver's note-ring collapsed to 2
  entries. Found by a **cross-emulator note-ring diff** (the signal
  "oscilloscope", `tools/scope_diff.py`), not by reading the driver; regression
  `m68k/tests/ring_offset_repro.rs`. **Lesson: an audio-silence symptom that
  appears during a timing-accuracy push is not automatically a timing bug — a
  cross-emulator *data* diff (here the 68k note-ring) localised it to a plain ISA
  decode error.** (See the `op_addsub` ADDX/SUBX gotcha in `CLAUDE.md`.)
- **CASE#11: BIOS Memory Manager — line scroll is a base, not an offset (and a near-miss).**
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
- **CASE#12: Panzer Dragoon Zwei — input dead (peripheral-only INTBACK).** The game
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
- **CASE#13: Panzer Dragoon Zwei — a MAME-derived CD command in a Mednafen world.** The
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

- **CASE#14: Super Robot Wars F — the trademark-scene buzz (CD-XA Form-2 delivered as Form-1).**
  A loud saturating buzz during the intro logos; Mednafen played it clean. Two
  waves of three competing agents each (the skill's "always fan out" default):
  wave 1 (SCSP synthesis / slot-register provenance / sample-data) established the
  buzz was a **legit SCSP slot-2 voice** correctly looping a buffer the game's own
  **SH-2 software audio mixer** had filled with full-scale garbage — so the SCSP
  was faithful and the fault was upstream; wave 2 (Mednafen oracle-diff / input-data
  provenance / onset-trigger) triangulated the root. The mixer at PC `0x0608E03C`
  is a **2-tap IIR/ADPCM decoder**: a *control byte* at `[buf+4]` selects a
  coefficient pair (`table[hi_nibble<<2]`, valid entries 0–3); the oracle read a
  valid `0x0-0x3` nibble and **never clamped**, ours read **0xFF** → entry 0xF =
  literal-pool bytes as coeffs (~25000, gain ~98×) → the IIR **ran away** to the
  −32768 floor (it feeds its own prior outputs back, so it self-sustained). The
  control byte was wrong because the game streams **CD-XA Form-2** XA-ADPCM audio
  (Track 2, `MODE2/2352`, subheader submode `0x64`) via the data port + SCU-DMA,
  striding its blocks on **2324 bytes** (DMA count `0x1228` = 2×2324; buffer
  midpoint `dst+2324`), but our `read_filtered_sector` delivered only the Form-1
  **2048** per sector — a **276-byte drift** that walked the control byte off its
  frame. Fix: deliver the full 2324-byte Form-2 payload (raw offset 24) when
  `sectlenin==2048` and submode bit 5 is set, matching Mednafen `cdb.cpp:1353`
  (`SECLEN_2048` + Mode-2 Form-2 → 1162 words); `filter_data` now propagates
  `curblock.size` (the data-port transfer paces on `block.size`, which
  `alloc_block` had seeded with `sectlenin`). Regression
  `form2_sector_delivers_2324_bytes_form1_stays_2048`; goldens + 1237 workspace
  tests pass; buzz gone (peak-abs 32768→11465, negative-clamp fires 879×→0).
  **Lessons:** (1) *the game's own software decoder producing garbage from correct
  code is the divergence signal* — don't reverse-engineer the decoder; pivot to
  what fed it (here, a CD delivery-size fidelity gap); (2) "bytes match the disc"
  proves the FIFO held disc bytes, **not** that the *framing* is right — check the
  stride/offset, not just the values (the onset agent was misled by the byte-match
  into suspecting SH-2 arithmetic; the oracle diff + the DMA-count = 2×2324 broke
  the tie); (3) **0xFF is the dropped/short-transfer signature**; (4) new fertile
  class: **CD-XA Form-2 sector sizing** (`disc::read_sector` still hard-2048).

- **CASE#15: Super Robot Wars F — scenario-movie black video (RGB888 direct-colour
  TILE unimplemented).** The Mazinger Z scenario-intro FMV played BLACK video with
  perfect audio (Mednafen shows both). The audio-fine/video-black split classified
  it instantly as a render/decode gap, not a movie-playback failure. The FULL NBG0
  register dump at the movie frame was the tell: `bgon=0x0001` (NBG0 only), NBG0
  `bitmap=0` **tile** mode, character colour `cmode=4` (**RGB888** direct colour).
  The decoded frame WAS in VRAM (32bpp `80 BE B4 84…` pixels + a 2-word PN table;
  char `0x3EF8×0x20` = the pixel address), so the data path worked — the render
  didn't. Root: `sample_pattern_cell` (the tile sampler, shared by NBG and
  rotation) only handled 4bpp/8bpp; depths 2/3/4 fell through to the 4bpp branch
  and were looked up in a black CRAM → black. Direct colour existed only in the
  *bitmap* path. Fix: add depth 2 (2048), 3 (RGB555), 4 (RGB888) arms mirroring
  the bitmap decode, with the character-number stride scaled per depth (`units =
  1/2/4/8` for 4/8/16/32-bpp — an 8×8 cell spans 0x20/0x40/0x80/0x100 bytes). An
  oracle-reference agent confirmed the stride exactly (Mednafen `vdp2_render.cpp`
  `cidx*(bpp>>2)`) and flagged the RGB transparency test — since fixed (a
  follow-up commit): direct-colour opacity keys on the dot MSB (bit31/bit15) not
  "colour≠0", with TPON forcing opaque, applied to all three direct-colour sites
  (NBG bitmap, NBG/RBG tile, and the easy-to-miss separate `sample_rot_bitmap`);
  six tests that used artificial MSB=0 dots were corrected. Regression
  `tile_rgb888_direct_colour_renders_not_black`; goldens + 1238 tests pass; FMV
  verified by **rendering and looking** (a sky-with-clouds frame). **Lessons:**
  (1) audio-works/video-black ⇒ render gap, and *data-present-in-VRAM* ⇒ our
  *decode*, not the game; (2) dump the FULL layer register set — `cmode=4` was the
  whole answer; (3) a feature tested for bitmaps may be missing for tiles (or vice
  versa) — new fertile class: **direct-colour TILE characters**; (4) `sdbg run
  <N>` diverges (single `run_for` re-anchors the batch grid — it reached the wrong
  screen), so added `SAT_SAVESTATE_AT_FRAME` to capture a *faithful* per-frame
  state that sdbg loads (disc grafted) for full `vdp2regs`/`cram`/`render`
  inspection; (5) render+look, never trust a pixel count for a screen ID.

- **CASE#16: Super Robot Wars F — combat-scene livelock (CD drive timing phase).**
  Entering combat: BGM played, screen black, stalled; Mednafen fine. The longest
  hunt yet (8 agents over 3 waves) because every *value* was correct — the bug
  was **phase**. Chain: the game streams a fragmented file as ~10
  `Seek-pause`/`Seek`/`Play(count≤2)` extents through an 8-channel pipeline; its
  streaming driver has a latent race (a per-frame chunk-submit poll must not run
  between the pickups of an extent's two sectors — request state 0 means both
  "idle" and "mid-request"). Our accumulated drive-timing drift walked the last
  extent's pickup pair across a frame boundary; the driver re-initialized its
  staging request mid-flight; sector 2 overwrote sector 1 in staging; the
  request stayed 0x800 short forever → no completion → no more Plays → livelock
  (slave SH-2 FTI-starved downstream; the 68k kept the BGM going). **Ruled out
  en route** (all verified, kept as negative evidence): CD command/HIRQ
  responses (byte-identical to Mednafen per command), sector data at the FIFO
  *and* the WRAM destination (byte-perfect, game-programmed on-chip DMAC — not
  SCU-DMA), Play range decode, cache staleness, interrupt/wakeup edges (the
  game masks the CD interrupt and polls; the divergent branch was synchronous),
  and the play-end-IRQ/buffer-resume fixes (real gaps, committed, but not this
  root). **Root: three drive-timing infidelities** (fixed `ce0f7a4`): the seek
  formula lacked Mednafen's direction split + conditional extra sector period
  (cdb.cpp:2226-2230); Seek-pause `0xFFFFFF` was instant + dropped to the 60 Hz
  idle report cadence instead of settling BUSY→PAUSE through the phase machine
  at 150 Hz (cdb.cpp:2874-2885) — ×10 per load, the dominant lever; buffer-full
  resume was an instant hop instead of a ~1.23M-CDB timed re-seek
  (cdb.cpp:2454). With faithful timing the loader completes and combat renders
  (verified visually). **Lessons:** (1) when every value matches the oracle but
  behaviour diverges, the divergence is *timing phase* — audit the TIMING MODEL
  constants/formulas against the reference as their own completeness sweep (the
  timing-audit table now lives in the commit + this case); (2) a user-played
  instrumented-oracle session (`mednafen -ss.dbg_mask cdb`, no code changes)
  showed the reference *behaviour shape* (job boundaries mid-play, abandon+
  retry reads) that static code-reading couldn't; (3) games contain latent
  races that hardware timing never trips — "the game is never wrong" includes
  *timing-dependent* game code, so the emulator's job is to keep the phase
  inside the hardware envelope; (4) intervention-testing candidate fixes
  (revert/apply + rerun the repro) separated "real fidelity gap" from "this
  stall's root" twice. Residual (audited, deferred): zero per-command
  processing time, Init parking the head at FAD 150, no STOPPED phase.

---

## Boot-blocker case files

The case studies above distill the *lesson* from each blocker; this chapter keeps
the fuller forensic records — the exact deterministic-repro recipe and the
ruled-out hypothesis table — for the investigations where that detail is worth
retaining. It is also where an **active** blocker would be tracked as a resume
point: **there are none currently** — all four commercial titles are fully
playable ([`compatible-game-titles.md`](compatible-game-titles.md)). References are
diffed against the never-committed local oracles (Mednafen, MAME, Yabause;
[ADR-0017](adr/0017-reference-oracle-policy.md)).

The **Cinepak FILM** path is well-exercised: *Sangokushi V* (eighteen Cinepak FILM
files) drove it through to gameplay first, *Panzer Dragoon Zwei* second. (VF2's
opening movie is **Duck TrueMotion**, a different codec; all are the games' own
SH-2 software run under LLE — no decoder to implement either way.)

### Sangokushi V — scenario-opening movie stall (full record)

Root cause is **CASE#3** (*Sangokushi V — interrupt in a delay slot*) above;
the value retained here is the forensic trail — how an interactively-"random"
stall was made deterministic, and how the field of suspects was cleared.

**Symptom.** The per-scenario opening movie *sometimes* fails to play and **stalls
the emulation**; **resetting usually bypasses it**. The startup intro FMV, title,
and menus all run — this is the per-scenario movie specifically. (`roms/SANGOKUSHI_V.cue`,
KOEI, JP, serial **T-7623G**, BIOS v1.01; no per-game hack in Mednafen → our-side
fidelity gap.)

**Deterministic repro.** The core is deterministic given (RTC seed + pad stream),
both captured by the jupiter `SAT_INPUT_REC` movie — so a recording of a *stalling*
session reproduces it on every headless replay. The interactive "intermittency" is
**not** core non-determinism: it is purely between-session variation in the RTC
seed (host wall-clock at boot) + human pad timing (the only entropy the
single-threaded, `rand`-free core sees).
- `sdbg replay <stall.rec>` parks at `master 002D6B04, CD status=01 fad=52005
  free_blocks=0 parts=[0:200]` — bit-identical to the interactive freeze.
- Fast repro: a savestate ~frame 4300 (just before the read) + ~140 frames with
  **no input** develops the stall; loading it + 400 frames was bit-identical
  **4/4 times** (master PC `002DF042`, slave `002D8E3E`, CD `fad=52005 free=0
  hirq=0FCD`).
- Post-fix: the same pre-stall savestate now crosses the freeze, reaching
  `fad=52604` with the buffer draining normally (`free=175`, `parts=[0:25]`).

**Ruled out (each with evidence).**
| Hypothesis | Verdict | Evidence |
| --- | --- | --- |
| Frontend pacing stall | ❌ | Audio watchdogs (`8ac18cb`) self-heal ≤1.5 s analytically; `SDLMOVIE` frames keep advancing *during* the stall |
| CD read-pump deadlock | ❌ | Buffer-full pause re-arms `sec_prebuf_in` at `cd_block.rs:1648`; resumes once the game frees a block |
| CD protocol gap after `CalcActualSize` | ❌ | The game stopped issuing `EndDataXfer` only after the DMA-end interrupt landed in the `rte` delay slot; delaying SCU interrupt delivery past the slot fixes the same repro |
| BFUL HIRQ latch (`a4df618`) | ❌ | `SAT_BFUL_READ_CLEAR=1` A/B — identical stall |
| **Cache coherency** (SAN5's usual signature) | ❌ | `sdbg caudit`: **0 stale lines on both CPUs**; the game's 182,942 associative purges are all honored |
| SCU DMA-halt removal (`64237d7`) | ❌ | Clean savestate bisection — the DMA-halt-restored build stalls identically |

**Discriminator** — does `SDLMOVIE f=…` keep printing when it stalls? Frames
advancing while `fad`/`parts`/PC stay stuck ⇒ a core CD/game wedge (what happens
here); if `SDLMOVIE` also stopped it would be a frontend pacing/thread stall;
input dying only after a load is the SMPC port-restore (`2a33f47`, addressed). The
fix leaves the SCU edge pending while `next_is_delay_slot()` is true and forwards
it at the next instruction boundary; `IST` stays software-visible until the guest
write-0-clears it. The SH-2 core's `interrupt_not_accepted_inside_delay_slot`
invariant now extends to the SCU forwarding layer.

### Panzer Dragoon Zwei — reference notes

Both fixes are case studies above — the CD **Seek (0x11)** decode (**CASE#13**)
and the peripheral-only **INTBACK** (**CASE#12**). Notes worth keeping:
- `mednaref/src/ss/db.cpp`: **no per-game hack for PDZ** — it boots on generic
  CD-block fidelity, so both blockers were our-side gaps.
- Mednafen's PROBLEMATIC-GAMES list flags PD2 as "relies on illegal/questionable
  VDP2 window settings" — an in-game rendering quirk to watch for, not a boot
  blocker.

---

See also: CLAUDE.md (Developer tools + the per-crate gotchas), ADR-0017 (the
behavioral-oracle policy), and
[`compatible-game-titles.md`](compatible-game-titles.md).
