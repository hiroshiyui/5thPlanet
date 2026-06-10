---
name: docs-engineering
description: Audit and update all project documentation to stay in sync with the current development status of the 5thPlanet SEGA Saturn emulator.
---

When performing documentation engineering, always follow these steps:

1. **Survey recent changes** — run `git log --oneline -20` and skim the diffs of recent `feat(...)` commits. This surfaces new opcodes, new chips, new peripherals, and behavioral changes that documentation may not yet reflect. Note which milestone tasks each commit advances; the roadmap should already reflect them but rolling back is cheap and worth verifying.

2. **Audit** all documentation against the current codebase. The review scope must include — without exception:
   - `README.md` — **an average-user-facing document** (user decision, 2026-06-10): project pitch, accuracy-first design axis, build/run instructions, controls, and *summary-level* status only. The **Status** section is the milestone table with short status cells plus a brief "what works today" paragraph — no commit hashes, register names, fix chains, or test counts (those live in `doc/roadmap.md`). The **Acknowledgements** section is a one-line-per-reference summary plus the never-committed/GPL-separation paragraph (legally load-bearing — keep it) — no debugging war stories. When a milestone lands, update the table cell and the "what works today" paragraph in user terms ("Virtua Fighter 2 is fully playable"), and put the technical detail in the roadmap instead. (Create if absent.)
   - `CLAUDE.md` — stack (Rust 2024, no_std + alloc for `sh2`, workspace `unsafe_code = "forbid"`), workspace layout, key gotchas (SH-2 PC base = `instr_pc + 4`, delay slots, big-endian, write-through cache, queue-and-drain pattern, destructure-self for disjoint borrows), test conventions. Must have a Saturn-side architecture section once `crates/saturn/` is non-stub. (Create if absent.)
   - `doc/roadmap.md` — milestone table; each task's status must reflect reality (✅ done / 🚧 in progress / pending). Each milestone's "what's landed so far" / "what landed" block should list the actual test breakdown by file family.
   - `doc/glossary.md` — Saturn-architecture vocabulary (chip names, address ranges, register acronyms, hardware-specific concepts). New chips / acronyms / address ranges introduced in `crates/saturn/` get entries; cross-link with `[term]` references to other glossary entries.
   - `doc/` chapter files as they are added (e.g. `doc/sh2-isa-notes.md`, `doc/cycle-timing.md`, `doc/bus-protocol.md`).
   - Crate-level `lib.rs` doc comments — must provide a real module overview, not a one-line summary. Both `crates/sh2/src/lib.rs` and `crates/saturn/src/lib.rs` (and any future `crates/*/src/lib.rs`) should give a `cargo doc` reader enough to navigate.
   - Per-opcode handler doc comments citing the SH-2 manual section (or equivalent — SH7604 hardware manual for peripherals, SCU manual for SCU-DSP, etc.).

3. **Revise and update** any documentation that is stale, incomplete, or inconsistent with the current code. In particular:
   - When a roadmap task is completed, flip its row to ✅ done. Also extend the per-milestone "what landed" block under the table with the new tests that file added (count + one-line description of each test family).
   - When a new chip or peripheral is started, add a roadmap entry (don't let `crates/saturn/` grow modules that aren't tracked) AND add it to the glossary AND add it to the relevant CLAUDE.md architecture section.
   - When a new workspace crate is added (e.g. `scu_dsp` arriving in M3), update README's "Workspace" section, CLAUDE.md's "Workspace layout" block, and write a real module-overview doc-comment in its `lib.rs`.
   - When the test count changes meaningfully (typically every milestone task), update the "X tests workspace-wide" line at the top of `doc/roadmap.md` (NOT `README.md` — the README is user-facing and carries no test count). Use `cargo test --workspace 2>&1 | grep '^test result' | awk -F'[. ]' '{s+=$5} END {print s}'` to compute it exactly.
   - When SH-2-side memory-routing rules or pipeline-interlock semantics change, update CLAUDE.md's contract bullets for `Cpu::mem_*` / `Cpu::step` so they stay accurate.

4. **Remove completed items** from any TODO list in `doc/`. If a brief summary of completed work is warranted, add it to the relevant `README.md` or roadmap section before deleting the TODO entry.

5. **Commit** documentation changes using the `commit-and-push` skill with scope `doc`, grouped by topic. Don't mix unrelated documentation changes in a single commit. A natural split is:
   - **Per-task roadmap flip**: when a feature commit lands, the immediately-following `docs:` commit just flips the roadmap row to ✅ done (no other doc work). This is the project's established cadence — keep doing it that way.
   - **Periodic doc sync**: README + CLAUDE.md + crate-level lib.rs docs + glossary updates that all reflect "the project's current state". These cluster naturally and ship as one commit because they're all the same topic ("bring docs in line with reality after milestone X").
   - **New reference docs**: a new `doc/*.md` chapter is its own commit (typically grouped with whatever feature commit motivated it, or as its own `docs:` commit when added retroactively).

   Skill invocations that try to bundle "feature lands + roadmap flip" into one commit are wrong — the project's git log is structured so each `feat(...)` is closely followed by a one-line `docs:` companion, and that one-line companion is greppable from `docs:` alone.
