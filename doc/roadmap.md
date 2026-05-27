# 5thPlanet Roadmap

Accuracy-first SEGA Saturn emulator in Rust. Milestones are scoped tightly so
the foundation is solid before the next chip is added.

The full design rationale lives in `/home/yhh/.claude/plans/temporal-strolling-hollerith.md`.

## Overview — component status

Per-chip / per-subsystem implementation progress. ✅ complete · 🟡 partial
(usable core, refinements pending) · 🔶 stub · ⬜ not started.

| Component | Status | Notes |
|-----------|--------|-------|
| SH-2 (SH7604) ×2 core | ✅ | Full ISA, 5-stage cycle model, cache, exceptions; on-chip INTC/DIVU + live FRT (counts + interrupts), working DMAC (transfers + interrupt), behavioral WDT (SCI/UBC remain storage stubs; BSC wait-state timing a refinement) |
| Saturn bus + memory map | ✅ | Typed regions, wait states, open-bus default |
| Event-driven scheduler | ✅ | Deterministic; SH-2 ×2 + CD-block entities |
| SMPC | ✅ | Slave hold/release, staged INTBACK + digital pad, NMIREQ, SNDON/SNDOFF, live RTC (SETTIME + host-seeded), SETSMEM, configurable region; clock-change/SYSRES still no-op |
| SCU (+ DMA + INTC) | ✅ | 3 DMA channels (direct + indirect, D*AD strides, hardware start factors: VBlank/sprite-end/sound), interrupt aggregation → master INTC; cycle-stealing bus timing a refinement |
| SCU-DSP | ✅ | Full VLIW core (ALU/MUL/buses/jumps/DMA/END), host-wired |
| VDP2 | ✅ | NBG0–3 (full pattern names, 8×8/16×16 cells, H/V flip, 2×2-page planes, 8bpp banks) + RBG0/1 rotation (4×4-page planes, line-coefficient, screen-over) + VDP1 sprite layer, priority composited; colour calc + W0/W1 windows (rect + per-line) + sprite shadow; per-line scroll/zoom + vertical cell scroll; CRAM modes 0/1/2. Sprite window unmodelled (as in the reference) |
| VDP1 | ✅ | Plotter (all primitives + colour modes), framebuffer erase, draw-end IRQ, VDP2 sprite-layer feed, gouraud shading, double-buffer swap (FBCR), cycle-accurate draw-end |
| MC68EC000 (sound CPU) | ✅ | Full ISA incl. MOVEP + memory shift/rotate, exception/interrupt model (`m68k` crate); remaining: address/bus-error frames, precise long-op timing |
| SCSP | ✅ | Hosted+scheduled 68k, timers + interrupts, 32-slot PCM engine, ADSR + TL, mixer/DAC (DISDL/DIPAN), 128-step effect DSP, 44.1 kHz output. (Refinements: effect-return pan, MIDI, master volume) |
| CD-block | 🔶 | HLE host-interface command protocol + "no disc, ready" status; full HLE engine (disc image, buffers/filters, CD-ROM FS) = M7 (active). SH-1 LLE is infeasible (undumped firmware + analog servo) and not pursued — HLE is the model, as in every Saturn emulator |
| Cartridge slot | ⬜ | Extension RAM (1 MB / 4 MB), backup-RAM, and ROM carts; cartridge region currently open-bus. M7, per-game config |
| SDL2 frontend | ✅ | Window + framebuffer, 44.1 kHz audio queue, keyboard → digital pad |
| Save states | ⬜ | Deferred until the peripheral set stabilises |

**Milestone status:** M1–M3 ✅ · M4 (BIOS splash) ⏸ parked on a cycle-phase
question · M5 (chip-coverage: VDP1 / MC68EC000 / VDP2) ✅ — all three complete,
plus a post-M6 fidelity pass that made the SH-2 on-chip peripherals (FRT/WDT/
DMAC), SCU DMA (start factors / indirect / strides), and SMPC (live RTC / region)
behaviorally faithful · M6 (SCSP audio) ✅ · **M7 (CD-block HLE + games +
cartridge slot) 🚧 active** — see the Milestone 7 section for the scoped phases.

