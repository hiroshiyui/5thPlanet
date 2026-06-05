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
#[cfg(any(feature = "sdl2-frontend", test))]
mod osd;

/// Host wall-clock time as seconds since the Unix epoch (0 if the clock is
/// somehow before the epoch). Used to seed the Saturn RTC.
fn host_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn main() -> ExitCode {
    // Split flags (`--cart=…`) from positional args (BIOS, disc).
    let mut positionals: Vec<String> = Vec::new();
    let mut cart_spec: Option<String> = None;
    for arg in env::args().skip(1) {
        if let Some(spec) = arg.strip_prefix("--cart=") {
            cart_spec = Some(spec.to_string());
        } else {
            positionals.push(arg);
        }
    }

    let bios_path = match positionals.first() {
        Some(p) => p.clone(),
        None => {
            eprintln!(
                "usage: jupiter <BIOS.bin> [game.cue|.iso|.ccd | cdrom:<device>] [--cart=<kind>]"
            );
            eprintln!();
            eprintln!(
                "  cdrom:<device>         live optical drive (needs the physical-disc feature)"
            );
            eprintln!("  --cart=ram1m | ram4m   Extension DRAM cart (1 MiB / 4 MiB)");
            eprintln!("  --cart=bram[4|8|16|32] battery backup-RAM cart (Mbit; default 32)");
            eprintln!("  --cart=rom:<path>      game ROM cart image");
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

    // Optional expansion cartridge.
    let cart = match cart_spec {
        Some(spec) => match parse_cart(&spec) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("bad --cart: {e}");
                return ExitCode::from(1);
            }
        },
        None => Cartridge::None,
    };

    // Sibling files for the quicksave state (`.state`) and the persisted
    // internal backup RAM / battery (`.bup`), keyed to the BIOS path.
    let save_base = std::path::PathBuf::from(&bios_path);

    let region = detect_region(&bios_path);
    run(bios, disc_spec, cart, save_base, region)
}

