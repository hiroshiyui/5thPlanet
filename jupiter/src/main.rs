//! 5thPlanet frontend.
//!
//! Two builds:
//!
//! * `cargo run -p jupiter -- BIOS.bin`
//!   (default features) — opens an SDL3 window, runs the Saturn at
//!   60 Hz, uploads each frame to a streaming texture. Quit with
//!   Esc or the window's close button.
//!
//! * `cargo run -p jupiter --no-default-features -- BIOS.bin`
//!   — headless. Runs a fixed number of frames and prints a short
//!   status report. Useful when libsdl3-dev isn't available, or
//!   for the BIOS-boot regression test that doesn't need a window.

use std::env;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use saturn::cartridge::Cartridge;
use saturn::host_files::{self, battery_path, cart_card_path, parse_cart, write_atomic};

// The OSD menu is pure logic (no sdl3): compile it for the SDL3 frontend and
// for tests (so its unit tests run even with `--no-default-features`), but not
// in a headless non-test build where nothing uses it.
#[cfg(any(feature = "sdl-frontend", test))]
mod battery;
mod config;
#[cfg(any(feature = "sdl-frontend", test))]
mod osd;
#[cfg(feature = "sdl-frontend")]
mod ports;
#[cfg(any(feature = "sdl-frontend", test))]
mod present;
// The SDL_GPU presentation backend (`--gpu`); compiled only with the
// `gpu-presenter` feature, or in tests for its pure unit tests.
#[cfg(any(feature = "gpu-presenter", test))]
mod present_gpu;
#[cfg(any(feature = "sdl-frontend", test))]
mod render_pipe;

/// Host wall-clock time as seconds since the Unix epoch (0 if the clock is
/// somehow before the epoch). Used to seed the Saturn RTC.
fn host_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// SCSP output byte rate: 44.1 kHz × 2 channels × 2 bytes/sample.
const AUDIO_BYTES_PER_SEC: u32 = 176_400;

/// Write a 44-byte canonical WAV header for 44.1 kHz 16-bit stereo PCM. Called
/// with `pcm_bytes = 0` up front (size unknown), then re-written in `Drop` once
/// the final byte count is known (see [`WavDump`]).
fn write_wav_header(mut w: impl Write, pcm_bytes: u32) -> std::io::Result<()> {
    let riff_bytes = 36u32.saturating_add(pcm_bytes);
    w.write_all(b"RIFF")?;
    w.write_all(&riff_bytes.to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&2u16.to_le_bytes())?; // stereo
    w.write_all(&44_100u32.to_le_bytes())?;
    w.write_all(&AUDIO_BYTES_PER_SEC.to_le_bytes())?; // byte rate
    w.write_all(&4u16.to_le_bytes())?; // block align
    w.write_all(&16u16.to_le_bytes())?; // bits per sample
    w.write_all(b"data")?;
    w.write_all(&pcm_bytes.to_le_bytes())
}

/// Debug audio capture (`SAT_AUDIO_DUMP=<path>`): a raw WAV writer. The header
/// is written with size 0 on open and **patched in `Drop`** with the final PCM
/// byte count (`pcm_bytes`), so the size is only correct after the dump closes.
struct WavDump {
    path: String,
    file: fs::File,
    pcm_bytes: u32,
}

impl WavDump {
    fn from_env(name: &str) -> Option<Self> {
        let path = std::env::var(name).ok()?;
        let mut file = match fs::File::create(&path) {
            Ok(file) => file,
            Err(e) => {
                eprintln!("create audio dump {path} failed: {e}");
                return None;
            }
        };
        if let Err(e) = write_wav_header(&mut file, 0) {
            eprintln!("create audio dump {path} failed: {e}");
            return None;
        }
        Some(Self {
            path,
            file,
            pcm_bytes: 0,
        })
    }

    fn write_samples(&mut self, samples: &[i16]) {
        // Bulk-write the whole slice (one syscall, vs one per i16) and credit
        // `pcm_bytes` only on success, so the size patched into the header in
        // `Drop` stays consistent with the bytes actually on disk.
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        if self.file.write_all(&bytes).is_ok() {
            self.pcm_bytes = self.pcm_bytes.saturating_add(bytes.len() as u32);
        }
    }

    #[cfg_attr(not(feature = "sdl-frontend"), allow(dead_code))]
    fn reset(&mut self) {
        if self.file.set_len(0).is_ok()
            && self.file.seek(SeekFrom::Start(0)).is_ok()
            && write_wav_header(&mut self.file, 0).is_ok()
        {
            self.pcm_bytes = 0;
        }
    }
}

impl Drop for WavDump {
    fn drop(&mut self) {
        if self.file.seek(SeekFrom::Start(0)).is_ok()
            && write_wav_header(&mut self.file, self.pcm_bytes).is_ok()
        {
            eprintln!(
                "wrote audio dump {} ({} bytes PCM)",
                self.path, self.pcm_bytes
            );
        }
    }
}

