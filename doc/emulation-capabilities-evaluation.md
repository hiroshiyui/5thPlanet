# Emulation Capabilities — 5thPlanet vs Mednafen (Saturn)

A capability comparison of 5thPlanet's Saturn emulation against **Mednafen**, the
project's reference oracle. Scope: what Mednafen's SS core does that we still
lack, plus a deep-dive on the **VDP2 renderer** (the area chosen for detailed
scoping).

The reference sources live locally under `mednaref/src/ss/` (never committed).
This document cites them by `file:line` so claims are checkable.

**Status date:** 2026-06-08 (milestone M11 active).

---

## 0. What kind of accuracy we are

5thPlanet is **accuracy-first**: a cycle-accurate, manual-sourced, *instruction-
stepped* interpreter (no JIT/dynarec — an explicit non-goal) with event-driven
peripheral timing, validated against Mednafen by **master-SH-2 PC-trace diff**.

Two deliberate departures from "cycle-accurate everywhere":

- **The SH-1 CD-block is HLE, not LLE** — its firmware is undumped (on-die mask
  ROM) and half its job is an analog servo with no digital ground truth, so
  there is nothing to be cycle-accurate *against*. Every Saturn emulator HLEs it.
- **Some VDP rendering is per-scanline / per-frame, not per-dot.** (Mednafen is
  *also* per-scanline here — see §2.7 — so this is not a Mednafen gap.)

---

## 1. System-level gaps (Mednafen has it, we don't)

Ordered by practical impact on "run more games."

### 1.1 The headline: game compatibility

Mednafen runs essentially the **entire commercial library**. We boot
**Doukyuusei ~if~ to its title screen**; **VF2 stalls in its intro** on a polled
CD-state divergence. Everything below contributes to closing that gap.

### 1.2 Input devices (largest concrete gap)

We support **only the digital pad on port 1**; the full INTBACK peripheral
handshake is a single-pad placeholder (`crates/saturn/src/smpc.rs`).

| Device | Mednafen | Ours |
|---|---|---|
| Digital pad | ✅ both ports | ✅ port 1 only |
| 3D / Analog Pad | ✅ | ❌ |
| Mission Stick / Dual Mission Stick | ✅ | ❌ |
| Arcade Racer wheel | ✅ | ❌ |
| Mouse | ✅ | ❌ |
| Light gun (Stunner / Virtua Gun) | ✅ | ❌ |
| Keyboard | ✅ | ❌ |
| 6-Player Multitap | ✅ | ❌ |

Implies the **full INTBACK peripheral enumeration protocol** (multi-port, per-
device ID + data length), which we don't yet do.

### 1.3 Disc / media handling

- **CHD support** — Mednafen reads `.chd`; we handle ISO / CUE-BIN / CCD only.
- **Multi-disc (`.m3u`) swapping** — Mednafen swaps discs via playlists; we have
  eject/insert but no playlist abstraction.
- **Seek timing** — Mednafen models drive seek latency; ours is near-instant
  (our M11 `Drive_Run` drive-phase port closed much of this).

### 1.4 Misc

- **SMPC clock-change (CKCHG320/352)** and **SYSRES** are recognized but
  **no-op** in ours (no software-driven NTSC↔PAL / horizontal-res switch).
- SH-2 on-chip **SCI / UBC / WDT** are storage stubs (rarely matter for games).

### 1.5 Things BOTH lack (don't over-credit Mednafen)

- **MPEG / Video-CD card** — *Mednafen does not emulate this either.* On our
  roadmap as "remaining," but not a Mednafen advantage.
- **CD move/copy sector ops** — niche; deferred on our side.
- **VDP1/VDP2 VRAM access contention** — we deliberately did *not* add it because
  Mednafen's oracle has no contention model (per our A4 audit).

---

## 2. VDP2 renderer — detailed comparison

