# 5thPlanet Roadmap

Accuracy-first SEGA Saturn emulator in Rust. Milestones are scoped tightly so
the foundation is solid before the next chip is added.

The full design rationale lives in `/home/yhh/.claude/plans/temporal-strolling-hollerith.md`.

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
| 3 | **MC68EC000** — new `m68k` CPU crate (SCSP sound CPU), structured like `sh2` | 🚧 in progress |
| 4 | **VDP2 build-out** — NBG0–3 priority compositing ✅, VDP1 sprite layer ✅; remaining: RBG0/1 rotation, windows, line-scroll, colour calc | 🚧 partial |

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

### Task #3 — MC68EC000 (`cargo test -p m68k` → 38 tests)

A new `m68k` crate, structured like `sh2` (`no_std` + alloc, big-endian,
host-owned bus returning `(value, stall)`):

- **Registers** — D0-D7 / A0-A7 with USP↔SSP banking on the S bit, PC, named SR.
- **Effective addresses** — all twelve modes (incl. brief-format index + PC-relative),
  resolved while executing.
- **Instructions** — reset; MOVE/MOVEA/MOVEQ; ADD/SUB/ADDA/SUBA/ADDQ/SUBQ;
  AND/OR/EOR/EXG; CMP/CMPA/CMPM; the immediate group (ADDI/SUBI/ANDI/ORI/EORI/CMPI
  + the -to-CCR/SR forms); CLR/TST/NEG/NOT/EXT/SWAP; MOVE to/from CCR/SR;
  shift/rotate (ASL/ASR/LSL/LSR/ROL/ROR/ROXL/ROXR, register target); BRA/BSR/Bcc,
  DBcc/Scc, RTS, JMP/JSR — all with correct NZVCX flags.

Cycle model counts the 68000's 4-clock bus cycle per word; per-instruction timing
tables are a later refinement. **Remaining:** MULU/MULS, DIVU/DIVS, ABCD/SBCD/NBCD,
bit ops (BTST/BCHG/BCLR/BSET), MOVEM/MOVEP, LINK/UNLK, memory shifts, the exception
model (TRAP, privilege, address error, interrupts), and SCSP host wiring.

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

**Remaining:** RBG0/1 rotation, windows, line-scroll, colour calculation
(sprite alpha + shadow), 2-word pattern names, 2×2-cell chars, larger plane
sizes, the 8bpp-tile colour bank, and CRAM modes 1/2.

## Later milestones (queued)

- **M6 — SCSP + audio.** Finish the MC68EC000 (mul/div/BCD/bit/MOVEM/exceptions),
  the SCSP DSP + mixer, SDL2 audio output, and SMPC peripheral data → SDL2 keyboard
  input. Save states once the peripheral set stabilises.
- **M7 — CD-block + games.** Real CD-block (SH-1) firmware, CD-ROM image loading,
  first commercial game booting.
- **Explicitly never** — JIT / dynarec (accuracy over performance is the project's design axis).
