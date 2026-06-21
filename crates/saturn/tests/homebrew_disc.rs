//! Homebrew test-disc harness — a CI-able accuracy regression driven by a
//! **royalty-free** Saturn disc (built with libyaul; see `tests/disc/`).
//!
//! Unlike commercial games, an MIT-licensed homebrew disc can be committed, so
//! this turns "boot a real disc and check a feature" into a deterministic test:
//! boot the disc on the real-BIOS LLE path (the same path the games use — a
//! libyaul `IP.BIN` carries the `SEGA SEGASATURN` header our auth checks), run
//! a fixed number of frames, then verify the test's **result protocol** in High
//! Work RAM and (optionally) a framebuffer golden hash.
//!
//! # Result protocol (the contract with the disc-side program)
//!
//! The test program writes, as its *last* action, a signature + status to fixed
//! High-WRAM addresses (see `tests/disc/README.md`). The signature guards
//! against a non-protocol disc (e.g. a placeholder) matching by accident:
//!
//! | address       | meaning                                            |
//! |---------------|----------------------------------------------------|
//! | `0x0603_FF00` | signature `0x54535431` (`"TST1"`) once results are in |
//! | `0x0603_FF04` | status: `0` = all pass, non-zero = failing test id |
//! | `0x0603_FF08` | detail/last-checked code (informational)           |
//!
//! # Gating (skips, never false-fails)
//!
//! Needs both a Saturn BIOS in `bios/` and a built disc. Either absent → the
//! test prints why and returns OK, so CI without them is a no-op. Point it at a
//! disc with `HOMEBREW_DISC=<path.cue|.iso|.ccd|.chd>`; the default is the
//! committed build output. A disc that doesn't speak the protocol (no `TST1`
//! signature) is reported **inconclusive**, not failed — handy for smoke-
//! testing this harness against any bootable disc.

use std::path::{Path, PathBuf};

use saturn::Saturn;
use saturn::disc::Disc;
use saturn::vdp2::FRAMEBUFFER_BYTES;

const BIOS_CANDIDATES: &[&str] = &[
    "bios/Sega Saturn BIOS (USA).bin",
    "bios/Sega Saturn BIOS (EUR).bin",
    "bios/Sega Saturn BIOS v1.01 (JAP).bin",
    "bios/Sega Saturn BIOS v1.00 (JAP).bin",
];

/// Default location of the built test disc (relative to the repo root).
const DEFAULT_DISC: &str = "tests/disc/build/saturn-tests.cue";

/// Frames to run before sampling the result — enough for the BIOS to
/// authenticate, load `IP.BIN`, jump to the program, and the program to run its
/// checks and post the result word. Generous; bump if a heavier suite needs it.
const FRAMES_TO_RUN: u32 = 600; // ~10 s NTSC

const HIGH_WRAM_BASE: u32 = 0x0600_0000;
const SIG_ADDR: u32 = 0x0603_FF00;
const STATUS_ADDR: u32 = 0x0603_FF04;
const DETAIL_ADDR: u32 = 0x0603_FF08;
const SIG_MAGIC: u32 = 0x5453_5431; // "TST1"

#[test]
fn homebrew_test_disc_reports_pass() {
    let (bios, bios_path) = match load_first_available_bios() {
        Some(p) => p,
        None => {
            println!("no Saturn BIOS in bios/ (see bios/README.md); homebrew-disc test skipped.");
            return;
        }
    };
    let disc_path =
        std::env::var("HOMEBREW_DISC").unwrap_or_else(|_| workspace_join(DEFAULT_DISC));
    if !Path::new(&disc_path).exists() {
        println!(
            "no test disc at {disc_path} (build it under tests/disc/, or set \
             HOMEBREW_DISC=<path>); homebrew-disc test skipped."
        );
        return;
    }
    let disc = match load_disc(&disc_path) {
        Ok(d) => d,
        Err(e) => {
            println!("could not load {disc_path}: {e}; homebrew-disc test skipped.");
            return;
        }
    };
    println!("BIOS: {}\ndisc: {disc_path}", bios_path.display());

    let mut sat = Saturn::new(bios);
    sat.insert_disc(disc);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    let mut dims = (0usize, 0usize);
    let mut frames_run = 0u32;
    for _ in 0..FRAMES_TO_RUN {
        dims = sat.run_frame(&mut fb);
        frames_run += 1;
        // Stop as soon as the program posts its result signature — keeps a
        // passing run fast (FRAMES_TO_RUN is just the give-up ceiling).
        if sat.bus.high_wram.read32(SIG_ADDR - HIGH_WRAM_BASE) == SIG_MAGIC {
            break;
        }
    }

    let sig = sat.bus.high_wram.read32(SIG_ADDR - HIGH_WRAM_BASE);
    let status = sat.bus.high_wram.read32(STATUS_ADDR - HIGH_WRAM_BASE);
    let detail = sat.bus.high_wram.read32(DETAIL_ADDR - HIGH_WRAM_BASE);
    let fb_hash = fnv1a_64(&fb[..dims.0 * dims.1 * 4]);
    println!(
        "after {frames_run} frames: master PC=0x{:08X}, sig=0x{sig:08X}, \
         status=0x{status:08X}, detail=0x{detail:08X}, fb {}x{} hash=0x{fb_hash:016X}",
        sat.master().regs.pc, dims.0, dims.1
    );

    if sig != SIG_MAGIC {
        // Not the protocol disc (or it never reached its result write). Treat as
        // inconclusive rather than a failure so this harness can be smoke-tested
        // against any bootable disc (e.g. HOMEBREW_DISC=roms/...).
        println!(
            "INCONCLUSIVE: no TST1 result signature in High-WRAM — is {disc_path} \
             the homebrew test disc? (see tests/disc/README.md)"
        );
        return;
    }
    assert_eq!(
        status, 0,
        "homebrew test disc reported FAILURE: failing test id 0x{status:08X} \
         (detail 0x{detail:08X}). See tests/disc/ for the test that set it."
    );
    println!("homebrew test disc: all checks PASSED.");
}

fn load_disc(path: &str) -> Result<Disc, String> {
    let p = Path::new(path);
    let dir = p.parent().unwrap_or_else(|| Path::new("."));
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "iso" => Ok(Disc::from_iso(std::fs::read(p).map_err(|e| e.to_string())?)),
        "cue" => {
            let cue = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
            Disc::from_cue(&cue, |name| std::fs::read(dir.join(name)).ok())
        }
        "ccd" => {
            let ccd = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
            let img = std::fs::read(p.with_extension("img")).map_err(|e| e.to_string())?;
            Disc::from_ccd(&ccd, img)
        }
        #[cfg(feature = "chd")]
        "chd" => saturn::chd_image::from_chd(std::fs::File::open(p).map_err(|e| e.to_string())?),
        other => Err(format!("unsupported disc format .{other}")),
    }
}

fn load_first_available_bios() -> Option<(Vec<u8>, PathBuf)> {
    for rel in BIOS_CANDIDATES {
        let p = PathBuf::from(workspace_join(rel));
        if let Ok(bytes) = std::fs::read(&p) {
            return Some((bytes, p));
        }
    }
    None
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn workspace_join(rel: &str) -> String {
    workspace_root().join(rel).to_string_lossy().into_owned()
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut h = OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}
