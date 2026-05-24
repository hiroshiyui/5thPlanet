# SEGA Saturn BIOS images

This directory holds SEGA Saturn BIOS dumps used by the emulator at runtime.
**Nothing in this directory other than this README is tracked in git** — the
BIOS is copyrighted by SEGA and redistribution is not permitted. Each
developer must supply their own legally-obtained dump.

## Expected filenames

When the frontend wires up BIOS loading (M2), it will probe `bios/` for
these names (matching the common preservation-community naming):

| Filename                              | Region | Notes                       |
| ------------------------------------- | ------ | --------------------------- |
| `Sega Saturn BIOS v1.00 (JAP).bin`    | Japan  | 512 KiB, earliest revision  |
| `Sega Saturn BIOS v1.01 (JAP).bin`    | Japan  | 512 KiB, common revision    |
| `Sega Saturn BIOS (USA).bin`          | USA    | 512 KiB                     |
| `Sega Saturn BIOS (EUR).bin`          | Europe | 512 KiB                     |

A valid Saturn BIOS image is exactly 524 288 bytes (512 KiB) and its
SH-2 reset vector at offset `0x00000000` points into BIOS-internal code.

## Why this is gitignored

Even short snippets of the BIOS would be a copyright violation to commit;
the full image absolutely is. `.gitignore` whitelists only this README, so
attempts to `git add bios/*.bin` will be silently skipped — that's by design.
