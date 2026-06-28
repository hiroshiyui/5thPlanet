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

// The OSD menu is pure logic (no sdl3): compile it for the SDL3 frontend and
// for tests (so its unit tests run even with `--no-default-features`), but not
// in a headless non-test build where nothing uses it.
mod config;
#[cfg(any(feature = "sdl-frontend", test))]
mod osd;
#[cfg(any(feature = "sdl-frontend", test))]
mod present;
// The SDL_GPU presentation backend (`--gpu`); compiled only with the
// `gpu-preview` feature, or in tests for its pure unit tests.
#[cfg(any(feature = "gpu-preview", test))]
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
    #[cfg(feature = "gpu-preview")]
    let mut gpu_override: Option<String> = None;
    #[cfg(feature = "gpu-preview")]
    let mut gpu_selftest = false;
    for arg in env::args().skip(1) {
        // `--gpu-selftest` runs the contained SDL_GPU Vulkan presenter proof and
        // exits (preview-only; see `present_gpu::run_selftest`).
        #[cfg(feature = "gpu-preview")]
        if arg == "--gpu-selftest" {
            gpu_selftest = true;
            continue;
        }
        // `--gpu[=off|auto|on]` is preview-only groundwork (off by default);
        // recognised only in `gpu-preview` builds — otherwise it falls through as
        // an unknown argument.
        #[cfg(feature = "gpu-preview")]
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
    #[cfg(feature = "gpu-preview")]
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
            #[cfg(feature = "gpu-preview")]
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
    // preview-only (the `gpu-preview` feature).
    #[cfg(feature = "gpu-preview")]
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

/// Subtract `n` from an atomic, clamping at 0. **Must saturate**: an
/// `AudioMsg::Reset` zeroes `audio_inflight` (`store(0)`) and can race ahead of
/// in-flight `Chunk` byte-count subtracts that were already queued, momentarily
/// driving the counter below zero — saturation absorbs that window so the pacing
/// reserve never underflows to a huge value (which would stall the emu thread).
#[cfg(feature = "sdl-frontend")]
fn atomic_saturating_sub(v: &std::sync::atomic::AtomicU32, n: u32) {
    use std::sync::atomic::Ordering;
    let _ = v.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
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
    let decoder = png::Decoder::new(ICON_PNG);
    let Ok(mut reader) = decoder.read_info() else {
        return;
    };
    let mut buf = vec![0u8; reader.output_buffer_size()];
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
/// parameter is `gpu-preview`-only; without that feature the renderer canvas is
/// the only backend.
#[cfg(feature = "sdl-frontend")]
// The `'a` ties both args to the result; without `gpu-preview` the `gpu` param is
// cfg'd out, leaving `'a` elidable — so silence the lint that only fires there.
#[allow(clippy::needless_lifetimes)]
fn backend_window_mut<'a>(
    canvas: &'a mut Option<sdl3::render::WindowCanvas>,
    #[cfg(feature = "gpu-preview")] gpu: &'a mut Option<present_gpu::GpuPresenter>,
) -> &'a mut sdl3::video::Window {
    if let Some(c) = canvas.as_mut() {
        return c.window_mut();
    }
    #[cfg(feature = "gpu-preview")]
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
    #[cfg(feature = "gpu-preview")] gpu: &'a Option<present_gpu::GpuPresenter>,
) -> &'a sdl3::video::Window {
    if let Some(c) = canvas.as_ref() {
        return c.window();
    }
    #[cfg(feature = "gpu-preview")]
    if let Some(g) = gpu.as_ref() {
        return g.window();
    }
    panic!("a presentation backend is always active");
}

