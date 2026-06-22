# SEGA Saturn disc images

This directory holds SEGA Saturn CD-ROM disc images (games, the boot/audio
discs) that the emulator reads at runtime. **Nothing in this directory other
than this README is tracked in git** — Saturn software is copyrighted and
redistribution is not permitted. Each developer must supply their own
legally-obtained dumps. `.gitignore` whitelists only this README, so
`git add roms/*.bin` (etc.) is silently skipped — that's by design.

The directory name `roms/` is conventional; Saturn titles are CD-ROM images,
not cartridge ROMs.

## Supported formats

The loader picks a parser by file extension (`jupiter/src/main.rs`
`load_image_disc`; core parsers in `crates/saturn/src/disc.rs`):

| Layout                 | You pass   | Also needs                              |
| ---------------------- | ---------- | --------------------------------------- |
| CUE sheet + BIN track(s) | `.cue`   | the `.bin` file(s) the cue `FILE`-references, alongside it |
| CloneCD                | `.ccd`     | a sibling `.img` of the same basename (`.sub` is optional/ignored) |
| Raw data track         | `.iso`     | nothing — a single 2048-byte/sector MODE1 track, no CD-DA |

Notes:

- **CUE/BIN is the preferred format** for real games — it carries the full
  track layout (data + CD-DA audio tracks), which the read pump and CDDA→SCSP
  path need. A bare `.iso` has no audio tracks, so CD-DA BGM won't play.
- BIN tracks must be **2352-byte raw sectors** (the cue `MODE1/2352` form),
  matching how the read pump and authentication expect to see sector data.

## Dumping your own discs to .cue + .bin

To play a game you own, dump its disc to a CUE/BIN set yourself. A Saturn disc
is a single session with a data track plus (usually) several CD-DA audio
tracks, so the dumper must capture **all** tracks at **raw 2352-byte sectors**.
Use a plain CD/DVD drive (PC SATA/USB optical drives work; many can read
Saturn discs even without special "audio-extraction" support).

Recommended tools, best first:

- **redumper** (Windows/Linux/macOS) — a modern, cross-platform redump-style
  dumper, and what `tools/dump_game_disc.sh` drives. Its `disc` command dumps,
  re-reads bad sectors, splits into per-track `.bin`s, and writes the `.cue` +
  a log + hashes, all with correct **LSB-first** audio and per-drive read-offset
  correction:

  ```
  redumper disc --drive=/dev/sr0 --retries=50 \
      --image-path=roms --image-name=game --overwrite
  ```

  Output is redump-style: `game.cue` plus one `game (Track N).bin` per track
  (the loader concatenates multi-`FILE` cues into one image). If redumper warns
  that your drive isn't in its offset database (`read offset: +0, using generic
  drive`), the dump still plays fine but won't be a submission-grade hash match.

- **DiscImageCreator** (Windows) — the [redump.org](http://redump.org)
  preservation standard. Produces `.bin` + `.cue` (plus `.sub`, `.ccd`, log
  files) with correct **LSB-first** audio. Roughly:

  ```
  DiscImageCreator.exe cd <drive-letter> game.bin 8
  ```

  (`8` = read speed; lower is safer on scratched discs.) You can then verify
  your dump's hashes against the redump.org database.

- **cdrdao** (Linux/cross-platform) — widely available, but ⚠️ it writes audio
  tracks **MSB-first (byte-swapped)**, which this emulator plays as noise (see
  the gotcha below). Use it only when redumper isn't available, and byte-swap
  the audio afterwards:

  ```
  cdrdao read-cd --read-raw --driver generic-mmc-raw \
      --device /dev/sr0 --datafile game.bin game.toc
  toc2cue game.toc game.cue
  ```

Whatever the tool, drop the resulting `game.cue` + `game.bin`(s) (keep them in
the same directory — the cue `FILE`-references the bins by name) into `roms/`.

## Loading a disc

```bash
# CLI: BIOS first, then the disc image
cargo run -p jupiter -- "bios/Sega Saturn BIOS (USA).bin" "roms/game.cue"

# A live optical drive instead of an image (physdisc feature, ADR-0009):
cargo run -p jupiter --features physdisc/libcdio -- <bios> cdrom:/dev/sr0
```

Or at runtime via the in-window OSD: **Esc → Load Disc…**, browse to a
`.cue`/`.iso`/`.ccd`, and it loads + power-cycles to boot the game.
(**F8** plays a disc's first audio track live, for CD-DA testing.)

## Gotcha: byte-swapped audio tracks

If a CUE/BIN was produced by a tool that writes audio **MSB-first** (e.g. a
cdrdao dump), the CD-DA tracks are byte-swapped relative to what the Saturn
expects and will play back as **noise**. The loader detects this and prints a
warning (`Disc::audio_looks_msb_first`); regenerate the image with LSB-first
audio, or byte-swap the audio region of the BIN.
