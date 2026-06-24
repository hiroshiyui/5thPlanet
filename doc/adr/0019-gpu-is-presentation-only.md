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
  stack.

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
- **Add wgpu (or librashader) for the GPU/shader needs.** Rejected once on SDL3:
  `SDL_GPU` provides a cross-platform shader pipeline inside the dependency we
  already link, so a second heavy GPU stack isn't warranted (librashader remains
  a possible *future* route only if running RetroArch presets verbatim becomes a
  goal). See [0020](0020-migrate-sdl2-to-sdl3.md).
- **Leave the render driver to SDL's default (no selection).** Rejected: not
  user-selectable, and the `auto` case couldn't even report what it resolved to.
