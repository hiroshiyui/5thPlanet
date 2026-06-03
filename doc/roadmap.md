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
| VDP2 | ✅ | NBG0–3 (full pattern names, 8×8/16×16 cells, H/V flip, 2×2-page planes, 8bpp banks) + RBG0/1 rotation (4×4-page planes, line-coefficient, screen-over) + VDP1 sprite layer, priority composited; colour calc + W0/W1 windows (rect + per-line) + sprite shadow; per-line scroll/zoom + vertical cell scroll; CRAM modes 0/1/2; hi-res output (320/352/640/704 from TVMD). Sprite window unmodelled (as in the reference) |
| VDP1 | ✅ | Plotter (all primitives + colour modes), framebuffer erase, draw-end IRQ, VDP2 sprite-layer feed, gouraud shading, double-buffer swap (FBCR), cycle-accurate draw-end |
| MC68EC000 (sound CPU) | ✅ | Full ISA incl. MOVEP + memory shift/rotate, exception/interrupt model (`m68k` crate); remaining: address/bus-error frames, precise long-op timing |
| SCSP | ✅ | Hosted+scheduled 68k, timers + interrupts, 32-slot PCM engine, ADSR + TL, mixer/DAC (DISDL/DIPAN), 128-step effect DSP, 44.1 kHz output. (Refinements: effect-return pan, MIDI, master volume) |
| CD-block | 🟡 | HLE engine complete (M7 phases 1–5): disc image (ISO / CUE-BIN / CCD-IMG) + TOC/session, 200-block buffer with 24 filters/partitions, 75 Hz read pump + 32-bit data transfer (SCU-DMA port), ISO9660 filesystem, authentication/region. **M10** adds CDDA→SCSP playback and live physical-disc reads (via the `SectorSource` trait + the `physdisc`/libcdio crate). **M11** adds the SCU external interrupt (vector 0x50, level 7) and a faithful port of Mednafen's `cdb.cpp` **`Drive_Run` drive-phase model** (`d0640a5`): a `DrivePhase` machine (Idle/Startup/SeekStart/Seek/Play/Pause) with the BUSY→SEEK→PLAY status sequence, a one-sector read-ahead pipeline, per-sector `CSCT`, buffer-full backpressure, and the `PauseCounter`-delayed PLAY→PAUSE end so the end IRQ (`PEND` for Play / `EFLS` for Read File) fires with the status already PAUSE — replacing the old immediate `PLAY→PAUSE` pump so the host-visible status sequence and HIRQ edge timeline match the reference, not just the final values. The **recognition spin-up** (`Startup`, `e2884e7`) extends this: a disc present at power-on/insert reports `STATUS_BUSY` for ~1 s before settling to PAUSE (Mednafen `DRIVEPHASE_STARTUP`, `1*44100*256` CD clocks), with a host `Init` guarded not to cancel it mid-spin-up — so the BIOS plays its disc-present **boot animation** (verified vs MAME with an audio CD inserted) instead of jumping straight to the static logo. Remaining: MPEG card, move/copy sector ops. SH-1 LLE is infeasible (undumped firmware + analog servo) — HLE is the model, as in every Saturn emulator |
| Cartridge slot | ✅ | Extension DRAM (1 MB / 4 MB, two banks), battery backup-RAM (odd-byte packing), and game ROM carts, mapped at `0x0200_0000..0x04FF_FFFF` with the probed cart-ID byte at `0x04FF_FFFF`; `--cart=` frontend flag. (CDDA→SCSP, MPEG card, move/copy ops still deferred within M7) |
| SDL2 frontend | ✅ | Window + framebuffer, 44.1 kHz audio queue, keyboard → digital pad, F5/F9 save-state hotkeys |
| Save states | ✅ | Full deterministic snapshot/restore (`Saturn::save_state`/`load_state`, bincode + versioned header). External media (BIOS / disc / ROM cart) referenced not embedded, validated by FNV-1a fingerprint. M8 |
| Backup RAM (battery) | ✅ | Internal 32 KiB backup RAM with hardware odd-byte packing + "BackUpRam Format" default; persisted to a host `.bup` file by the frontend. M8 |
| On-screen menu (OSD) | 🟡 | Hand-rolled, software-composited in-window menu (ADR-0008): Esc opens it; save/load slots, reset, eject/insert disc, quit. M9 Phase 1 done; graphics / controller / region-BIOS / cartridge submenus pending |

