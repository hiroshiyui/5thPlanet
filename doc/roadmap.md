# 5thPlanet Roadmap

Accuracy-first SEGA Saturn emulator in Rust. Milestones are scoped tightly so
the foundation is solid before the next chip is added.

The full design rationale lives in `/home/yhh/.claude/plans/temporal-strolling-hollerith.md`.

## Milestone 1 — Cycle-accurate SH-2 (SH7604) core

Single-chip deliverable: a standalone `sh2` library crate validated by unit
tests and ROM regressions, ready to be wired into a bus/scheduler later.

| # | Task | Status |
|---|------|--------|
| 1 | Workspace + `sh2` skeleton + `Bus` trait + `Cpu` struct | ✅ done |
| 2 | Full SH-2 ISA table (~142 ops) + decoder + decoder unit tests | ✅ done |
| 3 | Interpreter — first batch of ~20 core opcodes (MOV/ALU/CMP/branches with delay slots) + integration tests | ✅ done |
| 4 | Remaining ~120 opcodes, group by group, with tests alongside | ✅ done |
| 5 | Pipeline / cycle model: 5-stage scoreboard, load-use stalls, multiply latency, branch costs, interlock timing tests | ✅ done |
| 6 | Cache (4 KiB 4-way LRU) + on-chip peripherals (INTC, DMAC, DIVU, FRT; BSC/WDT/SCI/UBC as register stubs) | pending |
| 7 | Exception + interrupt dispatch (reset, illegal, slot-illegal, address error, NMI, TRAPA, external via INTC) | pending |
| 8 | ROM regression harness + committed golden state hashes | pending |

### Verification gates (all must pass to call M1 done)

1. `cargo test -p sh2` — every opcode unit test green.
2. `cargo test -p sh2 --test vectors` — SingleStepTests-style JSON vector corpus 100%.
3. `cargo test -p sh2 --test pipeline_timing` — cycle counts match SH-2 manual examples exactly.
4. `cargo test -p sh2 --test rom_harness` — golden hashes match.
5. Manual disassembly spot-check vs `objdump -m sh2`.

## Out of scope for M1 (queued for later milestones)

- **M2** — Saturn bus, scheduler, second SH-2 (slave), VDP2 minimal framebuffer, SMPC stub, BIOS boot to splash, SDL2 frontend.
- **M3** — VDP1, SCU + SCU-DSP, save states.
- **M4** — SCSP M68k + SCSP DSP, audio output.
- **M5** — CD block (SH-1), CD-ROM image loading, first commercial game booting.
- **Explicitly never** — JIT / dynarec (accuracy over performance is the project's design axis).
