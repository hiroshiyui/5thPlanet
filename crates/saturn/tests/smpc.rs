//! SMPC integration through the Saturn aggregate (M3 task #1).
//!
//! Verifies that:
//!   * `Saturn::reset()` halts the slave (matches power-on hardware behaviour)
//!   * A write to SMPC COMREG ripples through `Saturn::run_for` into the
//!     slave-control side effect (SSHON releases, SSHOFF re-halts)
//!   * A halted slave never advances its `pipeline.cycles`
//!   * SF (status flag) drops back to 0 once Saturn processes the command,
//!     so polling software sees "not busy" and unblocks.

use saturn::{Saturn, SmpcCommand};
use sh2::bus::{AccessKind, Bus};

// SMPC register absolute addresses (SMPC_BASE = 0x00100000).
const COMREG: u32 = 0x0010_001F;
const SR: u32 = 0x0010_0061;
const SF: u32 = 0x0010_0063;

fn build() -> Saturn {
    // Plant the BIOS reset vector so reset() leaves both CPUs at
    // predictable PC/SP rather than 0.
    let mut bios = vec![0u8; 512 * 1024];
    bios[0..4].copy_from_slice(&0x0020_1000u32.to_be_bytes());
    bios[4..8].copy_from_slice(&0x0020_8000u32.to_be_bytes());
    let mut sat = Saturn::new(bios);
    sat.reset();
    // Plant a tight NOP loop in low WRAM so both CPUs have something to
    // run when released.
    for i in 0..16u32 {
        sat.bus.low_wram.write16(0x1000 + i * 2, 0x0009);
    }
    sat
}

#[test]
fn reset_leaves_slave_halted_and_master_running() {
    let sat = build();
    assert!(sat.slave_is_halted(), "slave starts halted on reset");
    assert!(!sat.master_is_halted(), "master runs from reset");
}

#[test]
fn halted_slave_does_not_advance_during_run() {
    let mut sat = build();
    let before = sat.slave().pipeline.cycles;
    sat.run_for(1000);
    assert_eq!(
        sat.slave().pipeline.cycles,
        before,
        "halted slave must not advance"
    );
    assert!(sat.master().pipeline.cycles >= 1000, "master kept running");
}

#[test]
fn sshon_command_released_by_run_for_releases_slave() {
    let mut sat = build();
    assert!(sat.slave_is_halted());
    // Software writes SSHON (0x02) to COMREG.
    sat.bus
        .write8(COMREG, SmpcCommand::SshOn as u8, AccessKind::Data);
    // After SMPC's poll quantum elapses inside run_for, the slave
    // should be released and SF should drop.
    sat.run_for(512);
    assert!(
        !sat.slave_is_halted(),
        "SSHON should have released the slave"
    );
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF dropped after command processed");
    assert!(sat.slave().pipeline.cycles > 0, "slave actually stepped");
}

#[test]
fn sshoff_re_halts_a_running_slave() {
    let mut sat = build();
    sat.release_slave();
    // Let the slave accumulate some cycles.
    sat.run_for(256);
    let after_release = sat.slave().pipeline.cycles;
    assert!(after_release > 0);
    // Now request SSHOFF.
    sat.bus.write8(COMREG, 0x03, AccessKind::Data); // SSHOFF
    sat.run_for(512);
    assert!(sat.slave_is_halted(), "SSHOFF should have re-halted slave");
    let frozen_at = sat.slave().pipeline.cycles;
    sat.run_for(512);
    assert_eq!(
        sat.slave().pipeline.cycles,
        frozen_at,
        "slave cycles frozen while halted"
    );
}

#[test]
fn unknown_comreg_command_does_not_set_sf() {
    let mut sat = build();
    sat.bus.write8(COMREG, 0xFE, AccessKind::Data);
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "unknown commands don't go busy");
    assert_eq!(sat.bus.smpc.last_unknown_command, Some(0xFE));
}

