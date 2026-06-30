//! Per-port controller-input assignment (sdl-free, unit-tested).
//!
//! Each of the two SMPC controller ports is assigned an input [`Source`] — the
//! keyboard, the host mouse, a specific game controller, or nothing. The OSD
//! Controller screen cycles each port through the available sources; the
//! frontend (`main.rs`) owns the live SDL plumbing (gathering held buttons per
//! device) and routes each port's pad/mouse from its assigned source.
//!
//! Game controllers are identified by their stable SDL **GUID** (so an
//! assignment persists across launches) plus a display name (for the OSD). Two
//! *identical* controllers share a GUID, so they disambiguate by connection
//! order, not individually — a known SDL limitation. Multiple physical
//! keyboards are **not** distinguished (the binding offers no enumeration); they
//! merge into the single `Keyboard` source.
//!
//! All decision logic here is pure and unit-tested; only the SDL event/device
//! glue in `main.rs` is untested-by-construction.

use saturn::smpc::PortDevice;

/// A connected game controller as the frontend knows it: its stable GUID (config
/// identity) and display name (OSD label).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pad {
    pub guid: String,
    pub name: String,
}

/// What drives one controller port.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Source {
    /// Nothing plugged in (the port reports "no peripheral").
    #[default]
    None,
    /// The host keyboard (the rebindable pad keymap).
    Keyboard,
    /// The host mouse (Shuttle Mouse).
    Mouse,
    /// A specific game controller, by stable GUID + display name.
    Gamepad { guid: String, name: String },
}

impl Source {
    /// Short label for the OSD row, e.g. `Keyboard`, `Mouse`, `Pad: Xbox 360`.
    pub fn label(&self) -> String {
        match self {
            Source::None => "None".into(),
            Source::Keyboard => "Keyboard".into(),
            Source::Mouse => "Mouse".into(),
            // Truncate long controller names so the OSD panel doesn't overflow.
            Source::Gamepad { name, .. } => {
                let short: String = name.chars().take(14).collect();
                format!("Pad: {short}")
            }
        }
    }

    /// The SMPC device this source presents on its port.
    pub fn device(&self) -> PortDevice {
        match self {
            Source::None => PortDevice::None,
            Source::Mouse => PortDevice::Mouse,
            Source::Keyboard | Source::Gamepad { .. } => PortDevice::Pad,
        }
    }

    /// Serialize to a flat config token: `none` / `keyboard` / `mouse` /
    /// `gamepad:<guid>`.
    pub fn to_token(&self) -> String {
        match self {
            Source::None => "none".into(),
            Source::Keyboard => "keyboard".into(),
            Source::Mouse => "mouse".into(),
            Source::Gamepad { guid, .. } => format!("gamepad:{guid}"),
        }
    }

    /// Parse a config token, resolving a `gamepad:<guid>` against the currently
    /// connected pads (an unmatched GUID — that controller isn't plugged in —
    /// becomes [`Source::None`]).
    pub fn from_token(tok: &str, pads: &[Pad]) -> Source {
        let tok = tok.trim();
        match tok {
            "keyboard" => Source::Keyboard,
            "mouse" => Source::Mouse,
            _ if tok.starts_with("gamepad:") => {
                let guid = &tok["gamepad:".len()..];
                pads.iter()
                    .find(|p| p.guid == guid)
                    .map(|p| Source::Gamepad {
                        guid: p.guid.clone(),
                        name: p.name.clone(),
                    })
                    .unwrap_or(Source::None)
            }
            _ => Source::None, // "none" or anything unrecognized
        }
    }
}

/// The two-port input assignment.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ports {
    pub source: [Source; 2],
}

impl Ports {
    /// The default layout: keyboard on port 1, nothing on port 2 — the original
    /// single-player behaviour.
    pub fn default_layout() -> Self {
        Ports {
            source: [Source::Keyboard, Source::None],
        }
    }

    /// Build from the two config tokens, resolving gamepad GUIDs against `pads`.
    /// Both tokens empty (a config with no port keys that also lacked the legacy
    /// `mouse` key) falls back to [`Ports::default_layout`].
    pub fn from_tokens(p1: &str, p2: &str, pads: &[Pad]) -> Self {
        if p1.trim().is_empty() && p2.trim().is_empty() {
            return Self::default_layout();
        }
        Ports {
            source: [Source::from_token(p1, pads), Source::from_token(p2, pads)],
        }
    }

    /// The ordered candidate sources a port can cycle through, given the
    /// connected `pads`: None → Keyboard → Mouse → each pad in turn.
    fn candidates(pads: &[Pad]) -> Vec<Source> {
        let mut v = vec![Source::None, Source::Keyboard, Source::Mouse];
        v.extend(pads.iter().map(|p| Source::Gamepad {
            guid: p.guid.clone(),
            name: p.name.clone(),
        }));
        v
    }

