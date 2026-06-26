//! Smoke test for the system/boot diagnostics (`saturn::diagnostics::run_system`).
//!
//! `run_system` needs a real Saturn BIOS (it boots a throwaway machine), so —
//! like `bios_boot.rs` — this skips cleanly and passes when none is present
//! (CI has no BIOS). It guards `run_system` against API rot and, when a BIOS is
//! supplied, exercises the boot path end-to-end. The hermetic feature checks
//! (`run_all`) are covered separately by the in-module `all_diagnostics_pass`.

use std::path::PathBuf;

use saturn::diagnostics::run_system;

const BIOS_CANDIDATES: &[&str] = &[
    "bios/Sega Saturn BIOS (USA).bin",
    "bios/Sega Saturn BIOS (EUR).bin",
    "bios/Sega Saturn BIOS v1.01 (JAP).bin",
    "bios/Sega Saturn BIOS v1.00 (JAP).bin",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn load_bios() -> Option<Vec<u8>> {
    for rel in BIOS_CANDIDATES {
        if let Ok(b) = std::fs::read(workspace_root().join(rel)) {
            return Some(b);
        }
    }
    None
}

#[test]
fn run_system_reports_bios_video() {
    let Some(bios) = load_bios() else {
        println!("no Saturn BIOS in bios/ (see bios/README.md); run_system smoke skipped.");
        return;
    };
    // No disc → only the BIOS checks run; expect the splash to produce video.
    let outcomes = run_system(bios, None, saturn::smpc::region::NORTH_AMERICA);
    let video = outcomes
        .iter()
        .find(|o| o.name == "bios_video")
        .expect("bios_video outcome present");
    assert!(
        video.passed,
        "bios_video should pass for a real BIOS: {}",
        video.detail
    );
}
