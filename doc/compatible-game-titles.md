# Compatible game titles

Commercial SEGA Saturn titles brought up on **5thPlanet**, with their current
playability. This is the "what runs" list; titles still under active
investigation live in
[`wip-compatibility-titles.md`](wip-compatibility-titles.md), the milestone view
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
| **Sangokushi V** (三國志V) | T-7623G | JP / v1.01 | ✅ **Playable** — intro FMV → title → main menu → opening → in-game strategy screen. | — |

The **render-golden** column is the headless non-black pixel count asserted by
the `#[ignore]`d render-regression tests in `crates/saturn/tests/trace_boot.rs`
(`vf2_renders_non_black`, `doukyuusei_renders_non_black`) — a guard that these
titles keep rendering. Sangokushi V has no render golden yet.

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
  Cinepak** movie player through to gameplay. Three SH-2 cache/bus fidelity
  fixes carried it: the SCU-DMA-from-CD-FIFO halfword skip (FMV), the reset
  cache-purge (`35ce7e8`, blank menu), and the 16-bit `MOV.W @CCR` cache-purge
  (`6215aab`, blank menu buttons). Full chain in
  [`wip-compatibility-titles.md`](wip-compatibility-titles.md).

## Also runs

- The retail **BIOS** boots to the SEGA splash on both the USA and JP v1.01
  images, pixel-matching MAME (M4), and its built-in **audio-CD player**
  application plays music discs (M10).

## Under investigation

See [`wip-compatibility-titles.md`](wip-compatibility-titles.md) — e.g. *Panzer
Dragoon Zwei* (paused: enters the FILM movie player after the license screens,
then returns to the BIOS).

---

Game titles and logos are trademarks of their respective owners and are named
here for identification only; see [`screenshots/README.md`](screenshots/README.md)
for the full notice.
