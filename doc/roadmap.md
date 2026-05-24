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

## Later milestones (queued)

- **M3** — VDP1 + VDP2, SCU + SCU-DSP, SMPC stub, BIOS boot to splash, SDL2 frontend (window + input), save states.
- **M4** — SCSP M68k + SCSP-DSP, audio output via SDL2.
- **M5** — CD block (SH-1), CD-ROM image loading, first commercial game booting.
- **Explicitly never** — JIT / dynarec (accuracy over performance is the project's design axis).
