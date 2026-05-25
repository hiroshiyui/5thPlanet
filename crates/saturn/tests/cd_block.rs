//! CD-block command interface reachable through the Saturn bus (M4).
//!
//! Confirms the bus routes the CD-block host-interface registers
//! (0x0589_0008..0x0589_0026) correctly — distinct from the data FIFO at
//! 0x0589_8000 and not colliding with neighbouring regions — and that the
//! power-on signature and a basic command round-trip work end to end.
//! Register-level behaviour is covered by the unit tests in `cd_block.rs`.

use saturn::Saturn;
use saturn::cd_block::{CD_BLOCK_BASE, CD_BLOCK_END};
use sh2::bus::{AccessKind, Bus};

const HIRQ: u32 = CD_BLOCK_BASE + 0x08;
const CR1: u32 = CD_BLOCK_BASE + 0x18;
const CR2: u32 = CD_BLOCK_BASE + 0x1C;
const CR3: u32 = CD_BLOCK_BASE + 0x20;
const CR4: u32 = CD_BLOCK_BASE + 0x24;

#[test]
fn power_on_hirq_and_cdblock_signature_via_the_bus() {
    let mut sat = Saturn::with_blank_bios();
    let (hirq, _) = sat.bus.read16(HIRQ, AccessKind::Data);
    assert_eq!(hirq, 0xFFFF, "power-on HIRQ");
    // The BIOS reads CR1..CR4 for the ASCII "CDBLOCK" identity string.
    let (cr1, _) = sat.bus.read16(CR1, AccessKind::Data);
    let (cr2, _) = sat.bus.read16(CR2, AccessKind::Data);
    let (cr3, _) = sat.bus.read16(CR3, AccessKind::Data);
    let (cr4, _) = sat.bus.read16(CR4, AccessKind::Data);
    assert_eq!([cr1, cr2, cr3, cr4], [0x0043, 0x4442, 0x4C4F, 0x434B]);
}

#[test]
fn get_status_command_round_trips_through_the_bus() {
    let mut sat = Saturn::with_blank_bios();
    // Issue Get Status (command 0x00): write CR1..CR4; CR4 triggers it.
    sat.bus.write16(CR1, 0x0000, AccessKind::Data);
    sat.bus.write16(CR2, 0x0000, AccessKind::Data);
    sat.bus.write16(CR3, 0x0000, AccessKind::Data);
    sat.bus.write16(CR4, 0x0000, AccessKind::Data);
    let (hirq, _) = sat.bus.read16(HIRQ, AccessKind::Data);
    assert_eq!(hirq & 0x0001, 0x0001, "CMOK set after command");
    // Disc-present PAUSE status report: CR1 = 0x0100 (status 0x01 in the
    // high byte, options/repcnt 0).
    let (cr1, _) = sat.bus.read16(CR1, AccessKind::Data);
    assert_eq!(cr1, 0x0100);
}

#[test]
fn hirq_write_and_to_clear_then_command_relatches_cmok_via_the_bus() {
    let mut sat = Saturn::with_blank_bios();
    // Clear CMOK by writing a word with the CMOK bit zeroed (write-AND).
    sat.bus.write16(HIRQ, !0x0001u16, AccessKind::Data);
    let (hirq, _) = sat.bus.read16(HIRQ, AccessKind::Data);
    assert_eq!(hirq & 1, 0, "CMOK cleared");
    // Issue a command; CR4 write executes it and re-sets CMOK.
    sat.bus.write16(CR1, 0x0000, AccessKind::Data);
    sat.bus.write16(CR4, 0x0000, AccessKind::Data);
    let (hirq, _) = sat.bus.read16(HIRQ, AccessKind::Data);
    assert_eq!(hirq & 1, 1, "command must re-set CMOK");
}

#[test]
fn cd_block_does_not_collide_with_scu_or_vdp2() {
    let mut sat = Saturn::with_blank_bios();
    sat.bus.write32(0x05FE_0000, 0xAAAA_BBBB, AccessKind::Data); // SCU D0R
    sat.bus.write32(0x05E0_0000 + 0x100, 0xCCCC_DDDD, AccessKind::Data); // VDP2 VRAM
    // Writing CR1 alone (no CR4) does not trigger a command, so it holds.
    sat.bus.write16(CR1, 0xEEEE, AccessKind::Data);

    let (scu_d0r, _) = sat.bus.read32(0x05FE_0000, AccessKind::Data);
    assert_eq!(scu_d0r, 0xAAAA_BBBB);
    let (vdp2_vram, _) = sat.bus.read32(0x05E0_0000 + 0x100, AccessKind::Data);
    assert_eq!(vdp2_vram, 0xCCCC_DDDD);
    let (cd_cr1, _) = sat.bus.read16(CR1, AccessKind::Data);
    assert_eq!(cd_cr1, 0xEEEE);
}

#[test]
fn addresses_past_the_window_open_bus_through_abus_stub() {
    let mut sat = Saturn::with_blank_bios();
    // CD_BLOCK_END is 0x0589_FFFF. 0x058A_0000 is beyond it — but that's
    // SCSP sound RAM now; 0x0590_0000 is the broader A/B-bus stub (reads 0).
    let (v, _) = sat.bus.read32(0x0590_0000, AccessKind::Data);
    assert_eq!(v, 0);
    let _ = CD_BLOCK_END;
}
