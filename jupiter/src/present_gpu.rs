//! SDL_GPU capability detection — groundwork for the planned CRT-shader
//! presenter (ADR-0019, `shaders/README.md`).
//!
//! The Saturn picture is always composited in software (the accuracy-first core);
//! a future `SDL_GPU` presenter would only post-process the finished frame (CRT
//! filters), falling back to the plain `SDL_Renderer` blit when the host can't
//! support it. Before that presenter is wired, the frontend needs to answer one
//! question at startup: *can this machine create an SDL_GPU device for the shader
//! format we'd ship?* This module is that probe. Today it only **logs** its
//! verdict; when the presenter lands it becomes the gate the presenter consults.
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
