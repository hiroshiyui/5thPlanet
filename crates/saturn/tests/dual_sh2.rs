//! Dual-SH-2 coscheduling end-to-end test (M2 task #5).
//!
//! The canonical Saturn-flavour smoke test: master writes a sentinel
//! into shared work RAM, slave polls until it sees it. Proves that the
//! complete chain — decoder → interpreter → cache → routed memory →
//! Saturn bus → scheduler — works for two CPUs sharing real Saturn-
//! mapped memory.

use saturn::Saturn;

const MASTER_PC: u32 = 0x0020_1000;
const SLAVE_PC: u32 = 0x0020_2000;
const SHARED: u32 = 0x0600_1000;
const SENTINEL: u32 = 0xCAFE_F00D;

/// Place a sequence of 16-bit words into the bus at `addr`.
fn load(bus: &mut saturn::SaturnBus, addr: u32, words: &[u16]) {
    use sh2::bus::{AccessKind, Bus};
    for (i, w) in words.iter().enumerate() {
        bus.write16(addr + (i as u32) * 2, *w, AccessKind::Data);
    }
}

#[test]
fn master_writes_sentinel_slave_observes_it_within_budget() {
    // Master program at MASTER_PC:
    //   MOV.L R2, @R1   (write sentinel via address in R1)
    //   BRA   -2        (loop back to self; delay slot below)
    //   NOP             (BRA delay slot)
    //
    // After the one MOV.L the master spins inside BRA + NOP forever.
    //
    // Slave program at SLAVE_PC:
    //   MOV.L @R1, R10  (read shared address into R10)
    //   BRA   -3        (loop back to MOV.L; delay slot below)
    //   NOP             (BRA delay slot)
    let mut sat = Saturn::with_blank_bios();
    load(
        &mut sat.bus,
        MASTER_PC,
        &[
            0x2122, // MOV.L R2, @R1  (n=1, m=2)
            0xAFFE, // BRA -2
            0x0009, // NOP slot
        ],
    );
    load(
        &mut sat.bus,
        SLAVE_PC,
        &[
            0x6A12, // MOV.L @R1, R10  (n=10, m=1)
            0xAFFD, // BRA -3
            0x0009, // NOP slot
        ],
    );

    // Set up master: R1 = shared address, R2 = sentinel, PC = program.
    {
        let m = sat.master_mut();
        m.regs.pc = MASTER_PC;
        m.regs.r[1] = SHARED;
        m.regs.r[2] = SENTINEL;
        m.regs.r[15] = 0x0020_8000;
    }
    {
        let s = sat.slave_mut();
        s.regs.pc = SLAVE_PC;
        s.regs.r[1] = SHARED;
        s.regs.r[10] = 0;
        s.regs.r[15] = 0x0020_8400;
    }

    // Run until slave's R10 reflects the sentinel, but cap at a budget
    // so a deadlock would fail loudly rather than hang the test runner.
    const BUDGET: u64 = 500;
    let mut observed = false;
    for _ in 0..BUDGET {
        sat.run_for(1);
        if sat.slave().regs.r[10] == SENTINEL {
            observed = true;
            break;
        }
    }
    assert!(observed, "slave never observed master's sentinel write within {BUDGET} cycles");

    // Sanity: the shared memory itself holds the sentinel (proves it
    // wasn't slave R10 getting stamped by some other path).
    use sh2::bus::{AccessKind, Bus};
    let (v, _) = sat.bus.read32(SHARED, AccessKind::Data);
    assert_eq!(v, SENTINEL);
}

#[test]
fn run_for_advances_both_cpus_independently() {
    // Both CPUs spin in a NOP loop; verify the scheduler interleaves
    // them so neither runs away with all the cycles.
    let mut sat = Saturn::with_blank_bios();
    load(
        &mut sat.bus,
        MASTER_PC,
        &[0x0009, 0x0009, 0xAFFD, 0x0009], // NOP NOP BRA -3 NOP
    );
    load(
        &mut sat.bus,
        SLAVE_PC,
        &[0x0009, 0x0009, 0xAFFD, 0x0009],
    );
    sat.master_mut().regs.pc = MASTER_PC;
    sat.master_mut().regs.r[15] = 0x0020_8000;
    sat.slave_mut().regs.pc = SLAVE_PC;
    sat.slave_mut().regs.r[15] = 0x0020_8400;

    sat.run_for(200);
    let m = sat.master().pipeline.cycles;
    let s = sat.slave().pipeline.cycles;
    assert!(m >= 200, "master reached horizon");
    assert!(s >= 200, "slave reached horizon");
    let drift = m.abs_diff(s);
    assert!(drift < 50, "drift {drift} suggests scheduler fairness regressed");
}

