//! Persisted frontend configuration (M9).
//!
//! A deliberately tiny, dependency-free flat `key = value` file — a TOML
//! subset (strings quoted, integers and booleans bare, `#` comments) — at
//! a `jupiter.toml` sitting **next to the executable** (the portable /
//! self-contained-archive location), falling back to
//! `$XDG_CONFIG_HOME/5thplanet/jupiter.toml` (then `~/.config/…`). The portable
//! file wins so a bundled archive overrides any global config — see
//! [`Config::path`]. Like the OSD this module is **`sdl2`-free**: key bindings
//! are stored as SDL scancode *names* (strings); the frontend resolves them via
//! `Scancode::from_name` at the edge.
//!
//! Precedence: a command-line flag beats the config file beats the built-in
//! default (for the region, the BIOS-filename autodetect).

use std::fs;
use std::path::PathBuf;

/// Number of Saturn digital-pad buttons the keyboard maps.
pub const PAD_BUTTONS: usize = 13;

/// Pad-button display names, in the fixed binding order used everywhere
/// (config keys, OSD rows, the frontend's pad-bit table).
// Headless builds read the config but have no keyboard to name buttons for.
#[cfg_attr(not(feature = "sdl2-frontend"), allow(dead_code))]
pub const BUTTON_NAMES: [&str; PAD_BUTTONS] = [
    "Up", "Down", "Left", "Right", "A", "B", "C", "X", "Y", "Z", "L", "R", "Start",
];

/// Default key bindings (SDL scancode names), index-matched to
/// [`BUTTON_NAMES`]: arrows = D-pad, Z/X/C = A/B/C, A/S/D = X/Y/Z,
/// Q/W = L/R, Return = Start.
pub const DEFAULT_KEYS: [&str; PAD_BUTTONS] = [
    "Up", "Down", "Left", "Right", "Z", "X", "C", "A", "S", "D", "Q", "W", "Return",
];

/// The config-file keys for the bindings, index-matched to [`BUTTON_NAMES`].
const KEY_KEYS: [&str; PAD_BUTTONS] = [
    "key_up", "key_down", "key_left", "key_right", "key_a", "key_b", "key_c", "key_x", "key_y",
    "key_z", "key_l", "key_r", "key_start",
];

/// Persisted frontend settings. `region: None` means "autodetect from the
/// BIOS filename" (the user has never picked one in the OSD).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Window scale 1..=4 (base 320×224 × N).
    pub scale: u8,
    pub fullscreen: bool,
    /// SMPC region token (`japan` / `north-america` / `europe-pal` /
    /// `asia-ntsc`); `None` = autodetect.
    pub region: Option<String>,
    /// Cartridge token, same vocabulary as `--cart=`: `none` / `ram1m` /
    /// `ram4m` / `bram`.
    pub cartridge: String,
    /// Shuttle Mouse token, same vocabulary as `--mouse[=1|2]`: `off` (no
    /// mouse, default), `1` (mouse on port 1, replacing the pad) or `2` (mouse
    /// on port 2, keyboard pad stays on port 1). The CLI flag overrides this.
    pub mouse: String,
    /// Graphics-presentation backend token, same vocabulary as `--backend`:
    /// `auto` (default — SDL2 picks its platform default), `opengl`, `opengles`,
    /// `direct3d11`, `direct3d12`, `metal`, or `software`. Selects which SDL2
    /// render driver presents the framebuffer; the CLI flag overrides this.
    pub backend: String,
    /// SDL scancode names bound to each pad button ([`BUTTON_NAMES`] order).
    pub keys: [String; PAD_BUTTONS],
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scale: 2,
            fullscreen: false,
            region: None,
            cartridge: "none".into(),
            mouse: "off".into(),
            backend: "auto".into(),
            keys: DEFAULT_KEYS.map(str::to_string),
        }
    }
}

