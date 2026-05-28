//! 5thPlanet frontend.
//!
//! Two builds:
//!
//! * `cargo run -p fifth_planet -- BIOS.bin`
//!   (default features) — opens an SDL2 window, runs the Saturn at
//!   60 Hz, uploads each frame to a streaming texture. Quit with
//!   Esc or the window's close button.
//!
//! * `cargo run -p fifth_planet --no-default-features -- BIOS.bin`
//!   — headless. Runs a fixed number of frames and prints a short
//!   status report. Useful when libsdl2-dev isn't available, or
//!   for the BIOS-boot regression test that doesn't need a window.

use std::env;
use std::fs;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use saturn::cartridge::Cartridge;

/// Host wall-clock time as seconds since the Unix epoch (0 if the clock is
/// somehow before the epoch). Used to seed the Saturn RTC.
fn host_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn main() -> ExitCode {
    // Split flags (`--cart=…`) from positional args (BIOS, then disc).
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
            eprintln!("usage: fifth_planet <BIOS.bin> [game.cue|.iso|.ccd] [--cart=<kind>]");
            eprintln!();
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

    // Optional game disc (CUE/BIN, raw ISO, or CloneCD CCD/IMG).
    let disc = match positionals.get(1) {
        Some(path) => match load_disc(path) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("failed to load disc {path}: {e}");
                return ExitCode::from(1);
            }
        },
        None => None,
    };

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

    run(bios, disc, cart)
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

/// Load a disc image, picking the parser by file extension: `.iso` (raw
/// 2048-byte data track), `.cue` (CUE sheet + its `.bin`s), or `.ccd`
/// (CloneCD control file + sibling `.img`).
fn load_disc(path: &str) -> Result<saturn::disc::Disc, String> {
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

#[cfg(feature = "sdl2-frontend")]
fn run(bios: Vec<u8>, disc: Option<saturn::disc::Disc>, cart: Cartridge) -> ExitCode {
    use sdl2::audio::AudioSpecDesired;
    use sdl2::event::Event;
    use sdl2::keyboard::{Keycode, Scancode};
    use sdl2::pixels::PixelFormatEnum;

    use saturn::smpc::pad;

    use saturn::Saturn;
    use saturn::vdp2::{FRAME_HEIGHT, FRAME_WIDTH, FRAMEBUFFER_BYTES};

    let mut saturn = Saturn::new(bios);
    saturn.reset();
    // Seed the RTC from the host clock so the Saturn shows real wall-clock
    // time, like a console with a charged backup battery.
    saturn.set_rtc_unix(host_unix_secs());
    if let Some(d) = disc {
        saturn.insert_disc(d);
    }
    saturn.insert_cartridge(cart);

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

        for ev in events.poll_iter() {
            match ev {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => break 'main,
                _ => {}
            }
        }

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

        saturn.run_frame(&mut framebuffer);

        // Queue this frame's SCSP audio, unless we're already buffered well
        // ahead (keeps latency bounded if the host runs faster than realtime).
        let audio_samples = saturn.take_audio();
        if audio_queue.size() < 44_100 * 2 * 2 / 4 {
            audio_queue.queue_audio(&audio_samples).ok();
        }

        texture
            .update(None, &framebuffer, FRAME_WIDTH * 4)
            .expect("upload framebuffer");
        canvas.clear();
        canvas.copy(&texture, None, None).expect("blit to canvas");
        canvas.present(); // present_vsync caps us at the display rate
    }

    ExitCode::SUCCESS
}

#[cfg(not(feature = "sdl2-frontend"))]
fn run(bios: Vec<u8>, disc: Option<saturn::disc::Disc>, cart: Cartridge) -> ExitCode {
    use saturn::Saturn;
    use saturn::vdp2::FRAMEBUFFER_BYTES;

    const HEADLESS_FRAMES: u32 = 180; // ~3 seconds of virtual time at 60 Hz

    let mut saturn = Saturn::new(bios);
    saturn.reset();
    // Seed the RTC from the host clock so the Saturn shows real wall-clock
    // time, like a console with a charged backup battery.
    saturn.set_rtc_unix(host_unix_secs());
    if let Some(d) = disc {
        saturn.insert_disc(d);
    }
    saturn.insert_cartridge(cart);
    let mut framebuffer = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..HEADLESS_FRAMES {
        saturn.run_frame(&mut framebuffer);
    }

    let master_pc = saturn.master().regs.pc;
    let cycles = saturn.master().pipeline.cycles;
    println!(
        "headless run complete: master PC=0x{master_pc:08X}, cycles={cycles}, frames={HEADLESS_FRAMES}"
    );
    ExitCode::SUCCESS
}
