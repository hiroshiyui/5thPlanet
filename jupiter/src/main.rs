//! 5thPlanet frontend.
//!
//! Two builds:
//!
//! * `cargo run -p jupiter -- BIOS.bin`
//!   (default features) — opens an SDL2 window, runs the Saturn at
//!   60 Hz, uploads each frame to a streaming texture. Quit with
//!   Esc or the window's close button.
//!
//! * `cargo run -p jupiter --no-default-features -- BIOS.bin`
//!   — headless. Runs a fixed number of frames and prints a short
//!   status report. Useful when libsdl2-dev isn't available, or
//!   for the BIOS-boot regression test that doesn't need a window.

use std::env;
use std::fs;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use saturn::cartridge::Cartridge;

// The OSD menu is pure logic (no sdl2): compile it for the SDL2 frontend and
// for tests (so its unit tests run even with `--no-default-features`), but not
// in a headless non-test build where nothing uses it.
mod config;
#[cfg(any(feature = "sdl2-frontend", test))]
mod osd;
#[cfg(any(feature = "sdl2-frontend", test))]
mod render_pipe;

/// Host wall-clock time as seconds since the Unix epoch (0 if the clock is
/// somehow before the epoch). Used to seed the Saturn RTC.
fn host_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Classify a master-SH-2 PC into a coarse memory region for the OSD's live
/// status readout (cache-through `0x2…` aliases fold onto the same regions).
#[cfg(feature = "sdl2-frontend")]
fn classify_pc(pc: u32) -> &'static str {
    match pc & 0x0FFF_FFFF {
        0x0000_0000..=0x000F_FFFF => "BIOS",
        0x0020_0000..=0x002F_FFFF => "Low WRAM",
        0x0600_0000..=0x060F_FFFF => "High WRAM (game)",
        _ => "other",
    }
}

/// Print one diagnostics section and return whether every check passed.
fn print_diag_section(title: &str, results: &[saturn::diagnostics::DiagOutcome]) -> bool {
    println!("{title}");
    let mut passed = 0usize;
    for o in results {
        let tag = if o.passed { passed += 1; "PASS" } else { "FAIL" };
        println!("  [{tag}] {}/{}  {}", o.category, o.name, o.detail);
    }
    println!("  {passed}/{} passed", results.len());
    passed == results.len()
}

/// Run the diagnostics (`jupiter doctor [<BIOS> [<disc>]]`) and return an exit
/// code: `0` if everything passed, `1` otherwise. The hermetic feature checks
/// always run (no BIOS/disc/window needed). If a BIOS is given, the heuristic
/// **System / boot-compatibility** checks also run against a fresh throwaway
/// machine booted from that media (+ the disc, if given).
fn run_doctor(bios_path: Option<String>, disc_path: Option<String>) -> ExitCode {
    let mut ok = print_diag_section("5thPlanet self-diagnostics:", &saturn::diagnostics::run_all());

    if let Some(bp) = bios_path {
        match fs::read(&bp) {
            Ok(bios) => {
                let region = detect_region(&bp, None);
                let disc = match disc_path {
                    Some(dp) => match load_image_disc(&dp) {
                        Ok(d) => Some(d),
                        Err(e) => {
                            eprintln!("doctor: disc load failed ({dp}): {e}");
                            ok = false;
                            None
                        }
                    },
                    None => None,
                };
                ok &= print_diag_section(
                    "System (boot/compatibility — heuristic):",
                    &saturn::diagnostics::run_system(bios, disc, region),
                );
            }
            Err(e) => {
                eprintln!("doctor: BIOS read failed ({bp}): {e}");
                ok = false;
            }
        }
    }

    if ok { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

fn main() -> ExitCode {
    // Split flags (`--cart=…`) from positional args (BIOS, disc).
    let mut positionals: Vec<String> = Vec::new();
    let mut cart_spec: Option<String> = None;
    let mut mouse_port: Option<u8> = None;
    for arg in env::args().skip(1) {
        if let Some(spec) = arg.strip_prefix("--cart=") {
            cart_spec = Some(spec.to_string());
        } else if arg == "--mouse" || arg == "--mouse=2" {
            mouse_port = Some(2);
        } else if arg == "--mouse=1" {
            mouse_port = Some(1);
        } else {
            positionals.push(arg);
        }
    }

    // `jupiter doctor [<BIOS> [<disc>]]` — run diagnostics and exit, before the
    // normal BIOS/disc/config handling. The hermetic checks need no media; an
    // optional BIOS (+ disc) adds the boot/compatibility checks.
    if positionals.first().map(String::as_str) == Some("doctor") {
        return run_doctor(positionals.get(1).cloned(), positionals.get(2).cloned());
    }

    let bios_path = match positionals.first() {
        Some(p) => p.clone(),
        None => {
            eprintln!(
                "usage: jupiter <BIOS.bin> [game.cue|.iso|.ccd | cdrom:<device>] [--cart=<kind>]\n       jupiter doctor [<BIOS> [<disc>]]   run diagnostics and exit (BIOS/disc add boot checks)"
            );
            eprintln!();
            eprintln!(
                "  cdrom:<device>         live optical drive (needs the physical-disc feature)"
            );
            eprintln!("  --cart=ram1m | ram4m   Extension DRAM cart (1 MiB / 4 MiB)");
            eprintln!("  --cart=bram[4|8|16|32] battery backup-RAM cart (Mbit; default 32)");
            eprintln!("  --cart=rom:<path>      game ROM cart image");
            eprintln!("  --mouse[=1|2]          Shuttle Mouse on port 2 (default) or 1;");
            eprintln!("                         host mouse + clicks, Return = mouse Start,");
            eprintln!("                         F10 = toggle pointer capture (Esc/OSD also releases)");
            eprintln!();
            eprintln!("BIOS images are gitignored — see bios/README.md for");
            eprintln!("naming conventions and the legal situation. Each");
            eprintln!("developer supplies their own legally-obtained dump.");
            return ExitCode::from(2);
        }
    };
    let bios = match fs::read(&bios_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to read {bios_path}: {e}");
            return ExitCode::from(1);
        }
    };
    if bios.len() != 512 * 1024 {
        eprintln!(
            "warning: BIOS image is {} bytes — Saturn BIOS is 512 KiB. \
             Continuing, but expect addressing oddities if it isn't a real dump.",
            bios.len()
        );
    }

    // Optional game disc, given as a spec: an image path (CUE/BIN, raw ISO, or
    // CloneCD CCD/IMG) or a live drive `cdrom:<device>` (needs the
    // `physical-disc` feature). Loaded in `run` via `insert_from_spec`.
    let disc_spec = positionals.get(1).cloned();

    // Persisted frontend settings (M9). A command-line flag beats the file.
    let cfg = config::Config::load();

    // Optional expansion cartridge: `--cart=` wins, else the config file.
    let cart = match cart_spec {
        Some(spec) => match parse_cart(&spec) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("bad --cart: {e}");
                return ExitCode::from(1);
            }
        },
        None => match cfg.cartridge.as_str() {
            "none" => Cartridge::None,
            tok => parse_cart(tok).unwrap_or_else(|e| {
                eprintln!("config: bad cartridge ({e}); using none");
                Cartridge::None
            }),
        },
    };

    // Sibling files for the quicksave state (`.state`) and the persisted
    // internal backup RAM / battery (`.bup`), keyed to the BIOS path.
    let save_base = std::path::PathBuf::from(&bios_path);

    // Shuttle Mouse: `--mouse[=1|2]` wins, else the config `mouse` token
    // (`off` / `1` / `2`), mirroring how `--cart=` overrides `cfg.cartridge`.
    let mouse_port = mouse_port.or_else(|| match cfg.mouse.as_str() {
        "1" => Some(1),
        "2" => Some(2),
        "off" | "" => None,
        other => {
            eprintln!("config: bad mouse ('{other}'); using off");
            None
        }
    });

    let region = detect_region(&bios_path, cfg.region.as_deref());
    run(bios, disc_spec, cart, save_base, region, mouse_port, cfg)
}

/// Pick the SMPC area (region) code. A `SAT_REGION` env var (`J`/`U`/`T`/`E`)
/// overrides; then a region persisted in the config file (the OSD Region
/// screen writes one); otherwise it's inferred from the BIOS filename
/// (`(JAP)` → Japan, `(EUR)` → Europe-PAL, else North America). The region
/// must be compatible with both the BIOS build and the disc's IP.BIN area
/// string, or the BIOS rejects the disc with "Game disc unsuitable for this
/// system".
fn detect_region(bios_path: &str, cfg_region: Option<&str>) -> u8 {
    use saturn::smpc::region;
    if let Ok(r) = std::env::var("SAT_REGION") {
        return match r.trim().to_ascii_uppercase().as_str() {
            "J" | "JP" | "JAPAN" => region::JAPAN,
            "T" | "ASIA" => region::ASIA_NTSC,
            "E" | "EU" | "EUR" | "EUROPE" | "PAL" => region::EUROPE_PAL,
            _ => region::NORTH_AMERICA,
        };
    }
    if let Some(tok) = cfg_region {
        return match tok {
            "japan" => region::JAPAN,
            "north-america" => region::NORTH_AMERICA,
            "europe-pal" => region::EUROPE_PAL,
            "asia-ntsc" => region::ASIA_NTSC,
            other => {
                eprintln!("config: unknown region '{other}'; autodetecting");
                detect_region(bios_path, None)
            }
        };
    }
    let up = bios_path.to_ascii_uppercase();
    if up.contains("JAP") || up.contains("(JP") {
        region::JAPAN
    } else if up.contains("EUR") || up.contains("(EU") {
        region::EUROPE_PAL
    } else {
        region::NORTH_AMERICA
    }
}

/// Parse a `--cart=` spec into a [`Cartridge`]. Accepts `ram1m`/`ram4m`
/// (Extension DRAM), `bram[4|8|16|32]` (battery backup RAM, in Mbit), and
/// `rom:<path>` (a game ROM image loaded from disk).
fn parse_cart(spec: &str) -> Result<Cartridge, String> {
    if let Some(path) = spec.strip_prefix("rom:") {
        let bytes = fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        return Ok(Cartridge::rom(bytes));
    }
    match spec {
        "ram1m" => Ok(Cartridge::ext_ram_1mb()),
        "ram4m" => Ok(Cartridge::ext_ram_4mb()),
        "bram" | "bram32" => Ok(Cartridge::backup_ram(0x0040_0000)),
        "bram4" => Ok(Cartridge::backup_ram(0x0008_0000)),
        "bram8" => Ok(Cartridge::backup_ram(0x0010_0000)),
        "bram16" => Ok(Cartridge::backup_ram(0x0020_0000)),
        other => Err(format!(
            "unknown cart kind '{other}' (use ram1m / ram4m / bram[4|8|16|32] / rom:<path>)"
        )),
    }
}

/// Find the BIOS images the OSD BIOS screen can power-cycle into: every
/// `.bin` of exactly 512 KiB in the launched image's directory, sorted.
/// Returns the list and the launched image's index in it (the launched path
/// is prepended if the scan somehow misses it, e.g. a non-`.bin` extension).
#[cfg(feature = "sdl2-frontend")]
fn scan_bios_images(launched: &std::path::Path) -> (Vec<std::path::PathBuf>, usize) {
    let dir = launched
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut list: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("bin"))
                && fs::metadata(&p).is_ok_and(|m| m.len() == 512 * 1024)
            {
                list.push(p);
            }
        }
    }
    list.sort();
    let canon = |p: &std::path::Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let target = canon(launched);
    match list.iter().position(|p| canon(p) == target) {
        Some(i) => (list, i),
        None => {
            list.insert(0, launched.to_path_buf());
            (list, 0)
        }
    }
}

