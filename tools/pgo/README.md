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

## Adoption recipe — `build_release.sh`

PGO is **not** wired into the normal build (no checked-in `RUSTFLAGS` that every
`cargo build` would pay). It's a deliberate **release/packaging step**:
`build_release.sh` bakes a fresh profile and produces the optimized shipping
binary:

```bash
tools/pgo/build_release.sh        # -> target/release/jupiter (PGO)
```

What it does:

1. Builds an **instrumented headless** `jupiter` (`--no-default-features
   -Cprofile-generate`).
2. **Boot-trains** it over representative discs (auto-discovered from `roms/*.cue`,
   or `PGO_TRAIN_DISCS=a.cue:b.cue`), `PGO_FRAMES` (default 3000 ≈ title +
   attract; most games auto-play gameplay in attract, so the 3D/2D interpreter
   hot paths get covered with no scripted input).
3. `llvm-profdata merge` → one profile.
4. Builds the **shipping (SDL) `jupiter`** with `-Cprofile-use`.
5. Runs the accuracy gates (`bios_boot` golden + `savestate`) under
   `profile-use` and reports.

**Headless-train, SDL-ship is sound:** the hot code (`sh2::Cpu::step`, the
`execute()` match, `saturn` bus/cache) compiles identically in both, so the
profile matches by function hash; the SDL frontend glue legitimately isn't in
the profile (`-pgo-warn-missing-function=0` silences the expected misses). This
keeps training reproducible in CI (no window/audio). Cross-binary
generalisation is empirically validated (the held-out result above).

**No assets? It falls back** to a plain `cargo build --release -p jupiter` with a
warning, so packaging never breaks — you just don't get PGO.

**Knobs:** `PGO_BIOS`, `PGO_TRAIN_DISCS` (colon-separated), `PGO_FRAMES`.
**Extending training** (gameplay-targeted / byte-stable): instrument as above,
then drive the instrumented binary with the matching disc **and** a save-state
(`SAT_LOADSTATE=<snap> SAT_FRAMES=<n> jupiter <bios> <disc>`) or a recorded input
movie, before the merge.

Final gate before shipping a PGO build (the script runs the first two):

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
