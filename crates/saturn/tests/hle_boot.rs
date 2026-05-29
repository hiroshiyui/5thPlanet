//! M11 — HLE direct boot (ADR-0010).
//!
//! Verifies `Saturn::hle_boot` loads the disc's 1st-read program into work RAM
//! and points the master SH-2 at it. Gated on the copyrighted VF2 fixture
//! (`roms/vf2_full.cue` + a JP BIOS) — skips cleanly when absent, so CI is
//! unaffected.

use std::path::PathBuf;

use saturn::Saturn;
use sh2::bus::{AccessKind, Bus};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[test]
fn hle_boot_loads_the_first_read_program() {
    let root = workspace_root();
    let Ok(bios) = std::fs::read(root.join("bios/Sega Saturn BIOS v1.01 (JAP).bin")) else {
        eprintln!("no JP BIOS; skipped");
        return;
    };
    let Ok(cue) = std::fs::read_to_string(root.join("roms/vf2_full.cue")) else {
        eprintln!("no roms/vf2_full.cue fixture; skipped");
        return;
    };
    let disc = saturn::disc::Disc::from_cue(&cue, |name| {
        std::fs::read(root.join("roms").join(name)).ok()
    })
    .expect("parse vf2_full.cue");

    let mut sat = Saturn::new(bios);
    sat.reset();
    sat.insert_disc(disc);

    // HLE boot reads IP.BIN + the root directory straight from the disc; it
    // does not need the BIOS to have run first.
    let load_addr = sat
        .hle_boot()
        .expect("hle_boot should find a 1st-read file");
    assert_eq!(load_addr, 0x0600_4000, "VF2 IP.BIN 1st-read load address");
    assert_eq!(
        sat.master().regs.pc,
        0x0600_4000,
        "master SH-2 jumped to the 1st-read entry",
    );

    // AAAVF2.BIN's first instruction is `MOV.L @(disp,PC),R15` (0xDF0B) — the
    // program setting up its own stack. Confirm the bytes actually landed in
    // high work RAM at the load address.
    let (w0, _) = sat.bus.read16(0x0600_4000, AccessKind::Data);
    assert_eq!(w0, 0xDF0B, "1st-read entry word loaded into work RAM");
    let (w1, _) = sat.bus.read16(0x0600_4002, AccessKind::Data);
    assert_eq!(w1, 0xE0F0, "1st-read second word loaded into work RAM");
}
