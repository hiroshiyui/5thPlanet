//! SDL_GPU presentation backend — an alternative to the `SDL_Renderer` blit, and
//! groundwork for the planned CRT-shader presenter (ADR-0019, `shaders/README.md`).
//!
//! The Saturn picture is always composited in software (the accuracy-first core);
//! this module only posts the finished frame to the window via SDL_GPU instead of
//! the 2D renderer, so accuracy is untouched. It's **opt-in** and gated behind the
//! off-by-default `gpu-preview` feature (the future CRT passes aren't written yet):
//! the `gpu` config key / `--gpu` flag (`off` default / `auto` / `on`) selects it.
//!
//! ## [`GpuPresenter`] — the real backend (no shaders authored)
//!
//! `GpuPresenter` owns its own window + a Vulkan (SPIR-V) `Device` and presents a
//! frame each call: upload the `[R, G, B, A]` framebuffer to a GPU texture, then
//! post it to the swapchain via SDL's built-in `SDL_BlitGPUTexture` (which carries
//! its own blit shader — so **no SPIR-V is authored**), letterboxed to 4:3 on a
//! black clear. `main.rs` selects it over the renderer canvas when `--gpu=auto|on`
//! succeeds (the two are mutually exclusive — an SDL_GPU device claims the window
//! its swapchain owns), falling back to the `SDL_Renderer` blit otherwise. The
//! constructor *is* the capability check: `unsafe`-free because `Device::new`
//! returns a `Result`, so a host with no usable backend simply yields `Err`.
//!
//! ## [`run_selftest`] — the contained proof (`jupiter --gpu-selftest`)
//!
//! `--gpu-selftest` drives a `GpuPresenter` with an animated test pattern (no
//! emulator), a standalone one-shot validating the upload → blit → present pipeline
//! on real hardware. It shares the exact present path the real backend uses.
//!
//! ## Rejecting software Vulkan
//!
//! Vulkan device enumeration includes software drivers (Lavapipe/llvmpipe), so a
//! naïve `Device::new` could land `--gpu=auto` on a slow CPU renderer. We build
//! the device through `Properties` with `requirehardwareacceleration = true`
//! (`PROP_REQUIRE_HW_ACCEL`), so SDL refuses a software-only host at creation —
//! `GpuPresenter::new` returns `Err` and `main.rs` falls back to `SDL_Renderer`.
//! This is the property SDL documents for exactly this case ("if you can provide
//! your own fallback renderer"). The whole path stays `unsafe`-free — sdl3-rs's
//! `Setter`/`new_with_properties` wrap the FFI — so the workspace `forbid` holds.
//! (We still can't *label* the chosen backend: `SDL_GetGPUDeviceDriver` isn't
//! safe-wrapped in sdl3-rs 0.18.4 — a cosmetic follow-up, ADR-0019.)

/// Whether to present via the SDL_GPU backend. Parsed from the `gpu` config key /
/// `--gpu` flag (the CLI flag wins, like `--backend`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuMode {
    /// Never use SDL_GPU (default). Zero startup cost; the `SDL_Renderer` blit is
    /// the only presentation path.
    Off,
    /// Use SDL_GPU if a device can be created, else fall back to the renderer
    /// quietly (the host simply can't).
    Auto,
    /// Use SDL_GPU; if a device can't be created, warn loudly then fall back (the
    /// user explicitly forced it).
    On,
}

impl GpuMode {
    /// Parse a config/CLI token (case-insensitive). Unknown/empty tokens are the
    /// safe default — [`Off`](GpuMode::Off) — so a stale config never forces a
    /// device allocation.
    pub fn from_token(tok: &str) -> Self {
        match tok.trim().to_ascii_lowercase().as_str() {
            "auto" => GpuMode::Auto,
            "on" | "force" | "true" | "1" => GpuMode::On,
            _ => GpuMode::Off,
        }
    }

    /// The canonical config token for this mode (inverse of [`from_token`]).
    /// Symmetric with `RenderBackend::to_token`; the OSD will use it to persist
    /// the setting once a GPU presenter is wired (today only the round-trip test
    /// consumes it).
    ///
    /// [`from_token`]: GpuMode::from_token
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn to_token(self) -> &'static str {
        match self {
            GpuMode::Off => "off",
            GpuMode::Auto => "auto",
            GpuMode::On => "on",
        }
    }
}

