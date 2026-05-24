//! Cycle-accurate pipeline timing assertions (task #5).
//!
//! Each test loads a small sequence, runs it, and asserts the exact total
//! cycle count `Cpu::pipeline.cycles` reports. The intent is that any
//! future change to base cycle counts or interlock logic that drifts from
//! the SH-2 software manual or the SH7604 hardware manual shows up here
//! as a numeric mismatch, not as silent behavioural drift.

use sh2::Cpu;
use sh2::harness::MemBus;

const PC0: u32 = 0x0000_1000;

fn run(program: &[u16], n_steps: usize, setup: impl FnOnce(&mut Cpu)) -> Cpu {
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, program);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[15] = 0x0000_8000;
    setup(&mut cpu);
    for _ in 0..n_steps {
        cpu.step(&mut bus);
    }
    cpu
}

#[test]
fn straight_line_no_interlocks_accumulates_linearly() {
    // NOP × 4 → 4 cycles total.
    let cpu = run(&[0x0009, 0x0009, 0x0009, 0x0009], 4, |_| {});
    assert_eq!(cpu.pipeline.cycles, 4);
}

#[test]
fn add_chain_one_cycle_each() {
    // ADD R1,R2 × 3 → 3 cycles.
    let cpu = run(&[0x321C, 0x321C, 0x321C], 3, |c| {
        c.regs.r[1] = 1;
        c.regs.r[2] = 0;
    });
    assert_eq!(cpu.pipeline.cycles, 3);
}

#[test]
fn load_use_consecutive_stalls_one_cycle() {
    // MOV.L @R1, R2  (load, 1 cycle, destination = R2)
    // ADD   R2, R3   (uses R2 → 1-cycle load-use stall, +1)
    // Total: 1 (load) + 1 (interlock) + 1 (add) = 3.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[0x6212, 0x323C]); // MOV.L @R1,R2 ; ADD R2,R3
    bus.write_u32(0x2000, 0xDEAD_BEEF);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = 0x2000;

    let c1 = cpu.step(&mut bus); // load
    let c2 = cpu.step(&mut bus); // add — should stall
    assert_eq!(c1, 1, "load itself is 1 issue cycle");
    assert_eq!(c2, 2, "ADD pays 1 load-use stall + 1 base = 2");
    assert_eq!(cpu.pipeline.cycles, 3);
}

#[test]
fn load_then_unrelated_op_no_stall() {
    // MOV.L @R1,R2 ; ADD R3,R4
    // R4 isn't fed by the loaded R2, so no interlock.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[0x6212, 0x343C]);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = 0x2000;

    cpu.step(&mut bus);
    let c2 = cpu.step(&mut bus);
    assert_eq!(c2, 1, "no stall when ADD doesn't read R2");
    assert_eq!(cpu.pipeline.cycles, 2);
}

#[test]
fn nop_between_load_and_use_absorbs_stall() {
    // MOV.L @R1,R2 ; NOP ; ADD R2,R3
    // The NOP clears the load-use pending state before the consumer runs.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[0x6212, 0x0009, 0x323C]);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = 0x2000;

    for _ in 0..3 {
        cpu.step(&mut bus);
    }
    assert_eq!(
        cpu.pipeline.cycles, 3,
        "1 (load) + 1 (NOP) + 1 (ADD) — interlock absorbed by NOP"
    );
}

#[test]
fn load_use_via_addressing_base_also_stalls() {
    // MOV.L @R1,R2 ; MOV.L @R2,R3   (R2 is the address-base read on the
    // second op, which still counts as a read of R2.)
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[0x6212, 0x6322]);
    bus.write_u32(0x2000, 0x3000);
    bus.write_u32(0x3000, 0xCAFE);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = 0x2000;

    cpu.step(&mut bus);
    let c2 = cpu.step(&mut bus);
    assert_eq!(c2, 2, "second load pays 1 interlock + 1 base");
    assert_eq!(cpu.regs.r[3], 0xCAFE);
}

#[test]
fn jmp_after_load_stalls_when_target_register_is_the_loaded_one() {
    // MOV.L @R1, R3 ; JMP @R3 ; NOP (delay slot)
    // JMP reads R3, which is the freshly loaded register → +1 stall.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[0x6312, 0x432B, 0x0009]);
    bus.write_u32(0x2000, 0x0000_4000);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = 0x2000;

    cpu.step(&mut bus); // load
    let c_jmp = cpu.step(&mut bus); // JMP, expected base 2 + 1 stall = 3
    assert_eq!(c_jmp, 3);
}

#[test]
fn branch_with_delay_slot_total_cycles() {
    // BRA (target = PC0+6) ; NOP slot ; (landed) NOP
    // BRA base = 2, slot NOP = 1, landed NOP = 1. Total over 3 steps = 4.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[0xA001, 0x0009, 0x0009, 0x0009]);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    for _ in 0..3 {
        cpu.step(&mut bus);
    }
    assert_eq!(cpu.pipeline.cycles, 4);
}

#[test]
fn conditional_branch_taken_vs_not_taken() {
    // Layout:
    //   PC0+0  SETT
    //   PC0+2  BT (disp=0, target = PC0+6 = CLRT) — taken on first run
    //   PC0+4  NOP   (skipped by BT taken)
    //   PC0+6  CLRT  (landing)
    //   PC0+8  BT (disp=0) — not taken now (T=0)
    // Expected: 1 (SETT) + 3 (BT taken) + 1 (CLRT) + 1 (BT not taken) = 6.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[0x0018, 0x8900, 0x0009, 0x0008, 0x8900]);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;

    cpu.step(&mut bus); // SETT
    let bt_taken = cpu.step(&mut bus);
    assert_eq!(bt_taken, 3);
    cpu.step(&mut bus); // CLRT
    let bt_skipped = cpu.step(&mut bus);
    assert_eq!(bt_skipped, 1);
    assert_eq!(cpu.pipeline.cycles, 6);
}

#[test]
fn multiply_then_sts_no_extra_stall_in_m1_model() {
    // MUL.L R1,R2 (base 2) ; STS MACL,R3 (base 1)
    // In the M1 model multiply_latency() returns 0, so the issue cost
    // already covers the multiplier pipeline — no extra interlock.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[0x0217, 0x031A]); // MUL.L ; STS MACL,R3
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = 5;
    cpu.regs.r[2] = 7;

    cpu.step(&mut bus); // MUL.L → 2 cycles
    let c_sts = cpu.step(&mut bus); // STS MACL → 1 cycle
    assert_eq!(c_sts, 1);
    assert_eq!(cpu.pipeline.cycles, 3);
    assert_eq!(cpu.regs.r[3], 35);
}

#[test]
fn pipeline_cycles_is_monotonic_under_mixed_workload() {
    // Sanity: arbitrary mix never decreases pipeline.cycles between steps.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(
        PC0,
        &[
            0xE105, // MOV #5, R1
            0xE204, // MOV #4, R2
            0x321C, // ADD R1, R2
            0x0217, // MUL.L R1, R2
            0x031A, // STS MACL, R3
            0x0009, // NOP
        ],
    );
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    let mut last = 0u64;
    for _ in 0..6 {
        cpu.step(&mut bus);
        assert!(cpu.pipeline.cycles >= last);
        last = cpu.pipeline.cycles;
    }
}