/// Open a disc spec and insert it into the machine. A `cdrom:<device>` spec
/// opens a live optical drive (via the `physdisc` crate; errors without the
/// `physical-disc` feature); anything else is an image path. Both the launch
/// and the OSD "Insert Disc" re-insert go through here, so the source type
/// (image vs. live drive) stays in one place.
fn insert_from_spec(sat: &mut saturn::Saturn, spec: &str) -> Result<(), String> {
    if let Some(device) = spec.strip_prefix("cdrom:") {
        sat.insert_disc(physdisc::PhysicalDisc::open(device)?);
    } else {
        sat.insert_disc(load_image_disc(spec)?);
    }
    Ok(())
}

/// Load a disc image, picking the parser by file extension: `.iso` (raw
/// 2048-byte data track), `.cue` (CUE sheet + its `.bin`s), or `.ccd`
/// (CloneCD control file + sibling `.img`).
fn load_image_disc(path: &str) -> Result<saturn::disc::Disc, String> {
    use saturn::disc::Disc;
    use std::path::Path;

    let p = Path::new(path);
    let dir = p.parent().unwrap_or_else(|| Path::new("."));
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "iso" => Ok(Disc::from_iso(fs::read(p).map_err(|e| e.to_string())?)),
        "cue" => {
            let cue = fs::read_to_string(p).map_err(|e| e.to_string())?;
            Disc::from_cue(&cue, |name| fs::read(dir.join(name)).ok())
        }
        "ccd" => {
            let ccd = fs::read_to_string(p).map_err(|e| e.to_string())?;
            let img = p.with_extension("img");
            let bytes = fs::read(&img).map_err(|e| format!("{}: {e}", img.display()))?;
            Disc::from_ccd(&ccd, bytes)
        }
        other => Err(format!("unknown disc format '.{other}' (use .cue / .iso / .ccd)")),
    }
}

/// Game-frames to compute before the next render submit, given the current
/// audio reserve `depth` (source-rate queued bytes). Renders every game-frame
/// in normal play (1); only once the reserve has drained below `catchup_floor`
/// does it permit a `max`-frame "run N, show 1" catch-up, so a transient
/// compute spike can recover the reserve before an under-run *without*
/// chronically collapsing frames when there's headroom. The old unconditional
/// cap chased the full audio target and so dropped ~1/3 of rendered frames on
/// VF2 even at 6 ms/frame — cutting distinct fps and adding input latency
/// (`tmp/vf2_perflog`). `max` is the catch-up ceiling (`SAT_MAX_BURST`, ≥1).
fn burst_cap(depth: u32, catchup_floor: u32, max: u32) -> u32 {
    if depth < catchup_floor { max.max(1) } else { 1 }
}

