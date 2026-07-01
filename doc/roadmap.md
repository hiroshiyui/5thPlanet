# 5thPlanet Roadmap

Accuracy-first SEGA Saturn emulator in Rust. Milestones are scoped tightly so
the foundation is solid before the next chip is added. **This file is a status
tracker** — blow-by-blow investigation history lives in the git log,
`doc/system-architecture.md` §9 (Bootstrapping), and the referenced commits.
Companion docs: playable titles in
[`compatible-game-titles.md`](compatible-game-titles.md); boot-blocker case
studies + forensic files in [`debugging-playbook.md`](debugging-playbook.md).

**Status:** M1–M12 ✅ complete · M13 (hardware completeness & fidelity backlog)
📋 in progress · latest release **v0.18.0**. **Five commercial games are fully
playable** — *Virtua Fighter 2* (60 fps, tag `vf2-good-emulation`),
*Doukyuusei ~if~*, *Sangokushi V* (三國志V), *Panzer Dragoon Zwei*, and
*Greatest Nine '98* (see [`compatible-game-titles.md`](compatible-game-titles.md)).

Test count: **1207 workspace-wide, 0 failures** (default features; +2 with
`--features gpu-presenter`), ~85% line coverage (`cargo llvm-cov`; excludes the
SDL3 frontend and the FFI `physdisc` crate).

## Component status

Legend: ✅ complete · 🟡 partial (usable core, refinements pending) · 🔶 stub · ⬜ not started.

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
| Save states | ✅ `save_state`/`load_state` (bincode + versioned header, currently v15); media referenced not embedded, fingerprint-validated |
| Backup RAM (battery) | ✅ Internal 32 KiB, hardware odd-byte packing, persisted to `<bios>.bup` |
| On-screen menu (OSD) | ✅ Software-composited in-window menu (ADR-0008): save/load slots, reset, disc eject/insert + image browser, Settings (Graphics/Controller/Region/Cartridge/BIOS), persisted to `jupiter.toml` |

## Test & diagnostics infrastructure

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

## Completed milestones (M1–M12) ✅

