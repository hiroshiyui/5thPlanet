//! Cartridge slot reachable through the Saturn bus (M7).
//!
//! Confirms the rear expansion connector maps where hardware does
//! (`0x0200_0000..0x04FF_FFFF`), reports the right cart-ID byte at
//! `0x04FF_FFFF`, and that each cart family — Extension DRAM (1 MiB /
//! 4 MiB, two banks), battery backup RAM (odd-byte packing), and game ROM
//! — round-trips through the bus. Pure-module behaviour (mirroring, packing)
//! is asserted alongside.

use saturn::Saturn;
use saturn::cartridge::{
    CART_BRAM_BASE, CART_DRAM0_BASE, CART_DRAM1_BASE, CART_ID_ADDR, CART_ROM_BASE, Cartridge,
};
use sh2::bus::{AccessKind, Bus};

fn sat() -> Saturn {
    Saturn::with_blank_bios()
}

#[test]
fn empty_slot_floats_high() {
    let mut s = sat();
    // No cart: ID byte and all cart space read 0xFF (A-bus pulled high).
    let (id, _) = s.bus.read8(CART_ID_ADDR, AccessKind::Data);
    assert_eq!(id, 0xFF, "empty slot reports cart-ID 0xFF");
    let (rom, _) = s.bus.read32(CART_ROM_BASE, AccessKind::Data);
    assert_eq!(rom, 0xFFFF_FFFF, "empty ROM window floats high");
    let (dram, _) = s.bus.read32(CART_DRAM0_BASE, AccessKind::Data);
    assert_eq!(dram, 0xFFFF_FFFF, "empty DRAM window floats high");
    // Writes into an empty slot are dropped (don't panic, stay floating).
    s.bus
        .write32(CART_DRAM0_BASE, 0x1234_5678, AccessKind::Data);
    let (after, _) = s.bus.read32(CART_DRAM0_BASE, AccessKind::Data);
    assert_eq!(after, 0xFFFF_FFFF);
}

#[test]
fn ext_ram_1mb_two_banks_and_id() {
    let mut s = sat();
    s.insert_cartridge(Cartridge::ext_ram_1mb());
    let (id, _) = s.bus.read8(CART_ID_ADDR, AccessKind::Data);
    assert_eq!(id, 0x5A, "1 MiB DRAM cart reports 0x5A (8 Mbit)");

    // Both banks are independent, writable RAM.
    s.bus
        .write32(CART_DRAM0_BASE, 0xDEAD_BEEF, AccessKind::Data);
    s.bus
        .write32(CART_DRAM1_BASE, 0xCAFE_F00D, AccessKind::Data);
    let (b0, _) = s.bus.read32(CART_DRAM0_BASE, AccessKind::Data);
    let (b1, _) = s.bus.read32(CART_DRAM1_BASE, AccessKind::Data);
    assert_eq!(b0, 0xDEAD_BEEF);
    assert_eq!(b1, 0xCAFE_F00D);

    // Each 512 KiB bank mirrors across its 2 MiB window.
    let (mirror, _) = s
        .bus
        .read32(CART_DRAM0_BASE + 0x0008_0000, AccessKind::Data);
    assert_eq!(mirror, 0xDEAD_BEEF, "512 KiB bank mirrors at +0x80000");

    // Byte and halfword granularity work too (big-endian).
    s.bus
        .write16(CART_DRAM0_BASE + 0x10, 0xABCD, AccessKind::Data);
    let (hw, _) = s.bus.read16(CART_DRAM0_BASE + 0x10, AccessKind::Data);
    assert_eq!(hw, 0xABCD);
    let (hi, _) = s.bus.read8(CART_DRAM0_BASE + 0x10, AccessKind::Data);
    assert_eq!(hi, 0xAB);
}

#[test]
fn ext_ram_4mb_reports_32mbit_and_holds_2mb_banks() {
    let mut s = sat();
    s.insert_cartridge(Cartridge::ext_ram_4mb());
    let (id, _) = s.bus.read8(CART_ID_ADDR, AccessKind::Data);
    assert_eq!(id, 0x5C, "4 MiB DRAM cart reports 0x5C (32 Mbit)");

    // A 2 MiB bank does not mirror until its full window; the very top of
    // bank 0 is distinct from its base.
    s.bus
        .write32(CART_DRAM0_BASE, 0x1111_1111, AccessKind::Data);
    s.bus
        .write32(CART_DRAM0_BASE + 0x0010_0000, 0x2222_2222, AccessKind::Data);
    let (a, _) = s.bus.read32(CART_DRAM0_BASE, AccessKind::Data);
    let (b, _) = s
        .bus
        .read32(CART_DRAM0_BASE + 0x0010_0000, AccessKind::Data);
    assert_eq!(a, 0x1111_1111);
    assert_eq!(b, 0x2222_2222, "distinct cells within the 2 MiB bank");
}

