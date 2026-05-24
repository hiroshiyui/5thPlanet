//! CD-block stub reachable through the Saturn bus (M4 task #2).
//!
//! Doesn't test CD-block behaviour beyond what the unit tests cover —
//! just confirms the bus dispatch routes correctly into the new
//! module without colliding with neighbouring regions (SCU, VDP2,
//! abus_bbus stub).

use saturn::Saturn;
use saturn::cd_block::{CD_BLOCK_BASE, CD_BLOCK_END};
use sh2::bus::{AccessKind, Bus};

#[test]
fn hirq_initial_signals_a_drive_in_no_disc_state() {
    let mut sat = Saturn::with_blank_bios();
    let (hirq, _) = sat.bus.read16(CD_BLOCK_BASE, AccessKind::Data);
    // CMOK | DRDY | DCHG — drive present, accepting commands, recent change.
    assert_eq!(hirq, 0x0023);
}

#[test]
fn cr_register_writes_round_trip_through_the_bus() {
    let mut sat = Saturn::with_blank_bios();
    sat.bus.write16(CD_BLOCK_BASE + 0x18, 0x1234, AccessKind::Data); // CR1
    sat.bus.write16(CD_BLOCK_BASE + 0x24, 0xCAFE, AccessKind::Data); // CR4
    let (cr1, _) = sat.bus.read16(CD_BLOCK_BASE + 0x18, AccessKind::Data);
    let (cr4, _) = sat.bus.read16(CD_BLOCK_BASE + 0x24, AccessKind::Data);
    assert_eq!(cr1, 0x1234);
    assert_eq!(cr4, 0xCAFE);
}

#[test]
fn writing_a_command_relatches_hirq_cmok_via_the_bus() {
    let mut sat = Saturn::with_blank_bios();
    // Clear CMOK via W1C.
    sat.bus.write16(CD_BLOCK_BASE, 0x0001, AccessKind::Data);
    let (hirq, _) = sat.bus.read16(CD_BLOCK_BASE, AccessKind::Data);
    assert_eq!(hirq & 1, 0);
    // Now write CR1; CMOK should re-latch.
    sat.bus.write16(CD_BLOCK_BASE + 0x18, 0x0000, AccessKind::Data);
    let (hirq, _) = sat.bus.read16(CD_BLOCK_BASE, AccessKind::Data);
    assert_eq!(hirq & 1, 1, "CR write must re-set CMOK");
}

#[test]
fn cd_block_does_not_collide_with_scu_or_vdp2() {
    let mut sat = Saturn::with_blank_bios();
    // Write a sentinel pattern into both neighbours.
    sat.bus.write32(0x05FE_0000, 0xAAAA_BBBB, AccessKind::Data); // SCU D0R
    sat.bus.write32(0x05E0_0000 + 0x100, 0xCCCC_DDDD, AccessKind::Data); // VDP2 VRAM
    sat.bus.write16(CD_BLOCK_BASE + 0x18, 0xEEEE, AccessKind::Data); // CD CR1

    // Each readback hits only its own region.
    let (scu_d0r, _) = sat.bus.read32(0x05FE_0000, AccessKind::Data);
    assert_eq!(scu_d0r, 0xAAAA_BBBB);
    let (vdp2_vram, _) = sat.bus.read32(0x05E0_0000 + 0x100, AccessKind::Data);
    assert_eq!(vdp2_vram, 0xCCCC_DDDD);
    let (cd_cr1, _) = sat.bus.read16(CD_BLOCK_BASE + 0x18, AccessKind::Data);
    assert_eq!(cd_cr1, 0xEEEE);
}

#[test]
fn addresses_past_the_window_open_bus_through_abus_stub() {
    let mut sat = Saturn::with_blank_bios();
    // CD_BLOCK_END is 0x0589_FFFF. 0x0590_0000 is beyond it — should fall
    // through to the broader A/B-bus stub and read as 0.
    let (v, _) = sat.bus.read32(CD_BLOCK_END + 1, AccessKind::Data);
    assert_eq!(v, 0);
}
