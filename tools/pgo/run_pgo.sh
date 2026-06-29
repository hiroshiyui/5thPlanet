#!/usr/bin/env bash
# run_pgo.sh — Profile-Guided Optimization (PGO) experiment for the SH-2
# interpreter hot path (roadmap lever P4 / P2 "step dispatch").
#
# WHY: with `perf` blocked (kernel.perf_event_paranoid=3 on the dev host) and
# the per-instruction *source* micro-opts measured as noise (decode-LUT
# fixed-array, fat LTO — see doc/roadmap.md "Interpreter micro-opt
# investigation"), PGO is the one untested lever with plausibly non-noise
# magnitude. It does frequency-based block layout over the 143-arm `execute()`
# match and the branchy `mem_*`/`classify` chains — the kind of win neither thin
# nor fat LTO can do without a profile. PGO is **build-time only and
# bit-identical** (no source/semantics change), so it stays inside the
# accuracy-first charter.
#
# METHOD (manual PGO, no cargo-pgo dependency):
#   1. baseline   — build normally (thin LTO), measure the heavy scenes.
#   2. generate   — rebuild with `-Cprofile-generate`, run a representative
#                   workload so the instrumented binary emits .profraw.
#   3. merge      — llvm-profdata merge the .profraw into one .profdata.
#   4. use        — rebuild with `-Cprofile-use`, measure the same scenes.
#   5. report     — A/B table + the accuracy gates to run before adopting.
#
# OVER-FIT CAVEAT: the training workload below is the same family of heavy
# scenes used for measurement (VF2 fight + Doukyuusei menu). For an interpreter
# the hot-opcode mix is similar across a whole game, so the over-fit is mild,
# but a stricter read trains on a HELD-OUT title — drop a third game's
# `SAT_INPUT_REC` movie into the training set and measure on the benches.
#
# ADOPTING THE RESULT (if PGO wins): PGO is NOT wired into the normal build —
# this script is an experiment. To ship it you'd add a documented release
# recipe (a `tools/pgo/` profile baked at release time), NOT a checked-in
# RUSTFLAGS that every `cargo build` pays for. Keep the profile reproducible.
#
# Per project convention all intermediates land under ./target (never /tmp).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

RUNS="${PGO_RUNS:-3}"                       # measurement repetitions per scene
PROFRAW_DIR="$ROOT/target/pgo/profraw"      # instrumented run output
PROFDATA="$ROOT/target/pgo/merged.profdata" # merged profile
SYSROOT="$(rustc --print sysroot)"
LLVM_PROFDATA="$(find "$SYSROOT" -name 'llvm-profdata' 2>/dev/null | head -1)"

[ -n "$LLVM_PROFDATA" ] || {
    echo "error: llvm-profdata not found — install the rustup component:" >&2
    echo "       rustup component add llvm-tools" >&2
    exit 1
}

# Measurement scenes (compute-only / pipelined fps). Training reuses these plus
# the boot path that builds the snapshots on first run.
MEASURE_SCENES=(bench_vf2_fight bench_fps bench_vf2_pipeline)
TRAIN_SCENES=(bench_vf2_fight bench_fps bench_vf2_pipeline)

# Build the (ignored) bench binary with the given RUSTFLAGS; echo its path.
build_bench() {
    local flags="$1"
    RUSTFLAGS="$flags" cargo test --release -p saturn --test trace_boot --no-run 2>&1 \
        | grep -oE 'target/release/deps/trace_boot-[a-f0-9]+' | tail -1
}

# Run one bench, echo the fps figure(s) of interest (compute-only + pipelined).
run_scene() {
    local bin="$1" scene="$2"
    "$bin" "$scene" --ignored --nocapture --exact "$scene" 2>&1 \
        | grep -E 'compute-only|pipelined' \
        | grep -oE '[0-9]+\.[0-9]+ fps' | grep -oE '[0-9]+\.[0-9]+'
}

