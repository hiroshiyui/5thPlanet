# Shaders — presentation post-processing

This directory holds **presentation-layer shaders** — CRT filters (aperture
grille, scanline beam, bloom / halation, gamma) and any other post-process pass
applied to the final composited frame before it reaches the screen. **Nothing in
this directory other than this README is tracked in git** (`.gitignore` whitelists
only this README, so `git add shaders/*.glsl` etc. is silently skipped — by
design). Drop your own shaders here; bring your own license.

Two reasons shaders aren't committed: preset packs are frequently third-party and
**GPL-/CC-licensed** (e.g. RetroArch `crt-royale`), which the project keeps at
arm's length (same separation as `bios/`, `roms/`, and the reference emulators —
see the README Acknowledgements); and the compiled outputs (`.spv` / `.dxil` /
`.metallib`) are **build artifacts**.

> **Status:** the SDL_GPU CRT presenter is **not yet wired up**. It is a backlog
> item in [`../doc/roadmap.md`](../doc/roadmap.md) ("CRT-shader presentation via
> SDL_GPU"); this directory + guide is the groundwork. Until the `Presenter`
> lands, nothing here is loaded — the frontend presents through the plain
> `SDL_Renderer` blit (the `--backend` render-driver selector). This README will
> graduate to the real load paths / config keys when the feature ships.
>
> The one piece already built is the **capability probe** (`jupiter/src/present_gpu.rs`):
> the `gpu` config key / `--gpu` flag (`off` default / `auto` / `on`) makes the
> frontend try to create an `SDL_GPU` device for this host's shader format
> (SPIR-V / DXIL / MSL, per the table below) and log whether it's available —
> the gate the presenter will consult. It's `off` by default (the safe probe
> allocates a device); see [ADR-0019](../doc/adr/0019-gpu-is-presentation-only.md)
> "Capability detection" for why, and for the known limitation (no backend-name
> readback, so no software-rasterizer rejection yet).

## The bundled collection: `slang-shaders/`

