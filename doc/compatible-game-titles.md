# Compatible game titles

Commercial SEGA Saturn titles brought up on **5thPlanet**, with their current
playability. This is the "what runs" list; boot-blocker investigations
(currently all resolved) live in
[`boot-blocker-investigations.md`](boot-blocker-investigations.md), the milestone view
is in [`roadmap.md`](roadmap.md), and captures are in
[`screenshots/`](screenshots/README.md).

Everything boots on the **real-BIOS LLE path** — the emulator runs the retail
BIOS and the game's own code; there is no HLE boot. You supply your own
legally-dumped BIOS + disc images (see [`bios/README.md`](../bios/README.md) and
[`roms/README.md`](../roms/README.md)); nothing copyrighted ships with the
project.

| Title | Serial | Region / BIOS | Status | Render golden |
| ----- | ------ | ------------- | ------ | ------------- |
| **Virtua Fighter 2** | GS-9079 | JP / v1.01 | ✅ **Fully playable** — title, mode & character select with looping CD-DA BGM, full 3D fights to the K.O. screen at a steady 60 fps; balanced BGM/SFX. | 53440 px |
| **Doukyuusei ~if~** (同級生 if) | — | JP / v1.01 | ✅ **Fully playable** — graphics, SFX, and voices; in-game record-select menu; native 640×224 hi-res; Shuttle Mouse supported. | 143341 px |
| **Sangokushi V** (三國志V) | T-7623G | JP / v1.01 | ✅ **Fully playable** — intro FMV → title → main menu → in-game strategy screen, with the per-scenario opening introduction movie now crossing the former intermittent stall. | 71680 px |
| **Panzer Dragoon Zwei** | GS-9049 | JP / v1.01 | ✅ **Fully playable** — opening Cinepak FMV → title → main menu (NEW GAME / OPTIONS) → game, with controller input working at native 704×448 hi-res. | 274464 px |

The **render-golden** column is the headless non-black pixel count asserted by
the `#[ignore]`d render-regression tests in `crates/saturn/tests/trace_boot.rs`
(`vf2_renders_non_black`, `doukyuusei_renders_non_black`, `pdz_renders_non_black`,
`san5_renders_non_black`) — a guard that these titles keep rendering. All four
fully-playable titles now have a render golden; Sangokushi V's is captured during
its opening Cinepak FILM movie (the only no-input stable frame), guarding the
software-decoded-movie → VDP2 path.

## Notes

- **Virtua Fighter 2** was the project's original boot target (M11; tag
  `vf2-good-emulation`). The carrying fix-chain — CD seek/Play form, VDP1 8bpp +
  DIE interlace, an SH-2 PC-relative delay-slot bug, the SH-2→SCSP B-bus
  wait-states, and CD-DA routed through the SCSP EXTS inputs — is in
  [`roadmap.md`](roadmap.md).
- **Doukyuusei ~if~** was the first title to reach a proper in-game state. Two
  emulator bugs gated its record-select menu: an SCU indirect-DMA descriptor
  read through an unfolded SH-2 cache-through alias, and VDP1 framebuffer dots
  not being horizontally doubled in the 640/704-dot modes.
- **Sangokushi V** is the newest, and the first title to drive the **Sega FILM /
  Cinepak** movie player through to gameplay. Its bring-up required SH-2
  cache/bus fidelity fixes for the startup FMV and menus, plus an SCU
  interrupt-timing fix for the former intermittent per-scenario movie stall:
  DMA-end interrupts must not be forwarded while the master SH-2 is executing a
  delay slot. Full chain in the commit messages, the
  [`debugging-playbook.md`](debugging-playbook.md) SAN5 case study, and the
  closed SAN5 entry in [`boot-blocker-investigations.md`](boot-blocker-investigations.md).
- **Panzer Dragoon Zwei** is the second Sega FILM / Cinepak title, and the first
  to expose two distinct gaps that the other titles never exercised: a CD **Seek
  (0x11)** command decoded on the MAME track/FAD model instead of Mednafen's
  single-value `COMMAND_SEEK` (a post-FMV track-form seek left the head FAD stale
  and skipped the timed seek, so the BIOS disc-validity check bailed to the CD
  player), and a **peripheral-only SMPC INTBACK** (`IREG0 & 0xF == 0`) that must
  skip the status phase and return the pad directly in OREG0 with no CONTINUE —
  without it the game read "no controller" and ignored all input while other
  titles, which drive the status+continue handshake, worked. Both are in the
  [`debugging-playbook.md`](debugging-playbook.md) case studies.

## Also runs

- The retail **BIOS** boots to the SEGA splash on both the USA and JP v1.01
  images, pixel-matching MAME (M4), and its built-in **audio-CD player**
  application plays music discs (M10).

## Under investigation

No titles are currently under active investigation. Resolved boot-blocker case
studies (e.g. *Sangokushi V*, *Panzer Dragoon Zwei*) are kept as resume points /
methodology references in
[`boot-blocker-investigations.md`](boot-blocker-investigations.md).

---

Game titles and logos are trademarks of their respective owners and are named
here for identification only; see [`screenshots/README.md`](screenshots/README.md)
for the full notice.
