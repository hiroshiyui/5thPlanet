# Built-in presentation shaders

The project's own CRT post-process shader for the SDL_GPU presenter
(`present_gpu::GpuPresenter`, `gpu-preview` feature). Unlike the repo-root
`shaders/` directory (gitignored — third-party preset drop-zone), these are
**tracked, project-authored (MIT), and committed alongside their compiled
SPIR-V**, so a normal build needs no shader toolchain (`include_bytes!` the
`.spv`).

| Source            | Compiled    | Stage    |
| ----------------- | ----------- | -------- |
| `crt.vert.glsl`   | `crt.vert.spv` | vertex   |
| `crt.frag.glsl`   | `crt.frag.spv` | fragment |

`crt.frag.glsl` is single-pass and flat (scanlines + aperture-grille mask +
gamma; no curvature) — v1. It reads the already-composited frame and writes the
swapchain; **presentation-only**, the picture stays bit-identical (ADR-0019).

## Regenerate the SPIR-V after editing the GLSL

Requires `glslc` (shaderc) on PATH. From this directory:

```sh
glslc -fshader-stage=vert crt.vert.glsl -o crt.vert.spv
glslc -fshader-stage=frag crt.frag.glsl -o crt.frag.spv
```

Commit the regenerated `.spv` together with the `.glsl` change.

## SDL_GPU descriptor-set convention (load-bearing)

SDL_GPU's SPIR-V resource model fixes the descriptor sets: **fragment sampled
textures are `set = 2`, fragment uniform buffers `set = 3`** (vertex uniforms
`set = 1`). The fragment shader must declare the frame sampler at
`layout(set = 2, binding = 0)` and the params UBO at `layout(set = 3, binding =
0)`; a wrong set renders silent black. The Rust side binds them with
`bind_fragment_samplers(0, ..)` / `push_fragment_uniform_data(0, ..)`.

DXIL/MSL (non-Vulkan hosts) and multi-pass effects (bloom/halation, curvature)
are follow-ups; see `doc/roadmap.md`.
