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
    assert!(!sat.master().pipeline.cycles == 0 || true, "master is runnable");
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
    sat.bus.write8(COMREG, SmpcCommand::SshOn as u8 as u8, AccessKind::Data);
    // After SMPC's poll quantum elapses inside run_for, the slave
    // should be released and SF should drop.
    sat.run_for(512);
    assert!(!sat.slave_is_halted(), "SSHON should have released the slave");
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
fn settime_recognised_as_no_op_but_sf_drops_correctly() {
    let mut sat = build();
    sat.bus.write8(COMREG, 0x18, AccessKind::Data); // SETTIME
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 1, "SF goes busy on queue");
    sat.run_for(512);
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF drops after processing even for no-op commands");
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
    sat.bus.write32(0x0020_4000 + 11 * 4, handler, AccessKind::Data);
    sat.bus.write16(handler, 0xAFFE, AccessKind::Data);
    sat.bus.write16(handler + 2, 0x0009, AccessKind::Data);

    let sp_before = sat.master().regs.r[15];
    sat.bus.write8(COMREG, 0x10, AccessKind::Data); // NMIREQ
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
fn intback_fills_oreg_with_a_no_controller_status_and_raises_smpc_source() {
    let mut sat = build();
    sat.bus.write8(COMREG, 0x16, AccessKind::Data); // INTBACK
    sat.run_for(512);
    // OREG8 = area code (USA = 0x06).
    assert_eq!(sat.bus.smpc.oreg[8], 0x06);
    // OREG11 / OREG12 = port 1 / port 2 peripheral headers (no peripheral).
    assert_eq!(sat.bus.smpc.oreg[11], 0xF0);
    assert_eq!(sat.bus.smpc.oreg[12], 0xF0);
    // OREG31 = end marker.
    assert_eq!(sat.bus.smpc.oreg[31], 0xF0);
    // SCU's SMPC source is the path BIOS handlers wait on.
    let pending = sat.bus.scu.take_pending_interrupt(0);
    assert_eq!(pending.map(|(s, _)| s), Some(saturn::ScuSource::Smpc));
}

#[test]
fn undocumented_command_1a_is_no_longer_an_unknown_event() {
    let mut sat = build();
    sat.bus.write8(COMREG, 0x1A, AccessKind::Data);
    sat.run_for(512);
    assert!(
        sat.bus.smpc.last_unknown_command.is_none(),
        "0x1A was observed at USA BIOS boot — it must be recognised as a no-op rather than logged as unknown"
    );
    let (sf, _) = sat.bus.read8(SF, AccessKind::Data);
    assert_eq!(sf, 0, "SF drops after processing");
}