#[cfg(feature = "sdl2-frontend")]
fn run(
    bios: Vec<u8>,
    disc_spec: Option<String>,
    cart: Cartridge,
    save_base: std::path::PathBuf,
    region: u8,
    mut mouse_port: Option<u8>,
    cfg: config::Config,
) -> ExitCode {
    use sdl2::audio::AudioSpecDesired;
    use sdl2::event::Event;
    use sdl2::keyboard::{Keycode, Scancode};
    use sdl2::pixels::PixelFormatEnum;

    use osd::{Nav, Osd, OsdCtx};

    use saturn::smpc::pad;

    use saturn::Saturn;
    use saturn::vdp2::{FRAME_HEIGHT, FRAME_WIDTH, FRAMEBUFFER_BYTES};

    let mut saturn = Saturn::new(bios);
    saturn.reset();
    saturn.set_region(region);
    // Plug the Shuttle Mouse into the requested port (None = power-on default,
    // pad on 1). On port 2 the keyboard pad stays on port 1; --mouse=1 replaces
    // the pad (the mouse's Start button is on the Return key either way).
    if mouse_port.is_some() {
        apply_mouse_port(&mut saturn, mouse_port);
    }
    // Seed the RTC from the host clock so the Saturn shows real wall-clock
    // time, like a console with a charged backup battery.
    saturn.set_rtc_unix(host_unix_secs());
    // Insert the launched disc (image or live drive); keep its spec so the OSD
    // "Insert Disc" can re-insert it after an eject.
    if let Some(spec) = &disc_spec
        && let Err(e) = insert_from_spec(&mut saturn, spec)
    {
        eprintln!("failed to load disc {spec}: {e}");
        return ExitCode::from(1);
    }
    let launched_spec = disc_spec;
    // Remember the launched config so the OSD Settings screens can mark the
    // active region / cartridge and re-apply it across a runtime change.
    let ui = UiState {
        scale: cfg.scale,
        fullscreen: cfg.fullscreen,
        region: region_code_to_osd(region),
        cart: cartridge_to_osd(&cart),
    };
    saturn.insert_cartridge(cart);

    // Save-state quickslot and the persisted battery (internal backup RAM),
    // both keyed to the BIOS path. F5/F9 use the former; the latter is the
    // console's "memory card", loaded here and written back on exit.
    let battery_path = save_base.with_extension("bup");
    if let Ok(bytes) = fs::read(&battery_path) {
        saturn.load_internal_backup(&bytes);
        eprintln!("loaded backup RAM from {}", battery_path.display());
    }

    let sdl = sdl2::init().expect("SDL2 init");
    let video = sdl.video().expect("SDL2 video subsystem");
    // Host game controllers (the SDL GameController API normalizes every
    // recognized pad — XInput on Windows, evdev on Linux — to one Xbox-style
    // layout). Devices are opened on hot-plug events; SDL also delivers an
    // Added event for each pad already attached at init.
    let controller_subsystem = sdl
        .game_controller()
        .expect("SDL2 game-controller subsystem");
    let mut controllers: Vec<sdl2::controller::GameController> = Vec::new();

    // SCSP audio: a 44.1 kHz stereo S16 queue the SCSP fills each frame.
    let audio = sdl.audio().expect("SDL2 audio subsystem");
    let audio_queue = audio
        .open_queue::<i16, _>(
            None,
            &AudioSpecDesired {
                freq: Some(44_100),
                channels: Some(2),
                samples: None,
            },
        )
        .expect("open audio queue");
    // Report what SDL actually opened. If you hear nothing, check this line: a
    // dummy/empty driver or a spec far from 44100/2/S16 means SDL didn't get a
    // working backend — try `SDL_AUDIODRIVER=pulseaudio` (or `pipewire`/`alsa`)
    // and confirm the app shows up in your system mixer.
    eprintln!(
        "SDL audio: driver={:?}, obtained spec={:?}",
        audio.current_audio_driver(),
        audio_queue.spec()
    );
    // The device may have opened at a different rate (PipeWire/Pulse commonly
    // force 48 kHz). A 48 kHz device consumes our 44.1 kHz stream ~9% fast:
    // the BGM pitches up and — worse — the pacing reserve never fills, so the
    // queue rides near 0 ms and every compute dip becomes an audible underrun
    // and a felt slowdown. Convert each chunk to the device rate before
    // queueing, and keep all pacing math in source (44.1 kHz) units by
    // scaling the mirror back.
    let dev_freq = audio_queue.spec().freq as u32;
    let resample: Option<sdl2::audio::AudioCVT> = if dev_freq != 44_100 {
        eprintln!("SDL audio: device at {dev_freq} Hz — resampling 44100 -> {dev_freq}");
        sdl2::audio::AudioCVT::new(
            sdl2::audio::AudioFormat::S16LSB,
            2,
            44_100,
            sdl2::audio::AudioFormat::S16LSB,
            2,
            dev_freq as i32,
        )
        .ok()
    } else {
        None
    };
    // Leave the device PAUSED (SDL opens queues paused) until the reserve has
    // filled once — see the prebuffer gate in the main loop. Resuming here would
    // let the device drain from t=0 while the queue is still empty during boot,
    // which under-runs exactly once on a cold start (the "first-play buzz").
    let window = video
        .window(
            "5thPlanet",
            FRAME_WIDTH as u32 * cfg.scale as u32,
            FRAME_HEIGHT as u32 * cfg.scale as u32,
        )
        .position_centered()
        .build()
        .expect("create window");
    let mut canvas = window
        .into_canvas()
        .present_vsync()
        .build()
        .expect("canvas");
    if cfg.fullscreen {
        let _ = canvas
            .window_mut()
            .set_fullscreen(sdl2::video::FullscreenType::Desktop);
    }
    let creator = canvas.texture_creator();
    // ABGR8888 is the SDL packed format whose in-memory byte order on
    // little-endian hosts (which is everything that matters in 2026)
    // is exactly [R, G, B, A] — what `Saturn::run_frame` writes.
    // RGBA8888 has the opposite byte order on LE; we'd have to swap
    // every pixel for no benefit.
    let mut texture = creator
        .create_texture_streaming(
            PixelFormatEnum::ABGR8888,
            FRAME_WIDTH as u32,
            FRAME_HEIGHT as u32,
        )
        .expect("create streaming texture");

    let event_subsystem = sdl.event().expect("SDL event subsystem");
    let mut events = sdl.event_pump().expect("event pump");
    let mut framebuffer = vec![0u8; FRAMEBUFFER_BYTES];
    let osd = Osd::new();
    // Render-pipeline worker: composites each frame on a second core from a
    // cloned VDP snapshot, overlapped with the next frame's emulation, so the
    // displayed rate rises from the compute+render rate toward compute-only.
    // The displayed frame trails the emulated frame by one (standard pipeline
    // latency); `framebuffer` is the main thread's display buffer, the pipe
    // holds the spare, and they swap each frame.
    let pipe = render_pipe::RenderPipe::new(FRAMEBUFFER_BYTES);
    // ------------------------------------------------------------------
    // Emu-thread decoupling: the Saturn (advance + audio pacing + OSD) runs
    // on a dedicated thread; this main thread keeps everything SDL —
    // events, audio queueing, texture upload, vsync present. The emulation
    // core itself stays strictly single-threaded; it just no longer shares
    // a thread with the ~4-5 ms SDL present cost, which together with a
    // ~12-15 ms frame sat exactly on the 16.7 ms vsync budget and tipped
    // half the iterations into the emulate-2-display-1 burst (the "smooth
    // but ~30 fps" regime).
    //
    //   main:  events → EmuIn msgs   audio chunks → SDL queue (+ depth
    //          mirror)               latest frame → texture → present
    //   emu:   pad/Nav/cmds ← msgs   advance_frame paced on the mirrored
    //          queue depth           render pipe → frame (+ OSD overlay)
    //
    // Audio pacing uses an AtomicU32 mirror of the SDL queue depth that the
    // main thread refreshes every iteration (the SDL audio handles are not
    // Send), plus the bytes produced inside the current burst — at most one
    // vsync stale, bounded by the burst cap, and self-correcting either way.
    let audio_ms: u64 = std::env::var("SAT_AUDIO_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120)
        .max(10);
    let audio_target_bytes = (176_400 * audio_ms / 1000) as u32;
    let mut audio_started = false;
    // Catch-up ceiling: the *most* game-frames `burst_cap` will collapse into
    // one render when the audio reserve has drained low. Normal play renders
    // every frame regardless (see `burst_cap`); this only bounds recovery from a
    // sub-real-time spike. Default 2 ("run 2, show 1" at worst); `SAT_MAX_BURST=1`
    // disables catch-up entirely (pure 1-frame-per-render).
    let max_frames_per_burst: u32 = std::env::var("SAT_MAX_BURST")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(2);
    // Reserve below which catch-up is allowed: a third of the target (~40 ms at
    // the default 120 ms). Normal play hovers near the target (VF2 median
    // ~77 ms), so collapsing only kicks in on a genuine dip toward an under-run.
    let catchup_floor = audio_target_bytes / 3;

    let perflog = std::env::var_os("SAT_PERFLOG").is_some();

    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, mpsc};

    // Saturn pad bits in the fixed `config::BUTTON_NAMES` binding order.
    const PAD_BITS: [u16; config::PAD_BUTTONS] = [
        pad::UP,
        pad::DOWN,
        pad::LEFT,
        pad::RIGHT,
        pad::A,
        pad::B,
        pad::C,
        pad::X,
        pad::Y,
        pad::Z,
        pad::L,
        pad::R,
        pad::START,
    ];
    // Resolve the configured scancode names; an unknown name falls back to
    // that button's default binding. Lives on the SDL thread (where the
    // keyboard is sampled); the emu thread keeps the names for the OSD.
    let mut keymap: [Scancode; config::PAD_BUTTONS] = std::array::from_fn(|i| {
        Scancode::from_name(&cfg.keys[i]).unwrap_or_else(|| {
            eprintln!(
                "config: unknown key '{}' for {}; using {}",
                cfg.keys[i],
                config::BUTTON_NAMES[i],
                config::DEFAULT_KEYS[i]
            );
            Scancode::from_name(config::DEFAULT_KEYS[i]).expect("default scancode name")
        })
    });

    let (emu_tx, emu_rx) = mpsc::channel::<EmuIn>();
    let (frame_tx, frame_rx) = mpsc::sync_channel::<(Vec<u8>, (usize, usize))>(2);
    let (recycle_tx, recycle_rx) = mpsc::channel::<Vec<u8>>();
    let (audio_tx, audio_rx) = mpsc::channel::<Vec<i16>>();
    let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();
    let audio_mirror = Arc::new(AtomicU32::new(0));
    let osd_open = Arc::new(AtomicBool::new(false));
    let quit_flag = Arc::new(AtomicBool::new(false));

    // Bundle the dispatcher-owned state for the emu thread (the keymap and
    // window above already consumed what they need from `cfg`).
    let (bios_paths, bios_active) = scan_bios_images(&save_base);
    let bios_names: Vec<String> = bios_paths
        .iter()
        .map(|p| p.file_stem().unwrap_or_default().to_string_lossy().into_owned())
        .collect();
    // The disc browser opens in the launched disc's directory (when it's an
    // image path), else the working directory.
    let browse_dir = launched_spec
        .as_deref()
        .filter(|s| !s.starts_with("cdrom:"))
        .and_then(|s| std::path::Path::new(s).parent())
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let mut sess = Session {
        save_base: save_base.clone(),
        launched_spec,
        ui,
        cfg,
        bios_paths,
        bios_names,
        bios_active,
        mouse_port,
        browse_dir,
        browse_entries: Vec::new(),
        diag_results: Vec::new(),
    };
    sess.refresh_browse();

    std::thread::scope(|scope| {
        let emu_mirror = Arc::clone(&audio_mirror);
        let emu_osd_open = Arc::clone(&osd_open);
        let emu_quit = Arc::clone(&quit_flag);
        // Returns the machine plus the final save base — a BIOS swap re-keys
        // the battery file mid-session, so the exit persist must follow it.
        let emu = scope.spawn(move || -> (Saturn, std::path::PathBuf) {
            let mut saturn = saturn;
            let mut osd = osd;
            let mut pipe = pipe;
            let mut sess = sess;
            let mut held = 0u16;
            // Frame buffers circulating emu → main → recycle; seed enough that
            // the pipe spare + the in-flight frame + the displayed frame never
            // starve the pool (a dip just skips one frame's hand-off).
            let mut pool: Vec<Vec<u8>> = vec![
                vec![0u8; FRAMEBUFFER_BYTES],
                vec![0u8; FRAMEBUFFER_BYTES],
            ];
            // The last composited frame, retained so the OSD freeze-screen can
            // dim/overlay it while emulation is paused (toast-free copy).
            let mut last_frame = vec![0u8; FRAMEBUFFER_BYTES];
            let mut last_dims = (FRAME_WIDTH, FRAME_HEIGHT);
            let mut pl_advance = std::time::Duration::ZERO;
            let mut pl_frames = 0u32;
            let mut pl_bursts = [0u32; 3];
            let mut pl_last = std::time::Instant::now();

            'emu: loop {
                if emu_quit.load(Ordering::Relaxed) {
                    break;
                }
                while let Ok(b) = recycle_rx.try_recv() {
                    pool.push(b);
                }
                let ctx = OsdCtx {
                    disc_present: saturn.has_disc(),
                    slot_used: std::array::from_fn(|n| sess.slot_path(n as u8).exists()),
                    scale: sess.ui.scale,
                    fullscreen: sess.ui.fullscreen,
                    region: sess.ui.region,
                    cart: sess.ui.cart,
                    mouse: mouse_port_to_osd(sess.mouse_port),
                    // pad_keys/bios_names are read only by the Controller/BIOS
                    // settings sub-screens (via `items()`, gated on the menu
                    // being open), so skip cloning them on the gameplay hot path
                    // — same rationale as browse_entries below. pad_keys is a
                    // fixed array, so the closed case is empty (non-allocating)
                    // strings rather than an empty Vec.
                    pad_keys: if osd.is_open() {
                        sess.cfg.keys.clone()
                    } else {
                        std::array::from_fn(|_| String::new())
                    },
                    bios_names: if osd.is_open() {
                        sess.bios_names.clone()
                    } else {
                        Vec::new()
                    },
                    bios_active: sess.bios_active,
                    // The browser listing can be large; it's only read while the
                    // menu is open, so skip cloning it on the gameplay hot path.
                    browse_entries: if osd.is_open() {
                        sess.browse_entries.clone()
                    } else {
                        Vec::new()
                    },
                    browse_dir: if osd.is_open() {
                        sess.browse_dir.to_string_lossy().into_owned()
                    } else {
                        String::new()
                    },
                    diag_results: if osd.is_open() {
                        sess.diag_results
                            .iter()
                            .map(|o| osd::DiagResultRow {
                                label: format!("{}/{}", o.category, o.name),
                                passed: o.passed,
                            })
                            .collect()
                    } else {
                        Vec::new()
                    },
                    cpu_pc: if osd.is_open() { saturn.master().regs.pc } else { 0 },
                    cpu_where: if osd.is_open() {
                        classify_pc(saturn.master().regs.pc)
                    } else {
                        ""
                    },
                };
                while let Ok(msg) = emu_rx.try_recv() {
                    match msg {
                        EmuIn::Pad(p) => held = p,
                        EmuIn::Mouse(dx, dy, buttons) => saturn.feed_mouse(dx, dy, buttons),
                        EmuIn::Toggle => {
                            let _ = osd.toggle();
                        }
                        EmuIn::Nav(nav) => {
                            if let Some(action) = osd.handle(nav, &ctx)
                                && dispatch_osd(action, &mut osd, &mut saturn, &mut sess, &ui_tx)
                            {
                                let _ = ui_tx.send(UiMsg::Quit);
                                break 'emu;
                            }
                        }
                        EmuIn::Quicksave => match fs::write(sess.state_path(), saturn.save_state())
                        {
                            Ok(()) => osd.set_toast("Quicksave", 90),
                            Err(e) => eprintln!("save state failed: {e}"),
                        },
                        EmuIn::Quickload => match fs::read(sess.state_path()) {
                            Ok(bytes) => match saturn.load_state(&bytes) {
                                Ok(()) => osd.set_toast("Quickload", 90),
                                Err(e) => eprintln!("load state failed: {e}"),
                            },
                            Err(e) => eprintln!("no state to load ({e})"),
                        },
                        #[cfg(debug_assertions)]
                        EmuIn::PlayCdda => {
                            if saturn.dbg_play_first_audio_track() {
                                osd.set_toast("Playing CD audio", 120);
                            } else {
                                osd.set_toast("No CD audio track", 120);
                            }
                        }
                        EmuIn::BindResult(b, captured) => {
                            osd.end_capture();
                            let i = b as usize % config::PAD_BUTTONS;
                            match captured {
                                Some(name) => {
                                    osd.set_toast(
                                        format!("{} = {name}", config::BUTTON_NAMES[i]),
                                        120,
                                    );
                                    sess.cfg.keys[i] = name;
                                    sess.cfg.save();
                                }
                                None => osd.set_toast("Rebind cancelled", 90),
                            }
                        }
                    }
                }
                emu_osd_open.store(osd.is_open(), Ordering::Relaxed);

                if osd.is_open() {
                    // Frozen: don't advance the machine or feed the pad.
                    // Composite the menu over the retained last frame.
                    saturn.set_pad1(0);
                    osd.tick_toast();
                    if let Some(mut buf) = pool.pop() {
                        buf.copy_from_slice(&last_frame);
                        let (w, h) = last_dims;
                        osd.render_overlay(&mut buf, w, h, &ctx);
                        if let Err(e) = frame_tx.try_send((buf, last_dims)) {
                            let (b, _) = match e {
                                mpsc::TrySendError::Full(v) => v,
                                mpsc::TrySendError::Disconnected(v) => v,
                            };
                            pool.push(b);
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(15));
                    continue;
                }

                saturn.set_pad1(held);
                // Audio-paced burst: run frames until the (mirrored) SDL queue
                // depth plus what this burst just produced reaches the target.
                // `burst_cap` renders every game-frame in normal play and only
                // collapses (run N, show 1) when the reserve has drained low —
                // chasing the full target unconditionally dropped ~1/3 of VF2's
                // frames and added input latency.
                let mut depth = emu_mirror.load(Ordering::Relaxed);
                let cap = burst_cap(depth, catchup_floor, max_frames_per_burst);
                let mut burst = 0u32;
                while depth < audio_target_bytes && burst < cap {
                    let t = std::time::Instant::now();
                    saturn.advance_frame();
                    pl_advance += t.elapsed();
                    let chunk = saturn.take_audio();
                    let bytes = (chunk.len() * 2) as u32;
                    depth += bytes;
                    // Credit the in-flight bytes into the shared mirror too: the
                    // main thread refreshes it only once per vsync, so without
                    // this every emu iteration in between re-reads the same stale
                    // depth and over-produces (~3% fast bursts, then an overshoot
                    // stall — visible as alternating fast/slow gameplay). The
                    // main thread's authoritative `store` re-anchors the mirror
                    // to the real SDL queue size each frame.
                    emu_mirror.fetch_add(bytes, Ordering::Relaxed);
                    let _ = audio_tx.send(chunk);
                    burst += 1;
                }
                pl_frames += burst;
                pl_bursts[(burst as usize).min(2)] += 1;

                // Collect the frame the worker rendered while we computed,
                // overlay any toast, and hand it to the main thread; then
                // dispatch this frame's render to overlap the next iteration.
                // Only when the machine actually advanced — the idle loop
                // (audio reserve full) spins at ~kHz and re-submitting the
                // same state would keep the worker re-rendering an identical
                // frame, burning a core and memory bandwidth for nothing.
                if burst > 0 {
                if let Some((mut rendered, dims)) = pipe.wait() {
                    last_frame.copy_from_slice(&rendered);
                    last_dims = dims;
                    osd.tick_toast();
                    osd.render_overlay(&mut rendered, dims.0, dims.1, &ctx);
                    match frame_tx.try_send((rendered, dims)) {
                        Ok(()) => {}
                        Err(mpsc::TrySendError::Full((b, _)))
                        | Err(mpsc::TrySendError::Disconnected((b, _))) => pool.push(b),
                    }
                }
                // Feed the pipe a spare whenever it lacks one — this must be
                // independent of the wait() result above: if a submit was ever
                // skipped for want of a spare, wait() yields None from then
                // on, and a recycle gated behind it would never run again
                // (the black-screen pipeline deadlock).
                if pipe.needs_spare()
                    && let Some(b) = pool.pop()
                {
                    pipe.recycle(b);
                }
                pipe.submit(&saturn);
                } else {
                    // Audio reserve full: nothing to do until it drains a bit.
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }

                if perflog && pl_last.elapsed().as_secs() >= 1 {
                    let total: u32 = pl_bursts.iter().sum();
                    eprintln!(
                        "EMU  frames/s={pl_frames} burst[0/1/2]={}/{}/{} | advance avg {:.2} ms/frame",
                        pl_bursts[0],
                        pl_bursts[1],
                        pl_bursts[2],
                        if pl_frames > 0 {
                            pl_advance.as_secs_f64() * 1e3 / pl_frames as f64
                        } else {
                            0.0
                        },
                    );
                    let _ = total;
                    pl_advance = std::time::Duration::ZERO;
                    pl_frames = 0;
                    pl_bursts = [0; 3];
                    pl_last = std::time::Instant::now();
                }
            }
            (saturn, sess.save_base)
        });

        // ---- SDL main loop: events, audio, present -----------------------
        let mut cur_dims = (FRAME_WIDTH, FRAME_HEIGHT);
        // OSD rebind: which pad button (if any) captures the next keypress.
        let mut rebind_target: Option<u8> = None;
        // Shuttle Mouse capture state (only used when --mouse is given).
        let (mut mouse_dx, mut mouse_dy) = (0i32, 0i32);
        let mut mouse_grabbed = false;
        // F10 toggles capture: released, the pointer behaves as a normal host
        // mouse again (motion still feeds the game while over the window —
        // "transparent" mode — but can leave it and hit the screen edge).
        let mut mouse_capture_enabled = true;
        let mut pl_present = std::time::Duration::ZERO;
        let mut pl_iters = 0u32;
        let mut pl_last = std::time::Instant::now();
        'main: loop {
            // (See the event-range flush note in the git history: SDL ≥ 2.28
            // emits display events 0.37's binding panics on.)
            event_subsystem.flush_events(0x201, 0x20F);
            for ev in events.poll_iter() {
                match ev {
                    Event::Quit { .. } => break 'main,
                    // An armed rebind owns the next keypress (before the OSD
                    // nav arm — the menu is open during capture). Esc cancels.
                    Event::KeyDown {
                        keycode: Some(kc),
                        scancode,
                        ..
                    } if rebind_target.is_some() => {
                        let b = rebind_target.take().expect("armed");
                        if kc == Keycode::Escape {
                            let _ = emu_tx.send(EmuIn::BindResult(b, None));
                        } else if let Some(sc) = scancode {
                            keymap[b as usize % config::PAD_BUTTONS] = sc;
                            let _ =
                                emu_tx.send(EmuIn::BindResult(b, Some(sc.name().to_string())));
                        } else {
                            // A key with no scancode: keep waiting.
                            rebind_target = Some(b);
                        }
                    }
                    Event::KeyDown {
                        keycode: Some(kc), ..
                    } if osd_open.load(Ordering::Relaxed) => {
                        let msg = match kc {
                            Keycode::Up => Some(EmuIn::Nav(Nav::Up)),
                            Keycode::Down => Some(EmuIn::Nav(Nav::Down)),
                            Keycode::Return | Keycode::Z => Some(EmuIn::Nav(Nav::Select)),
                            Keycode::Backspace | Keycode::X => Some(EmuIn::Nav(Nav::Back)),
                            Keycode::Escape => Some(EmuIn::Toggle),
                            _ => None,
                        };
                        if let Some(m) = msg {
                            let _ = emu_tx.send(m);
                        }
                    }
                    Event::KeyDown {
                        keycode: Some(Keycode::Escape),
                        ..
                    } => {
                        let _ = emu_tx.send(EmuIn::Toggle);
                    }
                    Event::KeyDown {
                        keycode: Some(Keycode::F5),
                        ..
                    } => {
                        let _ = emu_tx.send(EmuIn::Quicksave);
                    }
                    Event::KeyDown {
                        keycode: Some(Keycode::F9),
                        ..
                    } => {
                        let _ = emu_tx.send(EmuIn::Quickload);
                    }
                    // F8 CD-audio play is a debug-build-only diagnostic (it
                    // drives CD-DA outside the BIOS path); release builds omit it.
                    #[cfg(debug_assertions)]
                    Event::KeyDown {
                        keycode: Some(Keycode::F8),
                        ..
                    } => {
                        let _ = emu_tx.send(EmuIn::PlayCdda);
                    }
                    Event::MouseMotion { xrel, yrel, .. } if mouse_port.is_some() => {
                        mouse_dx += xrel;
                        mouse_dy += yrel;
                    }
                    Event::KeyDown {
                        keycode: Some(Keycode::F10),
                        ..
                    } if mouse_port.is_some() => {
                        mouse_capture_enabled = !mouse_capture_enabled;
                    }
                    // Game-controller hot-plug. `which` is a joystick index on
                    // Added but an instance id on Removed (SDL semantics); the
                    // dedupe guard makes a startup enumeration + the init-time
                    // Added events idempotent.
                    Event::ControllerDeviceAdded { which, .. } => {
                        match controller_subsystem.open(which) {
                            Ok(c) => {
                                if !controllers.iter().any(|o| o.instance_id() == c.instance_id())
                                {
                                    eprintln!("controller connected: {}", c.name());
                                    controllers.push(c);
                                }
                            }
                            Err(e) => eprintln!("controller open failed: {e}"),
                        }
                    }
                    Event::ControllerDeviceRemoved { which, .. } => {
                        controllers.retain(|c| c.instance_id() != which);
                    }
                    // Controller navigation of the open menu: D-pad moves, A
                    // selects, B backs out, Start toggles. Suppressed while a
                    // key-capture rebind is armed (that modal owns the input).
                    Event::ControllerButtonDown { button, .. }
                        if osd_open.load(Ordering::Relaxed) && rebind_target.is_none() =>
                    {
                        use sdl2::controller::Button;
                        let msg = match button {
                            Button::DPadUp => Some(EmuIn::Nav(Nav::Up)),
                            Button::DPadDown => Some(EmuIn::Nav(Nav::Down)),
                            Button::A => Some(EmuIn::Nav(Nav::Select)),
                            Button::B => Some(EmuIn::Nav(Nav::Back)),
                            Button::Start => Some(EmuIn::Toggle),
                            _ => None,
                        };
                        if let Some(m) = msg {
                            let _ = emu_tx.send(m);
                        }
                    }
                    _ => {}
                }
            }
            // Map the host keyboard to the port-1 digital pad through the
            // rebindable keymap (defaults: arrows = D-pad, Z/X/C = A/B/C,
            // A/S/D = X/Y/Z, Q/W = L/R, Enter = Start).
            let keys = events.keyboard_state();
            let mut held = 0u16;
            for (sc, bit) in keymap.iter().zip(PAD_BITS) {
                if keys.is_scancode_pressed(*sc) {
                    held |= bit;
                }
            }
            // Merge any attached game controllers into the same port-1 pad.
            // Fixed mapping on SDL's normalized Xbox-style layout, following
            // Sega's own Saturn-port convention: A/B/C = X/A/B (bottom arc),
            // X/Y/Z = Y/LB/RB, L/R = the analog triggers (thresholded), D-pad
            // or left stick = D-pad. Per-button gamepad rebind arrives with
            // the M13 E2 analog-peripheral work.
            {
                use sdl2::controller::{Axis, Button};
                const TH: i16 = 16384; // half of full deflection
                for c in &controllers {
                    for (btn, bit) in [
                        (Button::DPadUp, pad::UP),
                        (Button::DPadDown, pad::DOWN),
                        (Button::DPadLeft, pad::LEFT),
                        (Button::DPadRight, pad::RIGHT),
                        (Button::X, pad::A),
                        (Button::A, pad::B),
                        (Button::B, pad::C),
                        (Button::Y, pad::X),
                        (Button::LeftShoulder, pad::Y),
                        (Button::RightShoulder, pad::Z),
                        (Button::Start, pad::START),
                    ] {
                        if c.button(btn) {
                            held |= bit;
                        }
                    }
                    if c.axis(Axis::TriggerLeft) > TH {
                        held |= pad::L;
                    }
                    if c.axis(Axis::TriggerRight) > TH {
                        held |= pad::R;
                    }
                    let (lx, ly) = (c.axis(Axis::LeftX), c.axis(Axis::LeftY));
                    if lx < -TH {
                        held |= pad::LEFT;
                    } else if lx > TH {
                        held |= pad::RIGHT;
                    }
                    if ly < -TH {
                        held |= pad::UP;
                    } else if ly > TH {
                        held |= pad::DOWN;
                    }
                }
            }
            let _ = emu_tx.send(EmuIn::Pad(held));

            // Shuttle Mouse: this iteration's accumulated relative motion plus
            // the held buttons (host Left/Right/Middle clicks; Return doubles
            // as the mouse's Start button so --mouse=1 — no pad — can still
            // start/pause). Relative-mouse mode is enabled while the OSD is
            // closed, so the host cursor is captured + hidden and the game
            // draws its own; Esc (the OSD) releases the pointer.
            if mouse_port.is_some() {
                let want_grab =
                    mouse_capture_enabled && !osd_open.load(Ordering::Relaxed);
                if want_grab != mouse_grabbed {
                    mouse_grabbed = want_grab;
                    sdl.mouse().set_relative_mouse_mode(mouse_grabbed);
                }
                let ms = events.mouse_state();
                let mut buttons = 0u8;
                if ms.left() {
                    buttons |= saturn::smpc::mouse::LEFT;
                }
                if ms.right() {
                    buttons |= saturn::smpc::mouse::RIGHT;
                }
                if ms.middle() {
                    buttons |= saturn::smpc::mouse::MIDDLE;
                }
                if keys.is_scancode_pressed(Scancode::Return) {
                    buttons |= saturn::smpc::mouse::START;
                }
                let _ = emu_tx.send(EmuIn::Mouse(mouse_dx, mouse_dy, buttons));
                mouse_dx = 0;
                mouse_dy = 0;
            }

            // Queue the emu thread's audio (resampled to the device rate when
            // it differs) and refresh the depth mirror it paces on — in
            // SOURCE-rate units, so the 44.1 kHz pacing math holds on a
            // 48 kHz device. Start playback once the reserve has first
            // filled, so a cold start never drains an empty queue.
            while let Ok(chunk) = audio_rx.try_recv() {
                match &resample {
                    Some(cvt) => {
                        let mut bytes = Vec::with_capacity(chunk.len() * 2);
                        for s in &chunk {
                            bytes.extend_from_slice(&s.to_le_bytes());
                        }
                        let out = cvt.convert(bytes);
                        let samples: Vec<i16> = out
                            .chunks_exact(2)
                            .map(|b| i16::from_le_bytes([b[0], b[1]]))
                            .collect();
                        let _ = audio_queue.queue_audio(&samples);
                    }
                    None => {
                        let _ = audio_queue.queue_audio(&chunk);
                    }
                }
            }
            let src_size = (audio_queue.size() as u64 * 44_100 / dev_freq as u64) as u32;
            audio_mirror.store(src_size, Ordering::Relaxed);
            if !audio_started && src_size >= audio_target_bytes {
                audio_queue.resume();
                audio_started = true;
            }

            // Window-affecting OSD actions are applied here (the canvas is
            // not Send); Quit comes back the same way.
            let mut quit = false;
            while let Ok(m) = ui_rx.try_recv() {
                match m {
                    UiMsg::Scale(sc) => {
                        let _ = canvas.window_mut().set_size(
                            FRAME_WIDTH as u32 * sc as u32,
                            FRAME_HEIGHT as u32 * sc as u32,
                        );
                    }
                    UiMsg::Fullscreen(on) => {
                        use sdl2::video::FullscreenType;
                        let mode = if on {
                            FullscreenType::Desktop
                        } else {
                            FullscreenType::Off
                        };
                        let _ = canvas.window_mut().set_fullscreen(mode);
                    }
                    UiMsg::ArmRebind(b) => rebind_target = Some(b),
                    UiMsg::ResetKeymap => {
                        keymap = std::array::from_fn(|i| {
                            Scancode::from_name(config::DEFAULT_KEYS[i])
                                .expect("default scancode name")
                        });
                    }
                    UiMsg::SetMouse(port) => {
                        // Off (None) releases any active pointer grab next frame
                        // (the grab loop is gated on `mouse_port.is_some()`).
                        mouse_port = port;
                    }
                    UiMsg::Quit => quit = true,
                }
            }
            if quit {
                break 'main;
            }

            // Take the newest frame (recycling any it superseded), upload,
            // and present at the display rate.
            let mut newest = None;
            while let Ok(f) = frame_rx.try_recv() {
                if let Some((old, _)) = newest.replace(f) {
                    let _ = recycle_tx.send(old);
                }
            }
            if let Some((buf, dims)) = newest {
                let old = std::mem::replace(&mut framebuffer, buf);
                let _ = recycle_tx.send(old);
                if dims != cur_dims {
                    texture = creator
                        .create_texture_streaming(
                            PixelFormatEnum::ABGR8888,
                            dims.0 as u32,
                            dims.1 as u32,
                        )
                        .expect("recreate streaming texture");
                    cur_dims = dims;
                }
            }
            let t = std::time::Instant::now();
            let (w, _) = cur_dims;
            texture
                .update(None, &framebuffer, w * 4)
                .expect("upload framebuffer");
            canvas.clear();
            canvas.copy(&texture, None, None).expect("blit to canvas");
            canvas.present(); // present_vsync caps us at the display rate
            pl_present += t.elapsed();
            pl_iters += 1;
            if perflog && pl_last.elapsed().as_secs() >= 1 && pl_iters > 0 {
                eprintln!(
                    "MAIN iters/s={pl_iters} | present avg {:.2} ms | queue={}ms",
                    pl_present.as_secs_f64() * 1e3 / pl_iters as f64,
                    (audio_queue.size() as u64 * 44_100 / dev_freq as u64 / 176) as u32,
                );
                pl_present = std::time::Duration::ZERO;
                pl_iters = 0;
                pl_last = std::time::Instant::now();
            }
        }

        quit_flag.store(true, Ordering::Relaxed);
        // Unblock and join the emu thread, then persist the battery from the
        // final machine state — keyed to the final save base (a BIOS swap
        // re-keys it mid-session).
        let (saturn, final_base) = emu.join().expect("emu thread");
        let battery_path = final_base.with_extension("bup");
        if let Err(e) = fs::write(&battery_path, saturn.internal_backup()) {
            eprintln!(
                "failed to persist backup RAM to {}: {e}",
                battery_path.display()
            );
        }
        ExitCode::SUCCESS
    })
}

