# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.11.0] - 2026-06-24

Migrates the `jupiter` frontend from **SDL2 to SDL3** and adds a user-selectable
graphics-presentation backend. **Building jupiter now requires SDL3** (`libsdl3`
dev libs + pkg-config), not SDL2 — update your build environment / CI. The
emulation core is untouched (frontend-only; save-state format unchanged; all
three playable games — VF2, Doukyuusei ~if~, Sangokushi V — verified working).

### Added

- **Selectable graphics-presentation backend.** Choose how the framebuffer is
  presented — `--backend=opengl|opengles|direct3d11|direct3d12|metal|software`
  (default `auto`), the `backend` config key, or the OSD Settings → Graphics
  **Renderer** row — with a fallback chain (preferred → opengl → software); the
  resolved driver is logged at startup. The picture is still rendered in software
  (accuracy-first); this only selects the present path.

### Changed

- **Frontend migrated SDL2 → SDL3** (sdl3 0.18.4 + sdl3-sys, system SDL 3.x via
  pkg-config), unlocking SDL3's modern APIs (SDL_GPU, stream-based auto-resampling
  audio, the unified gamepad API). The `sdl2-frontend` Cargo feature is renamed
  `sdl-frontend`. **Build dependency: SDL3 is now required** (the headless
  `--no-default-features` build still needs no SDL). See ADR-0019/0020.

### Fixed

- The startup "render backend" line reports the **actual** driver in use (queried
  from the renderer), not a useless "default" for `auto`.

## [0.10.0] - 2026-06-24

Brings **Sangokushi V** to fully playable — the third playable commercial title
— adds VDP2 16M-colour bitmap rendering, and lands a batch of SH-2/SCU
cache-and-bus accuracy fixes plus a large headless-debugging toolkit. Save-state
format unchanged (v10); the prior playable games (Virtua Fighter 2, Doukyuusei
~if~) are unaffected — their render goldens still pass.

### Added

- **Sangokushi V (三國志V) is playable** — the third playable commercial title,
  from its intro movie through the title, main menu, opening, and the in-game
  strategy screen; the first title to drive the Sega FILM / Cinepak movie player
  through to gameplay. See [`doc/compatible-game-titles.md`](doc/compatible-game-titles.md).
- **VDP2 16M-colour (RGB888) bitmap mode** for NBG/RBG.
- **`sdbg` headless toolkit** — `pad` / `poke` / `render` (drive input-gated
  paths + composite a frame without advancing), `replay` (deterministic playback
  of recorded input movies) with `--cart`, `caudit` (audit both SH-2 caches),
  `fbdump` (render the loaded state to a PPM), cache/FRT inspection, and a cache
  stale-read detector.
- **jupiter** — `SAT_INPUT_REC` (record input movies for headless replay), an
  SDL2 window icon, and the `SAT_SLOW_FETCH` timing probe.
- **Docs** — a [debugging playbook](doc/debugging-playbook.md), a
  [compatible-game-titles list](doc/compatible-game-titles.md), a WIP
  compatibility tracker, Sangokushi V screenshots, and an LLE-debugging
  methodology section in CLAUDE.md.

### Fixed

- **SH-2 cache coherency** — the two Sangokushi V menu blockers: `Cpu::reset`
  now purges and disables the cache (an SSHON-re-reset slave refetches instead of
  running stale code), and 16-bit `MOV.W @CCR` access routes to the cache so a
  word cache-purge is not silently dropped. Also: FTCSR status flags clear only
  after a read-1 (SH7604 latch), and the SH7604 6-bit pseudo-LRU cache
  replacement.
- **Saturn** — inter-CPU FTI triggers on a write of any width; the
  SCU-DMA-from-CD-FIFO source longword is read once per two 16-bit writes (fixes
  the Sangokushi V FMV "FILM" signature corruption).

### Changed

- `dump_game_disc.sh` is driven by **redumper** (with cdrdao as a fallback).

### Removed

- CHD disc-image support — the loader reads `.cue` / `.iso` / `.ccd` only; use
  `chdman extractcd` to convert a CHD.

