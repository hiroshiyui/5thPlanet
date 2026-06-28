//! Hand-rolled on-screen menu (ZSNES/fwNES-style), software-composited into
//! the emulator's RGBA framebuffer.
//!
//! This module is deliberately **`sdl3`-free and core-free**: it operates on a
//! `&mut [u8]` RGBA buffer ([`font::Canvas`]) and an abstract [`Nav`] input,
//! and emits [`OsdAction`]s the frontend executes against the `Saturn` API. So
//! the whole menu — navigation and rendering — is unit-testable without a
//! window. The frontend (`main.rs`) bridges SDL key events → `Nav` and passes
//! dynamic labels in via [`OsdCtx`].

mod font;

use crate::config::{BUTTON_NAMES, PAD_BUTTONS};
use font::{Canvas, Rgb};

/// Number of save-state slots the menu exposes.
pub const SLOTS: usize = 10;

// Palette (chunky retro blue).
const PANEL_BG: Rgb = (0x10, 0x12, 0x40);
const PANEL_BORDER: Rgb = (0xC8, 0xC8, 0xD8);
const TITLE: Rgb = (0xF8, 0xE0, 0x40);
const ITEM: Rgb = (0xD0, 0xD0, 0xE0);
const ITEM_SEL: Rgb = (0xFF, 0xFF, 0xFF);
const HILITE_BAR: Rgb = (0x28, 0x2C, 0x80);
const TOAST_BG: Rgb = (0x00, 0x00, 0x00);
const TOAST_FG: Rgb = (0x80, 0xF8, 0x80);
/// Diagnostics result row colours (green pass / red fail).
const DIAG_PASS: Rgb = (0x50, 0xE0, 0x50);
const DIAG_FAIL: Rgb = (0xF0, 0x50, 0x50);

/// Abstract navigation input (the frontend maps keys/pad to these).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Select,
    Back,
}

/// SMPC region, named in the OSD's own terms so the module stays core-free; the
/// frontend maps these to `saturn::smpc::region` codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdRegion {
    Japan,
    NorthAmerica,
    EuropePal,
    AsiaNtsc,
}

impl OsdRegion {
    /// Short display name (used by the Diagnostics live-status readout).
    fn label(self) -> &'static str {
        match self {
            OsdRegion::Japan => "Japan",
            OsdRegion::NorthAmerica => "North America",
            OsdRegion::EuropePal => "Europe (PAL)",
            OsdRegion::AsiaNtsc => "Asia (NTSC)",
        }
    }
}

/// Rear-slot cartridge kind, in the OSD's own terms (frontend maps to
/// `saturn::cartridge::Cartridge`). Backup-RAM size is the frontend's default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdCart {
    None,
    ExtRam1M,
    ExtRam4M,
    BackupRam,
}

/// Which SMPC port carries the Shuttle Mouse (frontend maps to
/// `saturn::smpc::PortDevice`): `Off` = both ports default (pad on 1), `Port1`
/// replaces the pad on port 1, `Port2` keeps the pad on 1 and adds a mouse on 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdMouse {
    Off,
    Port1,
    Port2,
}

impl OsdMouse {
    /// The next setting in the Off → Port 1 → Port 2 → Off cycle (the Controller
    /// screen advances through these on each activation).
    fn next(self) -> Self {
        match self {
            OsdMouse::Off => OsdMouse::Port1,
            OsdMouse::Port1 => OsdMouse::Port2,
            OsdMouse::Port2 => OsdMouse::Off,
        }
    }

    /// Short label for the cycling Controller-screen row.
    fn label(self) -> &'static str {
        match self {
            OsdMouse::Off => "Off",
            OsdMouse::Port1 => "Port 1",
            OsdMouse::Port2 => "Port 2",
        }
    }
}

/// Graphics-presentation backend as the OSD names it (the frontend maps these to
/// `present::RenderBackend` config tokens). A convenience subset of the full
/// `--backend` vocabulary: `Direct3D` stands in for the D3D11/12 tokens and
/// OpenGL ES folds into `OpenGl`. Selecting one writes the config; it applies on
/// the next launch (the SDL3 render driver is fixed when the window is created).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdBackend {
    Auto,
    OpenGl,
    Direct3D,
    Metal,
    Software,
}

impl OsdBackend {
    /// The next backend in the Auto → OpenGL → Direct3D → Metal → Software → Auto
    /// cycle (the Graphics screen advances through these on each activation).
    fn next(self) -> Self {
        match self {
            OsdBackend::Auto => OsdBackend::OpenGl,
            OsdBackend::OpenGl => OsdBackend::Direct3D,
            OsdBackend::Direct3D => OsdBackend::Metal,
            OsdBackend::Metal => OsdBackend::Software,
            OsdBackend::Software => OsdBackend::Auto,
        }
    }

    /// Short label for the cycling Graphics-screen row.
    fn label(self) -> &'static str {
        match self {
            OsdBackend::Auto => "Auto",
            OsdBackend::OpenGl => "OpenGL",
            OsdBackend::Direct3D => "Direct3D",
            OsdBackend::Metal => "Metal",
            OsdBackend::Software => "Software",
        }
    }
}

/// One row in the disc-image browser ([`Screen::DiscBrowser`]). The frontend
/// builds the list from the current directory (`..` first when not at the
/// filesystem root, then sub-directories, then disc-image files); the OSD only
/// displays it and reports the chosen index, so all `fs` I/O stays frontend-side
/// and the menu remains unit-testable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowseEntry {
    /// File or directory name (not the full path — the frontend rebuilds that
    /// from its current browse directory).
    pub name: String,
    /// `true` for a directory (selecting it descends/ascends); `false` for a
    /// loadable disc image (selecting it loads + boots it).
    pub is_dir: bool,
}

/// An effect the frontend must carry out. The OSD itself never touches the
/// emulator; it just says what the user chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdAction {
    Resume,
    Reset,
    Save(u8),
    Load(u8),
    EjectDisc,
    ReinsertDisc,
    Quit,
    /// Set the window scale (1..=4× the base 320×224); frontend window op.
    SetScale(u8),
    /// Toggle borderless-desktop fullscreen; frontend window op.
    ToggleFullscreen,
    /// Toggle texture scaling Sharp (nearest) ↔ Smooth (linear); the frontend
    /// applies it live to the streaming texture and persists it.
    ToggleScaling,
    /// Toggle fullscreen aspect Keep-ratio (letterbox) ↔ Fit-screen (stretch);
    /// the frontend applies it live via SDL3 logical presentation and persists it.
    ToggleAspect,
    /// Switch SMPC region (frontend applies it and resets the machine).
    SetRegion(OsdRegion),
    /// Swap the rear-slot cartridge (frontend applies it and resets).
    SetCartridge(OsdCart),
    /// Move the Shuttle Mouse (frontend re-points the SMPC ports live, no reset).
    SetMouse(OsdMouse),
    /// Change the graphics-presentation backend. The frontend writes the config;
    /// the SDL3 render driver is fixed at window creation, so it applies on the
    /// next launch.
    SetBackend(OsdBackend),
    /// Rebind a pad button ([`crate::config::BUTTON_NAMES`] index): the
    /// frontend captures the next host key and reports back via
    /// [`Osd::end_capture`].
    StartRebind(u8),
    /// Restore the default key bindings.
    ResetBinds,
    /// Power-cycle into another BIOS image (an [`OsdCtx::bios_names`] index).
    SetBios(u8),
    /// Descend into / ascend out of a directory in the disc browser (an
    /// [`OsdCtx::browse_entries`] index whose `is_dir` is `true`). The frontend
    /// updates its browse directory and rebuilds `browse_entries`; the browser
    /// screen stays open (the OSD resets the selection to the top).
    BrowseEnter(usize),
    /// Load + boot the disc image at an [`OsdCtx::browse_entries`] index (a file
    /// entry). The frontend inserts the disc, resets the machine, and closes.
    LoadDisc(usize),
    /// Run the built-in self-diagnostics; the frontend fills
    /// [`OsdCtx::diag_results`] for the next draw. The screen stays open.
    RunDiagnostics,
    /// Select the built-in presentation shader: `true` = the CRT post-process,
    /// `false` = the plain blit. The SDL_GPU backend applies it live + persists it.
    /// Preview-only (the `gpu-preview` Shaders chooser).
    #[cfg(feature = "gpu-preview")]
    SetShader(bool),
}