/// Messages from the SDL main thread to the emulation thread.
#[cfg(feature = "sdl2-frontend")]
enum EmuIn {
    /// Current held pad-1 bits (sampled from the keyboard each iteration).
    Pad(u16),
    /// OSD menu navigation (only sent while the menu is open).
    Nav(osd::Nav),
    /// Toggle the OSD menu.
    Toggle,
    /// F5/F9 quickslot save/load.
    Quicksave,
    Quickload,
    /// F8 (debug builds only): play the disc's first CD-DA track — a diagnostic
    /// that drives CD-DA outside the BIOS path.
    #[cfg(debug_assertions)]
    PlayCdda,
    /// Shuttle Mouse: motion since the last message (host convention,
    /// X+ right / Y+ down) + the held `saturn::smpc::mouse` button mask.
    Mouse(i32, i32, u8),
    /// A key-capture rebind finished on the SDL thread: the pad-button index
    /// and the captured scancode name (`None` = cancelled with Esc).
    BindResult(u8, Option<String>),
}

/// Messages from the emulation thread back to the SDL main thread —
/// window-affecting OSD actions (the canvas is not Send) and quit.
#[cfg(feature = "sdl2-frontend")]
enum UiMsg {
    Scale(u8),
    Fullscreen(bool),
    /// Capture the next host keypress for this pad button (OSD rebind).
    ArmRebind(u8),
    /// Restore the default keyboard→pad bindings.
    ResetKeymap,
    /// Move the Shuttle Mouse (or remove it): updates the SDL thread's capture
    /// gate so motion/clicks are fed only while a mouse port is active.
    SetMouse(Option<u8>),
    Quit,
}

