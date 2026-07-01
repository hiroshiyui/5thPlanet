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
    /// A specific game controller, by stable GUID + display name. `analog` =
    /// present it as the 3D Control Pad (analog stick + triggers, SMPC ID 0x16);
    /// otherwise the standard digital pad (ID 0x02). Both still send the digital
    /// buttons — a 3D pad is a superset.
    Gamepad {
        guid: String,
        name: String,
        analog: bool,
    },
}

impl Source {
    /// Short label for the OSD row, e.g. `Keyboard`, `Mouse`, `Pad: Xbox 360`.
    pub fn label(&self) -> String {
        match self {
            Source::None => "None".into(),
            Source::Keyboard => "Keyboard".into(),
            Source::Mouse => "Mouse".into(),
            // Truncate long controller names so the OSD panel doesn't overflow;
            // an analog (3D) pad is tagged so the Controller screen distinguishes
            // it from the same controller in digital mode.
            Source::Gamepad { name, analog, .. } => {
                let cap = if *analog { 9 } else { 14 };
                let short: String = name.chars().take(cap).collect();
                if *analog {
                    format!("Pad: {short} (3D)")
                } else {
                    format!("Pad: {short}")
                }
            }
        }
    }

    /// The SMPC device this source presents on its port.
    pub fn device(&self) -> PortDevice {
        match self {
            Source::None => PortDevice::None,
            Source::Mouse => PortDevice::Mouse,
            Source::Keyboard => PortDevice::Pad,
            Source::Gamepad { analog, .. } => {
                if *analog {
                    PortDevice::ThreeDPad
                } else {
                    PortDevice::Pad
                }
            }
        }
    }

    /// Serialize to a flat config token: `none` / `keyboard` / `mouse` /
    /// `gamepad:<guid>` (digital) / `gamepad3d:<guid>` (analog 3D pad).
    pub fn to_token(&self) -> String {
        match self {
            Source::None => "none".into(),
            Source::Keyboard => "keyboard".into(),
            Source::Mouse => "mouse".into(),
            Source::Gamepad {
                guid,
                analog: false,
                ..
            } => format!("gamepad:{guid}"),
            Source::Gamepad {
                guid, analog: true, ..
            } => format!("gamepad3d:{guid}"),
        }
    }

    /// Parse a config token, resolving a `gamepad[3d]:<guid>` against the
    /// currently connected pads (an unmatched GUID — that controller isn't
    /// plugged in — becomes [`Source::None`]).
    pub fn from_token(tok: &str, pads: &[Pad]) -> Source {
        let tok = tok.trim();
        let resolve = |guid: &str, analog: bool| {
            pads.iter()
                .find(|p| p.guid == guid)
                .map(|p| Source::Gamepad {
                    guid: p.guid.clone(),
                    name: p.name.clone(),
                    analog,
                })
                .unwrap_or(Source::None)
        };
        match tok {
            "keyboard" => Source::Keyboard,
            "mouse" => Source::Mouse,
            // Check the 3D prefix first — "gamepad3d:" does not start with
            // "gamepad:" (the 8th char is '3', not ':'), but be explicit.
            _ if tok.starts_with("gamepad3d:") => resolve(&tok["gamepad3d:".len()..], true),
            _ if tok.starts_with("gamepad:") => resolve(&tok["gamepad:".len()..], false),
            _ => Source::None, // "none" or anything unrecognized
        }
    }