## Milestone 1 — Cycle-accurate SH-2 (SH7604) core ✅ complete

Standalone `sh2` library crate validated by unit tests and ROM regressions.

- Full SH-2 ISA (~142 ops): decoder, interpreter, delay slots, exceptions.
- 5-stage pipeline cycle model (load-use stalls, multiply latency, branch costs).
- 4 KiB 4-way cache + on-chip peripherals (INTC, DMAC, DIVU, FRT, WDT behavioral;
  BSC/SCI/UBC register-storage stubs). FRT/WDT activation + working DMAC transfers
  landed in the post-M6 fidelity pass.
- Exception/interrupt dispatch (reset, illegal, slot-illegal, address error, NMI, TRAPA, external).
- ROM regression harness with committed golden state hashes.

`cargo test -p sh2` → 131 tests. (The proposed SingleStepTests corpus was
dropped — no public SH-2 corpus exists; per-opcode unit tests + ROM hashes
cover the same ground.)

## Milestone 2 — Saturn bus, dual SH-2, event-driven scheduler ✅ complete

Pairs the M1 SH-2 with a Saturn-shaped memory map, a second (slave) SH-2, and
a deterministic event-driven scheduler, with the M1 cache wired into the live
fetch/data paths.

- `sh2::Cache` line-data storage + write-through; cached vs cache-through dispatch.
- `SaturnBus` typed region structs + memory-map dispatch (open bus when unmapped).
- `Scheduler` + `SchedEntity` trait; `Saturn` aggregate + dual-SH-2 integration.

`cargo test --workspace` → 156 tests.

## Milestone 3 — SCU, SMPC, VDP2 minimal, SDL2 ✅ scaffolding complete

Stood up the system bridge (SCU + DMA + interrupt aggregator), the slave-release
path (SMPC), the display generator (VDP2 minimal — one NBG layer), the SCU-DSP,
and the SDL2 frontend shell.

- SMPC registers + `SETSL`/`SETSM` slave hold-release.
- SCU registers + 3 DMA channels (synchronous) + interrupt aggregator into the master INTC.
- `scu_dsp` crate — full VLIW DSP core (ALU + X/Y/D1 buses + multiplier, MVI/JMP/LPS/BTM,
  END/ENDI, DMA), wired into the SCU host (PPAF/PPD/PDA/PDD ports), run + DMA driven by the
  Saturn aggregate, raising `Source::DspEnd`. **Fully implemented and integrated.**
- VDP2 register bank + VRAM (512 KiB) + CRAM (4 KiB) + minimal NBG0 renderer (bitmap +
  4-cell tile, 8/16/32 bpp via CRAM).
- SDL2 frontend — window, run loop, framebuffer texture upload (default-on feature).
- BIOS-boot regression test gated on a committed golden hash.

`cargo test --workspace` → 240 tests. **The "SEGA logo on screen" goal is not yet
met** — see M4.

## Milestone 4 — BIOS splash on screen ⏸ parked (pivoted to M5)

Goal: boot a real BIOS to the SEGA logo, confirmed visually via SDL2. M4 closed
the known peripheral gaps the BIOS hits during init; the splash itself is
**blocked on an interrupt-phase/cycle-accuracy question** (below) rather than a
missing chip or spec bug, so work pivoted to M5 chip-coverage. Revisit the
splash once more chips/behaviours are in.