/// Mutable frontend display/config state the Settings screens read (for their
/// active-item marks) and write (when the user changes a setting).
#[cfg(feature = "sdl2-frontend")]
struct UiState {
    scale: u8,
    fullscreen: bool,
    region: osd::OsdRegion,
    cart: osd::OsdCart,
}

/// Everything the emu thread's OSD dispatcher owns besides the machine: the
/// Settings mirrors, the persisted config, the launch disc spec, and the
/// save-file base path (`<bios>` → `.state` / `.<n>.state` / `.bup` siblings).
#[cfg(feature = "sdl2-frontend")]
struct Session {
    save_base: std::path::PathBuf,
    launched_spec: Option<String>,
    ui: UiState,
    cfg: config::Config,
    /// The swappable BIOS images beside the launched one (paths + display
    /// stems, index-matched) and which one is running. A swap re-keys
    /// `save_base` to the new image.
    bios_paths: Vec<std::path::PathBuf>,
    bios_names: Vec<String>,
    bios_active: usize,
    /// Which port carries the Shuttle Mouse (re-applied across a power cycle).
    mouse_port: Option<u8>,
    /// The disc browser's current directory and its (cached) listing — rebuilt
    /// only as the user navigates, not every frame.
    browse_dir: std::path::PathBuf,
    browse_entries: Vec<osd::BrowseEntry>,
    /// Last self-diagnostics results (Settings → Diagnostics → "Run all").
    diag_results: Vec<saturn::diagnostics::DiagOutcome>,
}

/// Disc-image extensions the browser offers (lower-cased compare).
#[cfg(feature = "sdl2-frontend")]
const DISC_EXTS: &[&str] = &["cue", "iso", "ccd"];

#[cfg(feature = "sdl2-frontend")]
impl Session {
    fn slot_path(&self, n: u8) -> std::path::PathBuf {
        self.save_base.with_extension(format!("{n}.state"))
    }
    fn state_path(&self) -> std::path::PathBuf {
        self.save_base.with_extension("state")
    }

    /// Rebuild `browse_entries` from `browse_dir`: `..` first (when not at the
    /// filesystem root), then sub-directories, then disc-image files — each
    /// group sorted case-insensitively. All `fs` errors degrade to an empty
    /// listing (the browser just shows "(no disc images)").
    fn refresh_browse(&mut self) {
        let mut dirs: Vec<String> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.browse_dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue; // skip dotfiles / dotdirs
                }
                match e.file_type() {
                    Ok(ft) if ft.is_dir() => dirs.push(name),
                    Ok(_) => {
                        let ext = std::path::Path::new(&name)
                            .extension()
                            .and_then(|x| x.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if DISC_EXTS.contains(&ext.as_str()) {
                            files.push(name);
                        }
                    }
                    Err(_) => {}
                }
            }
        }
        let key = |s: &String| s.to_ascii_lowercase();
        dirs.sort_by_key(key);
        files.sort_by_key(key);

        let mut entries = Vec::with_capacity(dirs.len() + files.len() + 1);
        if self.browse_dir.parent().is_some() {
            entries.push(osd::BrowseEntry { name: "..".into(), is_dir: true });
        }
        entries.extend(dirs.into_iter().map(|name| osd::BrowseEntry { name, is_dir: true }));
        entries.extend(files.into_iter().map(|name| osd::BrowseEntry { name, is_dir: false }));
        self.browse_entries = entries;
    }
}

#[cfg(feature = "sdl2-frontend")]
fn region_code_to_osd(code: u8) -> osd::OsdRegion {
    use saturn::smpc::region;
    match code {
        c if c == region::NORTH_AMERICA => osd::OsdRegion::NorthAmerica,
        c if c == region::EUROPE_PAL => osd::OsdRegion::EuropePal,
        c if c == region::ASIA_NTSC => osd::OsdRegion::AsiaNtsc,
        _ => osd::OsdRegion::Japan,
    }
}

/// The config-file token for a region (the OSD Region screen persists it).
#[cfg(feature = "sdl2-frontend")]
fn osd_region_to_token(r: osd::OsdRegion) -> &'static str {
    match r {
        osd::OsdRegion::Japan => "japan",
        osd::OsdRegion::NorthAmerica => "north-america",
        osd::OsdRegion::EuropePal => "europe-pal",
        osd::OsdRegion::AsiaNtsc => "asia-ntsc",
    }
}

/// The config-file token for a menu cartridge (same vocabulary as `--cart=`).
#[cfg(feature = "sdl2-frontend")]
fn osd_cart_to_token(c: osd::OsdCart) -> &'static str {
    match c {
        osd::OsdCart::None => "none",
        osd::OsdCart::ExtRam1M => "ram1m",
        osd::OsdCart::ExtRam4M => "ram4m",
        osd::OsdCart::BackupRam => "bram",
    }
}

