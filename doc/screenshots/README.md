# Screenshots

Captures from the **5thPlanet** SEGA Saturn emulator, grouped by what they
demonstrate. All are taken from the SDL3 `jupiter` frontend running on the
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

## Sangokushi V (三國志V)

Koei's Three Kingdoms strategy game (serial T-7623G, JP) — playable from its
opening movie through to the in-game strategy screen, on the JP v1.01 BIOS.

| Image | What it shows |
| ----- | ------------- |
| [`sangokushi-fmv.png`](sangokushi-fmv.png) | The opening movie — the 「歴史シミュレーションゲーム」 (history-simulation game) tagline. The first title to drive the emulator's Sega FILM / Cinepak movie player through to gameplay. |
| [`sangokushi-title.png`](sangokushi-title.png) | The 「三國志V」 title logo, © 1985 / 1995 KOEI. |
| [`sangokushi-menu.png`](sangokushi-menu.png) | The main menu — 新しくゲームを始める / データをロードする / 武将データを登録する / サウンドウェアを聞く — over the dragon-tile background. (The blank-menu and missing-button blockers are fixed; see the [`debugging-playbook.md`](../debugging-playbook.md) SAN5 case study.) |
| [`sangokushi-opening.png`](sangokushi-opening.png) | The opening — Sun Jian (孫堅) and his advisor, 「184年 1月 春」. |
| [`sangokushi-strategy.png`](sangokushi-strategy.png) | The in-game strategy screen — the 孫堅軍 command menu (君主 / 家臣 / 移動 / 戦争 / 機能), gold / rice, and the provincial map. |

## Panzer Dragoon Zwei

SEGA's rail-shooter (serial GS-9049, JP) — fully playable on the JP v1.01 BIOS,
from its opening movie through the title and menus into 3D gameplay at native
704×448 hi-res. The second title to drive the emulator's Sega FILM / Cinepak
movie player.

| Image | What it shows |
| ----- | ------------- |
| [`panzer-fmv.png`](panzer-fmv.png) | The opening Cinepak FMV — the rider in the dusk wasteland (letterboxed). |
| [`panzer-title.png`](panzer-title.png) | The title screen — the "PANZER DRAGOON II Zwei" logo over the cracked-stone background with "PRESS START BUTTON", © SEGA ENTERPRISES, LTD. 1995, 1996. |
| [`panzer-game.png`](panzer-game.png) | In-game 3D rail-shooting — the rider on the dragon banking through a ruined canyon, with the lock-on reticle, an enemy gunship, and the health / lock-on gauges. |

## Greatest Nine '98 (グレイテストナイン'98)

SEGA's pro-baseball game (serial GS-9185, JP) — fully playable on the JP v1.01
BIOS, from the title through the menus into 3D gameplay, at native 704×480
double-density interlace. Its menus drive VDP1 in double-interlace (DIE) mode,
which the compositor field-weaves; the per-frame foreground strobe this fixed is
the GN98 case study in the [`debugging-playbook.md`](../debugging-playbook.md).

| Image | What it shows |
| ----- | ------------- |
| [`gn98-title.png`](gn98-title.png) | The title screen — the 「プロ野球 GREATEST NINE 98」 logo over the ballpark, "Press Start Button", © SEGA ENTERPRISES, LTD., 1997, 1998. |
| [`gn98-menu.png`](gn98-menu.png) | The "Game Menu Select" mode menu (オープン戦 / ペナントレース / なりきりモード / ホームラン競争 / チームエディット / オプション). Its interlaced foreground previously strobed every frame — now steady and full-resolution via the VDP1 double-interlace (DIE) field-weave. |
| [`gn98-matchup.png`](gn98-matchup.png) | The pre-game matchup for an exhibition game (オープン戦) — the Seibu Lions vs Yakult Swallows team flags at Meiji Jingu Stadium (明治神宮野球場). The flags exercise the VDP2 8bpp 2-word pattern-name palette-bank fix. |
| [`gn98-game.png`](gn98-game.png) | In-game 3D baseball — the top of the 1st (1回表), the batter (松井, AVG .309) facing the pitcher (石井), with the S-B-O count diamond, the batting reticle, and the pad guide. |

