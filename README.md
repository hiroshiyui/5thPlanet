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

| Milestone | Goal                                                    | State        |
| --------- | ------------------------------------------------------- | ------------ |
| M1        | Cycle-accurate SH-2 (SH7604) core                       | ✅ complete  |
| M2        | Saturn bus, dual SH-2, event-driven scheduler           | ✅ complete  |
| M3        | SCU, SMPC, VDP2 minimal, SDL2 window: BIOS to splash    | 🚧 active   |
| M4+       | VDP1, audio, save states, CD block, first game booting  | queued       |

Current test count: **210 workspace-wide, 0 failures.** Task-by-task
status lives in [`doc/roadmap.md`](doc/roadmap.md).

## Quick start

```bash
# Build & test everything
cargo test --workspace

# Format / lint
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings

# Run a single test
cargo test -p sh2 -- decoder::tests::decodes_branches

# The SDL2 frontend isn't wired yet (M3 task #7); `cargo run` is
# currently a placeholder.
cargo run -p fifth_planet
```

## Workspace

- [`crates/sh2`](crates/sh2) — cycle-accurate SH-2 (SH7604) CPU core.
  `no_std` + `alloc`, no I/O.
- [`crates/saturn`](crates/saturn) — Saturn system: memory map, dual
  SH-2 scheduler, SMPC, SCU + DMA + interrupt aggregator. VDP1/2,
  SCSP, CD-block to follow.
- [`crates/scu_dsp`](crates/scu_dsp) — SCU's embedded 32-bit DSP.
  Standalone for now; wired into the SCU host as M3+/M4 microcode
  needs surface.
- [`fifth_planet`](fifth_planet) — frontend binary. Gets an SDL2
  window in M3 task #7.

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
is collected in [`doc/glossary.md`](doc/glossary.md).

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
