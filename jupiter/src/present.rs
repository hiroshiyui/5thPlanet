//! Graphics-presentation backend selection for the SDL3 frontend.
//!
//! `jupiter` does **not** render the Saturn picture — the VDP1/VDP2 compositor
//! in `saturn` produces the finished RGBA framebuffer *in software* (the
//! accuracy-first core; the GPU is for presentation only). This module only
//! chooses *how that 2D frame is blitted to the window*. SDL3's 2D renderer
//! already abstracts the GPU backend — OpenGL, Direct3D, Metal, and software are
//! all SDL3 render *drivers* — so "selecting a backend" here means picking the
//! `SDL_RENDER_DRIVER` SDL3 uses, with a fallback chain.
//!
//! Note: SDL3's 2D renderer has **no Vulkan driver** (the `vulkan` token is
//! accepted but maps to [`RenderBackend::Auto`]); Vulkan would need a separate
//! `wgpu` backend, which is out of scope for this strategy.
//!
//! The driver must be chosen *before* the renderer is created
//! (`Window::into_canvas` reads the hint), so [`build_canvas`] sets the hint and
//! builds the canvas together, retrying the next candidate if creation fails.

/// A requested presentation backend, parsed from the `backend` config key /
/// `--backend` flag and mapped to an SDL3 render-driver name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderBackend {
    /// Let SDL3 use its own platform-default driver order (no hint set).
    Auto,
    OpenGl,
    OpenGlEs,
    Direct3D11,
    Direct3D12,
    Metal,
    Software,
}

impl RenderBackend {
    /// Parse a config/CLI token (case-insensitive). Unknown tokens — including
    /// `vulkan`, which the SDL3 2D renderer cannot provide — fall back to
    /// [`RenderBackend::Auto`] with a warning, so a stale or wishful config never
    /// blocks startup.
    pub fn from_token(tok: &str) -> Self {
        match tok.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => RenderBackend::Auto,
            "opengl" | "gl" => RenderBackend::OpenGl,
            "opengles" | "opengles2" | "gles" => RenderBackend::OpenGlEs,
            "direct3d" | "direct3d11" | "d3d11" | "d3d" => RenderBackend::Direct3D11,
            "direct3d12" | "d3d12" => RenderBackend::Direct3D12,
            "metal" => RenderBackend::Metal,
            "software" | "sw" => RenderBackend::Software,
            other => {
                eprintln!(
                    "render backend: unknown/unsupported backend {other:?} \
                     (the SDL3 2D renderer has no Vulkan driver); using auto"
                );
                RenderBackend::Auto
            }
        }
    }

    /// The canonical config token for this backend (inverse of [`from_token`]).
    ///
    /// [`from_token`]: RenderBackend::from_token
    pub fn to_token(self) -> &'static str {
        match self {
            RenderBackend::Auto => "auto",
            RenderBackend::OpenGl => "opengl",
            RenderBackend::OpenGlEs => "opengles",
            RenderBackend::Direct3D11 => "direct3d11",
            RenderBackend::Direct3D12 => "direct3d12",
            RenderBackend::Metal => "metal",
            RenderBackend::Software => "software",
        }
    }

    /// The SDL3 `SDL_RENDER_DRIVER` name this backend requests, or `None` for
    /// [`Auto`](RenderBackend::Auto) (no hint — SDL3 uses its own order).
    fn driver_name(self) -> Option<&'static str> {
        Some(match self {
            RenderBackend::Auto => return None,
            RenderBackend::OpenGl => "opengl",
            RenderBackend::OpenGlEs => "opengles2",
            RenderBackend::Direct3D11 => "direct3d11",
            RenderBackend::Direct3D12 => "direct3d12",
            RenderBackend::Metal => "metal",
            RenderBackend::Software => "software",
        })
    }

    /// The ordered SDL3 driver names to try for this preference: the requested
    /// driver first, then OpenGL, then software — so a host missing the preferred
    /// GPU API still gets a working window. [`Auto`](RenderBackend::Auto) yields
    /// an empty chain (let SDL3 pick its default).
    fn preference_chain(self) -> Vec<&'static str> {
        match self.driver_name() {
            None => Vec::new(),
            Some(name) => {
                let mut chain = vec![name];
                for fallback in ["opengl", "software"] {
                    if !chain.contains(&fallback) {
                        chain.push(fallback);
                    }
                }
                chain
            }
        }
    }
}