/// One self-diagnostics result row for the Diagnostics screen. OSD-local (the
/// frontend maps `saturn::diagnostics::DiagOutcome` into this) so the OSD stays
/// core-free.
#[derive(Clone)]
pub struct DiagResultRow {
    pub label: String,
    pub passed: bool,
}

/// Dynamic context the frontend supplies each draw so labels reflect live
/// state without the OSD depending on the core.
#[derive(Clone)]
pub struct OsdCtx {
    pub disc_present: bool,
    /// Whether each save slot already has a file on disk.
    pub slot_used: [bool; SLOTS],
    /// Current window scale (1..=4×) — shown on the Graphics screen.
    pub scale: u8,
    /// Whether the window is currently fullscreen.
    pub fullscreen: bool,
    /// Whether texture scaling is Sharp (nearest) vs Smooth (linear) — the
    /// Graphics screen shows + toggles it.
    pub sharp: bool,
    /// Whether fullscreen keeps the aspect ratio (letterbox) vs fits-to-screen
    /// (stretch) — the Graphics screen shows + toggles it.
    pub keep_aspect: bool,
    /// Current SMPC region — the Region screen marks it.
    pub region: OsdRegion,
    /// Current rear-slot cartridge — the Cartridge screen marks it.
    pub cart: OsdCart,
    /// Current Shuttle Mouse port — the Controller screen shows it.
    pub mouse: OsdMouse,
    /// Current graphics-presentation backend — the Graphics screen cycles it.
    pub backend: OsdBackend,
    /// Whether the CRT shader is selected — the Shaders screen marks it
    /// (`gpu-preview` only; `false`/None otherwise).
    #[cfg(feature = "gpu-preview")]
    pub shader_crt: bool,
    /// Host key name bound to each pad button ([`BUTTON_NAMES`] order) —
    /// the Controller screen lists them.
    pub pad_keys: [String; PAD_BUTTONS],
    /// Display names of the BIOS images found beside the launched one; the
    /// BIOS screen lists them and marks [`OsdCtx::bios_active`].
    pub bios_names: Vec<String>,
    /// Index of the currently-running BIOS in [`OsdCtx::bios_names`].
    pub bios_active: usize,
    /// Contents of the disc browser's current directory (`..`, sub-dirs, then
    /// disc images) — the frontend rebuilds this as the user navigates.
    pub browse_entries: Vec<BrowseEntry>,
    /// The browser's current directory path, shown as the screen title.
    pub browse_dir: String,
    /// Last self-diagnostics results (empty until "Run all" is selected) —
    /// the Diagnostics screen lists them as `[PASS]`/`[FAIL]`.
    pub diag_results: Vec<DiagResultRow>,
    /// Live master-SH-2 PC and a label for the region it's executing in (BIOS /
    /// Low WRAM / High WRAM (game) / other) — the Diagnostics screen's
    /// "current session" readout. `cpu_where` is empty when the menu is closed.
    pub cpu_pc: u32,
    pub cpu_where: &'static str,
}

/// Which screen is on top of the stack.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Main,
    /// Slot picker; `saving` selects Save vs Load semantics.
    Slots {
        saving: bool,
    },
    /// Settings hub → Graphics / Region / Cartridge.
    Settings,
    Graphics,
    /// Stub chooser for the (planned) SDL_GPU CRT-shader presenter — see
    /// ADR-0019 + `shaders/README.md`. Currently a read-only placeholder; it
    /// will list the presets from `shaders/` once the presenter lands.
    /// Preview-only groundwork (the `gpu-preview` feature; absent by default).
    #[cfg(feature = "gpu-preview")]
    Shaders,
    Controller,
    Region,
    Cartridge,
    Bios,
    /// Filesystem browser for picking a disc image to load.
    DiscBrowser,
    /// Built-in self-diagnostics: a "Run all" item plus the last results.
    Diagnostics,
    /// Program version + license notice (read-only).
    About,
}

/// One menu item: its label is computed from [`OsdCtx`] at draw time.
struct Item {
    label: String,
    /// What activating it does: push a screen, emit an action, or close.
    on_select: Select,
    /// Optional text colour override (e.g. green/red diagnostics rows). `None`
    /// uses the default selected/unselected colour.
    color: Option<Rgb>,
}

#[derive(Clone, Copy)]
enum Select {
    Push(Screen),
    Emit(OsdAction),
    /// Emit an action but keep the current screen, resetting the selection to
    /// the top — used by the disc browser when descending into a directory so
    /// the rebuilt listing starts at row 0.
    Browse(OsdAction),
    /// Close the menu (emits `Resume`).
    Close,
}

/// The in-window OSD menu state (ADR-0008): whether it's open, the
/// screen/selection stack, a transient toast line, and any pending key-capture.
/// A pure, sdl3-free, core-free state machine — driven by navigation input and
/// rendered into an RGBA buffer, so it unit-tests without a window.
pub struct Osd {
    open: bool,
    /// Screen stack with the selection index for each level.
    stack: Vec<(Screen, usize)>,
    /// Transient status line: (message, frames remaining).
    toast: Option<(String, u32)>,
    /// Key-capture mode: the pad button (a [`BUTTON_NAMES`] index) awaiting
    /// the frontend's next host keypress. While set, navigation is ignored
    /// and the panel shows a "press a key" prompt.
    capturing: Option<u8>,
}

impl Default for Osd {
    fn default() -> Self {
        Self::new()
    }
}

impl Osd {
    /// A closed OSD positioned at the main menu.
    pub fn new() -> Self {
        Self {
            open: false,
            stack: vec![(Screen::Main, 0)],
            toast: None,
            capturing: None,
        }
    }

    /// Enter key-capture mode for a pad button (the frontend grabs the next
    /// host keypress). Entered via [`OsdAction::StartRebind`].
    pub fn begin_capture(&mut self, button: u8) {
        self.capturing = Some(button);
    }