**Milestone status:** M1–M3 ✅ · M4 (BIOS splash) ✅ — SEGA splash renders
· M5 (chip-coverage: VDP1 / MC68EC000 / VDP2) ✅ — all three complete,
plus a post-M6 fidelity pass that made the SH-2 on-chip peripherals (FRT/WDT/
DMAC), SCU DMA (start factors / indirect / strides), and SMPC (live RTC / region)
behaviorally faithful · M6 (SCSP audio) ✅ · **M7 (CD-block HLE + cartridge
slot) ✅** — all five CD-block HLE phases (disc/TOC, buffer/filter/partition,
read pump + data transfer, ISO9660 FS, authentication) and the **cartridge
slot** (Extension DRAM / backup / ROM carts + cart-ID) are done. See the
Milestone 7 section. (Deferred within M7: CDDA→SCSP, MPEG card, move/copy
sector ops.) · **M8 (save states + battery-backed backup RAM) ✅** — full
deterministic snapshot/restore (`save_state`/`load_state`, bincode + versioned
header, media referenced not embedded) and a hardware-faithful, host-persisted
internal backup RAM. See the Milestone 8 section. · **M9 (frontend OSD) 🚧
active** — Phase 1 done: a hand-rolled, software-composited in-window menu
(ADR-0008) with save/load slots, reset, eject/insert disc, and quit; Esc opens
it. Graphics / controller / region-BIOS / cartridge submenus are the remaining
phases. · **M10 (live physical disc + CDDA→SCSP) ✅** — the `SectorSource`
trait, CD-audio BGM mixed into the SCSP, and live optical-drive reads via the
feature-gated `physdisc`/libcdio crate (ADR-0009; verified on a real drive with
Virtua Fighter 2). See the Milestone 10 section.

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

## Milestone 4 — BIOS splash on screen ✅ done

Goal: boot a real BIOS to the SEGA logo, confirmed visually. **Achieved
(2026-05-28):** the BIOS boots to the animated SEGA splash (blue ringed-planet
on green, "TM & © 1995 SEGA. All rights reserved."), the framebuffer settles to
a stable image by frame ~285, and the `bios_boot` golden was committed
(`0xF862E76BE919D7A6` at 300 frames). The blocker turned out to be a **single
missing interrupt**, found via a re-syncing PC diff against the verified MAME
reference (see below) — not the "cycle-phase" question we'd parked on.
*(Post-M4, five more fixes completed the renderer so the splash pixel-matches MAME's
bright brushed-metal logo; the golden was retargeted to frame 200 and is now
`0x2C379F92CE1B63F7` — see "Splash now renders fully" below.)*

| # | Task | Status |
|---|------|--------|
| 1 | SMPC `INTBACK` — full response (status + staged peripheral protocol, NMIREQ, RESENAB/RESDISA) | ✅ done |
| 2 | CD-block presence stub + host-interface command protocol | ✅ done |
| 3 | VDP1 register + VRAM + framebuffer stub (no rendering — became M5 task #1) | ✅ done |
| 4 | VDP2 register-decode fidelity — renderer reads real registers, not constants | ✅ done |
| 5 | Iterate-to-splash — trace BIOS, fix the next blocker | ✅ done (missing VBlank-OUT interrupt) |
| 6 | Commit splash golden + visual confirmation | ✅ done (`0xF862E76BE919D7A6`) |

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

**RESOLVED (2026-05-28) — the bug was a missing VBlank-OUT interrupt.** A
re-syncing PC diff (`mameref/resync_diff.py`) against a longer, verified MAME
trace matched our control flow for **31.5 M instructions** then diverged exactly
at the park: MAME exits (`0x060108C4`), we loop. Counting trampoline hits across
the traces showed why — **SCU vector `0x41` (VBlank-OUT) fired 98× in MAME and
0× in ours**; its installed callback (`0x060102AA`, runs `0x06013144`) is what
advances the `0x060408A4` frame counter. `update_video_timing` raised VBlank-IN
on the active→VBLANK edge but only triggered SCU **DMA** factor 1 (not the
**interrupt**) on the VBLANK→active edge. One line — `scu.raise(Source::VBlankOut)`
at that edge (`system.rs`) — ticks the counter, the park exits, the BIOS enables
VDP2 display (`TVMD.DISP` 0→1) and renders the splash. The earlier "install never
reached" probe was misleading: it watched vector `0x40`'s slot (legitimately a
stub); the counter is driven by vector `0x41`.

*Historical (pre-fix) analysis follows; kept for context.* The genuine bug was a
control-flow path that skips the (`0x41`) callback — because the `0x41` interrupt
never fired.

**MAME reference verified to boot fully (2026-05-27).** `mameref/saturn` boots the
*byte-identical* BIOS (USA/EUR/JP `mpr-17933.bin`, SHA1 `faa8ea18…`) past the SEGA
splash to the **"Set Language" setup menu** (snapshot via a Lua `video:snapshot()`
autoboot script + `-seconds_to_run`; screen black at 5 s, menu by 25 s). So the
splash blocker is squarely on **our** side. The earlier worry that the reference
might not reach the splash was wrong — the existing `/tmp/mame_pc.log` (ends in a
BIOS loop at `0x0003200`) was just the **2 s** default of `pctrace.sh`, too short.

**Concrete next step:** regenerate a longer reference trace (`./pctrace.sh 25`, or
a windowed window-of-interest to keep it from ballooning past ~19 M PCs/s), then run
a *re-syncing* (LCS / poll-loop-tolerant) diff — not the line-by-line one — to find
the first point our control flow leaves MAME's. Secondary lead worth checking: our
staged-INTBACK **peripheral** response reports a phantom port-1 pad (`OREG0=0xF1,
OREG1=0x02`) where M4's plan was "no controller" — verify that doesn't send the BIOS
down a divergent peripheral path.

### Post-splash menu transition — root cause fixed (2026-05-28)