#[test]
fn pctrace_records_register_state_at_trigger_pcs() {
    // The multi-PC logic analyzer (Saturn::enable_pctrace) must capture the
    // master's full register file + PR each time it executes a listed trigger
    // PC, skipping delay slots, and leave non-trigger PCs unrecorded.
    //
    // Master program at MASTER_PC:
    //   MOV #5, R3        ; r3 = 5            (PC+0)
    //   NOP               ; <-- trigger here  (PC+2)
    //   BRA -3            ; loop to PC+2      (PC+4)
    //   NOP               ; BRA delay slot    (PC+6)
    // After the MOV, +2/+4/+6 cycle forever with r3==5; +2 is the trigger and is
    // NOT a delay slot, +6 IS (and must never be recorded even if listed).
    let mut sat = Saturn::with_blank_bios();
    load(&mut sat.bus, MASTER_PC, &[0xE305, 0x0009, 0xAFFD, 0x0009]);
    load(&mut sat.bus, SLAVE_PC, &[0x0009, 0x0009, 0xAFFD, 0x0009]); // slave NOP-loops
    sat.master_mut().regs.pc = MASTER_PC;
    sat.master_mut().regs.r[15] = 0x0020_8000;
    sat.slave_mut().regs.pc = SLAVE_PC;
    sat.slave_mut().regs.r[15] = 0x0020_8400;

    let trigger = MASTER_PC + 2; // the NOP
    let slot = MASTER_PC + 6; // the BRA delay slot — listed but must be skipped
    sat.enable_pctrace(vec![trigger, slot]);
    sat.run_for(300);

    let log = sat.take_pctrace();
    assert!(!log.is_empty(), "pctrace recorded nothing at the looping trigger PC");
    for (pc, regs, _pr, _cyc) in &log {
        assert_eq!(*pc, trigger & 0x00FF_FFFF, "recorded a non-trigger / delay-slot PC");
        assert_eq!(regs[3], 5, "captured register state wrong (r3 should be the MOV #5 value)");
    }
    // take_pctrace drains but leaves the logger armed; an immediate re-take is empty.
    assert!(sat.take_pctrace().is_empty(), "take_pctrace should drain the buffer");
}

#[test]
fn reset_loads_pc_and_sp_from_bios_vector() {
    // Bake known reset-vector values into a BIOS image, construct
    // Saturn from it, call reset, and verify both CPUs picked them up.
    let mut bios = vec![0u8; 512 * 1024];
    bios[0..4].copy_from_slice(&0x0020_2000u32.to_be_bytes()); // PC
    bios[4..8].copy_from_slice(&0x0020_8000u32.to_be_bytes()); // SP
    let mut sat = Saturn::new(bios);
    sat.reset();
    assert_eq!(sat.master().regs.pc, 0x0020_2000);
    assert_eq!(sat.master().regs.r[15], 0x0020_8000);
    assert_eq!(sat.slave().regs.pc, 0x0020_2000);
    assert_eq!(sat.slave().regs.r[15], 0x0020_8000);
}

/// Releasing the slave from a long halt must resync its cycle counter to the
/// global clock, not resume it at the (frozen) cycle it was halted at. A halted
/// entity reports `next_deadline == u64::MAX`, so the scheduler skips it and its
/// `pipeline.cycles` freezes; if `release_slave` left that stale value in place,
/// the scheduler would see the slave as millions of cycles "behind" the master
/// and run it that many catch-up steps in a single batch — "time travelling"
/// through stale code. Regression for `Saturn::release_slave`.
#[test]
fn releasing_slave_resyncs_its_cycle_no_time_travel() {
    let mut sat = Saturn::new(vec![0u8; 512 * 1024]);
    sat.reset(); // power-on: slave held halted at its reset cycle (~0)
    assert!(sat.slave_is_halted());

    // Advance the master far past the slave's frozen cycle.
    sat.run_for(2_000_000);
    let now = sat.now();
    let slave_frozen = sat.slave().pipeline.cycles;
    assert!(
        slave_frozen < now,
        "slave cycle ({slave_frozen}) should be behind the global clock ({now}) while halted",
    );

    sat.release_slave();
    let slave_resumed = sat.slave().pipeline.cycles;
    assert!(
        slave_resumed >= now,
        "release_slave must resync the slave cycle to the global clock \
         (got {slave_resumed}, now {now}); leaving it at {slave_frozen} would make the \
         scheduler run ~{} catch-up cycles in one batch",
        now - slave_frozen,
    );
}