| # | Task | Status |
|---|------|--------|
| 1 | SMPC `INTBACK` — full response (status + staged peripheral protocol, NMIREQ, RESENAB/RESDISA) | ✅ done |
| 2 | CD-block presence stub + host-interface command protocol | ✅ done |
| 3 | VDP1 register + VRAM + framebuffer stub (no rendering — became M5 task #1) | ✅ done |
| 4 | VDP2 register-decode fidelity — renderer reads real registers, not constants | ✅ done |
| 5 | Iterate-to-splash — trace BIOS, fix the next blocker | ⏸ parked (cycle-phase frontier) |
| 6 | Commit splash golden + SDL2 visual confirmation | ⏸ blocked on #5 |

### BIOS-boot debug — outcome

A headless **Yabause** build (then **MAME** v0.287, both kept locally under
`yabref/` / `mameref/`, never committed) were patched to log the master SH-2 PC
stream and diffed against ours instruction-by-instruction (`mameref/resync_diff.py`,
poll-loop-tolerant). This proved the **SH-2 core / cache / SMPC / SCU / bus
correct** (bit-exact for millions of instructions) and pinned each boot blocker
to an exact instruction. Real bugs fixed from it:

- `sh2` — route `CCR` (`0xFFFFFE92`) to the cache (BIOS couldn't enable the I-cache).
- `sh2` — `LDC Rm,SR` / `LDC.L @Rm+,SR` are **not** slot-illegal (only PC-rewriting ops are).
- `saturn` — correct SMPC command codes (INTBACK=0x10, NMIREQ=0x18, …); INTBACK OREG
  layout, execution-time, and SF-settle-**on-read** at the exact completion cycle.
- `saturn` — VDP2 raster timing (`VCNT`/`TVSTAT`) live from the global cycle; correct
  NTSC frame length (`479_151`) + cycle-exact VBlank-IN edge.
- `saturn` — SCU presents **fixed external interrupt vectors** (`0x40 + source`), not
  the SH-2 auto-vector; back SCSP sound RAM (512 KiB) so init's write-verify completes.
- `saturn` — CD-block host-interface command protocol, MAME-aligned HIRQ/report
  behaviour, sub-frame periodic-report timing, and the staged INTBACK peripheral protocol.

**Why it's parked:** the residual divergence is a VBlank interrupt landing one
instruction off, from accumulated **cycle-phase** drift vs the references — not a
per-instruction cost we got wrong. Our SH-2 cycle model is **spec-correct**: `BF`/`BT`
= 3/1 per the SH7604 manual (same as MAME); deterministic hot loops match MAME's
iteration counts **exactly**; only *variable* poll loops differ, and their count
depends on *when* a polled flag flips (phase), not on cycle cost. Matching MAME's
exact poll counts would over-fit to MAME's timing approximations and make us *less*
faithful to the spec. So we stop PC-diffing here; closing the splash is a matter of
finding any genuine *spec* deviation (peripheral/interrupt behaviour vs the hardware
manuals), for which the reference builds + `resync_diff.py` remain available.

**`REVIEW(magic)` audit.** Values tuned to a reference emulator rather than a
datasheet are tagged inline; `grep -rn "REVIEW(magic)" crates` enumerates them
(INTBACK SF-busy timings, `CYCLES_PER_FRAME`, the HBLANK approximation, placeholder
RTC bytes, `OREG10=0x34`, CD Get-HW-Info literals). None gate boot; revisit a tag
only if a divergence implicates it. Spec-grounded values are deliberately not tagged.

### Splash blocker — refined diagnosis (2026-05-27)

Revisiting the park with the loop-collapsed PC diff + live-state probes
(`crates/saturn/tests/trace_boot.rs`, all `#[ignore]`) sharpens the picture and
**revises the "cycle-phase drift" theory above** — the residual is more likely a
**data-path divergence**, not interrupt phase:

- **Boot reaches a stable WRAM park at `0x060108BE`** (a `MOV.W @R3,R3 / CMP/EQ /
  BT` loop) waiting for **`[0x060408A4]`** (16-bit) to change. Disassembled live.
- The watched word **never changes across 600 frames (~10 s emulated)** — far past
  any splash timing — so this is a **genuine stall, not phase jitter / slowness**.
- VDP2 **display is still off** (`TVMD.DISP=0`), so it's strictly **pre-splash**.
- **Interrupts are live and correct**: `imask=0`; the VBlank-IN vector (`0x40`,
  `VBR=0x06000000` → trampoline `0x06000840` → common dispatcher `0x060008F4`)
  **fires every frame**, pushes regs, `JSR @R6` through the per-vector callback
  table, and `RTE`s cleanly. Verified the handler entry is hit ~2×/frame.
- **But every SCU-vector callback is still the BIOS default do-nothing stub**
  (`0x0600083C` = `RTS; nop`). Nothing advances `0x060408A4`, so the park spins
  forever and display is never enabled.
- **Control flow matches MAME** across the entire traceable window (loop-collapsed
  skeletons agree for all ~721 K basic blocks we cover; raw PC streams agree
  bit-exact for ~9.3 M instructions before the first poll-count shift). Since the
  PC paths agree but the installed callback differs, the divergence is a **value
  the BIOS writes into the callback table (or the source feeding it) — invisible to
  PC tracing**, not a control-flow/branch bug.

**Follow-up (2026-05-27, same session):**

- **Install is *not* reached.** The real callback table base is `0x06000900`
  (captured live from the dispatcher, not PC-rel arithmetic); the VBlank-IN slot
  `0x06000A00` holds the stub `0x0600083C` and **never changes over 900 frames
  (~15 s)** — no interrupt callback of any vector is ever installed. So this is
  not a store-*value* bug; the BIOS is blocked *before* the install code.
- **Region is not the gate.** The only BIOS present is the **EUR (PAL)** image,
  but booting it with the matching PAL area code (`0x0C`) instead of the default
  North-America `0x04` gives a **byte-identical stuck trajectory** — same park,
  same frame-by-frame PCs. The splash path is region-independent, as expected.
- **`0x060408A4` is a VBlank frame-counter.** The park is the standard "wait
  until the counter advances past a saved value" idiom; the counter is bumped by
  the (never-installed) VBlank-IN callback. We arrive at this wait loop *without*
  the BIOS having installed that callback.

**So the genuine bug is a control-flow path we take — somewhere after the
~9.3 M-instruction MAME-match point — that skips the callback install.** Caveat:
the existing MAME reference trace (44 M PCs) ends in a BIOS loop at `0x0003200`
and may not itself reach the install/splash, so it can't localise the skip.

**Concrete next step:** regenerate a MAME reference trace long enough to reach the
callback-install + splash, then run a *re-syncing* (LCS / poll-loop-tolerant) diff
— not the line-by-line one — to find the first point our control flow leaves
MAME's. Secondary lead worth checking: our staged-INTBACK **peripheral** response
reports a phantom port-1 pad (`OREG0=0xF1, OREG1=0x02`) where M4's plan was "no
controller" — verify that doesn't send the BIOS down a divergent peripheral path.

## Milestone 5 — Chip-coverage build-out (VDP1 → MC68EC000 → VDP2) 🚧 active

Turn the remaining presence-stubs into real chips, in the order set by the user —
**VDP1, then MC68EC000, then VDP2**. Each is a self-contained, independently
testable unit. MAME (`/mameref`) is an encoding/behaviour reference only; the
hardware manuals stay authoritative.

| # | Task | Status |
|---|------|--------|
| 1 | **VDP1 plotter** — list walker, all primitives + colour modes, render into the framebuffer | ✅ done |
| 2 | VDP1 finish — erase ✅, SCU sprite-draw-end interrupt ✅, VDP2 sprite-layer compositing ✅, gouraud shading ✅, double-buffer swap (FBCR) ✅, cycle-accurate draw-end ✅ | ✅ done |
| 3 | **MC68EC000** — `m68k` CPU crate ✅ (ISA + exceptions) + SCSP host wiring ✅ (sound RAM/registers, hosted+scheduled 68k, SNDON/SNDOFF). Audio engine = M6 | ✅ done |
| 4 | **VDP2 build-out** — NBG0–3 priority compositing ✅, VDP1 sprite layer ✅, RBG0/1 rotation ✅, background fidelity (full PN formats + 16×16 cells + flip + 2×2-page planes + 8bpp banks) ✅, colour calc + W0/W1 windows + sprite shadow ✅, CRAM modes 0/1/2 ✅, per-line scroll ✅, per-line windows ✅, vertical cell scroll ✅, rotation 4×4-page planes + line-coefficient + screen-over ✅, line-zoom ✅ (sprite window unmodelled, as in the reference) | ✅ done |

### Task #1 — VDP1 plotter (`cargo test -p saturn --test vdp1` → 22 tests)

`crates/saturn/src/vdp1/{command,plotter}.rs` turn the address-space stub into a
real plotter. `Command` decodes a 32-byte command-table entry; `Plotter` walks the
list (END / jump / skip / call+return) and rasterises into the 512×256 RGB555 frame
buffer:

- One textured-quad rasteriser (forward-differenced edge walk, 16.16 fixed point)
  backs polygons, distorted/scaled sprites, lines and polylines; normal sprites use
  a direct blit loop. Local coordinates (0xA) + system/user clipping (0x9/0x8).
- All six CMDPMOD colour modes, the no-transparency poly writer, SPD/MESH/end-code,
  and the replace/shadow/half-luminance/half-transparent calc modes.
- A `PTMR` write erases the EWLR..EWRR region (to EWDR), runs the list, latches
  `EDSR.CEF`/`COPR`, and flags the SCU sprite-draw-end interrupt (drained by the
  aggregate).
- **Gouraud shading** (CMDPMOD bit 2): the four CMDGRDA vertex colours are
  interpolated per-edge across the quad rasteriser (bilinear over normal sprites)
  and offset each RGB555 channel by `correction - 16`.
- **Double buffering**: the plotter targets a draw buffer; VDP2 composites the
  display buffer; `FBCR` swaps them at the VBlank edge (automatic 1-cycle mode
  or a manual one-shot change).
- **Cycle-accurate draw-end**: a `PTMR` plot models a draw duration (per-command
  + per-dot), so `EDSR.CEF` reads busy and the SCU sprite-draw-end interrupt
  fires only after the duration — settled by the bus on VDP1 access and by the
  run loop between batches. This closes task #2.

### Task #3 — MC68EC000 (`cargo test -p m68k` → 68 tests)

A new `m68k` crate, structured like `sh2` (`no_std` + alloc, big-endian,
host-owned bus returning `(value, stall)`). The CPU is now essentially
complete for user-mode code plus the interrupt model SCSP needs:

- **Registers** — D0-D7 / A0-A7 with USP↔SSP banking on the S bit, PC, named SR.
- **Effective addresses** — all twelve modes (incl. brief-format index + PC-relative).
- **Instructions** — MOVE/MOVEA/MOVEQ; the ADD/SUB/AND/OR/EOR/CMP families with
  immediate/quick/address/extend (ADDX/SUBX) forms; MULU/MULS, DIVU/DIVS;
  ABCD/SBCD/NBCD; NEG/NEGX/NOT/CLR/TST/TAS; bit ops (BTST/BCHG/BCLR/BSET, static
  + dynamic); EXT/SWAP/EXG; shifts/rotates; MOVEM, LINK/UNLK; BRA/BSR/Bcc/DBcc/Scc,
  RTS, JMP/JSR; MOVE to/from CCR/SR — all with correct NZVCX flags.
- **Exceptions** — TRAP/TRAPV/CHK, zero-divide, illegal + line-A/F, privilege
  violation, STOP/RESET/RTE/RTR, and external-interrupt dispatch (autovector,
  `SR.imask`-gated, level-7 NMI). `raise_interrupt()` is the host's entry point.

Cycle model counts the 68000's 4-clock bus cycle per word; per-instruction timing
tables are a later refinement.

**SCSP host wiring landed** (`crates/saturn/src/scsp/`): the SCSP owns the 512 KiB
sound RAM, the control/slot/DSP register bank, and the hosted 68k. The 68k's
`M68kView` maps sound RAM at 0 and the registers at 0x100000; the SH-2 sees them
at 0x05A0_0000 / 0x05B0_0000 (shared RAM). `Scsp::run` paces the 68k at the
11.2896 MHz SCSP clock from the Saturn run loop; SMPC `SNDON`/`SNDOFF` release /
re-hold it. End-to-end: SH-2 stages a program into sound RAM → SNDON → the
scheduler runs the 68k from it (9 tests).

**Remaining for the chip:** address/bus-error stack frames (rare) and precise
long-operation timing tables. (MOVEP and the memory shift/rotate forms landed
in the post-M6 fidelity pass.) As the **M6 audio milestone**: the SCSP slot/FM engine, the SCSP
DSP, the mixer/DAC, and the timer/sound interrupt sources feeding
`Scsp::raise_interrupt`.

### Task #4 — VDP2 multi-layer compositing (`cargo test -p saturn --lib vdp2` → 63 tests)

The NBG0-only renderer became a per-pixel priority compositor over NBG0–3.
Generalized per-layer register accessors drive it; two register-map bugs found
against MAME were fixed (MPOFN at 0x03C / 2-bit fields — 0x03E is the rotation
MPOFR — and PLSZ at 0x03A). Compositing picks the highest PRINA/PRINB priority
with a non-transparent dot (ties → lower-numbered layer; priority 0 hides a
layer; else the CRAM[0] backdrop). Colour formats: 4bpp/8bpp paletted (tile +
bitmap) and 16bpp RGB555 direct (bitmap); bitmap now uses the hardware width and
characters address as `char_number × cell_bytes`.

The **VDP1 sprite layer** is now composited too: `render_frame` reads the VDP1
frame buffer and `sample_sprite` decodes each word per the SPCTL sprite-type
tables (colour mask + priority shift/mask) into a colour (CRAM palette code, or
RGB555 direct when the MSB is set and SPCLMD is on) and a priority from
PRISA..PRISD; the sprite layer wins priority ties against the NBGs. A real VDP1
plot shows through at its sprite priority.

**Rotation (RBG0/RBG1)** is in too: `rotation.rs` reads the rotation-parameter
table (set A/B) and evaluates the affine screen→plane transform; `sample_rbg`
maps each dot and samples the rotation plane (bitmap or single-page tile). RBG0
(param A, priority PRIR) and RBG1 (param B, N0PRIN) join the race in the default
order sprite > RBG0 > NBG0 > RBG1 > NBG1..3.

**Background fidelity** (done): `sample_tile` now decodes the full pattern-name
set — 1-word (CNSM 12-bit char vs 10-bit char + H/V flip, SPCN/SPLT supplement)
and 2-word (15-bit char + 7-bit palette + flip) — supports 8×8 and 16×16
characters (four 8×8 cells addressed consecutively, per-character H/V flip), and
composes plane sizes 1×1 / 2×1 / 2×2 pages across planes A–D via MPABNn/MPCDNn +
MPOFN with plane-base alignment. 8bpp tiles select a CRAM colour bank from the PN
palette field. New regs accessors (PNCN one-word/CNSM/SPCN/SPLT, per-plane page).

**Colour calc + windows + shadow** (done): the compositor keeps the top two
opaque dots by priority; when the front layer enables colour calc (CCCTL) it
blends with the dot below — ratio/alpha mode (alpha = `(31-CCRT)/31`, from
CCRNA/CCRNB/CCRR) or additive (CCMD). Sprites blend via CCRSA..D selected per
sprite type, gated by SPCCEN + the SPCCCS/SPCCN priority condition. W0/W1
windows: each layer's WCTL byte enables W0/W1 with inside/outside area bits and
AND/OR combine logic; windowed-out dots are suppressed (rectangles from
WPSX/WPSY/WPEX/WPEY, X at half-dot resolution). An MSB-only sprite word on a
shadow-capable sprite type halves the layer beneath.

**CRAM modes + line scroll/windows** (done): palette lookups honour RAMCTL.CRMD
— RGB555 (modes 0/1) or true RGB888 (modes 2/3). NBG0/NBG1 apply per-line scroll
from the SCRCTL/LSTAn table (integer H/V, LSS interval), and W0/W1 can take their
horizontal extent per scanline from the LWTAn line-window table (vertical bounds
still from WPSY/WPEY).

**Rotation completeness** (done): RBG tiles compose the full 4×4-page plane
grid (A..P) with the shared pattern-name decode; the per-line coefficient
table (KTCTL) overrides kx/ky for perspective and flags transparent lines; and
the screen-over modes (RAOVR/RBOVR) repeat or clip the field.

**Remaining:** dual-parameter window selection, the sprite window plane,
per-line zoom, and the minor rotation refinements (coefficient Xp mode, CRAM
coefficient tables, the screen-over pattern).

## Milestone 6 — SCSP audio ✅ complete

The MC68EC000 is complete and hosted (M5 task #3); M6 turned the SCSP into a
sound source. The full chain runs end to end: a sound program on the 68k keys
slots → PCM + ADSR envelope + TL → mixer (DISDL/DIPAN) → optional effect DSP →
44.1 kHz stereo → SDL2; SMPC reports a keyboard-mapped digital pad. (Remaining
refinements: precise effect-return panning, MIDI, master-volume scaling.)

| # | Task | Status |
|---|------|--------|
| 1 | SCSP timers (A/B/C) + interrupt model (68k IRQ via SCIEB/SCIPD/SCILV, main-CPU sound IRQ via MCIPD/MCIEB → SCU) | ✅ done |
| 2 | Slot engine — 32 PCM slots: phase/pitch (OCT/FNS), loop control, 8/16-bit, interpolation, key-on/off | ✅ done |
| 3 | Envelope generator (ADSR) + total level (TL) attenuation per slot | ✅ done |
| 4 | Mixer / DAC — sum slots to L/R via DISDL/DIPAN; produce a 44.1 kHz stream | ✅ done |
| 5 | SDL2 audio output — feed the sample stream to the frontend | ✅ done |
| 6 | SCSP DSP — 128-step effect microprogram (reverb etc.) | ✅ done |
| 7 | SMPC peripheral data → SDL2 keyboard input (digital pad) | ✅ done |

### Task #1 — timers + interrupts (`cargo test -p saturn --lib scsp` → 8 tests)

`ScspCtrl` reacts to control-register writes: three timers tick at the
44.1 kHz sample clock ÷ 2^prescale and pend `SCIPD` bits on overflow; the
asserted 68k IRQ level is derived from `SCIPD & SCIEB` encoded through
`SCILV0..2`, presented level-triggered on `cpu.pending_irq` each instruction
boundary, and acknowledged via `SCIRE`. Timer A also pends the main-CPU sound
interrupt (`MCIPD`/`MCIEB`), forwarded by `Saturn::drain_scsp` to the SCU
`SoundRequest` source. The hosted 68k is now a real interrupt-driven engine.

### Task #2 — slot (PCM) engine

The 32-slot PCM engine plays waveforms from the shared sound RAM. Each slot
has a phase accumulator stepped by an OCT/FNS-derived increment; `slot_sample`
fetches 8-bit (×256) or 16-bit PCM at SA + phase with linear interpolation,
applies the loop mode (no-loop / normal / reverse / ping-pong), and advances.
A `KYONEX` write keys slots on/off by their `KYONB` bit. The envelope (ADSR,
task #3) and the mixer/DAC (task #4) consume these pre-envelope samples.

### Task #3 — envelope generator (ADSR + TL)

Each slot has an ADSR envelope: attack → decay1 → decay2/sustain → release,
with rates cached at key-on and scaled by OCT/KRS/FNS through the SCSP time
tables, a decay-level boundary, and EGHOLD. `eg_advance` runs the state machine
one sample and returns the linear EG × TL multiplier (attack ramps directly,
later phases via a log→linear table; TL is the SCSP per-bit dB attenuation
ladder). Key-off enters the release phase. The mixer (task #4) multiplies
`slot_sample` by `eg_advance` and pans the result to L/R.

### Tasks #4–7 — mixer, output, DSP, input

- **#4 Mixer/DAC:** per sample, each slot's enveloped voice is panned by
  DIPAN and scaled by the direct-sound level DISDL (precomputed gain tables),
  summed across 32 slots, and clamped to a 16-bit stereo pair (`next_sample`).
- **#5 Audio output:** `Scsp::run` buffers the 44.1 kHz stream;
  `Saturn::take_audio` hands it to the SDL2 frontend, which queues it each frame.
- **#6 Effect DSP** (`scsp/dsp.rs`): the 128-step microprogram engine
  (24×13-bit MAC, TEMP/MEMS/COEF, sound-RAM delay line with float PACK/UNPACK).
  Slot effect-sends (ISEL/IMXL) feed its input mix; EFREG folds back into L/R.
- **#7 Input:** the INTBACK peripheral phase reports a standard digital pad
  (`smpc::pad`), driven by the frontend's keyboard mapping.

## Milestone 7 — CD-block (HLE) + games 🚧 active

The CD-block is the last subsystem and the blocker for booting commercial
games. **Approach: HLE** (the established model in every Saturn emulator —
MAME, Yabause, Mednafen). The SH-1's CD-ROM firmware is undumped (on-die mask
ROM) and half its job is an analog servo with no digital ground truth, so
there is nothing to low-level-emulate *against*. We instead model the host
command interface + the buffer/filter/partition engine + the CD-ROM
filesystem, reading sectors from a disc image — which is exactly the surface
the BIOS and games interact with, and fully observable. (This is the one chip
where "never approximate" is satisfied by HLE rather than LLE; the earlier
"SH-1 firmware" framing is superseded.) Modelled against MAME's
`saturn_cd_hle.cpp`.

The current `cd_block.rs` already has the host-register shape (HIRQ/CR1–4,
`cmd_pending==0xF` dispatch, periodic report, the no-disc command subset).
M7 grows it into the full engine, in independently-testable phases:

| # | Phase | Notes |
|---|-------|-------|
| 1 | **Disc image + TOC** | CUE/BIN + raw ISO parser → tracks (Mode 1/2/audio), TOC, FAD↔sector. `Saturn::insert_disc`, frontend `<BIOS> [game]` arg. Real Get TOC / Get Session. |
| 2 | **Buffer/filter/partition core** | 200 blocks, 24 filters (FAD-range / file-id / channel / submode / coding-info), partitions. Reset Selector / Set Filter\* / Get Buffer\* (`0x40`–`0x54`). Pure-logic tests. |
| 3 | **Sector pump + data transfer** | 75/150 Hz read pump (extends `CdBlockEntity`) disc→filter→partition; Get/Get-and-Delete Sector Data (`0x60`–`0x63`) streaming the data port + the SCU-DMA path. **Address-map fix:** the data-transfer port is at `0x2581_8000` (the SCU DMA already special-cases `0x0581_8000`) — outside the current `0x0589_xxxx` window, so the bus needs that region. |
| 4 | **CD-ROM filesystem** | ISO9660 directory parse (PVD at FAD 166, `direntryT` records). Change Dir / Get File Scope / Get File Info / Read File (`0x70`–`0x75`). |
| 5 | **Authentication + game boot** | disc-validity (`0xE0`/`0xE1`) + the "SEGA SEGASATURN" header check; get a real game's IP.BIN / first program running. |

**Deferred within M7:** CDDA audio playback into the SCSP, the MPEG card
(`0x90`+), Move/Copy sector ops, and realistic seek timing — none block game
boot. (The BIOS splash park at `0x060108BA` is a VDP2-only path and may be
independent of the disc; treated as a possible side-benefit, not the M7 goal.)

Also in M7: the **cartridge slot** — Extension RAM carts (1 MB at `0x0240_0000`;
4 MB as two banks at `0x0240_0000` / `0x0260_0000`), the backup-RAM cart, and
game ROM carts, selected per game with the probed cart-ID. Open-bus today;
small RAM/ROM regions + an ID byte, needed by Street Fighter Zero 3 / KOF '97.

## Later milestones (queued)

- **Save states** — versioned serde across the crates, once the peripheral set stabilises.
- **Explicitly never** — JIT / dynarec (accuracy over performance is the project's design axis).
