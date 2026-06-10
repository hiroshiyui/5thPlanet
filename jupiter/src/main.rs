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
    run(bios, disc_spec, cart, save_base, region, mouse_port)
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
    mouse_port: Option<u8>,
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
    // Plug the Shuttle Mouse into the requested port. On port 2 (the default)
    // the keyboard pad stays on port 1; --mouse=1 replaces the pad (the
    // mouse's Start button is on the Return key either way).
    match mouse_port {
        Some(1) => saturn.set_port_devices(
            saturn::smpc::PortDevice::Mouse,
            saturn::smpc::PortDevice::None,
        ),
        Some(_) => saturn.set_port_devices(
            saturn::smpc::PortDevice::Pad,
            saturn::smpc::PortDevice::Mouse,
        ),
        None => {}
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
        scale: 2,
        fullscreen: false,
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
    // Leave the device PAUSED (SDL opens queues paused) until the reserve has
    // filled once — see the prebuffer gate in the main loop. Resuming here would
    // let the device drain from t=0 while the queue is still empty during boot,
    // which under-runs exactly once on a cold start (the "first-play buzz").
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
    let max_frames_per_burst = 2u32;

    let perflog = std::env::var_os("SAT_PERFLOG").is_some();

    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, mpsc};

    let (emu_tx, emu_rx) = mpsc::channel::<EmuIn>();
    let (frame_tx, frame_rx) = mpsc::sync_channel::<(Vec<u8>, (usize, usize))>(2);
    let (recycle_tx, recycle_rx) = mpsc::channel::<Vec<u8>>();
    let (audio_tx, audio_rx) = mpsc::channel::<Vec<i16>>();
    let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();
    let audio_mirror = Arc::new(AtomicU32::new(0));
    let osd_open = Arc::new(AtomicBool::new(false));
    let quit_flag = Arc::new(AtomicBool::new(false));

    std::thread::scope(|scope| {
        let emu_mirror = Arc::clone(&audio_mirror);
        let emu_osd_open = Arc::clone(&osd_open);
        let emu_quit = Arc::clone(&quit_flag);
        let save_base_emu = save_base.clone();
        let emu = scope.spawn(move || -> Saturn {
            let mut saturn = saturn;
            let mut osd = osd;
            let mut pipe = pipe;
            let mut ui = ui;
            let slot_path = |n: u8| save_base_emu.with_extension(format!("{n}.state"));
            let state_path = save_base_emu.with_extension("state");
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
                    slot_used: std::array::from_fn(|n| slot_path(n as u8).exists()),
                    scale: ui.scale,
                    fullscreen: ui.fullscreen,
                    region: ui.region,
                    cart: ui.cart,
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
                                && dispatch_osd(
                                    action,
                                    &mut osd,
                                    &mut saturn,
                                    &save_base_emu,
                                    &launched_spec,
                                    &mut ui,
                                    &ui_tx,
                                )
                            {
                                let _ = ui_tx.send(UiMsg::Quit);
                                break 'emu;
                            }
                        }
                        EmuIn::Quicksave => match fs::write(&state_path, saturn.save_state()) {
                            Ok(()) => osd.set_toast("Quicksave", 90),
                            Err(e) => eprintln!("save state failed: {e}"),
                        },
                        EmuIn::Quickload => match fs::read(&state_path) {
                            Ok(bytes) => match saturn.load_state(&bytes) {
                                Ok(()) => osd.set_toast("Quickload", 90),
                                Err(e) => eprintln!("load state failed: {e}"),
                            },
                            Err(e) => eprintln!("no state to load ({e})"),
                        },
                        EmuIn::PlayCdda => {
                            if saturn.dbg_play_first_audio_track() {
                                osd.set_toast("Playing CD audio", 120);
                            } else {
                                osd.set_toast("No CD audio track", 120);
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
                // The cap keeps a sub-real-time stretch from turning into a big
                // visual lurch (run 2, show 1 at worst).
                let mut depth = emu_mirror.load(Ordering::Relaxed);
                let mut burst = 0u32;
                while depth < audio_target_bytes && burst < max_frames_per_burst {
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
            saturn
        });

        // ---- SDL main loop: events, audio, present -----------------------
        let mut cur_dims = (FRAME_WIDTH, FRAME_HEIGHT);
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

            // Queue the emu thread's audio and refresh the depth mirror it
            // paces on. Start playback once the reserve has first filled, so
            // a cold start never drains an empty queue (the prebuffer gate).
            while let Ok(chunk) = audio_rx.try_recv() {
                let _ = audio_queue.queue_audio(&chunk);
            }
            audio_mirror.store(audio_queue.size(), Ordering::Relaxed);
            if !audio_started && audio_queue.size() >= audio_target_bytes {
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
                    audio_queue.size() / 176,
                );
                pl_present = std::time::Duration::ZERO;
                pl_iters = 0;
                pl_last = std::time::Instant::now();
            }
        }

        quit_flag.store(true, Ordering::Relaxed);
        // Unblock and join the emu thread, then persist the battery from the
        // final machine state.
        let saturn = emu.join().expect("emu thread");
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
    /// F8: play the disc's first CD-DA track.
    PlayCdda,
    /// Shuttle Mouse: motion since the last message (host convention,
    /// X+ right / Y+ down) + the held `saturn::smpc::mouse` button mask.
    Mouse(i32, i32, u8),
}

/// Messages from the emulation thread back to the SDL main thread —
/// window-affecting OSD actions (the canvas is not Send) and quit.
#[cfg(feature = "sdl2-frontend")]
enum UiMsg {
    Scale(u8),
    Fullscreen(bool),
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

/// Carry out a menu action against the running machine. Returns `true` if the
/// emulator should quit. Save-state slots live at `<bios>.<n>.state`.
#[cfg(feature = "sdl2-frontend")]
fn dispatch_osd(
    action: osd::OsdAction,
    osd: &mut osd::Osd,
    saturn: &mut saturn::Saturn,
    save_base: &std::path::Path,
    launched_spec: &Option<String>,
    ui: &mut UiState,
    ui_tx: &std::sync::mpsc::Sender<UiMsg>,
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
        OsdAction::SetScale(s) => {
            ui.scale = s;
            // Window pixels = base 320×224 × scale; applied on the SDL main
            // thread (the canvas is not Send). The canvas stretches the
            // texture to fill, so no texture re-create is needed.
            let _ = ui_tx.send(UiMsg::Scale(s));
            osd.set_toast(format!("Scale {s}x"), 90);
        }
        OsdAction::ToggleFullscreen => {
            ui.fullscreen = !ui.fullscreen;
            let _ = ui_tx.send(UiMsg::Fullscreen(ui.fullscreen));
            osd.set_toast(
                if ui.fullscreen { "Fullscreen on" } else { "Fullscreen off" },
                90,
            );
        }
        OsdAction::SetRegion(r) => {
            // A region change is a hardware-level change: reset and re-apply the
            // boot config (region + current cart). The disc stays inserted, so
            // the machine re-boots from it under the new region.
            ui.region = r;
            saturn.reset();
            saturn.set_region(osd_region_to_code(r));
            saturn.insert_cartridge(osd_cart_to_cartridge(ui.cart));
            let name = match r {
                osd::OsdRegion::Japan => "Japan",
                osd::OsdRegion::NorthAmerica => "North America",
                osd::OsdRegion::EuropePal => "Europe (PAL)",
                osd::OsdRegion::AsiaNtsc => "Asia (NTSC)",
            };
            osd.set_toast(format!("Region: {name} (reset)"), 150);
            osd.close();
        }
        OsdAction::SetCartridge(k) => {
            ui.cart = k;
            saturn.reset();
            saturn.set_region(osd_region_to_code(ui.region));
            saturn.insert_cartridge(osd_cart_to_cartridge(k));
            let name = match k {
                osd::OsdCart::None => "None",
                osd::OsdCart::ExtRam1M => "Ext RAM 1M",
                osd::OsdCart::ExtRam4M => "Ext RAM 4M",
                osd::OsdCart::BackupRam => "Backup RAM",
            };
            osd.set_toast(format!("Cartridge: {name} (reset)"), 150);
            osd.close();
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
    _mouse_port: Option<u8>,
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
    // Plug the Shuttle Mouse into the requested port. On port 2 (the default)
    // the keyboard pad stays on port 1; --mouse=1 replaces the pad (the
    // mouse's Start button is on the Return key either way).
    match mouse_port {
        Some(1) => saturn.set_port_devices(
            saturn::smpc::PortDevice::Mouse,
            saturn::smpc::PortDevice::None,
        ),
        Some(_) => saturn.set_port_devices(
            saturn::smpc::PortDevice::Pad,
            saturn::smpc::PortDevice::Mouse,
        ),
        None => {}
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
