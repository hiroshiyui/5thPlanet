# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

5thPlanet is an **accuracy-first** SEGA Saturn emulator in Rust. The Saturn has eight processors with tightly-coupled timing (2× SH-2 SH7604, MC68EC000, VDP1, VDP2, SCU + SCU-DSP, SCSP M68k + SCSP-DSP, SH-1 CD-block); the project is built up one chip at a time so the foundation stays solid. **Performance is explicitly subordinated to fidelity** — never introduce a JIT, dynarec, or "approximate cycle" shortcut.

**M1 (cycle-accurate SH-2 core) and M2 (Saturn bus + dual SH-2 + event-driven scheduler) are complete.** M3 is active: SCU, SMPC, VDP2 minimal, SDL2 window, goal = SEGA splash on screen. See `doc/roadmap.md` for task-by-task status and `doc/glossary.md` for the Saturn-specific vocabulary (chip names, address ranges, register acronyms) used throughout this file.

## Common commands

```bash
cargo check --workspace                    # fastest correctness pass
cargo build --workspace
cargo test  --workspace                    # all unit + integration tests
cargo test  -p sh2                         # just the SH-2 core
cargo test  -p saturn                      # just the Saturn system layer
cargo test  -p sh2 --test opcodes_basic    # one integration test file
cargo test  -p sh2 -- decoder::tests::decodes_branches   # single test by path
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

Run the binary with `cargo run -p fifth_planet` (currently just `Hello, world!`; the SDL2 frontend lands with M3 task #7).

## Architecture

### Workspace layout

```
crates/sh2/        — M1 deliverable: standalone SH-2 (SH7604) core.
                     no_std + extern alloc. Library-shaped, no I/O.
crates/saturn/     — M2+ deliverable: Saturn system glue (bus, scheduler,
                     SMPC, SCU + DMA + INTC). VDP2/VDP1/SCSP/CD-block
                     to follow.
crates/scu_dsp/    — M3 deliverable: SCU's embedded 32-bit DSP. Standalone
                     for now; SCU host wiring lands when target microcode
                     surfaces in M3+/M4.
fifth_planet/      — Binary crate. Gets the SDL2 frontend in M3 task #7.
doc/roadmap.md     — Milestone tracker. Update task status as work lands.
bios/              — Saturn BIOS images. Gitignored; see bios/README.md.
```

The root `Cargo.toml` is a `[workspace]` with `resolver = "3"` and edition 2024. All member crates inherit `version`, `edition`, `authors`, `license` from `[workspace.package]`. The lint `unsafe_code = "forbid"` is set workspace-wide — keep it that way; any new `unsafe` block requires an explicit `#![allow(unsafe_code)]` with justification, and reviewers should treat that as Critical until argued.

### SH-2 core (`crates/sh2/`) — pieces and their contracts

- **`bus::Bus` trait** is the only trust boundary. Each read/write method returns `(value, stall_cycles)`. The host owns wait-state math; the CPU just accumulates. SH-2 is **big-endian** — use `from_be_bytes` / `to_be_bytes` always.
- **`isa::Op`** is one variant per distinct SH-2 encoding (~142 variants). Operand fields (`rn`, `rm`, `imm`, `disp`) are pre-extracted by the decoder so the interpreter never re-parses the raw word. `Op::is_illegal_in_slot()` flags ops that must not appear in a delay slot — extend it when adding new branch/jump/SR-modifying ops. `Op::reads_reg()`, `load_dest()`, `multiply_latency()`, `reads_mac()` drive the pipeline scoreboard; extend them in lockstep.
- **`decoder::decode(u16) -> Op`** is a pure match dispatched on the top nibble, then on the bottom nibble or sub-opcode. Layout mirrors the SH-2 software manual's encoding tables.
- **`interpreter::Cpu::step()`** does interrupt check → fetch → decode → interlock check → execute → cycle-accumulate → scoreboard update. **Delay-slot machinery is centralised here**: when `pending_branch` is `Some`, the next step's instruction is the slot, and PC is overwritten to the branch target *after* the slot retires. Branch opcodes only set `pending_branch`; they never mutate PC directly.
- **PC-relative addressing uses `instr_pc + 4`**, not the running `regs.pc`. The instruction's own address is plumbed into `execute()` as `instr_pc`; use that for `MOV.L @(d,PC),Rn`, `BRA`, `BSR`, etc.
- **Memory routing** (`Cpu::mem_read*`/`mem_write*`) decodes the SH-2 address into a `(physical, cacheable)` pair via `classify()`. Cached region (`0x00000000..0x1FFFFFFF`) consults the cache (hit = 0 stall; miss = 4 × `bus.read32` line-fill then install). Cache-through (`0x20000000..0x3FFFFFFF`) strips the high 3 bits and bypasses the cache. On-chip range (`0xFFFFFE00+`) routes to `Cpu::onchip` instead of the external bus.
- **`cache::Cache`** stores tag + data per line. `lookup_*` returns `Hit([u8;16]) | Miss | Bypass`; the caller fetches from bus on miss and calls `install`. Writes are write-through (`write_through_u*`).
- **`exceptions` + `take_exception`** is the single entry point that pushes SR/PC, vectors via `VBR + vec*4`, optionally raises `SR.imask`. Interrupts checked at instruction boundary (never inside a delay slot); illegal and slot-illegal intercepted in `step()` before `execute()`.
- **`harness::MemBus`** is a flat big-endian RAM `Bus` impl for tests. New opcode integration tests under `crates/sh2/tests/` should build CPUs through it rather than introducing parallel bus mocks. `harness::state_digest(cpu, bus, regions)` returns a FNV-1a fingerprint used by the ROM regression harness.