The boot parked in a per-frame callback dispatcher (~`0x06028DD4`) waiting for the byte
`[0x060100AB]` to become `1`. That flag is set by the SCU **SMPC interrupt (vector
`0x47`, SCU source 7)**, which was **masked and never delivered** — but the mask was
bogus. **Our SCU register map was off by `0x10` from offset `0x90` up:** IMS was decoded
at `0xB0` (hardware IMS is `0xA0`), IST at `0xB4` (`0xA4`), and the timers at
`0xA0/0xA4/0xA8` instead of `0x90/0x94/0x98`, with a bogus `dsp_ctrl` window eating
`0x90..0x9C`. So the BIOS's *real* interrupt mask (SMPC unmasked) — written to `0xA0` —
was stored in `t0c` and ignored, while the BIOS's A-bus `ASR0` write to `0xB0`
(`0x1FF01FF0`, bit 7 set) landed in our `ims` field and **spuriously masked SMPC**. The
periodic peripheral poll then died after ~11 frames and the flag never advanced.

**Fix** (`crates/saturn/src/scu.rs`, commit `d51cfca`): corrected the map to the SCU
User's Manual / MAME `saturn_scu.cpp` `regs_map` — `0x90` T0C, `0x94` T1S, `0x98` T1MD,
`0xA0` IMS, `0xA4` IST, `0xA8` AIACK, `0xB0/B4` ASR0/1, `0xB8` AREF, `0xC4` RSEL, `0xC8`
VER. All 142 lib + integration tests pass; `bios_boot` golden unchanged (divergence is
post-frame-300). The boot now advances past the park (master PC → `0x0604xxxx`, `BGON`
`0` → `0x1011`, steady BIOS main loop by frame ~1000).

*(The earlier "vec `0x47` not masked, `IMS=0`" note was stale — measured before the
M5/M6 work; the post-fix `IMS=0x1FF01FF0` reading is exactly the misrouted `ASR0` value.
Diagnosis: an env-gated `SMPC_LOG` trace in `system.rs::drain_smpc` showed the periodic
poll stop with `IMS=0x1FF01FF0`; cross-checking that offset against MAME's `regs_map`
exposed the `0x10` shift.)*

### Splash now renders fully — pixel-matches MAME (2026-05-28)

With the boot unblocked, four further fixes took the splash from a frozen dark blob to
the genuine bright brushed-metal "SEGA SATURN" logo, byte-verified against MAME at the
`BGON=0x080C` phase (identical CRAM / VRAM char data / registers → identical pixels):

- **VDP1 automatic-draw, `7842133`** — the splash uses `PTMR` PTM=`0b10` (re-render the
  command list every frame), not a one-shot draw on the register write. We only redrew
  on the write, so the single populated frame buffer ping-ponged under the per-frame
  swap and the logo strobed at 30 Hz. Now `frame_change` re-renders in auto-draw mode.
- **VDP2 8bpp char addressing, `bafa590`** — character numbers are in `0x20`-byte units;
  an 8bpp cell is `0x40` bytes (two units), so the byte base is `char × 0x20`. We used
  `char × 64` and read the wrong VRAM region, so NBG2/NBG3 (the metal layers) drew
  garbage.
- **VDP2 colour-RAM address offset, `ca97b38`** — CRAOFA/CRAOFB (`NxCAOS << 8`) select a
  CRAM bank per layer; NBG3's silver palette is at CRAM `0x300+`. We ignored the offset
  and read the dark bank 0, rendering the logo dark maroon.
- **VDP2 transparent-pen-as-solid, `122db98`** — BGON `NxTPON`/`R0TPON` draw palette code
  0 as the opaque colour `CRAM[offset]` rather than transparent. NBG3 sets it; we always
  treated code 0 as transparent, leaving black speckle in the metal.

`bios_boot` golden retargeted to frame 200 (the stable splash plateau) and tracked the
renders: `0xED48761869D728FD` → `0x871FD74D6C91AF08` → `0x2D966904356AFCC3` →
`0x2C379F92CE1B63F7` (final). Boot speed: the debug build runs ~5–6 fps; `--release`
runs ~74 fps (use it to see the splash quickly).

**Open follow-up:** the post-splash **CD-player "Drive empty" screen** (the `BGON=0x1011`
phase — NBG0 bitmap + RBG0 rotation) benefits from the same VDP2 fixes but hasn't been
re-verified against MAME's snapshot; check it when M7 brings a disc in. A minor ~2×
*emulated-time* slowness in BIOS init (our cycle model charges a touch high) is also
noted for a later look.

## Milestone 5 — Chip-coverage build-out (VDP1 → MC68EC000 → VDP2) ✅ complete

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

## Milestone 7 — CD-block (HLE) + cartridge slot ✅ done

