//! BIOS boot regression (M3 task #8 — the milestone-closing test).
//!
//! Loads a real Saturn BIOS image from `bios/`, runs ~3 seconds of
//! virtual time, hashes the resulting framebuffer, and compares the
//! hash to the committed golden at `crates/saturn/tests/golden/
//! bios_splash.hash`. Detects silent drift in any of the M3 chain:
//! SH-2, cache, bus routing, SMPC, SCU, DMA, INTC forwarding, VDP2
//! storage, VDP2 renderer.
//!
//! # BIOS gating
//!
//! Saturn BIOS images are copyrighted by SEGA and never live in this
//! repo (see `bios/README.md`). If no BIOS is found, the test prints
//! a skip message and returns successfully — CI without a BIOS dump
//! treats this as a no-op rather than a failure.
//!
//! # Golden bootstrap
//!
//! On the first run with a real BIOS, the golden file probably
//! doesn't exist. The test prints the hash it observed and what to
//! do with it; subsequent runs read it back and compare.

use std::path::PathBuf;

use saturn::Saturn;
use saturn::vdp2::FRAMEBUFFER_BYTES;

const BIOS_CANDIDATES: &[&str] = &[
    "bios/Sega Saturn BIOS (USA).bin",
    "bios/Sega Saturn BIOS (EUR).bin",
    "bios/Sega Saturn BIOS v1.01 (JAP).bin",
    "bios/Sega Saturn BIOS v1.00 (JAP).bin",
];

/// Approximate splash-render budget: 3 seconds of virtual time at
/// 60 Hz. The BIOS spends most of this on hardware initialisation
/// before drawing.
const FRAMES_TO_RUN: u32 = 180;

const GOLDEN_PATH: &str = "tests/golden/bios_splash.hash";

#[test]
fn bios_boots_to_stable_framebuffer_hash() {
    let (bios, used_path) = match load_first_available_bios() {
        Some(pair) => pair,
        None => {
            println!(
                "no Saturn BIOS found in bios/ (see bios/README.md); \
                 BIOS-boot regression skipped."
            );
            return;
        }
    };
    println!("BIOS image: {}", used_path.display());
    if bios.len() != 512 * 1024 {
        println!(
            "warning: BIOS is {} bytes (expected 512 KiB); hash may not match \
             the canonical golden if one is later committed",
            bios.len()
        );
    }

    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..FRAMES_TO_RUN {
        sat.run_frame(&mut fb);
    }
    let hash = fnv1a_64(&fb);

    let golden_full = workspace_root().join("crates/saturn").join(GOLDEN_PATH);
    match std::fs::read_to_string(&golden_full) {
        Ok(s) => {
            let golden = parse_hex_u64(s.trim()).expect("golden file must hold a hex u64");
            assert_eq!(
                hash, golden,
                "BIOS-boot framebuffer hash drifted: actual 0x{hash:016X}, \
                 expected 0x{golden:016X}. If the change was intentional, \
                 update {} with the new value.",
                golden_full.display()
            );
            println!("BIOS boot hash matches golden: 0x{hash:016X}");
        }
        Err(_) => {
            println!(
                "no golden hash committed yet at {}; \
                 observed hash this run: 0x{hash:016X}",
                golden_full.display()
            );
            println!(
                "after visually verifying the SDL2 frontend shows the SEGA \
                 splash, commit that hex string as the golden file."
            );
        }
    }
}

fn load_first_available_bios() -> Option<(Vec<u8>, PathBuf)> {
    let root = workspace_root();
    for rel in BIOS_CANDIDATES {
        let p = root.join(rel);
        if let Ok(bytes) = std::fs::read(&p) {
            return Some((bytes, p));
        }
    }
    None
}

/// Walks up from this crate's manifest dir to the repo root, where
/// `bios/` and the rest of the workspace live.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .and_then(|p| p.parent()) // repo root
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
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

fn parse_hex_u64(s: &str) -> Option<u64> {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}