#[test]
fn settime_sets_the_clock_reported_by_intback() {
    let mut sat = build();
    // SETTIME IREG: 2001-09-11, 13:46:00. IREG layout matches the INTBACK RTC
    // bytes (year-hi, year-lo, weekday|month, day, hour, minute, second).
    for (off, val) in [
        (0x01u32, 0x20), // year hi
        (0x03, 0x01),    // year lo → 2001
        (0x05, 0x09),    // month 9 (weekday nibble is recomputed)
        (0x07, 0x11),    // day 11
        (0x09, 0x13),    // hour 13
        (0x0B, 0x46),    // minute 46
        (0x0D, 0x00),    // second 0
    ] {
        sat.bus.write8(0x0010_0000 + off, val, AccessKind::Data);
    }
    sat.bus.write8(COMREG, 0x16, AccessKind::Data); // SETTIME
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 1, "SF goes busy on queue");
    sat.run_for(512);
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF drops after SETTIME");

    // Read it back via an INTBACK status request.
    sat.bus.write8(0x0010_0001, 0x01, AccessKind::Data); // IREG0: status
    sat.bus.write8(0x0010_0003, 0x00, AccessKind::Data); // IREG1: status only
    sat.bus.write8(COMREG, 0x10, AccessKind::Data); // INTBACK
    sat.run_for(16_000);
    let o = &sat.bus.smpc.oreg;
    assert_eq!(o[1], 0x20, "RTC year hi");
    assert_eq!(o[2], 0x01, "RTC year lo");
    assert_eq!(o[3] & 0x0F, 0x09, "RTC month");
    assert_eq!(o[3] >> 4, 2, "weekday recomputed: 2001-09-11 = Tuesday");
    assert_eq!(o[4], 0x11, "RTC day");
    assert_eq!(o[5], 0x13, "RTC hour");
    assert_eq!(o[6], 0x46, "RTC minute");
}

#[test]
fn region_code_is_configurable_and_reported_by_intback() {
    let mut sat = build();
    sat.set_region(saturn::smpc::region::EUROPE_PAL);
    sat.bus.write8(0x0010_0001, 0x01, AccessKind::Data); // IREG0: status
    sat.bus.write8(COMREG, 0x10, AccessKind::Data); // INTBACK
    sat.run_for(16_000);
    assert_eq!(sat.bus.smpc.oreg[9], saturn::smpc::region::EUROPE_PAL);
}

#[test]
fn ireg_oreg_round_trip_through_the_bus() {
    let mut sat = build();
    // IREG0 at offset 0x01, OREG0 at offset 0x21.
    sat.bus.write8(0x0010_0001, 0xAB, AccessKind::Data);
    sat.bus.write8(0x0010_0021, 0xCD, AccessKind::Data);
    let (ireg0, _) = sat.bus.read8(0x0010_0001, AccessKind::Data);
    let (oreg0, _) = sat.bus.read8(0x0010_0021, AccessKind::Data);
    assert_eq!(ireg0, 0xAB);
    assert_eq!(oreg0, 0xCD);
}

#[test]
fn nmireq_raises_nmi_on_master_sh2_through_run_for() {
    let mut sat = build();
    // Replace the NOP train at PC0 with a tight BRA-self so the master
    // doesn't run off into uninitialised memory and cascade vector-4
    // illegal-instruction dispatches (which would also push SR+PC and
    // muddy the signal we're testing).
    sat.bus.low_wram.write16(0x1000, 0xAFFE); // BRA -2
    sat.bus.low_wram.write16(0x1002, 0x0009); // NOP slot
    sat.master_mut().regs.pc = 0x0020_1000;

    // Install a real NMI handler so the all-zero default doesn't itself
    // cascade. Point VBR at low WRAM and put a self-loop at vector 11.
    sat.master_mut().regs.vbr = 0x0020_4000;
    let handler = 0x0020_5000;
    sat.bus
        .write32(0x0020_4000 + 11 * 4, handler, AccessKind::Data);
    sat.bus.write16(handler, 0xAFFE, AccessKind::Data);
    sat.bus.write16(handler + 2, 0x0009, AccessKind::Data);

    let sp_before = sat.master().regs.r[15];
    sat.bus.write8(COMREG, 0x18, AccessKind::Data); // NMIREQ
    sat.run_for(512);
    let sp_after = sat.master().regs.r[15];

    // NMI is the only thing that pushes SR+PC unprompted; if SP dropped
    // by exactly 8 the master vectored through once. (Once dispatched,
    // the pending bit clears, so checking `next_pending` directly is
    // racy — the stack-frame side effect is the durable signal.)
    assert_eq!(
        sp_before.wrapping_sub(sp_after),
        8,
        "NMI should have pushed SR + PC = 8 bytes onto master stack"
    );
    // And master should be inside the handler now.
    assert!(
        sat.master().regs.pc == handler || sat.master().regs.pc == handler + 2,
        "master should be executing the NMI handler, got PC=0x{:08X}",
        sat.master().regs.pc
    );
}

