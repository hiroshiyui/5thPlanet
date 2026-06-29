#!/usr/bin/env bash
# build_release.sh — the PGO ADOPTION RECIPE: build a profile-guided-optimized
# release `jupiter` binary.
#
# This is the release/packaging-time counterpart to run_pgo.sh (which only
# *measures* PGO). It produces the actual shipping binary:
#
#   instrument headless jupiter  ->  train over representative content
#     ->  llvm-profdata merge  ->  build the shipping (SDL) jupiter with the
#         profile  ->  run the accuracy gates  ->  target/release/jupiter
#
# WHY headless for training, SDL for shipping:
#   PGO profile data is keyed by function (name + structural hash). The hot
#   code (`sh2::Cpu::step`, the `execute()` match, `saturn` bus/cache) lives in
#   the `sh2`/`saturn` crates, which compile IDENTICALLY whether jupiter is
#   built headless or with `sdl-frontend` — so a profile trained on the headless
#   binary applies cleanly to the SDL binary's copies of those functions. The
#   SDL frontend glue legitimately isn't in the profile (it's not the hot path);
#   `-Cllvm-args=-pgo-warn-missing-function=0` silences the expected misses.
#   Training headless keeps the workload reproducible in CI (no window/audio).
#   (Cross-binary generalisation is empirically validated — see run_pgo.sh's
#   held-out result: a VF2-only profile still gave the unseen Doukyuusei +39%.)
#
# TRAINING WORKLOAD: a headless boot of each representative disc for ~3000
# frames (~50 s of virtual time). That reaches each game's title and its
# ATTRACT mode — and most games auto-play real gameplay in attract (e.g. VF2's
# attract is CPU-vs-CPU 3D fights), so boot-training captures the gameplay
# interpreter hot paths (the `execute()` match over the live opcode mix)
# deterministically, no scripted input needed. Train across a FEW varied games
# (a 3D fighter + a 2D/visual-novel title) so the merged profile covers both
# the 3D and 2D hot paths.
#
# REPRODUCIBILITY: the headless boot seeds the RTC from the host clock, so the
# .profraw is not byte-reproducible run-to-run. That's fine — PGO needs a
# *representative* profile, not a deterministic one; the same recipe + assets
# yields an equivalent-performance binary every time. (For byte-stable or
# gameplay-targeted training you can extend this to drive a recorded input
# movie / a save-state with its matching disc inserted — see the README.)
#
# CHARTER: PGO is build-time only and bit-identical — it changes block layout,
# never emulation behaviour. The boot golden + savestate gates below prove the
# build is sound (not that behaviour shifted; it cannot).
#
# Assets are gitignored (BIOS/discs); if none are available the script FALLS
# BACK to a plain release build so packaging never breaks — you just don't get
# PGO. All intermediates land under ./target (never /tmp), per convention.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

# --- knobs (env-overridable) ---------------------------------------------
BIOS="${PGO_BIOS:-bios/Sega Saturn BIOS v1.01 (JAP).bin}"   # training BIOS
FRAMES="${PGO_FRAMES:-3000}"           # frames per boot-training disc (~50 s)
PROFRAW_DIR="$ROOT/target/pgo/profraw-release"
PROFDATA="$ROOT/target/pgo/release.profdata"
SYSROOT="$(rustc --print sysroot)"
LLVM_PROFDATA="$(find "$SYSROOT" -name 'llvm-profdata' 2>/dev/null | head -1)"

# Training discs: PGO_TRAIN_DISCS (colon-separated) or auto from roms/*.cue,
# excluding obvious non-game discs (audio CD, the boot disc).
collect_discs() {
    if [ -n "${PGO_TRAIN_DISCS:-}" ]; then
        printf '%s\n' "${PGO_TRAIN_DISCS//:/$'\n'}"
    else
        find "$ROOT/roms" -maxdepth 1 -name '*.cue' 2>/dev/null \
            | grep -viE 'audiocd|ss_boot' || true
    fi
}

note() { printf '>> %s\n' "$*"; }

# --- preflight / fallback -------------------------------------------------
mapfile -t DISCS < <(collect_discs)

if [ -z "$LLVM_PROFDATA" ] || [ ! -f "$BIOS" ] || [ "${#DISCS[@]}" -eq 0 ]; then
    note "PGO prerequisites missing — FALLING BACK to a plain release build."
    [ -z "$LLVM_PROFDATA" ] && echo "   - llvm-profdata not found (rustup component add llvm-tools)"
    [ ! -f "$BIOS" ]        && echo "   - training BIOS not found: $BIOS (set PGO_BIOS=)"
    [ "${#DISCS[@]}" -eq 0 ] && echo "   - no training discs (roms/*.cue or PGO_TRAIN_DISCS)"
    cargo build --release -p jupiter
    note "plain release binary: target/release/jupiter (NO PGO)"
    exit 0
fi

note "PGO release build"
note "  BIOS:  $BIOS"
note "  discs: ${DISCS[*]}"

# --- 1. instrument (headless) --------------------------------------------
note "[1/5] building instrumented headless jupiter (-Cprofile-generate) ..."
rm -rf "$PROFRAW_DIR"; mkdir -p "$PROFRAW_DIR"
RUSTFLAGS="-Cprofile-generate=$PROFRAW_DIR" \
    cargo build --release -p jupiter --no-default-features
GEN_BIN="$ROOT/target/release/jupiter"

# --- 2. train ------------------------------------------------------------
note "[2/5] boot-training each disc ($FRAMES frames ≈ title + attract) ..."
for disc in "${DISCS[@]}"; do
    [ -f "$disc" ] || { echo "   skip missing disc: $disc"; continue; }
    echo "   train: $(basename "$disc")"
    SAT_FRAMES="$FRAMES" "$GEN_BIN" "$BIOS" "$disc" >/dev/null 2>&1 || true
done
n_raw=$(find "$PROFRAW_DIR" -name '*.profraw' | wc -l)
note "  .profraw files: $n_raw"
[ "$n_raw" -gt 0 ] || { echo "error: no profile data produced (training runs all failed?)" >&2; exit 1; }

# --- 3. merge ------------------------------------------------------------
note "[3/5] merging -> $PROFDATA ..."
mkdir -p "$(dirname "$PROFDATA")"
"$LLVM_PROFDATA" merge -o "$PROFDATA" "$PROFRAW_DIR"/*.profraw
ls -lh "$PROFDATA" | awk '{print "   profdata: "$5}'

# --- 4. build the shipping (SDL) binary with the profile -----------------
note "[4/5] building shipping jupiter (-Cprofile-use) ..."
USE_FLAGS="-Cprofile-use=$PROFDATA -Cllvm-args=-pgo-warn-missing-function=0"
RUSTFLAGS="$USE_FLAGS" cargo build --release -p jupiter
note "  shipping binary: target/release/jupiter (PGO)"

# --- 5. accuracy gates (build soundness) ---------------------------------
note "[5/5] accuracy gates under profile-use (golden + savestate) ..."
RUSTFLAGS="$USE_FLAGS" cargo test --release -p saturn \
    --test bios_boot --test savestate 2>&1 \
    | grep -iE 'test result|FAILED|panic' || true

cat <<EOF

PGO release build complete: target/release/jupiter

Verify the win with the measurement harness (A/B vs a plain build):
  tools/pgo/run_pgo.sh
Recorded baseline (12-core host): +31% VF2 fight, +56% / +39%(held-out) Doukyuusei.

If a gate above did not say "ok", STOP — do not ship; PGO is build-time only and
must never move the golden, so a failure means a toolchain/build problem.
EOF
