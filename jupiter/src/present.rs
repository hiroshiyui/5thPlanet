//! Graphics-presentation backend selection for the SDL2 frontend.
//!
//! `jupiter` does **not** render the Saturn picture — the VDP1/VDP2 compositor
//! in `saturn` produces the finished RGBA framebuffer *in software* (the
//! accuracy-first core; the GPU is for presentation only). This module only
//! chooses *how that 2D frame is blitted to the window*. SDL2's 2D renderer
//! already abstracts the GPU backend — OpenGL, Direct3D, Metal, and software are
//! all SDL2 render *drivers* — so "selecting a backend" here means picking the
//! `SDL_RENDER_DRIVER` SDL2 uses, with a fallback chain.
//!
//! Note: SDL2's 2D renderer has **no Vulkan driver** (the `vulkan` token is
//! accepted but maps to [`RenderBackend::Auto`]); Vulkan would need a separate
//! `wgpu` backend, which is out of scope for this strategy.
//!
//! The driver must be chosen *before* the renderer is created
//! (`Window::into_canvas` reads the hint), so [`build_canvas`] sets the hint and
//! builds the canvas together, retrying the next candidate if creation fails.

/// A requested presentation backend, parsed from the `backend` config key /
/// `--backend` flag and mapped to an SDL2 render-driver name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderBackend {
    /// Let SDL2 use its own platform-default driver order (no hint set).
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
    /// `vulkan`, which the SDL2 2D renderer cannot provide — fall back to
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
                     (the SDL2 2D renderer has no Vulkan driver); using auto"
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

    /// The SDL2 `SDL_RENDER_DRIVER` name this backend requests, or `None` for
    /// [`Auto`](RenderBackend::Auto) (no hint — SDL2 uses its own order).
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

    /// The ordered SDL2 driver names to try for this preference: the requested
    /// driver first, then OpenGL, then software — so a host missing the preferred
    /// GPU API still gets a working window. [`Auto`](RenderBackend::Auto) yields
    /// an empty chain (let SDL2 pick its default).
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
/// `available` in this SDL2 build: the preference chain filtered to what's
/// available. **Pure** (no SDL), so the fallback policy is unit-testable. An
/// empty result means "let SDL2 choose its default" (the [`Auto`] case, or a
/// preference whose whole chain is unavailable).
///
/// [`Auto`]: RenderBackend::Auto
fn candidates(pref: RenderBackend, available: &[&str]) -> Vec<&'static str> {
    pref.preference_chain()
        .into_iter()
        .filter(|d| available.iter().any(|a| a == d))
        .collect()
}

/// Build the SDL2 window + canvas, selecting the render driver per `pref` with a
/// fallback chain. Returns the canvas and the SDL2 driver name actually in use,
/// queried from the created renderer via `Canvas::info` so `auto` (which sets no
/// hint) resolves to a real name like `opengl`, and a fallback shows what truly
/// loaded rather than what was asked for. The `SDL_RENDER_DRIVER` hint
/// is set before each `into_canvas`, and the window is rebuilt per attempt
/// because `into_canvas` consumes it. Panics only if even SDL2's default canvas
/// cannot be created — an unrecoverable video state.
#[cfg(feature = "sdl2-frontend")]
pub fn build_canvas(
    video: &sdl2::VideoSubsystem,
    title: &str,
    width: u32,
    height: u32,
    pref: RenderBackend,
) -> (sdl2::render::WindowCanvas, &'static str) {
    let available: Vec<&str> = sdl2::render::drivers().map(|d| d.name).collect();
    // Each available driver from the preference chain, then SDL2's default (no
    // hint) as the ultimate fallback.
    let attempts = candidates(pref, &available)
        .into_iter()
        .map(Some)
        .chain(std::iter::once(None));
    for cand in attempts {
        if let Some(driver) = cand {
            sdl2::hint::set("SDL_RENDER_DRIVER", driver);
        }
        let Ok(window) = video.window(title, width, height).position_centered().build() else {
            continue;
        };
        if let Ok(canvas) = window.into_canvas().present_vsync().build() {
            // Ask the created renderer which driver it actually is — for `auto`
            // (no hint) this is the only way to learn the resolved backend, and
            // after a fallback it confirms what truly loaded, not just the ask.
            let used = canvas.info().name;
            return (canvas, used);
        }
    }
    panic!("could not create any SDL2 renderer (no working video driver)");
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
        // Auto sets no hint regardless of what's available — SDL2 picks.
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
        // Nothing in the chain available -> empty -> caller uses SDL2 default.
        assert!(candidates(RenderBackend::OpenGl, &["direct3d11"]).is_empty());
    }
}