/// Format the `SAT_SCSP_MOVIE_PROBE` slot summary: the selected MSLC monitor
/// plus up to 8 active slots' SA / CA / loop / EG state. Empty when disabled.
/// Shared by both the SDL and headless movie-probe paths.
fn scsp_probe_string(scsp: &saturn::scsp::Scsp, enabled: bool) -> String {
    if !enabled {
        return String::new();
    }
    let (mslc, monitor) = scsp.dbg_slot_monitor();
    let active = (0..32)
        .filter(|&i| scsp.slot_active(i))
        .take(8)
        .map(|i| {
            let d = scsp.slot_debug(i);
            let ca = ((d.cur >> 12) & 0xFFFF) >> 12;
            format!(
                "{i}:sa={:05X} cur={} ca={ca:X} step={} lsa={} lea={} lp={} eg={} tl={:02X}",
                d.sa,
                d.cur >> 12,
                d.step,
                d.lsa,
                d.lea,
                d.lpctl,
                d.eg_state,
                d.tl
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(" scsp_mslc={mslc} mon={monitor:04X} slots=[{active}]")
}

/// Classify a master-SH-2 PC into a coarse memory region for the OSD's live
/// status readout (cache-through `0x2…` aliases fold onto the same regions).
#[cfg(feature = "sdl-frontend")]
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
        let tag = if o.passed {
            passed += 1;
            "PASS"
        } else {
            "FAIL"
        };
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
    let mut ok = print_diag_section(
        "5thPlanet self-diagnostics:",
        &saturn::diagnostics::run_all(),
    );

    if let Some(bp) = bios_path {
        match fs::read(&bp) {
            Ok(bios) => {
                let region = detect_region(&bp, None);
                let disc = match disc_path {
                    Some(dp) => match host_files::load_image_disc(std::path::Path::new(&dp)) {
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

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn main() -> ExitCode {
    // Split flags (`--cart=…`) from positional args (BIOS, disc).
    let mut positionals: Vec<String> = Vec::new();
    let mut cart_spec: Option<String> = None;
    let mut mouse_port: Option<u8> = None;
    let mut backend_override: Option<String> = None;
    #[cfg(feature = "gpu-presenter")]
    let mut gpu_override: Option<String> = None;
    #[cfg(feature = "gpu-presenter")]
    let mut gpu_selftest = false;
    for arg in env::args().skip(1) {
        // `--gpu-selftest` runs the contained SDL_GPU Vulkan presenter proof and
        // exits (preview-only; see `present_gpu::run_selftest`).
        #[cfg(feature = "gpu-presenter")]
        if arg == "--gpu-selftest" {
            gpu_selftest = true;
            continue;
        }
        // `--gpu[=off|auto|on]` is preview-only groundwork (off by default);
        // recognised only in `gpu-presenter` builds — otherwise it falls through as
        // an unknown argument.
        #[cfg(feature = "gpu-presenter")]
        if arg == "--gpu" || arg.starts_with("--gpu=") {
            gpu_override = Some(
                arg.strip_prefix("--gpu=")
                    .map_or_else(|| "auto".to_string(), str::to_string),
            );
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--cart=") {
            cart_spec = Some(spec.to_string());
        } else if arg == "--mouse" || arg == "--mouse=2" {
            mouse_port = Some(2);
        } else if arg == "--mouse=1" {
            mouse_port = Some(1);
        } else if let Some(tok) = arg.strip_prefix("--backend=") {
            backend_override = Some(tok.to_string());
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

    // `jupiter --gpu-selftest` — prove the SDL_GPU Vulkan presenter works (a
    // contained one-shot; the normal SDL_Renderer path is untouched) and exit.
    #[cfg(feature = "gpu-presenter")]
    if gpu_selftest {
        return present_gpu::run_selftest();
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
            eprintln!(
                "                         F10 = toggle pointer capture (Esc/OSD also releases)"
            );
            eprintln!("  --backend=<api>        presentation backend: auto (default) | opengl |");
            eprintln!(
                "                         opengles | direct3d11 | direct3d12 | metal | software"
            );
            #[cfg(feature = "gpu-presenter")]
            {
                eprintln!(
                    "  --gpu[=off|auto|on]    present via SDL_GPU (Vulkan) instead of SDL_Renderer"
                );
                eprintln!(
                    "                         (off = default; auto falls back if unavailable)"
                );
                eprintln!(
                    "  --gpu-selftest         present an animated test pattern via the SDL_GPU"
                );
                eprintln!(
                    "                         Vulkan blit and exit (proves the alternative presenter)"
                );
            }
            eprintln!();
            eprintln!("  Hotkeys: Esc = menu, F5/F9 = quick save/load, F11 = fullscreen,");
            eprintln!("           F12 = fullscreen aspect (keep ratio / fit screen).");
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
    let mut cfg = config::Config::load();
    // `--backend=` overrides the config's graphics backend (CLI > file).
    if let Some(b) = backend_override {
        cfg.backend = b;
    }
    // `--gpu[=…]` overrides the config's SDL_GPU backend mode (CLI > file);
    // preview-only (the `gpu-presenter` feature).
    #[cfg(feature = "gpu-presenter")]
    if let Some(g) = gpu_override {
        cfg.gpu = g;
    }

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

    // Base path for the BIOS-keyed save files — the internal backup RAM
    // (`.bup`) and the backup-cart cards (`.N.crtbup`). Save states key to the
    // loaded disc instead (`Session::state_base`, ADR-0023).
    let save_base = std::path::PathBuf::from(&bios_path);

    // Fold the legacy `mouse` config token (`off`/`1`/`2`) into the per-port
    // tokens (cfg.port1/port2) for the layered input model. The `--mouse[=1|2]`
    // CLI flag stays a startup override applied in run() (CLI > config),
    // mirroring how `--cart=` overrides `cfg.cartridge`.
    cfg.migrate_ports();

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

/// Find the BIOS images the OSD BIOS screen can power-cycle into: every
/// `.bin` of exactly 512 KiB in the launched image's directory, sorted.
/// Returns the list and the launched image's index in it (the launched path
/// is prepended if the scan somehow misses it, e.g. a non-`.bin` extension).
#[cfg(feature = "sdl-frontend")]
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
        sat.insert_disc(host_files::load_image_disc(std::path::Path::new(spec))?);
    }
    Ok(())
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
#[cfg_attr(not(feature = "sdl-frontend"), allow(dead_code))]
fn burst_cap(depth: u32, catchup_floor: u32, max: u32) -> u32 {
    if depth < catchup_floor { max.max(1) } else { 1 }
}

/// Base path for a game's save-state siblings: the loaded disc IMAGE so each game
/// gets its own slots, falling back to `bios_base` for a live `cdrom:` drive (no
/// image path) or a no-disc boot. The `.bup` battery keys to the BIOS separately
/// (a shared console resource); only save states follow the disc.
#[cfg_attr(not(feature = "sdl-frontend"), allow(dead_code))]
fn state_base_for(launched_spec: Option<&str>, bios_base: &std::path::Path) -> std::path::PathBuf {
    match launched_spec {
        Some(spec) if !spec.starts_with("cdrom:") => std::path::PathBuf::from(spec),
        _ => bios_base.to_path_buf(),
    }
}

/// Persist the currently-plugged backup cartridge (if any) to the active card's
/// file, before switching cards or ejecting (ADR-0028).
#[cfg(feature = "sdl-frontend")]
fn persist_active_backup_card(saturn: &saturn::Saturn, sess: &mut Session) {
    if let Some(bytes) = saturn.cartridge_backup() {
        let path = cart_card_path(&sess.save_base, sess.cfg.cart_card);
        sess.battery.persist_card(&path, bytes, true);
    }
}

/// Load backup-cart card `card`'s file into the plugged cart (if both exist)
/// and record it as already persisted, so the change tracker doesn't
/// immediately re-write the image it just read.
#[cfg(feature = "sdl-frontend")]
fn load_backup_card(saturn: &mut saturn::Saturn, sess: &mut Session, card: u8) {
    let path = cart_card_path(&sess.save_base, card);
    if saturn.cartridge_backup().is_some()
        && let Ok(bytes) = fs::read(&path)
    {
        saturn.load_cartridge_backup(&bytes);
        if let Some(loaded) = saturn.cartridge_backup() {
            sess.battery.note_card(&path, loaded);
        }
    }
}

/// Ensure the Backup RAM cartridge is plugged in, power-cycling into it when a
/// different (or no) cart is present. A no-op when it is already the active
/// cart, so switching cards mid-session hot-swaps the card contents without a
/// machine reset (ADR-0028).
#[cfg(feature = "sdl-frontend")]
fn ensure_backup_cart(
    saturn: &mut saturn::Saturn,
    sess: &mut Session,
    audio_tx: &std::sync::mpsc::Sender<AudioMsg>,
) {
    if saturn.cartridge_backup().is_some() {
        return; // already a Backup RAM cart — just load the new card into it
    }
    sess.ui.cart = osd::OsdCart::BackupRam;
    sess.cfg.cartridge = "bram".into();
    saturn.reset();
    saturn.set_region(osd_region_to_code(sess.ui.region));
    saturn.insert_cartridge(osd_cart_to_cartridge(osd::OsdCart::BackupRam));
    let _ = audio_tx.send(AudioMsg::Reset);
}

/// Subtract `n` from an atomic, clamping at 0. **Must saturate**: an
/// `AudioMsg::Reset` zeroes `audio_inflight` (`store(0)`) and can race ahead of
/// in-flight `Chunk` byte-count subtracts that were already queued, momentarily
/// driving the counter below zero — saturation absorbs that window so the pacing
/// reserve never underflows to a huge value (which would stall the emu thread).
#[cfg(feature = "sdl-frontend")]
fn atomic_saturating_sub(v: &std::sync::atomic::AtomicU32, n: u32) {
    use std::sync::atomic::Ordering;
    let _ = v.try_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
        Some(cur.saturating_sub(n))
    });
}

/// Decode the bundled PNG icon (`jupiter/assets/app_icon.png`, embedded at build time)
/// into a raw RGBA surface and apply it as the window/taskbar icon. Purely
/// cosmetic — any decode failure is silently ignored so a broken icon never
/// stops the emulator from opening.
#[cfg(feature = "sdl-frontend")]
fn set_window_icon(window: &mut sdl3::video::Window) {
    const ICON_PNG: &[u8] = include_bytes!("../assets/app_icon.png");
    // png 0.18's decoder needs `BufRead + Seek`; a `Cursor` provides both.
    let decoder = png::Decoder::new(std::io::Cursor::new(ICON_PNG));
    let Ok(mut reader) = decoder.read_info() else {
        return;
    };
    let Some(size) = reader.output_buffer_size() else {
        return;
    };
    let mut buf = vec![0u8; size];
    let Ok(info) = reader.next_frame(&mut buf) else {
        return;
    };
    // SDL's ABGR8888 byte order on little-endian hosts is [R,G,B,A] in memory,
    // matching the `png` crate's RGBA8 output — so no per-pixel swap is needed.
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return;
    }
    buf.truncate(info.buffer_size());
    let pitch = info.width * 4;
    let Ok(surface) = sdl3::surface::Surface::from_data(
        &mut buf,
        info.width,
        info.height,
        pitch,
        sdl3::pixels::PixelFormat::ABGR8888,
    ) else {
        return;
    };
    window.set_icon(surface);
}

/// Snap a windowed window to the 4:3 display aspect (keep width, derive height),
/// so the Saturn picture fills it with no black bars. A no-op when already 4:3.
/// Only call in windowed mode (in fullscreen the size is the display's). Takes a
/// bare `Window` so it serves both presentation backends (renderer canvas + GPU).
#[cfg(feature = "sdl-frontend")]
fn snap_window_to_4_3(window: &mut sdl3::video::Window) {
    let (w, h) = window.size();
    if let Some((nw, nh)) = present::window_aspect_lock(w, h) {
        let _ = window.set_size(nw, nh);
    }
}

/// The live presentation backend's window, for the shared window controls (icon,
/// size, fullscreen, mouse grab). Exactly one of the renderer canvas / GPU
/// presenter is active, so one of the `Option`s is always `Some`. The `gpu`
/// parameter is `gpu-presenter`-only; without that feature the renderer canvas is
/// the only backend.
#[cfg(feature = "sdl-frontend")]
// The `'a` ties both args to the result; without `gpu-presenter` the `gpu` param is
// cfg'd out, leaving `'a` elidable — so silence the lint that only fires there.
#[allow(clippy::needless_lifetimes)]
fn backend_window_mut<'a>(
    canvas: &'a mut Option<sdl3::render::WindowCanvas>,
    #[cfg(feature = "gpu-presenter")] gpu: &'a mut Option<present_gpu::GpuPresenter>,
) -> &'a mut sdl3::video::Window {
    if let Some(c) = canvas.as_mut() {
        return c.window_mut();
    }
    #[cfg(feature = "gpu-presenter")]
    if let Some(g) = gpu.as_mut() {
        return g.window_mut();
    }
    panic!("a presentation backend is always active");
}

/// Shared (immutable) window accessor — see [`backend_window_mut`].
#[cfg(feature = "sdl-frontend")]
#[allow(clippy::needless_lifetimes)]
fn backend_window<'a>(
    canvas: &'a Option<sdl3::render::WindowCanvas>,
    #[cfg(feature = "gpu-presenter")] gpu: &'a Option<present_gpu::GpuPresenter>,
) -> &'a sdl3::video::Window {
    if let Some(c) = canvas.as_ref() {
        return c.window();
    }
    #[cfg(feature = "gpu-presenter")]
    if let Some(g) = gpu.as_ref() {
        return g.window();
    }
    panic!("a presentation backend is always active");
}

/// Saturn pad bits in the fixed `config::BUTTON_NAMES` binding order.
#[cfg(feature = "sdl-frontend")]
const PAD_BITS: [u16; config::PAD_BUTTONS] = {
    use saturn::smpc::pad;
    [
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
    ]
};

/// Read an env var and parse it, `None` if unset or unparsable.
#[cfg(feature = "sdl-frontend")]
fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|s| s.parse().ok())
}

/// The machine as built at launch, before any SDL state exists.
#[cfg(feature = "sdl-frontend")]
struct Booted {
    saturn: saturn::Saturn,
    ports: ports::Ports,
    ui: UiState,
    rtc_seed: u64,
    battery: battery::BatteryWriter,
}

/// Build and configure the machine for the SDL frontend: ports (config, then
/// the `--mouse` override), RTC, launch disc, cartridge, the persisted battery
/// memories, and an optional `SAT_LOADSTATE`. `Err` carries the exit code for a
/// fatal launch error (unreadable disc / state).
#[cfg(feature = "sdl-frontend")]
fn boot_machine(
    bios: Vec<u8>,
    disc_spec: Option<&str>,
    cart: Cartridge,
    save_base: &std::path::Path,
    region: u8,
    mouse_port: Option<u8>,
    cfg: &config::Config,
) -> Result<Booted, ExitCode> {
    let mut saturn = saturn::Saturn::new(bios);
    saturn.reset();
    saturn.set_region(region);
    // Build the per-port input assignment from the persisted tokens
    // (cfg.port1/port2, already migrated from the legacy `mouse` key in main()).
    // The connected-controller list is empty at startup — the SDL thread sends
    // an EmuIn::PadList once pads enumerate, which re-resolves any gamepad GUIDs.
    let mut ports = ports::Ports::from_tokens(&cfg.port1, &cfg.port2, &[]);
    // --mouse[=1|2] overrides the configured assignment (CLI > config): 1 = mouse
    // on port 1 (no pad), 2 = keyboard on port 1 + mouse on port 2. The mouse's
    // Start button is on the Return key either way.
    if let Some(n) = mouse_port {
        ports.source = if n == 1 {
            [ports::Source::Mouse, ports::Source::None]
        } else {
            [ports::Source::Keyboard, ports::Source::Mouse]
        };
    }
    saturn.set_port_devices(ports.source[0].device(), ports.source[1].device());
    // Seed the RTC from the host clock so the Saturn shows real wall-clock
    // time, like a console with a charged backup battery. Capture the seed so
    // an input recording (SAT_INPUT_REC) can store it in its header — a
    // deterministic replay (sdbg `replay`) must re-seed the same RTC.
    let rtc_seed = host_unix_secs();
    saturn.set_rtc_unix(rtc_seed);
    // Insert the launched disc (image or live drive); its spec is kept in the
    // Session so the OSD "Insert Disc" can re-insert it after an eject.
    if let Some(spec) = disc_spec
        && let Err(e) = insert_from_spec(&mut saturn, spec)
    {
        eprintln!("failed to load disc {spec}: {e}");
        return Err(ExitCode::from(1));
    }
    // Remember the launched config so the OSD Settings screens can mark the
    // active region / cartridge and re-apply it across a runtime change.
    let ui = UiState {
        scale: cfg.scale,
        fullscreen: cfg.fullscreen,
        sharp: !present::is_smooth(&cfg.scaling),
        keep_aspect: present::keeps_aspect(&cfg.aspect),
        region: region_code_to_osd(region),
        cart: cartridge_to_osd(&cart),
        backend: token_to_osd_backend(&cfg.backend),
        #[cfg(feature = "gpu-presenter")]
        shader_crt: present_gpu::ShaderMode::from_token(&cfg.shader).is_crt(),
    };
    saturn.insert_cartridge(cart);

    // The persisted battery (internal backup RAM) — the console's "memory
    // card", keyed to the BIOS path; loaded here, then written back whenever it
    // changes and at exit (`battery::BatteryWriter`). (The F5/F9 quickslot is a
    // save state, keyed to the disc image — `Session::state_path`, ADR-0023.)
    let mut battery = battery::BatteryWriter::new();
    let bup_path = battery_path(save_base);
    if let Ok(bytes) = fs::read(&bup_path) {
        saturn.load_internal_backup(&bytes);
        battery.note_bup(&bup_path, saturn.internal_backup());
        eprintln!("loaded backup RAM from {}", bup_path.display());
    }

    // The battery backup *cartridge* persists to a `.crtbup` virtual memory-card
    // file, also keyed to the BIOS — both are console-level cards, not per-game
    // (see the save-file keying design). One card per slot (ADR-0028); the
    // active card (`cart_card`) is loaded here and written back like the `.bup`.
    // Only present when a `bram` cart is plugged in.
    let cart_battery_path = cart_card_path(save_base, cfg.cart_card);
    if saturn.cartridge_backup().is_some()
        && let Ok(bytes) = fs::read(&cart_battery_path)
    {
        saturn.load_cartridge_backup(&bytes);
        if let Some(loaded) = saturn.cartridge_backup() {
            battery.note_card(&cart_battery_path, loaded);
        }
        eprintln!(
            "loaded backup cartridge from {}",
            cart_battery_path.display()
        );
    }

    if let Ok(path) = std::env::var("SAT_LOADSTATE") {
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("read save state {path} failed: {e}");
                return Err(ExitCode::from(1));
            }
        };
        if let Err(e) = saturn.load_state(&bytes) {
            eprintln!("load save state {path} failed: {e:?}");
            return Err(ExitCode::from(1));
        }
        saturn.set_port_devices(ports.source[0].device(), ports.source[1].device());
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
    Ok(Booted {
        saturn,
        ports,
        ui,
        rtc_seed,
        battery,
    })
}

/// With the `gpu-presenter` feature, `--gpu=auto|on` presents via an SDL_GPU
/// (Vulkan/SPIR-V) device; `None` = use the `SDL_Renderer` canvas (the default,
/// or a failed probe).
#[cfg(feature = "gpu-presenter")]
fn create_gpu_presenter(
    video: &sdl3::VideoSubsystem,
    cfg: &config::Config,
) -> Option<present_gpu::GpuPresenter> {
    use saturn::vdp2::{FRAME_HEIGHT, FRAME_WIDTH};
    let mode = present_gpu::GpuMode::from_token(&cfg.gpu);
    if !present_gpu::should_probe(mode) {
        return None;
    }
    match present_gpu::GpuPresenter::new(
        video,
        "5thPlanet",
        FRAME_WIDTH as u32 * cfg.scale as u32,
        FRAME_HEIGHT as u32 * cfg.scale as u32,
        FRAME_WIDTH as u32,
        FRAME_HEIGHT as u32,
    ) {
        Ok(mut p) => {
            eprintln!("presentation: SDL_GPU (Vulkan/SPIR-V blit)");
            // Apply the persisted shader choice (CRT pipeline builds lazily).
            p.set_shader(present_gpu::ShaderMode::from_token(&cfg.shader).is_crt());
            Some(p)
        }
        Err(e) => {
            // `on` = the user forced it, so a failure warrants a louder line
            // than an `auto` host that simply has no GPU backend.
            let tag = if mode == present_gpu::GpuMode::On {
                "WARN"
            } else {
                "note"
            };
            eprintln!("SDL_GPU: {tag}: {e}; presenting via the SDL_Renderer blit");
            None
        }
    }
}

/// Audio-pacing + debug knobs read once from the environment.
#[cfg(feature = "sdl-frontend")]
#[derive(Clone, Copy)]
struct PacingKnobs {
    /// SDL queue reserve the emu thread bursts frames toward (`SAT_AUDIO_MS`).
    audio_target_bytes: u32,
    /// Reserve below which a "run N, show 1" catch-up is allowed.
    catchup_floor: u32,
    /// Catch-up ceiling (`SAT_MAX_BURST`, ≥1).
    max_frames_per_burst: u32,
    /// Stall watchdog period (`SAT_AUDIO_WATCHDOG_MS`; 0 disables).
    audio_watchdog_ms: u64,
    perflog: bool,
}

#[cfg(feature = "sdl-frontend")]
impl PacingKnobs {
    fn from_env() -> Self {
        // Audio pacing uses an AtomicU32 mirror of the SDL queue depth that the
        // main thread refreshes every iteration (the SDL audio handles are not
        // Send), plus chunks already sent to the SDL thread but not yet handed
        // to SDL. Without tracking that in-flight reserve, a slow present/event
        // pass can make the emu thread think the host queue is lower than it
        // really is.
        let audio_ms: u64 = env_parse("SAT_AUDIO_MS").unwrap_or(120).max(10);
        let audio_target_bytes = (AUDIO_BYTES_PER_SEC as u64 * audio_ms / 1000) as u32;
        Self {
            audio_target_bytes,
            // Reserve below which catch-up is allowed: a third of the target
            // (~40 ms at the default 120 ms). Normal play hovers near the target
            // (VF2 median ~77 ms), so collapsing only kicks in on a genuine dip
            // toward an under-run.
            catchup_floor: audio_target_bytes / 3,
            // Catch-up ceiling: the *most* game-frames `burst_cap` will collapse
            // into one render when the audio reserve has drained low. Normal play
            // renders every frame regardless (see `burst_cap`); this only bounds
            // recovery from a sub-real-time spike. Default 2 ("run 2, show 1" at
            // worst); `SAT_MAX_BURST=1` disables catch-up entirely.
            max_frames_per_burst: env_parse::<u32>("SAT_MAX_BURST")
                .filter(|&n| n >= 1)
                .unwrap_or(2),
            audio_watchdog_ms: env_parse("SAT_AUDIO_WATCHDOG_MS").unwrap_or(750),
            perflog: std::env::var_os("SAT_PERFLOG").is_some(),
        }
    }
}

/// Emu-thread-only debug knobs (scripted input, frame limit, movie probe).
#[cfg(feature = "sdl-frontend")]
struct EmuDebugKnobs {
    movie_probe: Option<u64>,
    scsp_movie_probe: bool,
    scripted_pad: Option<u16>,
    pad_from: u64,
    pad_to: u64,
    frame_limit: Option<u64>,
}

#[cfg(feature = "sdl-frontend")]
impl EmuDebugKnobs {
    fn from_env() -> Self {
        Self {
            movie_probe: env_parse("SAT_MOVIE_PROBE")
                .or_else(|| std::env::var_os("SAT_MOVIE_PROBE").map(|_| 30)),
            scsp_movie_probe: std::env::var_os("SAT_SCSP_MOVIE_PROBE").is_some(),
            scripted_pad: std::env::var("SAT_PAD")
                .ok()
                .and_then(|s| u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()),
            pad_from: env_parse("SAT_PAD_FROM").unwrap_or(0),
            pad_to: env_parse("SAT_PAD_TO").unwrap_or(u64::MAX),
            frame_limit: env_parse("SAT_FRAMES"),
        }
    }
}

/// State shared between the emu thread and the SDL main thread.
#[cfg(feature = "sdl-frontend")]
#[derive(Clone)]
struct Shared {
    /// Mirror of the SDL audio queue depth (source-rate bytes), refreshed by
    /// the main thread every iteration.
    audio_mirror: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// Bytes sent to the main thread but not yet queued to SDL.
    audio_inflight: std::sync::Arc<std::sync::atomic::AtomicU32>,
    osd_open: std::sync::Arc<std::sync::atomic::AtomicBool>,
    quit: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Sets the quit flag when dropped — including while unwinding from a panic on
/// the SDL main thread, so the emu thread exits (and persists the batteries)
/// instead of spinning forever inside `thread::scope`, which would hang the
/// process rather than report the panic.
#[cfg(feature = "sdl-frontend")]
struct QuitOnDrop<'a>(&'a std::sync::atomic::AtomicBool);

#[cfg(feature = "sdl-frontend")]
impl Drop for QuitOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(feature = "sdl-frontend")]
fn run(
    bios: Vec<u8>,
    disc_spec: Option<String>,
    cart: Cartridge,
    save_base: std::path::PathBuf,
    region: u8,
    mouse_port: Option<u8>,
    cfg: config::Config,
) -> ExitCode {
    use sdl3::audio::{AudioFormat, AudioSpec};
    use sdl3::keyboard::Scancode;
    use sdl3::pixels::PixelFormat;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32};
    use std::sync::mpsc;

    use saturn::vdp2::{FRAME_HEIGHT, FRAME_WIDTH, FRAMEBUFFER_BYTES};

    let Booted {
        saturn,
        ports: initial_ports,
        ui,
        rtc_seed,
        battery,
    } = match boot_machine(
        bios,
        disc_spec.as_deref(),
        cart,
        &save_base,
        region,
        mouse_port,
        &cfg,
    ) {
        Ok(b) => b,
        Err(code) => return code,
    };
    // Seed the SDL thread's mouse-capture gate to match the assignment.
    let mouse_port = initial_ports.mouse_port();
    let launched_spec = disc_spec;

    let sdl = sdl3::init().expect("SDL3 init");
    let video = sdl.video().expect("SDL3 video subsystem");
    // Host game controllers (the SDL gamepad API normalizes every recognized pad
    // — XInput on Windows, evdev on Linux — to one Xbox-style layout). Devices
    // are opened on hot-plug events; SDL also delivers an Added event for each
    // pad already attached at init.
    let controller_subsystem = sdl.gamepad().expect("SDL3 gamepad subsystem");

    // SCSP audio: a 44.1 kHz stereo S16 stream the SCSP fills each frame. SDL3's
    // AudioStream auto-resamples to whatever rate the device opens at, so there
    // is no manual AudioCVT and no device-rate pacing math — `queued_bytes()`
    // is already in source (44.1 kHz) units.
    let audio = sdl.audio().expect("SDL3 audio subsystem");
    let audio_spec = AudioSpec {
        freq: Some(44_100),
        channels: Some(2),
        format: Some(AudioFormat::S16LE),
    };
    let audio_stream = audio
        .open_playback_device(&audio_spec)
        .and_then(|dev| dev.open_device_stream(Some(&audio_spec)))
        .expect("open audio stream");
    // If you hear nothing, check the audio driver: a dummy/empty driver means SDL
    // didn't get a working backend — try `SDL_AUDIODRIVER=pipewire` (or
    // `pulseaudio`/`alsa`) and confirm the app shows up in your system mixer.
    eprintln!("SDL audio: driver={:?}", audio.current_audio_driver());
    // Leave the device PAUSED (SDL opens queues paused) until the reserve has
    // filled once — see the prebuffer gate in `SdlLoop::pump_audio`. Resuming
    // here would let the device drain from t=0 while the queue is still empty
    // during boot, which under-runs exactly once on a cold start (the
    // "first-play buzz").

    // Presentation backend: the framebuffer is rendered in software (for
    // accuracy); this only selects how the finished frame reaches the window.
    // Default = the SDL_Renderer streaming-texture blit (which render driver it
    // picks is `--backend=` / `cfg.backend`, with a fallback chain — see the
    // `present` module). With the `gpu-presenter` feature, `--gpu=auto|on` instead
    // presents via an SDL_GPU (Vulkan/SPIR-V) device. The two are **mutually
    // exclusive**: an SDL_GPU device claims the window its swapchain owns, so the
    // renderer canvas can't also own it. Exactly one backend is `Some`; window
    // controls + presentation route through whichever is live (`backend_window*`
    // and `SdlLoop::present`).
    #[cfg(feature = "gpu-presenter")]
    let gpu = create_gpu_presenter(&video, &cfg);
    #[cfg(feature = "gpu-presenter")]
    let use_renderer = gpu.is_none();
    #[cfg(not(feature = "gpu-presenter"))]
    let use_renderer = true;

    let mut canvas: Option<sdl3::render::WindowCanvas> = None;
    if use_renderer {
        let backend_pref = present::RenderBackend::from_token(&cfg.backend);
        let (c, active_driver) = present::build_canvas(
            &video,
            "5thPlanet",
            FRAME_WIDTH as u32 * cfg.scale as u32,
            FRAME_HEIGHT as u32 * cfg.scale as u32,
            backend_pref,
        );
        eprintln!(
            "render backend: {active_driver} (requested {})",
            backend_pref.to_token()
        );
        canvas = Some(c);
    }
    // Renderer streaming texture (None in GPU mode). `creator` is owned (holds an
    // Rc to the window context, not a borrow of the canvas), so `texture`
    // borrowing it sits fine beside the canvas.
    // ABGR8888 is the SDL packed format whose in-memory byte order on
    // little-endian hosts (everything that matters in 2026) is exactly
    // [R, G, B, A] — what `Saturn::run_frame` writes. RGBA8888 has the opposite
    // byte order on LE; we'd have to swap every pixel for no benefit.
    let creator = canvas.as_ref().map(|c| c.texture_creator());
    let mut texture = creator.as_ref().map(|cr| {
        cr.create_texture_streaming(
            PixelFormat::ABGR8888,
            FRAME_WIDTH as u32,
            FRAME_HEIGHT as u32,
        )
        .expect("create streaming texture")
    });
    // SDL3 defaults streaming textures to linear filtering (blurry on upscale);
    // honour the user's Sharp/Smooth choice (default Sharp = nearest). Re-applied
    // on every texture re-create (resolution change) + on the live OSD toggle.
    // The GPU backend applies the same choice per-frame in `GpuPresenter::present`.
    let sharp = !present::is_smooth(&cfg.scaling);
    if let Some(t) = texture.as_mut() {
        t.set_scale_mode(present::scale_mode(sharp));
    }
    // Aspect handling: keep-ratio (letterbox to the 4:3 display aspect) or
    // fit-screen (stretch). The renderer uses SDL logical presentation; the GPU
    // backend computes the blit destination rect per frame. The Saturn picture is
    // 4:3 with non-square pixels, so its native framebuffer ratio is not the
    // display ratio — see present::logical_size.
    let keep_aspect = present::keeps_aspect(&cfg.aspect);
    let fullscreen = cfg.fullscreen;
    if let Some(c) = canvas.as_mut() {
        present::apply_aspect(c, FRAME_WIDTH as u32, FRAME_HEIGHT as u32, keep_aspect);
    }

    let pacing = PacingKnobs::from_env();
    let emu_knobs = EmuDebugKnobs::from_env();

    // Resolve the configured scancode names; an unknown name falls back to
    // that button's default binding. Lives on the SDL thread (where the
    // keyboard is sampled); the emu thread keeps the names for the OSD.
    let keymap: [Scancode; config::PAD_BUTTONS] = std::array::from_fn(|i| {
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
    // Game-controller button/trigger tokens (SDL strings), resolved per read via
    // `gpad_pressed`. Owned by the SDL thread + rebound live like `keymap`.
    let gpad_map: [String; config::PAD_BUTTONS] = cfg.gpad.clone();

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
    let (emu_tx, emu_rx) = mpsc::channel::<EmuIn>();
    let (frame_tx, frame_rx) = mpsc::sync_channel::<(Vec<u8>, (usize, usize))>(2);
    let (recycle_tx, recycle_rx) = mpsc::channel::<Vec<u8>>();
    // Unbounded by design: the emu thread's `audio_tx.send` must never block (a
    // bounded channel would reintroduce emu stalls). The main thread fully
    // drains it every iteration, so it can't ratchet (and the watchdog backstops
    // a genuinely stuck main thread).
    let (audio_tx, audio_rx) = mpsc::channel::<AudioMsg>();
    let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();
    let shared = Shared {
        audio_mirror: Arc::new(AtomicU32::new(0)),
        audio_inflight: Arc::new(AtomicU32::new(0)),
        osd_open: Arc::new(AtomicBool::new(false)),
        quit: Arc::new(AtomicBool::new(false)),
    };

    // Bundle the dispatcher-owned state for the emu thread (the keymap and
    // window above already consumed what they need from `cfg`).
    let (bios_paths, bios_active) = scan_bios_images(&save_base);
    let bios_names: Vec<String> = bios_paths
        .iter()
        .map(|p| {
            p.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
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
        save_base,
        launched_spec,
        ui,
        cfg,
        bios_paths,
        bios_names,
        bios_active,
        ports: initial_ports,
        pad_list: Vec::new(),
        browse_dir,
        browse_entries: Vec::new(),
        diag_results: Vec::new(),
        battery,
    };
    sess.refresh_browse();

    let emu = EmuThread::new(
        saturn,
        sess,
        EmuLinks {
            rx: emu_rx,
            frame_tx,
            recycle_rx,
            audio_tx,
            ui_tx,
            shared: shared.clone(),
        },
        pacing,
        emu_knobs,
        rtc_seed,
    );

    let mut sdl_loop = SdlLoop {
        sdl: &sdl,
        events: sdl.event_pump().expect("event pump"),
        controller_subsystem,
        controllers: Vec::new(),
        audio_stream,
        canvas,
        creator: creator.as_ref(),
        texture,
        #[cfg(feature = "gpu-presenter")]
        gpu,
        framebuffer: vec![0u8; FRAMEBUFFER_BYTES],
        cur_dims: (FRAME_WIDTH, FRAME_HEIGHT),
        tex_dims: (FRAME_WIDTH, FRAME_HEIGHT),
        sharp,
        keep_aspect,
        fullscreen,
        keymap,
        gpad_map,
        rebind_target: None,
        gpad_rebind_target: None,
        mouse_port,
        mouse_dx: 0,
        mouse_dy: 0,
        mouse_grabbed: false,
        mouse_capture_enabled: true,
        audio_started: false,
        audio_drop_until_reset: false,
        audio_dump: WavDump::from_env("SAT_AUDIO_DUMP"),
        audio_queue_log: std::env::var_os("SAT_AUDIO_QUEUE_LOG").is_some(),
        audio_last_queued: 0,
        audio_last_change: std::time::Instant::now(),
        audio_last_log: std::time::Instant::now(),
        pl_present: std::time::Duration::ZERO,
        pl_iters: 0,
        pl_last: std::time::Instant::now(),
        present_errs: PresentErrs::default(),
        pacing,
        shared: shared.clone(),
        emu_tx,
        audio_rx,
        ui_rx,
        frame_rx,
        recycle_tx,
    };
    sdl_loop.init_window();

    std::thread::scope(|scope| {
        let emu = scope.spawn(move || emu.run());
        // From here on, any exit from this closure — a normal quit or a panic
        // unwinding out of the SDL loop — tells the emu thread to stop.
        let _quit_guard = QuitOnDrop(&shared.quit);
        sdl_loop.run(|| emu.is_finished());
        shared
            .quit
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // The emu thread persists the batteries itself on the way out (even
        // when it panicked), so a failed join loses nothing beyond the last
        // few seconds of unsaved play.
        match emu.join() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => {
                eprintln!("the emulation thread panicked (battery saves were persisted)");
                ExitCode::FAILURE
            }
        }
    })
}

/// The channels + shared atomics the emu thread talks to the SDL thread over.
#[cfg(feature = "sdl-frontend")]
struct EmuLinks {
    rx: std::sync::mpsc::Receiver<EmuIn>,
    frame_tx: std::sync::mpsc::SyncSender<(Vec<u8>, (usize, usize))>,
    recycle_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    audio_tx: std::sync::mpsc::Sender<AudioMsg>,
    ui_tx: std::sync::mpsc::Sender<UiMsg>,
    shared: Shared,
}

/// The emulation thread: owns the machine, the OSD, the render pipe and the
/// session, advances the machine paced on the mirrored audio-queue depth, and
/// hands composited frames to the SDL thread.
#[cfg(feature = "sdl-frontend")]
struct EmuThread {
    saturn: saturn::Saturn,
    osd: osd::Osd,
    pipe: render_pipe::RenderPipe,
    sess: Session,
    links: EmuLinks,
    pacing: PacingKnobs,
    knobs: EmuDebugKnobs,
    /// Port-1 pad mask the recorder logs (kept = the routed port-1 bits).
    held: u16,
    /// Latest per-device input from the SDL thread (EmuIn::Input): the
    /// keyboard mask + per-controller masks keyed by GUID. The port
    /// assignment in `sess.ports` routes these to pad 1/2 each frame.
    kbd_bits: u16,
    gamepad_states: Vec<(String, u16, [u8; 4])>,
    /// Frame buffers circulating emu → main → recycle; seeded so the pipe
    /// spare, the in-flight frame and the displayed frame never starve the
    /// pool (a dip just skips one frame's hand-off).
    pool: Vec<Vec<u8>>,
    /// The last composited frame, retained so the OSD freeze-screen can
    /// dim/overlay it while emulation is paused (toast-free copy).
    last_frame: Vec<u8>,
    last_dims: (usize, usize),
    pl_advance: std::time::Duration,
    pl_frames: u32,
    pl_bursts: [u32; 3],
    pl_last: std::time::Instant,
    audio_pacing_idle_since: Option<std::time::Instant>,
    next_movie_probe: Option<u64>,
    render_death_shown: bool,
    /// Optional input recording (SAT_INPUT_REC) — see [`EmuThread::new`].
    rec: Option<fs::File>,
    rec_frame: u64,
    rec_last: u16,
}

#[cfg(feature = "sdl-frontend")]
impl EmuThread {
    fn new(
        saturn: saturn::Saturn,
        sess: Session,
        links: EmuLinks,
        pacing: PacingKnobs,
        knobs: EmuDebugKnobs,
        rtc_seed: u64,
    ) -> Self {
        use saturn::vdp2::{FRAME_HEIGHT, FRAME_WIDTH, FRAMEBUFFER_BYTES};
        // Optional input recording (SAT_INPUT_REC=<path>): an "input movie"
        // for deterministic headless replay (sdbg `replay`). Writes the RTC
        // seed as a header, then one `<frame> <pad-hex>` line per port-1
        // pad-state change, where `frame` counts emulated frames
        // (advance_frame calls) from reset. The replay must match this
        // cadence (one frame + audio-drain per step) and re-seed the same
        // RTC. Caveat: record a clean play-through — opening the OSD or
        // quicksave/load desyncs the frame count from a fresh-boot replay.
        let rec = std::env::var("SAT_INPUT_REC").ok().and_then(|p| {
            let mut f = fs::File::create(&p).ok()?;
            let _ = writeln!(
                f,
                "# 5thplanet input movie (frame pad-hex; A=400 START=800)"
            );
            let _ = writeln!(f, "rtc {rtc_seed}");
            eprintln!("input recording -> {p}");
            Some(f)
        });
        let next_movie_probe = knobs.movie_probe.filter(|&period| period != 0);
        Self {
            saturn,
            osd: osd::Osd::new(),
            // Render-pipeline worker: composites each frame on a second core
            // from a cloned VDP snapshot, overlapped with the next frame's
            // emulation, so the displayed rate rises from the compute+render
            // rate toward compute-only. The displayed frame trails the emulated
            // frame by one (standard pipeline latency).
            pipe: render_pipe::RenderPipe::new(FRAMEBUFFER_BYTES),
            sess,
            links,
            pacing,
            knobs,
            held: 0,
            kbd_bits: 0,
            gamepad_states: Vec::new(),
            pool: vec![vec![0u8; FRAMEBUFFER_BYTES], vec![0u8; FRAMEBUFFER_BYTES]],
            last_frame: vec![0u8; FRAMEBUFFER_BYTES],
            last_dims: (FRAME_WIDTH, FRAME_HEIGHT),
            pl_advance: std::time::Duration::ZERO,
            pl_frames: 0,
            pl_bursts: [0; 3],
            pl_last: std::time::Instant::now(),
            audio_pacing_idle_since: None,
            next_movie_probe,
            render_death_shown: false,
            rec,
            rec_frame: 0,
            rec_last: 0,
        }
    }

    /// Thread entry: run the loop, then persist the battery memories whatever
    /// happened — a clean quit or a panic (re-raised afterwards so the join
    /// reports it).
    fn run(mut self) {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_loop()));
        self.persist_batteries(true);
        if let Err(p) = res {
            std::panic::resume_unwind(p);
        }
    }

    /// Write the internal backup RAM and the active backup-cart card if they
    /// changed since the last write (`force` = write regardless), keyed to the
    /// session's current save base (a BIOS swap re-keys it mid-session).
    fn persist_batteries(&mut self, force: bool) {
        persist_session_batteries(&self.saturn, &mut self.sess, force);
    }

    fn run_loop(&mut self) {
        use std::sync::atomic::Ordering;
        loop {
            if self.links.shared.quit.load(Ordering::Relaxed) {
                break;
            }
            while let Ok(b) = self.links.recycle_rx.try_recv() {
                self.pool.push(b);
            }
            // Battery-backed saves persist as the game makes them, not only
            // at a clean exit (see `battery`).
            if self.sess.battery.due() {
                self.persist_batteries(false);
            }
            let was_open = self.osd.is_open();
            let mut ctx = build_osd_ctx(&self.saturn, &self.sess, &self.osd);
            if self.drain_messages(&ctx) {
                let _ = self.links.ui_tx.send(UiMsg::Quit);
                break;
            }
            // The menu opened/closed this iteration: its gated fields (slots,
            // names, listings) were built for the other state — rebuild.
            if self.osd.is_open() != was_open {
                ctx = build_osd_ctx(&self.saturn, &self.sess, &self.osd);
            }
            self.links
                .shared
                .osd_open
                .store(self.osd.is_open(), Ordering::Relaxed);

            if self.osd.is_open() {
                self.show_frozen_frame(&ctx);
                std::thread::sleep(std::time::Duration::from_millis(15));
                continue;
            }

            self.route_input();
            let (burst, depth) = self.run_burst();
            self.movie_probe(burst, depth);
            if let Some(limit) = self.knobs.frame_limit
                && self.rec_frame >= limit
            {
                let _ = self.links.ui_tx.send(UiMsg::Quit);
                break;
            }

            // Collect the frame the worker rendered while we computed,
            // overlay any toast, and hand it to the main thread; then
            // dispatch this frame's render to overlap the next iteration.
            // Only when the machine actually advanced — the idle loop
            // (audio reserve full) spins at ~kHz and re-submitting the
            // same state would keep the worker re-rendering an identical
            // frame, burning a core and memory bandwidth for nothing.
            if burst > 0 {
                self.audio_pacing_idle_since = None;
                self.hand_off_frame(&ctx);
            } else {
                self.idle_watchdog();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }

            self.perflog();
        }
    }

    /// Apply every pending message from the SDL thread. Returns `true` if the
    /// user chose Quit.
    fn drain_messages(&mut self, ctx: &osd::OsdCtx) -> bool {
        while let Ok(msg) = self.links.rx.try_recv() {
            if self.handle_message(msg, ctx) {
                return true;
            }
        }
        false
    }

    /// Run one OSD action through [`dispatch_osd`]; `true` = quit.
    fn dispatch(&mut self, action: osd::OsdAction) -> bool {
        dispatch_osd(
            action,
            &mut self.osd,
            &mut self.saturn,
            &mut self.sess,
            &self.links.ui_tx,
            &self.links.audio_tx,
        )
    }

    fn handle_message(&mut self, msg: EmuIn, ctx: &osd::OsdCtx) -> bool {
        match msg {
            EmuIn::Input { kbd, pads } => {
                self.kbd_bits = kbd;
                self.gamepad_states = pads;
            }
            EmuIn::PadList(list) => {
                // Re-resolve the port assignment against the new device list
                // (the persisted port1/port2 tokens are the intent; a
                // reconnected pad reattaches, an unplugged one falls back to
                // None), then re-point the SMPC ports + the SDL mouse-capture
                // gate.
                self.sess.pad_list = list;
                self.sess.ports = ports::Ports::from_tokens(
                    &self.sess.cfg.port1,
                    &self.sess.cfg.port2,
                    &self.sess.pad_list,
                );
                apply_ports(&mut self.saturn, &self.sess.ports, &self.links.ui_tx);
            }
            EmuIn::Mouse(dx, dy, buttons) => self.saturn.feed_mouse(dx, dy, buttons),
            EmuIn::Toggle => {
                let _ = self.osd.toggle();
            }
            EmuIn::Nav(nav) => {
                if let Some(action) = self.osd.handle(nav, ctx) {
                    return self.dispatch(action);
                }
            }
            EmuIn::Quicksave => {
                match write_atomic(&self.sess.state_path(), &self.saturn.save_state()) {
                    Ok(()) => self.osd.set_toast("Quicksave", 90),
                    Err(e) => eprintln!("save state failed: {e}"),
                }
            }
            EmuIn::Quickload => self.quickload(),
            // F11/F12 reuse the menu's dispatch (updates ui state + config +
            // sends the UiMsg back to the SDL thread + toast).
            EmuIn::ToggleFullscreen => {
                let _ = self.dispatch(osd::OsdAction::ToggleFullscreen);
            }
            EmuIn::ToggleAspect => {
                let _ = self.dispatch(osd::OsdAction::ToggleAspect);
            }
            #[cfg(debug_assertions)]
            EmuIn::PlayCdda => {
                if self.saturn.dbg_play_first_audio_track() {
                    self.osd.set_toast("Playing CD audio", 120);
                } else {
                    self.osd.set_toast("No CD audio track", 120);
                }
            }
            EmuIn::BindResult(b, captured) => {
                self.osd.end_capture();
                let i = b as usize % config::PAD_BUTTONS;
                match captured {
                    Some(name) => {
                        self.osd
                            .set_toast(format!("{} = {name}", config::BUTTON_NAMES[i]), 120);
                        self.sess.cfg.keys[i] = name;
                        self.sess.cfg.save();
                    }
                    None => self.osd.set_toast("Rebind cancelled", 90),
                }
            }
            EmuIn::GamepadBindResult(b, captured) => {
                self.osd.end_capture();
                let i = b as usize % config::PAD_BUTTONS;
                match captured {
                    Some(tok) => {
                        self.osd
                            .set_toast(format!("{} = {tok}", config::BUTTON_NAMES[i]), 120);
                        self.sess.cfg.gpad[i] = tok;
                        self.sess.cfg.save();
                    }
                    None => self.osd.set_toast("Rebind cancelled", 90),
                }
            }
        }
        false
    }

    /// F9: load the quickslot state (the SDL thread has already paused and
    /// cleared the audio stream; every outcome resets the audio timeline).
    fn quickload(&mut self) {
        let path = self.sess.state_path();
        match fs::read(&path) {
            Ok(bytes) => match self.saturn.load_state(&bytes) {
                Ok(()) => {
                    apply_ports(&mut self.saturn, &self.sess.ports, &self.links.ui_tx);
                    let _ = self.links.audio_tx.send(AudioMsg::Reset);
                    eprintln!("quickload: {}", path.display());
                    self.osd.set_toast("Quickload", 90);
                }
                Err(e) => {
                    let _ = self.links.audio_tx.send(AudioMsg::Reset);
                    eprintln!("load state {} failed: {e}", path.display());
                }
            },
            Err(e) => {
                let _ = self.links.audio_tx.send(AudioMsg::Reset);
                eprintln!("no state to load at {} ({e})", path.display());
            }
        }
    }

    /// Menu open: emulation is frozen — don't advance the machine or feed the
    /// pads; composite the menu over the retained last frame.
    fn show_frozen_frame(&mut self, ctx: &osd::OsdCtx) {
        self.saturn.set_pad1(0);
        self.saturn.set_pad2(0);
        self.osd.tick_toast();
        if let Some(mut buf) = self.pool.pop() {
            buf.copy_from_slice(&self.last_frame);
            let (w, h) = self.last_dims;
            self.osd.render_overlay(&mut buf, w, h, ctx);
            if let Err(e) = self.links.frame_tx.try_send((buf, self.last_dims)) {
                let (b, _) = match e {
                    std::sync::mpsc::TrySendError::Full(v) => v,
                    std::sync::mpsc::TrySendError::Disconnected(v) => v,
                };
                self.pool.push(b);
            }
        }
    }

    /// Route this frame's per-device input to the two ports via the assignment,
    /// drive both SMPC pads + the 3D-pad analog channels (neutral when a port
    /// isn't an analog pad), and record a pad-state change for an input movie
    /// at the frame it takes effect (the burst holds `held` constant).
    fn route_input(&mut self) {
        if self.knobs.scripted_pad.is_none() {
            let (p1, p2) = self.sess.ports.route(self.kbd_bits, &self.gamepad_states);
            self.held = p1;
            self.saturn.set_pad1(p1);
            self.saturn.set_pad2(p2);
            let analog = self.sess.ports.route_analog(&self.gamepad_states);
            self.saturn
                .set_analog1(analog[0].unwrap_or(saturn::smpc::analog::NEUTRAL));
            self.saturn
                .set_analog2(analog[1].unwrap_or(saturn::smpc::analog::NEUTRAL));
        }
        if let Some(f) = self.rec.as_mut()
            && self.held != self.rec_last
        {
            let _ = writeln!(f, "{} {:04X}", self.rec_frame, self.held);
            let _ = f.flush();
            self.rec_last = self.held;
        }
    }

    /// Audio-paced burst: run frames until the (mirrored) SDL queue depth plus
    /// what this burst just produced reaches the target. `burst_cap` renders
    /// every game-frame in normal play and only collapses (run N, show 1) when
    /// the reserve has drained low — chasing the full target unconditionally
    /// dropped ~1/3 of VF2's frames and added input latency. Returns the number
    /// of frames run and the pacing depth it ended on.
    fn run_burst(&mut self) -> (u32, u32) {
        use std::sync::atomic::Ordering;
        let shared = &self.links.shared;
        let mut depth = shared
            .audio_mirror
            .load(Ordering::Relaxed)
            .saturating_add(shared.audio_inflight.load(Ordering::Relaxed));
        let cap = burst_cap(
            depth,
            self.pacing.catchup_floor,
            self.pacing.max_frames_per_burst,
        );
        let mut burst = 0u32;
        while depth < self.pacing.audio_target_bytes && burst < cap {
            if let Some(bits) = self.knobs.scripted_pad {
                let frame = self.rec_frame + burst as u64;
                self.saturn.set_pad1(
                    if (self.knobs.pad_from..self.knobs.pad_to).contains(&frame) {
                        bits
                    } else {
                        0
                    },
                );
            }
            let t = std::time::Instant::now();
            self.saturn.advance_frame();
            self.pl_advance += t.elapsed();
            let chunk = self.saturn.take_audio();
            let bytes = (chunk.len() * 2) as u32;
            depth += bytes;
            shared.audio_inflight.fetch_add(bytes, Ordering::Relaxed);
            if self
                .links
                .audio_tx
                .send(AudioMsg::Chunk {
                    samples: chunk,
                    bytes,
                })
                .is_err()
            {
                atomic_saturating_sub(&shared.audio_inflight, bytes);
            }
            burst += 1;
        }
        self.pl_frames += burst;
        self.pl_bursts[(burst as usize).min(2)] += 1;
        self.rec_frame += burst as u64;
        (burst, depth)
    }

    /// `SAT_MOVIE_PROBE`: a periodic CD/CPU/audio status line (classifies a
    /// stalled FMV — frames advancing while the CD froze, or vice versa).
    fn movie_probe(&mut self, burst: u32, depth: u32) {
        use std::sync::atomic::Ordering;
        let Some(period) = self.knobs.movie_probe else {
            return;
        };
        if burst == 0 {
            return;
        }
        let rec_frame = self.rec_frame;
        let due = period == 0 || {
            let next = self.next_movie_probe.get_or_insert(period);
            let crossed = rec_frame >= *next;
            while rec_frame >= *next {
                *next = next.saturating_add(period);
            }
            crossed
        };
        if !due {
            return;
        }
        let (cd_status, cd_fad, cd_left, cd_free, parts) = self.saturn.bus.cd_block.debug_state();
        let part_summary = parts
            .iter()
            .enumerate()
            .filter(|&(_, &n)| n != 0)
            .map(|(i, n)| format!("{i}:{n}"))
            .collect::<Vec<_>>()
            .join(",");
        let scsp_probe = scsp_probe_string(&self.saturn.bus.scsp, self.knobs.scsp_movie_probe);
        let shared = &self.links.shared;
        let mirror = shared.audio_mirror.load(Ordering::Relaxed);
        let inflight = shared.audio_inflight.load(Ordering::Relaxed);
        eprintln!(
            "SDLMOVIE f={rec_frame:04} pc={:08X} cd_st={cd_status:02X} fad={cd_fad} left={cd_left} free={cd_free} parts=[{part_summary}] depth={depth} mirror={mirror} inflight={inflight} burst={burst}{scsp_probe}",
            self.saturn.master().regs.pc,
        );
    }

    /// Collect the worker's frame, overlay any toast, hand it to the SDL thread,
    /// then dispatch this frame's render.
    fn hand_off_frame(&mut self, ctx: &osd::OsdCtx) {
        if let Some((mut rendered, dims)) = self.pipe.wait() {
            self.last_frame.copy_from_slice(&rendered);
            self.last_dims = dims;
            self.osd.tick_toast();
            self.osd.render_overlay(&mut rendered, dims.0, dims.1, ctx);
            match self.links.frame_tx.try_send((rendered, dims)) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full((b, _)))
                | Err(std::sync::mpsc::TrySendError::Disconnected((b, _))) => self.pool.push(b),
            }
        }
        // Feed the pipe a spare whenever it lacks one — this must be
        // independent of the wait() result above: if a submit was ever
        // skipped for want of a spare, wait() yields None from then
        // on, and a recycle gated behind it would never run again
        // (the black-screen pipeline deadlock).
        if self.pipe.needs_spare()
            && let Some(b) = self.pool.pop()
        {
            self.pipe.recycle(b);
        }
        self.pipe.submit(&self.saturn);
        // A compositor panic killed the render worker: the pipe now
        // renders inline (logged on stderr) — tell the user once.
        if self.pipe.is_dead() && !self.render_death_shown {
            self.render_death_shown = true;
            self.osd.set_toast("Render worker crashed - see log", 300);
        }
    }

    /// Audio reserve full: nothing to do until it drains a bit. If the SDL
    /// queue mirror/in-flight accounting ever gets stuck high across an OSD
    /// reset/load edge, the emulation thread would otherwise stop producing
    /// frames forever — so after `SAT_AUDIO_WATCHDOG_MS` idle, clear it.
    fn idle_watchdog(&mut self) {
        use std::sync::atomic::Ordering;
        let now = std::time::Instant::now();
        let idle_since = *self.audio_pacing_idle_since.get_or_insert(now);
        // `SAT_AUDIO_WATCHDOG_MS=0` disables the watchdog (matching
        // the main-thread one); `.max(16)` is only a floor for
        // nonzero values, so it can't fire every idle frame.
        let ms = self.pacing.audio_watchdog_ms;
        if ms != 0 && now.duration_since(idle_since) >= std::time::Duration::from_millis(ms.max(16))
        {
            let shared = &self.links.shared;
            let mirror = shared.audio_mirror.load(Ordering::Relaxed);
            let inflight = shared.audio_inflight.load(Ordering::Relaxed);
            eprintln!(
                "SDL audio pacing watchdog: stalled with mirror={mirror} inflight={inflight} target={}; clearing pacing reserve",
                self.pacing.audio_target_bytes
            );
            shared.audio_mirror.store(0, Ordering::Relaxed);
            shared.audio_inflight.store(0, Ordering::Relaxed);
            self.audio_pacing_idle_since = None;
        }
    }

    /// `SAT_PERFLOG`: once a second, frames/s + the burst distribution + the
    /// mean advance cost.
    fn perflog(&mut self) {
        if !self.pacing.perflog || self.pl_last.elapsed().as_secs() < 1 {
            return;
        }
        eprintln!(
            "EMU  frames/s={} burst[0/1/2]={}/{}/{} | advance avg {:.2} ms/frame",
            self.pl_frames,
            self.pl_bursts[0],
            self.pl_bursts[1],
            self.pl_bursts[2],
            if self.pl_frames > 0 {
                self.pl_advance.as_secs_f64() * 1e3 / self.pl_frames as f64
            } else {
                0.0
            },
        );
        self.pl_advance = std::time::Duration::ZERO;
        self.pl_frames = 0;
        self.pl_bursts = [0; 3];
        self.pl_last = std::time::Instant::now();
    }
}