#[cfg(feature = "sdl2-frontend")]
fn osd_region_to_code(r: osd::OsdRegion) -> u8 {
    use saturn::smpc::region;
    match r {
        osd::OsdRegion::Japan => region::JAPAN,
        osd::OsdRegion::NorthAmerica => region::NORTH_AMERICA,
        osd::OsdRegion::EuropePal => region::EUROPE_PAL,
        osd::OsdRegion::AsiaNtsc => region::ASIA_NTSC,
    }
}

/// Best-effort identification of an inserted cartridge for the Settings mark.
/// A ROM cart has no Settings equivalent (you can't re-create it from the menu),
/// so it reads as `None` — selecting a menu cart would replace it.
#[cfg(feature = "sdl2-frontend")]
fn cartridge_to_osd(c: &Cartridge) -> osd::OsdCart {
    match c {
        Cartridge::Dram { id, .. } if *id == 0x5C => osd::OsdCart::ExtRam4M,
        Cartridge::Dram { .. } => osd::OsdCart::ExtRam1M,
        Cartridge::Bram { .. } => osd::OsdCart::BackupRam,
        Cartridge::None | Cartridge::Rom { .. } => osd::OsdCart::None,
    }
}

#[cfg(feature = "sdl2-frontend")]
fn osd_cart_to_cartridge(c: osd::OsdCart) -> Cartridge {
    match c {
        osd::OsdCart::None => Cartridge::None,
        osd::OsdCart::ExtRam1M => Cartridge::ext_ram_1mb(),
        osd::OsdCart::ExtRam4M => Cartridge::ext_ram_4mb(),
        // The 32 Mbit default, matching `--cart=bram`.
        osd::OsdCart::BackupRam => Cartridge::backup_ram(0x0040_0000),
    }
}

/// The Shuttle Mouse port as the OSD names it (`None`/`1`/`2` ↔ Off/Port1/Port2).
#[cfg(feature = "sdl2-frontend")]
fn mouse_port_to_osd(port: Option<u8>) -> osd::OsdMouse {
    match port {
        Some(1) => osd::OsdMouse::Port1,
        Some(_) => osd::OsdMouse::Port2,
        None => osd::OsdMouse::Off,
    }
}

/// Inverse of [`mouse_port_to_osd`]: the port index the rest of the frontend
/// tracks (the config token and the SDL capture gate use this).
#[cfg(feature = "sdl2-frontend")]
fn osd_mouse_to_port(m: osd::OsdMouse) -> Option<u8> {
    match m {
        osd::OsdMouse::Off => None,
        osd::OsdMouse::Port1 => Some(1),
        osd::OsdMouse::Port2 => Some(2),
    }
}

/// Point the SMPC ports at the chosen mouse layout: port 1 mouse (no pad),
/// port 2 mouse (pad stays on 1), or off (the power-on default, pad on 1).
fn apply_mouse_port(saturn: &mut saturn::Saturn, port: Option<u8>) {
    use saturn::smpc::PortDevice::{Mouse, None as NoDev, Pad};
    let (p1, p2) = match port {
        Some(1) => (Mouse, NoDev),
        Some(_) => (Pad, Mouse),
        None => (Pad, NoDev),
    };
    saturn.set_port_devices(p1, p2);
}

/// Carry out a menu action against the running machine. Returns `true` if the
/// emulator should quit. Save-state slots live at `<bios>.<n>.state`.
#[cfg(feature = "sdl2-frontend")]
fn dispatch_osd(
    action: osd::OsdAction,
    osd: &mut osd::Osd,
    saturn: &mut saturn::Saturn,
    sess: &mut Session,
    ui_tx: &std::sync::mpsc::Sender<UiMsg>,
) -> bool {
    use osd::OsdAction;
    match action {
        OsdAction::Resume => osd.close(),
        OsdAction::Quit => return true,
        OsdAction::Reset => {
            saturn.reset();
            osd.set_toast("Reset", 120);
            osd.close();
        }
        OsdAction::Save(n) => match fs::write(sess.slot_path(n), saturn.save_state()) {
            Ok(()) => osd.set_toast(format!("Saved slot {n}"), 120),
            Err(e) => osd.set_toast(format!("Save failed: {e}"), 180),
        },
        OsdAction::Load(n) => match fs::read(sess.slot_path(n)) {
            Ok(bytes) => match saturn.load_state(&bytes) {
                Ok(()) => {
                    osd.set_toast(format!("Loaded slot {n}"), 120);
                    osd.close();
                }
                Err(e) => osd.set_toast(format!("Load failed: {e}"), 180),
            },
            Err(_) => osd.set_toast(format!("Slot {n} empty"), 120),
        },
        OsdAction::EjectDisc => {
            saturn.eject_disc();
            osd.set_toast("Disc ejected", 120);
        }
        OsdAction::ReinsertDisc => match &sess.launched_spec {
            Some(spec) => match insert_from_spec(saturn, spec) {
                Ok(()) => osd.set_toast("Disc inserted", 120),
                Err(e) => osd.set_toast(format!("Insert failed: {e}"), 180),
            },
            None => osd.set_toast("No disc to insert", 120),
        },
        OsdAction::BrowseEnter(i) => {
            // Descend into / ascend out of a directory, then rebuild the listing.
            if let Some(e) = sess.browse_entries.get(i).cloned() {
                if e.name == ".." {
                    if let Some(parent) = sess.browse_dir.parent() {
                        sess.browse_dir = parent.to_path_buf();
                    }
                } else {
                    sess.browse_dir.push(&e.name);
                }
                sess.refresh_browse();
            }
        }
        OsdAction::LoadDisc(i) => {
            // Resolve the chosen image, insert it, and power-cycle so the BIOS
            // boots the new game (region + cart re-applied across the reset,
            // mirroring SetRegion). The loaded disc becomes the session disc, so
            // a later Reset / Insert Disc references it.
            match sess.browse_entries.get(i) {
                Some(e) if !e.is_dir => {
                    let path = sess.browse_dir.join(&e.name);
                    let spec = path.to_string_lossy().into_owned();
                    match insert_from_spec(saturn, &spec) {
                        Ok(()) => {
                            saturn.reset();
                            saturn.set_region(osd_region_to_code(sess.ui.region));
                            saturn.insert_cartridge(osd_cart_to_cartridge(sess.ui.cart));
                            sess.launched_spec = Some(spec);
                            osd.set_toast(format!("Loaded {}", e.name), 150);
                            osd.close();
                        }
                        Err(err) => osd.set_toast(format!("Load failed: {err}"), 180),
                    }
                }
                _ => osd.set_toast("No such disc", 120),
            }
        }
        OsdAction::RunDiagnostics => {
            // Runs ~6 tiny SH-2 programs on throwaway machines (sub-millisecond),
            // synchronously here on the emu thread; it does NOT touch the live
            // `saturn`. Results surface on the next draw via OsdCtx; the screen
            // stays open. (`::saturn` — the param shadows the crate name.)
            sess.diag_results = ::saturn::diagnostics::run_all();
            let fails = sess.diag_results.iter().filter(|o| !o.passed).count();
            let msg = if fails == 0 {
                "Diagnostics: all passed".to_string()
            } else {
                format!("Diagnostics: {fails} failed")
            };
            osd.set_toast(msg, 150);
        }
        OsdAction::SetScale(s) => {
            sess.ui.scale = s;
            // Window pixels = base 320×224 × scale; applied on the SDL main
            // thread (the canvas is not Send). The canvas stretches the
            // texture to fill, so no texture re-create is needed.
            let _ = ui_tx.send(UiMsg::Scale(s));
            sess.cfg.scale = s;
            sess.cfg.save();
            osd.set_toast(format!("Scale {s}x"), 90);
        }
        OsdAction::ToggleFullscreen => {
            sess.ui.fullscreen = !sess.ui.fullscreen;
            let _ = ui_tx.send(UiMsg::Fullscreen(sess.ui.fullscreen));
            sess.cfg.fullscreen = sess.ui.fullscreen;
            sess.cfg.save();
            osd.set_toast(
                if sess.ui.fullscreen { "Fullscreen on" } else { "Fullscreen off" },
                90,
            );
        }
        OsdAction::SetRegion(r) => {
            // A region change is a hardware-level change: reset and re-apply the
            // boot config (region + current cart). The disc stays inserted, so
            // the machine re-boots from it under the new region.
            sess.ui.region = r;
            saturn.reset();
            saturn.set_region(osd_region_to_code(r));
            saturn.insert_cartridge(osd_cart_to_cartridge(sess.ui.cart));
            sess.cfg.region = Some(osd_region_to_token(r).to_string());
            sess.cfg.save();
            let name = match r {
                osd::OsdRegion::Japan => "Japan",
                osd::OsdRegion::NorthAmerica => "North America",
                osd::OsdRegion::EuropePal => "Europe (PAL)",
                osd::OsdRegion::AsiaNtsc => "Asia (NTSC)",
            };
            osd.set_toast(format!("Region: {name} (reset)"), 150);
            osd.close();
        }
        OsdAction::SetBios(i) => {
            let i = i as usize;
            let Some(path) = sess.bios_paths.get(i).cloned() else {
                osd.set_toast("No such BIOS image", 120);
                return false;
            };
            match fs::read(&path) {
                Err(e) => osd.set_toast(format!("BIOS read failed: {e}"), 180),
                Ok(bytes) => {
                    // Persist the outgoing machine's battery before it drops,
                    // then power-cycle into the new image, mirroring the
                    // launch path: region, ports, RTC, disc, cartridge. (A
                    // `--cart=rom:` cart has no Settings equivalent and is
                    // not re-created — same as the Cartridge screen.)
                    let old_bup = sess.save_base.with_extension("bup");
                    if let Err(e) = fs::write(&old_bup, saturn.internal_backup()) {
                        eprintln!("failed to persist backup RAM to {}: {e}", old_bup.display());
                    }
                    *saturn = saturn::Saturn::new(bytes);
                    saturn.reset();
                    saturn.set_region(osd_region_to_code(sess.ui.region));
                    apply_mouse_port(saturn, sess.mouse_port);
                    saturn.set_rtc_unix(host_unix_secs());
                    if let Some(spec) = &sess.launched_spec
                        && let Err(e) = insert_from_spec(saturn, spec)
                    {
                        osd.set_toast(format!("Disc re-insert failed: {e}"), 180);
                    }
                    saturn.insert_cartridge(osd_cart_to_cartridge(sess.ui.cart));
                    // Save files re-key to the new image; adopt its battery.
                    sess.save_base = path;
                    sess.bios_active = i;
                    if let Ok(b) = fs::read(sess.save_base.with_extension("bup")) {
                        saturn.load_internal_backup(&b);
                    }
                    osd.set_toast(
                        format!("BIOS: {} (power cycle)", sess.bios_names[i]),
                        150,
                    );
                    osd.close();
                }
            }
        }
        OsdAction::StartRebind(b) => {
            // Modal: the OSD shows "press a key" and swallows nav; the SDL
            // thread (which owns the keyboard) captures the next keypress and
            // answers with EmuIn::BindResult.
            osd.begin_capture(b);
            let _ = ui_tx.send(UiMsg::ArmRebind(b));
        }
        OsdAction::ResetBinds => {
            sess.cfg.keys = config::DEFAULT_KEYS.map(str::to_string);
            sess.cfg.save();
            let _ = ui_tx.send(UiMsg::ResetKeymap);
            osd.set_toast("Default keys restored", 120);
        }
        OsdAction::SetCartridge(k) => {
            sess.ui.cart = k;
            saturn.reset();
            saturn.set_region(osd_region_to_code(sess.ui.region));
            saturn.insert_cartridge(osd_cart_to_cartridge(k));
            sess.cfg.cartridge = osd_cart_to_token(k).to_string();
            sess.cfg.save();
            let name = match k {
                osd::OsdCart::None => "None",
                osd::OsdCart::ExtRam1M => "Ext RAM 1M",
                osd::OsdCart::ExtRam4M => "Ext RAM 4M",
                osd::OsdCart::BackupRam => "Backup RAM",
            };
            osd.set_toast(format!("Cartridge: {name} (reset)"), 150);
            osd.close();
        }
        OsdAction::SetMouse(m) => {
            // A peripheral swap, not a hardware reset: re-point the SMPC ports
            // live (the game re-reads devices on the next INTBACK poll), tell
            // the SDL thread so its mouse-capture gate follows, and persist.
            let port = osd_mouse_to_port(m);
            sess.mouse_port = port;
            apply_mouse_port(saturn, port);
            let _ = ui_tx.send(UiMsg::SetMouse(port));
            let (token, name) = match m {
                osd::OsdMouse::Off => ("off", "Off"),
                osd::OsdMouse::Port1 => ("1", "Port 1"),
                osd::OsdMouse::Port2 => ("2", "Port 2"),
            };
            sess.cfg.mouse = token.to_string();
            sess.cfg.save();
            osd.set_toast(format!("Mouse: {name}"), 120);
        }
    }
    false
}

