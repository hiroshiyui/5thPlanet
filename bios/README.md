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

## Known-good checksums (SHA-512)

To verify a dump, run `sha512sum "<file>"` and compare. These are the
images in use locally (each exactly 524 288 bytes):

| Filename                              | SHA-512                                                                                                                              |
| ------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| `Sega Saturn BIOS v1.00 (JAP).bin`    | `917c44d22dd1e6e85e8ec0427eaf831ed6d36a09e4d6677d8f99e5ee40ac347185b9642b20dfde350e85a2060b4c93381aee78d9ca571c58329d0b85c3ed75d8` |
| `Sega Saturn BIOS v1.01 (JAP).bin`    | `0516cc3deec97a6f2c0d1e08f68eec0331a90ba8697252357ea6b9a38974822a205860d7ff50f1cc345edd4bf7c41aa6ab1067c8cbc5aed3283b55641c1bc446` |
| `Sega Saturn BIOS (USA).bin`          | `bb6bef1a0d710494a50776fb8c14ff7b5e87d2e177ac129846d77c8a5dd3395b71a3b24a057fae3014de5ae50acc056f45c3abd4bf1aa549b44b72eee145c25e` |
| `Sega Saturn BIOS (EUR).bin`          | `bb6bef1a0d710494a50776fb8c14ff7b5e87d2e177ac129846d77c8a5dd3395b71a3b24a057fae3014de5ae50acc056f45c3abd4bf1aa549b44b72eee145c25e` |

The USA and EUR images above share the same hash — the local EUR file is
byte-for-byte identical to the USA dump. (The retail US and European Saturn
BIOSes are in fact the same image, differing only in the console region
switch, so a matching hash is expected rather than a misfiled dump.)

## Why this is gitignored

Even short snippets of the BIOS would be a copyright violation to commit;
the full image absolutely is. `.gitignore` whitelists only this README, so
attempts to `git add bios/*.bin` will be silently skipped — that's by design.
