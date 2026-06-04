# BIOS boot BGM — diagnosis

**Date:** 2026-06-04 · **References:** MAME v0.287 (`mameref/saturn`, no-disc) +
**Mednafen dev build** (`mednaref`, audio-CD, LLE oracle) · **Status:** animation
**fixed**; BGM **root localized** to a per-voice timing-divider phase divergence
in the sound 68k — a timing-accumulation bug, not a missing feature.

> **Update (2026-06-04b) — three suspects for the trigger-tick lead ruled out;
> the lead is localized to *post-recognition* polled CD state.** Following the
> lockstep diff below (root = ours triggers the BGM ~83 seq-ticks / ~10 frames
> early, putting a per-voice divider 3 phases off), this session eliminated three
> candidate causes of the early trigger: (1) the **VDP1 draw-slowdown** — ported
> the Mednafen primitive, then a `SS_VDP1DRAW` probe proved the boot-animation
> draws are *trivial* on both sides (~997 cy, empty list), so there is no draw to
> slow (`9934411`); (2) the **CD recognition duration** — `STARTUP_CYC ×2` moved
> the trigger the *wrong* way; and a recognition-frame probe (`0444b2b`) shows
> ours settles `Startup→PAUSE` at **frame 60** = the 1 s constant Mednafen also
> uses, so recognition is *not* the lead — it is entirely **post-recognition**
> (frames 60→529); (3) **SH-2 cache staleness** — the master reads sound RAM
> 100 % cache-through on both sides (`a82de6f`), so no stale read. With the CD
> command stream already proven byte-identical, the early trigger now points at
> **polled CD state** (the CR1–4 status report / partition sector-count
> transitioning early) — the *same* lead as the VF2 intro stall. **Next:**
> compare ours' vs Mednafen's CR1–4 status report + partition block-count across
> the post-recognition file-load window.
>
> **Update (2026-06-04) — root localized via a Mednafen lockstep 68k diff.**
> Using an **audio CD** (which Mednafen *can* boot, unlike no-disc) as an LLE
> oracle, a layered trace-down proved the SCSP synthesis, the 68k→SCSP path, and
> the BGM sequence data all **byte-for-byte correct**, then pinned the **first
> 68k control-flow divergence** to a single instruction — `0x484C: bcc`, a
> per-voice timing divider `[a4+3]` that is at a **different phase** in ours vs
> Mednafen when the BGM starts. The note-mis-processing and stall are downstream
> of that. See **[Update (2026-06-04): the audio-CD lockstep trace-down](#update-2026-06-04-the-audio-cd-lockstep-trace-down)**
> below; it supersedes the "candidate roots / suggested next step" of the earlier
> MAME-based analysis. Commits `43e7e94`, `32fdd11`, `f47cfc2`, `1d82aa4`.

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

## Update (2026-06-04): the audio-CD lockstep trace-down

The earlier analysis (below) localized the gap to the sound 68k but stalled on
*tooling*: MAME's no-disc boot couldn't be trace-diffed (the imgui debugger
needs a GPU; the SCSP write-tap was unreliable). This update switched references
and cracked the localization open.

### Why the reference changed — Mednafen, with an audio CD

Mednafen cannot boot no-disc, but it **can** boot with an **audio CD inserted**
(`mednaref/src/mednafen -force_module ss roms/audiocd.cue`), reaching the same
CD-player panel and playing its BGM. Mednafen is a true **LLE** core, so it runs
the *same* BIOS + sound-driver code ours does — making a byte-level lockstep diff
possible. Run it **headless** (`SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy`) so
no window lands on the active display.

The whole trace-down used the **JAP BIOS + `roms/audiocd.cue`** on both sides.

### What is now proven *correct* (newly ruled out)

| Layer | How it was proven | Verdict |
|---|---|---|
| **SCSP synthesis** (slot/FM/interp/EG/pan/mix) | A self-contained SH-2 **sine test ROM** (`audio_pipeline.rs`) cross-checked vs a mednaref `SS_SINETEST` hook | matches **0.4 % mean / 1.3 % max** |
| **68k→SCSP path** | A **68k**-driven sine ROM (`audio_pipeline_sine_68k`): the sound CPU keys a voice through its own SCSP window | full-scale tone — **works** |
| **BGM sequence data** | `sdbg m 0x05A18200` vs mednaref `SS_SEQDUMP` | **byte-identical** (`7F 00 B0 0A 3B 01 …`) |
| **BGM start** | ours' enqueue stream `ENQLOG` vs `SS_SEQDUMP` | first events identical (`B0 C0 B0 90/33 …`) |
| **Voices processed** | itrace `a4` set `{0x6000,0x7000,0x9800}` | identical — same voices |

So the bug is **none** of: synthesis, the 68k→SCSP register path, the sample
load, the sequence data, the master command stream, the CD-block data transfer.

### The BGM pipeline, mapped end-to-end (audio-CD CD-player driver)

Driver code lives at sound-RAM `0x1000–0x5100`; work base `a6 = 0x6000`.

```
Timer-B ISR  0x1388 ─┐ (lea 0x6000,a6 ; inc counters ; lea 0x1F00,a6 → 0x7F00)
                     └─ jsr 0x40F2  seq-tick
                          ├─ note-on interpreter  (0x46C2 ctrl-changes ; 0x4802/0x4812 notes)
                          │     reads event byte [a6+0x2D], delta-time gate
                          │     [a6+0x18] -= [a6+0x14] ; bcc (not-yet)
                          │     └─ jsr 0x4B9A  ENQUEUE → 4-byte cmd into the ring @0x7A00
                          │           (a2 = [0x0450]+0x1A00 ; cmd = (event>>4)&7)
                          └─ 8× jsr 0x4570  per-channel processor (reads 0x7F00 channels)
main thread ── seq-player 0x2162 ─ dispatch 0x21A4 `jmp (2,pc,d0.w)` on cmd&7
                  cmd 3 (0xBX) → 0x21C8 → table 0x2200 → 0x28A4 → 0x2E78  KYONEX (voice key)
                  cmd 0/1/4    → idle / note handlers
```

The ring at `0x7A00` is the 68k-internal command queue the interpreter fills and
the player drains; **only `0xBX` control-change events (ring cmd 3) reach the
voice-key strobe `0x2E78`.**

### The lockstep diff — the first divergence

An aligned **instruction-boundary** trace on both sides (ours `ITRACE`, mednaref
`SS_ITRACE` hooked at the M68K opcode fetch `PC-2`), armed at the first enqueue
(`0x4B9A`) and restricted to the seq-engine range `[0x4000,0x4C40)`:

```
both run BYTE-IDENTICAL PC paths for 1310 instructions, then:
  … 483E 4844 4848 484C
  ours → 4850   (bcc NOT taken)
  mfn  → 48AC   (bcc taken)
```

`0x484C: bcc 0x48ac` follows `0x4848: subq.b #1,(0x3,a4)` and is gated by
`0x483E: btst #7,(0x2,a4)` (voice active). **Same voice (`a4 = 0x9800` in both),
same instruction, identical decrement count in the window** — yet ours' divider
`[a4+3]` has already underflowed to **0** (carry set → branch *not* taken → reset
to 7 and run the periodic action at `0x4856+`) while Mednafen's is **≥1** (branch
taken → skip). Ours' `[a4+3]` at `0x4848` reads `02, 01, 00` then borrows.

