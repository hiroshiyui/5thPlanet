# 0008. Hand-rolled, software-composited frontend OSD

- **Status:** Accepted
- **Date:** 2026-05-28

## Context

The `jupiter` frontend was keyboard-only with a few hardcoded hotkeys
(Esc=quit, F5/F9 save/load). Everything M7/M8 added — save states, disc and
cartridge insertion, reset, the persisted battery — had no in-window UI. We
want an on-screen menu in the spirit of ZSNES / fwNES: a chunky, navigable
overlay drawn into the window.

Forces at play:

- The frontend renders by uploading the core's 320×224 RGBA framebuffer
  (`[R,G,B,A]` bytes) to a streaming SDL2 **canvas** texture scaled to the
  window — there is no OpenGL/wgpu surface in play.
- The project is deliberately dependency-light (the only frontend dependency
  is `sdl2`) and accuracy-first; `crates/saturn` must stay UI-agnostic and
  headless-clean (it already exposes `save_state`/`load_state`,
  `insert_disc`/`eject_disc`, `insert_cartridge`, `set_region`, `reset`,
  `internal_backup`).
- A real GUI toolkit (egui, imgui-rs) expects a GL/wgpu context and pulls in
  a stack of crates; adopting one would mean re-plumbing the canvas-based
  renderer and contradicting the minimal posture.

## Decision

We will build the OSD **by hand**, **software-composited** into the RGBA
framebuffer, and keep its logic **`sdl2`-free and core-free**:

- The `jupiter::osd` module draws with an embedded public-domain 8×8
  bitmap font (font8x8 "basic", CC0) directly into a `&mut [u8]` RGBA buffer.
  No GUI dependency; no new SDL render calls.
- The module takes input as an abstract `Nav { Up, Down, Select, Back }` enum
  and returns `OsdAction`s; the frontend bridges SDL key events → `Nav` and
  executes the actions against the `Saturn` API. The OSD never references
  `sdl2` or `saturn`, so its navigation **and** rendering are unit-testable
  without a window (they run even under `--no-default-features`).
- While the menu is open the emulator is **frozen** (no `run_frame`, no pad
  input, no audio queued); each frame the last image is copied, dimmed, and
  the menu composited over the copy.
- **Esc opens/closes** the menu (it no longer quits directly); Quit is a menu
  item, and the window close button still quits.

## Consequences

- **Easier:** zero new dependencies; the authentic chunky look; the OSD is
  fully unit-testable (font/draw pixels + the menu state machine) with no
  window; the core stays untouched; the menu is a clean front-end for the
  M7/M8 features and a natural home for later config (graphics, input,
  region/BIOS, cartridge) in subsequent M9 phases.
- **Harder / costs we accept:** we maintain our own widget, layout, and font
  code instead of getting widgets for free; text is chunky 320×224 (a
  feature here, not a bug); rich interactions (file pickers, scrolling,
  mouse) are extra hand-written work. Esc no longer quits, a small change to
  existing muscle memory.
- Follow-up: later M9 phases add a persisted config file and the
  graphics/input/region/cartridge submenus on this same framework.

## Alternatives considered

- **egui** — the modern Rust immediate-mode GUI. Rich widgets, but wants a
  GL/wgpu surface; integrating it means abandoning the SDL2 canvas blit or
  rendering egui to a texture, plus a stack of new crates. Too heavy for an
  in-window menu and against the project's minimal-dependency posture.
- **imgui-rs** — same surface/dependency problem as egui, with C++ bindings
  on top.
- **SDL2_ttf** — would still leave us writing all the menu/state logic by
  hand; it only replaces the (tiny, embedded) bitmap font with a runtime
  dependency on a system library and a shipped `.ttf`. Net negative here.