/// On the Saturn, a 16-bit write to a fixed region pulses the *other* SH-2's
/// free-running-timer input-capture pin (FTI): 0x0100_0000..0x017F_FFFF wakes
/// the slave, 0x0180_0000..0x01FF_FFFF the master. This is the inter-CPU
/// "wake/dispatch" signal — VF2's master uses it to release its slave's
/// ICF-polling dispatch loop. Verifies the bus flag + aggregate drain set the
/// target FRT's ICF (FTCSR bit 7) and leave the other core untouched.
#[test]
fn word_write_to_fti_region_pulses_target_frt_input_capture() {
    use sh2::bus::{AccessKind, Bus};

    let mut sat = Saturn::new(vec![0u8; 512 * 1024]);
    sat.reset();

    sat.bus.write16(0x0100_0000, 0x1234, AccessKind::Data); // slave FTI
    sat.run_for(512); // a batch boundary drains the pending input capture
    assert_eq!(sat.slave().onchip.frt.ftcsr & 0x80, 0x80, "slave ICF set");
    assert_eq!(
        sat.master().onchip.frt.ftcsr & 0x80,
        0,
        "master ICF untouched by a slave-FTI write"
    );

    sat.bus.write16(0x0180_0000, 0x1234, AccessKind::Data); // master FTI
    sat.run_for(512);
    assert_eq!(sat.master().onchip.frt.ftcsr & 0x80, 0x80, "master ICF set");
}

/// M13 A1: a queued SMPC command breaks the current batch so its side effect
/// dispatches within one instruction of the COMREG write, not up to a
/// `SMPC_POLL_QUANTUM` (256-cycle) batch late.
///
/// The distinguishing observable is cycle-cost-independent: with a `run_for`
/// budget *below* the poll quantum, the OLD batch-drain ran the entire budget
/// as one master-only batch and released the slave only at the boundary — so
/// the slave executed **nothing** within that `run_for`. The early break
/// releases the slave mid-budget, so it runs the rest of the window.
#[test]
fn pending_smpc_command_breaks_batch_so_released_slave_runs_this_window() {
    // SMPC COMREG is at SMPC offset 0x1F; SSHON (release slave) = 0x02.
    // The master writes it through the cache-through SMPC alias, like real
    // software MMIO (the CPU strips it to the physical SMPC region).
    const COMREG: u32 = 0x2010_001F;
    const SSHON: u32 = 0x02;

    // SSHON releases the slave by *resetting* its CPU, which reloads PC/SP from
    // the reset vector (BIOS bytes 0..8). Point that vector at the slave's
    // counting program so the released slave runs known code, not garbage.
    let mut bios = vec![0u8; 512 * 1024];
    bios[0..4].copy_from_slice(&SLAVE_PC.to_be_bytes()); // reset PC
    bios[4..8].copy_from_slice(&0x0020_8400u32.to_be_bytes()); // reset SP
    let mut sat = Saturn::new(bios);
    sat.reset(); // slave held halted at power-on
    assert!(sat.slave_is_halted(), "precondition: slave starts halted");

    // Master: MOV.B R2,@R1 (write SSHON to COMREG), then spin.
    load(
        &mut sat.bus,
        MASTER_PC,
        &[
            0x2120, // MOV.B R2, @R1  (n=1, m=2)
            0xAFFE, // BRA -2 (spin on self)
            0x0009, // NOP slot
        ],
    );
    // Slave (entered via the reset vector on SSHON): ADD #1,R10 in a tight
    // loop — R10 counts instructions the released slave executes.
    load(
        &mut sat.bus,
        SLAVE_PC,
        &[
            0x7A01, // ADD #1, R10
            0xAFFD, // BRA -3 (back to ADD)
            0x0009, // NOP slot
        ],
    );
    {
        let m = sat.master_mut();
        m.regs.pc = MASTER_PC;
        m.regs.r[1] = COMREG;
        m.regs.r[2] = SSHON;
        m.regs.r[15] = 0x0020_8000;
    }

    // Budget deliberately < SMPC_POLL_QUANTUM (256): the only way the slave
    // runs at all within this window is if the pending SSHON broke the batch
    // right after the write and the slave was released for the remainder.
    // (Pre-fix, the whole budget ran as one master-only batch and the slave
    // was released only at the boundary, executing nothing → R10 == 0.)
    sat.run_for(50);

    assert!(!sat.slave_is_halted(), "SSHON must have released the slave");
    assert!(
        sat.slave().regs.r[10] > 0,
        "released slave executed nothing this window — the SMPC command did \
         not break the batch (it drained only at the boundary)"
    );
}