/// Whether `mode` asks for the SDL_GPU backend at all (everything but
/// [`Off`](GpuMode::Off)). The caller still falls back to the renderer if the
/// device can't be created.
pub fn should_probe(mode: GpuMode) -> bool {
    !matches!(mode, GpuMode::Off)
}

/// The centred destination rectangle `(x, y, w, h)` for fitting an `ar_w:ar_h`
/// picture inside a `win_w × win_h` drawable while keeping its aspect ratio —
/// the letterbox/pillarbox geometry the SDL_GPU blit writes into the swapchain.
/// A window wider than the target ratio gets pillarbox bars (left/right); a
/// taller one gets letterbox bars (top/bottom). Returns all-zero for a degenerate
/// (zero-sized) window. **Pure** (no SDL), so the geometry is unit-testable.
pub fn letterbox_rect(win_w: u32, win_h: u32, ar_w: u32, ar_h: u32) -> (u32, u32, u32, u32) {
    if win_w == 0 || win_h == 0 || ar_w == 0 || ar_h == 0 {
        return (0, 0, 0, 0);
    }
    // Compare window aspect vs target without floats: win_w/win_h vs ar_w/ar_h.
    if win_w * ar_h >= win_h * ar_w {
        // Window is wider than (or equal to) the target → height fills, pillarbox.
        let w = win_h * ar_w / ar_h;
        ((win_w - w) / 2, 0, w, win_h)
    } else {
        // Window is taller → width fills, letterbox.
        let h = win_w * ar_h / ar_w;
        (0, (win_h - h) / 2, win_w, h)
    }
}

/// The precompiled shader format we'd ship for a given host — hence the format
/// the probed SDL_GPU device must accept (see the format table in
/// `shaders/README.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderKind {
    /// SPIR-V — the Vulkan backend (Linux + the cross-platform default).
    Spirv,
    /// DXIL — the Direct3D 12 backend (Windows).
    Dxil,
    /// MSL — the Metal backend (macOS / iOS).
    Msl,
}

/// `std::env::consts::OS` → the shader format SDL_GPU consumes on that host.
/// Pure (takes the OS string) so the mapping is unit-testable without `cfg!`.
fn map_os(os: &str) -> ShaderKind {
    match os {
        "windows" => ShaderKind::Dxil,
        "macos" | "ios" => ShaderKind::Msl,
        // Linux, the BSDs, and anything else target Vulkan/SPIR-V.
        _ => ShaderKind::Spirv,
    }
}

impl ShaderKind {
    /// The shader format for the host this binary is built for.
    fn for_target() -> Self {
        map_os(std::env::consts::OS)
    }

    /// The SDL_GPU device-create property key that opts this binary's shader
    /// format in (set `true` on the create `Properties`, in lieu of the
    /// `Device::new` format flags, so we can also set other create properties).
    /// These are SDL's stable ABI strings — the `&str` value of sdl3-sys's
    /// `SDL_PROP_GPU_DEVICE_CREATE_SHADERS_*_BOOLEAN` (read via `&str` so no
    /// `unsafe` CStr conversion of the raw `c_char` constant is needed). Pure, so
    /// it's unit-testable without the `gpu-preview` feature.
    fn create_prop_key(self) -> &'static str {
        match self {
            ShaderKind::Spirv => "SDL.gpu.device.create.shaders.spirv",
            ShaderKind::Dxil => "SDL.gpu.device.create.shaders.dxil",
            ShaderKind::Msl => "SDL.gpu.device.create.shaders.msl",
        }
    }
}

/// The SDL_GPU device-create property that requires a **hardware** Vulkan device.
/// Vulkan enumeration otherwise includes software drivers (Lavapipe/llvmpipe), so
/// without it `--gpu=auto` could silently land on a slow CPU renderer. We have a
/// real fallback (the `SDL_Renderer` blit), so we require hardware accel and let
/// creation fail when only software Vulkan exists. SDL's stable ABI string (the
/// `&str` value of `SDL_PROP_GPU_DEVICE_CREATE_VULKAN_REQUIRE_HARDWARE_ACCELERATION_BOOLEAN`);
/// it only affects the Vulkan backend (ignored by D3D12/Metal). See ADR-0019.
#[cfg(feature = "gpu-preview")]
const PROP_REQUIRE_HW_ACCEL: &str = "SDL.gpu.device.create.vulkan.requirehardwareacceleration";

