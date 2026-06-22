#!/usr/bin/env bash
# dump_game_disc.sh — companion automation for the `dump-game-disc` skill.
#
# Dumps a *legally-owned* SEGA Saturn disc to a 5thPlanet-loadable CUE-BIN,
# handling the cdrdao MSB-first CD-DA byte-swap gotcha. A Saturn disc is
# multi-track (track 1 = ISO9660 data, tracks 2+ = Red Book CD-DA audio), so we
# always rip raw/all-tracks — a plain .iso would drop the audio.
#
# Pipeline:  rip (raw, all tracks) -> toc2cue -> [verify]
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
#   -n, --name NAME     base name for the image (default: auto — the disc's own
#                       Saturn game title, slug-ified; falls back to "game")
#       --byteswap      re-read audio LSB-first via the cdrdao driver flag
#                       (drive-dependent; verify the result by listening)
#       --driver STR    override the cdrdao --driver string used with --byteswap
#                       (default: $CDRDAO_BYTESWAP_DRIVER or generic-mmc-raw:0x20000)
#       --bios PATH     after ripping, smoke-load the image in jupiter (headless)
#                       and report whether the audio still looks byte-swapped
#   -h, --help          show this help
#
# Per project convention, default output lands in ./tmp (never /tmp).
set -euo pipefail

DEVICE="/dev/sr0"
OUTDIR="tmp"
NAME="game"
NAME_EXPLICIT=0
DO_BYTESWAP=0
BIOS=""
DRIVER="${CDRDAO_BYTESWAP_DRIVER:-generic-mmc-raw:0x20000}"

die() { echo "error: $*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

usage() { sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//; s/^#$//'; }

# Echo the disc's Saturn game title (trimmed), or nothing if it can't be read /
# isn't a Saturn data disc. The IP.BIN header sits at the start of the first
# (data) track's user data: a 16-byte "SEGA SEGASATURN " hardware id, then
# fixed fields, then a 112-byte game title at offset 0x60. We read the drive's
# first 2048-byte cooked sector (Saturn games put the data track first).
detect_disc_title() {
    local dev="$1" magic title
    # Strip NUL/control bytes *inside* the pipe so a blank/audio sector doesn't
    # trip bash's "null byte in command substitution" warning.
    magic=$(dd if="$dev" bs=2048 count=1 status=none 2>/dev/null | head -c 15 | tr -d '\000-\037') || return 0
    [ "$magic" = "SEGA SEGASATURN" ] || return 0
    title=$(dd if="$dev" bs=2048 count=1 status=none 2>/dev/null | tail -c +97 | head -c 112 | tr -d '\000-\037') || return 0
    # Collapse runs of spaces, trim the ends.
    printf '%s' "$title" | sed -e 's/  */ /g' -e 's/^ *//' -e 's/ *$//'
}

# Turn a title into a filesystem-safe slug: spaces -> '_', keep [A-Za-z0-9._-].
slugify() { printf '%s' "$1" | tr ' ' '_' | tr -cd 'A-Za-z0-9._-'; }

while [ $# -gt 0 ]; do
    case "$1" in
        -d|--device) DEVICE="${2:?}"; shift 2;;
        -o|--out)    OUTDIR="${2:?}"; shift 2;;
        -n|--name)   NAME="${2:?}"; NAME_EXPLICIT=1; shift 2;;
        --byteswap)  DO_BYTESWAP=1;   shift;;
        --driver)    DRIVER="${2:?}"; shift 2;;
        --bios)      BIOS="${2:?}";   shift 2;;
        -h|--help)   usage; exit 0;;
        *) die "unknown argument: $1 (try --help)";;
    esac
done

# --- preflight ------------------------------------------------------------
have cdrdao  || die "cdrdao not found — install it (also provides toc2cue)."
have toc2cue || die "toc2cue not found — it ships with cdrdao."
[ -e "$DEVICE" ] || die "optical device $DEVICE not found (try -d /dev/srN)."
[ -n "$BIOS" ] && [ ! -f "$BIOS" ] && die "--bios path does not exist: $BIOS"

mkdir -p "$OUTDIR"

# Name the dump after the disc's own title unless the user forced -n.
if [ "$NAME_EXPLICIT" -eq 0 ]; then
    raw_title=$(detect_disc_title "$DEVICE" || true)
    slug=$(slugify "$raw_title")
    if [ -n "$slug" ]; then
        NAME="$slug"
        echo ">> disc title: '$raw_title' -> image name '$NAME'"
    else
        echo ">> no Saturn disc title read from $DEVICE; using name '$NAME' (override with -n)."
    fi
fi

TOC="$OUTDIR/$NAME.toc"
BIN="$OUTDIR/$NAME.bin"
CUE="$OUTDIR/$NAME.cue"

# --- step 1: rip raw, all tracks -----------------------------------------
echo ">> [1/3] ripping $DEVICE -> $BIN (raw, all tracks) ..."
RIP_ARGS=(read-cd --read-raw --datafile "$BIN")
if [ "$DO_BYTESWAP" -eq 1 ]; then
    echo "        (byteswap mode: --driver $DRIVER — drive-dependent, verify by ear)"
    RIP_ARGS+=(--driver "$DRIVER")
fi
RIP_ARGS+=(--device "$DEVICE" "$TOC")
cdrdao "${RIP_ARGS[@]}"

# --- step 2: TOC -> CUE ---------------------------------------------------
echo ">> [2/3] converting $TOC -> $CUE ..."
toc2cue "$TOC" "$CUE"
# toc2cue copies the datafile *path* (e.g. "tmp/NAME.bin") into the FILE line,
# but the .cue and .bin live in the same directory and a loader joins the FILE
# name with the cue's own dir — so strip any directory prefix to the basename.
sed -i -E 's#^(FILE )"[^"]*/([^"/]+)"#\1"\2"#' "$CUE"
if grep -qi 'AUDIO' "$CUE"; then
    echo "        audio tracks present: $(grep -ci 'AUDIO' "$CUE")"
else
    echo "        note: no AUDIO tracks in the cue (data-only game, or an incomplete rip)."
fi

# --- step 3: verify (optional) -------------------------------------------
if [ -n "$BIOS" ]; then
    echo ">> [3/3] smoke-loading in jupiter (headless, ~15s) to check byte order ..."
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
    echo ">> [3/3] skipping verify (no --bios given)."
fi

echo
echo "done. load it with:  cargo run -p jupiter -- <BIOS.bin> $CUE"
