# BIOS boot BGM — diagnosis

**Date:** 2026-06-03 · **Reference:** MAME v0.287 single-driver Saturn build
(`mameref/saturn`, HLE CD-block like ours) · **Status:** animation **fixed**;
audio still open.

> **Update (2026-06-03) — the disc-present boot animation is fixed (commit
> `e2884e7`).** A second reference run — MAME with an **audio CD inserted**
> (`mameref/saturn saturn -rompath mameref/roms -cdrom roms/audiocd.cue
> -skip_gameinfo`) — turned out to be the closest-to-real-hardware boot anyone
> had seen: the morphing SEGA-SATURN logo animation + menu sound from sec 1.
> Ours skipped it because `insert_disc` reported `STATUS_PAUSE` immediately,
> so the BIOS saw an already-ready drive and jumped to the static logo. Porting
> Mednafen's **`DRIVEPHASE_STARTUP`** — `STATUS_BUSY` for ~1 s of recognition
> spin-up before PAUSE, with a host-level Init no longer cancelling it
> mid-spin-up — makes the BIOS take the recognition branch and **play the
> animation**, while Doukyuusei ~if~ still boots to its title. The 68k-execution
> analysis below still stands for the **remaining gap: the animation/menu is
> silent** (the SCSP keys only the throwaway voice). The recognition fix was
> *necessary* (it gates the whole boot-sound path) but not *sufficient* — the
> silent-voices issue is the next piece.

## Target

Boot a Saturn BIOS with **no disc** and produce audio: the multimedia
CD-player panel's background music, plus the direction-key "feature select"
nav SFX. On real hardware this panel animates and plays BGM with no disc
inserted.

> **Why MAME, and why the USA BIOS.** Mednafen — our usual LLE oracle — **cannot
> boot with no disc** (it only launches via a game image), so it is unavailable
> for this target. MAME *can* boot bare, and its CD-block is **HLE like ours**,
> so it is the right reference: if MAME's HLE makes the BIOS play the BGM and
> ours does not, the gap is something ours is missing from the *same* HLE
> approach. The **USA BIOS** (`bios/Sega Saturn BIOS (USA).bin`, =
> `mameref/roms/saturn/mpr-17933.bin`) is used because (a) it is MAME's primary,
> fully-booting `saturn` driver, and (b) headless with no input it advances all
> the way to the CD-player panel, whereas the JAP BIOS parks on the silent
> first-boot clock-setting screen. The fix is expected to be BIOS-agnostic.

## Headline conclusion

**The BIOS-boot BGM gap is a pure 68k (MC68EC000 sound CPU) *execution*
divergence — not a missing feature, not a data/upload bug, not the CD-block,
not the boot path.** Given **byte-identical sound-driver code** and a
**byte-identical master→68k command buffer**, MAME's sound 68k keys the
multimedia-panel BGM voices and ours does not. The bug therefore lives in how
ours' 68k *runs* that identical driver — a subtle m68k core-instruction bug, or
an SCSP timer/interrupt timing difference the sequence player branches on.

## Evidence

Both emulators, USA BIOS, no disc, ~sec 9 (the CD-player panel):

| Layer | Observation | Verdict |
|---|---|---|
| Boot path / screen | both: SEGA-SATURN logo → **CD-Player panel** ("Drive empty / Play / Pause") at the same ~sec-9 timing | **match** |
| Early boot | both spin the *same* fixed-delay loops (`0x00001D3C` = `Dt R3; Bf`, and `0x000002B0`, **523584 iters each**) — `0x1D3C` is a counted delay, **not** a CD poll | **match** |
| Master→68k command buffer (sound RAM `0x500`) | `00 01 00 00 00 00 80 00 \| 10 01 80 00 00 00 40 00 \| 11 01 C0 00 …` | **byte-identical** |
| 68k sound-driver code (`0x1090`, `0x2E10`, `0x3240`) | same bytes (both loaded from the same BIOS) | **byte-identical** |
| 68k voice working area (`0x7000`) | MAME: populated and **evolving** (frame 540 ≠ 600); ours: **sparse/static** | **diverges** |
| BGM voices keyed | MAME keys **12** (slots 0,1,2,8,9,10,16,17,18,24,25,26); ours keys **none** of them | **diverges** |
| Audio (WAV) | MAME: peak **22857** at sec 8–9 (the panel BGM); ours: a single startup-"Sega!" blip (peak 6385) at sec ~4, then **silence** | **diverges** |
| Ours' KYONEX strobes | **frozen at 37** — all during startup; **zero key-ons after the panel appears** | symptom |

Ours' four "active" SCSP slots at the panel (0, 8, 16, 24 — all `eg=REL`, mostly
`disdl=0`/`imxl=0` = routed nowhere, DSP `MIXS`/`EFREG` high-water all 0) are
**leftover startup voices decaying**, not BGM. The real fault is the absence of
*any* BGM key-on, not silent-but-keyed voices.

So: ours' 68k runs the BGM tick (a breakpoint at `0x3EE8` hits, inside the
level-2 timer interrupt, `imask=2`) but its sequence player never reaches the
note-on / KYONEX-strobe path that MAME takes.

