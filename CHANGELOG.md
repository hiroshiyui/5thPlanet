# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.1] - 2026-06-19

A documentation and internal-cleanup release — **no behavioural change**. The
save-state format (v9) and the `bios_boot` golden hash are both unchanged, so
existing save states still load.

### Changed

- **NBG renderer cleanup.** Hoisted the VDP2 NBG per-dot register decode (mosaic
  `MZCTL`, line-scroll `SCRCTL`/`LSTAn`, vertical cell-scroll table addressing)
  out of the per-dot path into the once-per-frame `NbgCtx`, matching the existing
  `FrameCtx` hoist pattern. Bit-identical (golden + all 597 saturn tests
  unchanged); a clarity/redundancy cleanup, not a measurable perf win on the
  benched scenes (profiling confirmed `nbg_layer`'s cost is memory-bound
  sampling, not register decode).
- **Documentation overhaul.**
  - Merged `bootstrapping.md` into `system-architecture.md` as a new §9, so the
    reset→splash→game-boot sequence lives beside the chip→module map; refreshed
    its status to M11-complete.
  - Synced `system-architecture.md` to the current M11/M12/M13 reality — the M12
    BSC bus-timing model, CD-DA via the SCSP EXTS inputs, Shuttle Mouse support,
    the render-pipeline worker / audio-paced loop, and corrected test counts.
  - Added module/struct/function `///` doc comments across every crate
    (`saturn`, `sh2`, `m68k`, `scu_dsp`, `physdisc`, `jupiter`) via a
    docs-engineering source-comment audit; the skill now enforces this.
- **Four retroactive ADRs** recording settled, load-bearing decisions that the
  code already relied on: the CD-block HLE exception (0015), master-leads-slave
  SH-2 stepping (0016), the reference-oracle policy (0017), and the save-state
  design (0018).

## [0.4.0] - 2026-06-14

Adds the OSD disc-image browser and fixes VF2 input lag, plus an M13 A1
timing-accuracy refinement and a documentation overhaul. Backwards-compatible:
the save-state format (v9) and the `bios_boot` golden hash are both unchanged,
so existing save states still load.

### Added

- **Disc-image browser in the OSD.** Esc → **Load Disc…** opens a filesystem
  browser: navigate directories and pick a `.cue` / `.iso` / `.ccd`; selecting
  an image loads it and power-cycles to boot the game. The menu stays core-free
  and `fs`-free (the frontend supplies the directory listing); a scrolling
  viewport handles large directories.
- **Raster batch-drain jitter probe** (M13 A1, dev instrumentation) — an
  observer-only check confirming VCNT/TVSTAT reads are never stale, the evidence
  that the HBlank clamp edge can stay deferred.

### Changed

- **SMPC command dispatch is now an exact mid-batch event** (M13 A1). A queued
  command breaks the scheduler batch so it dispatches within one instruction of
  the COMREG write, matching the LLE reference. Golden-safe — a timing-fidelity
  improvement, not a behaviour regression.
- **Documentation overhaul.** Normalised all roadmap tables to one format;
  consolidated Tier G (residual reference-audit items) with status markers;
  retired the point-in-time MAME/Mednafen cross-reference audits, folding their
  durable residue into `bootstrapping.md` §C.1 and roadmap Tier G; added an
  **Enhancements** section (EN1: GLSL shader presets); and synced the disc
  browser into the feature docs.

### Fixed

- **Input lag / "not steady 60 fps" in VF2.** The emu-thread frame pacer was
  chronically collapsing ~1/3 of rendered frames ("run 2, show 1") because it
  chased an audio-reserve target it structurally couldn't reach. It now renders
  **every** game-frame in normal play, collapsing only when the audio reserve
  has genuinely drained toward an under-run — smoother motion and lower input
  latency. The `SAT_MAX_BURST` env var becomes the catch-up ceiling (default 2;
  `=1` disables catch-up).

## [0.3.1] - 2026-06-14

A small, behaviour-preserving release: a save/load latency optimization plus
documentation sync. No emulation behaviour changes — the boot golden hash and
the save-state format (v9) are unchanged, so existing save states still load.

### Changed

- **Disc fingerprint is cached at construction** instead of re-hashed on every
  `save_state`/`load_state`. The save-state media-identity check ran a full
  FNV-1a over the entire disc image (a ~1.5–1.7 s stall per quicksave *and* per
  quickload on a 600–700 MB game image); the hash is now computed once when the
  disc is inserted and stored in a field, so quicksaves and quickloads are
  effectively instant. The hash value is unchanged — bit-identical media
  identity, boot golden and save-state round-trip unaffected.

## [0.3.0] - 2026-06-12

**Milestone 12 (whole-system cycle accuracy) is complete.** The three
remaining timing models landed — the VDP1 draw-duration walk, the exact
B-bus deferred-write serialization, and the SCU A/B/C-bus DMA arbitration —
bringing the whole-system BGM-phase metric from a +182 seq-tick divergence
to within ~1% of the LLE reference (4450 vs 4497). Virtua Fighter 2 and
Doukyuusei ~if~ remain fully playable (user-verified under the new timing).
Save states: format v9 (older states are rejected, not migrated).