#[test]
fn intback_status_phase_fills_oreg_and_raises_smpc_source() {
    let mut sat = build();
    // Status-only INTBACK (IREG1 & 8 == 0 → no peripheral phase).
    sat.bus.write8(0x0010_0001, 0x01, AccessKind::Data); // IREG0: request status
    sat.bus.write8(COMREG, 0x10, AccessKind::Data); // INTBACK
    sat.run_for(16_000);
    // SF clears once the status phase completes.
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF clears after INTBACK status phase");
    // RTC is OREG1..7; OREG9 = area code (North America NTSC = 0x04);
    // OREG10 = system status 1 (0x34, MAME); OREG31 = command echo (0x10).
    assert_eq!(sat.bus.smpc.oreg[7], 0x00);
    assert_eq!(sat.bus.smpc.oreg[9], 0x04);
    assert_eq!(sat.bus.smpc.oreg[10], 0x34);
    assert_eq!(sat.bus.smpc.oreg[31], 0x10);
    // Status SR with no peripheral requested = 0x0F (Mednafen `(SR&~0x80&~NPE)
    // | 0x0F`; NPE/0x20 only set when peripheral data is also requested).
    let (sr, _) = sat.bus.read8(SR, AccessKind::Data);
    assert_eq!(sr, 0x0F);
    // SCU's SMPC source is the path BIOS handlers wait on. The SCU resets with
    // every source masked (IMS=0xBFFF), as the BIOS expects; unmask **only** the
    // SMPC source here to confirm INTBACK raised it (HBlank-IN also fires across
    // these scanlines and out-ranks it, so leave the rest masked to isolate it).
    sat.bus.scu.ims = 0xFFFF & !(1 << saturn::ScuSource::Smpc.bit());
    let pending = sat.bus.scu.take_pending_interrupt(0);
    assert_eq!(pending.map(|(s, _)| s), Some(saturn::ScuSource::Smpc));
}

#[test]
fn intback_peripheral_continuation_reports_the_digital_pad() {
    let mut sat = build();
    // Hold Start + Left on port 1.
    sat.set_pad1(saturn::smpc::pad::START | saturn::smpc::pad::LEFT);
    // Request status + peripheral data (IREG1 bit 3 set).
    sat.bus.write8(0x0010_0001, 0x01, AccessKind::Data); // IREG0: status
    sat.bus.write8(0x0010_0003, 0x08, AccessKind::Data); // IREG1: peripheral
    sat.bus.write8(COMREG, 0x10, AccessKind::Data); // INTBACK
    sat.run_for(40_000);
    // Status phase done: SR = 0x0F | NPE(0x20) = 0x2F (peripheral pending).
    let (sr, _) = sat.bus.read8(SR, AccessKind::Data);
    assert_eq!(sr, 0x2F, "status SR signals peripheral data pending");
    // Host requests CONTINUE (IREG0 bit 0x80) → peripheral phase.
    sat.bus.write8(0x0010_0001, 0x80, AccessKind::Data);
    sat.run_for(40_000);
    // Port 1: a directly-connected standard digital pad (0xF1, ID 0x02),
    // active-low data with Start (bit 3) and Left (bit 6) of byte 1 held.
    assert_eq!(sat.bus.smpc.oreg[0], 0xF1, "port 1: 1 device, direct");
    assert_eq!(sat.bus.smpc.oreg[1], 0x02, "standard digital pad");
    assert_eq!(
        sat.bus.smpc.oreg[2], !0x48,
        "Start + Left held (active low)"
    );
    assert_eq!(sat.bus.smpc.oreg[3], 0xFF, "no second-byte buttons held");
    assert_eq!(sat.bus.smpc.oreg[4], 0xF0, "port 2: no peripheral");
    let (sr, _) = sat.bus.read8(SR, AccessKind::Data);
    assert_eq!(sr & 0xC0, 0xC0, "first peripheral phase: more data");
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF clears after the peripheral phase completes");
}