/// Saturn lo-res frame shape the self-test uploads (mirrors the real
/// framebuffer's 320×224 native layout and `[R, G, B, A]` byte order).
#[cfg(feature = "gpu-preview")]
const SELFTEST_W: usize = 320;
#[cfg(feature = "gpu-preview")]
const SELFTEST_H: usize = 224;

/// Fill an `w × h` RGBA buffer with an animated SMPTE-style colour-bar pattern
/// plus a white bar sweeping left→right at `frame`'s rate. The motion proves the
/// presenter shows *live* frames (not a frozen blit), and the bars prove the
/// pixel byte order is correct end-to-end. **Pure** so it's testable without a
/// GPU; the bytes are laid out `[R, G, B, A]` to match `TextureFormat::R8g8b8a8`.
#[cfg(any(feature = "gpu-preview", test))]
fn fill_test_pattern(buf: &mut [u8], w: usize, h: usize, frame: u32) {
    // SMPTE-ish bars: white, yellow, cyan, green, magenta, red, blue, near-black.
    const BARS: [(u8, u8, u8); 8] = [
        (192, 192, 192),
        (192, 192, 0),
        (0, 192, 192),
        (0, 192, 0),
        (192, 0, 192),
        (192, 0, 0),
        (0, 0, 192),
        (16, 16, 16),
    ];
    let bar_w = (w / BARS.len()).max(1);
    let sweep = (frame as usize) % w;
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = if x.abs_diff(sweep) < 2 {
                (255, 255, 255)
            } else {
                BARS[(x / bar_w).min(BARS.len() - 1)]
            };
            let i = (y * w + x) * 4;
            buf[i] = r;
            buf[i + 1] = g;
            buf[i + 2] = b;
            buf[i + 3] = 255;
        }
    }
}

#[cfg(feature = "gpu-preview")]
use sdl3::gpu::{
    BlitInfo, Device, Filter, LoadOp, Texture, TextureCreateInfo, TextureFormat, TextureRegion,
    TextureTransferInfo, TextureType, TextureUsage, TransferBuffer, TransferBufferUsage,
};
#[cfg(feature = "gpu-preview")]
use sdl3::properties::{Properties, Setter};
#[cfg(feature = "gpu-preview")]
use sdl3::video::Window;

/// A self-contained **SDL_GPU presenter** that posts the software-composited
/// Saturn frame to a window via SDL's built-in `SDL_BlitGPUTexture` — the
/// alternative to the `SDL_Renderer` blit, **with no shaders authored**. It owns
/// its own bare window (an SDL_GPU device claims a window for its swapchain, so it
/// can't share the renderer's canvas-owned window) plus a Vulkan device, a SAMPLER
/// frame texture, and an UPLOAD transfer buffer. The same path the self-test
/// proves and the real `--gpu` backend uses.
///
/// **Field order is the drop order** (struct fields drop top-to-bottom): the
/// device must be destroyed *before* its claimed window, so `device` precedes
/// `window`. The GPU resources (`frame_tex`/`transfer`) hold only a `WeakDevice`,
/// so their release is safe in any order.
#[cfg(feature = "gpu-preview")]
pub struct GpuPresenter {
    /// SAMPLER texture the framebuffer uploads into; the blit source. Recreated
    /// when the active frame resolution changes.
    frame_tex: Texture<'static>,
    /// UPLOAD staging buffer for the per-frame texture upload; sized to the frame.
    transfer: TransferBuffer,
    /// The Vulkan (SPIR-V) device whose swapchain is the claimed `window`.
    device: Device,
    /// The presenter's own window (claimed by `device`; not a renderer canvas).
    window: Window,
    /// The frame resolution `frame_tex`/`transfer` are currently sized for; a
    /// change triggers their recreation in [`present`](GpuPresenter::present).
    frame_dims: (u32, u32),
}

#[cfg(feature = "gpu-preview")]
impl GpuPresenter {
    /// Build the SAMPLER frame texture + matching UPLOAD transfer buffer for a
    /// `w × h` frame. Split out so `new` and the resolution-change path share it.
    fn make_frame_resources(
        device: &Device,
        w: u32,
        h: u32,
    ) -> Result<(Texture<'static>, TransferBuffer), String> {
        let tex = device
            .create_texture(
                TextureCreateInfo::new()
                    .with_type(TextureType::_2D)
                    .with_format(TextureFormat::R8g8b8a8Unorm)
                    .with_width(w)
                    .with_height(h)
                    .with_layer_count_or_depth(1)
                    .with_num_levels(1)
                    .with_usage(TextureUsage::SAMPLER),
            )
            .map_err(|e| format!("frame texture create failed ({e})"))?;
        let transfer = device
            .create_transfer_buffer()
            .with_size(w * h * 4)
            .with_usage(TransferBufferUsage::UPLOAD)
            .build()
            .map_err(|e| format!("transfer buffer create failed ({e})"))?;
        Ok((tex, transfer))
    }

