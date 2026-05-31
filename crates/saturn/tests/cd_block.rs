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
    // Power-on HIRQ has only MPED (0x0800) set — the "no MPEG card" bit the
    // real CD-block (and Mednafen) holds from reset. No other bits are set
    // until an event (command / periodic). MAME instead starts at 0; we follow
    // Mednafen here since the BIOS reads MPED in every HIRQ during boot.
    assert_eq!(hirq, 0x0800, "power-on HIRQ = MPED only");
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
    // No disc inserted → NODISC status report: CR1 = 0x0700 (status 0x07 in
    // the high byte, options/repcnt 0), matching MAME's no-image reset.
    let (cr1, _) = sat.bus.read16(CR1, AccessKind::Data);
    assert_eq!(cr1, 0x0700);
}

#[test]
fn hirq_write_and_to_clear_then_command_relatches_cmok_via_the_bus() {
    let mut sat = Saturn::with_blank_bios();
    // Clear CMOK by writing a word with the CMOK bit zeroed (write-AND).
    sat.bus.write16(HIRQ, !0x0001u16, AccessKind::Data);
    let (hirq, _) = sat.bus.read16(HIRQ, AccessKind::Data);
    assert_eq!(hirq & 1, 0, "CMOK cleared");
    // Issue a command; a command requires all four CRs written, and the
    // CR4 write completes the set, executes, and re-sets CMOK.
    sat.bus.write16(CR1, 0x0000, AccessKind::Data);
    sat.bus.write16(CR2, 0x0000, AccessKind::Data);
    sat.bus.write16(CR3, 0x0000, AccessKind::Data);
    sat.bus.write16(CR4, 0x0000, AccessKind::Data);
    let (hirq, _) = sat.bus.read16(HIRQ, AccessKind::Data);
    assert_eq!(hirq & 1, 1, "command must re-set CMOK");
}

#[test]
fn insert_then_eject_round_trips_to_nodisc() {
    let mut sat = Saturn::with_blank_bios();
    // A small synthetic ISO is enough to flip the drive to disc-present.
    sat.insert_disc(saturn::disc::Disc::from_iso(vec![0u8; 2048 * 4]));
    assert!(sat.has_disc(), "disc present after insert");
    // Get Status now reports PAUSE (0x01 in the high byte).
    sat.bus.write16(CR1, 0x0000, AccessKind::Data);
    sat.bus.write16(CR2, 0x0000, AccessKind::Data);
    sat.bus.write16(CR3, 0x0000, AccessKind::Data);
    sat.bus.write16(CR4, 0x0000, AccessKind::Data);
    let (cr1, _) = sat.bus.read16(CR1, AccessKind::Data);
    assert_eq!(cr1 >> 8, 0x01, "PAUSE while a disc is loaded");

    sat.eject_disc();
    assert!(!sat.has_disc(), "no disc after eject");
    // Get Status now reports NODISC (0x07), like a cold empty drive.
    sat.bus.write16(CR1, 0x0000, AccessKind::Data);
    sat.bus.write16(CR2, 0x0000, AccessKind::Data);
    sat.bus.write16(CR3, 0x0000, AccessKind::Data);
    sat.bus.write16(CR4, 0x0000, AccessKind::Data);
    let (cr1, _) = sat.bus.read16(CR1, AccessKind::Data);
    assert_eq!(cr1 >> 8, 0x07, "NODISC after eject");
}

