# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

5thPlanet is an **accuracy-first** SEGA Saturn emulator in Rust. The Saturn has eight processors with tightly-coupled timing (2× SH-2 SH7604, MC68EC000, VDP1, VDP2, SCU + SCU-DSP, SCSP M68k + SCSP-DSP, SH-1 CD-block); the project is built up one chip at a time so the foundation stays solid. **Performance is explicitly subordinated to fidelity** — never introduce a JIT, dynarec, or "approximate cycle" shortcut.

Current milestone is **M1**: a standalone, cycle-accurate SH-2 (SH7604) interpreter validated by unit tests and ROM regressions. See `doc/roadmap.md` for task-by-task status.

## Common commands

```bash
cargo check --workspace                    # fastest correctness pass
cargo build --workspace
cargo test  --workspace                    # all unit + integration tests
cargo test  -p sh2                         # just the SH-2 core
cargo test  -p sh2 --test opcodes_basic    # one integration test file
cargo test  -p sh2 -- decoder::tests::decodes_branches   # single test by path
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Run the binary with `cargo run -p fifth_planet` (currently just `Hello, world!`; the SDL2 frontend lands with M2).

## Architecture

### Workspace layout

```
crates/sh2/        — M1 deliverable: standalone SH-2 (SH7604) core.
                     no_std + extern alloc. Library-shaped, no I/O.
crates/saturn/     — Empty stub. Will hold the system bus, scheduler,
                     VDP1/2, SCU, SCSP, CD-block from M2 onward.
fifth_planet/      — Binary crate. Will host the SDL2 frontend.
doc/roadmap.md     — Milestone tracker. Update task status as work lands.
```

The root `Cargo.toml` is a `[workspace]` with `resolver = "3"` and edition 2024. All member crates inherit `version`, `edition`, `authors`, `license` from `[workspace.package]`. The lint `unsafe_code = "forbid"` is set workspace-wide — keep it that way; any new `unsafe` block requires an explicit `#![allow(unsafe_code)]` with justification, and reviewers should treat that as Critical until argued.

### SH-2 core (`crates/sh2/`) — pieces and their contracts

- **`bus::Bus` trait** is the only trust boundary. Each read/write method returns `(value, stall_cycles)`. The host owns wait-state math; the CPU just accumulates. New bus impls (e.g. the future Saturn bus) plug into the unchanged `Cpu`. SH-2 is **big-endian** — use `from_be_bytes` / `to_be_bytes` always.
- **`isa::Op`** is one variant per distinct SH-2 encoding (~142 variants). Operand fields (`rn`, `rm`, `imm`, `disp`) are pre-extracted by the decoder so the interpreter never re-parses the raw word. `Op::is_illegal_in_slot()` flags ops that must not appear in a delay slot — extend it when adding new branch/jump/SR-modifying ops.
- **`decoder::decode(u16) -> Op`** is a pure match dispatched on the top nibble, then on the bottom nibble or sub-opcode. Layout mirrors the SH-2 software manual's encoding tables; keep that manual open when editing this file.
- **`interpreter::Cpu::step()`** does fetch → decode → execute → cycle-accumulate. **Delay-slot machinery is centralised here**: when `pending_branch` is `Some`, the next step's instruction is the slot, and PC is overwritten to the branch target *after* the slot retires. Branch opcodes only set `pending_branch`; they never mutate PC directly.
- **PC-relative addressing uses `instr_pc + 4`**, not the running `regs.pc`. The instruction's own address is plumbed into `execute()` as `instr_pc`; use that for `MOV.L @(d,PC),Rn`, `BRA`, `BSR`, etc.
- **`harness::MemBus`** is a flat big-endian RAM `Bus` impl for tests and (eventually) the ROM regression harness. New opcode integration tests under `crates/sh2/tests/` should build CPUs through it rather than introducing parallel bus mocks.
- **`pipeline`, `cache`, `exceptions`, `onchip/*`** are skeleton modules. They'll be filled out in roadmap tasks #5–#7. Read those task descriptions before implementing — the cycle model in particular needs to compose with `Bus`-returned stall counts.

### Cycle counting

Issue costs returned from `execute()` come from Appendix A of the *SH-1/SH-2 Software Manual*; bus stalls returned from `Bus` are added on top. Don't invent cycle counts — every value should be traceable to a manual entry. Once task #5 lands, pipeline interlocks (load-use, multiply latency, branch overhead) refine the model further; assertions for those live in `tests/pipeline_timing.rs`.

## Project conventions

- **Test layout** — opcode tests are integration tests under `crates/sh2/tests/`, one file per instruction family (`opcodes_basic.rs`, future `opcodes_logic.rs`, etc.). Decoder tests live in `#[cfg(test)] mod tests` inside `decoder.rs`.
- **Doc comments** — public items in `sh2` should cite the SH-2 manual section they implement when the semantics are non-obvious (delay slots, PC base, SR effects, cycle costs).
- **No `println!`/`eprintln!` in `sh2`** — the crate is `no_std` + `alloc`. Tracing belongs in `debug.rs`.
- **Commits** — Conventional Commits with scopes `sh2` / `saturn` / `frontend` / `workspace` / `doc` / `ci`. Reference roadmap task numbers when a commit advances M1 (e.g. "advances M1 task #4").

## Skills available in `.claude/skills/`

`code-review`, `commit-and-push`, `docs-engineering`, `release-engineering`, `security-audit` — all tailored to this project. Prefer invoking them over re-deriving their checklists.