    /// Leave key-capture mode (the frontend captured a key or cancelled).
    pub fn end_capture(&mut self) {
        self.capturing = None;
    }

    /// Whether the menu is currently showing (and intercepting input).
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Esc handler: open the menu, or (if already open) back out one level /
    /// close. Returns an action if backing out of the root closes the menu.
    pub fn toggle(&mut self) -> Option<OsdAction> {
        if self.open {
            self.back()
        } else {
            self.open = true;
            self.stack = vec![(Screen::Main, 0)];
            None
        }
    }

    /// Set a transient status message (shown for ~`frames` frames).
    pub fn set_toast(&mut self, msg: impl Into<String>, frames: u32) {
        self.toast = Some((msg.into(), frames));
    }

    /// Close the menu and resume emulation.
    pub fn close(&mut self) {
        self.open = false;
        self.stack = vec![(Screen::Main, 0)];
        self.capturing = None;
    }

    fn back(&mut self) -> Option<OsdAction> {
        if self.stack.len() > 1 {
            self.stack.pop();
            None
        } else {
            self.open = false;
            Some(OsdAction::Resume)
        }
    }

    fn screen(&self) -> Screen {
        self.stack.last().unwrap().0
    }
    fn sel(&self) -> usize {
        self.stack.last().unwrap().1
    }
    fn sel_mut(&mut self) -> &mut usize {
        &mut self.stack.last_mut().unwrap().1
    }

    /// The items for a screen, with live labels from `ctx`.
    fn items(&self, screen: Screen, ctx: &OsdCtx) -> Vec<Item> {
        let mk = |label: &str, on_select: Select| Item {
            label: label.to_string(),
            on_select,
            color: None,
        };
        match screen {
            Screen::Main => vec![
                mk("Resume", Select::Close),
                mk("Save State", Select::Push(Screen::Slots { saving: true })),
                mk("Load State", Select::Push(Screen::Slots { saving: false })),
                mk("Reset", Select::Emit(OsdAction::Reset)),
                if ctx.disc_present {
                    mk("Eject Disc", Select::Emit(OsdAction::EjectDisc))
                } else {
                    mk("Insert Disc", Select::Emit(OsdAction::ReinsertDisc))
                },
                mk("Load Disc...", Select::Push(Screen::DiscBrowser)),
                mk("Settings", Select::Push(Screen::Settings)),
                mk("About...", Select::Push(Screen::About)),
                mk("Quit", Select::Emit(OsdAction::Quit)),
            ],
            Screen::Slots { saving } => {
                let mut v = Vec::with_capacity(SLOTS + 1);
                for s in 0..SLOTS {
                    let used = ctx.slot_used[s];
                    let mark = if used { "*" } else { "-" };
                    let label = format!("Slot {s} [{mark}]");
                    let act = if saving {
                        OsdAction::Save(s as u8)
                    } else {
                        OsdAction::Load(s as u8)
                    };
                    v.push(mk(&label, Select::Emit(act)));
                }
                v.push(mk("Back", Select::Close)); // Close here means "pop one"
                v
            }
            Screen::Settings => vec![
                mk("Graphics...", Select::Push(Screen::Graphics)),
                mk("Controller", Select::Push(Screen::Controller)),
                mk("Region", Select::Push(Screen::Region)),
                mk("Cartridge", Select::Push(Screen::Cartridge)),
                mk("BIOS", Select::Push(Screen::Bios)),
                mk("Diagnostics...", Select::Push(Screen::Diagnostics)),
                mk("Back", Select::Close),
            ],
            Screen::Graphics => {
                // Scale cycles 1→2→3→4→1 on each activation.
                let next = ctx.scale % 4 + 1;
                let mut v = vec![
                    mk(
                        &format!("Scale: {}x", ctx.scale),
                        Select::Emit(OsdAction::SetScale(next)),
                    ),
                    mk(
                        &format!("Fullscreen: {}", if ctx.fullscreen { "On" } else { "Off" }),
                        Select::Emit(OsdAction::ToggleFullscreen),
                    ),
                    // The SDL3 render driver is fixed when the window is created,
                    // so this writes the config and takes effect on next launch.
                    mk(
                        &format!("Renderer: {}", ctx.backend.label()),
                        Select::Emit(OsdAction::SetBackend(ctx.backend.next())),
                    ),
                    // Nearest (Sharp) vs linear (Smooth) texture filtering; applied
                    // live, so no restart note like the renderer above.
                    mk(
                        &format!("Pixels: {}", if ctx.sharp { "Sharp" } else { "Smooth" }),
                        Select::Emit(OsdAction::ToggleScaling),
                    ),
                    // Letterbox (Keep ratio) vs stretch (Fit screen) in fullscreen;
                    // applied live via SDL3 logical presentation.
                    mk(
                        &format!(
                            "Aspect: {}",
                            if ctx.keep_aspect {
                                "Keep ratio"
                            } else {
                                "Fit screen"
                            }
                        ),
                        Select::Emit(OsdAction::ToggleAspect),
                    ),
                ];
                // The Shaders chooser is preview-only groundwork (gpu-preview).
                #[cfg(feature = "gpu-preview")]
                v.push(mk("Shaders...", Select::Push(Screen::Shaders)));
                v.push(mk("Back", Select::Close));
                v
            }
            #[cfg(feature = "gpu-preview")]
            Screen::Shaders => {
                // CRT post-process chooser for the SDL_GPU backend (ADR-0019;
                // `jupiter/src/shaders/`). The active option is marked '*'. Only the
                // SDL_GPU backend applies it — under the renderer it's a no-op
                // (the frontend toasts a hint). Future: list user presets here.
                let row = |name: &str, crt: bool| {
                    let mark = if ctx.shader_crt == crt { "* " } else { "  " };
                    mk(
                        &format!("{mark}{name}"),
                        Select::Emit(OsdAction::SetShader(crt)),
                    )
                };
                vec![
                    row("None (passthrough)", false),
                    row("CRT (scanlines + mask)", true),
                    mk("Back", Select::Close),
                ]
            }
            Screen::Controller => {
                // One row per pad button: "<button>  <bound key>"; selecting
                // it asks the frontend to capture the next host keypress.
                let mut v = Vec::with_capacity(PAD_BUTTONS + 2);
                for (i, name) in BUTTON_NAMES.iter().enumerate() {
                    v.push(mk(
                        &format!("{name:<6} {}", ctx.pad_keys[i]),
                        Select::Emit(OsdAction::StartRebind(i as u8)),
                    ));
                }
                // Shuttle Mouse port cycles Off → Port 1 → Port 2 → Off.
                v.push(mk(
                    &format!("Mouse: {}", ctx.mouse.label()),
                    Select::Emit(OsdAction::SetMouse(ctx.mouse.next())),
                ));
                v.push(mk("Reset Defaults", Select::Emit(OsdAction::ResetBinds)));
                v.push(mk("Back", Select::Close));
                v
            }
            Screen::Region => {
                // The active region is marked with a leading '*'.
                let row = |name: &str, r: OsdRegion| {
                    let mark = if ctx.region == r { "* " } else { "  " };
                    mk(
                        &format!("{mark}{name}"),
                        Select::Emit(OsdAction::SetRegion(r)),
                    )
                };
                vec![
                    row("Japan", OsdRegion::Japan),
                    row("North America", OsdRegion::NorthAmerica),
                    row("Europe (PAL)", OsdRegion::EuropePal),
                    row("Asia (NTSC)", OsdRegion::AsiaNtsc),
                    mk("Back", Select::Close),
                ]
            }
            Screen::Cartridge => {
                let row = |name: &str, k: OsdCart| {
                    let mark = if ctx.cart == k { "* " } else { "  " };
                    mk(
                        &format!("{mark}{name}"),
                        Select::Emit(OsdAction::SetCartridge(k)),
                    )
                };
                vec![
                    row("None", OsdCart::None),
                    row("Ext RAM 1M", OsdCart::ExtRam1M),
                    row("Ext RAM 4M", OsdCart::ExtRam4M),
                    row("Backup RAM", OsdCart::BackupRam),
                    mk("Back", Select::Close),
                ]
            }
            Screen::Bios => {
                // One row per discovered 512-KiB image; '*' marks the running
                // one. Long file stems are truncated to fit the panel.
                let mut v = Vec::with_capacity(ctx.bios_names.len() + 1);
                for (i, name) in ctx.bios_names.iter().enumerate() {
                    let mark = if i == ctx.bios_active { "* " } else { "  " };
                    let short: String = name.chars().take(18).collect();
                    v.push(mk(
                        &format!("{mark}{short}"),
                        Select::Emit(OsdAction::SetBios(i as u8)),
                    ));
                }
                if v.is_empty() {
                    v.push(mk("(no images found)", Select::Close));
                }
                v.push(mk("Back", Select::Close));
                v
            }
            Screen::DiscBrowser => {
                // One row per entry: directories descend (kept open, selection
                // reset to the top via `Select::Browse`), files load + boot.
                // Names are truncated to the panel width; dirs get a trailing
                // '/'. The action index is the `browse_entries` index, so the
                // entries must come first and "Back" last.
                let mut v = Vec::with_capacity(ctx.browse_entries.len() + 1);
                for (i, e) in ctx.browse_entries.iter().enumerate() {
                    if e.is_dir {
                        let short: String = e.name.chars().take(19).collect();
                        v.push(mk(
                            &format!("{short}/"),
                            Select::Browse(OsdAction::BrowseEnter(i)),
                        ));
                    } else {
                        let short: String = e.name.chars().take(20).collect();
                        v.push(mk(&short, Select::Emit(OsdAction::LoadDisc(i))));
                    }
                }
                if ctx.browse_entries.is_empty() {
                    v.push(mk("(no disc images)", Select::Close));
                }
                v.push(mk("Back", Select::Close));
                v
            }
            Screen::Diagnostics => {
                // "Run all" plus one inert, colour-coded row per result. The
                // panel auto-sizes to the widest row and the 15-row scrolling
                // viewport handles a long list (Back is one Up-wrap away).
                let mut v = Vec::with_capacity(ctx.diag_results.len() + 5);
                v.push(mk("Run all", Select::Emit(OsdAction::RunDiagnostics)));
                // Live "current session" status (read-only) — no boot involved.
                v.push(mk(
                    &format!("Region: {}", ctx.region.label()),
                    Select::Close,
                ));
                v.push(mk(
                    &format!(
                        "Disc: {}",
                        if ctx.disc_present { "present" } else { "none" }
                    ),
                    Select::Close,
                ));
                v.push(mk(
                    &format!("Master PC: {:08X} {}", ctx.cpu_pc, ctx.cpu_where),
                    Select::Close,
                ));
                if ctx.diag_results.is_empty() {
                    v.push(mk("(not run yet)", Select::Close));
                } else {
                    for r in &ctx.diag_results {
                        let (tag, col) = if r.passed {
                            ("PASS", DIAG_PASS)
                        } else {
                            ("FAIL", DIAG_FAIL)
                        };
                        v.push(Item {
                            label: format!("[{tag}] {}", r.label),
                            on_select: Select::Close,
                            color: Some(col),
                        });
                    }
                }
                v.push(mk("Back", Select::Close));
                v
            }
            Screen::About => {
                // Static program identity + license notice (read-only rows).
                // Version/license/author are this crate's compile-time package
                // metadata (inherited workspace-wide); the product name is
                // 5thPlanet (the binary is `jupiter`).
                vec![
                    mk(
                        concat!("5thPlanet  v", env!("CARGO_PKG_VERSION")),
                        Select::Close,
                    ),
                    mk("An accuracy-first SEGA Saturn emulator", Select::Close),
                    mk(concat!("(C) ", env!("CARGO_PKG_AUTHORS")), Select::Close),
                    mk(
                        concat!("License: ", env!("CARGO_PKG_LICENSE")),
                        Select::Close,
                    ),
                    mk("Provided AS IS, without warranty.", Select::Close),
                    mk("Full terms: see the LICENSE file.", Select::Close),
                    mk("Back", Select::Close),
                ]
            }
        }
    }