    /// Advance port `p` to the next candidate source, skipping any source that
    /// the *other* port already uses (a device drives at most one port). Wraps.
    pub fn cycle(&mut self, p: usize, pads: &[Pad]) {
        let other = self.source[1 - p].clone();
        let cands = Self::candidates(pads);
        let cur = cands.iter().position(|c| *c == self.source[p]).unwrap_or(0);
        // Walk forward to the next candidate that isn't taken by the other port
        // (None is always available — two ports may both be empty).
        for step in 1..=cands.len() {
            let cand = &cands[(cur + step) % cands.len()];
            if *cand == Source::None || *cand != other {
                self.source[p] = cand.clone();
                return;
            }
        }
    }

    /// Compute the `(port-1, port-2)` digital-pad bitmasks for one frame from the
    /// host keyboard bits and the per-controller bits (keyed by GUID). A port
    /// bound to [`Source::Keyboard`] receives the keyboard bits; a port bound to a
    /// specific [`Source::Gamepad`] receives that controller's bits;
    /// [`Source::Mouse`]/[`Source::None`] ports contribute no pad bits. Any
    /// connected controller **not** explicitly bound to a port merges into the
    /// "default" pad port — the keyboard port if there is one, else the first
    /// pad-device port — so a spare controller still drives single-player play
    /// (the pre-0.18 "any pad drives port 1" behaviour). Pure (no SDL, no
    /// Saturn-bit knowledge — just OR-ing the host-side `u16` masks).
    pub fn route(&self, keyboard: u16, pads: &[(String, u16)]) -> (u16, u16) {
        let bound = |guid: &str| {
            self.source
                .iter()
                .any(|s| matches!(s, Source::Gamepad { guid: g, .. } if g == guid))
        };
        let mut bits = [0u16; 2];
        for (i, s) in self.source.iter().enumerate() {
            match s {
                Source::Keyboard => bits[i] |= keyboard,
                Source::Gamepad { guid, .. } => {
                    if let Some((_, b)) = pads.iter().find(|(g, _)| g == guid) {
                        bits[i] |= *b;
                    }
                }
                Source::Mouse | Source::None => {}
            }
        }
        let merge_target = (0..2)
            .find(|&i| self.source[i] == Source::Keyboard)
            .or_else(|| (0..2).find(|&i| self.source[i].device() == PortDevice::Pad));
        if let Some(t) = merge_target {
            for (g, b) in pads {
                if !bound(g) {
                    bits[t] |= *b;
                }
            }
        }
        (bits[0], bits[1])
    }

