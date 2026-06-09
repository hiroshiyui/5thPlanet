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