All complete — summarized below for the record (commit-level history is in the
git log). **Active work is [Milestone 13](#milestone-13--hardware-completeness--fidelity-backlog-)
and [Performance](#performance-opt-in-fast-mode--future), further down.**

### Milestone 1 — Cycle-accurate SH-2 core ✅

Standalone `sh2` crate: full ISA (~142 ops), delay slots, exceptions, 5-stage
pipeline cycle model (load-use, multiply latency, branch costs), 4 KiB 4-way
cache, on-chip peripherals (INTC/DMAC/DIVU/FRT/WDT behavioral; BSC/SCI/UBC
storage stubs). ROM regression harness with committed golden hashes.

### Milestone 2 — Saturn bus, dual SH-2, scheduler ✅

`SaturnBus` typed regions + memory-map dispatch (open bus when unmapped), the
cache wired into live fetch/data paths, second (slave) SH-2, deterministic
event-driven `Scheduler` + `SchedEntity`, `Saturn` aggregate.

### Milestone 3 — SCU, SMPC, VDP2 minimal, SDL2 ✅

SMPC slave hold/release; SCU registers + 3 DMA channels + interrupt aggregator;
`scu_dsp` full VLIW DSP crate wired into the SCU host; VDP2 register bank +
VRAM/CRAM + minimal NBG0 renderer; SDL2 frontend shell; BIOS-boot golden test.

### Milestone 4 — BIOS splash on screen ✅ (2026-05-28)

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

### Milestone 5 — Chip build-out: VDP1, MC68EC000, VDP2 ✅

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

### Milestone 6 — SCSP audio ✅

Timers A/B/C + the 68k/main-CPU interrupt model (SCIEB/SCIPD/SCILV, MCIPD →
SCU); 32-slot PCM engine (OCT/FNS phase, loop modes, interpolation); ADSR + TL
envelope; DISDL/DIPAN mixer to 44.1 kHz stereo; SDL2 audio output; 128-step
effect DSP (MAC, delay line, PACK/UNPACK); SMPC digital pad from the keyboard.

### Milestone 7 — CD-block (HLE) + cartridge slot ✅

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

### Milestone 8 — Save states + battery backup RAM ✅

serde derives across the cores (feature-gated) and `saturn`;
`save_state`/`load_state` over bincode with a magic + version header; external
media `#[serde(skip)]`'d, re-grafted on load, fingerprint-validated; a
snapshot-then-equal-runs determinism test. `memory::BackupRam` (32 KiB,
hardware odd-byte packing, "BackUpRam Format" default) persisted to
`<bios>.bup`; F5/F9 frontend hotkeys. Deferred: state migration across version
bumps, rewind/run-ahead, compression.

### Milestone 9 — Frontend OSD ✅ (2026-06-11)

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

### Milestone 10 — Live physical disc + CDDA→SCSP ✅

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

### Milestone 11 — Boot a game to gameplay ✅ (2026-06-10, tag `vf2-good-emulation`)

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

### Milestone 12 — Whole-system cycle accuracy ✅ complete (2026-06-12)

Close residual whole-system timing gaps vs the LLE reference (Mednafen) so
timing-gated behaviour matches even when code/data are byte-identical.

> **BGM resolved 2026-06-06** — the BIOS CD-player BGM silence turned out to be
> an `m68k` decode bug, **not** a timing gap (`32662f7`), so the timing items
> below stand on their own merits. Full case study:
> [`debugging-playbook.md`](debugging-playbook.md) **CASE#10**.

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
targets (five games fully playable — VF2, *Doukyuusei ~if~*, *Sangokushi V*,
*Panzer Dragoon Zwei*, *Greatest Nine '98*),
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
| C6 | VDP1 framebuffer TVM modes | 🟡 8bpp + DIE-interlace plotting done (`0dd3ddd`); display-side **DIE field-weave** — the VDP2 compositor weaves the even/odd fields into one full-height image instead of line-doubling the current field (Mednafen per-field placement; default-on, opt-out `SAT_VDP1_NOWEAVE`) — done (`33ccf8a`→`b1bb3ce`, v0.17.0; fixed GN98's menu strobe, smoother VF2); TVM=3 (8bpp+rotate layout) deferred |
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
| E1 | Multitap + port-2 scanning | 🟡 core port-2 pad state done (H2e, `b424fec`: `Smpc::pad2` + per-port INTBACK report); **frontend per-port assignment + 2nd-controller feed done (E-1: `ports.rs` wired — `set_pad1`/`set_pad2` routed per port)**; remaining — the multitap/6-player adapter (E-3) |
| E2 | Analog peripherals (3D pad, Mission Stick, racing) + per-button gamepad rebind | 🟡 **3D Control Pad done — emulated (E-2a) + host wiring (E-2b)**: E-2a = `PortDevice::ThreeDPad`, analog INTBACK report (ID `0x16`, 6 bytes), `set_analog1/2`, savestate v16, oracle-faithful + tested. E-2b = `ports.rs` gamepad source carries an `analog` flag (token `gamepad3d:<guid>`), OSD CyclePort offers each pad in digital + 3D form, the SDL thread feeds stick/trigger axes (`route_analog` → `set_analog1/2`); same-physical-device skip across both modes. *Code-complete, gates green; needs play-test with a 3D-pad game.* **E-2d per-button gamepad rebind done**: config `gpad_*` tokens (SDL button/axis strings, `DEFAULT_GPAD`) + OSD **Controller → Gamepad Buttons** press-to-bind screen (`StartGamepadRebind`, captures button *or* trigger, Esc cancels); the SDL thread applies the token map (`gpad_pressed`) in place of the fixed table, left stick still doubles as D-pad. **⏸ E-2c (Mission Stick + wheel) DEFERRED** until a game surfaces the need — the project's LLE↔Mednafen method wants a real title as the verification oracle, and these have device-specific derived inputs that are error-prone to build blind (see the E-2c note below). |
| E3 | Specialty peripherals | 🟡 Shuttle Mouse done (`638cda7`/`80b7120`, savestate v5, `--mouse[=1|2]`); light gun + keyboard **⏸ deferred (E-4)** — each pulled when its title/peripheral surfaces |

**E — layered input-configuration plan** (📋 design; unifies the E1/E2/E3 work
above). A five-concern model — multitap → port enumeration → per-port controller
*type* → host-device binding → per-binding remap — split across the project's
existing sdl-free core seam. The two refinements over the naive five layers:
*list-ports* is not a step but a property of the topology tree, and *bind* +
*remap* are two edits on one `Binding` object whose map is keyed to the emulated
device **type**.

*Two-sided data model.*
- **Emulated side** (`crates/saturn/smpc.rs`, accuracy-critical, golden-safe,
  savestate-versioned): a **port-topology tree** — Port 1 / Port 2 each a direct
  device *or* a multitap with ≤6 sub-ports — plus the per-(sub)port **controller
  type** that fixes the INTBACK peripheral-phase **ID + report-byte layout**
  (today: pad `0x02` / Shuttle Mouse `0xE3`). Exact IDs/report formats come from
  the SMPC manual + Mednafen `smpc.cpp` — never invented.
- **Host side** (`jupiter`, sdl-free + unit-tested, OSD + `ports.rs`): a
  `Binding = (emulated port/subport + device-type) ← (host source GUID + capability
  map)`. The OSD "Controller" screen picks the host source (bind) and edits its map
  (remap); the map is **per emulated-device-type** (digital buttons / analog axes /
  relative deltas), so the OSD filters selectable host sources by capability — or
  synthesizes (e.g. digital keys → stepped analog) and says so. Bindings key on
  gamepad **GUID** so hot-plug re-attaches to the right port.

*Phased build (foundation-first; each phase ships something playable).*
| Phase | Side | Work | Closes |
|---|---|---|--------|
| E-1 | host | ✅ **done** — `ports.rs` wired into the frontend: 2-port device assignment + host binding for the Pad/Mouse types, multi-gamepad by stable SDL GUID, config `port1`/`port2` (legacy `mouse` migrated), OSD Controller **Port 1 / Port 2** `CyclePort` rows. The emu thread owns the `Ports` assignment + routes each frame's per-device input (`EmuIn::Input` keyboard + per-GUID pad bits) to `set_pad1`/`set_pad2`; the SDL thread is a pure sensor (hot-plug → `EmuIn::PadList`). Unblocks "P1 = gamepad, P2 = keyboard". | E1 frontend 2nd-controller feed (`set_pad2`) |
| E-2 | emulated + host | Analog controller **types** — 3D/Multi pad, Mission Stick, racing wheel: new INTBACK ID + report layout in SMPC (per-type INTBACK-layout regression tests, savestate bump); host capability-map + per-button gamepad rebind become load-bearing here. **E-2a + E-2b done**: emulated 3D Control Pad (`ThreeDPad`, ID `0x16`, savestate v16, tested) + host wiring (the `analog` flag on a gamepad source, `gamepad3d:` token, OSD digital/3D cycle, stick/trigger feed via `route_analog`). The layered type-vs-source split lands as "each pad appears twice in the cycle (digital + 3D)". **E-2d done**: per-button gamepad rebind (config `gpad_*` SDL tokens, OSD Controller → Gamepad Buttons press-to-bind capturing a button or trigger, `gpad_pressed` token map replacing the fixed table). **⏸ E-2c Mission Stick + wheel DEFERRED** — implement when a game needs it, using that title as the oracle. Wire formats already read from `mednaref/src/ss/input/` for a fast restart: **Wheel** ID `0x13` = 3 data bytes (2 button bytes + 1 wheel-position byte `0x01..0xFE`; the L/R **gear buttons derive from wheel position** via hysteresis — `wheel.cpp`). **Mission Stick** ID `0x15` (single, 5 bytes) / `0x19` (dual, 9 bytes) = 2 button bytes + stick X/Y + throttle (×2 for dual); **non-standard button bit layout** (B=6 A=7 C=8 START=9 L=10 X=11 Y=12 Z=13 R=14), the **D-pad directions derive from the analog stick**, plus **autofire** (afspeed) — `mission.cpp`. Both nibble-inverted per the standard INTBACK framing. | E2 |
| E-3 | emulated + host | **Multitap / 6-player — ⏸ DEFERRED** (build with a multi-player title as the oracle). Blocker found 2026-07-01: no clean local reference for the INTBACK **container-header** framing — mameref has no `read_saturn_ports` (our stated model), and Mednafen `input/multitap.cpp` is low-level serial nibble-negotiation, not a static byte layout. Reconstruction so far: each connected sub-pad = the standard `0x02` + 2 data bytes we already emit; an analog sub-device echoes its own `id2` (mouse `0xE3`); an empty sub-port = `0xF,0xF`; trailer `0x0,0x1`; the container header emits nibbles `0x4,0x1,0x6,0x0` (bytes `0x41 0x60`) whose mapping to our one-byte `0xF1` port header is unconfirmed. Host side also needs the `Ports` per-port sub-port tree + OSD. | E1 multitap |
| E-4 | emulated + host | Remaining specialty — **light gun** (raster-position latch) + **keyboard** (type-3 ID) — **⏸ DEFERRED**, each gated on its specific title/peripheral (same "pull when a game needs it" rationale as E-2c/E-3). | E3 |

*Status (2026-07-01).* Tier E's **broadly-useful work is complete** — per-port
assignment (E-1), the 3D Control Pad end-to-end (E-2a/b), and full gamepad
remapping (E-2d). The remainder — **E-2c** (Mission Stick + wheel), **E-3**
(multitap), **E-4** (light gun + keyboard) — is **⏸ deferred**, each pulled when
its specific game/peripheral surfaces to serve as the LLE verification oracle
(the wire-format research is captured in the rows above for a fast restart).

*Cross-cutting.* Every new device type / topology change bumps the savestate
version; the default port config stays Pad-on-1 so `bios_boot` + the game render
goldens hold (golden-safe); the host half stays sdl-free + unit-tested (the
`ports.rs` template), the emulated half covered by `smpc.rs` integration tests.

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

**Triage (updated 2026-07-01):** **G2, G3, G4 are ✅ done** (2026-07-01 SCSP
audit — all three were oracle-diffed against Mednafen, which decided each: G2 was
a mischaracterised guard, G3's "oracle skips it too" premise was wrong, and G4
fell out of G3's `RecalcSoundInt` port; all golden-safe). The remaining **G5–G7
are rendering/timing residues** (VDP1 erase/BEF, VDP2 VBLANK-clear phase, SCU
Timer0 HCNT) — lower audio exposure; **G6 still carries golden-risk** (the current
1-line VBLANK phase is load-bearing) — fix only with a repro. (G1, CHD disc
images, was implemented in v0.8.0 and **removed** after v0.9.0 — the `chd`
dependency wasn't worth it; convert `.chd` → CUE-BIN with `chdman extractcd`.)

| # | Gap | Status |
|---|-----|--------|
| G2 | SCSP `SNDON` re-reset a running 68k | ✅ (`d14abd7`) — the gap was mischaracterised: the fix is **not** un-halt-vs-reset but a **`!running` guard**. Mednafen `TurnSoundCPUOn` *also* does a full `SOUND_Reset68K`, but gated `if(!SoundCPUOn)` (smpc.cpp), so a redundant SNDON is a no-op. `Scsp::start` now returns early when already running; only the first SNDON after power-on / SNDOFF reloads the vectors. Golden-safe (bios_boot + 5 game render goldens unchanged); regression `sndon_while_running_does_not_re_reset_the_68k` |
| G3 | SCSP per-sample interrupt (SCIPD/MCIPD bit `0x400`) never generated | ✅ (`432a7a4`) — **the "both MAME and ours skip it" note was wrong about the oracle**: Mednafen `SCIPD \|= 0x400; MCIPD \|= 0x400;` every output sample (scsp.inc). Now pended in `tick_timers` (per batch); inert unless SCIEB/MCIEB bit 10 is enabled. Golden-safe; regression `one_sample_interrupt_pends_every_sample` |
| G4 | SCSP sound-IRQ level picks one source by priority, not the proper SCILV assembly | ✅ (`432a7a4`, landed with G3) — replaced the single-source priority chain with a faithful `RecalcSoundInt` port: the level is assembled bit-by-bit from all three SCILV planes (masked by active sources), and **sources above bit 7 collapse onto bit 7** (fixed a latent bug where Timer C read the nonexistent SCILV bit 8 → always level 0). Golden-safe; regressions `sound_irq_level_assembled_from_all_three_scilv_planes` + `sources_above_bit7_collapse_onto_bit7_for_their_irq_level` |
| G5 | VDP1 erase targets the *draw* buffer, not the displayed (non-draw) buffer at swap; `BEF` status flag always 0; `CEF`-clear-on-swap nuance | 🟡 **`CEF` itself is done** (latched on draw-end, cleared at list-start); the residue is erase-on-displayed + `BEF` + MAME's extra clear-on-swap. All edge cases |
| G6 | VDP2 VBLANK-clear ~1-line phase; ODD bit should be constant 1 in progressive (LSMD≠3) | 🟡 **VBlank-OUT itself is an exact clamp edge now** (`cycles_to_next_vblank_out`); residue is the 1-line VBLANK-*clear* phase + ODD-toggles-always (`system.rs:~828`). Marginal, golden-risk |
| G7 | SCU Timer0 missing the free-running HCNT counter mode; indirect-mode DMA write-back address; DMA-illegal predicate same-bus/unmapped vs MAME's BIOS-source key | 🟡 **Timer0 line-compare *does* fire** (the common mode, `system.rs:~888`); DMA-illegal predicate is test-covered, just unverified vs a BIOS-source DMA |

**Tier H — cross-chip silent-feature audit (2026-06-30).** ✅ **H1, H2, and H3
all landed — shipped in v0.18.0.** Output of a six-agent oracle-diff sweep run
after the Wachenröder RBG0 coefficient line-colour fix (`7e2341b`) — that bug
was a *silent* gap (an unread register bit/field that renders wrong with **no
panic and no log**), so this pass hunted the same class across every chip. Each
**H1/H2** row is verified by direct code inspection (not merely agent-reported).
None blocked the five playable titles; golden-safe throughout (`bios_boot` hash
+ all five game render goldens unchanged; the two pixel-visible H3 items were
play-test verified). Overlaps already on this backlog are cross-referenced, not
repeated (see "Already tracked" below); the remaining `⬜ deferred` items under
H3 are the only open Tier-H work.

**H1 — confirmed correctness bugs** (small, silent, high-value; several
contradict an existing ✅ — they are *refinements* of a tier marked done):

| # | Gap | Status |
|---|-----|--------|
| H1a | SH-2 NMI sets `SR.imask` = 0, not 15 | ✅ (`4273cd0`) — clamp the accepted level with `.min(15)`: NMI (synthetic priority 16) → imask 15, ordinary interrupts unchanged. Was `16 & 0xF = 0` → NMI handler fully preemptible. Regression `nmi_raises_imask_to_15_not_zero`; goldens unchanged |
| H1b | VDP2 colour-offset **sprite/back bits swapped** | ✅ (`fb50a1f`) — sprite/back now resolved in `apply_color_offset` as sprite=bit6 / back=bit5 (HW/oracle), NOT via `screen_bit()` (whose sprite=5 stays correct for the line-colour/shadow path). Was sprite=bit5/back=bit6 → sprite-only / backdrop-only fades misfired. Regression `color_offset_keys_the_back_screen_on_bit5_not_bit6`; goldens unchanged |
| H1c | VDP1 **16bpp MSB-ON** writes the sprite colour | ✅ (`c2d9830`) — added the 16bpp MSB-ON read-modify branch (dest MSB forced, source colour discarded, gouraud/colour-calc skipped), mirroring the already-correct bpp8 path; dropped the stray source-OR. Was painting a shadow sprite's flat CMDCOLR as a solid block. Regression `msbon_16bpp_sprite_flags_dest_msb_and_discards_its_colour`; goldens unchanged |
| H1d | SH-2 on-chip-DMAC vectors **VCRDMA0/1 unrouted** | ✅ (`0a0a3d8`) — `read32`/`write32` route 0x1A0/0x1A8 to `intc.vcrdma0/1` (vector masked to low 7 bits like VCRDIV). Was dropped by the DMAC arm → on-chip-DMAC transfer-end IRQ vectored through 0. Regression `vcrdma_routes_the_onchip_dmac_interrupt_vector`; goldens unchanged |
| H1e | SCU DMA **HBlank-IN/Timer0/Timer1 start-factors not wired** | ✅ (`71aa219`) — `trigger_dma_factor` now called at each event's raise site: HBlank-IN (2) + Timer-1 (4) in `scu.rs tick_timers`, Timer-0 (3) in `system.rs update_video_timing` (mirroring VBlank-IN/OUT). Was firing only factors 0/1/5/6 → HBlank/timer-paced DMA dead. Regressions `hblank_in_event_triggers_*`, `timer1_event_triggers_*`, `timer0_line_compare_triggers_*`; goldens unchanged |

**H2 — confirmed missing capabilities** (a whole feature/block absent):

| # | Gap | Status |
|---|-----|--------|
| H2a | SCSP slot **noise sound-source** (SourceControl, reg0 bits 8:7) | ✅ (`09017d1`) — `slot_sample` decodes SSCTL (reg0 bits 8:7) + SBCTL XOR (bits 10:9): source 0 = sound RAM (existing PCM, byte-unchanged), 1 = noise (shared LFSR low byte placed high), 2/3 = digital zero, then `^ SB_XOR_Table`. Was treating every slot as PCM → noise percussion/explosions/hats played sound-RAM garbage. No new state (LFSR already drives the LFO noise) → no savestate bump. Regressions `noise_source_slot_outputs_the_shared_lfsr`/`zero_source_slot_is_silent`/`sbctl_xor_inverts_the_noise_source`; goldens unchanged |
| H2b | SCSP **DMA engine** (regs 0x412–0x416 / RunDMA) + its DMA-end IRQ (SCIPD bit 4) | ✅ (`0009a65`) — faithful Mednafen `scsp.inc` RunDMA port: 0x412/0x414/0x416 decode DMEA/DRGA (word addrs) + DTLG + DEXE/DDIR/DGATE; `Scsp::run` drains the transfer synchronously (moves `dtlg` words sound-RAM↔reg-file, DGATE zero-fills), auto-clears DEXE, raises DMA-end (SCIPD/MCIPD bit 4) via `recompute_irq`. Reads: DMEA/DRGA/DTLG = 0, 0x416 = live DEXE/DDIR/DGATE (poll DEXE for completion). Was entirely absent → SCSP-DMA upload/clear no-op, a driver waiting on DMA-end could hang. savestate v14→v15. Regressions `scsp_dma_*` (×2); goldens unchanged |
| H2c | CD-block **FAD-search** (0x55 ExecuteFADSearch / 0x56 GetFADSearchResults) | ✅ (`d010936`) — faithful Mednafen `COMMAND_EXEC_FADSRCH`/`GET_FADSRCH` port (cdb.cpp:3411/3467): 0x55 scans partition `pnum` from list pos `offs` (0xFFFF=last) for the largest buffered FAD still ≤ target, latches {fad,spos,pnum}; 0x56 reads it back to CR1..CR4; bad/empty partition rejects (CMOK, no ESEL). Was falling to the default arm → CMOK "success" with no work/results. savestate v13→v14. Regressions `fad_search_*` (×3); goldens unchanged |
| H2d | VDP2 **normal (non-MSB) sprite shadow** | ✅ (`2879d98`) — `SpriteDot::Shadow(u8)` is priority-bearing: the compositor darkens the layer below only when the shadow wins the race (`spri >= top_pri`) + that layer's SDCTL bit is set (was unconditional). Normal shadow = colour code `== COLORMASK & !1` (Mednafen `nshad`, all types) → was a solid blob. New `Dot.self_shadow` halves an MSB self-shadow once (fixes 2× too bright). Pure-MSB shadow now honours TPShadSel (SDCTL bit 8). No savestate bump (render-time). Verified golden-safe: bios_boot hash + all 5 game render goldens pass. Regressions `sprite_normal_shadow_*`/`sprite_shadow_that_loses_*`/`sprite_msb_self_shadow_*` |
| H2e | SMPC **port-2 pad state** | ✅ (`b424fec`) — added `Smpc::pad2` + `Saturn::set_pad2`; INTBACK now reports each port's own pressed mask (was reporting pad1 for both → P2 mirrored P1). Default config unchanged. savestate v12→v13 (pad2 serialized). The **core data-model half of E1**; the frontend 2nd-controller feed + multitap remain under E1. Regression `intback_reports_port2_pad_independently_of_port1`; goldens unchanged |

**H3 — confirmed silent refinements** (lower current exposure, all verified-or-agent-reported):

- **SH-2:** ✅ **misaligned cached read no longer panics** (`de9a1b2`) — force-align `mem_read/write16/32` (`A &= ~(size-1)`, Mednafen `MemRead`), removing the out-of-bounds cache-line slice; golden-safe. ✅ **DIVU overflow writes a saturated quotient** (`114ec6b`, refines D1) — faithful `DIVU_S32_S32`/`S64_S32` incl. the `DIVU64_Partial` high-half + dropping the spurious `i32::MIN/-1` overflow. ⬜ *deferred:* address-error exceptions (vec 9/10) — the force-align captures the safe data-path half; raising the exception is unsafe (would trap on our own emulator bugs, and the oracle flags its address-error timing as approximate).
- **VDP2:** ✅ **bitmap palette-bank + colour-calc window done** (`9eac9a9`) — a ≤8bpp bitmap NBG folds its BMPNA/B palette bank into the CRAM index [10:8], and colour calc is gated by the WCTLD-hi-byte window; both inert at defaults. (MSB self-shadow 2× was fixed in H2d.) ✅ **blend ratio ÷31→÷32** (`30753c6`) — the hardware `(fore·(31−ratio) + below·(1+ratio)) >> 5`; a fully-opaque blend leaks 1/32 of the layer below. Pixel-visible; **play-test verified** (Wachenröder dialog + VF2). ⬜ *deferred:* CCRTMD (ratio-from-2nd-layer, CCCTL bit 9 — needs a per-pixel `Dot.cc_ratio`).
- **VDP1:** ✅ **mesh-phase, RGB-direct transparency, end-code truncation done** — mesh checkerboard phase corrected (`72bbe16`, `(x^y)&1 != 0` transparent); RGB-direct transparency = `raw < 0x4000` + end-code band `(raw & 0xC000) == 0x4000` (`b830aa6`); an end-code now truncates the rest of its textured row/span (`5b0e02e`). All pixel-visible; **play-test verified** in VF2. ⬜ *deferred:* user-clip "outside" mode (CMDPMOD bit 9, additive/low-value).
- **SCU-DSP** (least oracle-faithful SCU area): ✅ **ALU flags + V-sticky + count fixed** (`0057146`) — ADD/SUB carry/sign/zero on the native 32-bit width (not sign-extended i64), the V overflow flag is sticky (`|=`, cleared only by a status read), and a DMA `count==0` moves 256 words; golden-safe. ⬜ *deferred:* the MAME→Mednafen direction-dependent stride table (couples to the host `add>>2` writeback feeding the golden-sensitive boot-jingle DMA); program-RAM DMA (`drw==4`) folds to data bank 0 (needs a PRAM-staging buffer + savestate bump).
- **SCSP:** ✅ **SBXOR (PCM taps) + SoundDirect done** (`b9d06d7`) — SBXOR now XORs the PCM interpolation taps too (completing the H2a noise/zero path), and SoundDirect (reg6 bit 8) sends raw PCM at full level, bypassing EG/TL; both inert unless the bit is set. ⬜ *deferred:* AttackLoopLink, EGBypass, ShortWave (need new per-slot key-on state, rare in real games); EG/pan are float-table approximations, not bit-exact (a dedicated integer-EG refactor, bundle with EGBypass).
- **SMPC:** SYSRES (0x0D) decoded but a no-op (`// M4+ will route to reset()` — never did); direct-mode PDR/DDR peripheral I/O is store-only (no TH/TR handshake); SMEM (the BIOS language/setup byte) not persisted to a host file.

**Needs verification before acting** (agent-reported, inspection inconclusive):
VDP2 "2048-colour (depth 2) → 4bpp garbage" (the code has *partial* depth-2
awareness); VDP1 command-0xB severity (whether shipping games emit it — but our
plotter ends the list on 0xB while our timing model treats it as a clip command,
so the two halves disagree); SMPC per-command SF-busy durations.

**Already tracked on this backlog (not repeated here):** SCSP G2/G3/G4; VDP1
erase/BEF G5; VDP2 VBLANK-clear/ODD G6; rendering C5 (dual-window coeff,
screen-over mode 1), C6 (TVM=3), C9 (line-colour EXCC + gradient), C10
(rotation/bitmap-CG fetch gating); input E1 (multitap + port-2 scanning), E2
(analog peripherals), E3 (light gun + keyboard); F1 (MPEG card), F2 (CD
move/copy sector ops), F3 (SH-2 cache address/data-array spaces).

**Confirmed faithful (audit cleared, for the record):** SH-2 ISA decoder (all
~142 encodings) + illegal-slot table + cache (LRU/purge/CCR width) +
FRT/WDT/INTC/DMAC-run-condition + DIVU non-overflow; **M68k ISA essentially
complete**; **SCSP-DSP a faithful `RunDSP` port**; VDP2 windows / mosaic / the 16
sprite-type bit-splits / special-priority+CC / coefficient table (incl. the new
KTCTL line-colour) / back-screen; VDP1 command 0x0–0xA dispatch / gouraud / 4bpp
/ CEF.

## Performance (opt-in "fast mode" — future)

Accuracy stays the default and the trace-diff baseline; never a JIT/dynarec.
Levers catalogued from how Mednafen stays LLE at full speed:

| # | Lever | Status |
|---|-------|--------|
| P2 | Optimized interpreter dispatch | 🟢 partly landed, bit-identical: decode LUT, INTC O(1) cache, interrupt re-arm early-out, cache hit-path copy elimination. **Step-dispatch source micro-opts investigated 2026-06-29 (4-agent fan-out) and found to be a DEAD END** — see the dated note below; the unanimous top pick measured as noise. Remaining (codegen-only): PGO (P4); fastmap-style bus page table |
| P4 | Build & profile | 🟢 profiled (`bench_fps`/`bench_stages`/`bench_cache`/`bench_vf2_fight`). **Fat LTO measured-neutral 2026-06-29** (thin already captures the cross-crate inlining). **★ PGO measured 2026-06-29 = the BIG single-core win** (`tools/pgo/run_pgo.sh`): **+31% VF2 fight, +56% Doukyuusei menu** trained-on; **+39% Doukyuusei held-out** (trained on VF2 only → generalises across games). Build-time only, bit-identical (golden `0x0B1BA6E5180766F7` + savestate pass under `profile-use`), thermal-controlled (interleaved A/B). **Adoption recipe LANDED: `tools/pgo/build_release.sh`** — instruments a headless `jupiter`, boot+attract-trains over `roms/*.cue`, merges, builds the shipping SDL binary with `-Cprofile-use`, runs the gates; falls back to a plain build if assets are absent (a release/packaging step, NOT a checked-in `RUSTFLAGS`) |
| P6 | Hoist redundant per-instruction entity borrows in `step_cpus` | ✅ landed (golden-invariant): the 4 per-instruction `scheduler.entity(*master_id).sh2()` lookups collapsed to one borrow for the two gating reads (`imask` + delay-slot); `pc`/`cycle` moved into the rare interrupt-fire branch (only the trace consumes them). golden `0x0B1BA6E5180766F7` unchanged; dual_sh2/scu/scheduler green. Magnitude small/unverified (<1–2%) |
| P7 | Batch-invariant per-instruction scaffolding in `step_cpus` | 🔴 **investigated 2026-07-01 — not worth it, closed.** Inspection of `step_cpus:1225-1226` shows what P7 would remove is `cd_block.irq_active()` (`(hirq & hirq_mask) != 0`) + `set_cd_int` (a OnceLock read + a few field/branch ops) — **cheap idempotent reads/branches, exactly the class the P2/PGO investigation measured as noise** ([[pgo-is-the-big-interpreter-perf-lever]]). The genuinely costly per-instruction op is the SCU `take_pending_interrupt` sample, which **must stay** per-instruction for interrupt-acceptance timing (P7 doesn't touch it). And `set_cd_int` is a deliberate **level re-assertion** every instruction — skipping it (cache-compare / dirty-flag through the bus write path) risks the exact timing class that black-screened games (`b65cd18`), for a likely-noise reward. The FRT/WDT slice of the old "~15% scaffolding" was already event-driven (~1.3%); the CD-int residue is small. **PGO (P4) is the real single-core lever** (+31–56%, bit-identical). Re-open only with `perf` (blocked at `perf_event_paranoid=3`) showing a real slice |

(Other levers were catalogued and dropped 2026-06-12 — accuracy-affecting
sync/model shortcuts, and the Mednafen-style video-output levers, of which
per-field interlace rendering (P5) was implemented and reverted by user
choice: the bare weave showed ghosting in play-testing; see `4284c1c`/
`fe70809` and the git history of this section. (A *distinct* full-resolution
display-side **field-weave** for VDP1 double-interlace did later land and
**was** user play-test-accepted — `b1bb3ce`, *smoother* in VF2 — but as a
fidelity feature, not a perf lever; see the VDP1 DIE field-weave in
`CLAUDE.md` / `doc/glossary.md`.) Current performance is sufficient without
them.)

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

### Profile baseline (2026-06-29, 12-core host)

`bench_vf2_fight` (704×448 DD, worst case): compute-only 70.1 fps, compute+render
37.1 fps (render share 47%), **in-vivo overlapped `bench_vf2_pipeline` 64.5 fps**
(advance avg 15.41 ms vs the 16.67 ms budget → ~8% headroom); audio 742 samples/
frame, zero shortfalls. `bench_fps` (640×224 Doukyuusei menu): compute-only 82.8
fps, compute+render 62.3 fps (render share 25%). **Both heavy scenes already clear
60 fps real-time**, so the catalogued P6/P7 levers are *thin-margin / low-core-host
robustness*, not a current deficit — accuracy stays the default and no change is
required for raw speed. Render-thread sweep confirmed the default band count (4 on
12 cores) is optimal (2 → 48.8 fps render-starved, 6 → 62.9 oversubscribed). Cache
is **not** a lever: 99.903% hit, cold line-fill ~0.1%, and the hit-path whole-line
copy is already eliminated (`probe`/`line_at`/`extract_u*`, copy-free). Hotspot
attribution (P7 sizing) is blocked until `kernel.perf_event_paranoid ≤ 2` — `perf`
sampling was denied at the host's default `=3`.

#### Interpreter micro-opt investigation — measured DEAD ENDS (2026-06-29)

A 4-agent fan-out (tracing-thoroughly skill) catalogued every bit-identical
source/codegen micro-opt in the SH-2 `step`/dispatch hot path. The two
highest-ranked were **implemented and measured** (the only way to attribute with
`perf` blocked) and **both are noise** — do NOT re-chase:

- **Decode-LUT fixed-array** (`decode_lut: Box<[Op]>` → `Box<[Op; 65536]>` to elide
  the per-fetch bounds check on the hottest line) — the unanimous #1 pick across all
  four agents. Proven bit-identical (boot golden `0x0B1BA6E5180766F7` + savestate
  round-trip unchanged), but **zero measurable fps movement** (VF2 fight 70.1→69.7–
  70.1 compute / 64.5→63.9–64.4 pipeline; Doukyuusei 82.8→81.2–82.3). The never-taken
  bounds check is perfectly predicted → free. Reverted.
- **Fat LTO** (`lto = "thin"` → `true`) — full-workspace rebuild, **no reliable gain**
  (all scenes inside run-to-run variance; Doukyuusei +~1% but overlapping
  distributions). Thin already captures the `sh2→saturn` cross-crate inlining the hot
  path needs. Reverted.

**Generalised verdict:** the remaining per-instruction source micro-opts (register-
file `& 0xF` bounds-check elision, front-loading `if addr < 0x2000_0000` in `mem_*`,
gating `Cache::probe`'s `dbg_stats++`, a scoreboard-predicate LUT, `run_dma` arming)
are all the **same class** — removing a well-predicted branch or a store-buffer-
absorbed store — so they are strongly predicted to be noise too, and are **not worth
the churn on accuracy-critical core code**. The interpreter core is, as documented,
largely inherent and already tuned — at the *source* level.

#### ★ PGO is the win (2026-06-29)

The single-core headroom the source micro-opts couldn't find lives in **block
layout**, and PGO unlocks it. `tools/pgo/run_pgo.sh` (manual `-Cprofile-generate` →
representative run → `llvm-profdata merge` → `-Cprofile-use`) measured, A/B against
the thin-LTO baseline, interleaved to control thermals:

- **VF2 fight compute-only 69.7 → 92.1 fps (+31%)**, in-vivo pipeline 63.3 → 82.3 (+30%).
- **Doukyuusei menu compute-only 82.2 → 128.5 fps (+56%)** trained-on.
- **Held-out: train on VF2 only, the never-seen Doukyuusei menu still hits 114 fps
  (+39%)** — the profile generalises across games (over-fit inflation is modest).

PGO is **build-time only and bit-identical** — the boot golden `0x0B1BA6E5180766F7`
and the savestate round-trip both pass under `profile-use`, so it stays inside the
accuracy-first/no-JIT charter (it reorders the 143-arm `execute()` match + the
`mem_*`/`classify` chains by measured opcode frequency; neither thin nor fat LTO can
do this without a profile). This is **by far the biggest single-core lever in the
whole investigation** and touches zero source. **Adoption recipe landed**
(`tools/pgo/build_release.sh`): instrument a headless `jupiter`, boot+attract-train
over representative discs, merge, build the shipping SDL binary with `-Cprofile-use`,
run the golden + savestate gates — a release-time step, not a checked-in `RUSTFLAGS`
(falls back to a plain build when assets are absent).

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
  meanwhile falls back to the blit), and loading user `.spv`/preset shaders. The
  presenter stays behind the off-by-default **`gpu-presenter`** build feature
  because it's verified Linux/Vulkan-only; the gate comes off (compiled into
  default builds + `--gpu` made first-class) once DXIL/MSL land and it's tested on
  Windows/macOS — rationale + removal criterion in ADR-0019. SDL3's `SDL_GPU` (Vulkan/Metal/D3D12,
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
