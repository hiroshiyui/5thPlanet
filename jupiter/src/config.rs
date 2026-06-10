//! Persisted frontend configuration (M9).
//!
//! A deliberately tiny, dependency-free flat `key = value` file — a TOML
//! subset (strings quoted, integers and booleans bare, `#` comments) — at
//! `$XDG_CONFIG_HOME/5thplanet/jupiter.toml` (fallback `~/.config/…`). Like
//! the OSD this module is **`sdl2`-free**: key bindings are stored as SDL
//! scancode *names* (strings); the frontend resolves them via
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
        for (i, key) in KEY_KEYS.iter().enumerate() {
            out.push_str(&format!("{key} = \"{}\"\n", self.keys[i]));
        }
        out
    }

    /// `$XDG_CONFIG_HOME/5thplanet/jupiter.toml`, falling back to
    /// `~/.config/5thplanet/jupiter.toml`; `None` if neither env var exists.
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("5thplanet").join("jupiter.toml"))
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
}