### Added

- **M12 task #6 — VDP1 draw-duration model**: a Mednafen-faithful
  draw-cycle walk (`vdp1/timing.rs`) runs alongside the instant
  rasterisation and sizes every plot — per-command fetch/setup, per-span
  clip costs, per-pixel charges with anti-aliasing and the leave-clip early
  exit, and the fractional refresh-overhead scaling. `EDSR.CEF` and the
  sprite-draw-end interrupt now land when the reference's do (the BIOS
  CD-player panel draw spans ~258k cycles ≈ half a frame, matching the
  oracle within 0.3%). The VDP1 clip/local registers are modelled as the
  persistent hardware state they are, serialized with the machine.
- **M12 task #8 residual — exact B-bus deferred-write serialization**: an
  SH-2 B-bus write hands off in +2 CPU cycles and posts its device-side
  completion (SCSP +17/+13, VDP1 +9/+1, VDP2 +3/+1 per 16-bit half) on a
  separate timeline that only the next B-bus access waits out; B-bus reads
  are always two 16-bit halves (VDP1/VDP2 reads were undercharged by half).
- **M12 task #8 residual — SCU A/B/C-bus DMA arbitration**: DMA-timeline
  costs corrected to the reference's per-access values (B-bus VDP1/VDP2 1,
  SCSP 13 per 16-bit access; C-bus SDRAM read 6 per word, write free — the
  old flat values overpriced DMA writes up to 11×), and a C-bus-endpoint
  transfer now halts **both** SH-2s for its paced duration (the SCU owns
  the CPU bus) while a pure A↔B transfer halts neither. This was the
  dominant lever for the M12 phase residual.

### Changed

- The SH-2↔VDP1 RW draw-slowdown is now **opt-in** (`set_rw_slowdown`,
  default off), matching the reference where it is a per-game hack
  (applied to e.g. Virtua Fighter 1 — not VF2, not the BIOS).
- Save-state format bumped to **v9** (`Vdp1` draw-timing state +
  `BusTiming::bbus_write_finish`); older states are rejected on load.
- The M12 residual (~1%) is documented and closed under the stop rule: a
  discrete ~14-frame recognition-handshake offset plus a 68k-gated mailbox
  poll loop — oracle-approximation territory, recorded as follow-up
  threads in the roadmap.

## [0.2.0] - 2026-06-11

Milestone 9 (frontend OSD) is complete and Milestone 12 task #8 (the
per-access bus-timing model) landed; a chain of user-verified accuracy fixes
followed — VF2's "phantom ring-out" floor displacement, an audio-pacing
starvation, and a CD report-tearing bug found booting Panzer Dragoon Zwei.
Save states: format v8 (older states are rejected, not migrated).

### Added

- **M9 complete — frontend OSD**: a persisted config file
  (`$XDG_CONFIG_HOME/5thplanet/jupiter.toml`: scale, fullscreen, region,
  cartridge, keymap; CLI flag > config > autodetect), a **Settings →
  Controller** screen with press-to-bind keyboard remapping (Reset Defaults
  included), and a **Settings → BIOS** screen that power-cycles into any
  sibling 512-KiB image, re-keying the save files.
- **Game-controller support**: hot-plug SDL GameController (XInput/evdev)
  merged into the port-1 pad with a fixed Xbox-style mapping
  (A/B/C = X/A/B, X/Y/Z = Y/LB/RB, L/R = triggers, D-pad or left stick),
  plus controller navigation of the OSD.
- **M12 task #8 — per-access BSC bus-timing model** (a faithful Mednafen
  port, implemented bus-side): CS0 as a 16-bit bus with per-transaction
  costs, CS3 SDRAM read/write with the array-busy window, cache-line fills
  as one burst (`AccessKind::LineFill`), the SH-2 write buffer, bus
  turnaround, A-bus cartridge cost from live ASR0, and a shared bus
  timestamp giving CPU↔CPU arbitration. The `bios_boot` golden was
  unchanged; the BGM-phase metric moved toward the reference.

### Fixed

- **VDP2 rotation — the VF2 "phantom ring-out"**: in 640/704-dot hi-res
  modes the rotation layer renders at normal dot resolution (each rotation
  dot spans two display dots); ours stepped per display dot, compressing
  the floor 2× toward screen-left. Also a one-bit sign-mask typo that
  corrupted rotation viewpoint X values in [4096, 8191].
- **Audio pacing**: the SCSP is fed the master's actual cycle advance (the
  batch-edge overshoot was silently dropped — fights starved the audio
  reserve and felt slowed), delivered in fixed 256-cycle chunks so the
  Timer-B rate stays locked to the reference (88.000 samples/tick); SCSP
  output is resampled when the audio device opens at a non-44.1 kHz rate.
- **CD-block report tearing**: the periodic status report now holds until
  the host consumes it by reading CR4 (Mednafen's `ResultsRead` gate) —
  previously it recomposed per sector during playback and could tear a
  reader's CR1..CR4 sequence (found via Panzer Dragoon Zwei's BIOS
  disc-validity check).

### Changed