    /// Two sources are the *same physical device* if they're the same keyboard,
    /// the same mouse, or the same controller GUID (regardless of digital-vs-3D
    /// mode) — used so one device can't drive both ports. [`Source::None`] never
    /// conflicts (both ports may be empty).
    fn same_device(&self, other: &Source) -> bool {
        match (self, other) {
            (Source::None, _) | (_, Source::None) => false,
            (Source::Keyboard, Source::Keyboard) | (Source::Mouse, Source::Mouse) => true,
            (Source::Gamepad { guid: a, .. }, Source::Gamepad { guid: b, .. }) => a == b,
            _ => false,
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
    /// connected `pads`: None → Keyboard → Mouse → for each pad its digital then
    /// its 3D (analog) form.
    fn candidates(pads: &[Pad]) -> Vec<Source> {
        let mut v = vec![Source::None, Source::Keyboard, Source::Mouse];
        for p in pads {
            for analog in [false, true] {
                v.push(Source::Gamepad {
                    guid: p.guid.clone(),
                    name: p.name.clone(),
                    analog,
                });
            }
        }
        v
    }

    /// Advance port `p` to the next candidate source, skipping any source that is
    /// the same physical device as the *other* port already uses (a device drives
    /// at most one port — including across digital/3D modes of one controller).
    /// Wraps.
    pub fn cycle(&mut self, p: usize, pads: &[Pad]) {
        let other = self.source[1 - p].clone();
        let cands = Self::candidates(pads);
        let cur = cands.iter().position(|c| *c == self.source[p]).unwrap_or(0);
        // Walk forward to the next candidate the other port isn't already using
        // (None is always available — two ports may both be empty).
        for step in 1..=cands.len() {
            let cand = &cands[(cur + step) % cands.len()];
            if *cand == Source::None || !cand.same_device(&other) {
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
    pub fn route(&self, keyboard: u16, pads: &[(String, u16, [u8; 4])]) -> (u16, u16) {
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
                    if let Some((_, b, _)) = pads.iter().find(|(g, _, _)| g == guid) {
                        bits[i] |= *b;
                    }
                }
                Source::Mouse | Source::None => {}
            }
        }
        // A spare (unbound) controller's buttons merge into the keyboard port, or
        // failing that the first pad-bearing port (digital Pad *or* 3D pad).
        let is_pad_port = |i: usize| {
            matches!(
                self.source[i].device(),
                PortDevice::Pad | PortDevice::ThreeDPad
            )
        };
        let merge_target = (0..2)
            .find(|&i| self.source[i] == Source::Keyboard)
            .or_else(|| (0..2).find(|&i| is_pad_port(i)));
        if let Some(t) = merge_target {
            for (g, b, _) in pads {
                if !bound(g) {
                    bits[t] |= *b;
                }
            }
        }
        (bits[0], bits[1])
    }

    /// Per-port 3D-pad analog channels `[X, Y, L, R]` for this frame:
    /// `Some(channels)` for a port bound to an **analog** [`Source::Gamepad`]
    /// whose controller is present in `pads`, else `None` (the caller feeds the
    /// SMPC its neutral resting state). Pure.
    pub fn route_analog(&self, pads: &[(String, u16, [u8; 4])]) -> [Option<[u8; 4]>; 2] {
        std::array::from_fn(|i| match &self.source[i] {
            Source::Gamepad {
                guid, analog: true, ..
            } => pads.iter().find(|(g, _, _)| g == guid).map(|(_, _, a)| *a),
            _ => None,
        })
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

    /// A digital-mode gamepad `Source`.
    fn gpad(guid: &str, name: &str) -> Source {
        Source::Gamepad {
            guid: guid.into(),
            name: name.into(),
            analog: false,
        }
    }
    /// An analog (3D) gamepad `Source`.
    fn gpad3d(guid: &str, name: &str) -> Source {
        Source::Gamepad {
            guid: guid.into(),
            name: name.into(),
            analog: true,
        }
    }
    /// One per-controller input row for `route`/`route_analog`:
    /// `(guid, digital bits, analog [X,Y,L,R])`.
    fn gp(guid: &str, bits: u16) -> (String, u16, [u8; 4]) {
        (guid.to_string(), bits, [0x80, 0x80, 0x00, 0x00])
    }
    fn gp_a(guid: &str, bits: u16, analog: [u8; 4]) -> (String, u16, [u8; 4]) {
        (guid.to_string(), bits, analog)
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
        assert_eq!(gpad("AAAA", "X").device(), PortDevice::Pad);
        assert_eq!(gpad3d("AAAA", "X").device(), PortDevice::ThreeDPad);
    }

    #[test]
    fn token_round_trip_resolves_gamepad_by_guid_and_mode() {
        let pads = pads();
        for s in [
            Source::None,
            Source::Keyboard,
            Source::Mouse,
            gpad("BBBB", "DualShock 4"),
            gpad3d("AAAA", "Xbox 360"),
        ] {
            let back = Source::from_token(&s.to_token(), &pads);
            assert_eq!(back, s, "round trip for {s:?}");
        }
        // The 3D token is distinct from the digital one.
        assert_eq!(gpad("AAAA", "Xbox 360").to_token(), "gamepad:AAAA");
        assert_eq!(gpad3d("AAAA", "Xbox 360").to_token(), "gamepad3d:AAAA");
    }

    #[test]
    fn from_token_unplugged_gamepad_falls_back_to_none() {
        // GUID CCCC isn't connected → None (digital or 3D).
        assert_eq!(Source::from_token("gamepad:CCCC", &pads()), Source::None);
        assert_eq!(Source::from_token("gamepad3d:CCCC", &pads()), Source::None);
    }

    #[test]
    fn cycle_walks_none_keyboard_mouse_then_each_pad_digital_and_3d() {
        let pads = pads();
        let mut p = Ports {
            source: [Source::None, Source::None],
        };
        let order = [
            Source::Keyboard,
            Source::Mouse,
            gpad("AAAA", "Xbox 360"),
            gpad3d("AAAA", "Xbox 360"),
            gpad("BBBB", "DualShock 4"),
            gpad3d("BBBB", "DualShock 4"),
            Source::None, // wraps
        ];
        for expect in order {
            p.cycle(0, &pads);
            assert_eq!(p.source[0], expect);
        }
    }

    #[test]
    fn cycle_skips_the_same_physical_device_on_the_other_port() {
        let pads = pads();
        // Port 0 = Keyboard; cycling port 1 from None must skip Keyboard.
        let mut p = Ports {
            source: [Source::Keyboard, Source::None],
        };
        p.cycle(1, &pads); // None → (skip Keyboard) → Mouse
        assert_eq!(p.source[1], Source::Mouse);
        // Port 0 = Xbox (digital); cycling port 1 must skip BOTH the digital and
        // the 3D form of that same controller, landing on the DualShock.
        let mut p = Ports {
            source: [gpad("AAAA", "Xbox 360"), Source::Mouse],
        };
        p.cycle(1, &pads); // Mouse → (skip Xbox-d, Xbox-3D) → DualShock digital
        assert_eq!(p.source[1], gpad("BBBB", "DualShock 4"));
    }

    #[test]
    fn both_ports_may_be_none() {
        let pads = pads();
        // Cycling a port all the way around lands back on None even though the
        // other port is also None (None is never "taken").
        let mut p = Ports {
            source: [Source::None, Source::None],
        };
        for _ in 0..7 {
            p.cycle(0, &pads);
        }
        assert_eq!(p.source[0], Source::None);
    }

    #[test]
    fn from_tokens_resolves_each_port_and_defaults_when_empty() {
        let pads = pads();
        // Tokens resolve per port; a connected GUID reattaches, 3D mode honoured.
        let p = Ports::from_tokens("gamepad:AAAA", "gamepad3d:BBBB", &pads);
        assert_eq!(p.source[0], gpad("AAAA", "Xbox 360"));
        assert_eq!(p.source[1], gpad3d("BBBB", "DualShock 4"));
        // Both empty → the default single-player layout.
        assert_eq!(Ports::from_tokens("", "", &pads), Ports::default_layout());
        // An unplugged gamepad GUID falls back to None.
        assert_eq!(
            Ports::from_tokens("gamepad:ZZZZ", "none", &pads).source[0],
            Source::None
        );
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
            source: [gpad("AAAA", "Xbox 360"), Source::Keyboard],
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
        // Port 0 = a 3D pad AAAA, port 1 = None; an unbound pad merges into
        // port 0 (the first pad-bearing port, digital *or* 3D).
        let p = Ports {
            source: [gpad3d("AAAA", "Xbox 360"), Source::None],
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
    fn route_analog_feeds_only_present_analog_gamepad_ports() {
        // Port 0 = AAAA in 3D mode, port 1 = BBBB digital.
        let p = Ports {
            source: [gpad3d("AAAA", "Xbox 360"), gpad("BBBB", "DualShock 4")],
        };
        let inputs = [
            gp_a("AAAA", 0, [0x10, 0x20, 0x30, 0x40]),
            gp_a("BBBB", 0, [0xAA; 4]),
        ];
        let a = p.route_analog(&inputs);
        assert_eq!(
            a[0],
            Some([0x10, 0x20, 0x30, 0x40]),
            "3D port → its channels"
        );
        assert_eq!(a[1], None, "digital gamepad port → no analog");
        // An analog port whose controller isn't present reports None.
        let p2 = Ports {
            source: [gpad3d("AAAA", "X"), Source::None],
        };
        assert_eq!(p2.route_analog(&[]), [None, None]);
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
    fn label_distinguishes_digital_and_3d_pads() {
        assert_eq!(
            gpad("AAAA", "A Very Long Controller Name").label(),
            "Pad: A Very Long Co"
        );
        // A 3D pad is tagged and its name truncated tighter to leave room.
        assert_eq!(gpad3d("BBBB", "DualShock 4").label(), "Pad: DualShock (3D)");
        assert_eq!(Source::Keyboard.label(), "Keyboard");
    }
}
