//! Saturn bus routing (M2 task #3). One test per region — verify reads
//! and writes land in the right backing store, that BIOS mirrors,
//! and that unmapped addresses behave as open bus.

use saturn::SaturnBus;
use saturn::bus::{
    ABUS_BBUS_BASE, BACKUP_BASE, BIOS_BASE, HIGH_WRAM_BASE, LOW_WRAM_BASE, SMPC_BASE, SOUND_BASE,
};
use sh2::bus::{AccessKind, Bus};

fn bios_with_pattern() -> Vec<u8> {
    // 512 KiB image with a recognizable pattern: byte i = i & 0xFF.
    (0..512 * 1024).map(|i| (i & 0xFF) as u8).collect()
}

fn fresh() -> SaturnBus {
    SaturnBus::new(bios_with_pattern())
}

#[test]
fn bios_reads_from_image_and_mirrors_within_window() {
    let mut bus = fresh();
    // First quarter — direct read.
    let (a, _) = bus.read32(BIOS_BASE + 0x100, AccessKind::Fetch);
    assert_eq!(a, 0x0001_0203, "matches byte pattern at offset 0x100");
    // Past the image size, mirror back to the start.
    let (b, _) = bus.read32(BIOS_BASE + 0x8_0000 + 0x100, AccessKind::Fetch);
    assert_eq!(b, a, "BIOS mirrors across the 1 MiB window");
}

#[test]
fn bios_writes_are_silently_dropped() {
    let mut bus = fresh();
    let (before, _) = bus.read8(BIOS_BASE + 0x10, AccessKind::Data);
    bus.write8(BIOS_BASE + 0x10, 0xFF, AccessKind::Data);
    let (after, _) = bus.read8(BIOS_BASE + 0x10, AccessKind::Data);
    assert_eq!(before, after, "BIOS is read-only");
}

#[test]
fn low_wram_round_trip() {
    let mut bus = fresh();
    bus.write32(LOW_WRAM_BASE + 0x1234, 0xCAFE_F00D, AccessKind::Data);
    let (v, _) = bus.read32(LOW_WRAM_BASE + 0x1234, AccessKind::Data);
    assert_eq!(v, 0xCAFE_F00D);
}

#[test]
fn high_wram_round_trip_and_independent_of_low_wram() {
    let mut bus = fresh();
    bus.write32(HIGH_WRAM_BASE + 0x4000, 0xDEAD_BEEF, AccessKind::Data);
    let (v, _) = bus.read32(HIGH_WRAM_BASE + 0x4000, AccessKind::Data);
    assert_eq!(v, 0xDEAD_BEEF);
    // Same offset into low WRAM is independent — different region.
    let (z, _) = bus.read32(LOW_WRAM_BASE + 0x4000, AccessKind::Data);
    assert_eq!(z, 0);
}

#[test]
fn backup_ram_round_trip_and_mirrors() {
    let mut bus = fresh();
    bus.write8(BACKUP_BASE + 0x10, 0x77, AccessKind::Data);
    let (v, _) = bus.read8(BACKUP_BASE + 0x10, AccessKind::Data);
    assert_eq!(v, 0x77);
    // 32 KiB region mirrored within the 512 KiB window.
    let (mirror, _) = bus.read8(BACKUP_BASE + 0x8000 + 0x10, AccessKind::Data);
    assert_eq!(mirror, 0x77);
}

#[test]
fn smpc_sound_abus_bbus_stubs_return_zero() {
    let mut bus = fresh();
    for &base in &[SMPC_BASE, SOUND_BASE, ABUS_BBUS_BASE] {
        bus.write32(base + 0x100, 0xAAAA_BBBB, AccessKind::Data);
        let (v, _) = bus.read32(base + 0x100, AccessKind::Data);
        assert_eq!(v, 0, "stub region returns 0 even after a write");
    }
}

#[test]
fn unmapped_address_reads_zero() {
    let mut bus = fresh();
    // Between low WRAM end (0x002F_FFFF) and sound base (0x0040_0000)
    // is unmodeled space.
    let (v, _) = bus.read32(0x0035_0000, AccessKind::Data);
    assert_eq!(v, 0);
    bus.write32(0x0035_0000, 0xFFFF_FFFF, AccessKind::Data);
    let (v2, _) = bus.read32(0x0035_0000, AccessKind::Data);
    assert_eq!(v2, 0, "writes to unmapped space are dropped");
}

#[test]
fn wait_states_per_region_are_sane() {
    let mut bus = fresh();
    // Just exercise that each region returns *some* wait count without
    // panicking; concrete values are an implementation detail we don't
    // want to over-specify here, but BIOS should outlast work RAM.
    let (_, bios_w) = bus.read32(BIOS_BASE, AccessKind::Fetch);
    let (_, low_w) = bus.read32(LOW_WRAM_BASE, AccessKind::Data);
    let (_, high_w) = bus.read32(HIGH_WRAM_BASE, AccessKind::Data);
    assert!(bios_w >= low_w);
    assert!(low_w >= high_w);
}

#[test]
fn endianness_is_big_for_word_writes() {
    let mut bus = fresh();
    bus.write32(LOW_WRAM_BASE, 0x1122_3344, AccessKind::Data);
    let (b0, _) = bus.read8(LOW_WRAM_BASE, AccessKind::Data);
    let (b1, _) = bus.read8(LOW_WRAM_BASE + 1, AccessKind::Data);
    let (b2, _) = bus.read8(LOW_WRAM_BASE + 2, AccessKind::Data);
    let (b3, _) = bus.read8(LOW_WRAM_BASE + 3, AccessKind::Data);
    assert_eq!([b0, b1, b2, b3], [0x11, 0x22, 0x33, 0x44]);
}