/// The ordered driver candidates to attempt for `pref`, given the drivers
/// `available` in this SDL3 build: the preference chain filtered to what's
/// available. **Pure** (no SDL), so the fallback policy is unit-testable. An
/// empty result means "let SDL3 choose its default" (the [`Auto`] case, or a
/// preference whose whole chain is unavailable).
///
/// [`Auto`]: RenderBackend::Auto
fn candidates(pref: RenderBackend, available: &[&str]) -> Vec<&'static str> {
    pref.preference_chain()
        .into_iter()
        .filter(|d| available.iter().any(|a| a == d))
        .collect()
}

/// Build the SDL3 window + canvas, selecting the render driver per `pref` with a
/// fallback chain. Returns the canvas and the driver name actually in use (the
/// canvas's `renderer_name` field, so `auto` — which sets no hint — resolves to
/// a real name like `opengl`). The `SDL_RENDER_DRIVER` hint is set before
/// `into_canvas`. Candidates are pre-filtered to drivers SDL3 reports available,
/// so the chosen one should create successfully; `into_canvas` is **infallible**
/// in sdl3-rs (it panics on a renderer it cannot make), so there is no
/// per-candidate renderer-creation retry — only a failed window build falls
/// through to the next candidate. Vsync is intentionally not
/// requested: this frontend is audio-paced (see `main.rs`), not vsync-paced.
#[cfg(feature = "sdl-frontend")]
pub fn build_canvas(
    video: &sdl3::VideoSubsystem,
    title: &str,
    width: u32,
    height: u32,
    pref: RenderBackend,
) -> (sdl3::render::WindowCanvas, String) {
    // sdl3-rs `drivers()` yields the driver name strings directly.
    let names: Vec<String> = sdl3::render::drivers().collect();
    let available: Vec<&str> = names.iter().map(String::as_str).collect();
    // Each available driver from the preference chain, then SDL3's default (no
    // hint) as the ultimate fallback.
    let attempts = candidates(pref, &available)
        .into_iter()
        .map(Some)
        .chain(std::iter::once(None));
    for cand in attempts {
        if let Some(driver) = cand {
            sdl3::hint::set("SDL_RENDER_DRIVER", driver);
        }
        let Ok(window) = video.window(title, width, height).position_centered().build() else {
            continue;
        };
        let canvas = window.into_canvas();
        // The driver the renderer actually created (a public field on the canvas).
        let used = canvas.renderer_name.clone();
        return (canvas, used);
    }
    panic!("could not create any SDL3 renderer (no working video driver)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trips_for_every_backend() {
        for b in [
            RenderBackend::Auto,
            RenderBackend::OpenGl,
            RenderBackend::OpenGlEs,
            RenderBackend::Direct3D11,
            RenderBackend::Direct3D12,
            RenderBackend::Metal,
            RenderBackend::Software,
        ] {
            assert_eq!(RenderBackend::from_token(b.to_token()), b);
        }
    }

    #[test]
    fn unknown_and_vulkan_tokens_fall_back_to_auto() {
        assert_eq!(RenderBackend::from_token("vulkan"), RenderBackend::Auto);
        assert_eq!(RenderBackend::from_token("nonsense"), RenderBackend::Auto);
        assert_eq!(RenderBackend::from_token(""), RenderBackend::Auto);
        // Aliases + case-insensitivity.
        assert_eq!(RenderBackend::from_token("GL"), RenderBackend::OpenGl);
        assert_eq!(RenderBackend::from_token("D3D"), RenderBackend::Direct3D11);
    }

    #[test]
    fn auto_yields_no_candidates() {
        // Auto sets no hint regardless of what's available — SDL3 picks.
        assert!(candidates(RenderBackend::Auto, &["opengl", "metal", "software"]).is_empty());
    }

    #[test]
    fn preference_is_tried_first_then_opengl_then_software() {
        let avail = ["metal", "opengl", "software"];
        assert_eq!(candidates(RenderBackend::Metal, &avail), vec!["metal", "opengl", "software"]);
        assert_eq!(candidates(RenderBackend::OpenGl, &avail), vec!["opengl", "software"]);
        // Software-first when explicitly requested.
        assert_eq!(candidates(RenderBackend::Software, &avail), vec!["software", "opengl"]);
    }

    #[test]
    fn unavailable_preference_falls_through_to_what_exists() {
        // Metal requested on a host that only has opengl/software (e.g. Linux):
        // the preferred driver is dropped, the fallbacks remain in order.
        let avail = ["opengl", "software"];
        assert_eq!(candidates(RenderBackend::Metal, &avail), vec!["opengl", "software"]);
        assert_eq!(candidates(RenderBackend::Direct3D11, &avail), vec!["opengl", "software"]);
        // Nothing in the chain available -> empty -> caller uses SDL3 default.
        assert!(candidates(RenderBackend::OpenGl, &["direct3d11"]).is_empty());
    }
}