### Saturn system (`crates/saturn/`) — pieces and their contracts

- **`memory::{BiosRom, Ram, StubRegisterBank}`** are typed region structs. Each owns its bytes with big-endian `read*/write*` at *region-local* offsets and folds out-of-range offsets modulo the region size (so any image that's smaller than its window mirrors transparently).
- **`bus::SaturnBus`** impls `sh2::Bus`. Dispatches every access through one `match addr` against `*_BASE..=*_END` region constants in `bus.rs`. Unmapped addresses behave as open bus (0 read, drop write). Wait states are SH7604 BSC defaults; later refinement keys on real BSC register values.
- **`smpc::Smpc`** is the System Manager + Peripheral Control. Register bank at *odd* byte offsets (every other byte reserved). A write to COMREG decodes the byte and queues a `Command` in `pending`; `SF` (status flag) goes busy. The Saturn aggregate drains queued commands between scheduler batches via `take_pending` / `mark_command_done`. `Command` discriminants are `#[repr(u8)]` and match hardware codes — `SshOn as u8 == 0x02`.
- **`scu::Scu`** is the System Control Unit. Holds three DMA channels plus timers/IMS/IST/AIACK/ASR/RSEL/VER. DMA trigger fires *only* on 32-bit writes to `D*EN` with bit 8 (DGO) set and non-zero transfer count — byte/halfword writes deliberately don't fire, because software builds the register up piece-by-piece. `take_pending_dma` / `finish_dma` mirror the SMPC drain pattern.
- **`scheduler::SchedEntity`** trait has an associated `Context` so real chips (`SaturnBus`) and test fakes (`()`) can both use it. `next_deadline()` is the global cycle the entity wants to run at; `step(ctx)` advances one unit of work. `Sh2Entity` wraps `sh2::Cpu`; when its `halted` flag is set, `next_deadline()` returns `u64::MAX` so the scheduler's "smallest deadline wins" rule naturally skips it.
- **`scheduler::Scheduler<E>`** linear-scans `entities` per step. Ties resolve to insertion order — this is the entire determinism contract. Once entity count grows past a handful in M3+, swap the scan for a `BinaryHeap`.
- **`system::Saturn`** owns the bus + scheduler + master/slave IDs. `Saturn::run_for(cycles)` is the headless main loop: runs the scheduler in `SMPC_POLL_QUANTUM = 256`-cycle batches, then calls `drain_smpc()` and `drain_scu_dma()` between batches. `reset()` halts the slave (matches power-on; BIOS releases it via SMPC `SSHON`).

### Borrow patterns to know

- Methods on `Saturn` that need both `&mut self.bus` AND a scheduler entity (e.g. `reset()`) must **destructure `self`** into per-field borrows. Going through the `master_mut()` accessor borrows the whole `Self` and conflicts with the bus borrow. See `Saturn::reset` for the pattern.
- The "queue a side effect, drain at the aggregate" pattern (SMPC + SCU) sidesteps the bus-self-borrow problem: the peripheral notes "I want to fire" in a field; the aggregate later pops the notification (releasing the peripheral borrow) and uses `&mut self.bus` freely.

### Cycle counting

Issue costs returned from `execute()` come from Appendix A of the *SH-1/SH-2 Software Manual*; bus stalls returned from `Bus` are added on top. Don't invent cycle counts — every value should be traceable to a manual entry. Pipeline interlocks (load-use, multiply latency, branch overhead) refine the model further; assertions live in `crates/sh2/tests/pipeline_timing.rs`.

## Project conventions

- **Test layout** — opcode tests are integration tests under `crates/sh2/tests/`, one file per instruction family. Saturn-side integration tests live under `crates/saturn/tests/` (one file per peripheral: `bus_routing.rs`, `scheduler.rs`, `dual_sh2.rs`, `smpc.rs`, `scu.rs`). Decoder / pure-module tests live in `#[cfg(test)] mod tests` inside the source file.
- **Doc comments** — public items in `sh2` should cite the SH-2 manual section they implement when the semantics are non-obvious (delay slots, PC base, SR effects, cycle costs). Public items in `saturn` should call out the SH7604 hardware manual section for the peripheral or memory-map detail being modeled.
- **No `println!`/`eprintln!` in `sh2`** — the crate is `no_std` + `alloc`. Tracing belongs in `debug.rs`.
- **Commits** — Conventional Commits with scopes `sh2` / `saturn` / `frontend` / `workspace` / `doc` / `ci`. Reference roadmap task numbers when a commit advances the active milestone (e.g. "advances M3 task #2"). Each task that lands also updates `doc/roadmap.md` in a separate `docs:` commit; don't bundle.

## Skills available in `.claude/skills/`

`code-review`, `commit-and-push`, `docs-engineering`, `release-engineering`, `security-audit` — all tailored to this project. Prefer invoking them over re-deriving their checklists.
