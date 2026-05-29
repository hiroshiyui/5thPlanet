# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

5thPlanet is an **accuracy-first** SEGA Saturn emulator in Rust. The Saturn has eight processors with tightly-coupled timing (2× SH-2 SH7604, MC68EC000, VDP1, VDP2, SCU + SCU-DSP, SCSP M68k + SCSP-DSP, SH-1 CD-block); the project is built up one chip at a time so the foundation stays solid. **Performance is explicitly subordinated to fidelity** — never introduce a JIT, dynarec, or "approximate cycle" shortcut.

The one deliberate exception is the **SH-1 CD-block, which is high-level-emulated (HLE), not low-level-emulated** — its CD-ROM firmware is undumped (on-die mask ROM) and half its job is an analog servo with no observable digital ground truth, so there's nothing to be cycle-accurate *against*. Like every Saturn emulator (MAME/Yabause/Mednafen), we model the host command interface + the buffer/filter/partition engine + the CD-ROM filesystem, reading sectors from a disc image. This is M7; see `doc/roadmap.md` and `crates/saturn/src/cd_block.rs`.

**M1–M8 are complete.** M1 (cycle-accurate SH-2 core), M2 (Saturn bus + dual SH-2 + event-driven scheduler), M3 (SCU + SMPC + VDP2 minimal + SCU-DSP + SDL2 scaffolding), M4 (BIOS boots to the SEGA splash — now pixel-matching MAME), M5 (chip-coverage build-out: VDP1 full plotter, MC68EC000 core, full VDP2 NBG/RBG compositor), M6 (SCSP slot/FM audio engine), M7 (the **CD-block**, high-level-emulated — see below — plus the **cartridge slot**), and M8 (**save states + battery-backed backup RAM**). M7's five HLE phases (disc-image loading + TOC/session, the buffer/filter/partition engine, the read pump + data transfer, the ISO9660 filesystem, disc authentication) plus the cartridge slot are done. M8 adds `Saturn::save_state`/`load_state` (bincode snapshot of the whole machine; external media referenced not embedded) and a hardware-faithful, host-persisted internal backup RAM. **M9 (frontend OSD)** Phase 1 is done — a hand-rolled, software-composited in-window menu (ADR-0008): Esc opens it, with save/load slots, reset, eject/insert disc, and quit; graphics / controller / region-BIOS / cartridge submenus remain. **M10 is done: live physical disc + CDDA→SCSP** — a `SectorSource` trait (image or live drive), CD-audio BGM mixed into the SCSP, and optical-drive reads via the feature-gated `physdisc`/libcdio crate (ADR-0009). Still deferred from M7: the MPEG card and move/copy sector ops. **M11/M12 (boot a commercial game) are active** via an *opt-in* HLE direct boot (`--hle-boot`; the real-BIOS LLE path stays the default and reference): `Saturn::cold_hle_boot` loads the disc's 1st-read program, installs an HLE BIOS **SYS-call library** (`bios_hle`, ADR-0010/0011 — see the `bios_hle` bullet below), and hands off both SH-2s. Virtua Fighter 2 now runs its own code on both CPUs (master + slave dispatch, interrupts delivered) but does not yet render — work in progress. See `doc/roadmap.md` for task-by-task status and `doc/glossary.md` for the Saturn-specific vocabulary (chip names, address ranges, register acronyms) used throughout this file.

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

Run the binary with `cargo run -p fifth_planet -- <bios.bin>` — the SDL2 frontend (default-on `sdl2-frontend` feature) opens a window and runs the BIOS; `--no-default-features` runs headless.

## Architecture

### Workspace layout

