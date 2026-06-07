---
name: code-review
description: Perform a project-wide code review of the 5thPlanet SEGA Saturn emulator, covering correctness, emulation accuracy, code quality, tests, documentation, and style.
---

When performing a project-wide code review, always follow these steps:

1. **Survey recent changes** — Run `git log --oneline -20` and skim the corresponding diffs to understand the scope of work before examining individual files.

2. **Security & safety** — Apply the `security-audit` skill. Particular attention:
   - The workspace lint `unsafe_code = "forbid"` (in root `Cargo.toml`) must remain in place. Any new `#![allow(unsafe_code)]` requires justification.
   - Bus-trait implementations are the trust boundary with host memory — confirm address arithmetic uses `wrapping_*` or explicit bounds, never raw indexing that could panic on hostile addresses.

3. **Emulation accuracy** — The project's design axis is accuracy over performance. Review for:
   - *SH-2 ISA fidelity:* Every new opcode handler must cite the *SH-1/SH-2 Software Manual* section it implements, and its semantics (registers touched, SR.T effect, exception conditions) must match the manual exactly.
   - *Cycle counts:* Returned cycle totals must match Appendix A of the software manual (or `SH7604 Hardware Manual` §3 for pipeline interlocks). Hand-wavy "approximately N" cycles is a regression risk.
   - *Delay slots:* Confirm that branch instructions never execute their slot before the slot fetch, and that `Op::is_illegal_in_slot()` is consulted for new branch/jump/SR-modifying ops.
   - *PC-relative addressing:* SH-2 uses `PC_of_instr + 4` as the base, not the running `regs.pc`. Verify new PC-relative ops use the `instr_pc` argument plumbed through `execute()`.
   - *Endianness:* SH-2 is big-endian. All multi-byte bus accesses must use `from_be_bytes`/`to_be_bytes`.

4. **Correctness and logic** — Review for:
   - `unwrap()` / `expect()` in non-test code where a meaningful error or exception could be raised instead.
   - Integer overflow in cycle accumulation; `Pipeline::cycles` uses `saturating_add`, new accumulators should too.
   - Sign-extension correctness for 8-/12-bit immediate and displacement fields.

5. **Code smells** — Flag:
   - Duplicated decode/dispatch patterns that should be extracted into a helper or table entry.
   - Functions exceeding ~60 lines without clear justification (large opcode-dispatch matches are an acceptable exception).
   - Magic numbers — SH-2 register offsets, exception vector numbers, and FFFFFE00 on-chip base should be named constants.
   - Dead code or stale commented-out blocks.

6. **Test coverage** — Verify:
   - Each new opcode has at least one integration test under `crates/sh2/tests/` (file per opcode family).
   - Each new cycle-count claim has a `pipeline_timing.rs` assertion (once task #5 lands).
   - Decoder changes have a `crates/sh2/src/decoder.rs::tests` case.
   - Tests construct CPUs via the `MemBus` fixture (`sh2::harness`); ad-hoc bus mocks duplicate it without need.

   **Measure coverage with `cargo llvm-cov`** — don't eyeball it. The tool is `cargo-llvm-cov` (cargo subcommand) plus the `llvm-tools-preview` rustup component; if `cargo llvm-cov --version` fails, install with `cargo install cargo-llvm-cov` and `rustup component add llvm-tools-preview`.
   - **Collect once** (instrumented build + full suite, includes the ~30s `bios_boot` golden — give it a few minutes):
     `cargo llvm-cov --workspace --summary-only`
   - **Re-report from the cached profile data without re-running** (fast; use this for every follow-up view):
     `cargo llvm-cov report --summary-only` · `cargo llvm-cov report --show-missing-lines` · `cargo llvm-cov report --html` (writes `target/llvm-cov/html/index.html`).
   - **Confirm the diff is covered, not just the totals:** cross-check `--show-missing-lines` against the lines this change added/modified — *every* new or changed production line should be absent from the uncovered set. A green workspace total hides an untested new branch.
   - **When a finding is "extract a helper" (a code smell), prefer the version that is independently unit-testable** — extracting inline logic into a named `fn` and unit-testing it is a net coverage win even if the original call site stays uncovered. Note this trade-off in the finding.
   - **Read the totals in context:** the interactive `jupiter` frontend (`main.rs`, the SDL2 loop) and debug-only modules (`sh2/src/debug.rs`) are ~0% by nature and drag the workspace number down; judge the *core* emulation crates (`sh2`, `saturn`, `vdp2`, `scu_dsp`) on their own, where 80–100% is the expectation. Don't demand coverage of the interactive frontend.
   - **In the report (step 9), state the coverage delta** for the touched files and call out any new code that landed uncovered as a **Tests** finding with a concrete test to add.

7. **Documentation quality** — Confirm:
   - Public items in `sh2` carry `///` doc comments naming the SH-2 manual reference where relevant.
   - `doc/roadmap.md` reflects the current milestone status (mark tasks ✅ done as soon as they land).
   - Non-obvious cycle-count or interlock decisions carry an inline comment citing the manual section that justifies them.

8. **Code style** — Confirm:
   - **Formatting: do NOT run `cargo fmt --all`.** The workspace is *not* globally rustfmt-clean and never has been — it uses a deliberate compact hand-style (e.g. `crates/sh2/src/isa.rs` keeps ~142 enum variants each on a single line). Under the pinned toolchain (edition 2024) `cargo fmt --all` reformats ~60 files (`isa.rs` alone explodes by +790 lines), and `style_edition = "2021"` is *worse*. There is no `rustfmt.toml` and no CI fmt gate, so any "fmt-clean" expectation is aspirational. **Format only the lines a change adds/modifies, by hand, matching the surrounding code.** Flag a hunk only when its style clearly diverges from its neighbors; never recommend a workspace-wide reformat as part of a review (it's a separate, deliberate decision needing explicit sign-off). No `#[rustfmt::skip]` without justification.
   - `cargo clippy --workspace --all-targets -- -D warnings` is clean (this *is* enforceable and should pass). Any `#[allow(clippy::...)]` suppression must have a comment explaining why the lint is a false positive here.
   - No `println!` / `eprintln!` in the `sh2` library crate (it's `no_std` + `alloc` only).

9. **Report findings** — Present all identified issues grouped by category: Safety, Accuracy, Correctness, Code Smell, Tests, Documentation, Style. Assign each a severity of **Critical**, **High**, **Medium**, or **Low**. For every finding, include the file path and line number, a clear description, and a concrete recommendation for how to fix it.