The CD-block is the last core subsystem and the prerequisite for booting
commercial games — the disc-recognition + filesystem + authentication surface
the BIOS drives. (The actual boot-to-gameplay, loading a game's 1st-read program
via the real BIOS, is **M11**.) **Approach: HLE** (the established model in every Saturn emulator —
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
| 1 | **Disc image + TOC** ✅ done | `disc.rs`: raw ISO + CUE/BIN + CloneCD CCD/IMG parsers → a FAD-addressed `Disc` (FAD = LBA + 150), `read_sector` (2352→2048 user payload), and the 102-entry Saturn `toc()` (matches MAME `cd_readTOC`). CD-block holds `Option<Disc>`; `Saturn::insert_disc` + the frontend `<BIOS> [game]` arg load `.iso`/`.cue`/`.ccd`. Get Status/standard reports carry real geometry; Get TOC streams the 408-byte TOC through the data FIFO; Get Session returns the lead-out FAD. Tests: 7 disc-parser + 4 cd-block + 1 gated real-disc (the `roms/` boot disc). |
| 2 | **Buffer/filter/partition core** ✅ done | 200 blocks, 24 filters (FAD-range / file-id / channel / submode / coding-info), partitions. Reset Selector / Set Filter\* / Get Buffer\* (`0x40`–`0x54`). Pure-logic tests. |
| 3 | **Sector pump + data transfer** ✅ done | 75/150 Hz read pump (extends `CdBlockEntity`) disc→filter→partition; Get/Get-and-Delete Sector Data (`0x60`–`0x63`) streaming the data port + the SCU-DMA path. **Address-map fix:** the data-transfer port is at `0x2581_8000` (the SCU DMA already special-cases `0x0581_8000`) — outside the current `0x0589_xxxx` window, so the bus needs that region. |
| 4 | **CD-ROM filesystem** ✅ done | ISO9660 directory parse (PVD at FAD 166, `direntryT` records). Change Dir / Get File Scope / Get File Info / Read File (`0x70`–`0x75`). |
| 5 | **Authentication + region** ✅ done | disc-validity / region report (`0xE0`/`0xE1`) + the "SEGA SEGASATURN" header check, so the BIOS accepts the disc and reads its IP.BIN. (Actually *booting* a game — loading the 1st-read program and reaching its game code — is **M11**, done via the real BIOS; M7 delivers the CD-block surface that boot drives.) |
| 6 | **Cartridge slot** ✅ done | `cartridge.rs`: Extension DRAM (1 MB / 4 MB, two mirrored banks), battery backup-RAM (Saturn odd-byte packing + "BackUpRam Format" tag), and game ROM carts, at `0x0200_0000..0x04FF_FFFF` with the cart-ID byte at `0x04FF_FFFF` (empty slot floats high to `0xFF`). `Saturn::insert_cartridge` + frontend `--cart=ram1m|ram4m|bram[4|8|16|32]|rom:<path>`. 5 tests. |

**Deferred within M7 → done in M10:** CDDA audio playback into the SCSP, and
(beyond M7's image-only model) live physical-disc reads. **Still remaining:** the
MPEG card (`0x90`+), Move/Copy sector ops, and realistic seek timing — none
block game boot. (The BIOS splash park at `0x060108BA` is a VDP2-only path and
may be independent of the disc; treated as a possible side-benefit, not the M7
goal.)

Also in M7 (task #6, ✅ done): the **cartridge slot** — Extension DRAM carts
(1 MB at `0x0240_0000`; 4 MB as two banks at `0x0240_0000` / `0x0260_0000`),
the battery backup-RAM cart, and game ROM carts (`0x0200_0000`), selected per
game with the probed cart-ID byte at `0x04FF_FFFF`. The 4 MB Extension DRAM
cart is what Street Fighter Zero 3 and The King of Fighters '97 require.

## Milestone 8 — Save states + battery-backed backup RAM ✅

Snapshot/restore the whole machine, and persist the console's battery.

| # | Task | Notes |
|---|------|-------|
| 1 | **serde derives (cores)** ✅ done | Feature-gated `Serialize`/`Deserialize` on the sh2 / m68k / scu_dsp state types (off by default; the cores stay dependency-free). Big arrays via serde-big-array; the scu_dsp `[[u32;64];4]` data RAM via a no_std flat-tuple codec. |
| 2 | **serde derives (saturn) + save-state API** ✅ done | Derives across the bus, every peripheral, the scheduler, and `Saturn`. `savestate.rs`: `save_state`/`load_state` over bincode with a magic + version header; external media (BIOS / disc image / ROM-cart bytes) `#[serde(skip)]`'d and re-grafted on load, guarded by FNV-1a fingerprints. 8 tests incl. a snapshot-then-equal-runs determinism check. |
| 3 | **Battery-backed backup RAM** ✅ done | `memory::BackupRam` (32 KiB) with hardware odd-byte packing (matches MAME `backupram_r/w` + the M7 cart) + "BackUpRam Format" default; `Saturn::internal_backup`/`load_internal_backup`. BIOS-boot golden unchanged. |
| 4 | **Frontend** ✅ done | F5 quicksave / F9 quickload to `<bios>.state`; internal backup RAM loaded from / written to `<bios>.bup` (battery) on both builds. |

**Deferred (queued):** save-state migration across `VERSION` bumps (v1 rejects
mismatches), rewind/run-ahead, compressed states, and multiple slots / a
save-state UI.

## Milestone 9 — Frontend OSD (in-window menu) 🚧 active

A hand-rolled, ZSNES/fwNES-style on-screen menu in the SDL2 frontend
(see ADR-0008). Pure-frontend; the core stays UI-agnostic.

| # | Phase | Notes |
|---|-------|-------|
| 1 | **OSD framework + core actions** ✅ done | `jupiter/src/osd/` — `font.rs` (embedded CC0 8×8 font + RGBA `fill_rect`/`draw_text`/`dim` primitives) and `mod.rs` (menu state machine). Software-composited into the 320×224 framebuffer; **Esc** opens it; arrows/Enter navigate. Actions: save/load state to 10 slots (`<bios>.<n>.state`), Reset, Eject/Insert disc, Quit; transient toasts. The module is `sdl2`-free + core-free (`&mut [u8]` buffer + a `Nav` enum), so it's unit-tested without a window. Core add: `Saturn::eject_disc`. **Tests:** 12 OSD (font draw + nav state machine, run even with `--no-default-features`) + 1 cd-block (insert→eject NODISC round-trip). |
| 2 | **Graphics settings** ⬜ | Window scale 1×/2×/3×, fullscreen, integer/aspect scaling. Introduces a serde-backed config file persisted next to the BIOS. |
| 3 | **Controller settings** ⬜ | Keyboard rebind (config-driven map + press-to-bind UI) + SDL2 GameController (gamepad) support. Persisted. |
| 4 | **Region/BIOS + cartridge management** ⬜ | Scan `bios/` and power-cycle into a chosen BIOS + matching `set_region`; cartridge submenu over the `Cartridge` variants via `insert_cartridge`. Persisted. |

**Related fix landed alongside Phase 1:** with no disc the CD-block now reports
status `NODISC` (`0x07`) instead of `PAUSE`, matching MAME's no-image reset.

## Milestone 10 — Live physical disc + CDDA→SCSP audio ✅

The two CD capabilities deferred from M7: playing CD-audio BGM, and reading an
original disc from a host drive (see ADR-0009). The security ring is a non-issue
— our authentication is HLE/header-only.

| # | Phase | Notes |
|---|-------|-------|
| 1 | **`SectorSource` trait** ✅ done | `disc::SectorSource` (+ `TrackInfo`) decouples the CD-block from the in-memory `Disc`: reads fill a caller buffer, so a live drive can back the source and reads are on-demand. `CdBlock.disc` is now `Option<Box<dyn SectorSource>>`; `insert_disc` is generic; save-state media identity uses `fingerprint()`. `SaturnBus`/`CdBlock` drop `Clone`. Pure refactor — suite green, golden unchanged. |
| 2 | **CDDA→SCSP** ✅ done | Audio tracks decode to a CD-DA FIFO in the read pump (2352-byte sector → 588 stereo frames); `Saturn::take_audio` sums it with the SCSP output. Games with CD-audio BGM (e.g. Romance of the Three Kingdoms V) now play their music. 2 tests. Full level for now (SCSP CD-input level/pan deferred). |
| 3 | **Physical drive (`physdisc` + libcdio)** ✅ done | New feature-gated `crates/physdisc`: `PhysicalDisc` impls `SectorSource` via libcdio (TOC + raw sectors + CD-DA), cross-platform. Default = stub (no libcdio); the frontend's `physical-disc` feature + a `cdrom:<device>` disc spec enable it. The crate is the sole ADR-0007 unsafe exception (ADR-0009). Data sectors read through the kernel's cooked block device (no `CAP_SYS_RAWIO`); libcdio handles the TOC + CD-DA. **Verified on a real drive** (Virtua Fighter 2 on `/dev/sr0`): TOC, data, CD-DA, and the emulator boots from the disc — as a normal `cdrom`-group user. |

## Milestone 11 — Boot a game to gameplay 🚧

A discovery milestone (like M4): get a commercial game (*Virtua Fighter 2*, JP
`GS-9079`) past the BIOS CD player. Pursued purely on the **real-BIOS LLE path**,
trace-diffed against Mednafen (itself LLE — the only mode in which a master-SH-2
PC-trace-diff is valid). An opt-in HLE direct boot was tried and **removed** —
see "Removed: the HLE-boot detour" below.

**Status: a commercial game now boots to a correctly-rendered title screen.**
*Doukyuusei ~if~* (NEC InterChannel) boots the real BIOS, loads its 1st-read,
runs game code on both SH-2s, and reaches its **title screen** (「同級生」 "PRESS
START"), displayed at its native **640×224 hi-res**. The chain: the boot
breakthrough (Phase 3, the `DCHG` fix) → both VF2 and Doukyuusei load their
1st-read → Doukyuusei's intro **slave crash** turned out to be a **SH7604 FRT
`FTCSR` write-0-to-clear bug** (Phase 5): the inter-CPU FRT input-capture (FTI)
handshake clears `ICF` by writing 0 to it, which ours (wrongly W1C) ignored, so
`ICF` stuck set, the slave's wait-loop never waited, and it called a master-built
function-pointer table mid-rebuild (null) → fatal halt. With `ICF` clearing
correctly the slave waits properly and the game advances to its title; **VDP2
hi-res rendering** (320/352/640/704) then displays it without the overflow ours'
fixed 320-wide framebuffer caused.

**VF2 (the original M11 target) remains on a separate wall:** its intro demo-
script job engine loops on a **polled CD-state divergence**. This round exhausted
the CD-HIRQ axis for VF2 — the HIRQ *value* matches Mednafen, DCHG matches, and
the newly-added CD-block SCU interrupt is *masked* by VF2 (it polls) — so the
remaining divergence is other polled CD state (the CR1–4 status report / partition
block-count) or residual scheduler timing. The interactive debugger
(`crates/debugger`/`sdbg`), extended again this round, drives both investigations.

| # | Phase | Notes |
|---|-------|-------|
| 1 | **CD-block boot fixes** ✅ done | Trace-diff vs MAME found the disc was rejected before booting. Fixes: the data-transfer state machine (persistent TRANS bit + `xfer_done` count; End Data Transfer reports the real word count instead of "nothing"), `play_data` matching drive state with `PERI` masked, the disc-change one-shot + cold-boot DCHG, and the previously-unhandled `Seek (0x11)` + Init drive-state reset. VF2 now authenticates, passes the region check, reads IP.BIN, and shows the SEGA license screen. |
| 2 | **Frontend region + boot-debug toolkit** ✅ done | Region auto-detect (`SAT_REGION` / BIOS filename) so a JP disc boots on the JP BIOS; a full disc image (`cdrdao` CUE/BIN, all 34 tracks) as the deterministic target; headless hooks `CD_TRACE` / `SAT_PCTRACE` / `SAT_DISASM` / `SAT_BP` / `SAT_FBP` / `SAT_INLOOP` (full-speed PC trace + breakpoint capture; `SAT_SHELL_BASE` tunes the give-up detection) / `SAT_BIOSTRACE` / `SAT_WWATCH` / `SAT_IRQ_DUMP` / `SAT_INTC_TRACE` / `SAT_MEMDUMP` / `SAT_DUMP`. |
| 3 | **LLE trace-diff vs Mednafen** ✅ done — VF2 + Doukyuusei ~if~ boot to game code | Both run the **same BIOS CD-boot loader**; auth, region, and the IP.BIN read all pass. Our loader rejected the disc and re-recognized (`GetToc`/`Init` → CD player) where Mednafen reads the 1st-read (`ChangeDir`→`GetFileScope`→`ReadFile` → game). **Root: the CD-block re-raised `DCHG` (Disc Changed, HIRQ `0x20`) on the first `Init` after recognition** — the internal `disk_changed` latch was cleared only *inside* the Init handler, so that Init reported a fresh disc swap and the BIOS looped recognition forever. **Fix: clear `disk_changed` when the host write-1-to-clear-acknowledges `DCHG`** (matching Mednafen, which clears it once during recognition and never re-raises it at Init). **The method that cracked it:** the BIOS code is identical on both LLE sides, so the root had to be a differing CD-block response → a **command-level CD trace-diff** (detailed `cmd_log`: caller PC + CR1–4 in→out + HIRQ in→out, poll bursts collapsed) showed the streams match exactly through the 19th command (2nd `GetSession`), then the *only* state difference was the HIRQ — ours' Init left `DCHG` set (`0FC4→0FE5`) where Mednafen left it clear (`0F84`). With the fix, ours' command stream matches Mednafen and the master reaches VF2's load address `0x06004000` and executes game code (frame 653); Doukyuusei ~if~ likewise reaches `ReadFile`. `bios_boot` golden unchanged, full saturn suite green. (Earlier notes — state-diff beats the unreliable PC-diff; the CR1 `is_cdrom` fix — stand but weren't the decider; the *general recipe* is: find the first differing **input**, not the first differing behavior.) |
| 4 | **Post-boot run + blocker localization** 🚧 localized to the intro job engine | After Phase 3 the game runs but stalls mid asset-load (display off). Fixes: **BCR1 bit-15 master/slave** (`1f584d6`) — an `SSHON`-released slave takes the BIOS slave path instead of re-initialising work RAM over the running game (it was clobbering the loaded code → master fault); **`run_frame` = one `run_for(CYCLES_PER_FRAME)`** (`0b78733`) — the old active/VBLANK split re-anchored `run_for`'s batch grid and diverged the master's execution from the headless path. A **Mednafen dev-build** (`--enable-dev-build`, `-ss.dbg_mask cdb`) **CD trace-diff** then proved the CD command/response stream is **byte-identical to Mednafen** through the divergence and the data transfer is correct by construction; the game's next-file decision diverges on **timing** (stall point oscillates FAD 2596 ↔ 7772 with each timing change). **Mednafen scheduler alignment Phase 2** landed — **2A** master-leads-slave interleave (`b583cc4`), **2B** per-instruction SCU interrupt sampling (`70f4049`) — correct accuracy gains, `bios_boot` golden unchanged, but not sufficient alone. An evidence-driven cadence alignment followed (`cacffca`): the CD periodic/`SCDQ` report now fires **per-sector during PLAY** (matching Mednafen's `17712`-clock reload — was a flat ~60 Hz), found via the dev-build trace-diff — it advanced VF2's load further (stall moved 2596 → 7772). A later **file-load comparison** (full ordered Play streams, both sides) showed the next-file divergence is **not** timing-sensitive selection: ours loads the **same** intro files as Mednafen (`SOUNDS`/`SNDDAT`/`SLAU`) then **loops** instead of advancing. The loop is VF2's **intro demo-script job engine** (a linked list of timed jobs on the master at `0x06047xxx`; processor `0x06047718`, producer `0x06047544`, list driver `0x06047B2C`) where a job never reaches its terminal state nibble (`7`/`0xA`). **CD, sound (SCSP 68k), and the slave SH-2 are all ruled out:** the 68k runs its driver fine and cycles with SNDON/SNDOFF, and the slave's spin on a sound-RAM signal (`0x05A03C2A`) is a downstream symptom — the master *clears* that signal but, looping, never *sets* it to release the slave. **Tooling: `sdbg`** (`crates/debugger`) — breakpoints incl. register-guarded + stack backtrace, single-step, SH-2 **and** SCSP-68k disasm (`d68`) + 68k PC-trace (`t68`), read/write watchpoints (`SAT_RWATCH`/`bw`), memory search, CD-command / windowed-PC-trace breaks (`cb`/`t`), and SCSP/68k state probes (`scsp`). **Drive-phase port + trace-diff (`d0640a5`):** to exhaust the CD-timeline hypothesis, the CD read pump was rewritten as a faithful port of Mednafen's `cdb.cpp` `Drive_Run` (the BUSY→SEEK→PLAY status sequence, read-ahead pipeline, `PauseCounter`-delayed end IRQ, buffer-full backpressure) so the whole timeline — not just final values — matches the reference. A fresh `SS_PCTRACE` trace-diff against the new timeline then showed **the port did not change VF2's divergence**: ours' intro-loader **job state nibble never reaches terminal `0xA`** (job processor `0x06047752 CMP/EQ #10`: Mednafen reaches the terminal path `0x06047758` 14792×, ours 0×), and the GFS server never reaches its completion handler (`0x0604D2F2+`). The stuck job is "load **SLAU** (FAD 7772)" (persistent struct `0x060B1960`: nibble `[+4]` oscillates 1↔3, FAD `[+0xC]`=7772) — the file ours loops on; Mednafen completes it and advances to the next batch (→ MOVIE 8560). So with the CD-block timeline now faithful **and** byte-identical, the blocker is **definitively a deep game-logic state-machine condition in VF2's intro demo-script engine, not CD-block fidelity** — the nibble's advance, gated through a nested chain (job-list `0x06047B2C` → processor `0x06047718` ↔ producer `0x06047544` → GFS dispatch `0x0604D056`, work-byte `0x060FFCB4`) down to a CD-HIRQ read, never fires. **Open task (VF2):** pin the exact sub-step that advances the nibble `3→0xA` in Mednafen but not ours (instruction-level per-job-iteration RE). Doukyuusei ~if~ unaffected — still boots to its title. |
| 5 | **Doukyuusei ~if~ boots to its title screen** ✅ | A *different* commercial game reached its title via two fixes. (a) **SH7604 FRT `FTCSR` write-0-to-clear** (`073805d`): Doukyuusei renders its Inter Channel logo then its slave SH-2 crashed — a null `JSR @R2` (`R2 = [0x0602739C]`). Diagnosed (new `sdbg` register-guarded *slave* breakpoint + bus-level **memory probe**, `3e4c270`) as the slave reading a master-built function-pointer table *mid-rebuild* (transiently 0), because its inter-CPU FTI `ICF` never cleared: ours treated `FTCSR` as W1C, but the SH7604 clears its status flags by writing 0 after reading 1, so the slave's `ICF`-clear (write 0) was ignored and its wait-loop spun through. Fixed → the slave waits, reads a valid pointer, the game advances logo → title. (b) **VDP2 hi-res rendering** (`c0f2344`): Doukyuusei runs at **640×224** (TVMD HRESO=2) but ours hardcoded 320 → overflow. `Vdp2Regs::screen_dims()` decodes TVMD (320/352/640/704 × 224/240/256[×2]); `render_frame` renders the active w×h (returns the dims); the frontend re-creates its SDL texture on resolution change. `bios_boot` golden unchanged (hashes only the active region). Also landed (a real missing feature, though VF2 masks it): the **CD-block SCU external interrupt** (vector 0x50, level 7; `57a1066`) + the HIRQ-edge `hirqlog` trace (`94dfdd4`). |

### Spun out of M11 — kept general accuracy fixes

Real hardware-fidelity fixes that came out of the boot investigation and stand on
their own (independent of any boot path):

- **Scheduler cycle-resync on un-halt** — `Saturn::release_slave` (SMPC `SSHON`)
  bumps the slave's frozen `pipeline.cycles` up to the global clock before
  un-halting, so the scheduler can't "time-travel" through millions of stale
  catch-up cycles. Regression: `dual_sh2::releasing_slave_resyncs_its_cycle_no_time_travel`.
- **Inter-CPU FRT input-capture (FTI)** — a 16-bit write to
  `0x0100_0000..0x017F_FFFF` pulses the slave's `FTCSR.ICF`, `0x0180_..` the
  master's (`sh2 Frt::input_capture` + bus FTI regions + `Saturn::drain_input_capture`).
  Regression: `dual_sh2::word_write_to_fti_region_pulses_target_frt_input_capture`.
- **SCU clears IST on the SH-2 acknowledge cycle** — `take_pending_interrupt`
  clears the acknowledged source's IST bit (matching hardware/Mednafen, which W1C
  IST only a couple of times a run) instead of leaving it stale-set.
- **Event-edge-clamped scheduler batching** (Mednafen-alignment Phase 1) —
  `Saturn::run_for` now clamps each scheduler batch to the next *scheduled event
  edge* (VBlank-IN, VBlank-OUT, or a pending INTBACK completion) via a shared
  `batch_size`/`cycles_to_next_event`, instead of fixed ≤256-cycle batches. This
  mirrors Mednafen's `next_event_ts` model so interrupt assertion and the raster
  registers settle at the cycle-exact point the reference produces them — keeping
  the LLE↔Mednafen master-PC diff aligned (verified: the streams now correspond
  PC-for-PC from boot). HBlank and SCU-DMA are deliberately excluded as clamp
  edges. Also reconciled the INTBACK SF-busy time to Mednafen's 4 MHz SMPC-clock
  model (`intback_busy_us`; a status INTBACK ≈ 261 µs, was ~16 µs). The scheduler
  determinism contract is untouched.
- **CD status report `is_cdrom` bit** — the CD-block status report's CR1 low byte
  is now `(is_cdrom << 7) | (repcnt & 0x7F)` (matching Mednafen `cdb.cpp`), where
  bit 7 marks the current head position as a CD-ROM (data) track; it was a
  mismodeled `(options << 4) | repcnt` that left bit 7 clear. (A genuine fidelity
  fix; not the VF2 boot decider — see M11 Phase 3.)
- **Master-leads-slave CPU interleave** (Mednafen-alignment Phase 2A) —
  `Saturn::step_cpus` steps the master one instruction then runs the slave until
  it catches up to the master's timestamp (Mednafen `ss.cpp`: `CPU[0].Step` +
  `RunSlaveUntil`), replacing the scheduler's most-behind-first rule for the
  SH-2 pair (which could let the slave lead the master). `run_for` and
  `run_for_traced` both route through it; the generic `Scheduler::run_for` is
  kept for the CD timer + the determinism unit test. Golden unchanged;
  determinism + savestate round-trip green.