/// Persist the internal backup RAM and (when a backup cart is plugged) the
/// active card through the session's [`battery::BatteryWriter`] — only what
/// changed, unless `force`.
#[cfg(feature = "sdl-frontend")]
fn persist_session_batteries(saturn: &saturn::Saturn, sess: &mut Session, force: bool) {
    sess.battery.persist_bup(
        &battery_path(&sess.save_base),
        saturn.internal_backup(),
        force,
    );
    if let Some(bytes) = saturn.cartridge_backup() {
        sess.battery.persist_card(
            &cart_card_path(&sess.save_base, sess.cfg.cart_card),
            bytes,
            force,
        );
    }
}

/// Snapshot the OSD's view of the session. Fields only the open menu reads
/// (file stats, cloned name lists, listings) are filled only while it is open,
/// keeping the gameplay hot path (this runs every emu-loop iteration, at kHz
/// while idle) allocation- and syscall-free.
#[cfg(feature = "sdl-frontend")]
fn build_osd_ctx(saturn: &saturn::Saturn, sess: &Session, osd: &osd::Osd) -> osd::OsdCtx {
    let open = osd.is_open();
    osd::OsdCtx {
        disc_present: saturn.has_disc(),
        // Which save slots exist — 10 file stats, so only while open (the
        // Save/Load screens read it).
        slot_used: if open {
            std::array::from_fn(|n| sess.slot_path(n as u8).exists())
        } else {
            Default::default()
        },
        scale: sess.ui.scale,
        fullscreen: sess.ui.fullscreen,
        sharp: sess.ui.sharp,
        keep_aspect: sess.ui.keep_aspect,
        region: sess.ui.region,
        cart: sess.ui.cart,
        backup_card: sess.cfg.cart_card,
        // Which card files exist (the manager screen reads it).
        backup_card_used: if open {
            std::array::from_fn(|n| cart_card_path(&sess.save_base, n as u8).exists())
        } else {
            [false; config::BACKUP_CARDS]
        },
        // Per-port source labels for the Controller screen's CyclePort rows.
        port_labels: if open {
            [sess.ports.source[0].label(), sess.ports.source[1].label()]
        } else {
            [String::new(), String::new()]
        },
        backend: sess.ui.backend,
        #[cfg(feature = "gpu-presenter")]
        shader_crt: sess.ui.shader_crt,
        // pad_keys/gpad_binds/bios_names are read only by the Controller/BIOS
        // settings sub-screens (via `items()`, gated on the menu being open);
        // the fixed arrays get empty (non-allocating) strings when closed.
        pad_keys: if open {
            sess.cfg.keys.clone()
        } else {
            std::array::from_fn(|_| String::new())
        },
        gpad_binds: if open {
            sess.cfg.gpad.clone()
        } else {
            std::array::from_fn(|_| String::new())
        },
        bios_names: if open {
            sess.bios_names.clone()
        } else {
            Vec::new()
        },
        bios_active: sess.bios_active,
        // The browser listing can be large; only read while the menu is open.
        browse_entries: if open {
            sess.browse_entries.clone()
        } else {
            Vec::new()
        },
        browse_dir: if open {
            sess.browse_dir.to_string_lossy().into_owned()
        } else {
            String::new()
        },
        diag_results: if open {
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
        cpu_pc: if open { saturn.master().regs.pc } else { 0 },
        cpu_where: if open {
            classify_pc(saturn.master().regs.pc)
        } else {
            ""
        },
    }
}

/// Renderer present-path failures, each logged once (a persistent failure
/// would otherwise print every frame) — the frame is skipped, never fatal.
#[cfg(feature = "sdl-frontend")]
#[derive(Default)]
struct PresentErrs {
    recreate: bool,
    upload: bool,
    blit: bool,
}

/// Log `msg` the first time `flag` is raised.
#[cfg(feature = "sdl-frontend")]
fn log_once(flag: &mut bool, msg: impl FnOnce() -> String) {
    if !*flag {
        *flag = true;
        eprintln!("{}", msg());
    }
}

/// The SDL main thread: events → emu messages, audio chunks → the SDL stream,
/// window-affecting UI messages, and frame presentation.
#[cfg(feature = "sdl-frontend")]
struct SdlLoop<'a> {
    sdl: &'a sdl3::Sdl,
    events: sdl3::EventPump,
    controller_subsystem: sdl3::GamepadSubsystem,
    controllers: Vec<sdl3::gamepad::Gamepad>,
    audio_stream: sdl3::audio::AudioStreamOwner,
    canvas: Option<sdl3::render::WindowCanvas>,
    creator: Option<&'a sdl3::render::TextureCreator<sdl3::video::WindowContext>>,
    texture: Option<sdl3::render::Texture<'a>>,
    #[cfg(feature = "gpu-presenter")]
    gpu: Option<present_gpu::GpuPresenter>,
    /// The displayed frame (the emu thread's buffers swap through it).
    framebuffer: Vec<u8>,
    /// Dims of `framebuffer`'s frame, and the dims the renderer texture was
    /// built for (they differ only while a texture re-create keeps failing).
    cur_dims: (usize, usize),
    tex_dims: (usize, usize),
    sharp: bool,
    keep_aspect: bool,
    /// Mirrors the window state so the resize-snap is skipped while fullscreen
    /// (its Resized events are the display size, not a user request).
    fullscreen: bool,
    keymap: [sdl3::keyboard::Scancode; config::PAD_BUTTONS],
    gpad_map: [String; config::PAD_BUTTONS],
    /// OSD rebind: which pad button (if any) captures the next keypress.
    rebind_target: Option<u8>,
    gpad_rebind_target: Option<u8>,
    /// Shuttle Mouse capture state (only used when a port carries the mouse).
    mouse_port: Option<u8>,
    mouse_dx: i32,
    mouse_dy: i32,
    mouse_grabbed: bool,
    /// F10 toggles capture: released, the pointer behaves as a normal host
    /// mouse again (motion still feeds the game while over the window —
    /// "transparent" mode — but can leave it and hit the screen edge).
    mouse_capture_enabled: bool,
    audio_started: bool,
    audio_drop_until_reset: bool,
    audio_dump: Option<WavDump>,
    audio_queue_log: bool,
    audio_last_queued: u32,
    audio_last_change: std::time::Instant,
    audio_last_log: std::time::Instant,
    pl_present: std::time::Duration,
    pl_iters: u32,
    pl_last: std::time::Instant,
    present_errs: PresentErrs,
    pacing: PacingKnobs,
    shared: Shared,
    emu_tx: std::sync::mpsc::Sender<EmuIn>,
    audio_rx: std::sync::mpsc::Receiver<AudioMsg>,
    ui_rx: std::sync::mpsc::Receiver<UiMsg>,
    frame_rx: std::sync::mpsc::Receiver<(Vec<u8>, (usize, usize))>,
    recycle_tx: std::sync::mpsc::Sender<Vec<u8>>,
}