# Measure every scene RUNS times; print "scene: v1 v2 v3 | avg".
measure() {
    local bin="$1" label="$2"
    echo "----- $label -----"
    for scene in "${MEASURE_SCENES[@]}"; do
        local vals=()
        for _ in $(seq 1 "$RUNS"); do
            # bench_vf2_fight/bench_fps emit one compute-only line; pipeline emits
            # one pipelined line. Take the first fps figure each run.
            vals+=("$(run_scene "$bin" "$scene" | head -1)")
        done
        printf '%-22s: %s | avg %s\n' "$scene" "${vals[*]}" \
            "$(printf '%s\n' "${vals[@]}" | awk '{s+=$1; n++} END{printf "%.1f", s/n}')"
    done
}

echo "############################################################"
echo "## PGO experiment — SH-2 interpreter   ($(rustc --version))"
echo "## runs/scene=$RUNS  llvm-profdata=$LLVM_PROFDATA"
echo "############################################################"

# --- 1. baseline (thin LTO, no PGO) --------------------------------------
echo ">> [1/5] baseline build (no PGO) ..."
BASE_BIN="$(build_bench "")"
echo "   bin: $BASE_BIN"
measure "$BASE_BIN" "BASELINE (no PGO)"

# --- 2. generate (instrumented) ------------------------------------------
echo ">> [2/5] instrumented build (-Cprofile-generate) ..."
rm -rf "$PROFRAW_DIR"; mkdir -p "$PROFRAW_DIR"
GEN_BIN="$(build_bench "-Cprofile-generate=$PROFRAW_DIR")"
echo "   bin: $GEN_BIN"
echo ">> running training workload (emits .profraw) ..."
for scene in "${TRAIN_SCENES[@]}"; do
    echo "   train: $scene"
    "$GEN_BIN" "$scene" --ignored --nocapture --exact "$scene" >/dev/null 2>&1 || true
done
n_raw=$(find "$PROFRAW_DIR" -name '*.profraw' | wc -l)
echo "   .profraw files: $n_raw"
[ "$n_raw" -gt 0 ] || { echo "error: no .profraw produced — did the benches skip (missing BIOS/disc)?" >&2; exit 1; }

# --- 3. merge ------------------------------------------------------------
echo ">> [3/5] merge profiles -> $PROFDATA ..."
mkdir -p "$(dirname "$PROFDATA")"
"$LLVM_PROFDATA" merge -o "$PROFDATA" "$PROFRAW_DIR"/*.profraw
ls -lh "$PROFDATA" | awk '{print "   profdata: "$5}'

# --- 4. use (optimized) --------------------------------------------------
echo ">> [4/5] optimized build (-Cprofile-use) ..."
# -Cprofile-use warns on functions missing from the profile (cold/dep code);
# that's expected and harmless — silence it to keep the log readable.
PGO_BIN="$(build_bench "-Cprofile-use=$PROFDATA -Cllvm-args=-pgo-warn-missing-function=0")"
echo "   bin: $PGO_BIN"
measure "$PGO_BIN" "PGO (profile-use)"

# --- 5. report -----------------------------------------------------------
echo ">> [5/5] done."
cat <<EOF

Interpret the two tables above (BASELINE vs PGO). A real win is a consistent
gap LARGER than the baseline's own run-to-run spread on the SAME scene; a delta
inside that spread is noise (the verdict for decode-LUT-fixed-array and fat LTO).

If PGO shows a reliable gain, GATE it before adopting:
  cargo test -p saturn --test bios_boot     # golden 0x0B1BA6E5180766F7 unchanged
  cargo test -p saturn --test savestate     # round-trip unchanged
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt --all -- --check
PGO is build-time only, so it CANNOT change emulation output — the golden is a
sanity check that the build is sound, not that behavior shifted.

Re-run with a HELD-OUT training title for a stricter (non-over-fit) read.
EOF
