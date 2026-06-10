# 5thPlanet

An accuracy-first SEGA Saturn emulator in Rust.

*The Saturn is one of the hardest 5th-gen consoles to emulate* — eight
processors with tightly-coupled timing (2× SH-2 SH7604, MC68EC000,
VDP1, VDP2, SCU + SCU-DSP, SCSP M68k + SCSP-DSP, SH-1 CD-block) on a
shared bus. **Performance is explicitly subordinated to fidelity**:
this project will never include a JIT, dynarec, or any "approximate
cycle" shortcut. Each chip is built up one milestone at a time so the
foundation stays solid.

## Status

| Milestone | Goal                                                        | State        |
| --------- | ----------------------------------------------------------- | ------------ |
| M1        | Cycle-accurate SH-2 (SH7604) core                           | ✅ complete  |
| M2        | Saturn bus, dual SH-2, event-driven scheduler               | ✅ complete  |
| M3        | SCU, SMPC, VDP2 minimal, SCU-DSP, SDL2 window (scaffolding)  | ✅ complete  |
| M4        | BIOS splash on screen                                       | ✅ complete  |
| M5        | Chip-coverage build-out — VDP1, MC68EC000, full VDP2        | ✅ complete  |
| M6        | SCSP audio — slot/FM engine + SCSP-DSP                      | ✅ complete  |
| M7        | CD-block (HLE) + disc recognition + cartridge slot          | ✅ complete  |
| M8        | Save states + battery-backed backup RAM                     | ✅ complete  |
| M9        | Frontend OSD (in-window menu)                               | ✅ complete  |
| M10       | Live physical disc + CD audio                               | ✅ complete  |
| M11       | Boot a commercial game to gameplay                          | ✅ complete  |
| M12       | Whole-system cycle accuracy vs the reference emulator        | 🚧 active    |
| M13       | Hardware completeness & fidelity-gap backlog                 | 📋 in progress |

**What works today:** a real Saturn BIOS boots to its splash screen, and
commercial games run — **Virtua Fighter 2 is fully playable** (steady 60 fps,
3D fights with CD music and sound effects), and ***Doukyuusei ~if~* is fully
playable** (graphics, sound effects, and voices), including Shuttle Mouse
support. Games load from disc images (CUE/BIN, ISO,
CloneCD) or straight from an original disc in a host optical drive; save
states, the console's battery-backed save memory, expansion cartridges, and an
in-window menu (Esc — save slots, controller rebinding, region/cartridge/BIOS
switching, all persisted to a config file) are all in place.

Task-by-task technical status lives in [`doc/roadmap.md`](doc/roadmap.md).

| USA BIOS | Japanese BIOS (v1.01) |
| --- | --- |
| ![USA BIOS splash](doc/screenshots/bios-splash-usa.png) | ![Japanese v1.01 BIOS splash](doc/screenshots/bios-splash-jp.png) |

## Quick start

```bash
# Build & test everything
cargo test --workspace

# Lint (the codebase is a hand-maintained compact style — format added lines
# by hand; don't run `cargo fmt --all`, it reformats the whole tree)
cargo clippy --workspace --all-targets -- -D warnings

# Coverage (~85% line, excluding the SDL2 frontend + FFI crate)
cargo llvm-cov --workspace --summary-only

# Run a single test
cargo test -p sh2 -- decoder::tests::decodes_branches

# SDL2 frontend (default-on `sdl2-frontend` feature): opens a window and
# runs the supplied BIOS. Use --no-default-features for a headless run.
cargo run -p jupiter -- "bios/Sega Saturn BIOS (USA).bin"
```

## Controls

The SDL2 frontend maps the host keyboard — and any attached **game
controller** — to **port&nbsp;1** (a standard Saturn digital control pad) plus
a few emulator hotkeys. Every pad button can be rebound from the menu
(**Esc → Settings → Controller**, press-to-bind); the bindings persist in the
config file. Controllers hot-plug at any time (SDL's GameController layer —
XInput on Windows, evdev on Linux — so Xbox-style pads just work) and can
also navigate the menu (D-pad + A/B, Start toggles).

### Saturn control pad — port 1 (default bindings)

| Saturn button | Keyboard | Game controller |
| ------------- | -------- | --------------- |
| D-pad ↑ ↓ ← → | Arrow keys | D-pad or left stick |
| A / B / C     | Z / X / C | X / A / B |
| X / Y / Z     | A / S / D | Y / LB / RB |
| L / R         | Q / W | LT / RT |
| Start         | Enter | Start |

The gamepad mapping is fixed for now (it follows Sega's own layout from
their Xbox Saturn ports); per-button gamepad rebinding arrives with the
analog-peripheral work. The keyboard map is fully rebindable.

### Shuttle Mouse (`--mouse[=1|2]`)

Plug Sega's Saturn mouse into port&nbsp;2 (`--mouse`, keeping the keyboard pad
on port&nbsp;1) or port&nbsp;1 (`--mouse=1`, replacing the pad) for games that
support it — e.g. *Doukyuusei ~if~*. The host pointer is captured and hidden
while playing (the game draws its own cursor); host Left/Right/Middle clicks
map to the mouse buttons and **Enter** doubles as the mouse's Start button.

| Action                                   | Key |
| ---------------------------------------- | --- |
| Toggle pointer capture (free the cursor) | F10 |
| Release the pointer (menu open)          | Esc |

### Emulator hotkeys

| Action                            | Key |
| --------------------------------- | --- |
| Open / close the on-screen menu   | Esc |
| Quick save (to the quick slot)    | F5 |
| Quick load (from the quick slot)  | F9 |
| Play the disc's CD-audio track    | F8 |
| Toggle Shuttle Mouse capture      | F10 (with `--mouse`) |
| Quit                              | Close the window, or Esc → **Quit** |

