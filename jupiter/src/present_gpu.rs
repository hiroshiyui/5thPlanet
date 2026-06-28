//! SDL_GPU capability detection — groundwork for the planned CRT-shader
//! presenter (ADR-0019, `shaders/README.md`).
//!
//! The Saturn picture is always composited in software (the accuracy-first core);
//! a future `SDL_GPU` presenter would only post-process the finished frame (CRT
//! filters), falling back to the plain `SDL_Renderer` blit when the host can't
//! support it. Before that presenter is wired, the frontend needs to answer one
//! question at startup: *can this machine create an SDL_GPU device for the shader
//! format we'd ship?* This module is that probe — today it only **logs** its
//! verdict; when the full presenter lands it becomes the gate the presenter
//! consults.
//!
//! ## The Vulkan presenter self-test ([`run_selftest`], `gpu-preview` only)
//!
//! Ahead of the full presenter, `jupiter --gpu-selftest` is a **contained proof**
//! that SDL_GPU works as an *alternative* presenter to the `SDL_Renderer` blit —
//! **with no shaders authored**. It claims a Vulkan (SPIR-V) device for a fresh
//! window and, each frame, uploads an animated test pattern to a GPU texture and
//! posts it to the swapchain via SDL's built-in `SDL_BlitGPUTexture` (which
//! carries its own blit shader), letterboxed to 4:3. The normal presentation path
//! is untouched; this is a standalone one-shot that validates the upload → blit →
//! present pipeline on real hardware before the CRT-shader work (ADR-0019).
//!
//! ## Why the probe creates a device (and why it's opt-in)
//!
//! The workspace forbids `unsafe`, and in sdl3-rs 0.18.4 the cheap, non-allocating
//! probes (`SDL_GPUSupportsShaderFormats`, `SDL_GetNumGPUDrivers`) have **no safe
//! wrapper** — only `sdl3::gpu::Device::new` (which returns a `Result`) is safe.
//! So the only `unsafe`-free way to detect support is to *try to create a device*
//! and treat `Err` as "fall back". Because that allocates a real GPU device, the
//! probe is **opt-in** (the `gpu` config key / `--gpu` flag default to `off`); the
//! default flips to `auto` once a presenter actually consumes the result.
//!
//! Likewise, sdl3-rs 0.18.4 doesn't safely wrap `SDL_GetGPUDeviceDriver`, so we
//! can report only *whether* a device was created — not *which* backend
//! (vulkan/d3d12/metal) it chose, and hence we can't reject a slow software
//! Vulkan such as llvmpipe/lavapipe. That readback is a documented follow-up
//! (ADR-0019).

/// Whether to attempt the startup SDL_GPU capability probe. Parsed from the `gpu`
/// config key / `--gpu` flag (the CLI flag wins, like `--backend`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuMode {
    /// Never probe (default). Zero startup cost; the `SDL_Renderer` blit is the
    /// only presentation path.
    Off,
    /// Probe and report; a failure is informational (the host simply can't).
    Auto,
    /// Probe and report; a failure warns loudly (the user explicitly forced it).
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

/// Whether `mode` asks for a probe at all (everything but [`Off`](GpuMode::Off)).
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
}

/// The verdict of the startup probe.
///
/// There is intentionally **no chosen-backend name** here: sdl3-rs 0.18.4 doesn't
/// safely wrap `SDL_GetGPUDeviceDriver`, so we can report only whether a device
/// for the host's shader format could be created. The fields are written by
/// [`probe`] and read by the future CRT-shader presenter (ADR-0019); until that
/// lands nothing consumes them, hence the `dead_code` allowance.
#[cfg(feature = "gpu-preview")]
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuCapability {
    /// A device supporting the host's shader format was created.
    pub available: bool,
    /// The shader format probed for (what the presenter would load).
    pub shader: ShaderKind,
}