#[test]
fn intback_peripheral_only_returns_the_pad_directly_without_a_continue() {
    // A peripheral-only INTBACK — IREG0 low nibble 0 (no status acquisition),
    // IREG1 & 8 (peripheral) — is what Panzer Dragoon Zwei issues every frame to
    // read the pad. Mednafen (smpc.cpp:1217/1250) runs the status phase ONLY if
    // `IREG0 & 0xF`, and the peripheral phase's continue-wait runs ONLY if
    // SR_NPE is set (which only the status phase sets). So with no status, the
    // peripheral report lands DIRECTLY in OREG0.. with NO continue handshake.
    // (The old handler always returned the status phase — OREG0 = 0x80 — and
    // waited for a CONTINUE PDZ never sends, so it saw "no controller".)
    let mut sat = build();
    sat.set_pad1(saturn::smpc::pad::START);
    sat.bus.write8(0x0010_0001, 0x00, AccessKind::Data); // IREG0: no status
    sat.bus.write8(0x0010_0003, 0x08, AccessKind::Data); // IREG1: peripheral
    sat.bus.write8(COMREG, 0x10, AccessKind::Data); // INTBACK
    sat.run_for(40_000);
    // The pad report is at OREG0 immediately — NOT the 0x80 status byte.
    assert_eq!(
        sat.bus.smpc.oreg[0], 0xF1,
        "port 1: 1 device, direct (not status 0x80)"
    );
    assert_eq!(sat.bus.smpc.oreg[1], 0x02, "standard digital pad");
    assert_eq!(sat.bus.smpc.oreg[2], !0x08, "Start held (active low)");
    assert_eq!(sat.bus.smpc.oreg[3], 0xFF, "no second-byte buttons");
    assert_eq!(sat.bus.smpc.oreg[4], 0xF0, "port 2: no peripheral");
    // No continue is pending, and SF has dropped — the data is ready in one shot.
    assert_eq!(sat.bus.smpc.intback_stage, 0, "no staged continue expected");
    let (sr, _) = sat.bus.read8(SR, AccessKind::Data);
    assert_eq!(sr & 0x80, 0x80, "SR signals data ready");
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF clears after the one-shot peripheral phase");
}