## [0.9.0] - 2026-06-22

Expands the built-in self-diagnostics into a broad accuracy/boot tool and adds
two OSD screens. **No emulation-core or save-state changes** — the save-state
format stays v10, both playable games are unaffected, and the diagnostics run on
throwaway machines that never touch live state.

### Added

- **Self-diagnostics suite expanded 6 → 16 checks** across CPU ALU / branch /
  memory, the SH-2 on-chip **DIVU** (divider), **FRT** (free-running timer) and
  **DMAC**, the **SCU DMA** engine, **VDP2** back-screen rendering, and **SCSP**
  slot synthesis — each a tiny from-reset program (or bus-driven chip setup) on
  a throwaway machine, run via `jupiter doctor` and the OSD Diagnostics screen,
  with `all_diagnostics_pass` as a CI accuracy regression.
- **Boot/compatibility diagnostics** — `jupiter doctor <BIOS> [disc]` boots a
  throwaway machine and reports heuristic checks: the BIOS produces video, the
  disc TOC is valid, and the disc's 1st-read program reaches game RAM (auth +
  IP.BIN load + jump). The hermetic `doctor` (no media) is unchanged.
- **OSD "About…" screen** — product name, version, author, and the MIT license
  notice (read from compile-time package metadata).
- **OSD Diagnostics live-status** — a read-only region / disc / current
  master-PC-region readout of the running session (no boot involved).

### Changed

- **OSD panels auto-size to their content** (the old fixed 180px panel clipped
  long rows), and Diagnostics results are colour-coded **PASS** green / **FAIL**
  red.

### Fixed

- **CHD: corrupt or oversized track metadata** now produces a clean
  "runs past the data" error instead of risking a u32 overflow / out-of-bounds
  (the `assemble` offset math moved to `usize`).
- **`audio_pipeline` test** now programs MVOL — a fresh-reset SCSP has MVOL = 0
  and is silent, so the synthesis proof (a `#[ignore]` test) had silently
  regressed to peak 0; both variants produce full-scale output again.

## [0.8.0] - 2026-06-22

Adds two new user-visible capabilities — **CHD compressed disc-image support**
and a **built-in self-diagnostics suite** — plus disc-dumping tooling. **No
emulation-core or save-state changes**: the save-state format stays v10 and both
playable games (*Virtua Fighter 2*, *Doukyuusei ~if~*) are unaffected.

### Added

- **CHD disc images** (roadmap **G1** ✅). `saturn::chd_image::from_chd` decodes
  a CD CHD — MAME's compressed multi-track container — by decompressing every
  hunk and concatenating each track's raw 2352-byte sectors into the shared
  `Disc` builder, so the TOC, read paths, save-state fingerprint, and the
  cdrdao byte-swap warning are identical to the CUE/CCD parsers. Uses the
  pure-Rust `chd` crate behind a `chd` feature (no FFI/`unsafe`, unlike
  `physdisc`); `jupiter` enables it by default and reads `.chd` directly.
  Legacy `CHCD`/GD-ROM are rejected with a clear message. Validated
  byte-identical to the `.ccd` loader on a multitrack disc.
- **Built-in self-diagnostics** (`saturn::diagnostics`). A battery of tiny
  hand-assembled SH-2 programs run from reset on throwaway machines (no BIOS,
  no disc, no external toolchain), each verifying one behavior — `ADD`/`SUB`,
  `MUL.L`→MACL, a taken `BRA` delay slot, Low/High WRAM store-load round-trips
  — with an `all_diagnostics_pass` test making it a CI accuracy regression.
  Surfaced via a new **`jupiter doctor`** CLI subcommand (headless report, exit
  `0` all-pass / `1` on failure) and an OSD **Settings → "Diagnostics…"** screen
  with a "Run all" item and a scrollable `[PASS]`/`[FAIL]` list.
- **Disc-dumping tooling.** A `dump-game-disc` skill and `tools/dump_game_disc.sh`
  automate ripping an owned Saturn disc to a loadable image (cdrdao →
  `toc2cue` → verify → optional CHD), handling the cdrdao MSB-first CD-DA
  byte-swap gotcha.