    fn title(screen: Screen) -> &'static str {
        match screen {
            Screen::Main => "5thPlanet",
            Screen::Slots { saving: true } => "Save State",
            Screen::Slots { saving: false } => "Load State",
            Screen::Settings => "Settings",
            Screen::Graphics => "Graphics",
            #[cfg(feature = "gpu-preview")]
            Screen::Shaders => "Shaders",
            Screen::Controller => "Controller",
            Screen::Region => "Region",
            Screen::Cartridge => "Cartridge",
            Screen::Bios => "BIOS",
            Screen::DiscBrowser => "Load Disc",
            Screen::Diagnostics => "Diagnostics",
            Screen::About => "About",
        }
    }

    /// Advance the toast timer one frame (call once per rendered frame).
    pub fn tick_toast(&mut self) {
        if let Some((_, frames)) = &mut self.toast {
            *frames = frames.saturating_sub(1);
            if *frames == 0 {
                self.toast = None;
            }
        }
    }

    /// Handle a navigation input. Returns an action for the frontend to run.
    pub fn handle(&mut self, nav: Nav, ctx: &OsdCtx) -> Option<OsdAction> {
        if !self.open || self.capturing.is_some() {
            // In key-capture mode every host key belongs to the frontend's
            // capture (it ends via `end_capture`), not to navigation.
            return None;
        }
        let screen = self.screen();
        let n = self.items(screen, ctx).len();
        match nav {
            Nav::Up => {
                let s = self.sel();
                *self.sel_mut() = if s == 0 { n - 1 } else { s - 1 };
                None
            }
            Nav::Down => {
                let s = self.sel();
                *self.sel_mut() = if s + 1 >= n { 0 } else { s + 1 };
                None
            }
            Nav::Back => self.back(),
            Nav::Select => {
                let idx = self.sel().min(n - 1);
                let on_select = match self.items(screen, ctx).into_iter().nth(idx) {
                    Some(it) => it.on_select,
                    None => return None,
                };
                match on_select {
                    Select::Push(next) => {
                        self.stack.push((next, 0));
                        None
                    }
                    Select::Close => {
                        // On a submenu "Back" item this pops; on the root
                        // "Resume" it closes the menu.
                        self.back()
                    }
                    Select::Emit(action) => Some(action),
                    Select::Browse(action) => {
                        // Stay on this screen but reset the cursor: the frontend
                        // is about to rebuild `browse_entries` for the new
                        // directory, so the old index would point at a stale row.
                        *self.sel_mut() = 0;
                        Some(action)
                    }
                }
            }
        }
    }

    /// Frontend entry point: composite the OSD onto a `w × h` RGBA buffer
    /// (`[R,G,B,A]` per pixel). When the menu is open the underlying (frozen)
    /// frame is dimmed first so the overlay reads clearly; when closed only a
    /// lingering toast is drawn. Keeps [`Canvas`] private to this module.
    pub fn render_overlay(&self, buf: &mut [u8], w: usize, h: usize, ctx: &OsdCtx) {
        // Scale each axis independently by whether *that* axis is hi-res, so the
        // 8×8 glyphs don't shrink/smear yet the menu still fits: 640/704-dot is a
        // double-rate horizontal clock (sx=2), 448/480 is interlace (sy=2). A
        // wide-but-short 640×224 frame gets sx=2, sy=1 — readable and fitting;
        // doubling both would overflow its 224 lines. Lo-res stays pixel-exact.
        let sx = if w >= 640 { 2 } else { 1 };
        let sy = if h >= 400 { 2 } else { 1 };
        let mut c = Canvas::new(buf, w, h, sx, sy);
        if self.open {
            c.dim();
        }
        self.draw(&mut c, ctx);
    }

    /// Composite the menu over the (already-dimmed) framebuffer.
    fn draw(&self, c: &mut Canvas, ctx: &OsdCtx) {
        if !self.open {
            // Even when closed, a lingering toast is still shown.
            self.draw_toast(c);
            return;
        }
        // Key-capture mode replaces the items with a modal prompt.
        if let Some(b) = self.capturing {
            let name = BUTTON_NAMES[b as usize % PAD_BUTTONS];
            let line1 = format!("Press a key for {name}");
            let pw = 180usize;
            let ph = 44usize;
            let px = c.w.saturating_sub(pw) / 2;
            let py = c.h.saturating_sub(ph) / 2;
            c.fill_rect(px, py, pw, ph, PANEL_BG);
            c.rect_outline(px, py, pw, ph, PANEL_BORDER);
            c.draw_text(
                px + (pw - Canvas::text_width(&line1)) / 2,
                py + 10,
                &line1,
                TITLE,
            );
            let line2 = "(Esc cancels)";
            c.draw_text(
                px + (pw - Canvas::text_width(line2)) / 2,
                py + 26,
                line2,
                ITEM,
            );
            self.draw_toast(c);
            return;
        }
        let screen = self.screen();
        let items = self.items(screen, ctx);
        let sel = self.sel().min(items.len().saturating_sub(1));

        // Scrolling viewport: a screen with more rows than `MAX_VISIBLE_ROWS`
        // (the disc browser over a large directory) shows a window around the
        // selection. Every built-in screen has <= `MAX_VISIBLE_ROWS` rows, so
        // their layout is unchanged.
        const MAX_VISIBLE_ROWS: usize = 15;
        let total = items.len();
        let (first, shown) = if total <= MAX_VISIBLE_ROWS {
            (0, total)
        } else {
            let first = sel
                .saturating_sub(MAX_VISIBLE_ROWS / 2)
                .min(total - MAX_VISIBLE_ROWS);
            (first, MAX_VISIBLE_ROWS)
        };

        // Panel geometry: centred, height from the visible rows, width auto-
        // sized to the widest row (so a screen with long labels — the
        // diagnostics results — doesn't overflow). Floored at 180 so the
        // short-label screens look identical, capped to the framebuffer.
        const TEXT_INSET: usize = 12;
        let widest = items
            .iter()
            .map(|it| Canvas::text_width(&it.label))
            .max()
            .unwrap_or(0);
        let pw = (widest + TEXT_INSET + 8).clamp(180, c.w.saturating_sub(8).max(180));
        let ph = 28 + shown * 12 + 8;
        let px = c.w.saturating_sub(pw) / 2;
        let py = c.h.saturating_sub(ph) / 2;

        c.fill_rect(px, py, pw, ph, PANEL_BG);
        c.rect_outline(px, py, pw, ph, PANEL_BORDER);

        // The disc browser titles itself with its current directory (tail-
        // truncated so the *current* folder stays visible); others use a caption.
        let browser_title;
        let title: &str = if screen == Screen::DiscBrowser {
            browser_title = dir_tail(&ctx.browse_dir, 20);
            &browser_title
        } else {
            Self::title(screen)
        };
        c.draw_text(
            px + (pw.saturating_sub(Canvas::text_width(title))) / 2,
            py + 8,
            title,
            TITLE,
        );
        c.fill_rect(px + 6, py + 22, pw - 12, 1, PANEL_BORDER);

        let row0 = py + 28;
        for vis in 0..shown {
            let i = first + vis;
            let ry = row0 + vis * 12;
            let base = if i == sel {
                c.fill_rect(px + 4, ry - 2, pw - 8, 11, HILITE_BAR);
                ITEM_SEL
            } else {
                ITEM
            };
            // A per-item colour (diagnostics PASS/FAIL) overrides the default,
            // but the selected row stays white so the highlight stays legible.
            let color = if i == sel {
                base
            } else {
                items[i].color.unwrap_or(base)
            };
            c.draw_text(px + 12, ry, &items[i].label, color);
        }

        self.draw_toast(c);
    }

    fn draw_toast(&self, c: &mut Canvas) {
        if let Some((msg, _)) = &self.toast {
            let tw = Canvas::text_width(msg) + 8;
            let tx = c.w.saturating_sub(tw) / 2;
            let ty = c.h.saturating_sub(16);
            c.fill_rect(tx, ty, tw, 12, TOAST_BG);
            c.draw_text(tx + 4, ty + 2, msg, TOAST_FG);
        }
    }
}

