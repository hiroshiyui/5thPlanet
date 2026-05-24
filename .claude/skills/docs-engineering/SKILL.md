---
name: docs-engineering
description: Audit and update all project documentation to stay in sync with the current development status of the 5thPlanet SEGA Saturn emulator.
---

When performing documentation engineering, always follow these steps:

1. **Survey recent changes** by running `git log --oneline -20` and skimming the diffs of recent commits. This surfaces new opcodes, new chips, new peripherals, and behavioral changes that documentation may not yet reflect.

2. **Audit** all documentation against the current codebase. The review scope must include — without exception:
   - `README.md` — project pitch, accuracy-first design axis, build/test/run instructions, current milestone status. (Create if absent.)
   - `CLAUDE.md` — stack (Rust 2024, no_std + alloc for `sh2`, workspace `unsafe_code = "forbid"`), workspace layout, key gotchas (SH-2 PC base = `instr_pc + 4`, delay slots, big-endian), test conventions. (Create if absent.)
   - `doc/roadmap.md` — milestone table; each task's status must reflect reality (✅ done / 🚧 in progress / pending).
   - `doc/` chapter files as they are added (e.g. `doc/sh2-isa-notes.md`, `doc/cycle-timing.md`, `doc/bus-protocol.md`).
   - Crate-level `lib.rs` doc comments (module overview) for each `crates/*`.
   - Per-opcode handler doc comments citing the SH-2 manual section.

3. **Revise and update** any documentation that is stale, incomplete, or inconsistent with the current code. In particular:
   - When a roadmap task is completed, flip its row to ✅ done and add a short note of what landed.
   - When a new chip or peripheral is started, add a roadmap entry (don't let `crates/saturn/` grow modules that aren't tracked).
   - When the test count changes meaningfully, update any "X tests passing" line in `README.md`.

4. **Remove completed items** from any TODO list in `doc/`. If a brief summary of completed work is warranted, add it to the relevant `README.md` or roadmap section before deleting the TODO entry.

5. **Commit** documentation changes using the `commit-and-push` skill with scope `doc`, grouped by topic. Do not mix unrelated documentation changes in a single commit.