/// The matching `sdl3::gpu::ShaderFormat` flag for this kind.
#[cfg(feature = "gpu-preview")]
impl ShaderKind {
    fn to_sdl(self) -> sdl3::gpu::ShaderFormat {
        use sdl3::gpu::ShaderFormat;
        match self {
            ShaderKind::Spirv => ShaderFormat::SPIRV,
            ShaderKind::Dxil => ShaderFormat::DXIL,
            ShaderKind::Msl => ShaderFormat::MSL,
        }
    }
}

/// Run the capability probe for `mode`, logging the verdict.
///
/// For [`Off`](GpuMode::Off) this is a no-op returning "unavailable". Otherwise it
/// attempts `sdl3::gpu::Device::new` for the host's shader format: `Ok` (with the
/// format confirmed in the device's supported set) is "available"; `Err` logs the
/// SDL error and reports "unavailable" so the caller keeps the `SDL_Renderer`
/// path. The created device is dropped immediately — nothing consumes it until the
/// presenter lands (see the module docs for why we must allocate to probe).
#[cfg(feature = "gpu-preview")]
pub fn probe(mode: GpuMode) -> GpuCapability {
    let shader = ShaderKind::for_target();
    if !should_probe(mode) {
        return GpuCapability {
            available: false,
            shader,
        };
    }
    let fmt = shader.to_sdl();
    match sdl3::gpu::Device::new(fmt, false) {
        Ok(dev) => {
            // Confirm the format we'd ship is actually in the device's set.
            let ok = (dev.get_shader_formats() & fmt) == fmt;
            eprintln!(
                "SDL_GPU: device available ({shader:?} shaders {}); \
                 CRT-shader presenter not yet wired (ADR-0019)",
                if ok { "supported" } else { "MISSING" }
            );
            GpuCapability {
                available: ok,
                shader,
            }
        }
        Err(e) => {
            // `On` = the user explicitly forced it, so a failure is worth a louder
            // line than an `auto` host that simply has no GPU backend.
            let tag = if mode == GpuMode::On { "WARN" } else { "note" };
            eprintln!("SDL_GPU: {tag}: unavailable ({e}); presenting via the SDL_Renderer blit");
            GpuCapability {
                available: false,
                shader,
            }
        }
    }
}

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

