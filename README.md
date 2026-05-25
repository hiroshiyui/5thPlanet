# 5thPlanet

An accuracy-first SEGA Saturn emulator in Rust.

The Saturn is one of the hardest 5th-gen consoles to emulate — eight
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
| M4        | Finish the M3 stretch — SEGA splash on screen               | 🚧 active   |
| M5+       | VDP1 rendering, audio (SCSP+M68k), save states, CD-block     | queued       |

Current test count: **278 workspace-wide, 0 failures.** Task-by-task
status lives in [`doc/roadmap.md`](doc/roadmap.md).

M4 is the splash push. A real BIOS now boots far past M3's early init —
verified bit-for-bit against a reference emulator (see
[Acknowledgements](#acknowledgements)) — onto the genuine boot path;
the remaining gap is VDP2 raster-timing precision.

## Quick start

```bash
# Build & test everything
cargo test --workspace

# Format / lint
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings

# Run a single test
cargo test -p sh2 -- decoder::tests::decodes_branches

# SDL2 frontend (default-on `sdl2-frontend` feature): opens a window and
# runs the supplied BIOS. Use --no-default-features for a headless run.
cargo run -p fifth_planet -- "bios/Sega Saturn BIOS (USA).bin"
```

## Workspace

- [`crates/sh2`](crates/sh2) — cycle-accurate SH-2 (SH7604) CPU core.
  `no_std` + `alloc`, no I/O.
- [`crates/saturn`](crates/saturn) — Saturn system: memory map, dual
  SH-2 scheduler, SMPC, SCU + DMA + interrupt aggregator, VDP2 (minimal
  NBG0 renderer + live raster timing), and address-space stubs for VDP1
  and the CD-block. SCSP and full VDP1 rendering to follow.
- [`crates/scu_dsp`](crates/scu_dsp) — SCU's embedded 32-bit DSP.
  Standalone for now; wired into the SCU host as later microcode
  needs surface.
- [`fifth_planet`](fifth_planet) — SDL2 frontend binary (window +
  framebuffer upload, or headless), behind a default-on feature.

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
Decision Records in [`doc/adr/`](doc/adr/).

## Acknowledgements

During early development the open-source
[Yabause](https://github.com/Yabause/yabause) emulator was leaned on
heavily as a **reference oracle for verifying system architecture**: a
patched, headless Yabause was run against the same BIOS and its master
SH-2 instruction trace diffed against ours, which let us confirm the
SH-2 core, cache, SMPC, SCU, and bus reproduce known-good behavior
bit-for-bit and pinpoint boot-sequence bugs down to the exact
instruction. No Yabause code is included in or derived by this project —
it served purely as a behavioral reference for cross-checking.

## License

MIT — see [`LICENSE`](LICENSE).
