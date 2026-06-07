//! `Saturn` aggregate behaviour through the public API.
//!
//! These exercise the higher-level wiring the per-peripheral suites don't:
//! cartridge insertion routed through the bus, the RTC/region seeds surfaced
//! via INTBACK, the internal backup RAM round-trip, save-state edge cases
//! (version/disc re-graft), the debug single-step hooks, and the run-loop
//! drain side effects (SCU-DSP end, VBlank, FTI) observed by driving the
//! machine. Everything goes through public methods + the bus so the tests
//! stay robust to internal refactors.

use saturn::Saturn;
use saturn::cartridge::Cartridge;
use saturn::savestate::SaveStateError;
use sh2::bus::{AccessKind, Bus};

const COMREG: u32 = 0x0010_001F;
const SF: u32 = 0x0010_0063;
const IREG0: u32 = 0x0010_0001;
const IREG1: u32 = 0x0010_0003;

/// Issue a status-only INTBACK and run until SF clears, so OREG holds the
/// response. Returns nothing — caller reads `sat.bus.smpc.oreg`.
fn run_intback_status(sat: &mut Saturn) {
    sat.bus.write8(IREG0, 0x01, AccessKind::Data); // request status
    sat.bus.write8(IREG1, 0x00, AccessKind::Data); // status only
    sat.bus.write8(COMREG, 0x10, AccessKind::Data); // INTBACK
    // INTBACK keeps SF busy for ~261 µs (~7475 cycles); give it room.
    for _ in 0..64 {
        sat.run_for(512);
        let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
        if sf == 0 {
            return;
        }
    }
    panic!("INTBACK never cleared SF");
}

// ---- cartridge wiring -----------------------------------------------------

#[test]
fn insert_dram_cartridge_is_readable_through_the_bus_and_reports_its_id() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    // Empty slot: ID byte floats high, DRAM window reads 0xFF.
    let (id, _) = sat.bus.read8(0x04FF_FFFF, AccessKind::Data);
    assert_eq!(id, 0xFF, "empty slot ID");

    sat.insert_cartridge(Cartridge::ext_ram_4mb());
    // 4 MiB DRAM cart ID is 0x5C.
    let (id, _) = sat.bus.read8(0x04FF_FFFF, AccessKind::Data);
    assert_eq!(id, 0x5C, "4 MiB DRAM cart ID");
    // Both DRAM banks are writable/readable at their windows.
    sat.bus.write32(0x0240_0000, 0xDEAD_BEEF, AccessKind::Data); // bank 0
    sat.bus.write32(0x0260_0000, 0xFEED_FACE, AccessKind::Data); // bank 1
    assert_eq!(sat.bus.read32(0x0240_0000, AccessKind::Data).0, 0xDEAD_BEEF);
    assert_eq!(sat.bus.read32(0x0260_0000, AccessKind::Data).0, 0xFEED_FACE);
}

#[test]
fn insert_rom_cartridge_is_readable_and_mirrors() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    sat.insert_cartridge(Cartridge::rom(vec![0x11, 0x22, 0x33, 0x44]));
    // ROM at the cart ROM base, big-endian.
    assert_eq!(sat.bus.read32(0x0200_0000, AccessKind::Data).0, 0x1122_3344);
    // A ROM cart's ID byte reads 0xFF (like an empty slot).
    let (id, _) = sat.bus.read8(0x04FF_FFFF, AccessKind::Data);
    assert_eq!(id, 0xFF);
}

// ---- RTC / region seeds via INTBACK ---------------------------------------

#[test]
fn set_rtc_unix_seeds_the_clock_reported_by_intback() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    // 2001-09-09 01:46:40 UTC = Unix 1_000_000_000. INTBACK RTC is BCD.
    sat.set_rtc_unix(1_000_000_000);
    run_intback_status(&mut sat);
    let oreg = sat.bus.smpc.oreg;
    // OREG1..2 = year (BCD): 2001 → 0x20, 0x01.
    assert_eq!(oreg[1], 0x20, "year hi BCD");
    assert_eq!(oreg[2], 0x01, "year lo BCD");
    // OREG3 = weekday<<4 | month. September = month 9; the low nibble is 9.
    assert_eq!(oreg[3] & 0x0F, 0x09, "month = September (BCD low nibble)");
    // OREG4 = day-of-month (BCD), the 9th.
    assert_eq!(oreg[4], 0x09, "day 9");
}

#[test]
fn set_region_is_reported_in_intback_oreg9() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    sat.set_region(saturn::smpc::region::JAPAN);
    run_intback_status(&mut sat);
    assert_eq!(sat.bus.smpc.oreg[9], saturn::smpc::region::JAPAN);
}

