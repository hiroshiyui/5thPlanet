# 5thPlanet Roadmap

Accuracy-first SEGA Saturn emulator in Rust. Milestones are scoped tightly so
the foundation is solid before the next chip is added.

The full design rationale lives in `/home/yhh/.claude/plans/temporal-strolling-hollerith.md`.

## Overview — component status

Per-chip / per-subsystem implementation progress. ✅ complete · 🟡 partial
(usable core, refinements pending) · 🔶 stub · ⬜ not started.

| Component | Status | Notes |
|-----------|--------|-------|
| SH-2 (SH7604) ×2 core | ✅ | Full ISA, 5-stage cycle model, cache, on-chip peripherals, exceptions |
| Saturn bus + memory map | ✅ | Typed regions, wait states, open-bus default |
| Event-driven scheduler | ✅ | Deterministic; SH-2 ×2 + CD-block entities |
| SMPC | ✅ | Slave hold/release, staged INTBACK, NMIREQ, SNDON/SNDOFF, RTC/region |
| SCU (+ DMA + INTC) | ✅ | 3 DMA channels, interrupt aggregation → master INTC |
| SCU-DSP | ✅ | Full VLIW core (ALU/MUL/buses/jumps/DMA/END), host-wired |
| VDP2 | 🟡 | NBG0–3 + RBG0/1 rotation + VDP1 sprite layer, priority composited; **remaining:** windows, line-scroll, colour calculation, 2-word PNC, larger planes, CRAM modes 1/2 |
| VDP1 | 🟡 | Plotter (all primitives + colour modes), framebuffer erase, draw-end IRQ, VDP2 sprite-layer feed; **remaining:** gouraud, double-buffer swap, draw-end timing |
| MC68EC000 (sound CPU) | ✅ | Full user-mode ISA + exception/interrupt model (`m68k` crate) |
| SCSP | 🟡 | Sound RAM + registers, hosted+scheduled 68k, timers + interrupts, 32-slot PCM engine (pitch/loop/8-16bit); **remaining (M6):** envelope (ADSR), mixer/DAC, audio output, SCSP DSP, MIDI |
| CD-block | 🔶 | HLE host-interface command protocol + "no disc, ready" status; real SH-1 firmware = M7 |
| SH-1 (CD-block CPU) | ⬜ | M7 |
| Cartridge slot | ⬜ | Extension RAM (1 MB / 4 MB), backup-RAM, and ROM carts; cartridge region currently open-bus. M7, per-game config |
| SDL2 frontend | 🟡 | Window + framebuffer upload; **remaining:** audio output, keyboard input |
| Save states | ⬜ | Deferred until the peripheral set stabilises |

**Milestone status:** M1–M3 ✅ · M4 (BIOS splash) ⏸ parked on a cycle-phase
question · M5 (chip-coverage: VDP1 / MC68EC000 / VDP2) mostly done · **M6
(audio) 🚧 active** — SCSP timers + interrupts landed; synthesis next.

## Milestone 1 — Cycle-accurate SH-2 (SH7604) core ✅ complete

Standalone `sh2` library crate validated by unit tests and ROM regressions.

- Full SH-2 ISA (~142 ops): decoder, interpreter, delay slots, exceptions.
- 5-stage pipeline cycle model (load-use stalls, multiply latency, branch costs).
- 4 KiB 4-way cache + on-chip peripherals (INTC, DMAC, DIVU, FRT; BSC/WDT/SCI/UBC stubs).
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

## Milestone 5 — Chip-coverage build-out (VDP1 → MC68EC000 → VDP2) 🚧 active

Turn the remaining presence-stubs into real chips, in the order set by the user —
**VDP1, then MC68EC000, then VDP2**. Each is a self-contained, independently
testable unit. MAME (`/mameref`) is an encoding/behaviour reference only; the
hardware manuals stay authoritative.

| # | Task | Status |
|---|------|--------|
| 1 | **VDP1 plotter** — list walker, all primitives + colour modes, render into the framebuffer | ✅ done |
| 2 | VDP1 finish — erase ✅, SCU sprite-draw-end interrupt ✅, VDP2 sprite-layer compositing ✅; remaining: gouraud, double-buffer swap (FBCR), draw-end timing | 🚧 partial |
| 3 | **MC68EC000** — `m68k` CPU crate ✅ (ISA + exceptions) + SCSP host wiring ✅ (sound RAM/registers, hosted+scheduled 68k, SNDON/SNDOFF). Audio engine = M6 | ✅ done |
| 4 | **VDP2 build-out** — NBG0–3 priority compositing ✅, VDP1 sprite layer ✅, RBG0/1 rotation ✅; remaining: windows, line-scroll, colour calc | 🚧 partial |

### Task #1 — VDP1 plotter (`cargo test -p saturn --test vdp1` → 18 tests)

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
  aggregate). **Remaining (task #2):** gouraud, double-buffer swap, draw-end timing,
  VDP2 sprite-layer compositing.

### Task #3 — MC68EC000 (`cargo test -p m68k` → 64 tests)

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

**Remaining for the chip:** MOVEP, memory shift-by-1, address/bus-error frames
(rare), and — as the **M6 audio milestone** — the SCSP slot/FM engine, the SCSP
DSP, the mixer/DAC, and the timer/sound interrupt sources feeding
`Scsp::raise_interrupt`.

### Task #4 — VDP2 multi-layer compositing (`cargo test -p saturn --lib vdp2` → 31 tests)

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

**Remaining:** the rotation line-coefficient table + dual-parameter windows,
windows, line-scroll, colour calculation (sprite alpha + shadow), 2-word
pattern names, 2×2-cell chars, larger/4×4-page plane sizes, the 8bpp-tile
colour bank, and CRAM modes 1/2.

## Milestone 6 — SCSP audio 🚧 active

The MC68EC000 is complete and hosted (M5 task #3); M6 turns the SCSP into a
sound source.

| # | Task | Status |
|---|------|--------|
| 1 | SCSP timers (A/B/C) + interrupt model (68k IRQ via SCIEB/SCIPD/SCILV, main-CPU sound IRQ via MCIPD/MCIEB → SCU) | ✅ done |
| 2 | Slot engine — 32 PCM slots: phase/pitch (OCT/FNS), loop control, 8/16-bit, interpolation, key-on/off | ✅ done |
| 3 | Envelope generator (ADSR) + total level / pan per slot | pending |
| 4 | Mixer / DAC — sum slots to L/R at the master volume; produce a 44.1 kHz sample stream | pending |
| 5 | SDL2 audio output — feed the sample stream to the frontend | pending |
| 6 | SCSP DSP — 128-step effect microprogram (reverb etc.) | pending |
| 7 | SMPC peripheral data → SDL2 keyboard input (the deferred controller path) | pending |

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

## Later milestones (queued)

- **M7 — CD-block + games.** Real CD-block (SH-1) firmware, CD-ROM image loading,
  first commercial game booting, and the **cartridge slot**: Extension RAM carts
  (1 MB at `0x0240_0000`; 4 MB as two banks at `0x0240_0000` / `0x0260_0000`),
  the backup-RAM cart, and game ROM carts — selected per game, with the cart-ID
  the game probes (1 MB / 4 MB detection). The cartridge region is open-bus
  today; the carts are small RAM/ROM regions + an ID byte (no timing/state),
  needed by games like Street Fighter Zero 3 / KOF '97 that won't run without them.
- **Save states** — versioned serde across the crates, once the peripheral set stabilises.
- **Explicitly never** — JIT / dynarec (accuracy over performance is the project's design axis).