## Ruled out

- **The master / SH-2 side** — the `0x500` command buffer is byte-identical, so
  the master sends MAME the same BGM commands ours does.
- **The driver upload** — the 68k driver code is byte-identical in sound RAM.
- **The boot path / "skips the animation" theory** — on USA, ours reaches the
  same panel as MAME at the same time; the early-boot delay loops match exactly.
  (The `0x1D3C` "CD wait" is a counted delay, not a CD-state poll.)
- **`imask` masking the 68k timer IRQ** — on USA it is 0 (unmasked); still no
  BGM key-ons. (This re-confirms the earlier JAP-side refutation.)
- **m68k brief-index addressing** `(d8, An, Xn)` — `Cpu::brief_index`
  (`crates/m68k/src/interpreter.rs`) is correct (disp8 sign-extended, A/D select
  on bit 15, W/L on bit 11). The voice-setup code uses this mode heavily; it is
  not the bug.
- **The JAP-no-disc "BIOS keys 0 slots" dead end** — that was a BIOS-path
  artifact; the USA BIOS exercises the BGM trigger and gets far further.

## What remains (candidate roots)

1. **An m68k core-instruction bug** exercised by the sequence player but not by
   the simpler startup path — e.g. a flag/result edge case in `bclr`/`bset` on
   memory, `mulu`, `dbf`, or an addressing/extension-word case other than the
   brief-index mode already cleared. A core bug here would silently affect
   **every** game's sound (note VF2's stall also involves a sound-RAM
   handshake), so this is the higher-leverage hypothesis.
2. **SCSP timer / interrupt timing** — the BGM tick is driven by an SCSP timer
   (level-2 interrupt). If a timer's rate, or the moment its `SCIPD` pending bit
   is raised relative to the master/68k, is off, the sequence player can run but
   never advance to a note-on.

### Suggested next step

The decisive experiment is a **68k execution trace-diff** — ours' `t68`/`b68`
against MAME's `audiocpu` around the sec-8 key-on — to find the *first* divergent
instruction. Two practical blockers stopped this session:

- MAME's Lua `install_write_tap` on the SCSP device region (`0x100000+`) is
  **unreliable** — it caught only the frame-72 init burst and missed the BGM
  key-on writes. Trust MAME's audio output and memory *reads*, not SCSP-region
  write taps.
- A full MAME `audiocpu` debugger trace is reliable but huge/slow (2.4M lines /
  20s wall, no loop collapse) **and** requires a GPU — under Xvfb + llvmpipe the
  bgfx/imgui debugger fails to initialize, so headless tracing did not work here.

A more strategic alternative to MAME-trace-diffing is **differential testing of
the m68k core** against a 68k test ROM or a known-good 68k emulator; if the root
is hypothesis (1), that finds it and benefits the whole project at once.

## Reproduction & tooling

Ours (the `#[ignore]`d manual probes now take `BIOS=`/`REGION=` env, defaulting
to JAP):

```bash
# per-second audio peak + key-on counts (USA BIOS, no disc)
BIOS="bios/Sega Saturn BIOS (USA).bin" REGION=us FRAMES=720 \
  AUDIO_OUT=/tmp/ours.pcm \
  cargo test -p saturn --test trace_boot bios_audio_probe -- --ignored --nocapture

# framebuffer snapshots (PPM) of the boot screens
BIOS="bios/Sega Saturn BIOS (USA).bin" REGION=us \
  cargo test -p saturn --test trace_boot dump_framebuffer -- --ignored --nocapture

# interactive: SCSP slots, 68k disasm/trace/breakpoints at the panel
printf 'f 600\nscsp\nt68 40\nd68 00003EE8 20\nq\n' | \
  ./target/debug/sdbg --region=us "bios/Sega Saturn BIOS (USA).bin"
```

MAME reference (note: `mameref/saturn` opens a **real window** — there is no GPU
for a headless Xvfb here, so these land on the active display):

```bash
cd mameref
./saturn saturn -rompath ./roms -video soft -nothrottle \
  -seconds_to_run 12 -wavwrite /tmp/mame.wav      # audio: BGM peaks at sec 8–9
# snapshots: -autoboot_script Lua calling manager.machine.video:snapshot()
# audiocpu trace needs -debug -debugger imgui (GPU) — see blockers above
```

`-video soft` renders headlessly (no GPU); `-video bgfx` needs a GPU and renders
black otherwise. The sound 68k device tag is `audiocpu`; it sees sound RAM at
`0x000000` and the SCSP registers at `0x100000` (slot regs at `0x100000 +
slot*0x20`, KYONB = bit 11 / KYONEX = bit 12 of each slot's reg 0).

## Project note

Saturn audio (the SCSP plus the dual sound-CPU timing) is among the hardest
parts of Saturn emulation; mature emulators spent years on it. This target is an
**optional** quality-of-life goal, not a prerequisite for M11 (boot a commercial
game). It is parked here with the bug localized so it can be resumed
deliberately rather than chased opportunistically.