#[cfg(not(feature = "sdl2-frontend"))]
fn run(
    bios: Vec<u8>,
    disc_spec: Option<String>,
    cart: Cartridge,
    save_base: std::path::PathBuf,
    region: u8,
    mouse_port: Option<u8>,
    // The config already shaped `cart`/`region` in `main`; headless has no
    // window or keymap, so nothing else applies.
    _cfg: config::Config,
) -> ExitCode {
    use saturn::Saturn;
    use saturn::vdp2::{FRAME_HEIGHT, FRAME_WIDTH, FRAMEBUFFER_BYTES};

    // ~3 s of virtual time at 60 Hz by default; `SAT_FRAMES` overrides it for
    // longer headless runs (e.g. to reach the BIOS disc check / game boot).
    let headless_frames: u32 = std::env::var("SAT_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(180);

    let mut saturn = Saturn::new(bios);
    saturn.reset();
    saturn.set_region(region);
    // Plug the Shuttle Mouse into the requested port (None = power-on default,
    // pad on 1). On port 2 the keyboard pad stays on port 1; --mouse=1 replaces
    // the pad (the mouse's Start button is on the Return key either way).
    if mouse_port.is_some() {
        apply_mouse_port(&mut saturn, mouse_port);
    }
    // Seed the RTC from the host clock so the Saturn shows real wall-clock
    // time, like a console with a charged backup battery.
    saturn.set_rtc_unix(host_unix_secs());
    if let Some(spec) = &disc_spec
        && let Err(e) = insert_from_spec(&mut saturn, spec)
    {
        eprintln!("failed to load disc {spec}: {e}");
        return ExitCode::from(1);
    }
    saturn.insert_cartridge(cart);

    // Persist the internal backup RAM ("battery") across headless runs too.
    let battery_path = save_base.with_extension("bup");
    if let Ok(bytes) = fs::read(&battery_path) {
        saturn.load_internal_backup(&bytes);
    }

    let mut framebuffer = vec![0u8; FRAMEBUFFER_BYTES];

    // Optional: load a save state captured in the SDL frontend (F5), so the
    // trace hooks below operate on *that* machine rather than a fresh boot —
    // e.g. to diagnose where a game halts after a manual launch the headless
    // path can't reach. The BIOS + disc inserted above are re-grafted into the
    // loaded state by `load_state`. Prints where both SH-2s are parked.
    if let Ok(path) = std::env::var("SAT_LOADSTATE") {
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("read save state {path} failed: {e}");
                return ExitCode::from(1);
            }
        };
        if let Err(e) = saturn.load_state(&bytes) {
            eprintln!("load save state {path} failed: {e:?}");
            return ExitCode::from(1);
        }
        let m = saturn.master();
        let s = saturn.slave();
        eprintln!(
            "loaded save state {path}\n  master: PC={:08X} PR={:08X} SR={:08X} GBR={:08X} R15={:08X} halted={}\n  slave : PC={:08X} PR={:08X} SR={:08X} GBR={:08X} R15={:08X} halted={}",
            m.regs.pc,
            m.regs.pr,
            m.regs.sr.0,
            m.regs.gbr,
            m.regs.r[15],
            saturn.master_is_halted(),
            s.regs.pc,
            s.regs.pr,
            s.regs.sr.0,
            s.regs.gbr,
            s.regs.r[15],
            saturn.slave_is_halted(),
        );
    }

    // Optional scripted port-1 pad input: `SAT_PAD=0xBITS` (saturn::smpc::pad
    // mask) held over the frame window [`SAT_PAD_FROM`, `SAT_PAD_TO`) — lets a
    // headless run drive the BIOS menu (e.g. tap Start at the multiplayer screen
    // to launch the highlighted disc) so the manual-launch path can be traced.
    // Applied in every per-frame loop below via `apply_scripted_pad`.
    let scripted_pad: Option<u16> = std::env::var("SAT_PAD")
        .ok()
        .and_then(|s| u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
    let pad_from: u32 = std::env::var("SAT_PAD_FROM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let pad_to: u32 = std::env::var("SAT_PAD_TO")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(u32::MAX);
    let apply_scripted_pad = |saturn: &mut saturn::Saturn, f: u32| {
        if let Some(bits) = scripted_pad {
            saturn.set_pad1(if (pad_from..pad_to).contains(&f) {
                bits
            } else {
                0
            });
        }
    };

    // Optional instruction breakpoint: `SAT_BP=0xADDR` (opt `SAT_BP_FRAME=N`
    // fast-forwards N frames first) single-steps the master until PC==ADDR,
    // then dumps R0..R15 + the words at [R3]/[R4] (boot poll-loop debugging).
    if let Ok(bps) = std::env::var("SAT_BP") {
        use sh2::bus::{AccessKind, Bus};
        let bp = u32::from_str_radix(bps.trim().trim_start_matches("0x"), 16).unwrap_or(0);
        let ff: u32 = std::env::var("SAT_BP_FRAME")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(70);
        for _ in 0..ff {
            saturn.run_frame(&mut framebuffer);
        }
        let mut hit = false;
        for _ in 0..200_000_000u64 {
            saturn.debug_step_master();
            if saturn.master().regs.pc == bp {
                let r = saturn.master().regs.r;
                eprintln!("BP {bp:08X} hit. regs:");
                for (i, v) in r.iter().enumerate() {
                    eprintln!("  R{i:<2}= {v:08X}");
                }
                let (w3, _) = saturn.bus.read32(r[3], AccessKind::Data);
                let (w4, _) = saturn.bus.read32(r[4], AccessKind::Data);
                eprintln!(
                    "  [R3={:08X}]= {w3:08X}   [R4={:08X}]= {w4:08X}",
                    r[3], r[4]
                );
                hit = true;
                break;
            }
        }
        if !hit {
            eprintln!("BP {bp:08X} not hit");
        }
        return ExitCode::SUCCESS;
    }

    // Optional full-speed breakpoint capture: `SAT_FBP=0xADDR` arms a master
    // breakpoint that snapshots R0..R15 + 96 bytes of code at the instant the
    // PC is reached (works for transient work-RAM routines that single-step /
    // post-frame dumps miss), runs until it fires, then prints regs + disasm.
    if let Ok(s) = std::env::var("SAT_FBP") {
        let bp = u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap_or(0);
        // SAT_SLAVE_FBP=1 arms the breakpoint on the slave SH-2 instead.
        let on_slave = std::env::var_os("SAT_SLAVE_FBP").is_some();
        if on_slave {
            saturn.set_slave_bp(bp);
        } else {
            saturn.set_master_bp(bp);
        }
        // Optional: keep scanning hits until register R<SAT_FBP_RREG> lands in
        // [SAT_FBP_RLO, SAT_FBP_RHI) — to find the specific call (e.g. the
        // memory-fill whose destination R3 overlaps the loaded program).
        let rreg: Option<usize> = std::env::var("SAT_FBP_RREG")
            .ok()
            .and_then(|s| s.parse().ok());
        let rlo = std::env::var("SAT_FBP_RLO")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        let rhi = std::env::var("SAT_FBP_RHI")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(u32::MAX);
        let mut hit = None;
        for f in 0..headless_frames {
            apply_scripted_pad(&mut saturn, f);
            saturn.run_frame(&mut framebuffer);
            let h = if on_slave {
                saturn.take_slave_bp_hit()
            } else {
                saturn.take_master_bp_hit()
            };
            if let Some(h) = h {
                let matched = rreg.is_none_or(|r| (rlo..rhi).contains(&h.regs[r]));
                if matched {
                    hit = Some(h);
                    break;
                }
                // Re-arm and keep scanning for the call we want.
                if on_slave {
                    saturn.set_slave_bp(bp);
                } else {
                    saturn.set_master_bp(bp);
                }
            }
        }
        match hit {
            Some(h) => {
                let (r, pr, gbr, code) = (h.regs, h.pr, h.gbr, &h.code);
                eprintln!("FBP {bp:08X} hit. PR={pr:08X} GBR={gbr:08X} regs:");
                for (i, v) in r.iter().enumerate() {
                    eprintln!("  R{i:<2}= {v:08X}");
                }
                eprintln!("disasm:");
                for (i, &w) in code.iter().enumerate() {
                    let op = sh2::decoder::decode(w);
                    eprintln!(
                        "  {:08X}: {w:04X}  {}",
                        bp + (i as u32) * 2,
                        sh2::debug::disasm(op)
                    );
                }
            }
            None => eprintln!("FBP {bp:08X} not hit"),
        }
        return ExitCode::SUCCESS;
    }

    // Optional full-speed in-loop master-PC trace: `SAT_INLOOP=1` records every
    // master instruction's PC at run_frame speed (faithful interrupt timing),
    // running until the master enters the work-RAM shell region (0x0602_0000+)
    // or SAT_FRAMES, then prints the tail of the trace — the boot give-up branch.
    if std::env::var_os("SAT_INLOOP").is_some() {
        // Stop when the master reaches the work-RAM region the BIOS gives up
        // into, or an explicit `SAT_INLOOP_STOP=0xPC`, so the give-up branch is
        // the tail of the ring rather than being evicted by the destination's
        // own loop flooding it.
        let idle = std::env::var("SAT_INLOOP_STOP")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
        // `SAT_SHELL_BASE` overrides the give-up detection base (default
        // 0x0602_0000). The CD-boot *loader* legitimately runs in
        // 0x0602_xxxx/0x0603_xxxx, so set it to 0x0604_0000 to trace *through*
        // the loader and stop only at the CD-player give-up loop.
        let shell_base = std::env::var("SAT_SHELL_BASE")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x0602_0000);
        saturn.enable_master_pc_trace();
        // Freeze the ring at the give-up region (not the default 0x0602_0000,
        // which would freeze the moment the loader itself enters work RAM), so
        // the ring tail is the loader code right before the give-up.
        saturn.set_master_trace_freeze(shell_base, 0x0605_0000);
        let mut triggered = None;
        for f in 0..headless_frames {
            apply_scripted_pad(&mut saturn, f);
            saturn.run_frame(&mut framebuffer);
            let pc = saturn.master().regs.pc;
            let hit_shell = (shell_base..0x0605_0000).contains(&pc);
            if hit_shell || Some(pc) == idle {
                triggered = Some((f, pc));
                break;
            }
        }
        let trace = saturn.take_master_pc_trace();
        let n: usize = std::env::var("SAT_INLOOP_TAIL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(400);
        let head = std::env::var_os("SAT_INLOOP_HEAD").is_some();
        eprintln!("in-loop trace: {} PCs, stop={triggered:?}", trace.len());
        if head {
            for pc in trace.iter().take(n) {
                eprintln!("PC {pc:08X}");
            }
        } else {
            let tail = trace.len().saturating_sub(n);
            for pc in &trace[tail..] {
                eprintln!("PC {pc:08X}");
            }
        }
        return ExitCode::SUCCESS;
    }

    // Optional fine BIOS-ROM trace: `SAT_BIOSTRACE=1` fast-forwards
    // `SAT_BP_FRAME` frames, then single-steps logging every master-PC change
    // (deduped) until PC enters the work-RAM shell region (0x0602_0000+) or the
    // step cap, to capture the boot give-up branch path.
    if std::env::var_os("SAT_BIOSTRACE").is_some() {
        let ff: u32 = std::env::var("SAT_BP_FRAME")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(220);
        for _ in 0..ff {
            saturn.run_frame(&mut framebuffer);
        }
        let mut prev = u32::MAX;
        let mut printed = 0u32;
        for _ in 0..40_000_000u64 {
            saturn.debug_step_master();
            let pc = saturn.master().regs.pc;
            // Skip the master idle poll (0x2B0..0x2B6) so the VBlank-handler
            // bursts that actually run the boot state machine are visible.
            let idle = (0x0000_02B0..=0x0000_02B6).contains(&pc);
            if pc != prev && !idle {
                eprintln!("PC {pc:08X}");
                prev = pc;
                printed += 1;
                if printed > 4000 {
                    break;
                }
                if (0x0602_0000..0x0605_0000).contains(&pc) {
                    eprintln!("(entered shell region)");
                    break;
                }
            }
        }
        return ExitCode::SUCCESS;
    }

    // Optional master-PC trace for boot debugging: `SAT_PCTRACE=1` prints the
    // master SH-2 PC once per frame (collapsing runs of the same value), so a
    // boot can be located in time — BIOS ROM (0x0000_0000), work-RAM shell/game
    // (0x0600_0000+), etc. — without per-instruction overhead.
    let pctrace = std::env::var_os("SAT_PCTRACE").is_some();
    // Debug: `SAT_CACHE_PURGE=1` purges both SH-2 I-caches each frame, to test
    // whether a stale-cache fetch is the blocker (if a game runs past a spurious
    // illegal-instruction fault only with this on, the cache is incoherent).
    let cache_purge = std::env::var_os("SAT_CACHE_PURGE").is_some();
    // Debug: `SAT_SLOW_FETCH=N` charges N extra stall cycles per instruction-fetch
    // cache hit on both SH-2s — a timing-probe to test inter-CPU-race hypotheses
    // (changes timing only, no cache value/content change). 0 = off.
    let slow_fetch: u32 = std::env::var("SAT_SLOW_FETCH").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut last_pc = u32::MAX;
    let mut dump_dims = (FRAME_WIDTH, FRAME_HEIGHT);
    for f in 0..headless_frames {
        apply_scripted_pad(&mut saturn, f);
        if cache_purge {
            saturn.master_mut().cache.purge();
            saturn.slave_mut().cache.purge();
        }
        if slow_fetch != 0 {
            saturn.master_mut().dbg_slow_fetch = slow_fetch;
            saturn.slave_mut().dbg_slow_fetch = slow_fetch;
        }
        dump_dims = saturn.run_frame(&mut framebuffer);
        if pctrace {
            let pc = saturn.master().regs.pc;
            if pc != last_pc {
                eprintln!("frame {f:4} master PC=0x{pc:08X}");
                last_pc = pc;
            }
        }
    }

    if std::env::var_os("SAT_IRQ_DUMP").is_some() {
        use sh2::bus::{AccessKind, Bus};
        let imask = saturn.master().regs.sr.imask();
        let pc = saturn.master().regs.pc;
        let ims = saturn.bus.scu.ims;
        let ist = saturn.bus.scu.ist;
        let mask348 = saturn.bus.read32(0x0600_0348, AccessKind::Data).0;
        let tvstat = saturn.bus.read16(0x0500_0004, AccessKind::Data).0;
        let gbr = saturn.master().regs.gbr;
        let tvmd = saturn.bus.read16(0x05F8_0000, AccessKind::Data).0;
        eprintln!(
            "IRQ: PC={pc:08X} SR.imask={imask} SCU.IMS={ims:08X} SCU.IST={ist:08X} \
             [0x06000348]={mask348:08X} VDP2.TVSTAT={tvstat:04X} GBR={gbr:08X} VDP2.TVMD={tvmd:04X}"
        );
    }

    if let Err(e) = fs::write(&battery_path, saturn.internal_backup()) {
        eprintln!(
            "failed to persist backup RAM to {}: {e}",
            battery_path.display()
        );
    }

    // Optional raw memory dump: `SAT_MEMDUMP=0xADDR:N` reads N bytes via the
    // live bus and prints them as hex + ASCII (e.g. to verify IP.BIN landed in
    // work-RAM intact vs the disc).
    if let Ok(spec) = std::env::var("SAT_MEMDUMP") {
        use sh2::bus::{AccessKind, Bus};
        let (a, n) = spec.split_once(':').unwrap_or((spec.as_str(), "256"));
        let base = u32::from_str_radix(a.trim().trim_start_matches("0x"), 16).unwrap_or(0);
        let n: u32 = n.parse().unwrap_or(256);
        for row in 0..n.div_ceil(16) {
            let addr = base + row * 16;
            let bytes: Vec<u8> = (0..16)
                .map(|i| saturn.bus.read8(addr + i, AccessKind::Data).0)
                .collect();
            let hex: String = bytes.iter().map(|b| format!("{b:02x} ")).collect();
            let asc: String = bytes
                .iter()
                .map(|&b| {
                    if (0x20..0x7f).contains(&b) {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            eprintln!("{addr:08X}  {hex} {asc}");
        }
        return ExitCode::SUCCESS;
    }

    // Optional disassembly window for boot debugging: `SAT_DISASM=0xADDR:N`
    // decodes N SH-2 instructions from `addr` via the live bus (e.g. to inspect
    // a work-RAM wait loop the boot stalls in).
    if let Ok(spec) = std::env::var("SAT_DISASM") {
        use sh2::bus::{AccessKind, Bus};
        for region in spec.split(',') {
            let (a, n) = region.split_once(':').unwrap_or((region, "16"));
            let base = u32::from_str_radix(a.trim().trim_start_matches("0x"), 16).unwrap_or(0);
            let n: u32 = n.parse().unwrap_or(16);
            eprintln!("--- disasm {base:08X}..+{n} ---");
            for i in 0..n {
                let addr = base + i * 2;
                let (word, _) = saturn.bus.read16(addr, AccessKind::Data);
                let op = sh2::decoder::decode(word);
                eprintln!("  {addr:08X}: {word:04X}  {}", sh2::debug::disasm(op));
            }
        }
    }

    // Optional framebuffer snapshot for headless boot debugging: `SAT_DUMP=path`
    // writes the final frame as a binary PPM (P6) — lets a boot run be inspected
    // (CD-player vs splash vs game) without opening a window.
    if let Ok(path) = std::env::var("SAT_DUMP") {
        let (w, h) = dump_dims;
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        for px in framebuffer[..w * h * 4].chunks_exact(4) {
            ppm.extend_from_slice(&px[..3]); // RGBA → RGB
        }
        match fs::write(&path, &ppm) {
            Ok(()) => eprintln!("wrote framebuffer to {path}"),
            Err(e) => eprintln!("failed to write framebuffer to {path}: {e}"),
        }
    }

    let master_pc = saturn.master().regs.pc;
    let cycles = saturn.master().pipeline.cycles;
    let slave_pc = saturn.slave().regs.pc;
    let slave_halted = saturn.slave_is_halted();
    println!(
        "headless run complete: master PC=0x{master_pc:08X}, cycles={cycles}, frames={headless_frames}; slave PC=0x{slave_pc:08X} halted={slave_halted}"
    );
    if let Some((vec, pc)) = saturn.master().last_fault {
        use sh2::bus::{AccessKind, Bus};
        print!("  master last CPU exception: vector={vec} (0x{vec:02X}) at PC=0x{pc:08X}");
        if let Some(w) = saturn.master().last_illegal_word {
            let (bus_w, _) = saturn.bus.read16(pc, AccessKind::Data);
            print!(" — fetched word=0x{w:04X}, external-memory word=0x{bus_w:04X}");
            if w != bus_w {
                print!("  *** MISMATCH (stale I-cache) ***");
            }
        }
        println!();
    }
    if let Some((vec, pc)) = saturn.slave().last_fault {
        println!("  slave  last CPU exception: vector={vec} (0x{vec:02X}) at PC=0x{pc:08X}");
    }
    // SCU interrupt-vector table dump for boot debugging: print VBR and the
    // master's exception-vector entries the SCU sources use (0x40 VBlank-IN ..
    // 0x4D Sprite-Draw-End), so an unhandled-interrupt park can be traced to
    // which vector the BIOS left pointing at its default handler.
    if std::env::var_os("SAT_VEC_DUMP").is_some() {
        use sh2::bus::{AccessKind, Bus};
        let vbr = saturn.master().regs.vbr;
        eprint!("VBR={vbr:08X}");
        for v in 0x40u32..=0x4D {
            let (h, _) = saturn.bus.read32(vbr.wrapping_add(v * 4), AccessKind::Data);
            eprint!(" [{v:02X}]={h:08X}");
        }
        eprintln!();
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::burst_cap;

    // Healthy reserve (well above the floor) → render every frame, never
    // collapse — the VF2-steady-60 case (~77 ms vs a ~40 ms floor).
    #[test]
    fn renders_every_frame_with_reserve() {
        assert_eq!(burst_cap(80_000, 7_056, 2), 1);
        assert_eq!(burst_cap(7_056, 7_056, 2), 1); // exactly at the floor: not below
    }

    // Reserve drained below the floor → allow up to `max` to recover before an
    // under-run.
    #[test]
    fn allows_catchup_when_drained() {
        assert_eq!(burst_cap(5_000, 7_056, 2), 2);
        assert_eq!(burst_cap(0, 7_056, 3), 3);
    }

    // `SAT_MAX_BURST=1` disables catch-up: 1 frame per render even when drained.
    #[test]
    fn max_one_disables_catchup() {
        assert_eq!(burst_cap(0, 7_056, 1), 1);
        assert_eq!(burst_cap(80_000, 7_056, 1), 1);
        // A degenerate max of 0 still computes at least one frame.
        assert_eq!(burst_cap(0, 7_056, 0), 1);
    }
}
