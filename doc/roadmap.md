# 5thPlanet Roadmap

Accuracy-first SEGA Saturn emulator in Rust. Milestones are scoped tightly so
the foundation is solid before the next chip is added. This file is a status
tracker; blow-by-blow investigation history lives in the git log,
`doc/system-architecture.md` §9 (Bootstrapping), and the commit messages
referenced below. Commercial titles that run are listed in
[`doc/compatible-game-titles.md`](compatible-game-titles.md); the ones that don't
yet boot/run correctly (the active boot-blocker investigations) are tracked in
[`doc/wip-compatibility-titles.md`](wip-compatibility-titles.md).

Current test count: **1168 workspace-wide, 0 failures** (default features; +2 with `--features gpu-presenter`), ~85% line coverage
(`cargo llvm-cov`; excludes the SDL3 frontend and the FFI `physdisc` crate).

**Self-diagnostics suite:** `saturn::diagnostics` has two tiers. **Feature
checks** (`run_all`, 16 so far across cpu / branch / memory / onchip DIVU·FRT·DMAC
/ scu DMA / vdp2 render / scsp audio) run hermetically from reset — no BIOS, no
disc, no toolchain — each verifying one behavior on a throwaway machine; they're
golden, deterministic, and CI-able (the `all_diagnostics_pass` test). **System /
boot-compatibility checks** (`run_system`) are heuristic and *do* need real
media: given a BIOS (+ optional disc) they boot a throwaway and observe video /
TOC / the 1st-read program reaching High WRAM. Surfaced via `jupiter doctor`
(`doctor` = feature checks; `doctor <BIOS> [disc]` adds the boot checks), an OSD
**"Diagnostics…"** screen (feature checks), and the CI test. Grow the feature
set as chips/games surface needs.

## Component status

✅ complete · 🟡 partial (usable core, refinements pending) · 🔶 stub · ⬜ not started.

