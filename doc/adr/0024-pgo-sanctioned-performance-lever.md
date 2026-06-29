# 0024. PGO is the one sanctioned performance lever

- **Status:** Accepted
- **Date:** 2026-06-29

## Context

[ADR-0002](0002-accuracy-over-performance.md) forbids the usual emulator speed
levers — no JIT, no dynarec, no approximate-cycle shortcut — because accuracy is
the charter and the trace-diff-vs-Mednafen methodology requires the interpreter
to behave exactly as the hardware does. That leaves a real single-core cost: the
SH-2 core is serial and fidelity-locked, the read-only render edge is already
offloaded to a worker thread ([ADR-0013](0013-render-pipeline-worker-thread.md)),
and threading tops out around two cores — so the only remaining headroom is the
emulation thread's own per-instruction cost, which we are not allowed to make
faster by changing what it computes.

A 4-agent fan-out (2026-06-29) measured the candidate levers by interleaved A/B
benches (`bench_fps` / `bench_vf2_fight`; `perf` is blocked at
`perf_event_paranoid=3`, so benches are the attribution method):

- **Per-instruction source micro-opts** — the unanimous top pick, a decode-LUT
  fixed array (`Box<[Op]>` → `Box<[Op;65536]>`), plus hand-tuned dispatch — and
  **fat LTO** both measured as **noise**: the branch predictor makes the LUT
  bounds check effectively free, and thin LTO already captures the cross-crate
  inlining.
- **Profile-Guided Optimization** measured as the **big** win: **+31 %** on the
  VF2 fight, **+56 %** on the Doukyuusei menu trained-on, and **+39 %** held-out
  (trained on VF2 only, measured on Doukyuusei → it generalises across games).
  PGO reorders the interpreter's basic-block layout; it does **not** change
  behaviour — the `bios_boot` golden (`0x0B1BA6E5180766F7`) and the save-state
  round-trip both pass under `-Cprofile-use`. It is therefore **bit-identical**
  and fully honours ADR-0002 (no behaviour, no cycle change).

## Decision

We will treat **PGO as the one sanctioned way to make the core faster**, and keep
it a **build-time-only, bit-identical packaging step** — never wired into the
normal build:

- **No checked-in `RUSTFLAGS` / `-Cprofile-use`** in the workspace. A from-source
  `cargo build` produces the plain, un-profiled binary, so a developer's build and
  the trace-diff baseline stay un-perturbed.
- `tools/pgo/run_pgo.sh` measures the A/B; `tools/pgo/build_release.sh` produces
  the optimized release binary (instrument a headless `jupiter` → boot + attract-
  train over `roms/*.cue` → merge the profile → build the shipping SDL binary with
  `-Cprofile-use` → run the gates; it falls back to a plain build if the training
  assets are absent).
- **The golden + save-state gates must pass under `profile-use`.** A PGO build that
  moves a golden is *rejected*, not re-baselined — PGO is only ever a layout change.
- **The dead ends are closed:** do not re-chase per-instruction source micro-opts
  or fat LTO (measured noise). The single-core headroom lives in PGO's block
  layout, not in source.

## Consequences

- **Easier:** a ~30–56 % core speed-up that generalises across games, with zero
  accuracy risk and zero ongoing source churn.
- **Cost accepted:** the speed-up lands only in the *packaged release*, not in a
  developer's `cargo build` / `cargo test` (which stay plain — correct, since PGO
  must not perturb the trace-diff reference). The recipe needs `roms/*.cue` to
  train, and a toolchain that drops `-Cprofile-use` support would lose the lever.
- **Honours ADR-0002:** PGO is codegen-only — it never introduces a JIT/dynarec or
  an approximate cycle; the interpreter's behaviour and cycle accounting are
  byte-for-byte unchanged, which the unchanged golden proves on every PGO build.

## Alternatives considered

- **Per-instruction source micro-opts** (decode-LUT fixed array, hand-tuned
  dispatch). Rejected: measured as noise; not worth the core churn or the review
  cost on fidelity-locked hot code.
- **Fat LTO.** Rejected: measured-neutral — thin LTO already captures the
  cross-crate inlining PGO then re-orders.
- **Wire `-Cprofile-use` into the default build** via checked-in `RUSTFLAGS` + a
  committed `.profdata`. Rejected: it would make `cargo build` depend on a stale
  checked-in profile and silently diverge the dev binary's codegen from the
  trace-diff reference. PGO belongs at packaging time, not in the default build.
- **A JIT / dynarec** — the usual big emulator lever. Forbidden by ADR-0002; it is
  the very reason PGO is the *only* sanctioned lever.
