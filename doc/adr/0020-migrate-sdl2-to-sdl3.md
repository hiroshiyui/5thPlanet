# 0020. Migrate the SDL frontend from SDL2 to SDL3

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

The `jupiter` frontend used SDL2 (sdl2-rs 0.37) for its window, 2D presentation,
input, and audio. SDL3 is the current major version and brings the modern
multimedia APIs the roadmap wants:

- **`SDL_GPU`** — a cross-platform (Vulkan / Metal / D3D12), shader-capable,
  multi-pass render-target API. It is exactly what a high-quality CRT filter
  needs ([0019](0019-gpu-is-presentation-only.md)), and it lives *inside* SDL, so
  it removes the need for a separate GPU framework (wgpu/librashader).
- **`SDL_AudioStream`** — a stream-based audio model that buffers and
  **auto-resamples** to the device rate, replacing SDL2's `SDL_AudioQueue` +
  manual `AudioCVT`.
- The unified **gamepad** API (cleaner than SDL2's GameController).

Staying on SDL2 forecloses all of these (and SDL2 is now in maintenance mode).
The frontend is a leaf — nothing in the emulation core depends on it, and it is
entirely behind the `sdl-frontend` feature — so the migration is contained and
cannot affect determinism or accuracy. SDL 3.x is installable via pkg-config and
the `sdl3`/`sdl3-sys` crates link it.

## Decision

We will migrate `jupiter` to **SDL3** (sdl3-rs 0.18.4 + sdl3-sys, system lib via
the crate's `use-pkg-config`), and rename the feature `sdl2-frontend` →
**`sdl-frontend`** (version-neutral). The load-bearing API translations:

- **Audio:** `SDL_AudioQueue` (`open_queue`/`queue_audio`/`AudioCVT`) →
  `SDL_AudioStream` (`open_playback_device` → `open_device_stream` →
  `put_data_i16` / `queued_bytes` / `resume`). The stream auto-resamples, so the
  manual resampling **and** the device-rate pacing math are deleted; the
  audio-paced loop ([0014](0014-audio-paced-emulation-loop.md)) reads
  `queued_bytes()` directly (already source-rate).
- **Gamepad:** `sdl.game_controller()` → `sdl.gamepad()`; face buttons renamed
  A/B/X/Y → **South/East/West/North**; `open()` takes the `JoystickId` newtype;
  `id()` replaces `instance_id()`.
- **Render:** `into_canvas()` is infallible; **vsync is dropped** (the loop is
  audio-paced, not vsync-paced; sdl3-rs has no safe vsync wrapper and we don't
  need one); `copy()` takes float `FRect`s; texture format is `PixelFormat`; the
  active driver name comes from the `canvas.renderer_name` field.
- **Misc:** `set_fullscreen(bool)` (not `FullscreenType`); relative mouse mode is
  per-window; mouse-motion deltas are `f32`.

Validated: all three playable titles (Virtua Fighter 2, Doukyuusei ~if~,
Sangokushi V) run unchanged on SDL3 (user-confirmed, 2026-06-24).

## Consequences

- **Easier:** unlocks `SDL_GPU` for the CRT-shader path
  ([0019](0019-gpu-is-presentation-only.md)) with no new dependency; simpler audio
  (the stream resamples — net **−35 LOC**); the modern gamepad API.
- **Cost we knowingly accept:** **building `jupiter` now requires SDL3**
  (`libsdl3` + pkg-config) — CI and any other build machine must have SDL 3.x;
  SDL2 is dropped. sdl3-rs (0.18.4) is younger/less complete than sdl2-rs (e.g.
  no safe vsync wrapper — immaterial here). The headless build
  (`--no-default-features`) still needs no SDL at all.
- **Determinism / accuracy untouched:** frontend-only change; the core's
  deterministic stream and the goldens are unaffected.
- **Follow-up:** static-linked release binaries now target SDL3 (roadmap
  packaging item); the `SDL_GPU` CRT-shader feature (roadmap "Later milestones").

## Alternatives considered

- **Stay on SDL2.** Rejected: forecloses `SDL_GPU` / `AudioStream` / the modern
  gamepad API, and SDL2 is maintenance-mode — a dead end for the multimedia
  features the project wants next.
- **Add wgpu (and/or librashader) alongside SDL2** for the GPU/shader needs.
  Rejected: a second heavy GPU stack plus raw-window-handle version juggling,
  when SDL3 provides an equivalent (`SDL_GPU`) in the dependency we already link.
- **Switch wholesale to winit + wgpu.** Rejected: that rewrites window *and*
  input *and* audio, far larger and riskier than the contained SDL2→SDL3 swap on
  a leaf crate.