#[test]
fn setsmem_writes_smem_echoed_by_intback_oreg12_15() {
    let mut sat = build();
    // SETSMEM takes the four bytes from IREG0..3.
    sat.bus.write8(0x0010_0001, 0xDE, AccessKind::Data); // IREG0
    sat.bus.write8(0x0010_0003, 0xAD, AccessKind::Data); // IREG1
    sat.bus.write8(0x0010_0005, 0xBE, AccessKind::Data); // IREG2
    sat.bus.write8(0x0010_0007, 0xEF, AccessKind::Data); // IREG3
    sat.bus.write8(COMREG, 0x17, AccessKind::Data); // SETSMEM
    sat.run_for(512);
    assert_eq!(
        sat.bus.smpc.smem,
        [0xDE, 0xAD, 0xBE, 0xEF],
        "SETSMEM stored SMEM"
    );
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF drops after SETSMEM");

    // INTBACK status echoes SMEM in OREG12..15.
    sat.bus.write8(0x0010_0001, 0x01, AccessKind::Data); // IREG0: status
    sat.bus.write8(0x0010_0003, 0x00, AccessKind::Data); // IREG1: status only
    sat.bus.write8(COMREG, 0x10, AccessKind::Data); // INTBACK
    sat.run_for(16_000);
    assert_eq!(&sat.bus.smpc.oreg[12..16], &[0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn sndon_releases_the_sound_cpu_and_sndoff_re_holds_it() {
    let mut sat = build();
    assert!(!sat.bus.scsp.running, "sound CPU held at reset");
    sat.bus.write8(COMREG, 0x06, AccessKind::Data); // SNDON
    sat.run_for(512);
    assert!(sat.bus.scsp.running, "SNDON released the SCSP 68k");
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF drops after SNDON");

    sat.bus.write8(COMREG, 0x07, AccessKind::Data); // SNDOFF
    sat.run_for(512);
    assert!(!sat.bus.scsp.running, "SNDOFF re-held the SCSP 68k");
}

#[test]
fn ckchg_halts_the_slave_and_nmis_the_master() {
    let mut sat = build();
    // Pin the master in a tight BRA-self with an NMI handler installed, so the
    // CKCHG NMI is the only thing that pushes a stack frame (same setup as the
    // NMIREQ test).
    sat.bus.low_wram.write16(0x1000, 0xAFFE); // BRA -2
    sat.bus.low_wram.write16(0x1002, 0x0009); // NOP slot
    sat.master_mut().regs.pc = 0x0020_1000;
    sat.master_mut().regs.vbr = 0x0020_4000;
    let handler = 0x0020_5000;
    sat.bus
        .write32(0x0020_4000 + 11 * 4, handler, AccessKind::Data);
    sat.bus.write16(handler, 0xAFFE, AccessKind::Data);
    sat.bus.write16(handler + 2, 0x0009, AccessKind::Data);

    sat.release_slave();
    sat.run_for(256);
    assert!(!sat.slave_is_halted(), "slave running before CKCHG");
    let sp_before = sat.master().regs.r[15];

    // CKCHG320 reproduces the observable handshake: slave off + master NMI.
    sat.bus.write8(COMREG, 0x0F, AccessKind::Data); // CKCHG320
    sat.run_for(512);
    assert!(sat.slave_is_halted(), "CKCHG halts the slave");
    // The NMI pushed SR + PC = 8 bytes onto the master stack.
    assert_eq!(
        sp_before.wrapping_sub(sat.master().regs.r[15]),
        8,
        "CKCHG NMI'd the master (pushed SR+PC)"
    );
    assert!(
        sat.master().regs.pc == handler || sat.master().regs.pc == handler + 2,
        "master vectored into the NMI handler"
    );
    assert!(
        sat.bus.smpc.last_unknown_command.is_none(),
        "0x0F is CKCHG320, a known command"
    );
}

#[test]
fn intback_break_ends_the_peripheral_sequence() {
    let mut sat = build();
    sat.set_pad1(saturn::smpc::pad::A);
    // Status + peripheral request.
    sat.bus.write8(0x0010_0001, 0x01, AccessKind::Data); // IREG0: status
    sat.bus.write8(0x0010_0003, 0x08, AccessKind::Data); // IREG1: peripheral
    sat.bus.write8(COMREG, 0x10, AccessKind::Data); // INTBACK
    sat.run_for(40_000);
    let (sr, _) = sat.bus.read8(SR, AccessKind::Data);
    assert_eq!(sr, 0x2F, "status SR signals peripheral data pending");
    assert_ne!(
        sat.bus.smpc.intback_stage, 0,
        "a peripheral sequence is in progress"
    );
    // Host BREAKs (IREG0 bit 0x40) instead of CONTINUE — the sequence ends.
    sat.bus.write8(0x0010_0001, 0x40, AccessKind::Data);
    assert_eq!(sat.bus.smpc.intback_stage, 0, "BREAK ended the sequence");
    let (sr, _) = sat.bus.read8(SR, AccessKind::Data);
    assert_eq!(sr & 0xF0, 0x00, "BREAK acked the high SR nibble");
}

#[test]
fn resenab_command_0x19_is_recognised_and_drops_sf() {
    let mut sat = build();
    sat.bus.write8(COMREG, 0x19, AccessKind::Data); // RESENAB
    sat.run_for(512);
    assert!(
        sat.bus.smpc.last_unknown_command.is_none(),
        "0x19 is RESENAB (reset-button enable) — a known command"
    );
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF drops after processing");
}

#[test]
fn resdisa_command_0x1a_is_recognised_and_drops_sf() {
    let mut sat = build();
    sat.bus.write8(COMREG, 0x1A, AccessKind::Data); // RESDISA
    sat.run_for(512);
    assert!(
        sat.bus.smpc.last_unknown_command.is_none(),
        "0x1A is RESDISA (reset-button disable) — a known command, not unknown"
    );
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF drops after processing");
}

/// Shuttle Mouse on port 1 (M13 E3): the INTBACK peripheral phase reports ID
/// `0xE3` with three data bytes — `(flags << 4) | buttons`, X delta low byte,
/// Y delta low byte (Saturn Y+ = up; `Saturn::feed_mouse` takes host
/// screen-down Y and negates) — and consuming the report resets the motion
/// accumulators (Mednafen `input/mouse.cpp` / `smpc.cpp:1421`).
#[test]
fn intback_peripheral_reports_the_shuttle_mouse() {
    use saturn::smpc::{PortDevice, mouse};
    let mut sat = build();
    sat.set_port_devices(PortDevice::Mouse, PortDevice::None);
    // Move right 5, down 3 (host convention) with Left + Start held.
    sat.feed_mouse(5, 3, mouse::LEFT | mouse::START);
    sat.bus.write8(0x0010_0001, 0x01, AccessKind::Data); // IREG0: status
    sat.bus.write8(0x0010_0003, 0x08, AccessKind::Data); // IREG1: peripheral
    sat.bus.write8(COMREG, 0x10, AccessKind::Data); // INTBACK
    sat.run_for(40_000);
    sat.bus.write8(0x0010_0001, 0x80, AccessKind::Data); // CONTINUE
    sat.run_for(40_000);
    assert_eq!(sat.bus.smpc.oreg[0], 0xF1, "port 1: 1 device, direct");
    assert_eq!(sat.bus.smpc.oreg[1], 0xE3, "Shuttle Mouse peripheral ID");
    // dy is negative in Saturn convention (host +3 down = Saturn −3):
    // flags = Y-negative (bit 1); buttons = Left | Start.
    assert_eq!(
        sat.bus.smpc.oreg[2],
        (0x2 << 4) | mouse::LEFT | mouse::START,
        "flags<<4 | buttons"
    );
    assert_eq!(sat.bus.smpc.oreg[3], 5, "X delta low byte");
    assert_eq!(
        sat.bus.smpc.oreg[4],
        (-3i32 & 0xFF) as u8,
        "Y delta (up-positive)"
    );
    assert_eq!(sat.bus.smpc.oreg[5], 0xF0, "port 2: no peripheral");

    // The report consumed the deltas: a second phase reports zero motion
    // (buttons are level state, still held).
    sat.bus.write8(0x0010_0001, 0x80, AccessKind::Data); // CONTINUE (last)
    sat.run_for(40_000);
    assert_eq!(
        sat.bus.smpc.oreg[2],
        mouse::LEFT | mouse::START,
        "no motion flags"
    );
    assert_eq!(sat.bus.smpc.oreg[3], 0, "X accumulator reset");
    assert_eq!(sat.bus.smpc.oreg[4], 0, "Y accumulator reset");
}

/// Out-of-range mouse motion clamps to ±256/255 with the overflow flags set
/// (Mednafen `input/mouse.cpp` clamps and sets flags 0x4/0x8).
#[test]
fn mouse_deltas_clamp_with_overflow_flags() {
    use saturn::smpc::PortDevice;
    let mut sat = build();
    sat.set_port_devices(PortDevice::Mouse, PortDevice::None);
    sat.feed_mouse(1000, 1000, 0); // host down 1000 → Saturn −1000
    let (b1, x, y) = sat.bus.smpc.take_mouse_report();
    // X: positive overflow → clamp 255. Y: negative overflow → clamp −256.
    assert_eq!(
        b1 >> 4,
        0x4 | 0x8 | 0x2,
        "X-overflow + Y-overflow + Y-negative"
    );
    assert_eq!(x, 255);
    assert_eq!(y, (-256i32 & 0xFF) as u8);
}

/// A pad on port 1 and the mouse on port 2 lay their blocks out back-to-back
/// (the `--mouse` default keeps the keyboard pad usable).
#[test]
fn pad_on_port1_and_mouse_on_port2_pack_sequentially() {
    use saturn::smpc::{PortDevice, mouse};
    let mut sat = build();
    sat.set_port_devices(PortDevice::Pad, PortDevice::Mouse);
    sat.set_pad1(saturn::smpc::pad::START);
    sat.feed_mouse(7, 0, mouse::RIGHT);
    sat.bus.write8(0x0010_0001, 0x01, AccessKind::Data);
    sat.bus.write8(0x0010_0003, 0x08, AccessKind::Data);
    sat.bus.write8(COMREG, 0x10, AccessKind::Data);
    sat.run_for(40_000);
    sat.bus.write8(0x0010_0001, 0x80, AccessKind::Data);
    sat.run_for(40_000);
    let o = &sat.bus.smpc.oreg;
    assert_eq!(&o[0..4], &[0xF1, 0x02, !0x08, 0xFF], "port 1: pad block");
    assert_eq!(
        &o[4..9],
        &[0xF1, 0xE3, mouse::RIGHT, 7, 0],
        "port 2: mouse block"
    );
}