#[cfg(feature = "sdl-frontend")]
impl SdlLoop<'_> {
    /// The live presentation backend's window (see [`backend_window_mut`]).
    fn window_mut(&mut self) -> &mut sdl3::video::Window {
        backend_window_mut(
            &mut self.canvas,
            #[cfg(feature = "gpu-presenter")]
            &mut self.gpu,
        )
    }

    /// Icon + initial fullscreen / 4:3 window shape. Windowed mode is always
    /// aspect-locked to 4:3: we own the window, so snap it to 4:3 (no bars, no
    /// distortion) rather than letterboxing inside a mismatched window. The
    /// keep/stretch toggle then only takes effect in fullscreen (where the
    /// display shape is fixed). See `present::window_aspect_lock`.
    fn init_window(&mut self) {
        set_window_icon(self.window_mut());
        if self.fullscreen {
            let _ = self.window_mut().set_fullscreen(true);
        } else {
            snap_window_to_4_3(self.window_mut());
        }
    }

    /// The main loop; returns on quit (window close, OSD Quit) or once
    /// `emu_finished` reports the emulation thread has exited (e.g. panicked).
    fn run(&mut self, emu_finished: impl Fn() -> bool) {
        loop {
            if self.handle_events() || emu_finished() {
                return;
            }
            self.send_input();
            self.pump_audio();
            if self.apply_ui_messages() {
                return;
            }
            self.present();
        }
    }

    /// Poll SDL events. Returns `true` on a quit request.
    fn handle_events(&mut self) -> bool {
        // Collect first: the handlers need `&mut self` while `poll_iter`
        // borrows the pump.
        let evs: Vec<sdl3::event::Event> = self.events.poll_iter().collect();
        for ev in evs {
            if self.handle_event(ev) {
                return true;
            }
        }
        false
    }

    fn handle_event(&mut self, ev: sdl3::event::Event) -> bool {
        use osd::Nav;
        use sdl3::event::{Event, WindowEvent};
        use sdl3::keyboard::Keycode;
        use std::sync::atomic::Ordering;
        let osd_open = self.shared.osd_open.load(Ordering::Relaxed);
        match ev {
            Event::Quit { .. } => return true,
            // Windowed mode is aspect-locked to 4:3: when the user resizes
            // the window, snap it back so the picture never letterboxes.
            // The snap is idempotent (no-op once 4:3), so the re-fired
            // Resized event converges instead of looping. Skipped while
            // fullscreen (those Resized events are the display size).
            Event::Window {
                win_event: WindowEvent::Resized(..),
                ..
            } if !self.fullscreen => {
                snap_window_to_4_3(self.window_mut());
            }
            // Esc on the keyboard cancels an armed *gamepad* rebind
            // (before the nav arm consumes it as a menu Back).
            Event::KeyDown {
                keycode: Some(Keycode::Escape),
                ..
            } if self.gpad_rebind_target.is_some() => {
                let b = self.gpad_rebind_target.take().expect("armed");
                let _ = self.emu_tx.send(EmuIn::GamepadBindResult(b, None));
            }
            // An armed rebind owns the next keypress (before the OSD
            // nav arm — the menu is open during capture). Esc cancels.
            Event::KeyDown {
                keycode: Some(kc),
                scancode,
                ..
            } if self.rebind_target.is_some() => {
                let b = self.rebind_target.take().expect("armed");
                if kc == Keycode::Escape {
                    let _ = self.emu_tx.send(EmuIn::BindResult(b, None));
                } else if let Some(sc) = scancode {
                    self.keymap[b as usize % config::PAD_BUTTONS] = sc;
                    let _ = self
                        .emu_tx
                        .send(EmuIn::BindResult(b, Some(sc.name().to_string())));
                } else {
                    // A key with no scancode: keep waiting.
                    self.rebind_target = Some(b);
                }
            }
            Event::KeyDown {
                keycode: Some(kc), ..
            } if osd_open => {
                let msg = match kc {
                    Keycode::Up => Some(EmuIn::Nav(Nav::Up)),
                    Keycode::Down => Some(EmuIn::Nav(Nav::Down)),
                    Keycode::Return | Keycode::Z => Some(EmuIn::Nav(Nav::Select)),
                    Keycode::Backspace | Keycode::X => Some(EmuIn::Nav(Nav::Back)),
                    Keycode::Escape => Some(EmuIn::Toggle),
                    _ => None,
                };
                if let Some(m) = msg {
                    let _ = self.emu_tx.send(m);
                }
            }
            Event::KeyDown {
                keycode: Some(Keycode::Escape),
                ..
            } => {
                let _ = self.emu_tx.send(EmuIn::Toggle);
            }
            Event::KeyDown {
                keycode: Some(Keycode::F5),
                ..
            } => {
                let _ = self.emu_tx.send(EmuIn::Quicksave);
            }
            Event::KeyDown {
                keycode: Some(Keycode::F9),
                ..
            } => {
                let _ = self.audio_stream.pause();
                let _ = self.audio_stream.clear();
                self.audio_started = false;
                self.audio_drop_until_reset = true;
                self.shared.audio_mirror.store(0, Ordering::Relaxed);
                let _ = self.emu_tx.send(EmuIn::Quickload);
            }
            // F11 toggles fullscreen, F12 the fullscreen aspect mode
            // (Keep ratio ↔ Fit screen) — both routed to the OSD dispatch
            // so the menu labels + config track them.
            Event::KeyDown {
                keycode: Some(Keycode::F11),
                ..
            } => {
                let _ = self.emu_tx.send(EmuIn::ToggleFullscreen);
            }
            Event::KeyDown {
                keycode: Some(Keycode::F12),
                ..
            } => {
                let _ = self.emu_tx.send(EmuIn::ToggleAspect);
            }
            // F8 CD-audio play is a debug-build-only diagnostic (it
            // drives CD-DA outside the BIOS path); release builds omit it.
            #[cfg(debug_assertions)]
            Event::KeyDown {
                keycode: Some(Keycode::F8),
                ..
            } => {
                let _ = self.emu_tx.send(EmuIn::PlayCdda);
            }
            Event::MouseMotion { xrel, yrel, .. } if self.mouse_port.is_some() => {
                // SDL3 mouse coordinates are floats; we accumulate integer
                // deltas for the Shuttle Mouse.
                self.mouse_dx += xrel as i32;
                self.mouse_dy += yrel as i32;
            }
            Event::KeyDown {
                keycode: Some(Keycode::F10),
                ..
            } if self.mouse_port.is_some() => {
                self.mouse_capture_enabled = !self.mouse_capture_enabled;
            }
            // Game-controller hot-plug. `which` is the joystick instance
            // id on both Added and Removed (SDL3 semantics); the dedupe
            // guard makes a startup enumeration + the init-time Added
            // events idempotent.
            Event::GamepadAdded { which, .. } => match self.controller_subsystem.open(which) {
                Ok(c) => {
                    if !self.controllers.iter().any(|o| o.id().ok() == c.id().ok()) {
                        eprintln!("controller connected: {}", c.name().unwrap_or_default());
                        self.controllers.push(c);
                        self.send_pad_list();
                    }
                }
                Err(e) => eprintln!("controller open failed: {e}"),
            },
            Event::GamepadRemoved { which, .. } => {
                self.controllers.retain(|c| c.id().ok() != Some(which));
                self.send_pad_list();
            }
            // Controller navigation of the open menu: D-pad moves, A
            // selects, B backs out, Start toggles. Suppressed while a
            // key-capture rebind is armed (that modal owns the input).
            // An armed gamepad rebind captures the next controller
            // button (or a trigger, below) — before the nav arm.
            Event::GamepadButtonDown { button, .. } if self.gpad_rebind_target.is_some() => {
                let b = self.gpad_rebind_target.take().expect("armed");
                let tok = button.string();
                self.gpad_map[b as usize % config::PAD_BUTTONS] = tok.clone();
                let _ = self.emu_tx.send(EmuIn::GamepadBindResult(b, Some(tok)));
            }
            Event::GamepadAxisMotion { axis, value, .. }
                if self.gpad_rebind_target.is_some()
                    && matches!(
                        axis,
                        sdl3::gamepad::Axis::TriggerLeft | sdl3::gamepad::Axis::TriggerRight
                    )
                    && value > 24000 =>
            {
                let b = self.gpad_rebind_target.take().expect("armed");
                let tok = axis.string();
                self.gpad_map[b as usize % config::PAD_BUTTONS] = tok.clone();
                let _ = self.emu_tx.send(EmuIn::GamepadBindResult(b, Some(tok)));
            }
            Event::GamepadButtonDown { button, .. }
                if osd_open
                    && self.rebind_target.is_none()
                    && self.gpad_rebind_target.is_none() =>
            {
                use sdl3::gamepad::Button;
                let msg = match button {
                    Button::DPadUp => Some(EmuIn::Nav(Nav::Up)),
                    Button::DPadDown => Some(EmuIn::Nav(Nav::Down)),
                    Button::South => Some(EmuIn::Nav(Nav::Select)),
                    Button::East => Some(EmuIn::Nav(Nav::Back)),
                    Button::Start => Some(EmuIn::Toggle),
                    _ => None,
                };
                if let Some(m) = msg {
                    let _ = self.emu_tx.send(m);
                }
            }
            _ => {}
        }
        false
    }

    fn send_pad_list(&self) {
        let _ = self.emu_tx.send(EmuIn::PadList(pad_list_of(
            &self.controllers,
            &self.controller_subsystem,
        )));
    }

    /// Sample the host keyboard + controllers (+ the Shuttle Mouse) and send
    /// this iteration's per-device input to the emu thread, which routes each
    /// device to its assigned port (`ports::Ports`).
    fn send_input(&mut self) {
        use saturn::smpc::pad;
        use sdl3::gamepad::Axis;
        use std::sync::atomic::Ordering;
        // Keyboard → the rebindable pad mask (defaults: arrows = D-pad, Z/X/C =
        // A/B/C, A/S/D = X/Y/Z, Q/W = L/R, Enter = Start).
        let keys = self.events.keyboard_state();
        let mut kbd = 0u16;
        for (sc, bit) in self.keymap.iter().zip(PAD_BITS) {
            if keys.is_scancode_pressed(*sc) {
                kbd |= bit;
            }
        }
        let return_held = keys.is_scancode_pressed(sdl3::keyboard::Scancode::Return);
        // One held-bits entry per connected controller, keyed by its stable
        // SDL GUID (so the port assignment survives relaunch + hot-plug),
        // through the rebindable gamepad map; the left stick doubles as the
        // D-pad.
        let mut pad_inputs: Vec<(String, u16, [u8; 4])> =
            Vec::with_capacity(self.controllers.len());
        // Normalize an SDL axis to the Saturn 3D-pad byte: a centered stick
        // (i16 −32768..32767) → 0x00 (min) .. 0x80 (center) .. 0xFF (max); a
        // one-sided trigger (0..32767) → 0x00 (released) .. 0xFF (pressed).
        // The stick maps 0 exactly onto 0x80 (the `analog::NEUTRAL` center)
        // rather than 0x7F, so a resting pad matches the neutral default.
        let norm_stick = |v: i16| (128 + (v as i32) * 128 / 32768).clamp(0, 255) as u8;
        let norm_trig = |v: i16| ((v.max(0) as i32) * 255 / 32767) as u8;
        const TH: i16 = 16384; // half of full deflection
        for c in &self.controllers {
            let mut bits = 0u16;
            // Apply the rebindable gamepad map (token per Saturn button,
            // BUTTON_NAMES order), same as the keyboard `keymap` loop.
            for (tok, bit) in self.gpad_map.iter().zip(PAD_BITS) {
                if gpad_pressed(c, tok) {
                    bits |= bit;
                }
            }
            // The left analog stick always doubles as the D-pad.
            let (lx, ly) = (c.axis(Axis::LeftX), c.axis(Axis::LeftY));
            if lx < -TH {
                bits |= pad::LEFT;
            } else if lx > TH {
                bits |= pad::RIGHT;
            }
            if ly < -TH {
                bits |= pad::UP;
            } else if ly > TH {
                bits |= pad::DOWN;
            }
            // Analog channels for a 3D-pad assignment ([X, Y, L, R]); the
            // left stick + both triggers. Ignored unless this controller's
            // port is a `gamepad3d` (route_analog gates it).
            let analog = [
                norm_stick(lx),
                norm_stick(ly),
                norm_trig(c.axis(Axis::TriggerLeft)),
                norm_trig(c.axis(Axis::TriggerRight)),
            ];
            let guid = c
                .id()
                .ok()
                .map(|j| self.controller_subsystem.guid_for_id(j).string())
                .unwrap_or_default();
            pad_inputs.push((guid, bits, analog));
        }
        let _ = self.emu_tx.send(EmuIn::Input {
            kbd,
            pads: pad_inputs,
        });

        // Shuttle Mouse: this iteration's accumulated relative motion plus
        // the held buttons (host Left/Right/Middle clicks; Return doubles
        // as the mouse's Start button so --mouse=1 — no pad — can still
        // start/pause). Relative-mouse mode is enabled while the OSD is
        // closed, so the host cursor is captured + hidden and the game
        // draws its own; Esc (the OSD) releases the pointer.
        if self.mouse_port.is_some() {
            let want_grab =
                self.mouse_capture_enabled && !self.shared.osd_open.load(Ordering::Relaxed);
            if want_grab != self.mouse_grabbed {
                self.mouse_grabbed = want_grab;
                let grabbed = self.mouse_grabbed;
                let window = backend_window(
                    &self.canvas,
                    #[cfg(feature = "gpu-presenter")]
                    &self.gpu,
                );
                self.sdl.mouse().set_relative_mouse_mode(window, grabbed);
            }
            let ms = self.events.mouse_state();
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
            if return_held {
                buttons |= saturn::smpc::mouse::START;
            }
            let _ = self
                .emu_tx
                .send(EmuIn::Mouse(self.mouse_dx, self.mouse_dy, buttons));
            self.mouse_dx = 0;
            self.mouse_dy = 0;
        }
    }

    /// Push the emu thread's audio into the SDL3 stream (which auto-resamples
    /// to the device rate) and refresh the depth mirror it paces on.
    /// `queued_bytes()` is already in source (44.1 kHz) units, so no
    /// device-rate scaling is needed. Start playback once the reserve has
    /// first filled, so a cold start never drains an empty stream.
    fn pump_audio(&mut self) {
        use std::sync::atomic::Ordering;
        let target = self.pacing.audio_target_bytes;
        let mut audio_chunks_this_iter = 0u32;
        while let Ok(msg) = self.audio_rx.try_recv() {
            match msg {
                AudioMsg::Reset => {
                    let _ = self.audio_stream.pause();
                    let _ = self.audio_stream.clear();
                    self.audio_started = false;
                    self.audio_drop_until_reset = false;
                    self.shared.audio_mirror.store(0, Ordering::Relaxed);
                    self.shared.audio_inflight.store(0, Ordering::Relaxed);
                    if let Some(dump) = self.audio_dump.as_mut() {
                        dump.reset();
                    }
                    self.audio_last_queued = 0;
                    self.audio_last_change = std::time::Instant::now();
                }
                AudioMsg::Chunk { samples, bytes } => {
                    atomic_saturating_sub(&self.shared.audio_inflight, bytes);
                    audio_chunks_this_iter += 1;
                    if !self.audio_drop_until_reset
                        && self.audio_stream.put_data_i16(&samples).is_ok()
                        && let Some(dump) = self.audio_dump.as_mut()
                    {
                        dump.write_samples(&samples);
                    }
                }
            }
        }
        let mut src_size = self.audio_stream.queued_bytes().unwrap_or(0) as u32;
        let audio_now = std::time::Instant::now();
        if src_size != self.audio_last_queued {
            self.audio_last_queued = src_size;
            self.audio_last_change = audio_now;
        } else if self.pacing.audio_watchdog_ms != 0
            && self.audio_started
            && audio_chunks_this_iter == 0
            && src_size > self.pacing.catchup_floor
            && self.audio_last_change.elapsed()
                >= std::time::Duration::from_millis(self.pacing.audio_watchdog_ms)
        {
            eprintln!(
                "SDL audio queue watchdog: queued_bytes stuck at {src_size}; clearing host stream"
            );
            let _ = self.audio_stream.pause();
            let _ = self.audio_stream.clear();
            self.audio_started = false;
            self.shared.audio_mirror.store(0, Ordering::Relaxed);
            // Symmetry with the emu-thread + reset paths.
            self.shared.audio_inflight.store(0, Ordering::Relaxed);
            src_size = 0;
            self.audio_last_queued = 0;
            self.audio_last_change = audio_now;
        }
        self.shared.audio_mirror.store(src_size, Ordering::Relaxed);
        if !self.audio_started && src_size >= target {
            let _ = self.audio_stream.resume();
            self.audio_started = true;
        }
        if self.audio_queue_log && self.audio_last_log.elapsed().as_secs() >= 1 {
            eprintln!(
                "SDLAUDIO queued={src_size} inflight={} target={target} started={} drop={} chunks={audio_chunks_this_iter}",
                self.shared.audio_inflight.load(Ordering::Relaxed),
                self.audio_started,
                self.audio_drop_until_reset,
            );
            self.audio_last_log = std::time::Instant::now();
        }
    }

    /// Apply window-affecting OSD actions here (the canvas is not Send); Quit
    /// comes back the same way. Returns `true` on Quit.
    fn apply_ui_messages(&mut self) -> bool {
        use saturn::vdp2::{FRAME_HEIGHT, FRAME_WIDTH};
        let mut quit = false;
        while let Ok(m) = self.ui_rx.try_recv() {
            match m {
                UiMsg::Scale(sc) => {
                    let _ = self.window_mut().set_size(
                        FRAME_WIDTH as u32 * sc as u32,
                        FRAME_HEIGHT as u32 * sc as u32,
                    );
                    // The scale grid is 320x224 (10:7); snap to the 4:3 lock.
                    if !self.fullscreen {
                        snap_window_to_4_3(self.window_mut());
                    }
                }
                UiMsg::Fullscreen(on) => {
                    let _ = self.window_mut().set_fullscreen(on);
                    self.fullscreen = on;
                    // Returning to windowed re-asserts the 4:3 lock.
                    if !on {
                        snap_window_to_4_3(self.window_mut());
                    }
                }
                UiMsg::Scaling(on) => {
                    self.sharp = on;
                    // Renderer: set the texture filter now. GPU: read `sharp`
                    // per-frame in present (its blit picks the filter).
                    if let Some(t) = self.texture.as_mut() {
                        t.set_scale_mode(present::scale_mode(on));
                    }
                }
                UiMsg::Aspect(keep) => {
                    self.keep_aspect = keep;
                    // Renderer: re-apply logical presentation. GPU: read
                    // `keep_aspect` per-frame in present (its blit dst rect).
                    if let Some(c) = self.canvas.as_mut() {
                        let (w, h) = self.cur_dims;
                        present::apply_aspect(c, w as u32, h as u32, keep);
                    }
                }
                UiMsg::ArmRebind(b) => self.rebind_target = Some(b),
                UiMsg::ResetKeymap => {
                    self.keymap = std::array::from_fn(|i| {
                        sdl3::keyboard::Scancode::from_name(config::DEFAULT_KEYS[i])
                            .expect("default scancode name")
                    });
                }
                UiMsg::ArmGamepadRebind(b) => self.gpad_rebind_target = Some(b),
                UiMsg::ResetGamepadMap => {
                    self.gpad_map = config::DEFAULT_GPAD.map(str::to_string);
                }
                UiMsg::SetMouse(port) => {
                    // Off (None) releases any active pointer grab next frame
                    // (the grab loop is gated on `mouse_port.is_some()`).
                    self.mouse_port = port;
                }
                #[cfg(feature = "gpu-presenter")]
                UiMsg::SetShader(crt) => {
                    // Apply to the live GPU backend; a no-op under the renderer.
                    if let Some(g) = self.gpu.as_mut() {
                        g.set_shader(crt);
                    }
                }
                UiMsg::Quit => quit = true,
            }
        }
        quit
    }

    /// Take the newest frame (recycling any it superseded), upload, and present
    /// at the display rate. Renderer failures skip the frame (logged once),
    /// never abort — an `expect` here would kill the process before the
    /// battery saves were written.
    fn present(&mut self) {
        let mut newest = None;
        while let Ok(f) = self.frame_rx.try_recv() {
            if let Some((old, _)) = newest.replace(f) {
                let _ = self.recycle_tx.send(old);
            }
        }
        if let Some((buf, dims)) = newest {
            let old = std::mem::replace(&mut self.framebuffer, buf);
            let _ = self.recycle_tx.send(old);
            if dims != self.cur_dims {
                self.cur_dims = dims;
                // GPU: `GpuPresenter::present` recreates its own frame texture
                // on a dims change, so only the renderer needs work.
                if let Some(c) = self.canvas.as_mut() {
                    // Logical size tracks the new frame dims (keeps the aspect).
                    present::apply_aspect(c, dims.0 as u32, dims.1 as u32, self.keep_aspect);
                }
            }
        }
        // Renderer: (re)create the streaming texture whenever it doesn't match
        // the frame — retried each frame until it succeeds.
        if self.tex_dims != self.cur_dims
            && let Some(cr) = self.creator
        {
            let (w, h) = self.cur_dims;
            match cr.create_texture_streaming(
                sdl3::pixels::PixelFormat::ABGR8888,
                w as u32,
                h as u32,
            ) {
                Ok(mut t) => {
                    // A fresh texture resets to SDL3's linear default — re-apply.
                    t.set_scale_mode(present::scale_mode(self.sharp));
                    self.texture = Some(t);
                    self.tex_dims = self.cur_dims;
                    self.present_errs.recreate = false;
                }
                Err(e) => log_once(&mut self.present_errs.recreate, || {
                    format!(
                        "render: streaming texture {w}x{h} create failed ({e}); skipping frames"
                    )
                }),
            }
        }
        let t = std::time::Instant::now();
        let w = self.cur_dims.0;
        if self.tex_dims == self.cur_dims
            && let (Some(c), Some(tex)) = (self.canvas.as_mut(), self.texture.as_mut())
        {
            match tex.update(None, &self.framebuffer, w * 4) {
                Err(e) => log_once(&mut self.present_errs.upload, || {
                    format!("render: framebuffer upload failed ({e}); skipping frames")
                }),
                Ok(()) => {
                    c.clear();
                    match c.copy(&*tex, None, None) {
                        Err(e) => log_once(&mut self.present_errs.blit, || {
                            format!("render: blit failed ({e}); skipping frames")
                        }),
                        Ok(()) => {
                            c.present(); // audio-paced (see the emu burst / idle sleep)
                        }
                    }
                }
            }
        }
        #[cfg(feature = "gpu-presenter")]
        if let Some(g) = self.gpu.as_mut() {
            // GPU backend: upload + blit to the swapchain (OSD already
            // composited into `framebuffer`). Reads `sharp`/`keep_aspect` live.
            g.present(
                &self.framebuffer,
                (self.cur_dims.0 as u32, self.cur_dims.1 as u32),
                self.sharp,
                self.keep_aspect,
            );
        }
        self.pl_present += t.elapsed();
        self.pl_iters += 1;
        if self.pacing.perflog && self.pl_last.elapsed().as_secs() >= 1 && self.pl_iters > 0 {
            eprintln!(
                "MAIN iters/s={} | present avg {:.2} ms | queue={}ms",
                self.pl_iters,
                self.pl_present.as_secs_f64() * 1e3 / self.pl_iters as f64,
                (self.audio_stream.queued_bytes().unwrap_or(0) as u64 / 176) as u32,
            );
            self.pl_present = std::time::Duration::ZERO;
            self.pl_iters = 0;
            self.pl_last = std::time::Instant::now();
        }
    }
}

