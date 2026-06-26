# 0019. Frontend graphics are software-composited; the GPU is for presentation only

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

The project's design axis is accuracy over performance ([0002](0002-accuracy-over-performance.md)),
and its correctness method is instruction- and pixel-level diffing against the
Mednafen/MAME oracles ([0017](0017-reference-oracle-policy.md)). The VDP1 and
VDP2 are therefore emulated in software: `saturn::vdp2::render_frame` is a pure,
deterministic, bit-exact composite that produces one RGBA framebuffer per frame,
host- and backend-independent (the render goldens depend on it).

The `jupiter` frontend's only graphics job is to get that finished 2D buffer onto
the screen — upload, scale, blit. That *is* a natural GPU task, and SDL's 2D
renderer already abstracts the GPU backend (OpenGL / Direct3D / Metal / software).
But two temptations recur and need a settled answer: (1) "use the GPU to *render*
the Saturn picture" (rasterize sprites/tiles/rotation on the GPU), and (2) "add a
GPU framework (wgpu, librashader) for post-processing / CRT filters."

## Decision

We will keep all Saturn-picture generation in software and use the GPU **only for
presentation**. Concretely:

- **The VDP stays software-rendered.** The composited framebuffer is bit-identical
  regardless of host or graphics backend. A change that moves VDP1/VDP2
  rasterization onto the GPU violates this ADR — it would diverge from hardware
  output and defeat oracle diffing.
- **Backend selection is the SDL render driver,** chosen via
  `present::RenderBackend` (the `backend` config key / `--backend` flag) with a
  fallback chain (preferred → opengl → software); the driver actually created is
  read back and logged (`jupiter/src/present.rs`). The framebuffer-blit path
  (`texture.update` → `copy` → `present`) is unchanged across backends.
- **Post-processing (CRT scanline / aperture-grille shaders) is a
  presentation-layer cosmetic** and, when built, will use **SDL3's `SDL_GPU`** —
  the in-dependency, multi-pass, shader-capable API made available by the SDL3
  migration ([0020](0020-migrate-sdl2-to-sdl3.md)) — behind a `Presenter` trait
  with the `SDL_Renderer` blit as the default and fallback. Not a separate GPU
  stack. This route reimplements the slang multi-pass pipeline itself rather than
  embedding librashader — the trade is weighed under "Alternatives" + Revisited.

### Capability detection (added 2026-06-27)

Whether the `SDL_GPU` presenter can run at all is a *runtime* property of the
host, so the frontend probes it (`jupiter/src/present_gpu.rs`). The workspace
forbids `unsafe`, and sdl3-rs 0.18.4 gives a **safe** `sdl3::gpu::Device::new(...)
-> Result` but **no** safe wrapper for the cheap non-allocating pre-probes
(`SDL_GPUSupportsShaderFormats`, `SDL_GetNumGPUDrivers`). So the `unsafe`-free
detection is: pick the shader format we'd ship for this OS (SPIR-V on Vulkan, DXIL
on D3D12, MSL on Metal — `ShaderKind`) → attempt `Device::new` for it → `Ok` means
SDL_GPU is available, `Err` means log `SDL_GetError` and keep the `SDL_Renderer`
path. Because the safe probe must *allocate* a device, it is **opt-in** (the `gpu`
config key / `--gpu` flag: `off` default, `auto`, `on`); the default flips to
`auto` when the presenter actually consumes the verdict (`GpuCapability`).

**Known limitation / follow-up:** sdl3-rs 0.18.4 also leaves
`SDL_GetGPUDeviceDriver` unwrapped, so the probe reports only *whether* a device
was created, not *which* backend it chose — and therefore cannot reject a slow
software Vulkan (llvmpipe/lavapipe). Reading the driver back (to label the backend
and reject software rasterizers) waits on a newer sdl3-rs with a safe accessor, or
a justified, narrowly-scoped `#![allow(unsafe_code)]` shim.

