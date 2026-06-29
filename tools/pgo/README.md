# PGO experiment — SH-2 interpreter

`run_pgo.sh` measures whether **Profile-Guided Optimization** buys any
bit-identical single-core speedup on the interpreter hot path. It is the
roadmap's P4 lever and the one interpreter micro-opt not yet measured-out (the
decode-LUT fixed-array and fat LTO were both measured as noise — see
`doc/roadmap.md` "Interpreter micro-opt investigation").

## Why PGO (and why it's charter-safe)

PGO reorders basic blocks by *measured* execution frequency — the 143-arm
`execute()` match, the `mem_*`/`classify` dispatch chains, the cold panic edges.
Neither thin nor fat LTO can do this without a profile. It is **build-time only
and bit-identical**: no source change, no semantics change, so it stays inside
the accuracy-first/no-JIT charter. The boot golden is a build-soundness sanity
check, not a behavior gate.

## Run it

```bash
tools/pgo/run_pgo.sh            # full A/B: baseline vs PGO, 3 runs/scene
PGO_RUNS=5 tools/pgo/run_pgo.sh # more repetitions for a tighter read
```

Needs the rustup `llvm-tools` component (for `llvm-profdata`) and the bench
assets (`bios/Sega Saturn BIOS v1.01 (JAP).bin`, `roms/vf2_full_lsb.cue`,
`roms/Doukyuusei - if (Japan) (1M, 2M).cue`); the benches print "skipped" and
the script aborts if they're missing.

## Reading the result

A real win is a consistent gap **larger than the baseline's own run-to-run
spread** on the same scene. A delta inside that spread is noise. The host's
`perf` is blocked (`perf_event_paranoid=3`), so this end-to-end fps A/B is the
attribution method — believe the benches, not a structural estimate.

## If PGO wins

It is **not** wired into the normal build (no checked-in `RUSTFLAGS` that every
`cargo build` would pay). Adopting it means a documented release recipe that
bakes a reproducible profile at packaging time. Gate before adopting:

```bash
cargo test -p saturn --test bios_boot   # golden unchanged
cargo test -p saturn --test savestate   # round-trip unchanged
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check
```

## Over-fit note

The training workload reuses the heavy benches. For an interpreter the hot
opcode mix is similar across a whole game so the over-fit is mild, but a
stricter read trains on a **held-out** title — add a third game's
`SAT_INPUT_REC` movie to `TRAIN_SCENES` and keep the measurement on the benches.

## Intermediates

Everything lands under `target/pgo/` (per project convention — never `/tmp`):
`profraw/` (instrumented run output) and `merged.profdata`. Safe to delete.