## Wachenröder (ヴァッケンローダー)

SEGA's steampunk tactical RPG (serial GS-9183, JP) — **🚧 work in progress** on
the JP v1.01 BIOS. It runs from its opening movie through the title and story
scenes into the isometric tactical battle, whose rotating floor is an RBG0
rotation layer with additive colour calculation. That floor previously washed the
whole battle near-white until the VDP2 fix that takes the per-dot line-colour
index from the rotation **coefficient table** (KTCTL bit 4) rather than LCTA
(commit `7e2341b`; the Wachenröder case study in the
[`debugging-playbook.md`](../debugging-playbook.md)). Broader gameplay is still
being verified — it is not yet on the fully-playable list (see
[`compatible-game-titles.md`](../compatible-game-titles.md)).

| Image | What it shows |
| ----- | ------------- |
| [`wachenroder-fmv.png`](wachenroder-fmv.png) | The opening Cinepak FMV — a ruined tower block against the dusk sky. |
| [`wachenroder-title.png`](wachenroder-title.png) | The title screen — the riveted-brass "WACHENRÖDER" logo over the steam-machinery background with "Press start button", © SEGA ENTERPRISES, LTD., 1998. |
| [`wachenroder-scene.png`](wachenroder-scene.png) | A story scene — the protagonist Lucian (ルシアン) and a drunkard (酔漢) trading dialogue, character portraits over the live background. |
| [`wachenroder-party-select.png`](wachenroder-party-select.png) | The pre-battle unit-select / status screen — Lucian Tiller (ルシアン・ティラー) "SELECTED" at LV 1, HP 200, with the full stat block (AT / DF / SP / TP, MVP / ATP / SAP / OSP / ACP). |
| [`wachenroder-battle.png`](wachenroder-battle.png) | The isometric tactical battle — Lucian on the grid with the move cursor and the AP (Action Point) gauge. The rotating stone floor is the RBG0 rotation layer whose coefficient-fed line colour the KTCTL fix corrected. |

## Super Robot Wars F (スーパーロボット大戦F)

Banpresto's mecha tactical RPG (serial T-20610G, JP) — **playable**
(user-verified) on the JP v1.01 BIOS, from the title through the scenario-intro
movies and the strategy map into the combat animations; its second half,
*Super Robot Wars F Final* (スーパーロボット大戦F完結編, T-20612G), is playable
on the same fix chain. The bring-up produced three distinct case studies in the
[`debugging-playbook.md`](../debugging-playbook.md):
CD-XA **Form-2** sectors truncated to 2048 bytes (misframed its software
XA-ADPCM stream — the trademark-scene buzz), **RGB888 direct-colour tile**
characters missing from the VDP2 tile path (black scenario movies), and the
**CD drive-timing phase** model (seek / Seek-pause / buffer-full-resume timing,
commit `ce0f7a4`) whose drift tripped a latent race in the game's own CD
streaming driver and livelocked entering combat. See
[`compatible-game-titles.md`](../compatible-game-titles.md).

| Image | What it shows |
| ----- | ------------- |
| [`srwf-title.png`](srwf-title.png) | The title screen — the 「スーパーロボット大戦F」 logo over the starfield with "PRESS START BUTTON". |
| [`srwf-map.png`](srwf-map.png) | The in-scenario strategy map — the player's units (blue) advancing on the enemy force (red) across the overworld terrain, with the move cursor on a selected unit. |
| [`srwf-battle.png`](srwf-battle.png) | An in-combat attack animation — both units' HP/EN gauges, the attacking mech cutting in with its beam sabre against the sky, and the pilot's dialogue box (ジェス：「うおおおおっ！」). Reaching this scene is what the CD drive-timing fix unblocked. |