    /// Open a `win_w × win_h` resizable window, claim a Vulkan (SPIR-V) SDL_GPU
    /// device for it, and allocate the `frame_w × frame_h` upload resources.
    /// `Err` (no usable SDL_GPU backend, or allocation failure) lets the caller
    /// fall back to the `SDL_Renderer` path.
    pub fn new(
        video: &sdl3::VideoSubsystem,
        title: &str,
        win_w: u32,
        win_h: u32,
        frame_w: u32,
        frame_h: u32,
    ) -> Result<Self, String> {
        let window = video
            .window(title, win_w, win_h)
            .position_centered()
            .resizable()
            .build()
            .map_err(|e| format!("window create failed ({e})"))?;
        // Create the device through `Properties` (not `Device::new`'s format
        // flags) so we can also set `requirehardwareacceleration` — making SDL
        // reject a software Vulkan (Lavapipe/llvmpipe) at creation. The `unsafe`
        // is inside sdl3-rs's `Setter`/`new_with_properties`, so our crate stays
        // `forbid`. On a software-only host this `Err`s and the caller falls back.
        let shader = ShaderKind::for_target();
        let props = Properties::new().map_err(|e| format!("GPU device properties ({e:?})"))?;
        props
            .set(shader.create_prop_key(), true)
            .and_then(|()| props.set(PROP_REQUIRE_HW_ACCEL, true))
            .map_err(|e| format!("GPU device properties ({e:?})"))?;
        let device = Device::new_with_properties(props)
            .and_then(|d| d.with_window(&window))
            .map_err(|e| format!("no hardware GPU device for {shader:?} ({e})"))?;
        let (frame_tex, transfer) = Self::make_frame_resources(&device, frame_w, frame_h)?;
        Ok(Self {
            frame_tex,
            transfer,
            device,
            window,
            frame_dims: (frame_w, frame_h),
        })
    }

    /// The presenter's window, for the shared window controls (size / fullscreen /
    /// icon / relative-mouse). Both backends expose `&Window`/`&mut Window`.
    pub fn window(&self) -> &Window {
        &self.window
    }

    /// Mutable window accessor (see [`window`](GpuPresenter::window)).
    pub fn window_mut(&mut self) -> &mut Window {
        &mut self.window
    }