- **Per-instruction SCU interrupt sampling** (Phase 2B) — the SCU IRL is sampled
  before every master instruction (`step_cpus` / `debug_step_master`) instead of
  once per scheduler batch, so an interrupt is delivered at the exact instruction
  `SR.imask` drops below its level (matching Mednafen's per-instruction IRL
  sampling) rather than up to a batch late. `drain_scu_intc` retired.
- **SH7604 FRT `FTCSR` write-0-to-clear** (`073805d`) — the free-running timer's
  status flags (ICF/OCFA/OCFB/OVF) clear by writing 0 after reading 1 (the
  Hitachi semantic; consistent with our WDT `TCSR`), not W1C as ours had it. Was
  load-bearing for the inter-CPU FTI handshake (a core polling `FTCSR.ICF` clears
  it by writing 0; under W1C `ICF` stuck set and its wait-loop never waited) — it
  fixed Doukyuusei's intro slave crash (M11 Phase 5). Two FRT tests corrected.
- **CD-block SCU external interrupt** (`57a1066`) — the CD-block now drives SCU
  external interrupt 0 (IST bit 16, **vector 0x50, level 7**), asserted as a
  level `(HIRQ & HIRQ_Mask) != 0`, masked by IMS **bit 15** (the SCU sign-extends
  the 16-bit mask), and re-armed via the AIACK write (Mednafen `RecalcIRQOut` /
  `ABusIRQCheck`). ours had no CD interrupt at all. Unit-tested; golden unchanged.