- Save-state format v6→v8 across the bus-timing, `ResultsRead`, and
  SCSP-feed changes; older states are rejected with a clear error.
- Debug tooling: CD command-log ring widened to 8192 entries; the VF2
  PC-trace and disc-content probes are now CUE-parameterized.

## [0.1.0] - 2026-06-11

First release. 5thPlanet is an accuracy-first SEGA Saturn emulator in Rust:
**two commercial games are fully playable** — *Virtua Fighter 2* (steady 60 fps:
title → menus → character select → full 3D fights to the K.O. screen, with CD-DA
BGM and in-fight SFX at the hardware mix balance) and *Doukyuusei ~if~* (640×224
hi-res, graphics, SFX, and voices all working) — on the pure real-BIOS LLE path,
with no game-specific hacks. Milestones M1–M8, M10, and M11 are complete;
1058 workspace tests, ~85 % line coverage.

### Added

- **SH-2 (SH7604) ×2 core** (`sh2` crate, `no_std`): full ~142-op ISA, 5-stage
  pipeline cycle model (load-use/address-gen interlocks, multiply/divide
  latency), delay-slot machinery, 4 KiB 4-way cache (write-through, associative
  purge), exceptions/interrupts, and on-chip peripherals (INTC, DIVU, FRT, WDT,
  working DMAC, BSC master/slave bit).
- **Saturn system layer** (`saturn` crate): typed-region bus with per-region
  wait states (Mednafen-derived B-bus/A-bus costs), dual SH-2 in
  master-leads-slave interleave with per-instruction SCU interrupt sampling,
  and a deterministic event-driven scheduler clamped to peripheral-event edges.
- **SCU**: 3 DMA channels (direct + indirect, strides, hardware start factors,
  cycle-stealing, 27-bit address folding), interrupt aggregation with fixed
  vectors, Timer 0/1, and the embedded **SCU-DSP** (`scu_dsp` crate, full VLIW
  core).
- **SMPC**: command set with hardware codes, staged INTBACK (status +
  peripheral phases) at reconciled busy timings, live RTC, region selection,
  and per-port peripheral selection — digital pad and **Shuttle Mouse**.
- **VDP1**: full command-list plotter (all primitives and colour modes,
  gouraud, mesh, clipping), TVM 8bpp framebuffer + DIE/DIL double-interlace
  plotting, framebuffer erase/swap (FBCR), cycle-accurate draw-end IRQ.
- **VDP2**: multi-layer compositor — NBG0–3 + RBG0/1 rotation (per-dot
  coefficients, RDBS bank grants, RPMD 0–3) + the VDP1 sprite layer, with
  colour calculation (incl. extended 3-layer), windows (rect/per-line/sprite),
  special priority/colour-calc, colour offset, mosaic, line/back screens,
  per-line scroll/zoom, vertical cell scroll, VRAM cycle-pattern fetch gating,
  CRAM modes, and hi-res output (320/352/640/704 × 224/240/256, ×2 interlace).
- **SCSP** + hosted **MC68EC000** (`m68k` crate, full ISA + exceptions):
  32-slot PCM/FM engine (ADSR EG, LFO, slot-to-slot FM, slot monitor), 128-step
  effect DSP, CD-DA through the EXTS inputs at the game-programmed mix, MVOL,
  44.1 kHz stereo output.
- **CD-block (HLE)**: host command interface, 200-block buffer pool with 24
  filters/partitions, Mednafen-faithful drive-phase model (BUSY/SEEK/PLAY,
  recognition spin-up, periodic reports), 75 Hz read pump with read-ahead,
  data transfer (FIFO + SCU-DMA port), ISO9660 filesystem, disc
  authentication, and CD-DA playback (FAD and track/index Play forms, 1×
  audio pacing, byte-swapped-rip detection).
- **Disc sources**: ISO / CUE-BIN / CCD-IMG image loaders and **live optical
  drives** via the feature-gated `physdisc` crate (libcdio, ADR-0009).
- **Cartridge slot**: Extension DRAM (1 MB / 4 MB), battery backup-RAM, and
  game-ROM carts with the probed cart-ID byte.
- **Save states**: full deterministic machine snapshot/restore (bincode,
  versioned header, external media referenced by fingerprint, not embedded).
- **Internal backup RAM**: hardware-faithful odd-byte packing, pre-formatted,
  persisted to a host `.bup` file.
- **SDL2 frontend** (`jupiter`): audio-paced main loop with a render-pipeline
  worker thread, scanline-band parallel compositing, in-window OSD menu
  (save/load slots, reset, disc eject/insert, graphics/region/cartridge
  settings), save-state hotkeys, mouse capture (F10), and headless mode.
- **Interactive debugger** (`sdbg`): gdb-style REPL — master/slave
  register-guarded breakpoints, bus-level memory probe, watchpoints, SH-2 +
  68k disassembly and PC traces, HIRQ-edge trace, CD-block/SCSP state,
  save-state rewind.
- **Cycle-accuracy infrastructure**: PC-trace-diff and signal-scope harnesses
  against the MAME/Mednafen references, ROM regression goldens, and the
  committed `bios_boot` splash golden.