impl Config {
    /// Parse the flat `key = value` text. Tolerant: unknown keys, blank
    /// lines, `#` comments and malformed lines are skipped, missing keys
    /// keep their defaults — an old or hand-edited file never blocks boot.
    pub fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            let unquote = |s: &str| s.trim_matches('"').to_string();
            match k {
                "scale" => {
                    if let Ok(n) = v.parse::<u8>() {
                        cfg.scale = n.clamp(1, 4);
                    }
                }
                "fullscreen" => {
                    if let Ok(b) = v.parse::<bool>() {
                        cfg.fullscreen = b;
                    }
                }
                "region" => cfg.region = Some(unquote(v)).filter(|s| !s.is_empty()),
                "cartridge" => cfg.cartridge = unquote(v),
                "mouse" => cfg.mouse = unquote(v),
                "backend" => cfg.backend = unquote(v),
                _ => {
                    if let Some(i) = KEY_KEYS.iter().position(|kk| *kk == k) {
                        let name = unquote(v);
                        if !name.is_empty() {
                            cfg.keys[i] = name;
                        }
                    }
                }
            }
        }
        cfg
    }

    /// Serialize back to the flat TOML-subset text.
    // Only the SDL2 frontend writes the config (the OSD); headless reads it.
    #[cfg_attr(not(feature = "sdl2-frontend"), allow(dead_code))]
    pub fn to_text(&self) -> String {
        let mut out = String::from("# 5thPlanet frontend configuration\n");
        out.push_str(&format!("scale = {}\n", self.scale));
        out.push_str(&format!("fullscreen = {}\n", self.fullscreen));
        if let Some(r) = &self.region {
            out.push_str(&format!("region = \"{r}\"\n"));
        }
        out.push_str(&format!("cartridge = \"{}\"\n", self.cartridge));
        out.push_str(&format!("mouse = \"{}\"\n", self.mouse));
        out.push_str(&format!("backend = \"{}\"\n", self.backend));
        for (i, key) in KEY_KEYS.iter().enumerate() {
            out.push_str(&format!("{key} = \"{}\"\n", self.keys[i]));
        }
        out
    }

    /// The XDG location: `$XDG_CONFIG_HOME/5thplanet/jupiter.toml`, falling
    /// back to `~/.config/5thplanet/jupiter.toml`; `None` if neither env var
    /// exists.
    pub fn xdg_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("5thplanet").join("jupiter.toml"))
    }

    /// The portable location: a `jupiter.toml` in the same directory as the
    /// running executable — ship it inside a self-contained archive and the
    /// config travels with the binary. `None` if the exe path is unknown.
    pub fn local_path() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.parent()?.join("jupiter.toml"))
    }

    /// The config path to use. **Portable-first:** an *existing* `jupiter.toml`
    /// beside the executable wins (so a self-contained archive's bundled config
    /// overrides any global one), then an *existing* XDG file; if neither exists
    /// yet, a fresh write goes to the portable path when the exe location is
    /// known, else the XDG path. Both [`load`](Self::load) and
    /// [`save`](Self::save) route through here, so the chosen file is the one
    /// written back to.
    pub fn path() -> Option<PathBuf> {
        Self::pick_path(Self::xdg_path(), Self::local_path(), |p| p.is_file())
    }

    /// Pure path-resolution policy (filesystem probing injected via `exists`),
    /// so the precedence is unit-testable without touching real files. Portable
    /// (executable-adjacent) beats XDG; an existing file beats a fresh-write
    /// target.
    fn pick_path(
        xdg: Option<PathBuf>,
        local: Option<PathBuf>,
        exists: impl Fn(&std::path::Path) -> bool,
    ) -> Option<PathBuf> {
        if let Some(p) = &local
            && exists(p)
        {
            return local;
        }
        if let Some(p) = &xdg
            && exists(p)
        {
            return xdg;
        }
        local.or(xdg)
    }

    /// Load from [`Config::path`]; a missing or unreadable file is the default.
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    /// Persist to [`Config::path`], creating the directory. Errors are
    /// reported, not fatal — a read-only config dir shouldn't kill the session.
    #[cfg_attr(not(feature = "sdl2-frontend"), allow(dead_code))]
    pub fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(dir) = path.parent()
            && let Err(e) = fs::create_dir_all(dir)
        {
            eprintln!("config: cannot create {}: {e}", dir.display());
            return;
        }
        if let Err(e) = fs::write(&path, self.to_text()) {
            eprintln!("config: cannot write {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_text() {
        let mut cfg = Config {
            scale: 3,
            fullscreen: true,
            region: Some("europe-pal".into()),
            cartridge: "ram4m".into(),
            mouse: "2".into(),
            backend: "software".into(),
            ..Config::default()
        };
        cfg.keys[12] = "Space".into();
        assert_eq!(Config::parse(&cfg.to_text()), cfg);
    }

    #[test]
    fn missing_region_round_trips_as_autodetect() {
        let cfg = Config::default();
        assert_eq!(Config::parse(&cfg.to_text()).region, None);
    }

    #[test]
    fn parse_tolerates_junk_and_keeps_defaults() {
        let cfg = Config::parse("nonsense\nscale = banana\nunknown = 7\n# comment\nscale = 9\n");
        // 9 clamps to 4; everything else stays default.
        assert_eq!(cfg.scale, 4);
        assert_eq!(cfg, Config { scale: 4, ..Config::default() });
    }

    #[test]
    fn comments_and_spacing_are_ignored() {
        let cfg = Config::parse("  scale=1   # tiny window\n\nkey_start = \"Space\"  \n");
        assert_eq!(cfg.scale, 1);
        assert_eq!(cfg.keys[12], "Space");
        assert_eq!(cfg.keys[0], "Up");
    }

    #[test]
    fn mouse_token_parses_and_defaults_to_off() {
        assert_eq!(Config::default().mouse, "off");
        assert_eq!(Config::parse("mouse = \"2\"\n").mouse, "2");
        assert_eq!(Config::parse("mouse = \"1\"\n").mouse, "1");
        // A missing key keeps the default; main.rs treats unknown tokens as off.
        assert_eq!(Config::parse("scale = 2\n").mouse, "off");
    }

    #[test]
    fn backend_token_parses_and_defaults_to_auto() {
        assert_eq!(Config::default().backend, "auto");
        assert_eq!(Config::parse("backend = \"opengl\"\n").backend, "opengl");
        assert_eq!(Config::parse("backend = \"software\"\n").backend, "software");
        // A missing key keeps the default; present.rs maps unknown tokens to auto.
        assert_eq!(Config::parse("scale = 2\n").backend, "auto");
    }

    #[test]
    fn pick_path_prefers_existing_portable_then_xdg_then_fresh_portable() {
        let xdg = PathBuf::from("/cfg/jupiter.toml");
        let local = PathBuf::from("/app/jupiter.toml");
        let pick = |present: &[&str]| {
            let present: Vec<PathBuf> = present.iter().map(PathBuf::from).collect();
            Config::pick_path(Some(xdg.clone()), Some(local.clone()), |p| {
                present.iter().any(|q| q == p)
            })
        };
        // The portable file always wins when present, even alongside XDG —
        // a self-contained archive's bundled config overrides the global one.
        assert_eq!(pick(&["/cfg/jupiter.toml", "/app/jupiter.toml"]), Some(local.clone()));
        assert_eq!(pick(&["/app/jupiter.toml"]), Some(local.clone()));
        // Only the XDG file exists -> use it.
        assert_eq!(pick(&["/cfg/jupiter.toml"]), Some(xdg.clone()));
        // Neither exists yet -> a fresh write targets the portable path.
        assert_eq!(pick(&[]), Some(local));
    }

    #[test]
    fn pick_path_falls_back_to_xdg_when_no_portable() {
        let xdg = PathBuf::from("/cfg/jupiter.toml");
        // No exe location at all -> XDG path, whether or not it exists yet.
        assert_eq!(Config::pick_path(Some(xdg.clone()), None, |_| false), Some(xdg.clone()));
        assert_eq!(Config::pick_path(Some(xdg.clone()), None, |_| true), Some(xdg));
        // Nothing resolvable -> None.
        assert_eq!(Config::pick_path(None, None, |_| true), None);
    }

    /// The committed `jupiter.toml.example` must stay parseable and document
    /// the real defaults — with `region` commented out it parses to exactly
    /// `Config::default()`. Guards the sample against drifting from the parser.
    #[test]
    fn example_file_parses_to_defaults() {
        let text = include_str!("../jupiter.toml.example");
        assert_eq!(Config::parse(text), Config::default());
    }
}