```
crates/sh2/        — M1 deliverable: standalone SH-2 (SH7604) core.
                     no_std + extern alloc. Library-shaped, no I/O.
crates/m68k/       — M5 deliverable: MC68EC000 core (SCSP sound CPU).
                     no_std + alloc, library-shaped like sh2.
crates/scu_dsp/    — M3 deliverable: SCU's embedded 32-bit DSP.
crates/saturn/     — M2+ deliverable: Saturn system glue (bus, scheduler,
                     SMPC, SCU + DMA + INTC, VDP1 full plotter, VDP2
                     multi-layer NBG/RBG compositor + raster timing, SCSP
                     audio + hosted MC68EC000 + SCSP-DSP). CD-block is HLE:
                     disc image (`disc.rs`) + TOC, buffer/filter/partition,
                     read pump + data transfer, ISO9660 FS, authentication
                     (M7); cartridge slot (`cartridge.rs`, M7); save states
                     (`savestate.rs`) + battery backup RAM (M8); CDDA→SCSP +
                     the `SectorSource` trait (M10).
crates/physdisc/   — M10: live optical-drive `SectorSource` via libcdio,
                     feature-gated (`libcdio`); the sole `unsafe`/FFI crate
                     (ADR-0009). Default build is a stub.
fifth_planet/      — SDL2 frontend binary (window + framebuffer upload +
                     audio, or headless), behind the default-on
                     `sdl2-frontend` feature. The `osd` module (in-window
                     menu, ADR-0008) is hand-rolled + software-composited and
                     deliberately sdl2-free/core-free (operates on a `&mut [u8]`
                     RGBA buffer + a `Nav` enum), so it's unit-tested without a
                     window; `main.rs` bridges SDL events → `Nav`/pad.
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
- **Memory routing** (`Cpu::mem_read*`/`mem_write*`) decodes the SH-2 address into a `(physical, cacheable)` pair via `classify()`. Cached region (`0x00000000..0x1FFFFFFF`) consults the cache (hit = 0 stall; miss = 4 × `bus.read32` line-fill then install). Cache-through (`0x20000000..0x3FFFFFFF`) strips the high 3 bits and bypasses the cache. On-chip range (`0xFFFFFE00+`) routes to `Cpu::onchip` instead of the external bus — **except the cache-control register `CCR` at `0xFFFFFE92`, which `mem_read8`/`mem_write8` route to `self.cache` (the cache is a peer field, not part of `OnChip`); without that the BIOS can't enable the I-cache and runs ~8× slow.**
- **`cache::Cache`** stores tag + data per line. `lookup_*` returns `Hit([u8;16]) | Miss | Bypass`; the caller fetches from bus on miss and calls `install`. Writes are write-through (`write_through_u*`).
- **`exceptions` + `take_exception`** is the single entry point that pushes SR/PC, vectors via `VBR + vec*4`, optionally raises `SR.imask`. Interrupts checked at instruction boundary (never inside a delay slot); illegal and slot-illegal intercepted in `step()` before `execute()`.
- **`harness::MemBus`** is a flat big-endian RAM `Bus` impl for tests. New opcode integration tests under `crates/sh2/tests/` should build CPUs through it rather than introducing parallel bus mocks. `harness::state_digest(cpu, bus, regions)` returns a FNV-1a fingerprint used by the ROM regression harness.

### Saturn system (`crates/saturn/`) — pieces and their contracts

- **`memory::{BiosRom, Ram, StubRegisterBank}`** are typed region structs. Each owns its bytes with big-endian `read*/write*` at *region-local* offsets and folds out-of-range offsets modulo the region size (so any image that's smaller than its window mirrors transparently).
- **`bus::SaturnBus`** impls `sh2::Bus`. Dispatches every access through one `match addr` against `*_BASE..=*_END` region constants in `bus.rs`. Unmapped addresses behave as open bus (0 read, drop write). Wait states are SH7604 BSC defaults; later refinement keys on real BSC register values.
- **`smpc::Smpc`** is the System Manager + Peripheral Control. Register bank at *odd* byte offsets (every other byte reserved). A write to COMREG decodes the byte and queues a `Command` in `pending`; `SF` (status flag) goes busy. The Saturn aggregate drains queued commands between scheduler batches via `take_pending` / `mark_command_done`. `Command` discriminants are `#[repr(u8)]` and **match the hardware codes exactly** — `SshOn = 0x02`, **`IntBack = 0x10`**, `SetTime = 0x16`, **`NmiReq = 0x18`**, `ResEnab = 0x19`, `ResDisa = 0x1A` (these were verified against the SMPC manual; getting INTBACK/NMIREQ swapped silently breaks BIOS boot). **INTBACK is not instantaneous**: it keeps SF busy for `INTBACK_EXEC_CYCLES` (~250 µs ≈ 7150 cycles) via `intback_complete_at`, then fills OREG + raises the maskable SMPC interrupt + clears SF — the BIOS polls SF in a wait loop and derails if it clears too early.
- **`vdp2::Vdp2`** is the background generator: register bank + VRAM (512 KiB at `0x05E0_0000`) + CRAM (4 KiB) + a multi-layer compositor (`render_frame` in `vdp2/renderer.rs`). It composites NBG0–3 (tile or bitmap) and RBG0/1 (rotation, via `vdp2/rotation.rs`) by priority, plus the VDP1 sprite layer, with colour calculation, windows, and per-line scroll/zoom/cell-scroll. **Paletted-colour gotchas the BIOS splash exercised:** 8bpp character base is `char × 0x20` (an 8bpp cell is two `0x20` units), the per-layer colour-RAM address offset (`CRAOFA`/`CRAOFB`, `NxCAOS << 8`) selects the CRAM bank, and BGON `NxTPON`/`R0TPON` draw palette code 0 as the *solid* colour `CRAM[offset]` rather than transparent. `Vdp2::owns(addr)` gates bus dispatch. Raster registers (`VCNT`, `TVSTAT` VBLANK/HBLANK/ODD) are **live** — see `Saturn::update_video_timing`.
- **`vdp1::Vdp1`** is the sprite/polygon engine: a full command-list plotter (`vdp1/plotter.rs`) rasterising textured/scaled/distorted sprites, polygons and lines with gouraud shading and the colour-calc modes, into a double-buffered frame buffer (`0x05C8_0000`) that VDP2 composites. `PTMR` PTM=`0b01` draws once on the write; PTM=`0b10` (automatic) re-renders the list every frame at the buffer swap (`frame_change`).
- **`scsp::Scsp`** is the Sound Processor: 32-slot FM/PCM engine + SCSP-DSP (`scsp/dsp.rs`) + the hosted MC68EC000 (`m68k` crate) in sound RAM at `0x05A0_0000`, released by SMPC `SNDON`. `Saturn::take_audio` drains the mixed 44.1 kHz stereo each frame.
- **`cd_block::CdBlock`** is HLE (the SH-1 firmware is undumped — see above), modelled on MAME `saturn_cd_hle.cpp`. The host interface (HIRQ/CR1–4, `cmd_pending==0xF` dispatch, periodic report) drives the full engine: the **media** ([`disc::Disc`] — ISO/CUE-BIN/CCD parsers, FAD addressing, TOC); a **200-block pool** + **24 filters/partitions** (FAD-range/subheader matching, true/false routing); a **75 Hz read pump** (`tick` → `play_data` → `read_filtered_sector` → `filter_data`) feeding partitions; **data transfer** (16-bit FIFO + 32-bit SCU-DMA port at `0x0581_8000`); the **ISO9660 filesystem** (`read_new_dir`/`make_dir_current` + the file commands); and **authentication** (`0xE0`/`0xE1`). `insert_disc`/`eject_disc` move the drive between disc-present (status `PAUSE`) and empty — **no disc reports `NODISC` (0x07), not `PAUSE`** (matches MAME `saturn_cd_hle.cpp`; the old "report PAUSE with no image" was wrong). **M10:** sectors are read through a `disc::SectorSource` trait (in-memory `Disc` *or* a live drive via `physdisc`), not the concrete image — `disc` is `Option<Box<dyn SectorSource>>`, `insert_disc` is generic, and reads fill a caller buffer; this is why `SaturnBus`/`CdBlock` are **not `Clone`**. When an **audio** track plays, the read pump decodes 2352-byte sectors to a CD-DA FIFO (`cd_audio`) that `Saturn::take_audio` sums into the SCSP output (CDDA→SCSP). MPEG card and move/copy ops remain.
- **`cartridge::Cartridge`** is the rear expansion slot at `0x0200_0000..0x04FF_FFFF` (gated by `Cartridge::owns`). An enum — `None` (empty, floats high to `0xFF`), `Dram` (Extension RAM: two independent mirrored banks at `0x0240_0000`/`0x0260_0000`, ID `0x5A`=1 MB / `0x5C`=4 MB), `Bram` (battery RAM, IDs `0x21`–`0x24`), `Rom` (game ROM at `0x0200_0000`, ID `0xFF`). The cart-ID byte lives at `0x04FF_FFFF`. **All widths compose from `read8_impl`/`write8_impl`** so the backup cart's odd-byte packing (data only in bits 23–16 and 7–0 of each 32-bit word, matching MAME `read_ext_bram`) and the ID-byte placement stay consistent. Plugged in via `Saturn::insert_cartridge` or the frontend `--cart=` flag.
- **`savestate`** (M8) gives `Saturn::save_state() -> Vec<u8>` / `load_state(&[u8])`: a `bincode` snapshot of the whole machine. **Every state type derives `Serialize`/`Deserialize`** — in `saturn` the derive is unconditional; in `sh2`/`m68k`/`scu_dsp` it's behind an optional `serde` feature that `saturn` turns on (keeps the cores dependency-free standalone). Arrays >32 use `serde-big-array` (`#[serde(with = "BigArray")]`), except scu_dsp's `[[u32;64];4]` data RAM (a no_std flat-tuple codec, since big-array is 1-D only). **External media is referenced, not embedded**: `BiosRom.rom`, `CdBlock.disc`, and `Cartridge::Rom.bytes` are `#[serde(skip)]`'d and re-grafted from the live instance in `load_state`; a magic + version header plus FNV-1a BIOS/disc fingerprints reject a load onto the wrong media. Determinism is the contract — the round-trip test boots, snapshots, then runs the snapshot and the original forward by the same budget and asserts identical re-snapshots.
- **`memory::BackupRam`** is the internal 32 KiB battery-backed backup RAM at `0x0018_0000` (the console's built-in memory card). It models the hardware **odd-byte packing** (data only on odd byte addresses, even bytes read 0; `data[(off>>1) % 0x8000]`), matching MAME `backupram_r/w` and the cart `Bram` packing, and is pre-formatted with the "BackUpRam Format" tag (MAME `nvram_init`). `Saturn::internal_backup()`/`load_internal_backup()` expose the raw 32 KiB; the frontend persists it to a `<bios>.bup` file. **Don't revert it to a linear `Ram`** — the packing is load-bearing for backup-manager compatibility.
- **`scu::Scu`** is the System Control Unit. Holds three DMA channels plus timers/IMS/IST/AIACK/ASR/RSEL/VER. DMA trigger fires *only* on 32-bit writes to `D*EN` with bit 8 (DGO) set and non-zero transfer count — byte/halfword writes deliberately don't fire, because software builds the register up piece-by-piece. `take_pending_dma` / `finish_dma` mirror the SMPC drain pattern.
- **`scheduler::SchedEntity`** trait has an associated `Context` so real chips (`SaturnBus`) and test fakes (`()`) can both use it. `next_deadline()` is the global cycle the entity wants to run at; `step(ctx)` advances one unit of work. `Sh2Entity` wraps `sh2::Cpu`; when its `halted` flag is set, `next_deadline()` returns `u64::MAX` so the scheduler's "smallest deadline wins" rule naturally skips it. **Un-halting must resync the cycle**: while halted an entity's `pipeline.cycles` freezes (it's skipped), so releasing it (`Saturn::release_slave` on SMPC `SSHON`) must bump `pipeline.cycles` up to the current `now()` first — otherwise the scheduler sees it as millions of cycles "behind" and runs that many catch-up steps of stale code in one batch ("time travel"). This was a real bug: the slave's stale code ran far enough to zero an HLE-booted game's program. Regression: `dual_sh2::releasing_slave_resyncs_its_cycle_no_time_travel`.
- **`scheduler::Scheduler<E>`** linear-scans `entities` per step. Ties resolve to insertion order — this is the entire determinism contract. Once entity count grows past a handful in M3+, swap the scan for a `BinaryHeap`.
- **`system::Saturn`** owns the bus + scheduler + master/slave IDs. `Saturn::run_for(cycles)` is the headless main loop: runs the scheduler in `SMPC_POLL_QUANTUM = 256`-cycle batches, then between batches calls `update_video_timing()` (derives `VCNT`/`TVSTAT` from the global cycle and raises VBlank-IN on the active→VBLANK edge), `drain_smpc()`, `drain_scu_dma()`, and `drain_scu_intc()` (forwards the highest-priority unmasked SCU source to the master INTC). `run_frame(out)` runs one 263-line NTSC frame and snapshots the framebuffer at the active→VBLANK boundary. `reset()` halts the slave (matches power-on; BIOS releases it via SMPC `SSHON`). `drain_input_capture()` applies the inter-CPU FRT input-capture (FTI) pulses the bus flagged this batch (see below). `debug_step_master` / `debug_drain` are test-only single-step hooks used by the reference-trace diff.
- **Inter-CPU FRT input-capture (FTI)** — the two SH-2s wake each other via the free-running-timer input-capture pin: a **16-bit** write to `0x0100_0000..0x017F_FFFF` pulses the *slave*'s FTI, `0x0180_0000..0x01FF_FFFF` the *master*'s (`SLAVE_FTI_BASE`/`MASTER_FTI_BASE` in `bus.rs`). The pulse latches FRC→FICR and sets `FTCSR.ICF` (`sh2::onchip::Frt::input_capture`). The bus can't reach the cores, so `write16` just flags `slave/master_input_capture` and `Saturn::drain_input_capture` applies it at the aggregate (the SMPC/SCU drain pattern). This is how VF2's master releases its slave's `ICF`-polling dispatch loop.
- **`bios_hle`** is the HLE of the BIOS **system-call library** (ADR-0011, the M11/M12 cold-HLE direct boot — *opt-in*, the real-BIOS LLE path stays default). Games reach BIOS services through a pointer table in low work RAM (`0x06000200..0x06000360`); `Saturn::cold_hle_boot` loads the 1st-read program (`hle_boot`), writes that table (`install_call_table`), and enables a per-core dispatch hook in `Sh2Entity::step` (`hle_sys_active`) that intercepts a `JSR` landing on a SYS entry and runs a host `dispatch(cpu, bus, is_slave)` in place of BIOS code (modelled on Yabause `bios.c`). Implemented fns: `ChangeSystemClock`, `Get/ClearSemaphore`, `Set/GetScuInterrupt`, `Set/GetSh2Interrupt`, `Set/ChangeScuInterruptMask` (the last two **master-only** — a slave call must not clobber the SCU mask). The **slave** is started the BIOS way on `SSHON` (`release_slave`, Yabause `YabauseStartSlave`): jump to the game-written entry `[0x06000250]`, `VBR = 0x06000400`, slave stack `[0x060002AC]` or `0x06001000` — not a resume of stale BIOS code.

### Borrow patterns to know

- Methods on `Saturn` that need both `&mut self.bus` AND a scheduler entity (e.g. `reset()`) must **destructure `self`** into per-field borrows. Going through the `master_mut()` accessor borrows the whole `Self` and conflicts with the bus borrow. See `Saturn::reset` for the pattern.
- The "queue a side effect, drain at the aggregate" pattern (SMPC + SCU) sidesteps the bus-self-borrow problem: the peripheral notes "I want to fire" in a field; the aggregate later pops the notification (releasing the peripheral borrow) and uses `&mut self.bus` freely.

### Cycle counting

Issue costs returned from `execute()` come from Appendix A of the *SH-1/SH-2 Software Manual*; bus stalls returned from `Bus` are added on top. Don't invent cycle counts — every value should be traceable to a manual entry. Pipeline interlocks (load-use, multiply latency, branch overhead) refine the model further; assertions live in `crates/sh2/tests/pipeline_timing.rs`.

## Project conventions

- **Test layout** — opcode tests are integration tests under `crates/sh2/tests/`, one file per instruction family. Saturn-side integration tests live under `crates/saturn/tests/` (one file per peripheral: `bus_routing.rs`, `scheduler.rs`, `dual_sh2.rs`, `smpc.rs`, `scu.rs`, `vdp1.rs`, `vdp2_storage.rs`, `vdp2_render.rs`, `cd_block.rs`, `cartridge.rs`, `savestate.rs`, `bios_boot.rs`). Decoder / pure-module tests live in `#[cfg(test)] mod tests` inside the source file.
- **Doc comments** — public items in `sh2` should cite the SH-2 manual section they implement when the semantics are non-obvious (delay slots, PC base, SR effects, cycle costs). Public items in `saturn` should call out the SH7604 hardware manual section for the peripheral or memory-map detail being modeled.
- **No `println!`/`eprintln!` in `sh2`** — the crate is `no_std` + `alloc`. Tracing belongs in `debug.rs`.
- **Commits** — Conventional Commits with scopes `sh2` / `saturn` / `frontend` / `workspace` / `doc` / `ci`. Reference roadmap task numbers when a commit advances the active milestone (e.g. "advances M3 task #2"). Each task that lands also updates `doc/roadmap.md` in a separate `docs:` commit; don't bundle.

## Skills available in `.claude/skills/`

`code-review`, `commit-and-push`, `docs-engineering`, `release-engineering`, `security-audit` — all tailored to this project. Prefer invoking them over re-deriving their checklists.