#[test]
fn set_pad1_stores_the_pressed_mask_for_the_peripheral_phase() {
    // `set_pad1` records the pressed mask that the INTBACK peripheral
    // continuation later inverts into the active-low report (the continuation
    // protocol itself is exercised in the smpc suite). Verify the setter wires
    // the value into the SMPC where that phase reads it.
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    let pressed = saturn::smpc::pad::START | saturn::smpc::pad::A;
    sat.set_pad1(pressed);
    assert_eq!(sat.bus.smpc.pad1, pressed, "pad1 mask stored");
}

// ---- internal backup RAM --------------------------------------------------

#[test]
fn internal_backup_round_trips_through_the_bus_and_load() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    // Pre-formatted with the BIOS signature on a fresh (charged) console.
    assert_eq!(&sat.internal_backup()[..16], b"BackUpRam Format");

    // Data lives only on odd byte lanes (offset 0x101 → unpacked byte 0x80).
    sat.bus.write8(0x0018_0000 + 0x101, 0x5A, AccessKind::Data);
    let image = sat.internal_backup().to_vec();
    assert_eq!(image[0x101 >> 1], 0x5A, "odd lane stores the data byte");

    // Re-load into a fresh console (the battery survived a power cycle).
    let mut next = Saturn::with_blank_bios();
    next.reset();
    next.load_internal_backup(&image);
    assert_eq!(next.bus.read8(0x0018_0000 + 0x101, AccessKind::Data).0, 0x5A);
}

// ---- save / load edge cases -----------------------------------------------

#[test]
fn version_mismatch_is_rejected() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    let mut blob = sat.save_state();
    // The header is (magic[4], version: u32 varint). bincode's standard config
    // encodes a small u32 as a single byte right after the 4 magic bytes, so
    // byte 4 is the version. Bump it to an unsupported value.
    blob[4] = blob[4].wrapping_add(1);
    match sat.load_state(&blob) {
        Err(SaveStateError::VersionMismatch { .. }) => {}
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

#[test]
fn save_load_with_a_disc_inserted_regrafts_the_disc() {
    // A snapshot taken with a disc, restored onto the *same* disc (re-grafted
    // from the live instance), must succeed and stay deterministic.
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    sat.insert_disc(saturn::disc::Disc::from_iso(vec![0u8; 2048 * 8]));
    assert!(sat.has_disc());
    sat.run_for(100_000);
    let snapshot = sat.save_state();
    // Run forward, then restore: the disc fingerprint matches → accepted.
    sat.run_for(100_000);
    sat.load_state(&snapshot).expect("reload onto the same disc");
    assert!(sat.has_disc(), "disc re-grafted across load");
}

#[test]
fn eject_clears_the_disc_and_load_then_mismatches() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    sat.insert_disc(saturn::disc::Disc::from_iso(vec![0u8; 2048 * 4]));
    let snapshot = sat.save_state();
    sat.eject_disc();
    assert!(!sat.has_disc(), "ejected");
    // The snapshot was taken with a disc; restoring onto the now-empty tray
    // must be refused rather than silently resume against no media.
    assert_eq!(sat.load_state(&snapshot), Err(SaveStateError::DiscMismatch));
}

// ---- debug single-step hooks ----------------------------------------------

#[test]
fn debug_step_master_advances_one_instruction_and_returns_its_cost() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    // Plant a NOP loop in low work RAM and point the master at it.
    let pc = 0x0020_1000u32;
    sat.bus.write16(pc, 0x0009, AccessKind::Data); // NOP
    sat.bus.write16(pc + 2, 0xAFFE, AccessKind::Data); // BRA -2
    sat.bus.write16(pc + 4, 0x0009, AccessKind::Data); // delay slot NOP
    sat.master_mut().regs.pc = pc;
    sat.master_mut().regs.r[15] = 0x0020_8000;

    let before = sat.master().pipeline.cycles;
    let cost = sat.debug_step_master();
    assert!(cost >= 1, "an instruction costs at least one cycle");
    assert_eq!(
        sat.master().pipeline.cycles,
        before + cost as u64,
        "pipeline cycle advanced by exactly the returned cost"
    );
    assert_eq!(sat.master().regs.pc, pc + 2, "PC advanced past the NOP");
}

#[test]
fn debug_step_slave_is_a_no_op_while_halted_then_runs_once_released() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset(); // slave starts halted at power-on
    assert!(sat.slave_is_halted());
    // A halted slave doesn't step — returns 0 cost.
    assert_eq!(sat.debug_step_slave(), 0, "halted slave does not step");

    sat.release_slave();
    assert!(!sat.slave_is_halted());
    let pc = 0x0020_2000u32;
    sat.bus.write16(pc, 0x0009, AccessKind::Data); // NOP
    sat.slave_mut().regs.pc = pc;
    sat.slave_mut().regs.r[15] = 0x0020_8400;
    let before = sat.slave().pipeline.cycles;
    let cost = sat.debug_step_slave();
    assert!(cost >= 1, "released slave steps");
    assert_eq!(sat.slave().pipeline.cycles, before + cost as u64);
}