#[test]
fn cd_block_does_not_collide_with_scu_or_vdp2() {
    let mut sat = Saturn::with_blank_bios();
    sat.bus.write32(0x05FE_0000, 0xAAAA_BBBB, AccessKind::Data); // SCU D0R
    sat.bus
        .write32(0x05E0_0000 + 0x100, 0xCCCC_DDDD, AccessKind::Data); // VDP2 VRAM
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

/// End-to-end Phase-1 check against a real disc image, if one is present in
/// `roms/` (gitignored — copyrighted). Inserts the CloneCD boot disc, issues
/// Get TOC over the bus, and confirms the streamed TOC describes a data track 1
/// and a sensible lead-out. Skipped (passing) when no disc is available.
#[test]
fn real_ccd_disc_get_toc_over_the_bus() {
    use std::path::PathBuf;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_default();
    let ccd_path = root.join("roms/SS - Boot Disc.ccd");
    let img_path = root.join("roms/SS - Boot Disc.img");
    let (Ok(ccd), Ok(img)) = (
        std::fs::read_to_string(&ccd_path),
        std::fs::read(&img_path),
    ) else {
        println!("no roms/ boot disc; real-disc Get TOC test skipped");
        return;
    };

    let disc = saturn::disc::Disc::from_ccd(&ccd, img).expect("parse CCD/IMG");
    assert_eq!(disc.first_track(), 1);
    assert!(disc.lead_out_fad() > 150, "lead-out past the lead-in");
    assert_eq!(disc.tracks()[0].ctrl_addr(), 0x41, "track 1 is data");

    let mut sat = Saturn::with_blank_bios();
    sat.insert_disc(disc);
    // Get TOC: write CR1 = 0x0200, CR2..CR4 = 0 (CR4 triggers the command).
    sat.bus.write16(CR1, 0x0200, AccessKind::Data);
    sat.bus.write16(CR2, 0x0000, AccessKind::Data);
    sat.bus.write16(CR3, 0x0000, AccessKind::Data);
    sat.bus.write16(CR4, 0x0000, AccessKind::Data);
    let (cr2, _) = sat.bus.read16(CR2, AccessKind::Data);
    assert_eq!(cr2, 0x00CC, "TOC length = 102 words");
    // First TOC word from the data FIFO: ctrl/adr 0x41 in the high byte.
    let (w0, _) = sat.bus.read16(CD_BLOCK_BASE + 0x8000, AccessKind::Data);
    assert_eq!(w0 >> 8, 0x41, "TOC track 1 is a data track");
}

/// Phase-5 end-to-end against the real boot disc, if present: the IP sector
/// carries the "SEGA SEGASATURN" security header, and the authentication +
/// region commands report a valid Saturn disc. Skipped (passing) with no disc.
#[test]
fn real_disc_authenticates_as_a_saturn_disc() {
    use std::path::PathBuf;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_default();
    let (Ok(ccd), Ok(img)) = (
        std::fs::read_to_string(root.join("roms/SS - Boot Disc.ccd")),
        std::fs::read(root.join("roms/SS - Boot Disc.img")),
    ) else {
        println!("no roms/ boot disc; real-disc auth test skipped");
        return;
    };
    let disc = saturn::disc::Disc::from_ccd(&ccd, img).expect("parse CCD/IMG");
    // The first data sector carries the Saturn security header.
    let ip = disc.read_sector(150).expect("FAD 150 user data");
    assert_eq!(&ip[0..15], b"SEGA SEGASATURN", "Saturn IP header");

    let mut sat = Saturn::with_blank_bios();
    sat.insert_disc(disc);
    // Check copy protection (0xE0): authentication HIRQ pattern (ECPY = 0x100).
    sat.bus.write16(CR1, 0xE000, AccessKind::Data);
    sat.bus.write16(CR2, 0x0000, AccessKind::Data);
    sat.bus.write16(CR3, 0x0000, AccessKind::Data);
    sat.bus.write16(CR4, 0x0000, AccessKind::Data);
    let (hirq, _) = sat.bus.read16(HIRQ, AccessKind::Data);
    assert_ne!(hirq & 0x0100, 0, "authentication complete (ECPY)");
    // Get disc region (0xE1): 4 = Saturn data disc.
    sat.bus.write16(CR1, 0xE100, AccessKind::Data);
    sat.bus.write16(CR2, 0x0000, AccessKind::Data);
    sat.bus.write16(CR3, 0x0000, AccessKind::Data);
    sat.bus.write16(CR4, 0x0000, AccessKind::Data);
    let (region, _) = sat.bus.read16(CR2, AccessKind::Data);
    assert_eq!(region, 0x0004, "real disc reports as a Saturn disc");
}