- **VDP2 hi-res rendering** (`c0f2344`) — the renderer reads the active resolution
  from TVMD (`Vdp2Regs::screen_dims`: 320/352/640/704 × 224/240/256[×2]) and
  renders that width×height (returning the dims) instead of a fixed 320×224; the
  SDL frontend re-creates its texture on a mode change. Fixed the 640-wide
  Doukyuusei title-screen overflow (M11 Phase 5); `bios_boot` golden unchanged.
- **`sdbg` interactive debugger** (`crates/debugger`) — a headless gdb-style REPL
  over the core: breakpoints (incl. register-guarded, with a stack backtrace on
  hit), single-step, SH-2 + **SCSP-68k** disassembly (`d68`) and 68k PC-trace
  (`t68`), read/write watchpoints (`SAT_RWATCH`/`bw`), memory search, CD-block
  state + command history + a `cb` CD-command breakpoint, SCSP/68k state probes
  (`scsp`), and save-state rewind. Built to drive the boot/CD/sound/timing
  investigation without rebuild cycles. It bundles a small MC68000 disassembler.

### Removed: the HLE-boot detour (ADR-0010/0011 Superseded)

An opt-in HLE direct boot (`--hle-boot`: load the 1st-read + jump) plus an HLE
BIOS SYS-call library (`bios_hle`, `cold_hle_boot`) got VF2's own code running on
both SH-2s but never rendered, and — being a different program from the real-BIOS
boot — broke the PC-trace-diff methodology. Since the reference oracle (Mednafen)
is LLE, the path was **removed** to keep the codebase clean and the diff valid.
The general fixes above were salvaged from it.

## Later milestones (queued)

- **MPEG card** + CD move/copy sector ops (deferred from M7).
- **Explicitly never** — JIT / dynarec (accuracy over performance is the project's design axis).