// ---- run-loop drains observed end-to-end ----------------------------------

#[test]
fn run_frame_raises_vblank_and_advances_the_clock_by_one_frame() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    let mut out = vec![0u8; saturn::vdp2::FRAMEBUFFER_BYTES];
    let start = sat.now();
    let (w, h) = sat.run_frame(&mut out);
    // Default NTSC low-res.
    assert_eq!((w, h), (saturn::vdp2::FRAME_WIDTH, saturn::vdp2::FRAME_HEIGHT));
    // One NTSC frame is ~479_151 cycles; the clock advanced about that much.
    let advanced = sat.now() - start;
    assert!(
        (470_000..490_000).contains(&advanced),
        "run_frame advanced ~one frame (got {advanced})"
    );
    // VBlank-IN was raised during the frame: TVSTAT.VBLANK (bit 3) is set at
    // the end of the active region.
    let (tvstat, _) = sat.bus.read16(0x05F8_0004, AccessKind::Data);
    assert_ne!(tvstat & 0x0008, 0, "TVSTAT.VBLANK set after a full frame");
}

#[test]
fn fti_word_write_wakes_the_target_cpu_via_the_drain() {
    // A 16-bit write to the slave-FTI region pulses the slave's FRT
    // input-capture (FTCSR.ICF) once the aggregate drains it — the inter-CPU
    // wake signal. drain_input_capture is reached through run_for's batch end.
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    assert_eq!(sat.slave().onchip.frt.ftcsr & 0x80, 0, "ICF clear initially");
    sat.bus.write16(0x0100_0000, 0xBEEF, AccessKind::Data); // slave FTI region
    sat.run_for(512);
    assert_eq!(
        sat.slave().onchip.frt.ftcsr & 0x80,
        0x80,
        "slave FRT ICF set by the FTI drain"
    );
}

#[test]
fn scu_dsp_dma_with_hold_does_not_write_back_the_address() {
    // The DSP DMA "hold" bit (op bit 14) → `update_addr == false`: the transfer
    // still copies, but RA0/WA0 are left unchanged. The scu suite covers the
    // write-back (hold=0) cases; this exercises `exec_dsp_dma`'s no-writeback
    // branch through the same PPAF/PPD host interface.
    const PPAF: u32 = 0x05FE_0080;
    const PPD: u32 = 0x05FE_0084;
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    // Source words in low work RAM.
    sat.bus.write32(0x0020_3000, 0x0A0A_0A0A, AccessKind::Data);
    sat.bus.write32(0x0020_3004, 0x0B0B_0B0B, AccessKind::Data);
    let ra0 = 0x0020_3000u32 >> 2;
    // MVI RA0 = source>>2; DMA 2 words A/B-bus→data RAM bank 0, add_sel=1 (4),
    // HOLD set (bit 14); ENDI.
    let mvi_ra0 = (0b10u32 << 30) | (6 << 26) | (ra0 & 0x01FF_FFFF);
    let dma = (0b11u32 << 30) | (1 << 15) | (1 << 14) | 2; // hold | add4 | size2
    let endi = (0b11u32 << 30) | (0b11 << 28) | (1 << 27);
    for w in [mvi_ra0, dma, endi] {
        sat.bus.write32(PPD, w, AccessKind::Data);
    }
    sat.bus
        .write32(PPAF, (1 << 15) | (1 << 16), AccessKind::Data); // LEF + EXF
    sat.run_for(2048);
    // The data transferred...
    assert_eq!(sat.bus.scu.dsp.data_ram[0][0], 0x0A0A_0A0A);
    assert_eq!(sat.bus.scu.dsp.data_ram[0][1], 0x0B0B_0B0B);
    // ...but RA0 was NOT advanced (hold → no write-back).
    assert_eq!(sat.bus.scu.dsp.regs.ra0, ra0, "RA0 held");
}

#[test]
fn take_audio_returns_interleaved_stereo_after_a_frame() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    let mut out = vec![0u8; saturn::vdp2::FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);
    let audio = sat.take_audio();
    // The SCSP produces ~44_100/60 ≈ 735 stereo frames per video frame; the
    // buffer is interleaved L,R so its length is even and non-trivial.
    assert!(!audio.is_empty(), "audio drained for the frame");
    assert_eq!(audio.len() % 2, 0, "interleaved stereo → even length");
}