### Root: a per-voice timing-divider *phase* divergence

Because the PC path is identical up to the divergence and both decrement the
divider equally, ours' divider must have **started the BGM window at a different
phase** than Mednafen's — a difference set *before* the first enqueue (the
reg-hash variant of the trace differs from line 1). This is **not** a single
pokeable opcode and **not** a different voice; it is a **timing-accumulation**
divergence: when ours triggers the BGM, the driver's internal voice dividers are
out of phase with the reference, so the periodic voice action fires a phase early,
the note stream mis-processes (each chord note enqueued once instead of twice —
`33,38,3C` vs `33,33,38,38,3C,3C`), and the sequence **stalls after the intro**
(9 events vs Mednafen's continuing stream) → the ring drains → silence.

The most likely origin is that **ours triggers the BGM at a different absolute
tick** than Mednafen — i.e. ours' CD/SMPC/Timer cycle-timing is not yet
cycle-identical to the reference (this ties back to the M11 CD-timing work and
[mednafen-divergence-review.md](mednafen-divergence-review.md)).

### Tooling built (all committed, debug-only, `#[serde(skip)]`, golden-safe)

| Tool | Where | Purpose |
|---|---|---|
| `audio_pipeline_sine` / `_68k` | `crates/saturn/tests/audio_pipeline.rs` | prove SCSP synthesis (SH-2- and 68k-driven) |
| 68k **footprint** | `Scsp::enable_68k_footprint`, `bios_audio_probe` `TRACE68=`/`FOOT_OUT=` | every distinct 68k PC over a whole run |
| sound-RAM **census** | `bios_audio_probe` | non-zero spans + the `0x500`/`0x700` command channel |
| **enqueue log** `ENQLOG=` | `Scsp::enable_enq_log` | `[d0-3,a6]` at a hot PC (the BGM event stream) |
| **itrace** `ITRACE=` | `Scsp::enable_68k_itrace` | aligned instruction-boundary `(pc,a4)` trace |
| mednaref hooks | `scsp.inc`, `sound.cpp`, `m68k.cpp` (gitignored tree) | `SS_SINETEST`, `SS_KYONEX`, `SS_SEQDUMP`, `SS_WWATCH`, `SS_ITRACE` |

### Honest status & next step

The BGM gap is now a **well-characterized timing bug**, not a mystery: a per-voice
divider phase set before the BGM, downstream of (probably) the CD/Timer trigger
tick. The two strategic continuations:

1. **Arm the itrace earlier** (before the BGM setup) to catch where the voice
   phase *first* diverges — a larger, harder-to-align window.
2. **Compare the BGM-trigger tick** — does ours start the sequence at the same
   cycle as Mednafen? This rejoins the broader CD/SMPC/Timer cycle-timing work
   and is the higher-leverage path (it would benefit game audio generally).

This is parked deliberately: the next push is cycle-timing engineering, not more
tracing.

## Target

Boot a Saturn BIOS with **no disc** and produce audio: the multimedia
CD-player panel's background music, plus the direction-key "feature select"
nav SFX. On real hardware this panel animates and plays BGM with no disc
inserted.

> **Note (2026-06-04):** the deep root analysis above used the **audio-CD**
> CD-player driver (the Mednafen-bootable oracle). The no-disc panel below is the
> *original* target; both exercise the same sound-driver execution and the same
> per-voice-divider machinery, so the finding applies to both. The sections that
> follow are the earlier **MAME / no-disc** investigation that led here.

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