/// Messages from the SDL main thread to the emulation thread.
#[cfg(feature = "sdl-frontend")]
enum EmuIn {
    /// This iteration's per-device input: the keyboard pad mask plus one
    /// `(GUID, digital mask, analog [X,Y,L,R])` entry per connected game
    /// controller. The emu thread routes these to ports 1/2 through the
    /// `ports::Ports` assignment (digital buttons via `route`, the 3D-pad analog
    /// stick/trigger channels via `route_analog`).
    Input {
        kbd: u16,
        pads: Vec<(String, u16, [u8; 4])>,
    },
    /// The connected-controller list changed (hot-plug): GUID + display name per
    /// open pad. The emu thread re-resolves its port assignment against it.
    PadList(Vec<ports::Pad>),
    /// OSD menu navigation (only sent while the menu is open).
    Nav(osd::Nav),
    /// Toggle the OSD menu.
    Toggle,
    /// F5/F9 quickslot save/load.
    Quicksave,
    Quickload,
    /// F11 / F12 display hotkeys: toggle fullscreen, or the fullscreen aspect
    /// mode (Keep ratio ↔ Fit screen). Routed through the same OSD dispatch as
    /// the menu rows so the Settings labels + persisted config stay in sync.
    ToggleFullscreen,
    ToggleAspect,
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
    /// A gamepad-button rebind finished on the SDL thread: the pad-button index
    /// and the captured SDL gamepad token (`None` = cancelled with Esc).
    GamepadBindResult(u8, Option<String>),
}