    /// Present one `dims.0 × dims.1` frame (tightly packed `[R, G, B, A]`, the
    /// VDP2 framebuffer layout): upload it to the GPU texture and blit it to the
    /// swapchain. `sharp` picks nearest vs linear filtering; `keep_aspect` letterboxes
    /// the picture to 4:3 (else it stretches to fill the window — matching the
    /// renderer's logical-presentation modes). Recreates the frame resources when
    /// `dims` changes (the old ones auto-release via their `WeakDevice`). Returns
    /// `true` if the frame reached the swapchain, `false` if it was skipped (a
    /// transient acquire failure or a minimised window) — the caller treats a skip
    /// as a dropped frame, never a fatal error.
    pub fn present(
        &mut self,
        framebuffer: &[u8],
        dims: (u32, u32),
        sharp: bool,
        keep_aspect: bool,
    ) -> bool {
        if dims != self.frame_dims {
            match Self::make_frame_resources(&self.device, dims.0, dims.1) {
                Ok((tex, transfer)) => {
                    self.frame_tex = tex;
                    self.transfer = transfer;
                    self.frame_dims = dims;
                }
                Err(e) => {
                    eprintln!("SDL_GPU: frame-resource resize failed ({e}); skipping frame");
                    return false;
                }
            }
        }
        let frame_bytes = (dims.0 * dims.1 * 4) as usize;
        if framebuffer.len() < frame_bytes {
            return false;
        }

        // 1) Upload the frame into the GPU texture via a copy pass.
        {
            let mut map = self.transfer.map::<u8>(&self.device, true);
            map.mem_mut().copy_from_slice(&framebuffer[..frame_bytes]);
            map.unmap();
        }
        let Ok(mut copy_cmd) = self.device.acquire_command_buffer() else {
            return false;
        };
        let Ok(copy_pass) = self.device.begin_copy_pass(&copy_cmd) else {
            copy_cmd.cancel();
            return false;
        };
        copy_pass.upload_to_gpu_texture(
            TextureTransferInfo::new()
                .with_transfer_buffer(&self.transfer)
                .with_offset(0),
            TextureRegion::new()
                .with_texture(&self.frame_tex)
                .with_width(dims.0)
                .with_height(dims.1)
                .with_depth(1),
            false,
        );
        self.device.end_copy_pass(copy_pass);
        if copy_cmd.submit().is_err() {
            return false;
        }

        // 2) Blit the texture into the swapchain. SDL's built-in blit supplies its
        //    own shader — none authored. `keep_aspect` → 4:3 letterbox dst on a
        //    black clear; otherwise the full window (stretch).
        let Ok(mut draw_cmd) = self.device.acquire_command_buffer() else {
            return false;
        };
        if let Ok(swapchain) = draw_cmd.wait_and_acquire_swapchain_texture(&self.window) {
            let (sw, sh) = (swapchain.width(), swapchain.height());
            let (dx, dy, dw, dh) = if keep_aspect {
                letterbox_rect(sw, sh, 4, 3)
            } else {
                (0, 0, sw, sh)
            };
            if dw == 0 || dh == 0 {
                // Minimised / zero-sized drawable — nothing to present this frame.
                draw_cmd.cancel();
                return false;
            }
            let blit = BlitInfo::default()
                .with_source_texture(&self.frame_tex)
                .with_source_region(0, 0, 0, dims.0, dims.1)
                .with_destination_texture(&swapchain)
                .with_destination_region(0, dx, dy, dw, dh)
                .with_load_op(LoadOp::CLEAR)
                .with_clear_color(sdl3::pixels::Color::RGB(0, 0, 0))
                .with_filter(if sharp {
                    Filter::Nearest
                } else {
                    Filter::Linear
                });
            draw_cmd.blit_texture(blit);
            draw_cmd.submit().is_ok()
        } else {
            draw_cmd.cancel();
            false
        }
    }
}