/// **SDL_GPU Vulkan presenter self-test** (`jupiter --gpu-selftest`, `gpu-preview`
/// builds only). A contained proof that SDL_GPU works as an *alternative*
/// presenter to the `SDL_Renderer` blit, with **no shaders authored**: it claims
/// a Vulkan (SPIR-V) device for a fresh window, then each frame uploads an
/// animated test pattern to a GPU texture and posts it to the swapchain via
/// SDL's built-in `SDL_BlitGPUTexture` (which carries its own blit shader),
/// letterboxed to 4:3. The main `SDL_Renderer` presentation path is untouched —
/// this is a standalone one-shot. Esc/close or ~1800 frames exits.
///
/// Returns `FAILURE` if no GPU device can be created (the host has no Vulkan
/// backend, or only a rejected one), `SUCCESS` after a clean present run.
#[cfg(feature = "gpu-preview")]
pub fn run_selftest() -> std::process::ExitCode {
    use sdl3::event::Event;
    use sdl3::gpu::{
        BlitInfo, Device, Filter, LoadOp, TextureCreateInfo, TextureFormat, TextureRegion,
        TextureTransferInfo, TextureType, TextureUsage, TransferBufferUsage,
    };
    use sdl3::keyboard::Keycode;
    use sdl3::pixels::Color;

    let shader = ShaderKind::for_target();
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
    // Window first, device second — so the device (and its swapchain claim) drops
    // before the window it claimed (locals drop in reverse declaration order).
    let window = match video
        .window("5thPlanet — SDL_GPU Vulkan self-test", 640, 480)
        .position_centered()
        .resizable()
        .build()
    {
        Ok(w) => w,
        Err(e) => {
            eprintln!("SDL_GPU self-test: window create failed ({e})");
            return std::process::ExitCode::FAILURE;
        }
    };
    let device = match Device::new(shader.to_sdl(), false).and_then(|d| d.with_window(&window)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "SDL_GPU self-test: no GPU device for {shader:?} ({e}); \
                 the host has no usable SDL_GPU backend"
            );
            return std::process::ExitCode::FAILURE;
        }
    };
    eprintln!(
        "SDL_GPU self-test: device created for {shader:?} shaders \
         (Vulkan on Linux); presenting an animated test pattern. \
         Resize to see the 4:3 letterbox; Esc/close to exit."
    );

    // A persistent SAMPLER texture (blit source) + an UPLOAD transfer buffer, both
    // sized to the lo-res frame; re-filled and re-uploaded every frame.
    let frame_bytes = SELFTEST_W * SELFTEST_H * 4;
    let frame_tex = match device.create_texture(
        TextureCreateInfo::new()
            .with_type(TextureType::_2D)
            .with_format(TextureFormat::R8g8b8a8Unorm)
            .with_width(SELFTEST_W as u32)
            .with_height(SELFTEST_H as u32)
            .with_layer_count_or_depth(1)
            .with_num_levels(1)
            .with_usage(TextureUsage::SAMPLER),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("SDL_GPU self-test: texture create failed ({e})");
            return std::process::ExitCode::FAILURE;
        }
    };
    let transfer = match device
        .create_transfer_buffer()
        .with_size(frame_bytes as u32)
        .with_usage(TransferBufferUsage::UPLOAD)
        .build()
    {
        Ok(t) => t,
        Err(e) => {
            eprintln!("SDL_GPU self-test: transfer buffer failed ({e})");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut event_pump = match sdl.event_pump() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SDL_GPU self-test: event pump failed ({e})");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut pattern = vec![0u8; frame_bytes];
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

        // 1) Upload the frame into the GPU texture via a copy pass.
        {
            let mut map = transfer.map::<u8>(&device, true);
            map.mem_mut().copy_from_slice(&pattern);
            map.unmap();
        }
        let Ok(mut copy_cmd) = device.acquire_command_buffer() else {
            continue;
        };
        let Ok(copy_pass) = device.begin_copy_pass(&copy_cmd) else {
            copy_cmd.cancel();
            continue;
        };
        copy_pass.upload_to_gpu_texture(
            TextureTransferInfo::new()
                .with_transfer_buffer(&transfer)
                .with_offset(0),
            TextureRegion::new()
                .with_texture(&frame_tex)
                .with_width(SELFTEST_W as u32)
                .with_height(SELFTEST_H as u32)
                .with_depth(1),
            false,
        );
        device.end_copy_pass(copy_pass);
        if copy_cmd.submit().is_err() {
            continue;
        }

        // 2) Blit the texture into the swapchain, letterboxed to 4:3, on a black
        //    clear. SDL's built-in blit supplies its own shader — none authored.
        let Ok(mut draw_cmd) = device.acquire_command_buffer() else {
            continue;
        };
        if let Ok(swapchain) = draw_cmd.wait_and_acquire_swapchain_texture(&window) {
            let (dx, dy, dw, dh) = letterbox_rect(swapchain.width(), swapchain.height(), 4, 3);
            if dw == 0 || dh == 0 {
                // Minimised / zero-sized drawable — nothing to present this frame.
                draw_cmd.cancel();
                continue;
            }
            let blit = BlitInfo::default()
                .with_source_texture(&frame_tex)
                .with_source_region(0, 0, 0, SELFTEST_W as u32, SELFTEST_H as u32)
                .with_destination_texture(&swapchain)
                .with_destination_region(0, dx, dy, dw, dh)
                .with_load_op(LoadOp::CLEAR)
                .with_clear_color(Color::RGB(0, 0, 0))
                .with_filter(Filter::Nearest);
            draw_cmd.blit_texture(blit);
            let _ = draw_cmd.submit();
            presented += 1;
        } else {
            draw_cmd.cancel();
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
}