/// Pick the SMPC area (region) code. A `SAT_REGION` env var (`J`/`U`/`T`/`E`)
/// overrides; otherwise it's inferred from the BIOS filename (`(JAP)` → Japan,
/// `(EUR)` → Europe-PAL, else North America). The region must be compatible
/// with both the BIOS build and the disc's IP.BIN area string, or the BIOS
/// rejects the disc with "Game disc unsuitable for this system" (until the
/// M9 region/BIOS picker lands, this keeps a JP BIOS + JP disc booting).
fn detect_region(bios_path: &str) -> u8 {
    use saturn::smpc::region;
    if let Ok(r) = std::env::var("SAT_REGION") {
        return match r.trim().to_ascii_uppercase().as_str() {
            "J" | "JP" | "JAPAN" => region::JAPAN,
            "T" | "ASIA" => region::ASIA_NTSC,
            "E" | "EU" | "EUR" | "EUROPE" | "PAL" => region::EUROPE_PAL,
            _ => region::NORTH_AMERICA,
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
        other => Err(format!(
            "unknown disc format '.{other}' (use .cue / .iso / .ccd)"
        )),
    }
}

#[cfg(feature = "sdl2-frontend")]
fn run(
    bios: Vec<u8>,
    disc_spec: Option<String>,
    cart: Cartridge,
    save_base: std::path::PathBuf,
    region: u8,
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
    saturn.insert_cartridge(cart);

    // Save-state quickslot and the persisted battery (internal backup RAM),
    // both keyed to the BIOS path. F5/F9 use the former; the latter is the
    // console's "memory card", loaded here and written back on exit.
    let state_path = save_base.with_extension("state");
    let battery_path = save_base.with_extension("bup");
    if let Ok(bytes) = fs::read(&battery_path) {
        saturn.load_internal_backup(&bytes);
        eprintln!("loaded backup RAM from {}", battery_path.display());
    }

    let sdl = sdl2::init().expect("SDL2 init");
    let video = sdl.video().expect("SDL2 video subsystem");

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
    audio_queue.resume();
    let window = video
        .window("5thPlanet", FRAME_WIDTH as u32 * 2, FRAME_HEIGHT as u32 * 2)
        .position_centered()
        .build()
        .expect("create window");
    let mut canvas = window
        .into_canvas()
        .present_vsync()
        .build()
        .expect("canvas");
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
    // Scratch buffer for compositing the OSD without corrupting the frozen
    // last frame (we redraw the menu over the same frame while it's open).
    let mut compose = vec![0u8; FRAMEBUFFER_BYTES];
    let mut osd = Osd::new();
    // Current texture (and frozen-frame) resolution; updated when a game
    // switches video mode so the texture/pitch track the active display size.
    let mut cur_dims = (FRAME_WIDTH, FRAME_HEIGHT);
    // Audio-paced emulation: keep ~this many bytes of SCSP output buffered in the
    // SDL queue. `44_100 * 2 * 2` is one second (44.1 kHz × 2 channels × 2 bytes),
    // so `/ 12` ≈ 83 ms (~5 frames) — comfortably above SDL's ~23 ms period. The
    // audio draining in real time, not the display vsync, sets the emulator speed.
    let audio_target_bytes = 44_100 * 2 * 2 / 12;
    // Cap emulated frames run per *displayed* frame. This is the smoothness
    // knob: the burst below advances the machine until audio is buffered, but
    // only the LAST frame of the burst is ever presented — so a large cap turns
    // a throughput deficit into a big visual lurch (run 8, show 1 = motion jumps
    // 8 frames at once, the "heavy frame-skip" symptom). A small cap keeps each
    // displayed frame within a few emulated frames of the last, so motion stays
    // smooth even when the core can't sustain full real-time; the trade is that
    // audio catch-up after a stall (menu close, state load, slow CD seek) takes
    // a few more iterations instead of one gallop. 2 keeps the visual jump to at
    // most one extra frame while still allowing modest catch-up.
    let max_frames_per_burst = 2;

    // Per-slot save-state path: `<bios>.<n>.state` (the F5/F9 quickslot keeps
    // the slot-less `<bios>.state`).
    let slot_path = |n: u8| save_base.with_extension(format!("{n}.state"));

    'main: loop {
        // The host SDL2 library on a modern Linux desktop emits event
        // codes 0.37's binding doesn't recognise — notably 0x207
        // (SDL_DISPLAYEVENT_MOVED on SDL ≥ 2.28). `poll_iter` panics
        // (non-unwinding, because the call originates inside an
        // `extern "C"` callback) when it sees one, aborting the
        // process. Flushing the whole 0x201..=0x20F range from the
        // queue before each poll drops them safely without needing
        // raw FFI. Range covers all the post-2.28 top-level display
        // events; widen if a future SDL adds more.
        event_subsystem.flush_events(0x201, 0x20F);

        // Live context for the menu (disc presence + which slots are filled).
        let ctx = OsdCtx {
            disc_present: saturn.has_disc(),
            slot_used: std::array::from_fn(|n| slot_path(n as u8).exists()),
        };

        for ev in events.poll_iter() {
            match ev {
                Event::Quit { .. } => break 'main,
                Event::KeyDown {
                    keycode: Some(kc), ..
                } if osd.is_open() => {
                    // Menu navigation. Esc/Backspace backs out (closing at root).
                    let action = match kc {
                        Keycode::Up => osd.handle(Nav::Up, &ctx),
                        Keycode::Down => osd.handle(Nav::Down, &ctx),
                        Keycode::Return | Keycode::Z => osd.handle(Nav::Select, &ctx),
                        Keycode::Backspace | Keycode::X => osd.handle(Nav::Back, &ctx),
                        Keycode::Escape => osd.toggle(),
                        _ => None,
                    };
                    if let Some(action) = action
                        && dispatch_osd(action, &mut osd, &mut saturn, &save_base, &launched_spec)
                    {
                        break 'main; // Quit
                    }
                }
                // Esc opens the menu (when closed).
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => {
                    osd.toggle();
                }
                // F5/F9 quick save/load to the slot-less quickslot.
                Event::KeyDown {
                    keycode: Some(Keycode::F5),
                    ..
                } => match fs::write(&state_path, saturn.save_state()) {
                    Ok(()) => osd.set_toast("Quicksave", 90),
                    Err(e) => eprintln!("save state failed: {e}"),
                },
                Event::KeyDown {
                    keycode: Some(Keycode::F9),
                    ..
                } => match fs::read(&state_path) {
                    Ok(bytes) => match saturn.load_state(&bytes) {
                        Ok(()) => osd.set_toast("Quickload", 90),
                        Err(e) => eprintln!("load state failed: {e}"),
                    },
                    Err(e) => eprintln!("no state to load ({e})"),
                },
                // F8: play the disc's first CD-DA (Red Book audio) track through
                // the live SCSP-mixed output. Lets an audio disc actually play in
                // the window without the BIOS issuing Play (the LLE-68k trigger
                // wall) — the CDDA→SCSP path is faithful; only the trigger is
                // missing. A no-op (toast) if the disc has no audio track.
                Event::KeyDown {
                    keycode: Some(Keycode::F8),
                    ..
                } => {
                    if saturn.dbg_play_first_audio_track() {
                        osd.set_toast("Playing CD audio", 120);
                    } else {
                        osd.set_toast("No CD audio track", 120);
                    }
                }
                _ => {}
            }
        }

        if osd.is_open() {
            // Frozen: don't advance the machine or feed the pad. Composite the
            // menu over a dimmed copy of the last frame.
            saturn.set_pad1(0);
            osd.tick_toast();
            let (w, h) = cur_dims;
            compose.copy_from_slice(&framebuffer);
            osd.render_overlay(&mut compose, w, h, &ctx);
            texture
                .update(None, &compose, w * 4)
                .expect("upload framebuffer");
        } else {
            // Map the host keyboard to the port-1 digital pad: arrows = D-pad,
            // Z/X/C = A/B/C, A/S/D = X/Y/Z, Q/W = L/R, Enter = Start.
            let keys = events.keyboard_state();
            let mut held = 0u16;
            for (sc, bit) in [
                (Scancode::Up, pad::UP),
                (Scancode::Down, pad::DOWN),
                (Scancode::Left, pad::LEFT),
                (Scancode::Right, pad::RIGHT),
                (Scancode::Z, pad::A),
                (Scancode::X, pad::B),
                (Scancode::C, pad::C),
                (Scancode::A, pad::X),
                (Scancode::S, pad::Y),
                (Scancode::D, pad::Z),
                (Scancode::Q, pad::L),
                (Scancode::W, pad::R),
                (Scancode::Return, pad::START),
            ] {
                if keys.is_scancode_pressed(sc) {
                    held |= bit;
                }
            }
            saturn.set_pad1(held);

            // Audio-paced emulation: run frames until the SDL audio queue holds
            // the target latency, then stop. The SCSP's 44.1 kHz output drains in
            // real time, so *the audio device* sets the emulator's speed — not the
            // display vsync — and the queue never under-runs (smooth BGM on any
            // refresh rate). *All* audio is queued (no dropping). The burst is
            // capped so a slow frame / stalled device can't starve the render; it
            // just falls a little behind and catches up next iteration. The TVMD
            // resolution can change mid-burst (a hi-res title), so re-create the
            // texture on change.
            let mut burst = 0;
            while audio_queue.size() < audio_target_bytes && burst < max_frames_per_burst {
                let dims = saturn.run_frame(&mut framebuffer);
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
                audio_queue.queue_audio(&saturn.take_audio()).ok();
                burst += 1;
            }

            // Present the latest frame (re-presents the last one if the queue was
            // already full and no frame ran). A lingering toast (e.g. "Quicksave")
            // is drawn over it.
            let (w, h) = cur_dims;
            osd.tick_toast();
            osd.render_overlay(&mut framebuffer, w, h, &ctx);
            texture
                .update(None, &framebuffer, w * 4)
                .expect("upload framebuffer");
        }

        canvas.clear();
        canvas.copy(&texture, None, None).expect("blit to canvas");
        canvas.present(); // present_vsync caps us at the display rate
    }

    // Persist the internal backup RAM ("battery") so game saves survive.
    if let Err(e) = fs::write(&battery_path, saturn.internal_backup()) {
        eprintln!(
            "failed to persist backup RAM to {}: {e}",
            battery_path.display()
        );
    }

    ExitCode::SUCCESS
}

