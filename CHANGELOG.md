# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
