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

## Milestone 2 — Saturn bus, dual SH-2, event-driven scheduler 🚧 active

Pairs the M1 SH-2 with a Saturn-shaped memory map, a second SH-2 (slave), and
an event-driven scheduler that decides which CPU advances next. Wires the M1
cache structure into live fetch/data paths so cycle counts on cache-resident
code (which is most Saturn code) start matching hardware. Scope is deliberately
**no graphics, no audio, no BIOS boot, no SDL2** — those wait for M3 once
there's something to render.

| # | Task | Status |
|---|------|--------|
| 1 | Extend `sh2::Cache` with line data storage + write-through update API | pending |
| 2 | Wire cache into `Cpu::mem_read*/mem_write*` (cached vs cache-through dispatch) | pending |
| 3 | Saturn bus + typed region structs + memory-map dispatch | pending |
| 4 | Event-driven `Scheduler` with `SchedEntity` trait | pending |
| 5 | `Saturn` system aggregate + dual SH-2 integration test | pending |

### Verification gates

1. `cargo test -p sh2` — all 131 M1 tests still green (cache wiring must not regress).
2. `cargo test -p saturn --test bus_routing` — every memory region round-trips; BIOS mirroring works; on-chip range stays with the SH-2.
3. `cargo test -p saturn --test cache_wiring` — second read of a BIOS address from master costs fewer cycles than the first (proves the hit path).
4. `cargo test -p saturn --test scheduler` — `Saturn::run_for(N)` produces identical per-CPU `pipeline.cycles` across two runs from the same seed state.
5. `cargo test -p saturn --test dual_sh2` — master writes a sentinel into high work RAM; slave reads it within a bounded cycle budget.

## Later milestones (queued)

- **M3** — VDP1 + VDP2, SCU + SCU-DSP, SMPC stub, BIOS boot to splash, SDL2 frontend (window + input), save states.
- **M4** — SCSP M68k + SCSP-DSP, audio output via SDL2.
- **M5** — CD block (SH-1), CD-ROM image loading, first commercial game booting.
- **Explicitly never** — JIT / dynarec (accuracy over performance is the project's design axis).
