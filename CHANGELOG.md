# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