Our renderer (`crates/saturn/src/vdp2/renderer.rs`, ~2.3k lines) already does:
NBG0-3 (tile + bitmap), RBG0/1 rotation, priority compositing, colour
calculation (ratio + additive, top-two), W0/W1 rectangle + line windows, sprite
window, sprite MSB shadow, line-colour screen, back screen, per-line scroll,
per-line horizontal zoom, per-column vertical cell scroll, mosaic, CRAM modes
0-3, rotation line-coefficient table (modes 0/1/2).

Mednafen reference: `mednaref/src/ss/vdp2_render.cpp` + `vdp2.cpp`.

### Validated gap list (ranked by impact × tractability)

| # | Feature | Mednafen | Ours | Impact | Effort |
|---|---|---|---|---|---|
| 1 | **Colour offset** (CLOFEN/CLOFSL, COAR/COAG/COAB, COBR/…) — per-layer RGB add/subtract | `MixIt` `vdp2_render.cpp:2581-2600`; enable/sel `:2954-2988` | ✅ **implemented 2026-06-08** | **High** | **Low** |
| 2 | **NBG0/1 reduction + fractional scroll** (ZMXN/ZMYN, XScrollF) | fixed-point `CurXCoordInc`/`CurXScrollIF` per dot (`:110`, `:114`, `:1590-1592`) | ✅ **implemented 2026-06-08** | **High** | Med |
| 3 | **Special priority / special colour-calc per-dot** (SFPRMD/SFCCMD/SFCODE/SFSEL) | templated `priomode`/`ccmode` `:3065`; SFCODE LUT | ✅ **implemented 2026-06-08** | Med | Med |
| 4 | **Extended colour calc — 3/4-layer blending** | `MIXIT_SPECIAL_EXCC_*` `:2491-2549` | 🟡 **non-line EXCC (CRAM0/CRAM12) done 2026-06-08** — front blends over avg(2nd, 3rd); line-colour + gradient EXCC variants deferred | Med | Med-High |
| 5 | **Dual rotation parameter selection** (RPMD 0/1 whole-layer; 2/3 per-pixel) | `EffRPMD` `:1862`, `rotabsel[x]` `:1977-2004`, `GetWinRotAB` | 🟡 **RPMD 0/1 done 2026-06-08**; modes 2/3 (per-dot coeff / window) deferred | Med (rotation games) | Med-High |
| 6 | **VRAM access cycle patterns** (CYCA0-CYCB1 / VCP) — bandwidth gating of fetches + reduction limits | full `VCPRegs` model `:71`, `:1399-1454` | 🟡 **fetch-gating done 2026-06-08** (NBG name-table/character `nt_ok`/`cg_ok` → dummy tile); reduction-limit half **excluded** (Mednafen per-game whitelist `:1714-1726` = oracle hack) | Low visual / High edge-case | **High** |

### 2.7 What is NOT a gap (corrections to first-pass analysis)

- **Dot-exact raster timing.** Mednafen's renderer is **per-scanline**
  (`VDP2REND_DrawLine`, `vdp2_render.cpp:2697`); line parameters are fetched once
  at line start. Its only per-dot logic is priority / colour-calc-bit selection,
  not mid-line register effects. So dot-exact mid-line rendering is *not* a
  Mednafen advantage.
