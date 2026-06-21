---
name: dump-game-disc
description: Dump a physical SEGA Saturn game disc to a 5thPlanet-loadable image (CUE-BIN, optionally CHD), handling the cdrdao MSB-first CD-DA byte-swap gotcha.
---

Dump a **legally-owned** SEGA Saturn disc to an image this emulator can load.
A Saturn disc is multi-track: track 1 is the ISO9660 data track, tracks 2+ are
Red Book **CD-DA audio** (BGM). A plain `.iso` captures only track 1 and loses
the audio — so the target is always a full multi-track image (CUE-BIN, or CHD
as a compressed archive). The pipeline is **rip → fix byte order → verify →
(optionally) compress**. Confirm the user owns the disc before starting; never
help circumvent disc copy-protection.

**Automation:** `tools/dump_game_disc.sh` runs the rip → cue → verify →
(optional) CHD pipeline below in one command (e.g.
`tools/dump_game_disc.sh -n mygame --bios bios/saturn_bios.bin`, add
`--byteswap` if the audio comes out MSB-first). Use it for the happy path;
fall back to the manual steps when a stage needs hands-on attention (a flaky
rip, or the post-process per-track swap in step 4). Run `tools/dump_game_disc.sh
--help` for options.

Always follow these steps:

1. **Find the optical drive and confirm tooling.** Run `lsblk -o NAME,TYPE | grep rom` or check `ls /dev/sr*` to locate the drive (usually `/dev/sr0`). Confirm `cdrdao` is installed (`cdrdao --version`); note that `toc2cue` ships with it. If the user also has `chdman` (from MAME) it enables the optional CHD step; if not, skip step 6. Do all intermediate I/O under the project's `tmp/` subdirectory (never `/tmp`).

2. **Rip the disc raw, all tracks.** Read raw 2352-byte sectors so the CD-DA audio tracks come along:
   ```bash
   cdrdao read-cd --read-raw --datafile tmp/game.bin --device /dev/sr0 tmp/game.toc
   ```
   This is slow and may retry bad sectors — let it finish. A dirty/scratched disc can take many minutes; surface read errors rather than ignoring them. **Always pass real paths, never `<placeholder>`** in commands you give the user — a literal `>`/`<` in a pasted command can truncate a file to 0 bytes (a real past incident; see the `no-placeholder-in-runnable-commands` memory).

3. **Convert TOC → CUE.** The emulator loads CUE-BIN, not cdrdao's `.toc`:
   ```bash
   toc2cue tmp/game.toc tmp/game.cue
   ```
   Skim `tmp/game.cue`: a multi-track game should list one `TRACK 01 MODE1/2352` (data) followed by `TRACK NN AUDIO` entries. A single-track-only cue means either a data-only game or an incomplete rip — flag it.

4. **Handle the cdrdao MSB-first CD-DA byte-swap (the load-bearing gotcha).** cdrdao on many drives reads audio **byte-swapped (MSB-first)**, which makes CD-DA play as *noise* in this emulator. The loader detects this — `Disc::audio_looks_msb_first()` in `crates/saturn/src/disc.rs` prints a warning at load (it only *warns*; it does not auto-correct). Do a dry-run load to check (see step 5). If the audio is flagged byte-swapped, fix the **image** by **one** of:
   - **Preferred — re-rip with cdrdao's byteswap driver flag** — pass `--driver generic-mmc-raw:<flags>` (the byteswap bit; check `man cdrdao` "driver options" for your version's value), then re-verify. The swap is drive-dependent, so this is trial-and-verify, but it produces a clean LSB-first image with no extra steps.
   - **Post-process swap** — split per track (`bchunk -r tmp/game.bin tmp/game.cue tmp/track`), byte-swap each **audio** track file pairwise (`xxd`/a short script swapping adjacent byte pairs), then rebuild a multi-FILE cue. Swap **only the audio-track files**, never the data track — swapping data corrupts the ISO9660 filesystem. Use this if the driver flag doesn't take.

5. **Verify by loading it.** Boot the image headlessly or in the frontend and confirm it authenticates and CD-DA plays cleanly (not noise):
   The disc image is a **positional** argument after the BIOS (`jupiter <BIOS.bin> [game.cue|.iso|.ccd]`), not a flag:
   ```bash
   cargo run -p jupiter -- bios/saturn_bios.bin tmp/game.cue   # windowed: boot + listen to the BGM
   ```
   Use the user's real BIOS path. Watch stderr for the byte-swap warning while it loads.
   Watch stderr for the `audio tracks appear byte-swapped` warning. If it fires, return to step 4. The acceptance bar is: boots to the game, and audio-track BGM sounds correct.

6. **(Optional) Compress to CHD for archival.** Only if the user wants a compact single-file archive *and* `chdman` is available (note: the emulator does not yet read `.chd` — that's roadmap task G1 — so this is storage-only, not a loadable format today):
   ```bash
   chdman createcd -i tmp/game.cue -o tmp/game.chd        # archive
   chdman extractcd -i tmp/game.chd -o out.cue -ob out.bin # to get CUE-BIN back later
   ```
   Make clear to the user that until G1 lands they must extract back to CUE-BIN to actually play it.

7. **Place the result and clean up.** Move the verified `.cue` + `.bin` (and `.chd` if made) to the user's chosen library path; leave nothing stray in `tmp/`. Remind the user which file to point the frontend at (the `.cue`).

Notes:
- This skill is **observer-only on the codebase** — it dumps and fixes disc images at the file level and does not modify emulator source. The byte-swap is always corrected in the *image* (step 4), not in the loader.
- Prefer `redumper` over `cdrdao` when available (it avoids the byte-swap entirely), but this skill assumes a cdrdao-only system per the project's tooling.