## [0.7.0] - 2026-06-20

A frontend (`jupiter`) release: adds a Shuttle Mouse OSD toggle and config
persistence, a portable (executable-adjacent) config location, and hi-res-aware
OSD text scaling, plus a screenshots gallery and disc/BIOS documentation. **No
emulation-core or save-state changes** — the save-state format stays v10 and
both playable games (*Virtua Fighter 2*, *Doukyuusei ~if~*) are unaffected.

### Added

- **Shuttle Mouse toggle in the OSD.** The Controller settings screen gains a
  live "Mouse: Off / Port 1 / Port 2" row that re-points the SMPC ports without
  a reset (the game re-reads devices on the next INTBACK).
- **Shuttle Mouse persisted in the config** via a new `mouse` key (`off`/`1`/`2`,
  same vocabulary as the `--mouse[=1|2]` flag, which still overrides it).
- **Portable config location.** A `jupiter.toml` sitting next to the executable
  is now read and written, so a self-contained archive carries its own config.
- **Documented sample config** `jupiter/jupiter.toml.example` (every key, guarded
  by a test so it can't drift from the parser).
- **Documentation:** a `doc/screenshots/` gallery README (with a trademarks &
  copyright notice), a `roms/` README incl. a disc-dumping guide, and BIOS
  SHA-512 checksums in `bios/README.md`.

### Changed

- **Config lookup is portable-first:** an existing `jupiter.toml` beside the
  executable wins over `$XDG_CONFIG_HOME/5thplanet/jupiter.toml`; the chosen
  file is also the one written back to.
- **OSD text/menu scales for hi-res framebuffers.** The 8×8 menu font is scaled
  per-axis by whether that axis is hi-res (640/704-dot → 2× wide, 448/480
  interlace → 2× tall), so the menu stays legible and correctly proportioned in
  every video mode while still fitting non-square modes like 640×224. Lo-res
  (320×224) is pixel-exact as before.

### Removed

- **F8 ("play the disc's CD-audio track") is gone from release builds.** It was
  a developer diagnostic that drives CD-DA outside the BIOS path; it is now
  gated behind `#[cfg(debug_assertions)]` (debug builds only).

### Fixed

- **OSD menu overflow in wide-but-short hi-res modes.** An interim uniform-2×
  scaling overflowed 640×224 (only 224 lines), clipping the bottom menu rows;
  the per-axis scaling above fixes it.
- **VS Code TOML association** for `jupiter.toml.example` — the `files.associations`
  glob is anchored with `**/` so it actually matches.

## [0.6.0] - 2026-06-20

Reworks the SH-2 on-chip FRT/WDT timers and interrupt recalc to Mednafen's
lazy/event-scheduled model, fixes a regression that black-screened *Doukyuusei
~if~*, and corrects the FRT prescaler. **Save-state format break: v9 → v10** —
existing `.sav`/`.state` files (and cached bench snapshots) are rejected and
must be recreated. Both playable games (*Virtua Fighter 2*, *Doukyuusei ~if~*)
remain fully playable (user-verified); the `bios_boot` golden hash is unchanged.

### Fixed

- **_Doukyuusei ~if~_ black screen (regression since v0.4.0).** A mid-batch SMPC
  command dispatch (`smpc.has_pending()` breaking the SH-2 batch) re-anchored
  `run_frame`'s event-clamped batch grid mid-frame, so VDP2 stopped compositing
  while the CPU ran normally — the game booted to an all-black framebuffer. The
  batch break is removed; SMPC commands drain at the batch boundary as before.
  Found by bisecting from the last working tag with a headless render probe.
- **FRT prescaler mapping.** TCR CKS1-0 now decodes to φ/8, φ/32, φ/128,
  external clock (the SH7604 mapping; Mednafen + Yabause agree) instead of the
  shifted φ/1, φ/8, φ/32, φ/128 — the FRC no longer runs 4–8× too fast, and the
  external-clock setting (CKS=3) freezes the counter rather than ticking it.

### Changed

- **On-chip FRT/WDT timers + INTC are now event-driven** (advances roadmap M13
  A1). The per-instruction `advance_timers` (FRC/WTCNT tick) and
  `refresh_interrupts` (INTC re-arm) are gone: the timers materialize lazily
  from the elapsed-cycle delta and are scheduled by a next-event timestamp, and
  the INTC is recomputed only when an input changes (timer events, on-chip
  register writes, DMAC transfer-end, FTI capture). Bit-identical to the prior
  model (ported in four golden-invariant stages); removes ~10 percentage points
  of per-instruction timer/interrupt overhead in poll-heavy scenes.
- **Save-state format v9 → v10** — the FRT/WDT rework dropped the per-cycle
  prescaler accumulators and added the timer epoch/next-event state. Old saves
  are rejected by the version check.
- **Minor allocation/CPU tidy-ups** (all bit-identical): cache the FRT prescaler
  decode, skip a no-op cache-LRU rotate, dedup a WDT interrupt read, cache the
  `SAT_FTILOG`/`SAT_VDP1LOG` debug-env flags behind `OnceLock`, and skip the OSD
  context clones while the menu is closed.

### Added

- **Game-render goldens.** `doukyuusei_renders_non_black` and
  `vf2_renders_non_black` boot each game and assert the master runs game code
  (HWRAM) and VDP2 composites a non-black frame — the game-level analogue of the
  BIOS-splash golden, closing the gap that let the black-screen regression
  through (the BIOS golden never exercised a game's render path).
- **SMPC batch-break regression guard** (`pending_smpc_command_does_not_break_the_batch`)
  — a CI-runnable synthetic test that fails if the mid-batch dispatch is reintroduced.

## [0.5.0] - 2026-06-19

Grows the `sdbg` headless debugger into a full trace-diff workbench, and adds a
reference/developer-tooling documentation layer. **No change to emulator runtime
behaviour** — the `bios_boot` golden hash and the save-state format (v9) are
unchanged, so existing save states still load.

### Added

- **`sdbg` reference master-PC trace-diff (`tdiff`).** `tdiff <ref> [frames]`
  runs ours through the real full-system path and compares the loop-collapsed
  master PC stream against a Mednafen `SS_PCTRACE` dump, stopping at the **first
  divergent PC** with a both-sides context window. On a divergence it then
  **rewinds to a pre-trace snapshot and re-runs to a breakpoint there**, printing
  full registers + the stack call-chain and parking the machine at the
  divergence. Knobs: `TDIFF_ADD` (Mednafen fetch-PC offset), `PCTRACE_LO`/`HI`.
  This hosts the project's primary debugging methodology (the LLE↔Mednafen
  trace-diff) inside the REPL instead of hand-diffing two trace files.
- **`sdbg` multiple breakpoints + symbols.** Several register-guarded master /
  slave breakpoints at once (`b <addr> [ri v]` adds, `b` lists, `bd <id|*>`
  deletes), honoured by both `c` and `fc`. A symbol table (`sym`, `syms <file>`,
  `--syms=<file>`) resolves names anywhere an address is expected and annotates
  output (`name+0xNN`) in disassembly, breakpoint hits, and the call-chain.
- **Core support:** `Sh2Entity` now holds a *set* of breakpoints and `BpHit`
  carries the firing PC (`Saturn::set_master_bps`/`set_slave_bps`). Debug-only
  and `#[serde(skip)]`, so the golden and save-state format are unaffected.

### Changed

- **Documentation layer.** Added a **Developer tools** catalog and a
  **References** section (authoritative SEGA/Hitachi/Motorola manuals + the
  behavioural oracles, with verified download locations) to `CLAUDE.md`; merged
  `bootstrapping.md` into `system-architecture.md` §9; recorded four ADRs
  (0015 CD-block HLE, 0016 master-leads-slave stepping, 0017 reference-oracle
  policy, 0018 save-state design); and recorded the M12 #9 cycle-accuracy
  residual's cross-emulator corroboration (the post-spin-up seek is Mednafen-only;
  MAME and Yabause match ours — left as-is per the stop rule).

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