- **NBG reduction is NOT our advantage.** An earlier automated pass claimed
  Mednafen lacks whole-screen NBG reduction and that we had it. Direct source
  reading disproved this: Mednafen does per-dot reduction via `XCoordInc`
  (ZMXN/ZMYN) and fractional scroll via `XScrollF`. We do *neither* (see #2).
- **Per-line scroll / zoom / vertical cell scroll** — we and Mednafen both do
  these; roughly at parity.

### Per-feature notes

1. **Colour offset.** Mednafen: each layer selects offset set A or B (CLOFSL);
   if enabled (CLOFEN) a signed 9-bit per-channel offset is added and clamped
   0..255, in the `MixIt` compositing stage. We have no colour-offset path at
   all. Registers needed: CLOFEN (0x110), CLOFSL (0x112), COAR/COAG/COAB
   (0x114/116/118), COBR/COBG/COBB (0x11A/11C/11E).

2. **Reduction + fractional scroll.** Mednafen keeps an integer+8-bit-fraction
   scroll accumulator (`CurXScrollIF`) and a per-dot coordinate increment
   (`CurXCoordInc`) so backgrounds can be scaled (1×, ½×, ¼×) and scrolled at
   sub-pixel precision. Ours samples on integer coordinates only; `regs.rs:494`
   notes the line-zoom longword is read "even though the renderer doesn't apply
   the zoom yet."

3. **Special priority / colour-calc.** We decode SFPRMD/SFCCMD/SFCODE/SFSEL
   already (`regs.rs:330-347`) but the per-dot application is intentionally
   deferred to avoid mis-rendering on a partial model. Mednafen applies it via
   templated `priomode`/`ccmode` plus an SFCODE lookup table.

4. **Extended colour calculation.** Mednafen blends up to 3-4 layers in several
   special MixIt modes (gradient, extended-CC, line-colour combos). **Done
   2026-06-08 (non-line EXCC):** the compositor now keeps the top *three* opaque
   dots and, when CCCTL EXCEN (bit 10) is set in low-res, the front layer's
   colour-calc partner becomes the rounding-down average of the 2nd and 3rd
   layers (gated on the 2nd layer's own CCCTL CC bit; RGB888 CRAM mode averages
   only an RGB 3rd layer) — Mednafen `MIXIT_SPECIAL_EXCC_CRAM0`/`CRAM12`,
   `vdp2_render.cpp:2537-2550` + the `:3136` mode selection. The **line-colour**
   EXCC variants (`EXCC_LINE_CRAM0/12`) and the **gradient** (`MIXIT_SPECIAL_GRAD`)
   special blend remain deferred.

5. **Dual rotation parameter selection.** Mednafen honours RPMD fully: mode 2
   selects param set A/B *per pixel* from the coefficient MSB, mode 3 selects via
   a rotation-parameter window (`GetWinRotAB`). We hardwire RBG0→A, RBG1→B and
   ignore RPMD; rotation coefficient mode 3 (Xp override) is also deferred.

6. **VRAM access cycle patterns.** Mednafen models the CYCA0-CYCB1 access-pattern
   table (4 banks × 8 cycles) that gates which layer may fetch pattern-name /
   character / vertical-cell-scroll data each cycle and constrains reduction.
   **Done 2026-06-08 (fetch-gating half):** `Vdp2Regs::nbg_vcp_fetch_masks`
   decodes the VCP table + VRAM partition (`vram_partition_mode`/`vcp_esb`) +
   RDBS into per-bank name-table / character permission masks (Mednafen's
   `nt_ok`/`cg_ok`, `vdp2_render.cpp:270-311`); `sample_tile` dummies a fetch
   from a non-granted bank → transparent (the hardware bandwidth gate). Validated
   by the `bios_boot` golden, which is unchanged because the BIOS splash programs
   a VCP table (VRAM_Mode 3, dumped from a boot) that grants its NBG2/NBG3 fetches
   the banks their data lives in — i.e. honouring the table reproduces the splash
   exactly. **The reduction-limit half is deliberately excluded:** Mednafen could
   not generalize it and carries a *hardcoded per-game VCP whitelist*
   (`vdp2_render.cpp:1714-1726`: Akumajou Dracula X, Alien Trilogy, Daytona,
   Fighters Megamix…) — porting that is importing per-game oracle hacks, against
   this project's accuracy-first / no-special-casing stance. Bitmap CG gating and
   the rotation (RDBS) fetch path also remain deferred.

---

## 3. Recommended order of work

1. ✅ **Colour offset (#1)** — DONE 2026-06-08. Post-blend step in `render_frame`
   (`apply_color_offset`, keyed on the front screen / back screen) + `Vdp2Regs`
   `color_offset_enable`/`color_offset_select`/`color_offset` accessors + two
   unit tests (`vdp2_render.rs`). **Validation:** the BIOS splash itself uses it
   — it programs `CLOFEN=0x08` (NBG3) + offset set B `(−64,−64,−64)` to darken
   the brushed-metal logo layer, which we previously ignored. Applying it is a
   correctness fix (matches Mednafen's `MixIt`); the `bios_splash` golden moved
   `0x2C379F92CE1B63F7` → `0x0B1BA6E5180766F7` accordingly.
2. ✅ **Reduction + fractional scroll (#2)** — DONE 2026-06-08. `sample_nbg`
   reworked to walk the source coordinate as 16.16 fixed point: whole-layer
   ZMXN/ZMYN reduction (new `nbg_coord_inc`) + the NBG0/1 8-bit scroll fraction
   (new `nbg_scroll_frac`), composed with the existing per-line scroll/zoom and
   vertical cell scroll. NBG2/3 stay 1:1 (no fraction/zoom), so the path
   collapses to the old `scroll + coord` and the splash golden is unchanged.
   Tests: `nbg0_horizontal_reduction_halves_the_layer`,
   `nbg0_fractional_scroll_shifts_the_sampled_source_pixel`.
3. ✅ **Special priority/CC (#3)** — DONE 2026-06-08 (full faithful port). The
   samplers return a `Sample` (rgb + palette code + spr/scc + is_rgb + CRAM-MSB);
   `resolve_special` ports Mednafen's `MakeSFCodeLUT` + `MakeNBGRBGPix` across all
   four SFPRMD priority modes and four SFCCMD colour-calc modes for NBG0–3 +
   RBG0/1. Golden-safe (collapses to prior behaviour when special modes are 0).
   Validated by unit tests vs the oracle algorithm — no game exercises it yet.
4. 🟡 **Dual rotation params (#5)** — RPMD **0/1 done 2026-06-08**: RBG0 selects
   rotation parameter set A or B whole-layer (`rotation_param_mode`; forced to A
   when RBG1 is active). `sample_rbg` now takes the parameter set and the layer
   separately (geometry/coefficient/plane from the set; CRAM offset / transparent
   pen from the layer). Modes **2/3** (per-dot coefficient-MSB / rotation-param
   window) are **deferred** — they need per-dot-x coefficient sampling, which our
   per-line coefficient model doesn't do (same root as the C5-deferred per-dot
   coefficient modes); fall back to set A for now. Test:
   `rpmd_selects_rotation_parameter_set_for_rbg0`.
4. 🟡 **Extended colour calc (#4)** — **non-line EXCC done 2026-06-08**: the
   compositor keeps the top *three* dots and blends the front over avg(2nd, 3rd)
   when CCCTL EXCEN is set (low-res, 2nd-layer CC bit set; RGB888 averages only
   an RGB 3rd layer). Line-colour + gradient EXCC variants deferred. Test:
   `extended_color_calc_blends_front_over_second_third_average`.
5. 🟡 **VCP cycle patterns (#6)** — **fetch-gating done 2026-06-08**: NBG
   name-table/character fetches are gated per VRAM bank by the CYCA0–CYCB1 table
   (`nbg_vcp_fetch_masks` → dummy tile when not granted), validated by the
   splash golden. The **reduction-limit half is excluded** (Mednafen per-game
   whitelist = oracle hack); bitmap CG + rotation RDBS gating deferred. Tests:
   `vram_cycle_pattern_gates_a_tile_layers_character_fetch`,
   `nbg_vcp_fetch_masks_decode_the_bios_splash_cycle_pattern`.

Per the M13 Tier C principle ("don't ship an active rendering feature whose
behaviour *direction* you can't validate"), #1 and #2 are the safest first steps;
#5/#6 want a specific game exercising them before landing.

---

## 4. Methodology / caveats

- Mednafen line numbers are from the local `mednaref` checkout and may drift with
  upstream versions.
- "Impact" is a judgement call about how many commercial games visibly exercise a
  feature; treat it as guidance, not measurement.
- Mednafen carries per-*game* timing/behaviour hacks (`HORRIBLEHACK_*` in
  `db.cpp`, keyed by serial/disc-fingerprint) — grep `db.cpp` for a game's serial
  before trusting Mednafen as that game's oracle.
