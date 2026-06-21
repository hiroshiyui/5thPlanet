#!/usr/bin/env bash
# dump_game_disc.sh — companion automation for the `dump-game-disc` skill.
#
# Dumps a *legally-owned* SEGA Saturn disc to a 5thPlanet-loadable CUE-BIN
# (optionally a CHD archive), handling the cdrdao MSB-first CD-DA byte-swap
# gotcha. A Saturn disc is multi-track (track 1 = ISO9660 data, tracks 2+ =
# Red Book CD-DA audio), so we always rip raw/all-tracks — a plain .iso would
# drop the audio.
#
# Pipeline:  rip (raw, all tracks) -> toc2cue -> [verify] -> [chd]
#
# The byte-swap fix is applied at *rip time* via cdrdao's byteswap driver flag
# (`--byteswap`); if your drive needs the post-process per-track swap instead,
# see the skill's step 4 (that path is manual on purpose).
#
# Usage:
#   tools/dump_game_disc.sh [options]
#
# Options:
#   -d, --device DEV    optical drive (default: /dev/sr0)
#   -o, --out DIR       output directory (default: tmp)
#   -n, --name NAME     base name for the image (default: game)
#       --byteswap      re-read audio LSB-first via the cdrdao driver flag
#                       (drive-dependent; verify the result by listening)
#       --driver STR    override the cdrdao --driver string used with --byteswap
#                       (default: $CDRDAO_BYTESWAP_DRIVER or generic-mmc-raw:0x20000)
#       --bios PATH     after ripping, smoke-load the image in jupiter (headless)
#                       and report whether the audio still looks byte-swapped
#       --chd           also produce a compressed .chd archive (needs chdman;
#                       NOTE: the emulator can't load .chd yet — archival only)
#   -h, --help          show this help
#
# Per project convention, default output lands in ./tmp (never /tmp).
set -euo pipefail

DEVICE="/dev/sr0"
OUTDIR="tmp"
NAME="game"
DO_BYTESWAP=0
DO_CHD=0
BIOS=""
DRIVER="${CDRDAO_BYTESWAP_DRIVER:-generic-mmc-raw:0x20000}"

die() { echo "error: $*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

usage() { sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//; s/^#$//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        -d|--device) DEVICE="${2:?}"; shift 2;;
        -o|--out)    OUTDIR="${2:?}"; shift 2;;
        -n|--name)   NAME="${2:?}";   shift 2;;
        --byteswap)  DO_BYTESWAP=1;   shift;;
        --driver)    DRIVER="${2:?}"; shift 2;;
        --bios)      BIOS="${2:?}";   shift 2;;
        --chd)       DO_CHD=1;        shift;;
        -h|--help)   usage; exit 0;;
        *) die "unknown argument: $1 (try --help)";;
    esac
done

# --- preflight ------------------------------------------------------------
have cdrdao  || die "cdrdao not found — install it (also provides toc2cue)."
have toc2cue || die "toc2cue not found — it ships with cdrdao."
[ -e "$DEVICE" ] || die "optical device $DEVICE not found (try -d /dev/srN)."
[ "$DO_CHD" -eq 1 ] && ! have chdman && die "--chd requested but chdman (MAME) not found."
[ -n "$BIOS" ] && [ ! -f "$BIOS" ] && die "--bios path does not exist: $BIOS"

mkdir -p "$OUTDIR"
TOC="$OUTDIR/$NAME.toc"
BIN="$OUTDIR/$NAME.bin"
CUE="$OUTDIR/$NAME.cue"
CHD="$OUTDIR/$NAME.chd"

# --- step 1: rip raw, all tracks -----------------------------------------
echo ">> [1/4] ripping $DEVICE -> $BIN (raw, all tracks) ..."
RIP_ARGS=(read-cd --read-raw --datafile "$BIN")
if [ "$DO_BYTESWAP" -eq 1 ]; then
    echo "        (byteswap mode: --driver $DRIVER — drive-dependent, verify by ear)"
    RIP_ARGS+=(--driver "$DRIVER")
fi
RIP_ARGS+=(--device "$DEVICE" "$TOC")
cdrdao "${RIP_ARGS[@]}"

# --- step 2: TOC -> CUE ---------------------------------------------------
echo ">> [2/4] converting $TOC -> $CUE ..."
toc2cue "$TOC" "$CUE"
if grep -qi 'AUDIO' "$CUE"; then
    echo "        audio tracks present: $(grep -ci 'AUDIO' "$CUE")"
else
    echo "        note: no AUDIO tracks in the cue (data-only game, or an incomplete rip)."
fi

# --- step 3: verify (optional) -------------------------------------------
if [ -n "$BIOS" ]; then
    echo ">> [3/4] smoke-loading in jupiter (headless, ~15s) to check byte order ..."
    LOG="$OUTDIR/$NAME.verify.log"
    # The loader prints a warning when the audio tracks look MSB-first
    # (Disc::audio_looks_msb_first). Run briefly and scrape stderr.
    timeout 15 cargo run -q -p jupiter --no-default-features -- "$BIOS" "$CUE" \
        >"$LOG" 2>&1 || true
    if grep -qi 'byte-swapped' "$LOG"; then
        echo "   !! audio tracks look BYTE-SWAPPED (MSB-first) — CD-DA will be noise."
        if [ "$DO_BYTESWAP" -eq 0 ]; then
            echo "      re-run with --byteswap, or apply the post-process per-track"
            echo "      swap from the dump-game-disc skill (step 4)."
        else
            echo "      --byteswap did not correct it for this drive; try a different"
            echo "      --driver value, or the post-process swap (skill step 4)."
        fi
    else
        echo "        no byte-swap warning — audio byte order looks correct."
    fi
else
    echo ">> [3/4] skipping verify (no --bios given)."
fi

# --- step 4: CHD archive (optional) --------------------------------------
if [ "$DO_CHD" -eq 1 ]; then
    echo ">> [4/4] compressing -> $CHD (archival; not loadable by the emulator yet) ..."
    chdman createcd -i "$CUE" -o "$CHD"
else
    echo ">> [4/4] skipping CHD (no --chd given)."
fi

echo
echo "done. load it with:  cargo run -p jupiter -- <BIOS.bin> $CUE"
[ "$DO_CHD" -eq 1 ] && echo "      ($CHD is archival only — extract with 'chdman extractcd' to play)"
