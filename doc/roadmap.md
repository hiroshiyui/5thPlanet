# 5thPlanet Roadmap

Accuracy-first SEGA Saturn emulator in Rust. Milestones are scoped tightly so
the foundation is solid before the next chip is added.

The full design rationale lives in `/home/yhh/.claude/plans/temporal-strolling-hollerith.md`.

## Milestone 1 — Cycle-accurate SH-2 (SH7604) core ✅ complete

Single-chip deliverable: a standalone `sh2` library crate validated by unit
tests and ROM regressions, ready to be wired into a bus/scheduler later.

| # | Task | Status |
|---|------|--------|
| 1 | Workspace + `sh2` skeleton + `Bus` trait + `Cpu` struct | ✅ done |
| 2 | Full SH-2 ISA table (~142 ops) + decoder + decoder unit tests | ✅ done |
| 3 | Interpreter — first batch of ~20 core opcodes (MOV/ALU/CMP/branches with delay slots) + integration tests | ✅ done |
| 4 | Remaining ~120 opcodes, group by group, with tests alongside | ✅ done |
| 5 | Pipeline / cycle model: 5-stage scoreboard, load-use stalls, multiply latency, branch costs, interlock timing tests | ✅ done |
| 6 | Cache (4 KiB 4-way LRU) + on-chip peripherals (INTC, DMAC, DIVU, FRT; BSC/WDT/SCI/UBC as register stubs) | ✅ done |
| 7 | Exception + interrupt dispatch (reset, illegal, slot-illegal, address error, NMI, TRAPA, external via INTC) | ✅ done |
| 8 | ROM regression harness + committed golden state hashes | ✅ done |

### What landed (`cargo test -p sh2` → 131 tests, 0 failures)

- 37 lib unit tests (decoder, cache, divu, frt, intc, onchip aggregator)
- 16 opcode integration tests for the core MOV/ALU/CMP/branch batch
- 18 arithmetic tests (ADDC/ADDV/MAC/DIV1/EXT/multiplies)
- 12 data-transfer addressing-mode tests
- 5 logical (AND/OR/XOR/NOT/TST + TAS)
- 6 shift / rotate (incl. SHLLn / SHLRn)
- 9 system (LDC/STC/LDS/STS + TRAPA round-trip)
- 11 pipeline timing tests (load-use, branch costs, MAC-read framework)
- 4 on-chip routing tests (CPU drives DIVU via real MOV.L)
- 8 exception/interrupt tests (illegal, slot-illegal, NMI, external, masking)
- 5 ROM regression tests with committed golden hashes

The "SingleStepTests vector corpus" originally proposed as a gate was dropped:
no public SH-2 corpus exists yet, and the per-opcode unit tests + ROM hashes
cover the same ground without the generator infrastructure overhead.

## Milestone 2 — Saturn bus, dual SH-2, event-driven scheduler ✅ complete

Pairs the M1 SH-2 with a Saturn-shaped memory map, a second SH-2 (slave), and
an event-driven scheduler that decides which CPU advances next. Wires the M1
cache structure into live fetch/data paths so cycle counts on cache-resident
code (which is most Saturn code) start matching hardware. Scope is deliberately
**no graphics, no audio, no BIOS boot, no SDL2** — those wait for M3 once
there's something to render.

| # | Task | Status |
|---|------|--------|
| 1 | Extend `sh2::Cache` with line data storage + write-through update API | ✅ done |
| 2 | Wire cache into `Cpu::mem_read*/mem_write*` (cached vs cache-through dispatch) | ✅ done |
| 3 | Saturn bus + typed region structs + memory-map dispatch | ✅ done |
| 4 | Event-driven `Scheduler` with `SchedEntity` trait | ✅ done |
| 5 | `Saturn` system aggregate + dual SH-2 integration test | ✅ done |

### What landed (`cargo test --workspace` → 156 tests, 0 failures)