    /// The 1-based index of the port presenting the Shuttle Mouse, if any — the
    /// SDL thread's pointer-capture gate follows this.
    pub fn mouse_port(&self) -> Option<u8> {
        self.source
            .iter()
            .position(|s| *s == Source::Mouse)
            .map(|i| i as u8 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pads() -> Vec<Pad> {
        vec![
            Pad {
                guid: "AAAA".into(),
                name: "Xbox 360".into(),
            },
            Pad {
                guid: "BBBB".into(),
                name: "DualShock 4".into(),
            },
        ]
    }

    #[test]
    fn default_layout_is_keyboard_then_none() {
        let p = Ports::default_layout();
        assert_eq!(p.source[0], Source::Keyboard);
        assert_eq!(p.source[1], Source::None);
    }

    #[test]
    fn device_maps_source_to_smpc_port_device() {
        assert_eq!(Source::None.device(), PortDevice::None);
        assert_eq!(Source::Keyboard.device(), PortDevice::Pad);
        assert_eq!(Source::Mouse.device(), PortDevice::Mouse);
        assert_eq!(
            Source::Gamepad {
                guid: "AAAA".into(),
                name: "X".into()
            }
            .device(),
            PortDevice::Pad
        );
    }

    #[test]
    fn token_round_trip_resolves_gamepad_by_guid() {
        let pads = pads();
        for s in [
            Source::None,
            Source::Keyboard,
            Source::Mouse,
            Source::Gamepad {
                guid: "BBBB".into(),
                name: "DualShock 4".into(),
            },
        ] {
            let back = Source::from_token(&s.to_token(), &pads);
            assert_eq!(back, s, "round trip for {s:?}");
        }
    }

    #[test]
    fn from_token_unplugged_gamepad_falls_back_to_none() {
        // GUID CCCC isn't connected → None.
        assert_eq!(Source::from_token("gamepad:CCCC", &pads()), Source::None);
    }

    #[test]
    fn cycle_walks_none_keyboard_mouse_then_each_pad() {
        let pads = pads();
        let mut p = Ports {
            source: [Source::None, Source::None],
        };
        let order = [
            Source::Keyboard,
            Source::Mouse,
            Source::Gamepad {
                guid: "AAAA".into(),
                name: "Xbox 360".into(),
            },
            Source::Gamepad {
                guid: "BBBB".into(),
                name: "DualShock 4".into(),
            },
            Source::None, // wraps
        ];
        for expect in order {
            p.cycle(0, &pads);
            assert_eq!(p.source[0], expect);
        }
    }

    #[test]
    fn cycle_skips_a_source_the_other_port_already_uses() {
        let pads = pads();
        // Port 0 = Keyboard; cycling port 1 from None must skip Keyboard.
        let mut p = Ports {
            source: [Source::Keyboard, Source::None],
        };
        p.cycle(1, &pads); // None → (skip Keyboard) → Mouse
        assert_eq!(p.source[1], Source::Mouse);
        // Port 0 = Xbox; cycling port 1 must skip that specific pad.
        let mut p = Ports {
            source: [
                Source::Gamepad {
                    guid: "AAAA".into(),
                    name: "Xbox 360".into(),
                },
                Source::Mouse,
            ],
        };
        p.cycle(1, &pads); // Mouse → (skip Xbox AAAA) → DualShock BBBB
        assert_eq!(
            p.source[1],
            Source::Gamepad {
                guid: "BBBB".into(),
                name: "DualShock 4".into()
            }
        );
    }

    #[test]
    fn both_ports_may_be_none() {
        let pads = pads();
        // Cycling a port all the way around lands back on None even though the
        // other port is also None (None is never "taken").
        let mut p = Ports {
            source: [Source::None, Source::None],
        };
        for _ in 0..5 {
            p.cycle(0, &pads);
        }
        assert_eq!(p.source[0], Source::None);
    }

    #[test]
    fn from_tokens_resolves_each_port_and_defaults_when_empty() {
        let pads = pads();
        // Tokens resolve per port; a connected gamepad GUID reattaches.
        let p = Ports::from_tokens("gamepad:AAAA", "mouse", &pads);
        assert_eq!(
            p.source[0],
            Source::Gamepad {
                guid: "AAAA".into(),
                name: "Xbox 360".into()
            }
        );
        assert_eq!(p.source[1], Source::Mouse);
        // Both empty → the default single-player layout.
        assert_eq!(Ports::from_tokens("", "", &pads), Ports::default_layout());
        // An unplugged gamepad GUID falls back to None.
        assert_eq!(
            Ports::from_tokens("gamepad:ZZZZ", "none", &pads).source[0],
            Source::None
        );
    }

    fn gp(guid: &str, bits: u16) -> (String, u16) {
        (guid.to_string(), bits)
    }

    #[test]
    fn route_keyboard_port_gets_keyboard_bits() {
        let p = Ports {
            source: [Source::Keyboard, Source::None],
        };
        assert_eq!(p.route(0x0042, &[]), (0x0042, 0));
    }

    #[test]
    fn route_bound_gamepad_goes_to_its_port_only() {
        let p = Ports {
            source: [
                Source::Gamepad {
                    guid: "AAAA".into(),
                    name: "Xbox 360".into(),
                },
                Source::Keyboard,
            ],
        };
        // Port 0 = pad AAAA, port 1 = keyboard; each gets only its own source.
        assert_eq!(p.route(0x0001, &[gp("AAAA", 0x0080)]), (0x0080, 0x0001));
    }

    #[test]
    fn route_unbound_gamepad_merges_into_the_keyboard_port() {
        // Default layout: a spare (unassigned) pad still drives the keyboard
        // port — the pre-0.18 "any pad drives port 1" convenience.
        let p = Ports::default_layout(); // [Keyboard, None]
        assert_eq!(p.route(0x0001, &[gp("AAAA", 0x0080)]), (0x0081, 0));
    }

    #[test]
    fn route_unbound_gamepad_falls_to_first_pad_port_when_no_keyboard() {
        // Port 0 = pad AAAA, port 1 = None; an unbound pad merges into port 0
        // (the first pad-device port) since neither port is the keyboard.
        let p = Ports {
            source: [
                Source::Gamepad {
                    guid: "AAAA".into(),
                    name: "Xbox 360".into(),
                },
                Source::None,
            ],
        };
        assert_eq!(
            p.route(0, &[gp("AAAA", 0x0010), gp("BBBB", 0x0020)]),
            (0x0030, 0)
        );
    }

    #[test]
    fn route_mouse_and_none_ports_get_no_pad_bits() {
        let p = Ports {
            source: [Source::Mouse, Source::None],
        };
        // No keyboard / pad-device port, so even an unbound pad is dropped.
        assert_eq!(p.route(0xFFFF, &[gp("AAAA", 0x0080)]), (0, 0));
    }

    #[test]
    fn mouse_port_reports_the_one_based_index() {
        assert_eq!(Ports::default_layout().mouse_port(), None);
        assert_eq!(
            Ports {
                source: [Source::Mouse, Source::Keyboard]
            }
            .mouse_port(),
            Some(1)
        );
        assert_eq!(
            Ports {
                source: [Source::Keyboard, Source::Mouse]
            }
            .mouse_port(),
            Some(2)
        );
    }

    #[test]
    fn label_truncates_long_pad_names() {
        let s = Source::Gamepad {
            guid: "AAAA".into(),
            name: "A Very Long Controller Name".into(),
        };
        assert_eq!(s.label(), "Pad: A Very Long Co");
        assert_eq!(Source::Keyboard.label(), "Keyboard");
    }
}