/// Ordered audio-control stream from emulation to SDL. A machine timeline reset
/// must clear host-queued samples before later chunks from the new timeline play.
#[cfg(feature = "sdl-frontend")]
enum AudioMsg {
    /// `bytes` = `samples.len() * 2`, carried so the receiver subtracts the
    /// `audio_inflight` reserve without recomputing and keeps the add/sub pairing
    /// explicit (the emu thread `fetch_add`s the same value when it sends).
    Chunk {
        samples: Vec<i16>,
        bytes: u32,
    },
    Reset,
}

/// Messages from the emulation thread back to the SDL main thread —
/// window-affecting OSD actions (the canvas is not Send) and quit.
#[cfg(feature = "sdl-frontend")]
enum UiMsg {
    Scale(u8),
    Fullscreen(bool),
    /// Set texture scaling Sharp (nearest, `true`) vs Smooth (linear, `false`);
    /// applied live to the streaming texture on the SDL thread.
    Scaling(bool),
    /// Set fullscreen aspect Keep-ratio (letterbox, `true`) vs Fit-screen
    /// (stretch, `false`); applied live via SDL3 logical presentation.
    Aspect(bool),
    /// Capture the next host keypress for this pad button (OSD rebind).
    ArmRebind(u8),
    /// Restore the default keyboard→pad bindings.
    ResetKeymap,
    /// Capture the next gamepad button/trigger for this pad button (OSD rebind).
    ArmGamepadRebind(u8),
    /// Restore the default gamepad→pad bindings.
    ResetGamepadMap,
    /// Move the Shuttle Mouse (or remove it): updates the SDL thread's capture
    /// gate so motion/clicks are fed only while a mouse port is active.
    SetMouse(Option<u8>),
    /// Select the CRT shader (`true`) vs the plain blit (`false`) on the SDL_GPU
    /// backend, applied live. Preview-only (the `gpu-presenter` Shaders chooser).
    #[cfg(feature = "gpu-presenter")]
    SetShader(bool),
    Quit,
}

/// Mutable frontend display/config state the Settings screens read (for their
/// active-item marks) and write (when the user changes a setting).
#[cfg(feature = "sdl-frontend")]
struct UiState {
    scale: u8,
    fullscreen: bool,
    /// Sharp (nearest) vs Smooth (linear) texture scaling — mirrors
    /// `cfg.scaling` for the Graphics screen's label + toggle.
    sharp: bool,
    /// Keep-ratio (letterbox) vs fit-screen (stretch) — mirrors `cfg.aspect`
    /// for the Graphics screen's label + toggle.
    keep_aspect: bool,
    region: osd::OsdRegion,
    cart: osd::OsdCart,
    backend: osd::OsdBackend,
    /// Whether the CRT shader is selected — mirrors `cfg.shader` for the OSD
    /// Shaders chooser. `gpu-presenter`-only (the only build with a shader path).
    #[cfg(feature = "gpu-presenter")]
    shader_crt: bool,
}

/// Everything the emu thread's OSD dispatcher owns besides the machine: the
/// Settings mirrors, the persisted config, the launch disc spec, and the
/// save-file bases. The `.bup` battery keys to the BIOS (`save_base`, a shared
/// console resource); save states key to the loaded disc image (`state_base`,
/// per-game).
#[cfg(feature = "sdl-frontend")]
struct Session {
    save_base: std::path::PathBuf,
    launched_spec: Option<String>,
    ui: UiState,
    cfg: config::Config,
    /// The swappable BIOS images beside the launched one (paths + display
    /// stems, index-matched) and which one is running. A swap re-keys
    /// `save_base` (and thus the `.bup`) to the new image; save states follow
    /// the disc via `launched_spec`, not the BIOS.
    bios_paths: Vec<std::path::PathBuf>,
    bios_names: Vec<String>,
    bios_active: usize,
    /// The live per-port input assignment (the source of truth) and the
    /// connected-controller list its gamepad GUIDs resolve against. The
    /// *persisted* intent is `cfg.port1`/`port2`; this is re-derived from those
    /// tokens whenever the device list changes, and re-applied across a power
    /// cycle / state load (`apply_ports`).
    ports: ports::Ports,
    pad_list: Vec<ports::Pad>,
    /// The disc browser's current directory and its (cached) listing — rebuilt
    /// only as the user navigates, not every frame.
    browse_dir: std::path::PathBuf,
    browse_entries: Vec<osd::BrowseEntry>,
    /// Last self-diagnostics results (Settings → Diagnostics → "Run all").
    diag_results: Vec<saturn::diagnostics::DiagOutcome>,
    /// Change-tracking writer for the `.bup` / `.crtbup` battery files.
    battery: battery::BatteryWriter,
}

/// Disc-image extensions the browser offers (lower-cased compare).
#[cfg(feature = "sdl-frontend")]
const DISC_EXTS: &[&str] = &["cue", "iso", "ccd"];