/// Carry out a menu action against the running machine. Returns `true` if the
/// emulator should quit. Save-state slots live at `<bios>.<n>.state`.
#[cfg(feature = "sdl2-frontend")]
fn dispatch_osd(
    action: osd::OsdAction,
    osd: &mut osd::Osd,
    saturn: &mut saturn::Saturn,
    save_base: &std::path::Path,
    launched_spec: &Option<String>,
) -> bool {
    use osd::OsdAction;
    let slot_path = |n: u8| save_base.with_extension(format!("{n}.state"));
    match action {
        OsdAction::Resume => osd.close(),
        OsdAction::Quit => return true,
        OsdAction::Reset => {
            saturn.reset();
            osd.set_toast("Reset", 120);
            osd.close();
        }
        OsdAction::Save(n) => match fs::write(slot_path(n), saturn.save_state()) {
            Ok(()) => osd.set_toast(format!("Saved slot {n}"), 120),
            Err(e) => osd.set_toast(format!("Save failed: {e}"), 180),
        },
        OsdAction::Load(n) => match fs::read(slot_path(n)) {
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
        OsdAction::ReinsertDisc => match launched_spec {
            Some(spec) => match insert_from_spec(saturn, spec) {
                Ok(()) => osd.set_toast("Disc inserted", 120),
                Err(e) => osd.set_toast(format!("Insert failed: {e}"), 180),
            },
            None => osd.set_toast("No disc to insert", 120),
        },
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
                let matched = rreg.is_none_or(|r| (rlo..rhi).contains(&h.0[r]));
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
            Some((r, pr, gbr, code, _probe)) => {
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
    let mut last_pc = u32::MAX;
    let mut dump_dims = (FRAME_WIDTH, FRAME_HEIGHT);
    for f in 0..headless_frames {
        apply_scripted_pad(&mut saturn, f);
        if cache_purge {
            saturn.master_mut().cache.purge();
            saturn.slave_mut().cache.purge();
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
