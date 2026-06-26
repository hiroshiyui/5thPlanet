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

## Expected layout (to be finalized when the presenter lands)

One sub-directory per preset, e.g.:

```
shaders/
  crt-royale/
    crt-royale.glsl          # or a small manifest + per-pass shaders
    *.spv / *.dxil / *.msl   # precompiled outputs (optional)
  scanline-simple/
    scanline.frag
```

The active preset will be chosen via the frontend config (a shader key in
`jupiter.toml`), mirroring how `--backend` already selects the render driver.

## Getting shaders

- **RetroArch / libretro shaders** (crt-royale, crt-guest, …) — widely available,
  GPL/CC-licensed. If the `librashader` alternative (roadmap) is taken, these run
  closer to verbatim.
- **Hand-authored GLSL** — start from the de-risking **passthrough** shader (a 1:1
  copy of the input texture) the roadmap calls for, confirm the SDL_GPU swapchain
  path works, then add CRT passes.

## See also

- [`../doc/adr/0019-gpu-is-presentation-only.md`](../doc/adr/0019-gpu-is-presentation-only.md) — the presentation-only contract.
- [`../doc/roadmap.md`](../doc/roadmap.md) — the "CRT-shader presentation via SDL_GPU" backlog item (device entry point `SDL_CreateGPUDevice`, the pipeline, formats, and why dedicated-GPU selection is OS/driver-level, not an SDL flag).
