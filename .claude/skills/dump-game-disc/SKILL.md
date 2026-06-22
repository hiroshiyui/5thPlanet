---
name: dump-game-disc
description: Dump a physical SEGA Saturn game disc to a 5thPlanet-loadable CUE-BIN image, using redumper (preferred) or cdrdao (handling its MSB-first CD-DA byte-swap gotcha).
---

Dump a **legally-owned** SEGA Saturn disc to an image this emulator can load.
A Saturn disc is multi-track: track 1 is the ISO9660 data track, tracks 2+ are
Red Book **CD-DA audio** (BGM). A plain `.iso` captures only track 1 and loses
the audio — so the target is always a full multi-track CUE-BIN image. Confirm
the user owns the disc before starting; never help circumvent disc
copy-protection.

**Prefer `redumper`** — it is the redump.org-grade dumper: it rips every track
raw, applies the drive's read-offset correction, writes CD-DA in the correct
(LSB-first) byte order, and emits a redump-style multi-`FILE` cue. That makes
the cdrdao MSB-first byte-swap gotcha (below) **disappear entirely**. Use
cdrdao only as a fallback when redumper is unavailable.

**Automation:** `tools/dump_game_disc.sh` runs the whole pipeline in one command
(`redumper disc` → verify), e.g.
`tools/dump_game_disc.sh --bios bios/saturn_bios.bin` (auto-names the image from
the disc's own Saturn title; `-n NAME` overrides, `--retries N`/`--speed N` tune
the rip). Run `tools/dump_game_disc.sh --help` for options. Use it for the happy
path; fall back to the manual steps when a stage needs hands-on attention.

Do all intermediate I/O under the project's `tmp/` subdirectory (never `/tmp`).
**Always pass real paths, never `<placeholder>`** in commands you give the user
— a literal `>`/`<` in a pasted command can truncate a file to 0 bytes (a real
past incident; see the `no-placeholder-in-runnable-commands` memory).

## Preferred path — redumper

1. **Find the drive and confirm tooling.** `ls /dev/sr*` (usually `/dev/sr0`);
   confirm `redumper --version`.

2. **Dump + split in one command.** The `disc` aggregate dumps, refines (re-reads
   bad sectors), splits into per-track BINs, and writes the cue + a log + hashes:
   ```bash
   redumper disc --drive=/dev/sr0 --retries=50 \
       --image-path=tmp --image-name=game --overwrite
   ```
   This is slow (it reads the whole disc, retrying errors) — let it finish.
   Watch the running `errors: { SCSIs, C2s, Q }` tally and the final log: a clean
   dump ends with zero SCSI/C2 errors. Output is redump-style: `tmp/game.cue`
   plus one `tmp/game (Track N).bin` per track. Our loader concatenates
   multi-`FILE` cues into one image (`Disc::from_cue`), so it loads directly.

3. **Note the offset caveat.** redumper warns if the drive isn't in its offset
   database (`drive read offset not found … using generic drive`, read offset
   `+0`). The dump still plays correctly, but the audio isn't shifted to the
   redump standard, so it won't be a submission-grade hash match — fine for
   emulation. A drive that *is* in the database gives a fully offset-corrected,
   redump-matchable dump.

Then verify (step 5 below) and place the result (step 6).

## Fallback path — cdrdao

Use only when redumper isn't available. cdrdao on many drives reads audio
**byte-swapped (MSB-first)**, which makes CD-DA play as *noise* here.

1. **Rip raw, all tracks** (raw 2352-byte sectors keep the CD-DA):
   ```bash
   cdrdao read-cd --read-raw --datafile tmp/game.bin --device /dev/sr0 tmp/game.toc
   ```
   Slow; surface read errors rather than ignoring them.

2. **Convert TOC → CUE** (the emulator loads CUE-BIN, not cdrdao's `.toc`):
   ```bash
   toc2cue tmp/game.toc tmp/game.cue
   ```
   A multi-track game lists one `TRACK 01 MODE1/2352` (data) then `TRACK NN
   AUDIO` entries. A single-track-only cue means a data-only game or an
   incomplete rip — flag it.

3. **Fix the MSB-first byte-swap (the load-bearing gotcha).** The loader detects
   it — `Disc::audio_looks_msb_first()` in `crates/saturn/src/disc.rs` *warns* at
   load but does not auto-correct. If flagged, fix the **image** by one of:
   - **Re-rip with cdrdao's byteswap driver flag** — `--driver
     generic-mmc-raw:<flags>` (check `man cdrdao` "driver options" for the
     byteswap bit value), then re-verify. Drive-dependent, so trial-and-verify.
   - **Post-process swap** — split per track (`bchunk -r tmp/game.bin
     tmp/game.cue tmp/track`), byte-swap each **audio** track file pairwise
     (swap adjacent byte pairs), then rebuild a multi-`FILE` cue. Swap **only the
     audio-track files**, never the data track — swapping data corrupts the
     ISO9660 filesystem.

## Verify and finish (both paths)

5. **Verify by loading it.** Boot the image and confirm it authenticates and
   CD-DA plays cleanly (not noise). The disc image is a **positional** argument
   after the BIOS (`jupiter <BIOS.bin> [game.cue|.iso|.ccd]`), not a flag:
   ```bash
   cargo run -p jupiter -- bios/saturn_bios.bin tmp/game.cue   # windowed: boot + listen
   ```
   Use the user's real BIOS path. Watch stderr for the `audio tracks appear
   byte-swapped` warning while it loads — with redumper it should never fire;
   with cdrdao, if it does, return to the cdrdao byte-swap step. The acceptance
   bar is: boots to the game, and audio-track BGM sounds correct.

6. **Place the result and clean up.** Move the verified `.cue` + `.bin`(s) to the
   user's chosen library path; leave nothing stray in `tmp/`. Point the frontend
   at the `.cue`. The emulator reads `.cue`/`.iso`/`.ccd` only — **CHD support was
   dropped** (commit `302a43d`), so convert any existing `.chd` back to CUE-BIN
   with `chdman extractcd`.

Notes:
- This skill is **observer-only on the codebase** — it dumps and fixes disc
  images at the file level and does not modify emulator source. Any byte-swap is
  corrected in the *image*, not the loader.