/// **SDL_GPU Vulkan presenter self-test** (`jupiter --gpu-selftest`, `gpu-preview`
/// builds only). A contained proof that SDL_GPU works as an *alternative*
/// presenter to the `SDL_Renderer` blit, with **no shaders authored**: it drives a
/// [`GpuPresenter`] (the exact path the real `--gpu` backend uses), feeding it an
/// animated test pattern each frame. Esc/close or ~1800 frames exits.
///
/// Returns `FAILURE` if no GPU device can be created (the host has no Vulkan
/// backend, or only a rejected one), `SUCCESS` after a clean present run.
#[cfg(feature = "gpu-preview")]
pub fn run_selftest() -> std::process::ExitCode {
    use sdl3::event::Event;
    use sdl3::keyboard::Keycode;

    let Ok(sdl) = sdl3::init() else {
        eprintln!("SDL_GPU self-test: SDL3 init failed");
        return std::process::ExitCode::FAILURE;
    };
    let video = match sdl.video() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("SDL_GPU self-test: no video subsystem ({e})");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut presenter = match GpuPresenter::new(
        &video,
        "5thPlanet — SDL_GPU Vulkan self-test",
        640,
        480,
        SELFTEST_W as u32,
        SELFTEST_H as u32,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SDL_GPU self-test: {e}; the host has no usable SDL_GPU backend");
            return std::process::ExitCode::FAILURE;
        }
    };
    eprintln!(
        "SDL_GPU self-test: device created ({:?} shaders, Vulkan on Linux); \
         presenting an animated test pattern. Resize to see the 4:3 letterbox; \
         Esc/close to exit.",
        ShaderKind::for_target()
    );

    let mut event_pump = match sdl.event_pump() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SDL_GPU self-test: event pump failed ({e})");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut pattern = vec![0u8; SELFTEST_W * SELFTEST_H * 4];
    let mut presented: u32 = 0;
    // Safety cap so a headless/CI invocation can't hang forever (~30 s at 60 Hz).
    const MAX_FRAMES: u32 = 1800;
    'run: for frame in 0..MAX_FRAMES {
        for ev in event_pump.poll_iter() {
            match ev {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => break 'run,
                _ => {}
            }
        }

        fill_test_pattern(&mut pattern, SELFTEST_W, SELFTEST_H, frame);
        if presenter.present(&pattern, (SELFTEST_W as u32, SELFTEST_H as u32), true, true) {
            presented += 1;
        }

        std::thread::sleep(std::time::Duration::from_millis(16));
    }

    eprintln!("SDL_GPU self-test: presented {presented} frame(s) via the Vulkan blit — OK");
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_token_round_trips_and_defaults_to_off() {
        for m in [GpuMode::Off, GpuMode::Auto, GpuMode::On] {
            assert_eq!(GpuMode::from_token(m.to_token()), m);
        }
        // Aliases + case-insensitivity.
        assert_eq!(GpuMode::from_token("ON"), GpuMode::On);
        assert_eq!(GpuMode::from_token("force"), GpuMode::On);
        assert_eq!(GpuMode::from_token("Auto"), GpuMode::Auto);
        // Unknown/empty fall back to Off (no probe → no device allocation).
        assert_eq!(GpuMode::from_token(""), GpuMode::Off);
        assert_eq!(GpuMode::from_token("nonsense"), GpuMode::Off);
    }

    #[test]
    fn should_probe_only_when_not_off() {
        assert!(!should_probe(GpuMode::Off));
        assert!(should_probe(GpuMode::Auto));
        assert!(should_probe(GpuMode::On));
    }

    #[test]
    fn letterbox_rect_centres_and_keeps_4_3() {
        // Exactly 4:3 → fills the whole drawable, no bars.
        assert_eq!(letterbox_rect(640, 480, 4, 3), (0, 0, 640, 480));
        // Wider than 4:3 → pillarbox: full height, centred horizontally.
        // 4:3 at height 480 is 640 wide; in an 800-wide window x = (800-640)/2 = 80.
        assert_eq!(letterbox_rect(800, 480, 4, 3), (80, 0, 640, 480));
        // Taller than 4:3 → letterbox: full width, centred vertically.
        // 4:3 at width 640 is 480 tall; in a 600-tall window y = (600-480)/2 = 60.
        assert_eq!(letterbox_rect(640, 600, 4, 3), (0, 60, 640, 480));
        // Degenerate windows are a no-op rather than a divide-by-zero.
        assert_eq!(letterbox_rect(0, 480, 4, 3), (0, 0, 0, 0));
        assert_eq!(letterbox_rect(640, 0, 4, 3), (0, 0, 0, 0));
    }

    #[test]
    fn test_pattern_fills_opaque_rgba_and_animates() {
        let (w, h) = (320usize, 224usize);
        let mut a = vec![0u8; w * h * 4];
        let mut b = vec![0u8; w * h * 4];
        fill_test_pattern(&mut a, w, h, 0);
        fill_test_pattern(&mut b, w, h, 100);
        // Every pixel is fully opaque (alpha = 255).
        assert!(a.chunks_exact(4).all(|px| px[3] == 255));
        // The sweeping bar moves, so frame 0 and frame 100 differ — proves the
        // presenter would show live motion, not a static blit.
        assert_ne!(a, b);
    }

    #[test]
    fn os_maps_to_the_expected_shader_format() {
        assert_eq!(map_os("windows"), ShaderKind::Dxil);
        assert_eq!(map_os("macos"), ShaderKind::Msl);
        assert_eq!(map_os("ios"), ShaderKind::Msl);
        assert_eq!(map_os("linux"), ShaderKind::Spirv);
        assert_eq!(map_os("freebsd"), ShaderKind::Spirv);
        // The host build resolves through the same table.
        assert_eq!(ShaderKind::for_target(), map_os(std::env::consts::OS));
    }

    #[test]
    fn shader_kind_maps_to_the_sdl_create_property_key() {
        // The SDL_GPU device-create boolean keys (stable ABI), one per format.
        assert_eq!(
            ShaderKind::Spirv.create_prop_key(),
            "SDL.gpu.device.create.shaders.spirv"
        );
        assert_eq!(
            ShaderKind::Dxil.create_prop_key(),
            "SDL.gpu.device.create.shaders.dxil"
        );
        assert_eq!(
            ShaderKind::Msl.create_prop_key(),
            "SDL.gpu.device.create.shaders.msl"
        );
    }
}