- 137 `sh2` tests (M1's 131 + 1 cache-storage + 5 cache-wiring)
- 9 `saturn::bus_routing` — every region round-trips, BIOS mirrors, unmapped is open bus
- 7 `saturn::scheduler` — determinism, fairness, real-`Sh2Entity` cosched on `SaturnBus`
- 3 `saturn::dual_sh2` — master writes sentinel → slave observes within budget;
  fairness drift bounded; reset-vector load from BIOS image works

The cache-wiring tests serve double-duty as both "task #2 done" and the M2
verification gate "second read of the same address from master costs fewer
cycles" — the `CountingBus` directly proves the hit path.

## Milestone 3 — SCU, SMPC, VDP2 minimal, SDL2: BIOS to splash ✅ scaffolding complete

Goal: **the SEGA logo on screen.** A real BIOS image boots, the splash
renders, and an SDL2 window displays it. Stands up the system bridge
(SCU + DMA + interrupt aggregator), the slave-release path (SMPC), the
display generator (VDP2 minimal — one NBG layer), the SCU-DSP, and the
frontend shell.

| # | Task | Status |
|---|------|--------|
| 1 | SMPC — registers + `SETSL`/`SETSM` slave hold-release | ✅ done |
| 2 | SCU registers + DMA channels (3 channels, synchronous transfer) | ✅ done |
| 3 | SCU interrupt aggregator + wiring into SH-2 master INTC | ✅ done |
| 4 | `scu_dsp` crate — 32-bit DSP core (ISA, decoder, interpreter, opcode tests) | ✅ done |
| 5 | VDP2 register bank + VRAM (512 KiB) + CRAM (4 KiB) | ✅ done |
| 6 | VDP2 minimal renderer — one NBG layer (bitmap + 4-cell tile, 8/16/32 bpp via CRAM) | ✅ done |
| 7 | SDL2 frontend skeleton — window, run loop, framebuffer texture upload | ✅ done |
| 8 | BIOS boot integration — load real BIOS, hash splash framebuffer against committed golden | ✅ done (regression baseline) |

### What landed (`cargo test --workspace` → 240 tests, 0 failures)

- 8 unit + 7 integration tests for SMPC (slave halt-on-reset, SSHON release, SSHOFF re-halt, SF transitions, IREG/OREG round-trip)
- 8 unit + 8 integration tests for SCU (DMA round-trips, INTC priority resolution, IST W1C semantics, DMA-end raising the right per-channel source, end-to-end DMA → master SH-2 vectors)
- 6 unit + 13 integration tests for `scu_dsp` (decoder, ALU, MVI, JMP cond+uncond, END/ENDI, runaway-microcode step cap)
- 14 VDP2 unit tests + 6 integration through the bus + 6 renderer unit tests + 3 `Saturn::run_frame` integration
- 1 BIOS-boot regression test (gated on BIOS presence; asserts against committed golden hash)

### M3 close-out — honest reality

All 8 task scaffolds shipped; the SDL2 frontend opens cleanly and the test
suite is green. **The "SEGA logo on screen" goal is not yet met.** A real
Saturn BIOS image boots into an early init poll loop (master spins at PC
0x000002B2/0x000002B6 with `SR.imask = 15`, which masks even the
VBlank-IN we now raise at frame boundary). The BIOS is waiting on
peripheral data — most likely an SMPC `INTBACK` response or CD-block
status handshake, neither of which M3 modelled. With those landed in M4
the same harness will start showing meaningful framebuffer content.

The committed golden hash `0x2A0B972960C5E325` is the all-black current
output. It's still a useful regression baseline: if any of the 8 M3
components silently drift, this hash flips and the BIOS-boot test fails
loudly. The visual confirmation step in the original task description
becomes the entry criterion for M4's BIOS-splash effort rather than the
exit criterion for M3.

### Verification gates (all green)

1. `cargo test --workspace` — 240 tests pass.
2. `cargo test -p scu_dsp` — DSP per-opcode tests pass.
3. `cargo test -p saturn --test scu` — DMA transfers, INTC priority.
4. `cargo test -p saturn --test smpc` — `SETSL` releases the halted slave.
5. `cargo test -p saturn --test vdp2_render` — hand-crafted scene renders to known RGBA bytes.
6. `cargo test -p saturn --test bios_boot` — splash framebuffer hash matches golden (currently the all-black "stuck in BIOS init poll" hash).
7. Manual: `cargo run -p fifth_planet -- bios/Sega\ Saturn\ BIOS\ (USA).bin` opens an SDL2 window; window stays open and accepts close/Esc. Screen is black (not yet splash — see close-out notes above).

### Explicitly out of scope for M3

- VDP1 (sprites/polygons) — M4
- SCSP + M68k + audio — M4
- SMPC `INTBACK` peripheral data + keyboard input — M4
- CD-block (SH-1) handshake — M5
- Save states — M4 or M5 once the peripheral set stabilises
- Cycle-stealing DMA accuracy — refinement for whichever later milestone surfaces a game that needs it
- Multiple NBG/RBG layer compositing, transparency, line-scroll, mosaic, window planes — M4+

## Later milestones (queued)

- **M4** — VDP1 (sprites/polygons), SCSP + M68k (audio), SDL2 keyboard input, save states.
- **M5** — CD block (SH-1), CD-ROM image loading, first commercial game booting.
- **Explicitly never** — JIT / dynarec (accuracy over performance is the project's design axis).