#[cfg(feature = "sdl-frontend")]
impl Session {
    /// Base path for the per-game save-state siblings (`.state` / `.<n>.state`).
    /// Unlike the `.bup` battery — a shared console resource keyed to the BIOS
    /// (`save_base`) — a save state belongs to a specific game, so it keys to the
    /// loaded disc IMAGE. A live `cdrom:` drive has no image path and a no-disc
    /// boot has no game, so both fall back to the BIOS base.
    fn state_base(&self) -> std::path::PathBuf {
        state_base_for(self.launched_spec.as_deref(), &self.save_base)
    }
    fn slot_path(&self, n: u8) -> std::path::PathBuf {
        self.state_base().with_extension(format!("{n}.state"))
    }
    fn state_path(&self) -> std::path::PathBuf {
        self.state_base().with_extension("state")
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
            entries.push(osd::BrowseEntry {
                name: "..".into(),
                is_dir: true,
            });
        }
        entries.extend(
            dirs.into_iter()
                .map(|name| osd::BrowseEntry { name, is_dir: true }),
        );
        entries.extend(files.into_iter().map(|name| osd::BrowseEntry {
            name,
            is_dir: false,
        }));
        self.browse_entries = entries;
    }
}

#[cfg(feature = "sdl-frontend")]
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
#[cfg(feature = "sdl-frontend")]
fn osd_region_to_token(r: osd::OsdRegion) -> &'static str {
    match r {
        osd::OsdRegion::Japan => "japan",
        osd::OsdRegion::NorthAmerica => "north-america",
        osd::OsdRegion::EuropePal => "europe-pal",
        osd::OsdRegion::AsiaNtsc => "asia-ntsc",
    }
}

/// The config-file token for a menu cartridge (same vocabulary as `--cart=`).
#[cfg(feature = "sdl-frontend")]
fn osd_cart_to_token(c: osd::OsdCart) -> &'static str {
    match c {
        osd::OsdCart::None => "none",
        osd::OsdCart::ExtRam1M => "ram1m",
        osd::OsdCart::ExtRam4M => "ram4m",
        osd::OsdCart::BackupRam => "bram",
    }
}

/// The config token for a menu backend (a subset of the `--backend` vocabulary;
/// `Direct3D` persists as `direct3d11`). Inverse of [`token_to_osd_backend`].
#[cfg(feature = "sdl-frontend")]
fn osd_backend_to_token(b: osd::OsdBackend) -> &'static str {
    match b {
        osd::OsdBackend::Auto => "auto",
        osd::OsdBackend::OpenGl => "opengl",
        osd::OsdBackend::Direct3D => "direct3d11",
        osd::OsdBackend::Metal => "metal",
        osd::OsdBackend::Software => "software",
    }
}

/// Map a config/CLI backend token to the menu's backend enum, folding the full
/// [`present::RenderBackend`] vocabulary (OpenGL ES, D3D11/12) onto the OSD's
/// subset so the Graphics screen always has a row to show.
#[cfg(feature = "sdl-frontend")]
fn token_to_osd_backend(tok: &str) -> osd::OsdBackend {
    use present::RenderBackend as Rb;
    match Rb::from_token(tok) {
        Rb::Auto => osd::OsdBackend::Auto,
        Rb::OpenGl | Rb::OpenGlEs => osd::OsdBackend::OpenGl,
        Rb::Direct3D11 | Rb::Direct3D12 => osd::OsdBackend::Direct3D,
        Rb::Metal => osd::OsdBackend::Metal,
        Rb::Software => osd::OsdBackend::Software,
    }
}

#[cfg(feature = "sdl-frontend")]
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
#[cfg(feature = "sdl-frontend")]
fn cartridge_to_osd(c: &Cartridge) -> osd::OsdCart {
    match c {
        Cartridge::Dram { id, .. } if *id == 0x5C => osd::OsdCart::ExtRam4M,
        Cartridge::Dram { .. } => osd::OsdCart::ExtRam1M,
        Cartridge::Bram { .. } => osd::OsdCart::BackupRam,
        Cartridge::None | Cartridge::Rom { .. } => osd::OsdCart::None,
    }
}

#[cfg(feature = "sdl-frontend")]
fn osd_cart_to_cartridge(c: osd::OsdCart) -> Cartridge {
    match c {
        osd::OsdCart::None => Cartridge::None,
        osd::OsdCart::ExtRam1M => Cartridge::ext_ram_1mb(),
        osd::OsdCart::ExtRam4M => Cartridge::ext_ram_4mb(),
        // The 32 Mbit default, matching `--cart=bram`.
        osd::OsdCart::BackupRam => Cartridge::backup_ram(0x0040_0000),
    }
}

/// Apply the per-port input assignment to the live machine: set each SMPC port's
/// device from its [`ports::Source`], and tell the SDL thread which port (if any)
/// now carries the mouse so its pointer-capture gate follows. A peripheral swap,
/// not a reset — the game re-reads its devices on the next INTBACK poll.
#[cfg(feature = "sdl-frontend")]
fn apply_ports(
    saturn: &mut saturn::Saturn,
    ports: &ports::Ports,
    ui_tx: &std::sync::mpsc::Sender<UiMsg>,
) {
    saturn.set_port_devices(ports.source[0].device(), ports.source[1].device());
    let _ = ui_tx.send(UiMsg::SetMouse(ports.mouse_port()));
}

/// Is the game-controller input named by `token` currently pressed on `c`? A
/// token is an SDL gamepad button name (`a`/`b`/`x`/`y`/`dpup`/`leftshoulder`/…)
/// or one of the two analog triggers (`lefttrigger`/`righttrigger`, thresholded
/// to a digital press). An unknown token is never pressed.
#[cfg(feature = "sdl-frontend")]
fn gpad_pressed(c: &sdl3::gamepad::Gamepad, token: &str) -> bool {
    use sdl3::gamepad::{Axis, Button};
    const TRIG_TH: i16 = 16384; // half deflection
    if let Some(btn) = Button::from_string(token) {
        c.button(btn)
    } else if let Some(ax) = Axis::from_string(token) {
        // Only the triggers make sense as a digital binding.
        matches!(ax, Axis::TriggerLeft | Axis::TriggerRight) && c.axis(ax) > TRIG_TH
    } else {
        false
    }
}

/// Snapshot the connected controllers as [`ports::Pad`] (stable GUID + display
/// name) for the emu thread's port assignment and the OSD labels.
#[cfg(feature = "sdl-frontend")]
fn pad_list_of(
    controllers: &[sdl3::gamepad::Gamepad],
    subsys: &sdl3::GamepadSubsystem,
) -> Vec<ports::Pad> {
    controllers
        .iter()
        .filter_map(|c| {
            let id = c.id().ok()?;
            Some(ports::Pad {
                guid: subsys.guid_for_id(id).string(),
                name: c.name().unwrap_or_default(),
            })
        })
        .collect()
}

