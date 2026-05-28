//! Save-state round-trip, determinism, and validation (M8).
//!
//! The determinism test is the important one: it proves the snapshot is
//! *complete*. Run a while, snapshot, then run the snapshot and the live
//! machine forward by the same amount — if any un-serialized state existed,
//! the two would diverge and their re-snapshots would differ.

use saturn::Saturn;
use saturn::cartridge::Cartridge;
use saturn::savestate::SaveStateError;
use sh2::bus::{AccessKind, Bus};

const LOW_WRAM: u32 = 0x0020_0000;
const HIGH_WRAM: u32 = 0x0600_0000;

#[test]
fn save_load_roundtrip_preserves_ram() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    // Stamp distinctive data into both work-RAM tiers.
    sat.bus.write32(LOW_WRAM + 0x40, 0xDEAD_BEEF, AccessKind::Data);
    sat.bus.write32(HIGH_WRAM + 0x80, 0xCAFE_F00D, AccessKind::Data);

    let snapshot = sat.save_state();

    // Scribble over it, then restore.
    sat.bus.write32(LOW_WRAM + 0x40, 0, AccessKind::Data);
    sat.bus.write32(HIGH_WRAM + 0x80, 0, AccessKind::Data);
    sat.load_state(&snapshot).expect("reload");

    let (lo, _) = sat.bus.read32(LOW_WRAM + 0x40, AccessKind::Data);
    let (hi, _) = sat.bus.read32(HIGH_WRAM + 0x80, AccessKind::Data);
    assert_eq!(lo, 0xDEAD_BEEF);
    assert_eq!(hi, 0xCAFE_F00D);
}

#[test]
fn snapshot_then_equal_runs_stay_identical() {
    // Completeness/determinism: a restored machine and the original, run
    // forward by the same budget, must produce byte-identical snapshots.
    let mut a = Saturn::with_blank_bios();
    a.reset();
    a.run_for(50_000);
    a.bus.write32(LOW_WRAM + 0x10, 0x1234_5678, AccessKind::Data);

    let snapshot = a.save_state();

    let mut b = Saturn::with_blank_bios();
    b.reset();
    b.load_state(&snapshot).expect("reload");

    // Identical state in → identical state out after an identical run.
    a.run_for(200_000);
    b.run_for(200_000);
    assert_eq!(
        a.save_state(),
        b.save_state(),
        "restored machine diverged from the original — some state isn't serialized"
    );
}

#[test]
fn dram_cart_volatile_ram_survives_but_rom_is_regrafted() {
    // Extension DRAM is volatile state (serialized); a ROM cart's image is
    // external media (skipped + re-grafted), so load must keep the live ROM.
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    sat.insert_cartridge(Cartridge::ext_ram_4mb());
    sat.bus.write32(0x0240_0000, 0xABCD_1234, AccessKind::Data); // bank 0
    let snapshot = sat.save_state();
    sat.bus.write32(0x0240_0000, 0, AccessKind::Data);
    sat.load_state(&snapshot).expect("reload");
    let (v, _) = sat.bus.read32(0x0240_0000, AccessKind::Data);
    assert_eq!(v, 0xABCD_1234, "DRAM cart contents restored");

    // A ROM cart: bytes are skipped, so the running instance keeps its image.
    let mut rom_sat = Saturn::with_blank_bios();
    rom_sat.reset();
    rom_sat.insert_cartridge(Cartridge::rom(vec![0x11, 0x22, 0x33, 0x44]));
    let snap = rom_sat.save_state();
    rom_sat.load_state(&snap).expect("reload");
    let (w, _) = rom_sat.bus.read32(0x0200_0000, AccessKind::Data);
    assert_eq!(w, 0x1122_3344, "ROM image re-grafted across load");
}

#[test]
fn internal_backup_is_preformatted_and_persists() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    // A fresh (charged-battery) console shows the BIOS format signature.
    assert_eq!(&sat.internal_backup()[..16], b"BackUpRam Format");

    // A game writes a save (odd byte lanes hold data); the unpacked image
    // that battery persistence writes out should capture it.
    sat.bus.write8(0x0018_0000 + 0x101, 0x42, AccessKind::Data);
    let image = sat.internal_backup().to_vec();
    assert_eq!(image[0x101 >> 1], 0x42);

    // Persisted image reloads into a fresh console (the battery survived).
    let mut next = Saturn::with_blank_bios();
    next.reset();
    next.load_internal_backup(&image);
    let (v, _) = next.bus.read8(0x0018_0000 + 0x101, AccessKind::Data);
    assert_eq!(v, 0x42);
}

#[test]
fn bad_magic_is_rejected() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    let mut blob = sat.save_state();
    blob[0] ^= 0xFF; // corrupt the magic, leave the rest decodable
    assert_eq!(sat.load_state(&blob), Err(SaveStateError::BadMagic));
}

#[test]
fn truncated_blob_is_a_decode_error() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    let blob = sat.save_state();
    let half = &blob[..blob.len() / 2];
    assert!(matches!(
        sat.load_state(half),
        Err(SaveStateError::Decode(_))
    ));
}

#[test]
fn bios_mismatch_is_rejected() {
    let mut a = Saturn::new(vec![0u8; 512 * 1024]);
    a.reset();
    let snapshot = a.save_state();

    // A machine with a different BIOS image must refuse the state.
    let mut b = Saturn::new(vec![0x5A; 512 * 1024]);
    b.reset();
    assert_eq!(b.load_state(&snapshot), Err(SaveStateError::BiosMismatch));
}

#[test]
fn disc_mismatch_is_rejected() {
    // Snapshot taken with a disc inserted; restoring onto a disc-less machine
    // (same BIOS) must fail rather than silently resume against no media.
    let mut a = Saturn::with_blank_bios();
    a.reset();
    a.insert_disc(saturn::disc::Disc::from_iso(vec![0u8; 2048 * 4]));
    let snapshot = a.save_state();

    let mut b = Saturn::with_blank_bios();
    b.reset();
    assert_eq!(b.load_state(&snapshot), Err(SaveStateError::DiscMismatch));
}