### On-screen menu (while it is open)

| Action                              | Key |
| ----------------------------------- | --- |
| Move selection                      | ↑ / ↓ |
| Select                              | Enter or Z |
| Back (closes the menu at top level) | Backspace or X |
| Close menu                          | Esc |

## Workspace

- [`crates/sh2`](crates/sh2) — cycle-accurate SH-2 (SH7604) CPU core.
  `no_std` + `alloc`, no I/O.
- [`crates/m68k`](crates/m68k) — MC68EC000 core (the Saturn's SCSP sound
  CPU). `no_std` + `alloc`, library-shaped like `sh2`.
- [`crates/scu_dsp`](crates/scu_dsp) — SCU's embedded 32-bit DSP.
- [`crates/saturn`](crates/saturn) — Saturn system glue: memory map, dual
  SH-2 scheduler, SMPC, SCU + DMA + interrupt aggregator, VDP1 (full
  sprite/polygon plotter), VDP2 (multi-layer NBG/RBG compositor with
  rotation + live raster timing), SCSP (slot/FM audio engine + hosted
  MC68EC000 + SCSP-DSP), the CD-block (HLE: disc image + TOC,
  buffer/filter/partition, read pump + transfer, ISO9660 FS, authentication),
  and the cartridge slot (Extension DRAM / backup-RAM / ROM carts).
- [`crates/physdisc`](crates/physdisc) — live optical-drive `SectorSource`
  via libcdio, feature-gated (`libcdio`); the only crate that uses `unsafe`
  (FFI). Default build is a stub.
- [`crates/debugger`](crates/debugger) — interactive headless Saturn debugger
  (bin `sdbg`): a gdb-style REPL over the core with breakpoints (incl.
  register-guarded), single-step, SH-2 **and** SCSP-68k disassembly + PC-trace,
  read/write watchpoints, memory search, CD-block + SCSP/68k state, command
  history, and save-state rewind. `cargo run -p sdbg -- <bios.bin> [disc.cue]`.
- [`jupiter`](jupiter) — SDL2 frontend binary (window +
  framebuffer upload + audio, or headless), behind a default-on feature.
  Includes the hand-rolled in-window OSD menu (`src/osd/`, Esc to open) and
  the persisted config file (`src/config.rs`,
  `$XDG_CONFIG_HOME/5thplanet/jupiter.toml`).
  `cargo run -p jupiter -- <bios.bin> [disc.cue]`. (The binary is named
  `jupiter` for Jupiter — the 5th planet, hence the project name
  *5thPlanet* — the neighbour of Saturn: closest to it, but not identical.)

## BIOS

SEGA Saturn BIOS images go in `bios/` and are **never committed** —
they're copyrighted by SEGA and each developer supplies their own
legally-obtained dump. See [`bios/README.md`](bios/README.md) for the
expected filenames and rationale.

## Contributing

The repository ships with project-tailored Claude Code skills under
[`.claude/skills/`](.claude/skills) (`code-review`,
`commit-and-push`, `docs-engineering`, `release-engineering`,
`security-audit`). [`CLAUDE.md`](CLAUDE.md) documents the architecture
in depth and the conventions a contributor needs to follow. The
Saturn-specific vocabulary used throughout the codebase and commits
is collected in [`doc/glossary.md`](doc/glossary.md), and significant
design decisions and their rationale are recorded as Architecture
Decision Records in [`doc/adr/`](doc/adr/). For how the Saturn hardware
maps onto this project's crates and modules, see
[`doc/system-architecture.md`](doc/system-architecture.md).

## Acknowledgements

Three open-source emulators serve as **behavioral references**: local builds
are run against the same BIOS and games so their behavior can be compared
with ours, instruction by instruction where needed.

- [Mednafen](https://mednafen.github.io/) — the accuracy reference for
  game-level behavior, and the primary oracle for the game-boot and
  cycle-accuracy work.
- [MAME](https://github.com/mamedev/mame) — the low-level / early-boot
  reference for CPU, bus, and peripheral behavior.
- [Yabause](https://github.com/Yabause/yabause) — the secondary reference,
  and the primary one during early development.

Each is set up as a local, **never-committed** build (gitignored
`mednaref/`, `mameref/`, `yabref/`). **No emulator code is included in or
derived by this project** — they serve purely as behavioral references for
cross-checking; each is GPL-licensed and remains entirely separate from
5thPlanet's MIT-licensed source.

## Trademarks & copyrights

5thPlanet is an independent, unofficial project and is **not affiliated with,
endorsed by, or sponsored by SEGA**. "SEGA", "Sega Saturn", and the Sega Saturn
logos are trademarks of their respective owners (SEGA Corporation and/or its
affiliates). They are referenced here only for identification and
interoperability — to describe the hardware this emulator targets.

The Sega Saturn BIOS, games, and any disc images are **copyrighted by their
respective owners and are not distributed with this project** (see
[BIOS](#bios)); supply your own legally-obtained copies. The screenshots in
[`doc/screenshots/`](doc/screenshots) show SEGA's copyrighted boot logos and are
included solely to document the emulator's output.

No SEGA code, firmware, or assets are included in or derived by this project; it
is a clean-room behavioral re-implementation cross-checked against the
[reference emulators](#acknowledgements). Only the original source in this
repository is covered by the licence below.

## License

MIT — see [`LICENSE`](LICENSE). This licence applies to 5thPlanet's own source
code only, not to any third-party trademarks or copyrighted material referenced
above.
