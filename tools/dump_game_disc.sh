#!/usr/bin/env bash
# dump_game_disc.sh — companion automation for the `dump-game-disc` skill.
#
# Dumps a *legally-owned* SEGA Saturn disc to a 5thPlanet-loadable CUE-BIN using
# redumper (https://github.com/superg/redumper) — the redump.org-grade dumper.
# A Saturn disc is multi-track (track 1 = ISO9660 data, tracks 2+ = Red Book
# CD-DA audio); redumper rips every track raw, applies the drive's read-offset
# correction, and writes the audio in correct (LSB-first) byte order. That means
# the old cdrdao MSB-first byte-swap gotcha no longer applies — there is no
# --byteswap path to get wrong.
#
# redumper emits a redump-style split: one `<name> (Track N).bin` per track plus
# a multi-FILE `<name>.cue`. Our loader concatenates multi-FILE cues into one
# image (`Disc::from_cue`), so the result loads directly.
#
# Pipeline:  redumper disc (dump -> refine -> split + hash) -> [verify]
#
# Usage:
#   tools/dump_game_disc.sh [options]
#
# Options:
#   -d, --device DEV    optical drive (default: /dev/sr0)
#   -o, --out DIR       output directory (default: tmp)
#   -n, --name NAME     base name for the image (default: auto — the disc's own
#                       Saturn game title, slug-ified; falls back to "game")
#       --retries N     sector re-read retries on a SCSI/C2 error (default: 50)
#       --speed N       drive read speed (default: redumper picks the optimal)
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
RETRIES=50
SPEED=""
BIOS=""

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
        -d|--device)  DEVICE="${2:?}"; shift 2;;
        -o|--out)     OUTDIR="${2:?}"; shift 2;;
        -n|--name)    NAME="${2:?}"; NAME_EXPLICIT=1; shift 2;;
        --retries)    RETRIES="${2:?}"; shift 2;;
        --speed)      SPEED="${2:?}"; shift 2;;
        --bios)       BIOS="${2:?}"; shift 2;;
        -h|--help)    usage; exit 0;;
        *) die "unknown argument: $1 (try --help)";;
    esac
done

# --- preflight ------------------------------------------------------------
have redumper || die "redumper not found — install it (https://github.com/superg/redumper)."
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

CUE="$OUTDIR/$NAME.cue"
LOG="$OUTDIR/$NAME.log"

# --- step 1: redumper disc (dump + refine + split + hash) -----------------
echo ">> [1/2] redumper: dumping $DEVICE -> $OUTDIR/$NAME.* (raw, all tracks) ..."
RD_ARGS=(disc --drive="$DEVICE" --retries="$RETRIES"
         --image-path="$OUTDIR" --image-name="$NAME" --overwrite)
[ -n "$SPEED" ] && RD_ARGS+=(--speed="$SPEED")
redumper "${RD_ARGS[@]}"

[ -f "$CUE" ] || die "redumper finished but no cue at $CUE — check $LOG."

if grep -qi 'AUDIO' "$CUE"; then
    echo "        audio tracks present: $(grep -ci 'AUDIO' "$CUE")"
else
    echo "        note: no AUDIO tracks in the cue (data-only game, or an incomplete rip)."
fi

# Surface redumper's own error tally from the log (errors/SCSI/C2 lines).
if [ -f "$LOG" ]; then
    echo "        redumper log summary:"
    grep -iE 'errors|SCSI|C2|redump match|read offset|disc write offset' "$LOG" \
        | sed 's/^/          /' | tail -20 || true
fi

# --- step 2: verify (optional) -------------------------------------------
if [ -n "$BIOS" ]; then
    echo ">> [2/2] smoke-loading in jupiter (headless, ~15s) to check byte order ..."
    VLOG="$OUTDIR/$NAME.verify.log"
    # The loader prints a warning when the audio tracks look MSB-first
    # (Disc::audio_looks_msb_first). Run briefly and scrape stderr.
    timeout 15 cargo run -q -p jupiter --no-default-features -- "$BIOS" "$CUE" \
        >"$VLOG" 2>&1 || true
    if grep -qi 'byte-swapped' "$VLOG"; then
        echo "   !! audio tracks look BYTE-SWAPPED (MSB-first) — CD-DA will be noise."
        echo "      Unexpected with redumper; inspect $VLOG and the redumper log."
    else
        echo "        no byte-swap warning — audio byte order looks correct."
    fi
else
    echo ">> [2/2] skipping verify (no --bios given)."
fi

echo
echo "done. load it with:  cargo run -p jupiter -- <BIOS.bin> $CUE"