/// M13 A1 safety: a pending SMPC command must not stall `run_for`. The early
/// break always follows a retired master instruction, and `drain_smpc`
/// consumes `pending` before the next batch, so the loop keeps making
/// progress and the queue clears.
#[test]
fn pending_smpc_command_does_not_stall_run_for() {
    use sh2::bus::{AccessKind, Bus};
    let mut sat = Saturn::new(vec![0u8; 512 * 1024]);
    sat.reset();
    // Queue SSHON directly via the bus (no CPU program needed). A direct bus
    // write takes the *physical* SMPC address — only the CPU strips the
    // cache-through alias.
    sat.bus.write8(0x0010_001F, 0x02, AccessKind::Data);
    assert!(sat.bus.smpc.has_pending(), "command queued");

    let before = sat.now();
    sat.run_for(2000);
    assert!(sat.now() >= before + 2000, "run_for advanced the full budget");
    assert!(!sat.bus.smpc.has_pending(), "the queued command was dispatched");
}

/// Debug multi-breakpoint: several breakpoints can be armed at once, and the
/// one **first reached in execution** fires (not the first in the armed list),
/// with the hit's `pc` field identifying which. Exercises the core check via
/// the real `run_for` → `step_cpus` → `entity.step` path that `fc` uses.
#[test]
fn multiple_master_breakpoints_fire_at_the_first_reached() {
    const PC: u32 = 0x0020_2000;
    let mut bios = vec![0u8; 512 * 1024];
    bios[0..4].copy_from_slice(&PC.to_be_bytes()); // reset PC vector
    bios[4..8].copy_from_slice(&0x0020_8000u32.to_be_bytes()); // reset SP vector
    let mut sat = Saturn::new(bios);
    sat.reset();
    // Straight-line adds in low WRAM, then a self-spin.
    load(
        &mut sat.bus,
        PC,
        &[
            0x7001, // ADD #1, R0   @ PC
            0x7002, // ADD #2, R0   @ PC+2   <- the earlier breakpoint
            0x7004, // ADD #4, R0   @ PC+4
            0x7008, // ADD #8, R0   @ PC+6   <- the later breakpoint
            0xAFFE, // BRA -2 (spin on self)
            0x0009, // NOP (delay slot)
        ],
    );
    {
        let m = sat.master_mut();
        m.regs.pc = PC;
        m.regs.r[0] = 0;
        m.regs.r[15] = 0x0020_8000;
    }
    // Arm BOTH — the *later* PC first in the list, to prove ordering is by
    // execution, not by list position.
    sat.set_master_bps(vec![(PC + 6, None), (PC + 2, None)]);
    sat.run_for(64);
    let hit = sat.take_master_bp_hit().expect("a breakpoint must fire");
    assert_eq!(hit.pc, PC + 2, "the first PC reached fires, not the first listed");
    // The bp fires when `regs.pc == bp` (the bp instruction is *pending*), so
    // only the ADD #1 ahead of it has retired: R0 == 1.
    assert_eq!(hit.regs[0], 1, "regs are captured at the bp instruction (pre-execute)");
}

/// A register-guarded breakpoint in a multi-bp set fires only on the matching
/// iteration; an unrelated armed bp at a never-reached PC is inert.
#[test]
fn guarded_breakpoint_in_a_set_waits_for_its_register_value() {
    const PC: u32 = 0x0020_2000;
    let mut bios = vec![0u8; 512 * 1024];
    bios[0..4].copy_from_slice(&PC.to_be_bytes());
    bios[4..8].copy_from_slice(&0x0020_8000u32.to_be_bytes());
    let mut sat = Saturn::new(bios);
    sat.reset();
    // Loop: ADD #1,R0 ; BRA back ; (NOP slot) — R0 counts laps; the bp at the
    // ADD fires only when R0 has reached 5 (guard reg 0 == 5).
    load(
        &mut sat.bus,
        PC,
        &[
            0x7001, // ADD #1, R0   @ PC
            0xAFFD, // BRA -3 (back to PC)
            0x0009, // NOP (delay slot)
        ],
    );
    {
        let m = sat.master_mut();
        m.regs.pc = PC;
        m.regs.r[0] = 0;
        m.regs.r[15] = 0x0020_8000;
    }
    sat.set_master_bps(vec![(0x0020_3000, None), (PC, Some((0, 5)))]);
    sat.run_for(256);
    let hit = sat.take_master_bp_hit().expect("the guarded bp must fire");
    assert_eq!(hit.pc, PC);
    assert_eq!(hit.regs[0], 5, "fires on the iteration where R0 == the guard value");
}