#[test]
fn rom_cart_reads_back_image_and_ignores_writes() {
    let mut s = sat();
    // Big-endian image: 0x00 0x11 0x22 0x33 at the base.
    let image = vec![0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
    s.insert_cartridge(Cartridge::rom(image));
    // ROM cart reports 0xFF like an empty slot (matches hardware/MAME).
    let (id, _) = s.bus.read8(CART_ID_ADDR, AccessKind::Data);
    assert_eq!(id, 0xFF);
    let (w0, _) = s.bus.read32(CART_ROM_BASE, AccessKind::Data);
    assert_eq!(w0, 0x0011_2233);
    // Writes to ROM disappear.
    s.bus.write32(CART_ROM_BASE, 0xFFFF_FFFF, AccessKind::Data);
    let (after, _) = s.bus.read32(CART_ROM_BASE, AccessKind::Data);
    assert_eq!(after, 0x0011_2233, "ROM is read-only");
    // Small image mirrors across the 4 MiB window.
    let (mirror, _) = s.bus.read32(CART_ROM_BASE + 8, AccessKind::Data);
    assert_eq!(mirror, 0x0011_2233);
}

#[test]
fn backup_ram_cart_odd_byte_packing_and_format_tag() {
    let mut s = sat();
    s.insert_cartridge(Cartridge::backup_ram(0x0008_0000)); // 4 Mbit / 512 KiB
    let (id, _) = s.bus.read8(CART_ID_ADDR, AccessKind::Data);
    assert_eq!(id, 0x21, "512 KiB battery cart reports 0x21 (4 Mbit)");

    // Pre-formatted with the "BackUpRam Format" tag — visible as one data
    // byte per packed lane. Word 0 packs bytes[0]='B' (bits 23..16) and
    // bytes[1]='a' (bits 7..0); the other two lanes are wired to 0.
    let (w0, _) = s.bus.read32(CART_BRAM_BASE, AccessKind::Data);
    assert_eq!(w0, ((b'B' as u32) << 16) | (b'a' as u32));

    // Round-trip respects the packing: only bits 23..16 and 7..0 persist.
    s.bus
        .write32(CART_BRAM_BASE + 0x40, 0xAABB_CCDD, AccessKind::Data);
    let (rb, _) = s.bus.read32(CART_BRAM_BASE + 0x40, AccessKind::Data);
    assert_eq!(rb, 0x00BB_00DD, "wired-to-0 lanes read back 0");
}

#[test]
fn backup_cartridge_persists_and_reloads() {
    // The host-persistence API (`cartridge_backup`/`load_cartridge_backup`):
    // only a battery `Bram` cart exposes bytes, and a save→reload round-trip
    // restores its contents into a fresh cart — the file-backed battery.
    let mut s = sat();

    // No cart, and non-battery carts, have nothing to persist.
    assert!(
        s.cartridge_backup().is_none(),
        "empty slot: nothing to save"
    );
    s.insert_cartridge(Cartridge::ext_ram_1mb());
    assert!(
        s.cartridge_backup().is_none(),
        "DRAM cart isn't battery-backed"
    );

    // A battery cart with a game write, captured as a persisted image.
    s.insert_cartridge(Cartridge::backup_ram(0x0008_0000));
    s.bus
        .write32(CART_BRAM_BASE + 0x40, 0xAABB_CCDD, AccessKind::Data);
    let saved = s
        .cartridge_backup()
        .expect("battery cart exposes its bytes")
        .to_vec();

    // A fresh console + fresh (formatted) cart of the same size; loading the
    // image restores the written word through the bus packing.
    let mut s2 = sat();
    s2.insert_cartridge(Cartridge::backup_ram(0x0008_0000));
    s2.load_cartridge_backup(&saved);
    let (rb, _) = s2.bus.read32(CART_BRAM_BASE + 0x40, AccessKind::Data);
    assert_eq!(
        rb, 0x00BB_00DD,
        "persisted battery contents survive a reload"
    );

    // Length reconciliation: loading into a smaller cart truncates without
    // panicking, and a no-cart load is a silent no-op.
    let mut s3 = sat();
    s3.insert_cartridge(Cartridge::backup_ram(0x0008_0000));
    s3.load_cartridge_backup(&vec![0xFF; 0x0040_0000]); // 4 MiB image into 512 KiB
    let mut s4 = sat();
    s4.load_cartridge_backup(&saved); // no cart plugged in
    assert!(s4.cartridge_backup().is_none());
}