#[cfg(feature = "sdl-frontend")]
fn run(
    bios: Vec<u8>,
    disc_spec: Option<String>,
    cart: Cartridge,
    save_base: std::path::PathBuf,
    region: u8,
    mut mouse_port: Option<u8>,
    cfg: config::Config,
) -> ExitCode {
    use sdl3::audio::{AudioFormat, AudioSpec};
    use sdl3::event::{Event, WindowEvent};
    use sdl3::keyboard::{Keycode, Scancode};
    use sdl3::pixels::PixelFormat;

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
    // time, like a console with a charged backup battery. Capture the seed so
    // an input recording (SAT_INPUT_REC) can store it in its header — a
    // deterministic replay (sdbg `replay`) must re-seed the same RTC.
    let rtc_seed = host_unix_secs();
    saturn.set_rtc_unix(rtc_seed);
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
        sharp: !present::is_smooth(&cfg.scaling),
        keep_aspect: present::keeps_aspect(&cfg.aspect),
        region: region_code_to_osd(region),
        cart: cartridge_to_osd(&cart),
        backend: token_to_osd_backend(&cfg.backend),
        #[cfg(feature = "gpu-preview")]
        shader_crt: present_gpu::ShaderMode::from_token(&cfg.shader).is_crt(),
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
        apply_mouse_port(&mut saturn, mouse_port);
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

    let sdl = sdl3::init().expect("SDL3 init");
    let video = sdl.video().expect("SDL3 video subsystem");
    // Host game controllers (the SDL gamepad API normalizes every recognized pad
    // — XInput on Windows, evdev on Linux — to one Xbox-style layout). Devices
    // are opened on hot-plug events; SDL also delivers an Added event for each
    // pad already attached at init.
    let controller_subsystem = sdl.gamepad().expect("SDL3 gamepad subsystem");
    let mut controllers: Vec<sdl3::gamepad::Gamepad> = Vec::new();

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
    // filled once — see the prebuffer gate in the main loop. Resuming here would
    // let the device drain from t=0 while the queue is still empty during boot,
    // which under-runs exactly once on a cold start (the "first-play buzz").
    // Presentation backend: the framebuffer is rendered in software (for
    // accuracy); this only selects how the finished frame reaches the window.
    // Default = the SDL_Renderer streaming-texture blit (which render driver it
    // picks is `--backend=` / `cfg.backend`, with a fallback chain — see the
    // `present` module). With the `gpu-preview` feature, `--gpu=auto|on` instead
    // presents via an SDL_GPU (Vulkan/SPIR-V) device. The two are **mutually
    // exclusive**: an SDL_GPU device claims the window its swapchain owns, so the
    // renderer canvas can't also own it. Exactly one backend is `Some`; window
    // controls + presentation route through whichever is live (`backend_window*`
    // and the present block).
    #[cfg(feature = "gpu-preview")]
    let mut gpu: Option<present_gpu::GpuPresenter> = {
        let mode = present_gpu::GpuMode::from_token(&cfg.gpu);
        if present_gpu::should_probe(mode) {
            match present_gpu::GpuPresenter::new(
                &video,
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
        } else {
            None
        }
    };
    // The renderer backend runs whenever the GPU backend isn't active.
    #[cfg(feature = "gpu-preview")]
    let use_renderer = gpu.is_none();
    #[cfg(not(feature = "gpu-preview"))]
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
    // borrowing it sits fine beside the canvas as a sibling local.
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
    let mut sharp = !present::is_smooth(&cfg.scaling);
    if let Some(t) = texture.as_mut() {
        t.set_scale_mode(present::scale_mode(sharp));
    }
    // Aspect handling: keep-ratio (letterbox to the 4:3 display aspect) or
    // fit-screen (stretch). The renderer uses SDL logical presentation; the GPU
    // backend computes the blit destination rect per frame. The Saturn picture is
    // 4:3 with non-square pixels, so its native framebuffer ratio is not the
    // display ratio — see present::logical_size.
    let mut keep_aspect = present::keeps_aspect(&cfg.aspect);
    if let Some(c) = canvas.as_mut() {
        present::apply_aspect(c, FRAME_WIDTH as u32, FRAME_HEIGHT as u32, keep_aspect);
    }
    set_window_icon(backend_window_mut(
        &mut canvas,
        #[cfg(feature = "gpu-preview")]
        &mut gpu,
    ));
    // Windowed mode is always aspect-locked to 4:3: we own the window, so snap it
    // to 4:3 (no bars, no distortion) rather than letterboxing inside a mismatched
    // window. The keep/stretch toggle then only takes effect in fullscreen (where
    // the display shape is fixed). `fullscreen` mirrors the window state so the
    // resize-snap is skipped while fullscreen (its Resized events are the display
    // size, not a user request). See `present::window_aspect_lock`.
    let mut fullscreen = cfg.fullscreen;
    if fullscreen {
        let _ = backend_window_mut(
            &mut canvas,
            #[cfg(feature = "gpu-preview")]
            &mut gpu,
        )
        .set_fullscreen(true);
    } else {
        snap_window_to_4_3(backend_window_mut(
            &mut canvas,
            #[cfg(feature = "gpu-preview")]
            &mut gpu,
        ));
    }

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
    // Send), plus chunks already sent to the SDL thread but not yet handed to
    // SDL. Without tracking that in-flight reserve, a slow present/event pass
    // can make the emu thread think the host queue is lower than it really is.
    let audio_ms: u64 = std::env::var("SAT_AUDIO_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120)
        .max(10);
    let audio_target_bytes = (AUDIO_BYTES_PER_SEC as u64 * audio_ms / 1000) as u32;
    let mut audio_started = false;
    let mut audio_drop_until_reset = false;
    let mut audio_dump = WavDump::from_env("SAT_AUDIO_DUMP");
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
    let audio_queue_log = std::env::var_os("SAT_AUDIO_QUEUE_LOG").is_some();
    let audio_watchdog_ms: u64 = std::env::var("SAT_AUDIO_WATCHDOG_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(750);
    let movie_probe: Option<u64> = std::env::var("SAT_MOVIE_PROBE")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| std::env::var_os("SAT_MOVIE_PROBE").map(|_| 30));
    let scsp_movie_probe = std::env::var_os("SAT_SCSP_MOVIE_PROBE").is_some();
    let scripted_pad: Option<u16> = std::env::var("SAT_PAD")
        .ok()
        .and_then(|s| u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
    let pad_from: u64 = std::env::var("SAT_PAD_FROM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let pad_to: u64 = std::env::var("SAT_PAD_TO")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(u64::MAX);
    let frame_limit: Option<u64> = std::env::var("SAT_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok());

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
    // Unbounded by design: the emu thread's `audio_tx.send` must never block (a
    // bounded channel would reintroduce emu stalls). The main thread fully
    // drains it every iteration, so it can't ratchet (and the watchdog backstops
    // a genuinely stuck main thread).
    let (audio_tx, audio_rx) = mpsc::channel::<AudioMsg>();
    let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();
    let audio_mirror = Arc::new(AtomicU32::new(0));
    let audio_inflight = Arc::new(AtomicU32::new(0));
    let osd_open = Arc::new(AtomicBool::new(false));
    let quit_flag = Arc::new(AtomicBool::new(false));

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
        let emu_inflight = Arc::clone(&audio_inflight);
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
            let mut audio_pacing_idle_since: Option<std::time::Instant> = None;
            let mut next_movie_probe = movie_probe.filter(|&period| period != 0);

            // Optional input recording (SAT_INPUT_REC=<path>): an "input movie"
            // for deterministic headless replay (sdbg `replay`). Writes the RTC
            // seed as a header, then one `<frame> <pad-hex>` line per port-1
            // pad-state change, where `frame` counts emulated frames
            // (advance_frame calls) from reset. The replay must match this
            // cadence (one frame + audio-drain per step) and re-seed the same
            // RTC. Caveat: record a clean play-through — opening the OSD or
            // quicksave/load desyncs the frame count from a fresh-boot replay.
            let mut rec = std::env::var("SAT_INPUT_REC").ok().and_then(|p| {
                use std::io::Write;
                let mut f = std::fs::File::create(&p).ok()?;
                let _ = writeln!(f, "# 5thplanet input movie (frame pad-hex; A=400 START=800)");
                let _ = writeln!(f, "rtc {rtc_seed}");
                eprintln!("input recording -> {p}");
                Some(f)
            });
            let mut rec_frame = 0u64;
            let mut rec_last = 0u16;

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
                    sharp: sess.ui.sharp,
                    keep_aspect: sess.ui.keep_aspect,
                    region: sess.ui.region,
                    cart: sess.ui.cart,
                    mouse: mouse_port_to_osd(sess.mouse_port),
                    backend: sess.ui.backend,
                    #[cfg(feature = "gpu-preview")]
                    shader_crt: sess.ui.shader_crt,
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
                                && dispatch_osd(
                                    action,
                                    &mut osd,
                                    &mut saturn,
                                    &mut sess,
                                    &ui_tx,
                                    &audio_tx,
                                )
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
                        EmuIn::Quickload => {
                            let path = sess.state_path();
                            match fs::read(&path) {
                            Ok(bytes) => match saturn.load_state(&bytes) {
                                Ok(()) => {
                                    apply_mouse_port(&mut saturn, sess.mouse_port);
                                    let _ = audio_tx.send(AudioMsg::Reset);
                                    eprintln!("quickload: {}", path.display());
                                    osd.set_toast("Quickload", 90);
                                }
                                Err(e) => {
                                    let _ = audio_tx.send(AudioMsg::Reset);
                                    eprintln!("load state {} failed: {e}", path.display());
                                }
                            },
                            Err(e) => {
                                let _ = audio_tx.send(AudioMsg::Reset);
                                eprintln!("no state to load at {} ({e})", path.display());
                            }
                            }
                        }
                        // F11/F12 reuse the menu's dispatch (updates ui state +
                        // config + sends the UiMsg back to the SDL thread + toast).
                        EmuIn::ToggleFullscreen => {
                            let _ = dispatch_osd(
                                osd::OsdAction::ToggleFullscreen,
                                &mut osd,
                                &mut saturn,
                                &mut sess,
                                &ui_tx,
                                &audio_tx,
                            );
                        }
                        EmuIn::ToggleAspect => {
                            let _ = dispatch_osd(
                                osd::OsdAction::ToggleAspect,
                                &mut osd,
                                &mut saturn,
                                &mut sess,
                                &ui_tx,
                                &audio_tx,
                            );
                        }
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

                if scripted_pad.is_none() {
                    saturn.set_pad1(held);
                }
                // Record a pad-state change at the frame it takes effect (the
                // burst below holds `held` constant for its whole span).
                if let Some(f) = rec.as_mut()
                    && held != rec_last
                {
                    use std::io::Write;
                    let _ = writeln!(f, "{rec_frame} {held:04X}");
                    let _ = f.flush();
                    rec_last = held;
                }
                // Audio-paced burst: run frames until the (mirrored) SDL queue
                // depth plus what this burst just produced reaches the target.
                // `burst_cap` renders every game-frame in normal play and only
                // collapses (run N, show 1) when the reserve has drained low —
                // chasing the full target unconditionally dropped ~1/3 of VF2's
                // frames and added input latency.
                let mut depth = emu_mirror
                    .load(Ordering::Relaxed)
                    .saturating_add(emu_inflight.load(Ordering::Relaxed));
                let cap = burst_cap(depth, catchup_floor, max_frames_per_burst);
                let mut burst = 0u32;
                while depth < audio_target_bytes && burst < cap {
                    if let Some(bits) = scripted_pad {
                        let frame = rec_frame + burst as u64;
                        saturn.set_pad1(if (pad_from..pad_to).contains(&frame) {
                            bits
                        } else {
                            0
                        });
                    }
                    let t = std::time::Instant::now();
                    saturn.advance_frame();
                    pl_advance += t.elapsed();
                    let chunk = saturn.take_audio();
                    let bytes = (chunk.len() * 2) as u32;
                    depth += bytes;
                    emu_inflight.fetch_add(bytes, Ordering::Relaxed);
                    if audio_tx.send(AudioMsg::Chunk { samples: chunk, bytes }).is_err() {
                        atomic_saturating_sub(&emu_inflight, bytes);
                    }
                    burst += 1;
                }
                pl_frames += burst;
                pl_bursts[(burst as usize).min(2)] += 1;
                rec_frame += burst as u64;
                if burst > 0
                    && let Some(period) = movie_probe
                    && (period == 0 || {
                        let next = next_movie_probe.get_or_insert(period);
                        let crossed = rec_frame >= *next;
                        while rec_frame >= *next {
                            *next = next.saturating_add(period);
                        }
                        crossed
                    })
                {
                    let (cd_status, cd_fad, cd_left, cd_free, parts) =
                        saturn.bus.cd_block.debug_state();
                    let part_summary = parts
                        .iter()
                        .enumerate()
                        .filter(|&(_, &n)| n != 0)
                        .map(|(i, n)| format!("{i}:{n}"))
                        .collect::<Vec<_>>()
                        .join(",");
                    let scsp_probe = scsp_probe_string(&saturn.bus.scsp, scsp_movie_probe);
                    eprintln!(
                        "SDLMOVIE f={rec_frame:04} pc={:08X} cd_st={cd_status:02X} fad={cd_fad} left={cd_left} free={cd_free} parts=[{part_summary}] depth={depth} mirror={} inflight={} burst={burst}{scsp_probe}",
                        saturn.master().regs.pc,
                        emu_mirror.load(Ordering::Relaxed),
                        emu_inflight.load(Ordering::Relaxed),
                    );
                }
                if let Some(limit) = frame_limit
                    && rec_frame >= limit
                {
                    let _ = ui_tx.send(UiMsg::Quit);
                    break 'emu;
                }

                // Collect the frame the worker rendered while we computed,
                // overlay any toast, and hand it to the main thread; then
                // dispatch this frame's render to overlap the next iteration.
                // Only when the machine actually advanced — the idle loop
                // (audio reserve full) spins at ~kHz and re-submitting the
                // same state would keep the worker re-rendering an identical
                // frame, burning a core and memory bandwidth for nothing.
                if burst > 0 {
                    audio_pacing_idle_since = None;
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
                    // If the SDL queue mirror/in-flight accounting ever gets
                    // stuck high across an OSD reset/load edge, the emulation
                    // thread would otherwise stop producing frames forever.
                    let now = std::time::Instant::now();
                    let idle_since = audio_pacing_idle_since.get_or_insert(now);
                    // `SAT_AUDIO_WATCHDOG_MS=0` disables the watchdog (matching
                    // the main-thread one); `.max(16)` is only a floor for
                    // nonzero values, so it can't fire every idle frame.
                    if audio_watchdog_ms != 0
                        && now.duration_since(*idle_since)
                            >= std::time::Duration::from_millis(audio_watchdog_ms.max(16))
                    {
                        let mirror = emu_mirror.load(Ordering::Relaxed);
                        let inflight = emu_inflight.load(Ordering::Relaxed);
                        eprintln!(
                            "SDL audio pacing watchdog: stalled with mirror={mirror} inflight={inflight} target={audio_target_bytes}; clearing pacing reserve"
                        );
                        emu_mirror.store(0, Ordering::Relaxed);
                        emu_inflight.store(0, Ordering::Relaxed);
                        audio_pacing_idle_since = None;
                    }
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
        let mut audio_last_queued = 0u32;
        let mut audio_last_change = std::time::Instant::now();
        let mut audio_last_log = std::time::Instant::now();
        'main: loop {
            // (See the event-range flush note in the git history: SDL ≥ 2.28
            // emits display events 0.37's binding panics on.)
            event_subsystem.flush_events(0x201, 0x20F);
            for ev in events.poll_iter() {
                match ev {
                    Event::Quit { .. } => break 'main,
                    // Windowed mode is aspect-locked to 4:3: when the user resizes
                    // the window, snap it back so the picture never letterboxes.
                    // The snap is idempotent (no-op once 4:3), so the re-fired
                    // Resized event converges instead of looping. Skipped while
                    // fullscreen (those Resized events are the display size).
                    Event::Window {
                        win_event: WindowEvent::Resized(..),
                        ..
                    } if !fullscreen => {
                        snap_window_to_4_3(backend_window_mut(
                            &mut canvas,
                            #[cfg(feature = "gpu-preview")]
                            &mut gpu,
                        ));
                    }
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
                            let _ = emu_tx.send(EmuIn::BindResult(b, Some(sc.name().to_string())));
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
                        let _ = audio_stream.pause();
                        let _ = audio_stream.clear();
                        audio_started = false;
                        audio_drop_until_reset = true;
                        audio_mirror.store(0, Ordering::Relaxed);
                        let _ = emu_tx.send(EmuIn::Quickload);
                    }
                    // F11 toggles fullscreen, F12 the fullscreen aspect mode
                    // (Keep ratio ↔ Fit screen) — both routed to the OSD dispatch
                    // so the menu labels + config track them.
                    Event::KeyDown {
                        keycode: Some(Keycode::F11),
                        ..
                    } => {
                        let _ = emu_tx.send(EmuIn::ToggleFullscreen);
                    }
                    Event::KeyDown {
                        keycode: Some(Keycode::F12),
                        ..
                    } => {
                        let _ = emu_tx.send(EmuIn::ToggleAspect);
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
                        // SDL3 mouse coordinates are floats; we accumulate integer
                        // deltas for the Shuttle Mouse.
                        mouse_dx += xrel as i32;
                        mouse_dy += yrel as i32;
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
                        // SDL3's JoystickId is a newtype around the u32 the event carries.
                        let jid = sdl3::sys::joystick::SDL_JoystickID(which);
                        match controller_subsystem.open(jid) {
                            Ok(c) => {
                                if !controllers.iter().any(|o| o.id().ok() == c.id().ok()) {
                                    eprintln!(
                                        "controller connected: {}",
                                        c.name().unwrap_or_default()
                                    );
                                    controllers.push(c);
                                }
                            }
                            Err(e) => eprintln!("controller open failed: {e}"),
                        }
                    }
                    Event::ControllerDeviceRemoved { which, .. } => {
                        controllers.retain(|c| c.id().ok().map(|j| j.0) != Some(which));
                    }
                    // Controller navigation of the open menu: D-pad moves, A
                    // selects, B backs out, Start toggles. Suppressed while a
                    // key-capture rebind is armed (that modal owns the input).
                    Event::ControllerButtonDown { button, .. }
                        if osd_open.load(Ordering::Relaxed) && rebind_target.is_none() =>
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
                use sdl3::gamepad::{Axis, Button};
                const TH: i16 = 16384; // half of full deflection
                for c in &controllers {
                    // SDL3 renamed the face buttons: South/East/West/North are
                    // the physical bottom/right/left/top (Xbox A/B/X/Y).
                    for (btn, bit) in [
                        (Button::DPadUp, pad::UP),
                        (Button::DPadDown, pad::DOWN),
                        (Button::DPadLeft, pad::LEFT),
                        (Button::DPadRight, pad::RIGHT),
                        (Button::West, pad::A),
                        (Button::South, pad::B),
                        (Button::East, pad::C),
                        (Button::North, pad::X),
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
                let want_grab = mouse_capture_enabled && !osd_open.load(Ordering::Relaxed);
                if want_grab != mouse_grabbed {
                    mouse_grabbed = want_grab;
                    sdl.mouse().set_relative_mouse_mode(
                        backend_window(
                            &canvas,
                            #[cfg(feature = "gpu-preview")]
                            &gpu,
                        ),
                        mouse_grabbed,
                    );
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

            // Push the emu thread's audio into the SDL3 stream (which auto-
            // resamples to the device rate) and refresh the depth mirror it paces
            // on. `queued_bytes()` is already in source (44.1 kHz) units, so no
            // device-rate scaling is needed. Start playback once the reserve has
            // first filled, so a cold start never drains an empty stream.
            let mut audio_chunks_this_iter = 0u32;
            while let Ok(msg) = audio_rx.try_recv() {
                match msg {
                    AudioMsg::Reset => {
                        let _ = audio_stream.pause();
                        let _ = audio_stream.clear();
                        audio_started = false;
                        audio_drop_until_reset = false;
                        audio_mirror.store(0, Ordering::Relaxed);
                        audio_inflight.store(0, Ordering::Relaxed);
                        if let Some(dump) = audio_dump.as_mut() {
                            dump.reset();
                        }
                        audio_last_queued = 0;
                        audio_last_change = std::time::Instant::now();
                    }
                    AudioMsg::Chunk { samples, bytes } => {
                        atomic_saturating_sub(&audio_inflight, bytes);
                        audio_chunks_this_iter += 1;
                        if !audio_drop_until_reset
                            && audio_stream.put_data_i16(&samples).is_ok()
                            && let Some(dump) = audio_dump.as_mut()
                        {
                            dump.write_samples(&samples);
                        }
                    }
                }
            }
            let mut src_size = audio_stream.queued_bytes().unwrap_or(0) as u32;
            let audio_now = std::time::Instant::now();
            if src_size != audio_last_queued {
                audio_last_queued = src_size;
                audio_last_change = audio_now;
            } else if audio_watchdog_ms != 0
                && audio_started
                && audio_chunks_this_iter == 0
                && src_size > catchup_floor
                && audio_last_change.elapsed()
                    >= std::time::Duration::from_millis(audio_watchdog_ms)
            {
                eprintln!(
                    "SDL audio queue watchdog: queued_bytes stuck at {src_size}; clearing host stream"
                );
                let _ = audio_stream.pause();
                let _ = audio_stream.clear();
                audio_started = false;
                audio_mirror.store(0, Ordering::Relaxed);
                audio_inflight.store(0, Ordering::Relaxed); // symmetry with the emu-thread + reset paths
                src_size = 0;
                audio_last_queued = 0;
                audio_last_change = audio_now;
            }
            audio_mirror.store(src_size, Ordering::Relaxed);
            if !audio_started && src_size >= audio_target_bytes {
                let _ = audio_stream.resume();
                audio_started = true;
            }
            if audio_queue_log && audio_last_log.elapsed().as_secs() >= 1 {
                eprintln!(
                    "SDLAUDIO queued={src_size} inflight={} target={audio_target_bytes} started={audio_started} drop={audio_drop_until_reset} chunks={audio_chunks_this_iter}",
                    audio_inflight.load(Ordering::Relaxed),
                );
                audio_last_log = std::time::Instant::now();
            }

            // Window-affecting OSD actions are applied here (the canvas is
            // not Send); Quit comes back the same way.
            let mut quit = false;
            while let Ok(m) = ui_rx.try_recv() {
                match m {
                    UiMsg::Scale(sc) => {
                        let _ = backend_window_mut(
                            &mut canvas,
                            #[cfg(feature = "gpu-preview")]
                            &mut gpu,
                        )
                        .set_size(
                            FRAME_WIDTH as u32 * sc as u32,
                            FRAME_HEIGHT as u32 * sc as u32,
                        );
                        // The scale grid is 320x224 (10:7); snap to the 4:3 lock.
                        if !fullscreen {
                            snap_window_to_4_3(backend_window_mut(
                                &mut canvas,
                                #[cfg(feature = "gpu-preview")]
                                &mut gpu,
                            ));
                        }
                    }
                    UiMsg::Fullscreen(on) => {
                        let _ = backend_window_mut(
                            &mut canvas,
                            #[cfg(feature = "gpu-preview")]
                            &mut gpu,
                        )
                        .set_fullscreen(on);
                        fullscreen = on;
                        // Returning to windowed re-asserts the 4:3 lock.
                        if !on {
                            snap_window_to_4_3(backend_window_mut(
                                &mut canvas,
                                #[cfg(feature = "gpu-preview")]
                                &mut gpu,
                            ));
                        }
                    }
                    UiMsg::Scaling(on) => {
                        sharp = on;
                        // Renderer: set the texture filter now. GPU: read `sharp`
                        // per-frame in present (its blit picks the filter).
                        if let Some(t) = texture.as_mut() {
                            t.set_scale_mode(present::scale_mode(sharp));
                        }
                    }
                    UiMsg::Aspect(keep) => {
                        keep_aspect = keep;
                        // Renderer: re-apply logical presentation. GPU: read
                        // `keep_aspect` per-frame in present (its blit dst rect).
                        if let Some(c) = canvas.as_mut() {
                            let (w, h) = cur_dims;
                            present::apply_aspect(c, w as u32, h as u32, keep_aspect);
                        }
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
                    #[cfg(feature = "gpu-preview")]
                    UiMsg::SetShader(crt) => {
                        // Apply to the live GPU backend; a no-op under the renderer.
                        if let Some(g) = gpu.as_mut() {
                            g.set_shader(crt);
                        }
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
                    // Renderer: recreate the streaming texture + re-apply filter and
                    // aspect. GPU: `GpuPresenter::present` recreates its own frame
                    // texture on a dims change, so nothing to do here.
                    if let (Some(cr), Some(c)) = (creator.as_ref(), canvas.as_mut()) {
                        let mut t = cr
                            .create_texture_streaming(
                                PixelFormat::ABGR8888,
                                dims.0 as u32,
                                dims.1 as u32,
                            )
                            .expect("recreate streaming texture");
                        // A fresh texture resets to SDL3's linear default — re-apply.
                        t.set_scale_mode(present::scale_mode(sharp));
                        texture = Some(t);
                        // Logical size tracks the new frame dims (keeps the aspect).
                        present::apply_aspect(c, dims.0 as u32, dims.1 as u32, keep_aspect);
                    }
                    cur_dims = dims;
                }
            }
            let t = std::time::Instant::now();
            let w = cur_dims.0;
            if let (Some(c), Some(tex)) = (canvas.as_mut(), texture.as_mut()) {
                tex.update(None, &framebuffer, w * 4)
                    .expect("upload framebuffer");
                c.clear();
                c.copy(&*tex, None, None).expect("blit to canvas");
                c.present(); // audio-paced (see the burst loop / idle sleep)
            }
            #[cfg(feature = "gpu-preview")]
            if let Some(g) = gpu.as_mut() {
                // GPU backend: upload + blit to the swapchain (OSD already
                // composited into `framebuffer`). Reads `sharp`/`keep_aspect` live.
                g.present(
                    &framebuffer,
                    (cur_dims.0 as u32, cur_dims.1 as u32),
                    sharp,
                    keep_aspect,
                );
            }
            pl_present += t.elapsed();
            pl_iters += 1;
            if perflog && pl_last.elapsed().as_secs() >= 1 && pl_iters > 0 {
                eprintln!(
                    "MAIN iters/s={pl_iters} | present avg {:.2} ms | queue={}ms",
                    pl_present.as_secs_f64() * 1e3 / pl_iters as f64,
                    (audio_stream.queued_bytes().unwrap_or(0) as u64 / 176) as u32,
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
#[cfg(feature = "sdl-frontend")]
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
    /// Move the Shuttle Mouse (or remove it): updates the SDL thread's capture
    /// gate so motion/clicks are fed only while a mouse port is active.
    SetMouse(Option<u8>),
    /// Select the CRT shader (`true`) vs the plain blit (`false`) on the SDL_GPU
    /// backend, applied live. Preview-only (the `gpu-preview` Shaders chooser).
    #[cfg(feature = "gpu-preview")]
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
    /// Shaders chooser. `gpu-preview`-only (the only build with a shader path).
    #[cfg(feature = "gpu-preview")]
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

/// The Shuttle Mouse port as the OSD names it (`None`/`1`/`2` ↔ Off/Port1/Port2).
#[cfg(feature = "sdl-frontend")]
fn mouse_port_to_osd(port: Option<u8>) -> osd::OsdMouse {
    match port {
        Some(1) => osd::OsdMouse::Port1,
        Some(_) => osd::OsdMouse::Port2,
        None => osd::OsdMouse::Off,
    }
}

/// Inverse of [`mouse_port_to_osd`]: the port index the rest of the frontend
/// tracks (the config token and the SDL capture gate use this).
#[cfg(feature = "sdl-frontend")]
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
        OsdAction::Save(n) => match fs::write(sess.slot_path(n), saturn.save_state()) {
            Ok(()) => osd.set_toast(format!("Saved slot {n}"), 120),
            Err(e) => osd.set_toast(format!("Save failed: {e}"), 180),
        },
        OsdAction::Load(n) => {
            let path = sess.slot_path(n);
            match fs::read(&path) {
                Ok(bytes) => match saturn.load_state(&bytes) {
                    Ok(()) => {
                        apply_mouse_port(saturn, sess.mouse_port);
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
        #[cfg(feature = "gpu-preview")]
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
        OsdAction::SetCartridge(k) => {
            sess.ui.cart = k;
            saturn.reset();
            saturn.set_region(osd_region_to_code(sess.ui.region));
            saturn.insert_cartridge(osd_cart_to_cartridge(k));
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

#[cfg(not(feature = "sdl-frontend"))]
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
                .chunks_exact(2)
                .filter(|p| p[0] != 0 || p[1] != 0)
                .count();
            let vdp1_draw = saturn
                .bus
                .vdp1
                .fb
                .as_slice()
                .chunks_exact(2)
                .filter(|p| p[0] != 0 || p[1] != 0)
                .count();
            let out_nonblack = framebuffer
                .chunks_exact(4)
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
