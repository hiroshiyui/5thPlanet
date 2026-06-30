# Compatible game titles

Commercial SEGA Saturn titles brought up on **5thPlanet**, with their current
playability. This is the "what runs" list; the boot-blocker case studies +
forensic case files live in the
[`debugging-playbook.md`](debugging-playbook.md), the milestone view
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
| **Greatest Nine '98** (グレイテストナイン'98) | GS-9185 | JP / v1.01 | ✅ **Fully playable** — boots to title, game-menu, and team-select; the interlaced foreground (titles, menu items, team-flag previews) renders steady and full-resolution at native 704×480 via the VDP1 DIE field-weave. | 146336 px |

The **render-golden** column is the headless non-black pixel count asserted by
the `#[ignore]`d render-regression tests in `crates/saturn/tests/trace_boot.rs`
(`vf2_renders_non_black`, `doukyuusei_renders_non_black`, `pdz_renders_non_black`,
`san5_renders_non_black`, and `gn98_boots_to_title`) — a guard that these titles
keep rendering. All five fully-playable titles now have a render golden;
Sangokushi V's is captured during its opening Cinepak FILM movie (the only
no-input stable frame), guarding the software-decoded-movie → VDP2 path.

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
  delay slot. Full chain in the commit messages and the
  [`debugging-playbook.md`](debugging-playbook.md) SAN5 case study (with the full
  forensic record under "Boot-blocker case files").
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
- **Greatest Nine '98** needed a four-fix chain, each a distinct fidelity gap:
  a VDP1 draw-end-flag (`EDSR.CEF`) timing fix to clear its "Now Loading" stall;
  dropping a spurious SMPC `SF` busy on a COMREG write (a black-screen self-loop);
  an 8bpp 2-word pattern-name palette-bank decode fix (scrambled team-flag
  previews); and the **VDP1 double-interlace (DIE) field-weave** — the game
  rasterizes its even/odd interlace fields into the two VDP1 framebuffers on
  alternating frames, which we were line-doubling one field at a time (a per-frame
  field strobe of the whole foreground) instead of weaving into a full-height
  image. All four are in the [`debugging-playbook.md`](debugging-playbook.md) case
  studies.

## Also runs

- The retail **BIOS** boots to the SEGA splash on both the USA and JP v1.01
  images, pixel-matching MAME (M4), and its built-in **audio-CD player**
  application plays music discs (M10).

## Under investigation

- **Wachenröder** (ヴァッケンローダー) — Sega, serial **GS-9183**, JP / v1.01.
  🚧 **Work in progress.** Boots to its 3D battle scene, which now renders
  correctly after a VDP2 fidelity fix: the RBG0 rotating-floor layer's per-dot
  line-colour index is taken from the rotation **coefficient-table** (KTCTL
  bit 4), not the LCTA table — without it the floor's additive colour-calc
  washed the whole scene near-white (commit `7e2341b`; case study in
  [`debugging-playbook.md`](debugging-playbook.md)). Broader gameplay is being
  verified; not yet promoted to the fully-playable list above.

Resolved boot-blocker case studies (e.g. *Sangokushi V*, *Panzer Dragoon Zwei*)
are kept as methodology references in the
[`debugging-playbook.md`](debugging-playbook.md) ("Case studies" +
"Boot-blocker case files").

---

Game titles and logos are trademarks of their respective owners and are named
here for identification only; see [`screenshots/README.md`](screenshots/README.md)
for the full notice.
