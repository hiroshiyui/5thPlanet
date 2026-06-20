# Screenshots

Captures from the **5thPlanet** SEGA Saturn emulator, grouped by what they
demonstrate. All are taken from the SDL2 `jupiter` frontend running on the
real-BIOS LLE path (see [`doc/roadmap.md`](../roadmap.md) and the project
[`README`](../../README.md)).

## BIOS & boot

| Image | What it shows |
| ----- | ------------- |
| [`bios-splash-usa.png`](bios-splash-usa.png) | The USA BIOS boot splash — the blue planet-and-ring logo with "TM & © 1995 SEGA". The M4 milestone (BIOS boots to the SEGA splash, pixel-matching MAME). |
| [`bios-splash-jp.png`](bios-splash-jp.png) | The Japanese BIOS (v1.01) boot splash — the brushed-metal "SEGA SATURN" wordmark, "Ver. 1.01". |
| [`bios-cd-player.png`](bios-cd-player.png) | The BIOS's built-in **audio-CD player** application playing a music disc (TRACKS / TIME readout, transport controls). Exercises the HLE CD-block + CD-DA → SCSP path (M10). |

## Doukyuusei ~if~ (同級生 if)

The first commercial game brought to a proper title; fully playable
(graphics, SFX, and voices), at native 640×224 hi-res.

| Image | What it shows |
| ----- | ------------- |
| [`doukyuusei-title.png`](doukyuusei-title.png) | The title screen — 「同級生 if」 with "PRESS START BUTTON", © 1996 NEC InterChannel. |
| [`doukyuusei-town.png`](doukyuusei-town.png) | In-game overworld: the player on the town map with the date / time / money HUD along the bottom. |
| [`doukyuusei-scene.png`](doukyuusei-scene.png) | An event scene with a character portrait over the background and the dialogue text box. |

## Virtua Fighter 2

The original boot target — fully playable at a steady 60 fps with correct
graphics, looping CD-DA BGM, and SFX (tag `vf2-good-emulation`).

| Image | What it shows |
| ----- | ------------- |
| [`vf2-am2-logo.png`](vf2-am2-logo.png) | The AM2 (AM R&D Dept. #2) developer logo shown on boot. |
| [`vf2-title.png`](vf2-title.png) | The title screen with the blinking "PRESS START BUTTON". |
| [`vf2-attract-akira.png`](vf2-attract-akira.png) | An attract-mode character introduction (letterboxed) — "AKIRA YUKI". |
| [`vf2-player-select.png`](vf2-player-select.png) | The PLAYER SELECT screen with the character roster and Akira's profile card. |
| [`vf2-fight.png`](vf2-fight.png) | An in-match 3D fight — Akira (left) in Round 1, with the textured stage, health bars, and timer. |

## Frontend (in-window OSD)

| Image | What it shows |
| ----- | ------------- |
| [`osd-menu.png`](osd-menu.png) | The hand-rolled, software-composited OSD menu (Esc to open) overlaid on a paused VF2 — Resume / Save State / Load State / Reset / Eject Disc / Load Disc… / Settings / Quit. |