## Frontend (in-window OSD)

| Image | What it shows |
| ----- | ------------- |
| [`osd-menu.png`](osd-menu.png) | The hand-rolled, software-composited OSD menu (Esc to open) overlaid on a paused VF2 — Resume / Save State / Load State / Reset / Eject Disc / Load Disc… / Settings / Quit. |

## CRT shader (SDL_GPU presenter)

The optional **SDL_GPU / Vulkan presenter** with its built-in single-pass **CRT
post-process** — scanlines + aperture-grille (Trinitron-style) mask + gamma,
flat geometry. It's selectable in `gpu-presenter` builds (`--gpu=on`, then
**Settings → Graphics → Shaders → CRT** in the OSD), off by default.
Presentation-only — the framebuffer stays bit-identical, so accuracy is untouched
([ADR-0019](../adr/0019-gpu-is-presentation-only.md)). The scanline/mask pattern
is easiest to see at full size.

| Image | What it shows |
| ----- | ------------- |
| [`crt-bios-splash.png`](crt-bios-splash.png) | The JP BIOS "SEGA SATURN" splash through the CRT filter — visible scanlines + RGB grille over the brushed-metal wordmark. |
| [`crt-sega-logo.png`](crt-sega-logo.png) | The mandatory SEGA licence logo shown at game boot ("PRODUCED BY or UNDER LICENSE FROM SEGA ENTERPRISES, LTD."), CRT-filtered — the mask is clearest on the solid blue logo and black field. |
| [`crt-doukyuusei-title.png`](crt-doukyuusei-title.png) | *Doukyuusei ~if~* title (同級生 if) with the CRT shader. |
| [`crt-doukyuusei-scene.png`](crt-doukyuusei-scene.png) | *Doukyuusei ~if~* in-game scene (the bedroom + date/time/money HUD) with the CRT shader on a detailed 2D background. |
| [`crt-vf2-title.png`](crt-vf2-title.png) | *Virtua Fighter 2* title with the CRT shader. |
| [`crt-vf2-player-select.png`](crt-vf2-player-select.png) | *Virtua Fighter 2* PLAYER SELECT (Akira's profile + the roster) through the CRT filter. |
| [`crt-vf2-fight.png`](crt-vf2-fight.png) | A *Virtua Fighter 2* 3D match ("READY", Akira vs Lau) with the CRT shader over the textured 3D stage. |

## Trademarks & copyright

These screenshots are reproduced **solely to demonstrate the emulator's
capabilities**, with **no intent to infringe** any copyright or trademark.

- *SEGA*, *SEGA Saturn*, the Saturn logos, *Virtua Fighter 2*, *Panzer
  Dragoon Zwei*, *Greatest Nine '98*, and *Wachenröder* are trademarks and/or
  registered trademarks of **SEGA Corporation**. © SEGA.
- *Doukyuusei ~if~* (同級生 if) is © **NEC InterChannel, Ltd.** / élf.
- *Sangokushi V* (三國志V / *Romance of the Three Kingdoms V*) is © **KOEI Co., Ltd.** (now Koei Tecmo Games).
- *Super Robot Wars F* (スーパーロボット大戦F) is © **Banpresto Co., Ltd.** (now Bandai Namco), with the featured robots and characters © their respective anime rights holders.
- All other game titles, logos, characters, and artwork shown are the property
  of their respective owners.

Each name and image is used here for **identification and illustration only**
(nominative / fair use). No copyrighted game data, BIOS image, or disc content
is distributed with this project — the emulator requires the user to supply
their own legally-obtained dumps (see [`bios/README.md`](../../bios/README.md)
and [`roms/README.md`](../../roms/README.md)). No affiliation with or
endorsement by any rights holder is implied. If you are a rights holder and
have a concern about any image here, please open an issue and it will be
removed.