A full copy of the RetroArch [**`slang-shaders`**](https://github.com/libretro/slang-shaders)
collection lives at [`slang-shaders/`](slang-shaders/) (gitignored, like everything
else here — it's GPL/CC third-party material, bring your own). It's **self-contained**:
~100 ready-to-use CRT presets at `slang-shaders/crt/*.slangp`, each referencing only
its own internal `shaders/…slang` passes (no external dependency tree to resolve).
Trinitron-style looks are already in the box — e.g.
`crt/crt-gdv-mini-ultra-trinitron.slangp`, and `crt/crt-guest-advanced.slangp`
(which uses a bundled `trinitron-lut.png`).

⚠️ **Format note:** these are **`.slangp` / `.slang`** (RetroArch's Slang preset
chain — Vulkan-GLSL dialect + a preprocessor), which is **librashader's native
format, not raw `SDL_GPU`**. SDL_GPU consumes compiled SPIR-V/DXIL/MSL (see
[Shader formats](#shader-formats) below), so two routes to actually run them:
take the **librashader** path (roadmap alternative) to load these presets verbatim,
or **hand-port** a chosen preset's GLSL passes to SPIR-V via `SDL_shadercross` for
the in-dependency SDL_GPU presenter. Until the presenter lands, this collection is
reference/source material — nothing in the build loads it yet.

## The accuracy contract (what a shader must NOT touch)

Per [ADR-0019](../doc/adr/0019-gpu-is-presentation-only.md), **the GPU is for
presentation only** — the Saturn picture is software-composited and stays
bit-identical regardless of host or backend. A shader here is a *cosmetic
post-process* on the already-final framebuffer
(`framebuffer → texture → 1–3 shader passes → swapchain`). It must never feed back
into VDP1/VDP2 rasterization, or accuracy (and oracle trace-diffing) is gone.

## Shader formats

The presenter targets SDL3's `SDL_GPU` API, which wants a different **compiled**
shader format per backend. Author once in GLSL and cross-compile:

| Backend (`--backend`) | Compiled format SDL_GPU consumes | Toolchain                                   |
| --------------------- | -------------------------------- | ------------------------------------------- |
| Vulkan                | **SPIR-V** (`.spv`)              | `glslc` / `glslangValidator`, or `SDL_shadercross` |
| Direct3D 12           | DXIL (or DXBC)                   | `SDL_shadercross` (SPIR-V → DXIL)           |
| Metal                 | MSL / `metallib`                | `SDL_shadercross` (SPIR-V → MSL)            |

`SDL_shadercross` (SDL's own cross-compiler) is the path of least resistance:
author GLSL → SPIR-V, then have it emit DXIL / MSL for non-Vulkan hosts.
Precompiling and dropping the `.spv` / `.dxil` / `.metallib` beside the source is
also fine — they just stay gitignored here.

> ⚠️ **`.slang` is not plain GLSL — `glslc` can't eat it directly.** The
> [`slang-shaders/`](slang-shaders/) sources are the **libretro Slang** dialect:
> *both* shader stages live in one file split by `#pragma stage vertex` /
> `#pragma stage fragment`, plus `#pragma name` / `#pragma parameter` /
> `#pragma format` directives `glslc` doesn't understand. So a `.slang` needs a
> **Slang-preprocess step first** — split the two stages and strip the pragmas
> (parsing the parameters out for the runtime) — *then* `glslc` / `glslangValidator`
> compiles each stage to SPIR-V (and `SDL_shadercross` on to DXIL/MSL). And that's
> only the per-pass compile: a `.slangp` is a **multi-pass pipeline** (per-pass
> scale/format/filter, LUT textures, feedback/history, parameters, and the libretro
> semantic uniforms `MVP`/`SourceSize`/`OutputSize`/`FrameCount`) the presenter must
> orchestrate itself. **librashader** does the whole preprocess + compile +
> multi-pass run internally, which is why it's the verbatim route; the hand-port
> route uses `glslc` for one sub-step of a longer chain. See ADR-0019 / the roadmap
> for the route trade-off.

## Expected layout (to be finalized when the presenter lands)

The dropped-in RetroArch collection keeps its own tree (`slang-shaders/crt/…`);
any hand-authored or precompiled presets for the SDL_GPU path sit alongside it,
one sub-directory per preset, e.g.:

```
shaders/
  slang-shaders/             # the RetroArch collection (gitignored) — see above
    crt/*.slangp             #   ~100 ready CRT presets (.slangp / .slang)
  crt-royale/                # a hand-ported / precompiled SDL_GPU preset, e.g.
    crt-royale.glsl          #   or a small manifest + per-pass shaders
    *.spv / *.dxil / *.msl   #   precompiled outputs (optional)
  scanline-simple/
    scanline.frag
```

The active preset will be chosen via the frontend config (a shader key in
`jupiter.toml`), mirroring how `--backend` already selects the render driver.

## Getting shaders

- **The bundled `slang-shaders/` collection** (above) — ~100 CRT presets already
  here. Native to **librashader** (`.slangp` runs verbatim); for the SDL_GPU path,
  hand-port a preset's GLSL passes via `SDL_shadercross`.
- **More RetroArch / libretro shaders** (crt-royale, crt-guest, …) — widely
  available, GPL/CC-licensed; drop them here too (gitignored).
- **Hand-authored GLSL** — start from the de-risking **passthrough** shader (a 1:1
  copy of the input texture) the roadmap calls for, confirm the SDL_GPU swapchain
  path works, then add CRT passes.

## See also

- [`../doc/adr/0019-gpu-is-presentation-only.md`](../doc/adr/0019-gpu-is-presentation-only.md) — the presentation-only contract.
- [`../doc/roadmap.md`](../doc/roadmap.md) — the "CRT-shader presentation via SDL_GPU" backlog item (device entry point `SDL_CreateGPUDevice`, the pipeline, formats, and why dedicated-GPU selection is OS/driver-level, not an SDL flag).