| Component | Status |
|-----------|--------|
| SH-2 (SH7604) ×2 core | ✅ Full ISA, 5-stage cycle model, cache, exceptions, on-chip INTC/DIVU/FRT/DMAC/WDT (FRT/WDT event-scheduled, INTC recalc-on-change; SCI/UBC storage stubs) |
| Saturn bus + memory map | ✅ Typed regions, per-access BSC bus-timing model (M12 #8), open-bus default |
| Event-driven scheduler | ✅ Deterministic; master-leads-slave SH-2 interleave + CD-block entity |
| SMPC | ✅ Slave hold/release, staged INTBACK + pad/mouse, NMIREQ, SNDON/OFF, live RTC, region; clock-change/SYSRES no-op |
| SCU (+ DMA + INTC) | ✅ 3 DMA channels (direct/indirect/strides/start factors, cycle-stealing), interrupt aggregation, Timer 0/1 |
| SCU-DSP | ✅ Full VLIW core (ALU/MUL/buses/jumps/DMA/END), host-wired |
| VDP2 | ✅ NBG0–3 + RBG0/1 + sprite layer compositor: priority, colour calc (+special/extended), windows, shadow, mosaic, colour offset, line/back screens, per-line scroll/zoom, VCP fetch gating, CRAM modes, hi-res (320–704), live raster regs |
| VDP1 | ✅ Full plotter (all primitives/colour modes, gouraud), erase, double-buffer (FBCR), TVM 8bpp + DIE interlace, cycle-accurate draw-end IRQ. TVM=3 (8bpp+rotate) deferred |
| MC68EC000 (sound CPU) | ✅ Full ISA + exception/interrupt model, exact MUL/DIV timing, address/bus/trace exceptions |
| SCSP | ✅ 32-slot PCM+FM engine, ADSR, LFO, mixer/DAC, MVOL, slot monitor (0x408), 128-step effect DSP, CD-DA via EXTS, 44.1 kHz output |
| CD-block | 🟡 HLE (SH-1 firmware undumped — HLE is the model, as in every Saturn emulator): disc image (ISO/CUE/CCD) + TOC, 200-block buffer + 24 filters/partitions, Mednafen-faithful drive-phase read pump, data transfer (FIFO + SCU-DMA), ISO9660 FS, auth, SCU external IRQ (vec 0x50). Remaining: MPEG card, move/copy ops |
| Cartridge slot | ✅ Extension DRAM (1/4 MB), battery backup RAM, ROM carts; cart-ID at `0x04FF_FFFF`; `--cart=` flag |
| SDL3 frontend | ✅ Window + framebuffer, audio-paced run loop, rebindable keyboard + hot-plug gamepad, save-state hotkeys, persisted config (migrated SDL2→SDL3); optional SDL_GPU/Vulkan presenter + built-in CRT shader (`gpu-presenter` feature, off by default) |
| Save states | ✅ `save_state`/`load_state` (bincode + versioned header, currently v12); media referenced not embedded, fingerprint-validated |
| Backup RAM (battery) | ✅ Internal 32 KiB, hardware odd-byte packing, persisted to `<bios>.bup` |
| On-screen menu (OSD) | ✅ Software-composited in-window menu (ADR-0008): save/load slots, reset, disc eject/insert + image browser, Settings (Graphics/Controller/Region/Cartridge/BIOS), persisted to `jupiter.toml` |

**Milestones:** M1–M12 ✅ · M13 (fidelity backlog) 📋.
Four commercial games are **fully playable**: *Virtua Fighter 2* (60 fps, tag
`vf2-good-emulation`), *Doukyuusei ~if~*, *Sangokushi V* (三國志V), and *Panzer
Dragoon Zwei* — see [`compatible-game-titles.md`](compatible-game-titles.md).

## Milestone 1 — Cycle-accurate SH-2 core ✅

Standalone `sh2` crate: full ISA (~142 ops), delay slots, exceptions, 5-stage
pipeline cycle model (load-use, multiply latency, branch costs), 4 KiB 4-way
cache, on-chip peripherals (INTC/DMAC/DIVU/FRT/WDT behavioral; BSC/SCI/UBC
storage stubs). ROM regression harness with committed golden hashes.

## Milestone 2 — Saturn bus, dual SH-2, scheduler ✅

`SaturnBus` typed regions + memory-map dispatch (open bus when unmapped), the
cache wired into live fetch/data paths, second (slave) SH-2, deterministic
event-driven `Scheduler` + `SchedEntity`, `Saturn` aggregate.

## Milestone 3 — SCU, SMPC, VDP2 minimal, SDL2 ✅

SMPC slave hold/release; SCU registers + 3 DMA channels + interrupt aggregator;
`scu_dsp` full VLIW DSP crate wired into the SCU host; VDP2 register bank +
VRAM/CRAM + minimal NBG0 renderer; SDL2 frontend shell; BIOS-boot golden test.

## Milestone 4 — BIOS splash on screen ✅ (2026-05-28)

The real BIOS boots to the animated SEGA splash, pixel-matching MAME (golden
`0x2C379F92CE1B63F7` at frame 200; later re-baselined by C7 to
`0x0B1BA6E5180766F7`). Key fixes found by re-syncing PC diff vs MAME/Yabause:

- The boot blocker was a **missing VBlank-OUT interrupt** (SCU vector `0x41`) —
  its callback advances the BIOS frame counter the boot park polls.
- SCU register map was off by `0x10` from offset `0x90` up (IMS/IST/timers),
  spuriously masking the SMPC interrupt (`d51cfca`).
- `sh2`: route CCR (`0xFFFFFE92`) to the cache; `LDC …,SR` is not slot-illegal.
- SMPC command codes + INTBACK timing/layout; VDP2 live raster timing
  (`VCNT`/`TVSTAT`, cycle-exact VBlank edges); SCU fixed external vectors
  (`0x40 + source`).
- Splash render fixes: VDP1 automatic draw (PTM=0b10), VDP2 8bpp char
  addressing (`char × 0x20`), CRAM address offset (CRAOFA/B), transparent-pen-
  as-solid (`7842133`/`bafa590`/`ca97b38`/`122db98`).

Values tuned to a reference rather than a datasheet are tagged `REVIEW(magic)`
(`grep -rn "REVIEW(magic)" crates`); revisit a tag only if a divergence
implicates it.

## Milestone 5 — Chip build-out: VDP1, MC68EC000, VDP2 ✅

- **VDP1 plotter** — one textured-quad rasteriser backs polygons/sprites/lines;
  all CMDPMOD colour modes, clipping, gouraud, erase, double-buffering (FBCR),
  cycle-accurate draw-end + SCU sprite-draw-end interrupt.
- **MC68EC000** — new `m68k` crate (`no_std`, structured like `sh2`): all EA
  modes, full user-mode ISA, exception/interrupt model. SCSP host wiring: 512 KiB
  sound RAM + hosted 68k paced at 11.2896 MHz, released by SMPC SNDON.
- **VDP2 build-out** — NBG0–3 priority compositing, sprite layer (SPCTL type
  decode + PRISA..D), RBG0/1 rotation (parameter table, coefficients,
  screen-over), full pattern-name decode (1/2-word, 16×16 cells, flips, plane
  sizes), colour calc + W0/W1 windows + sprite shadow, CRAM modes, per-line
  scroll/zoom/windows, vertical cell scroll.

## Milestone 6 — SCSP audio ✅

Timers A/B/C + the 68k/main-CPU interrupt model (SCIEB/SCIPD/SCILV, MCIPD →
SCU); 32-slot PCM engine (OCT/FNS phase, loop modes, interpolation); ADSR + TL
envelope; DISDL/DIPAN mixer to 44.1 kHz stereo; SDL2 audio output; 128-step
effect DSP (MAC, delay line, PACK/UNPACK); SMPC digital pad from the keyboard.

## Milestone 7 — CD-block (HLE) + cartridge slot ✅

**Approach: HLE** — the SH-1's CD firmware is undumped and half its job is an
analog servo, so there is nothing to LLE against; we model the host command
interface + buffer engine + filesystem (as MAME/Yabause/Mednafen do), against
MAME `saturn_cd_hle.cpp`.

| # | Phase | Status |
|---|-------|--------|
| 1 | Disc image + TOC (`disc.rs`: ISO/CUE-BIN/CCD → FAD-addressed `Disc`, 102-entry TOC) | ✅ |
| 2 | Buffer/filter/partition core (200 blocks, 24 filters, commands `0x40`–`0x54`) | ✅ |
| 3 | Sector read pump + data transfer (FIFO port + SCU-DMA at `0x2581_8000`) | ✅ |
| 4 | ISO9660 filesystem (Change Dir / file info / Read File, `0x70`–`0x75`) | ✅ |
| 5 | Authentication + region (`0xE0`/`0xE1`, "SEGA SEGASATURN" header) | ✅ |
| 6 | Cartridge slot (`cartridge.rs`: DRAM/backup/ROM carts + cart-ID) | ✅ |

Deferred within M7: CDDA→SCSP and live discs (→ done in M10); MPEG card and
move/copy sector ops (still open, block nothing).

## Milestone 8 — Save states + battery backup RAM ✅

serde derives across the cores (feature-gated) and `saturn`;
`save_state`/`load_state` over bincode with a magic + version header; external
media `#[serde(skip)]`'d, re-grafted on load, fingerprint-validated; a
snapshot-then-equal-runs determinism test. `memory::BackupRam` (32 KiB,
hardware odd-byte packing, "BackUpRam Format" default) persisted to
`<bios>.bup`; F5/F9 frontend hotkeys. Deferred: state migration across version
bumps, rewind/run-ahead, compression.

## Milestone 9 — Frontend OSD ✅ (2026-06-11)

Hand-rolled, software-composited in-window menu (ADR-0008), sdl2-free +
core-free so it's unit-tested without a window. Esc opens it: save/load slots
(10), Reset, Eject/Insert disc, a **Load Disc…** filesystem image browser
(navigate dirs, pick a `.cue`/`.iso`/`.ccd`, load + boot — frontend owns the
`fs`, the menu stays pure), Quit, plus Settings — **Graphics** (scale
1×–4×, fullscreen), **Controller** (press-to-bind keyboard rebind + a live
Shuttle Mouse port toggle Off/1/2), **Region**, **Cartridge**,
**BIOS** (power-cycle into a sibling 512-KiB image,
save files re-keyed). All persisted to a flat TOML-subset config, **portable-first**: a `jupiter.toml`
beside the executable wins (portable/self-contained archive), falling back to
`$XDG_CONFIG_HOME/5thplanet/jupiter.toml` (a committed
`jupiter/jupiter.toml.example` documents every key; CLI flag > config >
autodetect).
Basic hot-plug SDL2 GameController support (fixed Xbox-style mapping, OSD
navigation); per-button gamepad rebind + analog devices ride with M13 E2.
Related fix: no disc now reports `NODISC` (0x07), matching MAME.

## Milestone 10 — Live physical disc + CDDA→SCSP ✅

- **`SectorSource` trait** decouples the CD-block from the in-memory `Disc`;
  `CdBlock.disc` is `Option<Box<dyn SectorSource>>` (drops `Clone`).
- **CDDA→SCSP** — audio sectors decode to a CD-DA FIFO consumed as the SCSP
  **EXTS inputs** (see M11), pulled through a ~0.5 s pre-roll jitter buffer
  (`take_cd_audio_buffered`, `87b85e9`) that absorbs the burst-vs-steady
  mismatch. Debug hooks `dbg_play_cdda`/`dbg_play_first_audio_track` (jupiter
  F8, **debug builds only** — `#[cfg(debug_assertions)]`) play CD-DA without a
  BIOS Play.
- **`physdisc` crate** — feature-gated libcdio `SectorSource` (the sole unsafe
  crate, ADR-0009); verified booting VF2 from a real drive on `/dev/sr0`.

## Milestone 11 — Boot a game to gameplay ✅ (2026-06-10, tag `vf2-good-emulation`)

**Virtua Fighter 2 is fully playable at a steady 60 fps** (title → menus →
character select with CD-DA BGM → 3D fights to the K.O. screen, correct
graphics/audio balance/pacing, user-verified). ***Doukyuusei ~if~* is also
fully playable** (GFX, SFX, voices — user-verified 2026-06-11, native 640×224
hi-res). Pursued purely on the **real-BIOS LLE path**, trace-diffed against
Mednafen (itself LLE — the only mode where a master-SH-2 PC-trace diff is
valid). An opt-in HLE direct boot (ADR-0010/0011) was tried and **removed**.

Fix chain (each with regressions; details in the commits):

- **Boot**: data-transfer state machine + Seek/Init handling; `DCHG` cleared by
  the host W1C, never re-raised at Init (the recognition-loop root); region
  autodetect; the `cmd_log` command-level CD trace-diff methodology.
- **Post-boot**: SH7604 BCR1 master/slave bit (`1f584d6`); `run_frame` = one
  `run_for(CYCLES_PER_FRAME)` (`0b78733`); Mednafen scheduler alignment —
  master-leads-slave interleave (`b583cc4`) + per-instruction SCU interrupt
  sampling (`70f4049`); per-sector CD periodic during PLAY (`cacffca`);
  Mednafen `Drive_Run` drive-phase port (`d0640a5`) + recognition spin-up
  (~1 s BUSY, `e2884e7` — the BIOS boot animation plays).
- **VF2 intro → title**: stop-then-Play seek origin from the physical pickup
  (`fb52d0c`); the periodic report must not clobber a half-composed command +
  Get Subcode 0x20 + the BUSY seek-settle (`c1b4dc9`); VDP1 TVM 8bpp + DIE
  interlace + VDP2 8-bit sprite decode (`0dd3ddd`, savestate v4); sprite
  SPCAOS CRAM offset (`5a192c0`); hi-res window-X scaling (`caec91f`).
- **VF2 audio**: CD Play track/index form (`9d46803`); CD-DA paced at 1×
  (`b08d100`); MSB-first (cdrdao) rip detection warning (`072fea2`); SH-2→SCSP
  B-bus wait-states (`7dfbfab` — a 0-wait SCSP let VF2's sound-submit timeout
  latch a permanent "sound wedged" flag, muting all SFX); CD-DA through the
  SCSP **EXTS** inputs at the game-programmed mix (`7ac3837`); KYONEX re-keys
  only a Release-phase EG (`e963b19`).
- **SH-2 core**: PC-relative fetches are slot-*legal* (branch family only is
  slot-illegal), with slot base = branch target + 2 when taken (`1d49088`).
- **Doukyuusei**: FRT `FTCSR` write-0-to-clear (`073805d` — the inter-CPU FTI
  handshake root); VDP2 hi-res rendering (`c0f2344`); SCU-DSP DMA cache-through
  mask (`ea20509`) + DSP effect-send scaling/MIXS wrap (`845f611`) + the SCSP
  slot CA monitor `0x408` (the boot-jingle loop root) for the boot jingle;
  SCU indirect-DMA table-pointer alias fold (`bfd6240`) + VDP1 fb hi-res
  horizontal doubling (`4b93204`) for the in-game menu.
- **Performance** (follow-on, bit-identical): renderer `FrameCtx` hoists +
  scanline-band parallel composite (`1e1e115`), emu-thread decoupling, fight
  hot-path cuts (`03e842a`), audio-pacing mirror credit (`021dab5`) — VF2
  fights 13.8 → 34.7 fps full-path, displayed ceiling ≈63 fps = real-time.

General accuracy fixes spun out (independent of any boot path): scheduler
cycle-resync on un-halt; inter-CPU FRT input-capture (FTI) regions; SCU IST
cleared on the acknowledge cycle; event-edge-clamped scheduler batching; CD
status `is_cdrom` bit; CD-block SCU external interrupt (vector 0x50, level 7,
`57a1066`); the **`sdbg`** interactive debugger (`crates/debugger` — see
project conventions).

## Milestone 12 — Whole-system cycle accuracy ✅ complete (2026-06-12)

Close residual whole-system timing gaps vs the LLE reference (Mednafen) so
timing-gated behaviour matches even when code/data are byte-identical.

> **BGM resolved 2026-06-06** — the BIOS CD-player BGM silence was **not** a
> timing gap but an `m68k` decode bug (`32662f7`): `ADDA.L`/`SUBA.L Dn,An`
> mis-decoded as ADDX/SUBX (the guard must exclude opmode `0b11`), collapsing
> the sound driver's note-ring to 2 entries. Found by a cross-emulator
> note-ring diff; regression `m68k/tests/ring_offset_repro.rs`. The timing
> items below stand on their own merits.

| # | Task | Status |
|---|------|--------|
| 1 | SCSP timer free-running 8-bit model + clock verification + measurement harness (ITRACE/seq-tick counter) | ✅ (`d7f5444`) |
| 2 | Disambiguate the 83-seq-tick gap: the Timer-B *period* matches Mednafen exactly (88.0) — the gap is pure trigger-time, ruling out rate-class causes | ✅ (`0d3455f`) |
| 3 | 68k↔SCSP per-sample interleave granularity | ⏸ not the BGM root (→ landed as M13 A2) |
| 4 | 68k instruction cycle audit | ⏸ not the BGM root (→ landed as M13 D4) |
| 5–6 | VDP1 draw-slowdown hypothesis: `Write_/Read_CheckDrawSlowdown` ported (`9934411`) but proven a no-op for the BGM (our modelled draw duration was ~100× too short to overlap). **Draw-DURATION model landed (`ce1ec2c`, savestate v9):** a Mednafen-faithful draw-cycle walk (`vdp1/timing.rs` — per-command fetch/setup, per-span setup + pre-clip, per-pixel 1/6 cy + AA + the drawn_ac early exit, AdjustDrawTiming ×(1+48/256), persistent clip/local registers) sizes `begin_plot`; validated vs mednaref `SS_VDP1DRAW` (boot anim 96,778 vs 97,206 cy @654 cmds; CD-player panel 258,132 vs ~258,400 cy @226 cmds). The RW slowdown itself is now **opt-in** (`set_rw_slowdown`, default off) — in the oracle it's a per-game hack (`HORRIBLEHACK_VDP1RWDRAWSLOWDOWN`: VF1 yes, VF2/BIOS no). Confirmed not the BGM-phase lever (seq-ticks unchanged) | ✅ duration model landed (2026-06-12) |
| 8 | **Per-access BSC bus-timing model** — faithful Mednafen `BSC_BusRead/Write` port, done bus-side (`57cbfe5`+`006187a`, savestate v6): CS0 16-bit per-transaction costs, CS3 SDRAM + write buffer + line-fill burst + turnaround, A-bus from live ASR0, B-bus flat totals, shared bus timestamp (CPU↔CPU arbitration). Golden unchanged; BGM phase 4179→4204 vs oracle 4497; VF2/Doukyuusei stable. Residual gap chased below. **Both remaining refinements landed 2026-06-12:** (a) **exact B-bus deferred-write serialization** (`6973ce8`) — a B-bus write hands off in +2 CPU cycles and posts its device-side completion (SCSP +17/+13, VDP1 +9/+1, VDP2 +3/+1 per 16-bit half) on `BusTiming::bbus_write_finish`, which only the *next B-bus access* waits out; B-bus reads are always two 16-bit halves (VDP1 28/VDP2 40 — the flat model undercharged those by half); (b) **SCU A/B/C-bus DMA arbitration** (`a101f15`) — DMA-timeline costs corrected to Mednafen `dma_time_thing` (B-bus VDP1/VDP2 **1**, SCSP 13 per 16-bit access; C-bus read 6/write free — the old flat values overpriced DMA writes up to 11×), and a C-bus-endpoint transfer halts both SH-2s for its paced duration (`RecalcDMAHalt`/`SetExtHalt`) while a pure A↔B transfer halts neither (the DMA-end interrupt timestamp at trigger-time is the documented boundary). **⚠️ The both-CPU-HALT portion was SUPERSEDED 2026-06-26 (`64237d7`): `drain_dma` now returns 0 — a synchronous immediate copy with no SH-2 halt charge, since charging a non-time-running DMA as a CPU stall double-counted; the `dma_time_thing` per-access costs + DMA-end-at-trigger stay.** | ✅ landed (2026-06-11/12; halt superseded 2026-06-26) |
| 9 | Validate: BGM phase matches, *Doukyuusei* stable, master PC-trace aligned over a multi-second run, golden + suite green | ✅ (2026-06-12) BGM-phase seq-ticks **+182 → −47 vs the oracle** (4450 vs a same-day re-measured 4497, ~1%): the DMA cost model (#8b) was the dominant term. The residual decomposes into (a) a **discrete ~14-frame recognition-handshake offset** (boot-anim start f125 vs oracle f111; recognition/INTBACK `REVIEW(magic)`-class — ours' Startup also skips the oracle's post-spin-up `StartSeek(0x800096)`; a 2026-06-19 cross-emulator check confirmed that auto-seek is **Mednafen-only** — MAME `stvcd`/`saturn_cd_hle` and Yabause `cs2.c` both settle straight to PAUSE@150 with no seek, matching ours, and no servo doc exists per ADR-0015, so this is left as-is) and (b) a **diffuse component dominated by a 68k-gated mailbox poll loop** (master PC `0x06032D02`, store-then-poll-until-cleared) — poll-loop *phase*, i.e. oracle-approximation territory per the M4 stop rule. Both documented as follow-up threads, not per-instruction cost errors. `bios_boot` golden unchanged through all of M12; Timer-B period locked (88.0009); VF2 trajectory (late_game f999, 0 stalls) + Doukyuusei title BGM (avg \|amplitude\| 1203) healthy |

The full BGM/phase trace-down (probes, lockstep tools, refuted hypotheses) is
in `doc/system-architecture.md` §9, Part B.7.

## Milestone 13 — Hardware completeness & fidelity backlog 📋

Output of a full per-chip architecture audit (2026-06-04). The emulator is
"boot-complete" but not "hardware-complete": none of these block the current
targets (four games fully playable — VF2, *Doukyuusei ~if~*, *Sangokushi V*,
*Panzer Dragoon Zwei*),
but together they are the path to broad
compatibility + full cycle-accuracy. A **prioritized backlog** — tasks are
pulled when a game or accuracy need surfaces, golden-safe throughout.

**Tier A — whole-system timing** (extends M12; push closed 2026-06-04)

| # | Gap | Status |
|---|-----|--------|
| A1 | Continuous event timeline (kill batch-drain jitter) | 🚧 VDP1 draw-end, SCU Timer-0, and FTI converted to exact events. **★ SH-2 on-chip FRT/WDT timers + INTC now event-driven** (`d2f2b0e`/`ef6bf19`/`c643fce`/`e6b3d72`, savestate v10): the per-instruction `advance_timers` + `refresh_interrupts` are gone — the FRT/WDT use Mednafen's lazy materialize (`(now>>shift)-(lastts>>shift)`, scheduled by `next_ts`; register access catches them up) and the INTC is recomputed only on change (timer events, on-chip writes, DMAC TE, FTI). Ported in 4 golden-invariant-by-construction stages; bit-identical (golden unchanged, both games play-tested); poll-scene per-instruction timer/INTC overhead ~11%→~1.3%. ⚠️ **Write-triggered SMPC mid-batch dispatch was tried (`b65cd18`) and REVERTED (`4d0c67f`)** — breaking the batch on `smpc.has_pending()` re-anchored `run_frame`'s grid and black-screened Doukyuusei; SMPC commands still drain at the batch boundary. **HBlank clamp edge + lift `SMPC_POLL_QUANTUM`: DEFERRED with evidence** — the `raster_jitter_probe` (an observer-only screen comparing each VCNT/TVSTAT read's batched value to the cycle-exact `raster_state`) found **0 stale reads** across BIOS boot + a VF2 fight + the Doukyuusei menu (VBLANK, the bit games poll, is already an exact clamp edge; HBLANK/VCNT are never read stale — games use the HBlank-IN interrupt). Re-open the SMPC poll-quantum lift only via the event template the FRT/WDT port established (not a batch break); re-open HBlank only if a game's oracle diff points at it |
| A2 | SCSP per-sample interleave with the 68k | ✅ (`d539341`) |
| A3 | SCU-DMA cycle-stealing (CPU stalls for the real transfer cost) | ✅ (`80551c2`+`7d997b1`) — **CPU-halt portion reverted 2026-06-26 (`64237d7`): DMA completes synchronously with no SH-2 halt charge (see M12 #8b)** |
| A4 | Bus contention / VDP timing | ✅ base B-bus waits (`864ce3b`); VRAM *contention* deliberately dropped (the oracle has none); shared-timestamp CPU↔CPU arbitration resolved by M12 #8. Remaining items closed by M12 #8 residuals (2026-06-12): B-bus deferred-write serialization + SCU A/B-bus DMA arbitration |
| A5 | Real HBlank dot-count (per-mode `HTimings`) | ✅ (`7207810`) |
| A6 | VDP1 command-list divergence | ✅ closed — refuted: frame-aligned diff proves the lists match byte-for-byte; the BGM lead is not a VDP1 phenomenon |
| A7 | Sound-68k timing | ✅ 2 fixes (`729bfc3` sound-RAM access wait, `d755708` interleave budget carry), oracle-validated; built the cross-emulator signal "oscilloscope" (`tools/scope_diff.py`) |

**Tier B — SCSP audio features** ✅ all done: B1 LFO (`b1085eb`), B2 slot-to-slot
FM (validated by the boot jingle), B3 misc (slot monitor 0x408, MVOL, MIDI
empty-status, DAC18B/MEM4MB faithful no-ops).

**Tier C — VDP2/VDP1 rendering features**

| # | Gap | Status |
|---|-----|--------|
| C1 | VDP2 mosaic (MZCTL) | ✅ (`8419717`; sprite mosaic TODO) |
| C2 | Shadow gated by SDCTL per-screen enable | ✅ |
| C3 | Line-colour screen + back-screen register | ✅ (simplified line-colour model) |
| C4 | Special priority + special colour-calc (SFPRMD/SFCCMD, all modes) | ✅ (2026-06-08) |
| C5 | Windows + rotation edge cases | 🟡 sprite window, CRAM mode 3, RPMD 0–3, per-dot coefficients (DKAx walk, RDBS bank grants, CRKTE CRAM tables, mode-3 Xp) all done (`5ee3ecb`+`ac712a8`, VF2's floor; in DD interlace the rotation accumulators advance per *field* line). Deferred: dual-parameter window selection, screen-over mode 1 |
| C6 | VDP1 framebuffer TVM modes | 🟡 8bpp + DIE interlace done (`0dd3ddd`); TVM=3 (8bpp+rotate layout) deferred |
| C7 | Colour offset (CLOFEN/COA*/COB*) | ✅ (deliberate golden re-baseline → `0x0B1BA6E5180766F7`; validated by Doukyuusei's logo fade) |
| C8 | NBG0/1 reduction (ZMXN/ZMYN) + fractional scroll | ✅ |
| C9 | Extended colour calc (3-layer) | 🟡 non-line EXCC done; line-colour variants + gradient blend deferred |
| C10 | VRAM cycle-pattern fetch gating | 🟡 fetch gating done (validated by the unchanged splash golden); reduction-limit deliberately excluded (Mednafen uses a per-game whitelist — an oracle hack); bitmap-CG + rotation fetch path deferred |

**Tier D — CPU & SCU peripherals** ✅ complete (2026-06-07): D1 SH-2 DIVU 39-cy
latency + overflow IRQ; D2 on-chip DMAC transfer engine (Mednafen
`DMA_DoTransfer` semantics); D3 address-generation interlock (unified with
load-use, locked in by tests); D4 68k exact MUL/DIV cycle tables +
address/bus/trace exceptions with the group-0 frame; D5 SCU Timer 1 +
HBlank-IN + DMA-illegal (Pad interrupt and refresh registers deliberately
matched to the oracle's no-op).

**Tier E — input devices**

| # | Gap | Status |
|---|-----|--------|
| E1 | Multitap + port-2 scanning | ⬜ |
| E2 | Analog peripherals (3D pad, Mission Stick, racing) + per-button gamepad rebind | ⬜ |
| E3 | Specialty peripherals | 🟡 Shuttle Mouse done (`638cda7`/`80b7120`, savestate v5, `--mouse[=1|2]`); light gun + keyboard remaining |

**Tier F — already-deferred:** F1 MPEG card ⏸ · F2 CD move/copy sector ops ⏸ ·
F3 SH-2 cache address/data array spaces (open bus today; rare outside
cache-as-RAM).

**Tier G — residual reference-audit items.** Consolidated from the point-in-time
MAME + Mednafen cross-reference audits (2026-06-08, since retired — their
boot-critical findings all landed; these are the small open remainders). None
block the current targets; each is golden-safe and pulled when a game needs it.
The deliberate, *do-not-regress* divergences from MAME/Yabause those audits
recorded now live in [`system-architecture.md`](system-architecture.md) §9,
Part C.1.

**Triage (2026-06-14 re-verified at HEAD):** all rows still hold. **G2 and G3
are the two most likely to actually surface** (both audio, both plausibly hit by
a real sound driver) — check them first if a future game has a sound bug. G2/G6
carry real regression risk (the current behaviour is load-bearing) — fix only
with a repro. (G1, CHD disc images, was implemented in v0.8.0 and **removed**
after v0.9.0 — the `chd` dependency wasn't worth it; convert `.chd` → CUE-BIN
with `chdman extractcd`. The G2–G7 IDs are unchanged.)

| # | Gap | Status |
|---|-----|--------|
| G2 | SCSP `SNDON` does a full 68k reset, not an un-halt | ⬜ a `SNDON`-after-running re-resets the sound driver; want a `SetExtHalted`-style gate (`scsp/mod.rs:~1589`). **Risk: the full reset is currently load-bearing for working BGM — needs a repro before touching** |
| G3 | SCSP per-sample interrupt (SCIPD/MCIPD bit `0x400`) never generated | ⬜ only timers A/B/C + MIDI pend SCIPD (`scsp/mod.rs:~580`); a driver clocked off the per-sample tick gets no tick (both MAME and ours skip it) |
| G4 | SCSP sound-IRQ level picks one source by priority, not the OR of enabled SCILV levels | ⬜ `recompute_irq`/`decode_sci` (`scsp/mod.rs:~599`); very low impact (needs simultaneous sources at different levels) |
| G5 | VDP1 erase targets the *draw* buffer, not the displayed (non-draw) buffer at swap; `BEF` status flag always 0; `CEF`-clear-on-swap nuance | 🟡 **`CEF` itself is done** (latched on draw-end, cleared at list-start); the residue is erase-on-displayed + `BEF` + MAME's extra clear-on-swap. All edge cases |
| G6 | VDP2 VBLANK-clear ~1-line phase; ODD bit should be constant 1 in progressive (LSMD≠3) | 🟡 **VBlank-OUT itself is an exact clamp edge now** (`cycles_to_next_vblank_out`); residue is the 1-line VBLANK-*clear* phase + ODD-toggles-always (`system.rs:~828`). Marginal, golden-risk |
| G7 | SCU Timer0 missing the free-running HCNT counter mode; indirect-mode DMA write-back address; DMA-illegal predicate same-bus/unmapped vs MAME's BIOS-source key | 🟡 **Timer0 line-compare *does* fire** (the common mode, `system.rs:~888`); DMA-illegal predicate is test-covered, just unverified vs a BIOS-source DMA |

## Performance (opt-in "fast mode" — future)

Accuracy stays the default and the trace-diff baseline; never a JIT/dynarec.
Levers catalogued from how Mednafen stays LLE at full speed:

| # | Lever | Status |
|---|-------|--------|
| P2 | Optimized interpreter dispatch | 🟢 partly landed, bit-identical: decode LUT, INTC O(1) cache, interrupt re-arm early-out, cache hit-path copy elimination. Remaining: `step` dispatch, fastmap-style bus page table |
| P4 | Build & profile | 🟢 profiled (`bench_fps`/`bench_stages`/`bench_cache`/`bench_vf2_fight`); PGO/LTO remaining |

(Other levers were catalogued and dropped 2026-06-12 — accuracy-affecting
sync/model shortcuts, and the Mednafen-style video-output levers, of which
per-field interlace rendering (P5) was implemented and reverted by user
choice: the bare weave showed ghosting in play-testing; see `4284c1c`/
`fe70809` and the git history of this section. Current performance is
sufficient without them.)

Plus the accuracy-neutral frontend lever already landed: the render-pipeline
worker thread (`757f164`) overlaps VDP2 compositing onto a second core
(displayed frame trails by 1, pixels bit-identical).

Save/load latency (accuracy-neutral, `df02192`): the disc fingerprint — the
FNV-1a media identity the save-state header checks — is now computed once at
`Disc` construction and cached in a field, not re-hashed over the whole image on
every `save_state`/`load_state`. That re-hash was a ~1.5–1.7 s stall per
quicksave **and** per quickload on a 600–700 MB image; the cost now falls once at
disc-insert (an already-slow load), so quicksaves are instant. Same hash value —
bit-identical, golden + savestate round-trip unchanged. (Measured-and-rejected at
the same time: a per-frame CRAM→RGB888 LUT in the compositor — bit-identical and
it did cut `color_rgb888` self-time 2.3→0.1%, but the end-to-end fps gain was
within noise since render is the band-parallel edge and both heavy scenes already
clear 60 fps; re-land only for a heavier-NBG/bitmap game or a low-core host.)

## Later milestones (queued)

- **Precompiled binary packages (download-and-run distribution).** Ship
  self-contained `jupiter` frontend binaries on GitHub Releases so users don't
  need a Rust toolchain.
  - **Crux — SDL3 linking:** currently dynamic (`sdl3` via system pkg-config),
    so a bare binary needs the host's `libSDL3`. Use `sdl3-sys`'s `static-link`
    (with a vendored/CMake SDL3 build) so *release* artifacts statically link
    SDL3 (self-contained, no system SDL3) while local dev keeps the fast dynamic
    pkg-config build. Cost: a C toolchain/CMake in the build env, +~2–4 MB.
  - **Automation:** no CI exists yet → use **cargo-dist (`dist`)**: one
    `[workspace.metadata.dist]` block generates a GitHub Actions workflow that
    builds the per-platform matrix on the `v*` tag push (which the
    `release-engineering` flow already creates), making archives + SHA256 +
    installers and uploading to the Release.
  - **Platforms:** Phase 1 = Linux x86_64 + Windows x86_64 (free runners; mark
    Windows **experimental/untested** per "Linux-verified, others untested").
    Phase 2 = macOS x86_64/arm64 (needs a tester) + optional musl fully-static
    Linux.
  - **Legal (non-negotiable):** (1) the **BIOS is never shipped** — release notes
    must state the user supplies their own legally-obtained dump (the `bios/`
    policy); (2) keep **`physical-disc`/libcdio OFF** in every distributed binary
    — libcdio is GPL and the project is MIT, so default-feature builds stay
    MIT-clean (a release build must not enable it).
  - Archive contents: the `jupiter` binary (optionally `sdbg`), README,
    LICENSE (MIT) + bundled SDL3 zlib licence, the BIOS-not-included note,
    SHA256SUMS.
- **CRT-shader presentation via SDL_GPU.** **CRT shader v1: DONE** (`feat f635aea`,
  user-verified on the SEGA/SATURN splashes + Doukyuusei) — a single-pass, **flat**
  CRT (scanlines + aperture-grille mask + gamma; no curvature) on the SDL_GPU
  backend, selectable via the OSD Shaders chooser (None / CRT) + the `shader`
  config key. Project-authored GLSL → SPIR-V in `jupiter/src/shaders/` (committed
  `.spv` + `include_bytes!`, so normal builds need no `glslc`); `present` runs a
  fullscreen-triangle render pass over the frame when CRT is on, else the blit.
  The two gotchas were real and fixed: SDL_GPU's fixed SPIR-V descriptor sets
  (fragment sampler `set=2`, uniforms `set=3` — wrong set = silent black) and the
  swapchain Y-down flip (`crt.vert.glsl` flips V). Follow-ups: multi-pass
  bloom/halation, barrel curvature, **DXIL/MSL for non-Vulkan hosts** (`build_crt`
  is already format-agnostic via `ShaderKind::crt_shaders` — cross-compile the GLSL
  with `SDL_shadercross`, commit the `.dxil`/`.msl` + a match arm; a non-Vulkan host
  meanwhile falls back to the blit), and loading user `.spv`/preset shaders. SDL3's `SDL_GPU` (Vulkan/Metal/D3D12,
  multi-pass render targets, SPIR-V shaders — exposed by `sdl3::gpu`, *no new
  dependency* now that the frontend is on SDL3) makes a high-quality CRT filter
  feasible: Sony Trinitron-style aperture grille + scanline beam + bloom/halation
  + gamma. Plan: a `Presenter` trait keeping the current `SDL_Renderer` blit as
  the default/fallback (and the only path where Vulkan/Metal/D3D12 or a GPU is
  unavailable), plus an opt-in SDL_GPU CRT presenter (framebuffer → texture →
  1–3-pass shader → swapchain) selected via config. Presentation-only — the
  framebuffer stays bit-identical, so accuracy is untouched. Shaders authored
  GLSL → SPIR-V (precompiled, or `SDL_shadercross`; DXIL/MSL for non-Vulkan
  hosts). De-risk with a passthrough-shader spike first; the `--backend`
  render-driver selector is the groundwork. **Capability detection: DONE** (behind
  the off-by-default **`gpu-presenter`** build feature — the GPU code + the OSD
  "Shaders…" stub are preview-only until the CRT passes exist, so they're hidden
  from normal builds; `cargo build --features gpu-presenter` to work on them)
  (`jupiter/src/present_gpu.rs`) — the `gpu` config key / `--gpu` flag
  (`off` default / `auto` / `on`) attempts `sdl3::gpu::Device::new` for the host's
  shader format (SPIR-V/DXIL/MSL), falling back to the `SDL_Renderer` blit;
  `unsafe`-free because `Device::new` returns a `Result`. (The standalone probe +
  its `GpuCapability` verdict were later folded into the presenter's constructor —
  building a `GpuPresenter` *is* the capability check.) **Vulkan presenter
  self-test: DONE** (`feat d108bb6`,
  `gpu-presenter` only) — `jupiter --gpu-selftest` is a contained one-shot that proves
  SDL_GPU works as an alternative presenter to the `SDL_Renderer` blit **with no
  shaders authored**: it claims a Vulkan (SPIR-V) device for a fresh window, then each
  frame uploads an animated test pattern to an `R8G8B8A8` GPU texture (transfer buffer
  + copy pass) and posts it to the swapchain via SDL's built-in `SDL_BlitGPUTexture`
  (which carries its own blit shader), letterboxed to 4:3. This validates the riskiest
  plumbing — `with_window` swapchain claim → `map`/`upload_to_gpu_texture` →
  `wait_and_acquire_swapchain_texture` → `blit_texture` → `submit`, all `unsafe`-free
  in sdl3-rs 0.18.4 — on real hardware (verified NVIDIA RTX 3060: device created, 311
  frames presented, clean exit). The built-in blit sidesteps shader authoring entirely
  for the proof; the full CRT presenter below still needs the authored multi-pass
  shaders. The normal `SDL_Renderer` path is untouched (the self-test returns before
  `run()`). New pure helpers + unit tests: `letterbox_rect` (4:3 centred-fit geometry)
  and `fill_test_pattern` (animated opaque RGBA). **Alternative presenter backend: DONE**
  (`feat e7c119f`, user-verified on Doukyuusei + VF2; **stays behind `gpu-presenter`**)
  -- `gpu=auto/on` now presents the **live emulator** via `GpuPresenter` (a
  Vulkan/SPIR-V `Device` + a SAMPLER frame texture + an UPLOAD transfer buffer)
  instead of the `SDL_Renderer` canvas: per frame it maps the transfer buffer,
  uploads to the texture (copy pass), then `wait_and_acquire_swapchain_texture`
  + `blit_texture` + `submit`, letterboxed to 4:3 or stretched per `keep_aspect`.
  **No shaders authored** -- SDL's built-in `SDL_BlitGPUTexture` carries its own
  blit shader, which **supersedes** the planned authored-passthrough-shader spike
  (the built-in blit already does the 1:1 upload-then-present, simpler). The OSD
  is composited into the framebuffer on the emu thread, so the GPU path shows it
  (menus + toasts) for free. **Selection** is once at startup (`should_probe`):
  build a `GpuPresenter`, else fall back to the renderer (quiet for `auto`, warns
  for `on`). **Window-ownership crux** (as predicted): the two backends are
  mutually exclusive (an SDL_GPU device claims the window its swapchain owns), so
  the GPU presenter owns its own window and the present + window-control sites
  branch via the `backend_window*` accessors. **sdl3's `unsafe_textures` was
  deliberately NOT enabled** (user call): rather than cache the renderer `Texture`
  in a unified enum (which the borrow checker forbids without it -- the texture
  borrows its creator), the renderer canvas/creator/texture stay safe + auto-drop
  **sibling `Option` locals** beside `Option<GpuPresenter>`, and the sites branch.
  **Default stays `off`** -- a GPU-vs-renderer pixel-path swap is user-visible, so
  per the per-field-interlace lesson it is **not** auto-defaulted to `auto`.
  **Software-Vulkan rejection: DONE** (`feat 496fe67`) — `GpuPresenter::new`
  builds the device through `Properties` with `requirehardwareacceleration = true`,
  so SDL refuses a software Vulkan (Lavapipe/llvmpipe) at creation and the caller
  falls back to the renderer (`unsafe`-free via sdl3-rs's `Setter` +
  `new_with_properties`; verified with `VK_DRIVER_FILES=lavapipe`). Remaining
  follow-up: read the chosen backend (`SDL_GetGPUDeviceDriver`, still unwrapped in
  sdl3-rs 0.18.4 → needs `unsafe`) only to **label** it in the log — cosmetic.
  **Device entry point:**
  `SDL_CreateGPUDevice(format_flags, debug_mode, name)` /
  `SDL_CreateGPUDeviceWithProperties` (safe-wrapped by `sdl3::gpu` — no `unsafe`
  despite the workspace `forbid`). The `name` picks the **backend**
  (`vulkan`/`direct3d12`/`metal`/null), **not** a physical GPU — SDL_GPU has no
  integrated-vs-discrete device selector; the closest knob is the property
  `SDL_PROP_GPU_DEVICE_CREATE_PREFERLOWPOWER_BOOLEAN` (default **false** →
  already prefers the performance/discrete GPU). Pinning the discrete GPU on a
  multi-GPU host is **OS/driver-level**, not an SDL flag: Linux `DRI_PRIME=1` /
  NVIDIA `__NV_PRIME_RENDER_OFFLOAD=1` / `MESA_VK_DEVICE_SELECT`; Windows per-app
  Graphics settings or the `NvOptimusEnablement` / `AMD PowerXpressRequestHighPerformance`
  export convention; macOS via Metal. (Alternative: `librashader` — a pure-Rust,
  Cargo-native, verbatim `.slangp` runtime that would run the whole `slang-shaders`
  corpus as-is; reconsidered 2026-06-27 and **not** chosen — the real trade is its
  MPL/GPL copyleft + a heavier dep tree (`ash`/`wgpu` + `glslang`/`naga`/
  `spirv-cross2`) + a separate GPU context vs SDL_GPU's in-dependency, permissive,
  in-SDL3 path. SDL_GPU's cost: reimplement the slang multi-pass runtime. See
  ADR-0019 "Revisited 2026-06-27".)
- MPEG card + CD move/copy sector ops (deferred from M7).
- **Explicitly never** — JIT / dynarec (accuracy over performance is the
  project's design axis).