/// The trailing `max` characters of a path string, prefixed with ".." when
/// clipped — keeps the *current* folder visible in the disc-browser title
/// rather than the (usually long, usually shared) leading path.
fn dir_tail(path: &str, max: usize) -> String {
    let n = path.chars().count();
    if n <= max {
        path.to_string()
    } else {
        let keep = max.saturating_sub(2);
        let tail: String = path.chars().skip(n - keep).collect();
        format!("..{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(disc: bool) -> OsdCtx {
        OsdCtx {
            disc_present: disc,
            slot_used: [false; SLOTS],
            scale: 2,
            fullscreen: false,
            sharp: true,
            keep_aspect: true,
            region: OsdRegion::Japan,
            cart: OsdCart::None,
            mouse: OsdMouse::Off,
            backend: OsdBackend::Auto,
            #[cfg(feature = "gpu-preview")]
            shader_crt: false,
            pad_keys: crate::config::DEFAULT_KEYS.map(str::to_string),
            bios_names: vec!["sega_101".into(), "mpr-17933".into()],
            bios_active: 0,
            browse_entries: Vec::new(),
            browse_dir: "/games".into(),
            diag_results: Vec::new(),
            cpu_pc: 0x0600_1234,
            cpu_where: "High WRAM (game)",
        }
    }

    fn dir(name: &str) -> BrowseEntry {
        BrowseEntry {
            name: name.into(),
            is_dir: true,
        }
    }
    fn file(name: &str) -> BrowseEntry {
        BrowseEntry {
            name: name.into(),
            is_dir: false,
        }
    }

    /// Navigate from the main screen to a named item by repeated Down, then
    /// Select. Returns the action (if any) the Select produced.
    fn select_main(osd: &mut Osd, c: &OsdCtx, label: &str) -> Option<OsdAction> {
        // Find the index of `label` on the current screen.
        let items = osd.items(osd.screen(), c);
        let idx = items
            .iter()
            .position(|it| it.label == label)
            .expect("item exists");
        for _ in 0..idx {
            osd.handle(Nav::Down, c);
        }
        osd.handle(Nav::Select, c)
    }

    #[test]
    fn opens_and_backs_out_to_resume() {
        let mut osd = Osd::new();
        assert!(!osd.is_open());
        assert_eq!(osd.toggle(), None);
        assert!(osd.is_open());
        // Esc at root closes and resumes.
        assert_eq!(osd.toggle(), Some(OsdAction::Resume));
        assert!(!osd.is_open());
    }

    #[test]
    fn down_wraps_and_selects_reset() {
        let mut osd = Osd::new();
        osd.toggle(); // open at Main, sel=0 (Resume)
        let c = ctx(true);
        // Main: Resume, Save, Load, Reset, Eject, Quit
        osd.handle(Nav::Down, &c); // Save
        osd.handle(Nav::Down, &c); // Load
        osd.handle(Nav::Down, &c); // Reset
        assert_eq!(osd.handle(Nav::Select, &c), Some(OsdAction::Reset));
    }

    #[test]
    fn up_wraps_to_quit() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true);
        osd.handle(Nav::Up, &c); // wraps to last item = Quit
        assert_eq!(osd.handle(Nav::Select, &c), Some(OsdAction::Quit));
    }

    #[cfg(feature = "gpu-preview")]
    #[test]
    fn graphics_shaders_chooser_offers_none_and_crt() {
        let mut osd = Osd::new();
        osd.toggle(); // open at Main
        let c = ctx(true); // shader_crt = false → "None" is the marked row
        // Main → Settings → Graphics... → Shaders... (each Push emits no action).
        assert_eq!(select_main(&mut osd, &c, "Settings"), None);
        assert_eq!(select_main(&mut osd, &c, "Graphics..."), None);
        assert_eq!(select_main(&mut osd, &c, "Shaders..."), None);
        assert_eq!(Osd::title(osd.screen()), "Shaders");
        // None is marked '*' (shader_crt = false), CRT unmarked, plus Back.
        let items = osd.items(osd.screen(), &c);
        assert!(items.iter().any(|it| it.label == "* None (passthrough)"));
        assert!(
            items
                .iter()
                .any(|it| it.label == "  CRT (scanlines + mask)")
        );
        assert!(items.iter().any(|it| it.label == "Back"));
        // Selecting the CRT row emits SetShader(true) (one select after the Push
        // reset the cursor to the top).
        assert_eq!(
            select_main(&mut osd, &c, "  CRT (scanlines + mask)"),
            Some(OsdAction::SetShader(true))
        );
    }

    #[test]
    fn save_submenu_emits_slot_and_back_pops() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true);
        osd.handle(Nav::Down, &c); // Save State
        assert_eq!(osd.handle(Nav::Select, &c), None); // pushes Slots
        // Slots screen: Slot 0 selected → Save(0)
        assert_eq!(osd.handle(Nav::Select, &c), Some(OsdAction::Save(0)));
        // Selecting slot 2 then Save.
        osd.handle(Nav::Down, &c);
        osd.handle(Nav::Down, &c);
        assert_eq!(osd.handle(Nav::Select, &c), Some(OsdAction::Save(2)));
        // Back pops to Main (still open), not Resume.
        assert_eq!(osd.handle(Nav::Back, &c), None);
        assert!(osd.is_open());
        // close() (used by the frontend on Resume/Load/Reset) shuts the menu
        // and resets to the root.
        osd.close();
        assert!(!osd.is_open());
    }

    #[test]
    fn diagnostics_screen_runs_and_lists_results() {
        let mut osd = Osd::new();
        osd.toggle();
        let mut c = ctx(true);
        // Main → Settings → Diagnostics…
        assert_eq!(select_main(&mut osd, &c, "Settings"), None);
        assert_eq!(select_main(&mut osd, &c, "Diagnostics..."), None);
        // "Run all" emits the action; the frontend then fills diag_results.
        assert_eq!(
            select_main(&mut osd, &c, "Run all"),
            Some(OsdAction::RunDiagnostics)
        );
        // With results present, each renders as a [PASS]/[FAIL] row.
        c.diag_results = vec![
            DiagResultRow {
                label: "cpu/cpu_add_imm".into(),
                passed: true,
            },
            DiagResultRow {
                label: "memory/mem_roundtrip_low".into(),
                passed: false,
            },
        ];
        let labels: Vec<String> = osd
            .items(osd.screen(), &c)
            .into_iter()
            .map(|it| it.label)
            .collect();
        assert!(
            labels
                .iter()
                .any(|l| l.starts_with("[PASS] cpu/cpu_add_imm"))
        );
        assert!(
            labels
                .iter()
                .any(|l| l.starts_with("[FAIL] memory/mem_roundtrip"))
        );
        // Live "current session" status rows reflect ctx (region/disc/PC).
        assert!(labels.iter().any(|l| l == "Region: Japan"));
        assert!(labels.iter().any(|l| l == "Disc: present"));
        assert!(
            labels
                .iter()
                .any(|l| l == "Master PC: 06001234 High WRAM (game)")
        );
    }

    #[test]
    fn about_screen_shows_version_and_license() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true);
        assert_eq!(select_main(&mut osd, &c, "About..."), None); // pushes About
        let labels: Vec<String> = osd
            .items(osd.screen(), &c)
            .into_iter()
            .map(|it| it.label)
            .collect();
        assert!(
            labels.iter().any(|l| l.contains(env!("CARGO_PKG_VERSION"))),
            "About lists the version: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.starts_with("License:")),
            "About lists the license"
        );
        // Back pops to Main (still open).
        assert_eq!(osd.handle(Nav::Back, &c), None);
        assert!(osd.is_open());
    }

    #[test]
    fn eject_label_flips_to_insert_without_disc() {
        let mut osd = Osd::new();
        osd.toggle();
        // No disc → the 5th Main item is Insert Disc.
        for _ in 0..4 {
            osd.handle(Nav::Down, &ctx(false));
        }
        assert_eq!(
            osd.handle(Nav::Select, &ctx(false)),
            Some(OsdAction::ReinsertDisc)
        );
    }

    #[test]
    fn draw_paints_panel_pixels_when_open() {
        let (w, h) = (320usize, 224usize);
        let mut buf = vec![0u8; w * h * 4];
        let mut osd = Osd::new();
        osd.toggle();
        osd.render_overlay(&mut buf, w, h, &ctx(true));
        assert!(buf.iter().any(|&b| b != 0), "open menu paints pixels");
    }

    #[test]
    fn closed_menu_draws_nothing_but_toast() {
        let (w, h) = (320usize, 224usize);
        let mut buf = vec![0u8; w * h * 4];
        let mut osd = Osd::new(); // closed
        osd.render_overlay(&mut buf, w, h, &ctx(true));
        assert!(buf.iter().all(|&b| b == 0), "closed menu paints nothing");
        osd.set_toast("hi", 60);
        osd.render_overlay(&mut buf, w, h, &ctx(true));
        assert!(buf.iter().any(|&b| b != 0), "toast shows even when closed");
    }

    #[test]
    fn toast_expires() {
        let mut osd = Osd::new();
        osd.set_toast("x", 2);
        osd.tick_toast();
        assert!(osd.toast.is_some());
        osd.tick_toast();
        assert!(osd.toast.is_none());
    }

    #[test]
    fn settings_graphics_cycles_scale_and_toggles_fullscreen() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true); // scale = 2, fullscreen = false
        assert_eq!(select_main(&mut osd, &c, "Settings"), None); // push Settings
        assert_eq!(select_main(&mut osd, &c, "Graphics..."), None); // push Graphics
        // Scale 2 → next is 3.
        assert_eq!(osd.handle(Nav::Select, &c), Some(OsdAction::SetScale(3)));
        // Fullscreen item toggles.
        osd.handle(Nav::Down, &c);
        assert_eq!(
            osd.handle(Nav::Select, &c),
            Some(OsdAction::ToggleFullscreen)
        );
    }

    #[test]
    fn graphics_pixels_row_toggles_scaling() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true); // sharp = true → row reads "Pixels: Sharp"
        assert_eq!(select_main(&mut osd, &c, "Settings"), None);
        assert_eq!(select_main(&mut osd, &c, "Graphics..."), None);
        assert_eq!(
            select_main(&mut osd, &c, "Pixels: Sharp"),
            Some(OsdAction::ToggleScaling)
        );
    }

    #[test]
    fn graphics_aspect_row_toggles() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true); // keep_aspect = true → row reads "Aspect: Keep ratio"
        assert_eq!(select_main(&mut osd, &c, "Settings"), None);
        assert_eq!(select_main(&mut osd, &c, "Graphics..."), None);
        assert_eq!(
            select_main(&mut osd, &c, "Aspect: Keep ratio"),
            Some(OsdAction::ToggleAspect)
        );
    }

    #[test]
    fn osd_mouse_cycles_and_labels() {
        assert_eq!(OsdMouse::Off.next(), OsdMouse::Port1);
        assert_eq!(OsdMouse::Port1.next(), OsdMouse::Port2);
        assert_eq!(OsdMouse::Port2.next(), OsdMouse::Off);
        assert_eq!(OsdMouse::Off.label(), "Off");
        assert_eq!(OsdMouse::Port1.label(), "Port 1");
        assert_eq!(OsdMouse::Port2.label(), "Port 2");
    }

    #[test]
    fn osd_backend_cycles_and_labels() {
        let mut b = OsdBackend::Auto;
        let mut seen = vec![b.label()];
        for _ in 0..4 {
            b = b.next();
            seen.push(b.label());
        }
        assert_eq!(seen, ["Auto", "OpenGL", "Direct3D", "Metal", "Software"]);
        assert_eq!(b.next(), OsdBackend::Auto); // wraps back to Auto
    }

    #[test]
    fn graphics_renderer_row_cycles_backend() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true); // backend = Auto
        select_main(&mut osd, &c, "Settings");
        select_main(&mut osd, &c, "Graphics...");
        // The row reads "Renderer: Auto"; activating it advances to OpenGL.
        assert_eq!(
            select_main(&mut osd, &c, "Renderer: Auto"),
            Some(OsdAction::SetBackend(OsdBackend::OpenGl))
        );
    }

    #[test]
    fn controller_mouse_row_emits_next_port() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true); // mouse = Off
        select_main(&mut osd, &c, "Settings");
        select_main(&mut osd, &c, "Controller");
        // The row reads "Mouse: Off"; activating it cycles to Port 1.
        assert_eq!(
            select_main(&mut osd, &c, "Mouse: Off"),
            Some(OsdAction::SetMouse(OsdMouse::Port1))
        );
    }

    #[test]
    fn graphics_scale_wraps_4_to_1() {
        let mut osd = Osd::new();
        osd.toggle();
        let mut c = ctx(true);
        c.scale = 4;
        select_main(&mut osd, &c, "Settings");
        select_main(&mut osd, &c, "Graphics...");
        assert_eq!(osd.handle(Nav::Select, &c), Some(OsdAction::SetScale(1)));
    }

    #[test]
    fn region_screen_emits_selected_region() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true); // region = Japan (marked)
        select_main(&mut osd, &c, "Settings");
        select_main(&mut osd, &c, "Region");
        // Region order: Japan, North America, Europe, Asia, Back.
        assert_eq!(
            select_main(&mut osd, &c, "  Europe (PAL)"),
            Some(OsdAction::SetRegion(OsdRegion::EuropePal))
        );
    }

    #[test]
    fn region_marks_the_active_region() {
        let mut osd = Osd::new();
        osd.toggle();
        let mut c = ctx(true);
        c.region = OsdRegion::NorthAmerica;
        select_main(&mut osd, &c, "Settings");
        select_main(&mut osd, &c, "Region");
        let items = osd.items(osd.screen(), &c);
        assert!(items.iter().any(|it| it.label == "* North America"));
        assert!(items.iter().any(|it| it.label == "  Japan"));
    }

    #[test]
    fn cartridge_screen_emits_selected_cart() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true); // cart = None
        select_main(&mut osd, &c, "Settings");
        select_main(&mut osd, &c, "Cartridge");
        assert_eq!(
            select_main(&mut osd, &c, "  Ext RAM 4M"),
            Some(OsdAction::SetCartridge(OsdCart::ExtRam4M))
        );
    }

    #[test]
    fn controller_screen_lists_bindings_and_starts_a_rebind() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true);
        select_main(&mut osd, &c, "Settings");
        select_main(&mut osd, &c, "Controller");
        // Rows are "<button> <key>" in BUTTON_NAMES order; row 4 = A on Z.
        let items = osd.items(osd.screen(), &c);
        assert!(items.iter().any(|it| it.label == "A      Z"));
        assert_eq!(
            select_main(&mut osd, &c, "Start  Return"),
            Some(OsdAction::StartRebind(12))
        );
        // Selection stays on Start (12); the rows below are Mouse (13) then
        // Reset Defaults (14), so two Downs reach Reset Defaults.
        osd.handle(Nav::Down, &c); // Mouse
        osd.handle(Nav::Down, &c); // Reset Defaults
        assert_eq!(osd.handle(Nav::Select, &c), Some(OsdAction::ResetBinds));
    }

    #[test]
    fn capture_mode_swallows_navigation_until_ended() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true);
        select_main(&mut osd, &c, "Settings");
        select_main(&mut osd, &c, "Controller");
        osd.begin_capture(12);
        // While capturing, nav does nothing — not even Back.
        assert_eq!(osd.handle(Nav::Back, &c), None);
        assert_eq!(osd.handle(Nav::Select, &c), None);
        assert!(osd.is_open());
        // The frontend reports the capture finished; nav works again.
        osd.end_capture();
        assert_eq!(osd.handle(Nav::Back, &c), None); // pops to Settings
        assert!(osd.is_open());
    }

    #[test]
    fn capture_mode_draws_the_prompt() {
        let (w, h) = (320usize, 224usize);
        let mut osd = Osd::new();
        osd.toggle();
        osd.begin_capture(0);
        let mut buf = vec![0u8; w * h * 4];
        osd.render_overlay(&mut buf, w, h, &ctx(true));
        assert!(buf.iter().any(|&b| b != 0), "capture prompt paints pixels");
    }

    #[test]
    fn bios_screen_marks_active_and_emits_swap() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true); // bios_active = 0 (sega_101)
        select_main(&mut osd, &c, "Settings");
        select_main(&mut osd, &c, "BIOS");
        let items = osd.items(osd.screen(), &c);
        assert!(items.iter().any(|it| it.label == "* sega_101"));
        assert_eq!(
            select_main(&mut osd, &c, "  mpr-17933"),
            Some(OsdAction::SetBios(1))
        );
    }

    #[test]
    fn bios_screen_with_no_images_offers_only_back() {
        let mut osd = Osd::new();
        osd.toggle();
        let mut c = ctx(true);
        c.bios_names.clear();
        select_main(&mut osd, &c, "Settings");
        select_main(&mut osd, &c, "BIOS");
        let items = osd.items(osd.screen(), &c);
        assert_eq!(items.len(), 2); // "(no images found)" + Back
        // Selecting the placeholder just pops back to Settings.
        assert_eq!(osd.handle(Nav::Select, &c), None);
        assert!(osd.is_open());
    }

    #[test]
    fn settings_back_returns_to_main_then_resume() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true);
        select_main(&mut osd, &c, "Settings");
        // Back from Settings pops to Main (still open).
        assert_eq!(osd.handle(Nav::Back, &c), None);
        assert!(osd.is_open());
        // Back from Main closes/resumes.
        assert_eq!(osd.handle(Nav::Back, &c), Some(OsdAction::Resume));
    }

    /// Open the disc browser from Main, with a given directory listing.
    fn open_browser(osd: &mut Osd, c: &OsdCtx) {
        assert_eq!(select_main(osd, c, "Load Disc..."), None); // pushes browser
    }

    #[test]
    fn load_disc_item_lists_entries_dirs_get_a_slash() {
        let mut osd = Osd::new();
        osd.toggle();
        let mut c = ctx(true);
        c.browse_entries = vec![dir(".."), dir("saturn"), file("vf2.cue"), file("game.iso")];
        open_browser(&mut osd, &c);
        let items = osd.items(osd.screen(), &c);
        // Dirs carry a trailing '/', files don't; "Back" is last.
        assert_eq!(items[0].label, "../");
        assert_eq!(items[1].label, "saturn/");
        assert_eq!(items[2].label, "vf2.cue");
        assert_eq!(items[3].label, "game.iso");
        assert_eq!(items.last().unwrap().label, "Back");
    }

    #[test]
    fn browser_directory_emits_enter_and_resets_selection() {
        let mut osd = Osd::new();
        osd.toggle();
        let mut c = ctx(true);
        c.browse_entries = vec![dir(".."), dir("saturn"), file("vf2.cue")];
        open_browser(&mut osd, &c);
        osd.handle(Nav::Down, &c); // sel = 1 ("saturn/")
        // Selecting a directory emits BrowseEnter(idx), stays open, resets cursor.
        assert_eq!(osd.handle(Nav::Select, &c), Some(OsdAction::BrowseEnter(1)));
        assert_eq!(osd.sel(), 0, "selection resets for the rebuilt listing");
        assert!(osd.is_open());
    }

    #[test]
    fn browser_file_emits_load_disc() {
        let mut osd = Osd::new();
        osd.toggle();
        let mut c = ctx(true);
        c.browse_entries = vec![dir(".."), file("vf2.cue"), file("game.iso")];
        open_browser(&mut osd, &c);
        osd.handle(Nav::Down, &c); // sel = 1
        osd.handle(Nav::Down, &c); // sel = 2 ("game.iso")
        assert_eq!(osd.handle(Nav::Select, &c), Some(OsdAction::LoadDisc(2)));
    }

    #[test]
    fn browser_empty_offers_placeholder_then_back() {
        let mut osd = Osd::new();
        osd.toggle();
        let c = ctx(true); // browse_entries empty
        open_browser(&mut osd, &c);
        let items = osd.items(osd.screen(), &c);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "(no disc images)");
        // Selecting the placeholder just pops back to Main.
        assert_eq!(osd.handle(Nav::Select, &c), None);
        assert!(osd.is_open());
    }

    #[test]
    fn browser_back_pops_to_main() {
        let mut osd = Osd::new();
        osd.toggle();
        let mut c = ctx(true);
        c.browse_entries = vec![file("vf2.cue")];
        open_browser(&mut osd, &c);
        // "Back" is the last row.
        assert_eq!(select_main(&mut osd, &c, "Back"), None);
        assert!(osd.is_open());
        // Now back at Main: the next Back resumes.
        assert_eq!(osd.handle(Nav::Back, &c), Some(OsdAction::Resume));
    }

    #[test]
    fn browser_long_directory_scrolls_without_panic() {
        let mut osd = Osd::new();
        osd.toggle();
        let mut c = ctx(true);
        c.browse_entries = (0..40).map(|i| file(&format!("disc{i:02}.cue"))).collect();
        open_browser(&mut osd, &c);
        // Walk well past the viewport; the windowed draw must stay in bounds.
        for _ in 0..30 {
            osd.handle(Nav::Down, &c);
        }
        assert_eq!(osd.sel(), 30);
        let (w, h) = (320usize, 240usize);
        let mut buf = vec![0u8; w * h * 4];
        osd.render_overlay(&mut buf, w, h, &c); // must not panic / overflow
        assert!(
            buf.iter().any(|&b| b != 0),
            "scrolled browser paints pixels"
        );
    }

    #[test]
    fn dir_tail_keeps_the_current_folder() {
        assert_eq!(dir_tail("/games", 20), "/games");
        let long = "/home/user/roms/saturn/jp";
        let t = dir_tail(long, 20);
        assert_eq!(t.chars().count(), 20);
        assert!(t.starts_with(".."));
        assert!(t.ends_with("saturn/jp"));
    }
}