### Revisited 2026-06-27 — librashader weighed again, SDL_GPU reaffirmed

Prompted by assembling the RetroArch `slang-shaders` corpus (the presets we'd
actually want to run), the librashader-vs-SDL_GPU route was reconsidered with
current facts. The earlier "heavy parallel GPU stack" framing was wrong:
librashader is pure-Rust, Cargo-native, mature, and runs the whole `.slangp`
pipeline verbatim — strictly *less* implementation work than the SDL_GPU route.
**SDL_GPU is nonetheless reaffirmed**, trading that convenience for three things
the project values more here: a permissive license (vs librashader's MPL/GPL
copyleft), an SDL-only dependency tree (vs `ash`/`wgpu` + `glslang`/`naga`/
`spirv-cross2`), and staying inside SDL3's presentation surface (librashader has
no SDL_GPU backend and would need its own GL/Vulkan/wgpu context). **Accepted
cost:** the route reimplements the slang multi-pass runtime + Slang-preprocess
itself, so realistically only a few simple presets get hand-ported (e.g. 1-pass
`crt-geom`), not the 12-pass `crt-guest-advanced`. librashader remains the
documented escape hatch if running the full corpus verbatim ever outweighs the
copyleft + dep-weight costs. The full research backing this is in the
`shaders/README.md` route discussion. (See the updated "Alternatives" bullet.)

## Consequences

- **Easier:** cross-platform presentation comes for free (SDL's backends); the
  accuracy contract is unbreakable from the frontend (a GPU/driver bug can only
  affect *display* of an already-correct frame, never the emulated pixels); the
  render goldens stay host-independent.
- **Cost we accept:** no GPU-accelerated VDP, ever (deliberate — it would break
  accuracy and oracle diffing). High-quality CRT emulation is therefore deferred
  shader work (roadmap "Later milestones"), not a quick win.
- **Invariant for reviewers:** emulation rendering is software; GPU code lives
  only in the presentation path (`present`/a future `Presenter`).

## Alternatives considered

- **GPU-accelerate the VDP.** Rejected: output would no longer be bit-exact
  against hardware/Mednafen, killing the diff methodology — the opposite of the
  project's axis.
- **Use librashader (or wgpu) for the GPU/shader needs.** Not chosen, but on an
  *accurate* weighing (see Revisited 2026-06-27): librashader is **pure-Rust,
  Cargo-native, mature, modular**, and a **complete verbatim runtime of the
  RetroArch `.slangp` slang pipeline** (multi-pass, feedback/history, LUTs,
  parameters, the libretro semantic uniforms) that plugs in exactly as a
  presentation post-process (input texture → caller output target → caller
  presents) — *not* the "heavy parallel stack" an earlier draft called it. It is
  still not chosen for three concrete reasons: (1) **license** — it is
  MPL-2.0/GPL-3.0 copyleft (link-friendly + file-level, but a posture change for
  an otherwise-permissive tree); (2) **dependency weight** — it pulls a backend
  (`ash`/`wgpu`) plus the reflection stack (`glslang`/`naga`/`spirv-cross2`),
  much heavier than SDL-only; (3) **no SDL_GPU backend** — it targets
  GL/Vulkan/D3D/Metal/wgpu and can't ride SDL3's 2D renderer, so adopting it means
  standing up a separate GPU device/context. `SDL_GPU` instead provides a
  shader pipeline *inside the dependency we already link*, permissively. The
  accepted cost is that the SDL_GPU route must reimplement the slang multi-pass
  runtime + the Slang-preprocess front-end itself; librashader stays the
  documented escape hatch if running the full preset corpus verbatim ever
  outweighs those costs. See [0020](0020-migrate-sdl2-to-sdl3.md).
- **Leave the render driver to SDL's default (no selection).** Rejected: not
  user-selectable, and the `auto` case couldn't even report what it resolved to.