/// Carry out a menu action against the running machine. Returns `true` if the
/// emulator should quit. Save-state slots live at `<bios>.<n>.state`.
#[cfg(feature = "sdl-frontend")]
fn dispatch_osd(
    action: osd::OsdAction,
    osd: &mut osd::Osd,
    saturn: &mut saturn::Saturn,
    sess: &mut Session,
    ui_tx: &std::sync::mpsc::Sender<UiMsg>,
    audio_tx: &std::sync::mpsc::Sender<AudioMsg>,
) -> bool {
    use osd::OsdAction;
    match action {
        OsdAction::Resume => osd.close(),
        OsdAction::Quit => return true,
        OsdAction::Reset => {
            saturn.reset();
            let _ = audio_tx.send(AudioMsg::Reset);
            osd.set_toast("Reset", 120);
            osd.close();
        }
        OsdAction::Save(n) => match write_atomic(&sess.slot_path(n), &saturn.save_state()) {
            Ok(()) => osd.set_toast(format!("Saved slot {n}"), 120),
            Err(e) => osd.set_toast(format!("Save failed: {e}"), 180),
        },
        OsdAction::Load(n) => {
            let path = sess.slot_path(n);
            match fs::read(&path) {
                Ok(bytes) => match saturn.load_state(&bytes) {
                    Ok(()) => {
                        apply_ports(saturn, &sess.ports, ui_tx);
                        let _ = audio_tx.send(AudioMsg::Reset);
                        eprintln!("loaded slot {n}: {}", path.display());
                        osd.set_toast(format!("Loaded slot {n}"), 120);
                        osd.close();
                    }
                    Err(e) => {
                        eprintln!("load slot {n} {} failed: {e}", path.display());
                        osd.set_toast(format!("Load failed: {e}"), 180)
                    }
                },
                Err(e) => {
                    eprintln!("slot {n} empty at {} ({e})", path.display());
                    osd.set_toast(format!("Slot {n} empty"), 120)
                }
            }
        }
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
                            let _ = audio_tx.send(AudioMsg::Reset);
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
                if sess.ui.fullscreen {
                    "Fullscreen on"
                } else {
                    "Fullscreen off"
                },
                90,
            );
        }
        OsdAction::ToggleScaling => {
            sess.ui.sharp = !sess.ui.sharp;
            let _ = ui_tx.send(UiMsg::Scaling(sess.ui.sharp));
            sess.cfg.scaling = if sess.ui.sharp { "sharp" } else { "smooth" }.to_string();
            sess.cfg.save();
            osd.set_toast(
                if sess.ui.sharp {
                    "Pixels: Sharp"
                } else {
                    "Pixels: Smooth"
                },
                90,
            );
        }
        OsdAction::ToggleAspect => {
            sess.ui.keep_aspect = !sess.ui.keep_aspect;
            let _ = ui_tx.send(UiMsg::Aspect(sess.ui.keep_aspect));
            sess.cfg.aspect = if sess.ui.keep_aspect {
                "keep"
            } else {
                "stretch"
            }
            .to_string();
            sess.cfg.save();
            osd.set_toast(
                if sess.ui.keep_aspect {
                    "Aspect: Keep ratio"
                } else {
                    "Aspect: Fit screen"
                },
                90,
            );
        }
        #[cfg(feature = "gpu-presenter")]
        OsdAction::SetShader(crt) => {
            // The SDL thread applies it to the live GpuPresenter (a no-op under the
            // SDL_Renderer backend, which has no shader path — hence the toast hint).
            sess.ui.shader_crt = crt;
            let _ = ui_tx.send(UiMsg::SetShader(crt));
            sess.cfg.shader = if crt { "crt" } else { "none" }.to_string();
            sess.cfg.save();
            osd.set_toast(
                if crt {
                    "Shader: CRT (SDL_GPU only)"
                } else {
                    "Shader: None"
                },
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
            let _ = audio_tx.send(AudioMsg::Reset);
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
        OsdAction::SetBackend(b) => {
            // The SDL3 render driver is chosen when the window/canvas is built,
            // so a change can't take effect live — persist it and tell the user
            // it applies on the next launch.
            let tok = osd_backend_to_token(b);
            sess.ui.backend = b;
            sess.cfg.backend = tok.to_string();
            sess.cfg.save();
            osd.set_toast(format!("Renderer: {tok} (restart to apply)"), 180);
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
                    persist_session_batteries(saturn, sess, true);
                    *saturn = saturn::Saturn::new(bytes);
                    saturn.reset();
                    saturn.set_region(osd_region_to_code(sess.ui.region));
                    apply_ports(saturn, &sess.ports, ui_tx);
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
                    let bup = battery_path(&sess.save_base);
                    if let Ok(b) = fs::read(&bup) {
                        saturn.load_internal_backup(&b);
                        sess.battery.note_bup(&bup, saturn.internal_backup());
                    }
                    load_backup_card(saturn, sess, sess.cfg.cart_card);
                    let _ = audio_tx.send(AudioMsg::Reset);
                    osd.set_toast(format!("BIOS: {} (power cycle)", sess.bios_names[i]), 150);
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
        OsdAction::StartGamepadRebind(b) => {
            // Modal like StartRebind, but the SDL thread captures the next
            // controller button/trigger and answers with GamepadBindResult.
            osd.begin_capture(b);
            let _ = ui_tx.send(UiMsg::ArmGamepadRebind(b));
        }
        OsdAction::ResetGamepadBinds => {
            sess.cfg.gpad = config::DEFAULT_GPAD.map(str::to_string);
            sess.cfg.save();
            let _ = ui_tx.send(UiMsg::ResetGamepadMap);
            osd.set_toast("Default gamepad binds restored", 120);
        }
        OsdAction::SetCartridge(k) => {
            // Persist the outgoing backup cartridge (if any) before it's swapped
            // out, so switching carts doesn't lose its saves.
            persist_active_backup_card(saturn, sess);
            sess.ui.cart = k;
            saturn.reset();
            saturn.set_region(osd_region_to_code(sess.ui.region));
            saturn.insert_cartridge(osd_cart_to_cartridge(k));
            // Adopt the active card's persisted contents, if present.
            load_backup_card(saturn, sess, sess.cfg.cart_card);
            let _ = audio_tx.send(AudioMsg::Reset);
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
        OsdAction::SelectBackupCard(n) => {
            let n = n.min(config::BACKUP_CARDS as u8 - 1);
            // Persist the outgoing card, switch, ensure the Backup RAM cart is
            // plugged, then load the selected card (ADR-0028).
            persist_active_backup_card(saturn, sess);
            sess.cfg.cart_card = n;
            ensure_backup_cart(saturn, sess, audio_tx);
            load_backup_card(saturn, sess, n);
            sess.cfg.save();
            osd.set_toast(format!("Backup Card {n} selected"), 150);
            osd.close();
        }
        OsdAction::CreateBackupCard(n) => {
            let n = n.min(config::BACKUP_CARDS as u8 - 1);
            persist_active_backup_card(saturn, sess);
            // Write a fresh, formatted card image to the slot's file.
            let fresh = osd_cart_to_cartridge(osd::OsdCart::BackupRam);
            if let Some(b) = fresh.bram_bytes()
                && let Err(e) = write_atomic(&cart_card_path(&sess.save_base, n), b)
            {
                eprintln!("failed to create backup card {n}: {e}");
            }
            sess.cfg.cart_card = n;
            ensure_backup_cart(saturn, sess, audio_tx);
            // Load the just-created (fresh) card so the live cart matches its file.
            load_backup_card(saturn, sess, n);
            sess.cfg.save();
            osd.set_toast(format!("Backup Card {n} created"), 150);
            osd.close();
        }
        OsdAction::DeleteBackupCard(n) => {
            let n = n.min(config::BACKUP_CARDS as u8 - 1);
            let path = cart_card_path(&sess.save_base, n);
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("failed to delete backup card {n}: {e}");
            }
            // Deleting the active, currently-plugged card ejects the cart — no
            // active-but-fileless state, and we don't re-save the deleted card.
            if n == sess.cfg.cart_card && saturn.cartridge_backup().is_some() {
                sess.ui.cart = osd::OsdCart::None;
                sess.cfg.cartridge = "none".into();
                saturn.reset();
                saturn.set_region(osd_region_to_code(sess.ui.region));
                saturn.insert_cartridge(osd_cart_to_cartridge(osd::OsdCart::None));
                let _ = audio_tx.send(AudioMsg::Reset);
            }
            sess.cfg.save();
            osd.set_toast(format!("Backup Card {n} deleted"), 150);
            osd.close();
        }
        OsdAction::CyclePort(p) => {
            // Advance this port to its next source against the live device list
            // (skipping whatever the other port already uses), persist the new
            // token, and re-point the SMPC ports + the SDL mouse-capture gate.
            let p = (p as usize) & 1;
            sess.ports.cycle(p, &sess.pad_list);
            let token = sess.ports.source[p].to_token();
            if p == 0 {
                sess.cfg.port1 = token;
            } else {
                sess.cfg.port2 = token;
            }
            sess.cfg.save();
            apply_ports(saturn, &sess.ports, ui_tx);
            osd.set_toast(
                format!("Port {}: {}", p + 1, sess.ports.source[p].label()),
                120,
            );
        }
    }
    false
}

#[cfg(not(feature = "sdl-frontend"))]
fn run(
    bios: Vec<u8>,
    disc_spec: Option<String>,
    cart: Cartridge,
    save_base: std::path::PathBuf,
    region: u8,
    mouse_port: Option<u8>,
    // The config already shaped `cart`/`region` in `main`; headless has no
    // window or keymap, so only the active backup card (`cart_card`) still
    // applies to the `.crtbup` path.
    cfg: config::Config,
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
    // the pad. Headless has no OSD/`ports` module, so apply the device layout
    // inline (the same mapping `ports::Source::device()` would produce).
    if let Some(n) = mouse_port {
        use saturn::smpc::PortDevice::{Mouse, None as NoDev, Pad};
        let (p1, p2) = if n == 1 { (Mouse, NoDev) } else { (Pad, Mouse) };
        saturn.set_port_devices(p1, p2);
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
    let bup_path = battery_path(&save_base);
    if let Ok(bytes) = fs::read(&bup_path) {
        saturn.load_internal_backup(&bytes);
    }
    let cart_battery_path = cart_card_path(&save_base, cfg.cart_card);
    if saturn.cartridge_backup().is_some()
        && let Ok(bytes) = fs::read(&cart_battery_path)
    {
        saturn.load_cartridge_backup(&bytes);
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
    let cd_cmd_log = std::env::var_os("SAT_CD_CMD_LOG").is_some();
    if cd_cmd_log {
        saturn.bus.cd_block.cmd_log_on = true;
        saturn.bus.cd_block.hirq_log_on = true;
    }
    let ftcsr_log = std::env::var("SAT_FTCSR_LOG").ok();
    if let Some(mode) = ftcsr_log.as_deref() {
        let after = std::env::var("SAT_FTCSR_AFTER_CYC")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if matches!(mode, "1" | "master" | "all") {
            saturn.master_mut().dbg_ftcsr = true;
            saturn.master_mut().dbg_ftcsr_after = after;
            saturn.master_mut().dbg_ftcsr_log.clear();
        }
        if matches!(mode, "slave" | "all") {
            saturn.slave_mut().dbg_ftcsr = true;
            saturn.slave_mut().dbg_ftcsr_after = after;
            saturn.slave_mut().dbg_ftcsr_log.clear();
        }
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
        let after_cycle = std::env::var("SAT_FBP_AFTER_CYC")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
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
                let matched =
                    h.cycle >= after_cycle && rreg.is_none_or(|r| (rlo..rhi).contains(&h.regs[r]));
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
                let (r, pr, gbr, cycle, code) = (h.regs, h.pr, h.gbr, h.cycle, &h.code);
                eprintln!("FBP {bp:08X} hit. PR={pr:08X} GBR={gbr:08X} cycle={cycle} regs:");
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
    let movie_probe: Option<u32> = std::env::var("SAT_MOVIE_PROBE")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| std::env::var_os("SAT_MOVIE_PROBE").map(|_| 30));
    let scsp_movie_probe = std::env::var_os("SAT_SCSP_MOVIE_PROBE").is_some();
    // Debug: `SAT_CACHE_PURGE=1` purges both SH-2 I-caches each frame, to test
    // whether a stale-cache fetch is the blocker (if a game runs past a spurious
    // illegal-instruction fault only with this on, the cache is incoherent).
    let cache_purge = std::env::var_os("SAT_CACHE_PURGE").is_some();
    // Debug: `SAT_SLOW_FETCH=N` charges N extra stall cycles per instruction-fetch
    // cache hit on both SH-2s — a timing-probe to test inter-CPU-race hypotheses
    // (changes timing only, no cache value/content change). 0 = off.
    let slow_fetch: u32 = std::env::var("SAT_SLOW_FETCH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // Debug: `SAT_REGWATCH=idx:hexval[:after_cyc]` logs (on the master) every PC
    // where R[idx] transitions to the value — finds where a value is computed
    // into a register, beating the bp-stack / line-granular-read-watch confounds.
    // The optional after_cyc keeps the run full-speed until that cycle (so a long
    // run reaching a late window stays fast). e.g. SAT_REGWATCH=12:B1:470000000.
    if let Ok(spec) = std::env::var("SAT_REGWATCH") {
        let p: Vec<&str> = spec.split(':').collect();
        if let (Some(i), Some(v)) = (p.first(), p.get(1))
            && let Ok(idx) = i.trim().parse::<u8>()
            && let Ok(val) = u32::from_str_radix(v.trim().trim_start_matches("0x"), 16)
        {
            let after = p
                .get(2)
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let m = saturn.master_mut();
            m.dbg_regwatch = Some((idx, val));
            m.dbg_regwatch_after = after;
        }
    }
    let mut last_pc = u32::MAX;
    let mut dump_dims = (FRAME_WIDTH, FRAME_HEIGHT);
    let mut audio_dump = WavDump::from_env("SAT_AUDIO_DUMP");
    // Observer-only: `SAT_SRAM_DUMP_FRAME=N SAT_SRAM_DUMP_ADDR=0xOFF [SAT_SRAM_DUMP_LEN=0x40]`
    // hex-dumps a window of SCSP sound RAM (byte offset within the 512 KiB) at the
    // end of the given frame, interpreting the words as i16 PCM. Read-only.
    let sram_dump_frame: Option<u32> = std::env::var("SAT_SRAM_DUMP_FRAME")
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let sram_dump_addr: u32 = std::env::var("SAT_SRAM_DUMP_ADDR")
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x7C000);
    let sram_dump_len: u32 = std::env::var("SAT_SRAM_DUMP_LEN")
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x40);
    // Observer-only: `SAT_SAVESTATE_AT_FRAME=N SAT_SAVESTATE_OUT=path` writes a
    // faithful (per-frame) save state at end of frame N — a foothold sdbg can
    // load with the disc grafted, since sdbg's single `run_for` diverges.
    let savestate_at_frame: Option<u32> = std::env::var("SAT_SAVESTATE_AT_FRAME")
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let savestate_out = std::env::var("SAT_SAVESTATE_OUT").ok();
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
        if savestate_at_frame == Some(f)
            && let Some(path) = savestate_out.as_deref()
        {
            match fs::write(path, saturn.save_state()) {
                Ok(()) => eprintln!("SAVESTATE wrote {path} at frame {f}"),
                Err(e) => eprintln!("SAVESTATE write {path} failed: {e}"),
            }
        }
        let mut frame_audio = if audio_dump.is_some() {
            saturn.take_audio()
        } else {
            Vec::new()
        };
        if let Some(dump) = audio_dump.as_mut() {
            dump.write_samples(&frame_audio);
        }
        if let Some(period) = movie_probe
            && (period == 0 || f % period == 0)
        {
            let (cd_status, cd_fad, cd_left, cd_free, parts) = saturn.bus.cd_block.debug_state();
            let part_summary = parts
                .iter()
                .enumerate()
                .filter(|&(_, &n)| n != 0)
                .map(|(i, n)| format!("{i}:{n}"))
                .collect::<Vec<_>>()
                .join(",");
            let (plots, cmds, px, dur) = saturn.bus.vdp1.dbg_take_frame();
            let vdp1_front = saturn
                .bus
                .vdp1
                .display_fb()
                .as_slice()
                .as_chunks::<2>()
                .0
                .iter()
                .filter(|p| p[0] != 0 || p[1] != 0)
                .count();
            let vdp1_draw = saturn
                .bus
                .vdp1
                .fb
                .as_slice()
                .as_chunks::<2>()
                .0
                .iter()
                .filter(|p| p[0] != 0 || p[1] != 0)
                .count();
            let out_nonblack = framebuffer
                .as_chunks::<4>()
                .0
                .iter()
                .take(dump_dims.0 * dump_dims.1)
                .filter(|p| p[0] != 0 || p[1] != 0 || p[2] != 0)
                .count();
            let audio = if audio_dump.is_some() {
                std::mem::take(&mut frame_audio)
            } else {
                saturn.take_audio()
            };
            let audio_abs: i64 = audio.iter().map(|&x| (x as i64).abs()).sum();
            let scsp_probe = scsp_probe_string(&saturn.bus.scsp, scsp_movie_probe);
            eprintln!(
                "MOVIE f={f:04} pc={:08X} cd_st={cd_status:02X} fad={cd_fad} left={cd_left} free={cd_free} parts=[{part_summary}] \
                 vdp2_disp={} tvmd={:04X} bgon={:04X} vdp1 plots={plots} cmds={cmds} px={px} dur={dur} front={vdp1_front} draw={vdp1_draw} out={out_nonblack} audio_abs={audio_abs}{scsp_probe}",
                saturn.master().regs.pc,
                saturn.bus.vdp2.regs.display_enabled(),
                saturn.bus.vdp2.regs.read16(0x000),
                saturn.bus.vdp2.regs.read16(0x020),
            );
        }
        if pctrace {
            let pc = saturn.master().regs.pc;
            if pc != last_pc {
                eprintln!("frame {f:4} master PC=0x{pc:08X}");
                last_pc = pc;
            }
        }
        if sram_dump_frame == Some(f) {
            let ram = saturn.bus.scsp.ram.as_slice();
            // Widen to usize up front: the env-supplied addr/len must not be able
            // to overflow u32 offset arithmetic (a debug-build panic).
            let addr = sram_dump_addr as usize;
            let mut nonzero = 0usize;
            // Stats over the full requested window (may be large; rows suppressed
            // when SAT_SRAM_DUMP_ROWS=0).
            let mut maxabs = 0i32;
            let mut maxdelta = 0i32;
            let mut prev = 0i32;
            let mut w = 0u32;
            while w < sram_dump_len / 2 {
                let base = addr + w as usize * 2;
                let b0 = *ram.get(base).unwrap_or(&0);
                let b1 = *ram.get(base + 1).unwrap_or(&0);
                let s = i16::from_be_bytes([b0, b1]) as i32;
                if s != 0 {
                    nonzero += 1;
                }
                maxabs = maxabs.max(s.abs());
                if w > 0 {
                    maxdelta = maxdelta.max((s - prev).abs());
                }
                prev = s;
                w += 1;
            }
            let show_rows = std::env::var("SAT_SRAM_DUMP_ROWS")
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(u32::MAX);
            eprintln!(
                "SRAM DUMP f={f} off=0x{sram_dump_addr:05X} len=0x{sram_dump_len:X}: maxabs={maxabs} max_sample_delta={maxdelta}"
            );
            let mut off = 0u32;
            while off < sram_dump_len && (off / 16) < show_rows {
                let base = addr + off as usize;
                let mut bytes = String::new();
                let mut words = String::new();
                for w in 0..8u32 {
                    let b0 = *ram.get(base + (w * 2) as usize).unwrap_or(&0);
                    let b1 = *ram.get(base + (w * 2 + 1) as usize).unwrap_or(&0);
                    // sound RAM is big-endian 16-bit
                    let s = i16::from_be_bytes([b0, b1]);
                    bytes.push_str(&format!("{b0:02X}{b1:02X} "));
                    words.push_str(&format!("{s:6} "));
                }
                eprintln!("  {:05X}: {bytes}| {words}", addr + off as usize);
                off += 16;
            }
            eprintln!("  (nonzero i16 words in window: {nonzero})");
        }
    }
    if std::env::var_os("SAT_REGWATCH").is_some() {
        let log = std::mem::take(&mut saturn.master_mut().dbg_regwatch_log);
        eprintln!("REGWATCH: {} transition(s) to target value:", log.len());
        for (pc, cyc) in &log {
            eprintln!("  pc={pc:08X}  cyc={cyc}");
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

    if cd_cmd_log {
        let name = |c: u8| -> &'static str {
            match c {
                0x00 => "GetStatus",
                0x01 => "GetHwInfo",
                0x02 => "GetToc",
                0x03 => "GetSession",
                0x04 => "Init",
                0x06 => "EndDataXfer",
                0x10 => "Play",
                0x11 => "Seek",
                0x12 => "Scan",
                0x20 => "GetSubcodeQ",
                0x30 => "SetDevConn",
                0x31 => "GetDevConn",
                0x32 => "GetLastBuf",
                0x40 => "SetFilterRange",
                0x42 => "SetFilterSubhdr",
                0x44 => "SetFilterMode",
                0x46 => "SetFilterConn",
                0x48 => "ResetSelector",
                0x50 => "GetBufSize",
                0x51 => "GetBufStat",
                0x52 => "CalcActualSize",
                0x53 => "GetActualDataSize",
                0x60 => "SetSectorLen",
                0x61 => "GetSectorData",
                0x62 => "DelSectorData",
                0x63 => "GetThenDel",
                0x64 => "PutSectorData",
                0x67 => "GetCopyError",
                0x70 => "ChangeDir",
                0x71 => "ReadDir",
                0x72 => "GetFileScope",
                0x73 => "GetFileInfo",
                0x74 => "ReadFile",
                0x75 => "AbortFile",
                0xE0 => "Auth",
                0xE1 => "GetDiscRegion",
                _ => "?",
            }
        };
        let tail = std::env::var("SAT_CD_CMD_TAIL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(80usize);
        let log = &saturn.bus.cd_block.cmd_log;
        eprintln!("CD command log tail {tail} of {}:", log.len());
        for (i, e) in log.iter().enumerate().skip(log.len().saturating_sub(tail)) {
            eprintln!(
                "  [{i:04}] @{:08X} {:02X} {:<14} in={:04X},{:04X},{:04X},{:04X} -> out={:04X},{:04X},{:04X},{:04X} HIRQ {:04X}->{:04X} st={:02X}",
                e.caller_pc,
                e.cmd,
                name(e.cmd),
                e.cr_in[0],
                e.cr_in[1],
                e.cr_in[2],
                e.cr_in[3],
                e.cr_out[0],
                e.cr_out[1],
                e.cr_out[2],
                e.cr_out[3],
                e.hirq_in,
                e.hirq_out,
                e.status,
            );
        }
        let hlog = &saturn.bus.cd_block.hirq_log;
        eprintln!("CD HIRQ log tail {tail} of {}:", hlog.len());
        for (i, (old, new, cause)) in hlog
            .iter()
            .enumerate()
            .skip(hlog.len().saturating_sub(tail))
        {
            eprintln!("  [{i:04}] {old:04X}->{new:04X} cause={cause:03X}");
        }
    }

    if let Err(e) = write_atomic(&bup_path, saturn.internal_backup()) {
        eprintln!(
            "failed to persist backup RAM to {}: {e}",
            bup_path.display()
        );
    }
    if let Some(bytes) = saturn.cartridge_backup()
        && let Err(e) = write_atomic(&cart_battery_path, bytes)
    {
        eprintln!(
            "failed to persist backup cartridge to {}: {e}",
            cart_battery_path.display()
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
        for px in framebuffer[..w * h * 4].as_chunks::<4>().0 {
            ppm.extend_from_slice(&px[..3]); // RGBA → RGB
        }
        match fs::write(&path, &ppm) {
            Ok(()) => eprintln!("wrote framebuffer to {path}"),
            Err(e) => eprintln!("failed to write framebuffer to {path}: {e}"),
        }
    }

    if ftcsr_log.is_some() {
        let tail = std::env::var("SAT_FTCSR_TAIL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120usize);
        let dump = |name: &str, log: &[(u32, u8, bool, u64)]| {
            eprintln!("FTCSR {name} tail {tail} of {}:", log.len());
            for (pc, val, is_write, cycle) in log.iter().skip(log.len().saturating_sub(tail)) {
                eprintln!(
                    "  {} pc={pc:08X} val={val:02X} ICF={} cyc={cycle}",
                    if *is_write { "WR" } else { "RD" },
                    u8::from(val & 0x80 != 0)
                );
            }
        };
        dump("master", &saturn.master().dbg_ftcsr_log);
        dump("slave", &saturn.slave().dbg_ftcsr_log);
        let pc_tail = std::env::var("SAT_FTCSR_PC_TAIL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0usize);
        if pc_tail != 0 {
            let dump_pc = |name: &str, pcs: &[u32]| {
                eprintln!(
                    "FTCSR post-write PC {name} head {pc_tail} of {}:",
                    pcs.len()
                );
                for pc in pcs.iter().take(pc_tail) {
                    eprintln!("  {pc:08X}");
                }
            };
            dump_pc("master", &saturn.master().dbg_pc_log);
            dump_pc("slave", &saturn.slave().dbg_pc_log);
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
    use super::{burst_cap, state_base_for};
    use std::path::Path;

    // Save states key to the game disc (so each game has its own slots); a live
    // `cdrom:` drive and a no-disc boot fall back to the BIOS base. (The `.bup`
    // battery keys to the BIOS separately — not exercised here.)
    #[test]
    fn save_state_base_keys_to_disc_then_falls_back_to_bios() {
        let bios = Path::new("/bios/saturn.bin");
        assert_eq!(
            state_base_for(Some("/roms/game.cue"), bios),
            Path::new("/roms/game.cue"),
            "a disc image keys the save states to itself"
        );
        assert_eq!(
            state_base_for(Some("cdrom:/dev/sr0"), bios),
            bios,
            "a live optical drive has no image path → BIOS fallback"
        );
        assert_eq!(state_base_for(None, bios), bios, "no disc → BIOS fallback");
    }

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
