//! Hardware-divider (DIVU) latency timing (M13 D1).
//!
//! The SH7604 divider runs autonomously (~39 cycles for a 32/32 divide); a read
//! of any DIVU register before it retires stalls the CPU until the result is
//! ready (Mednafen `divide_finish_timestamp`). These tests trigger a divide
//! through the on-chip register window via real MOV.L instructions and observe
//! the stall the consuming read pays.

use sh2::Cpu;
use sh2::harness::MemBus;

const PC0: u32 = 0x0000_1000;

// DIVU register addresses.
const DVSR: u32 = 0xFFFF_FF00;
const DVDNT: u32 = 0xFFFF_FF04;

// MOV.L R4,@R2 (DVSR = R4) ; MOV.L R5,@R1 (DVDNT = R5, triggers the divide) ;
// MOV.L @R1,R6 (read DVDNT back).
const STORE_DVSR: u16 = 0x2242;
const STORE_DVDNT: u16 = 0x2152;
const LOAD_DVDNT: u16 = 0x6612;
const NOP: u16 = 0x0009;

fn setup(cpu: &mut Cpu) {
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = DVDNT;
    cpu.regs.r[2] = DVSR;
    cpu.regs.r[4] = 7; // divisor
    cpu.regs.r[5] = 50; // dividend → 50 / 7 = 7 r 1
}

#[test]
fn divu_read_immediately_after_trigger_stalls_for_the_divide() {
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[STORE_DVSR, STORE_DVDNT, LOAD_DVDNT]);
    let mut cpu = Cpu::new();
    setup(&mut cpu);

    cpu.step(&mut bus); // DVSR = 7
    cpu.step(&mut bus); // DVDNT = 50 → divide triggered, ~39-cycle latency armed
    let c_read = cpu.step(&mut bus); // read DVDNT → stalls until the divider retires

    assert!(
        c_read >= 30,
        "a read one instruction after the trigger pays most of the 39-cycle \
         divide latency, got {c_read}"
    );
    // The quotient is still computed correctly (the value is eager; only the
    // timing is deferred).
    assert_eq!(cpu.regs.r[6] as i32, 7, "50 / 7 = 7");
}

#[test]
fn divu_read_after_latency_elapsed_does_not_stall() {
    // Pad enough NOPs between the trigger and the read to cover the 39-cycle
    // divide; the read then pays only its base load cost.
    let mut prog = vec![STORE_DVSR, STORE_DVDNT];
    prog.extend(std::iter::repeat_n(NOP, 40));
    prog.push(LOAD_DVDNT);
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &prog);
    let mut cpu = Cpu::new();
    setup(&mut cpu);

    for _ in 0..prog.len() - 1 {
        cpu.step(&mut bus);
    }
    let c_read = cpu.step(&mut bus); // the divider has long since retired

    assert!(
        c_read <= 2,
        "once the 39-cycle latency has elapsed the DVDNT read does not stall, \
         got {c_read}"
    );
    assert_eq!(cpu.regs.r[6] as i32, 7);
}

#[test]
fn divu_overflow_settles_faster_than_a_normal_divide() {
    // Divide-by-zero overflows; the divider settles in ~6 cycles, not ~39, so
    // an immediate read of DVCR stalls far less.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[STORE_DVSR, STORE_DVDNT, LOAD_DVDNT]);
    let mut cpu = Cpu::new();
    setup(&mut cpu);
    cpu.regs.r[4] = 0; // DVSR = 0 → divide-by-zero overflow

    cpu.step(&mut bus); // DVSR = 0
    cpu.step(&mut bus); // DVDNT = 50 → overflow
    let c_read = cpu.step(&mut bus); // read DVDNT

    assert!(
        (1..=10).contains(&c_read),
        "overflow settles in ~6 cycles, so the read stalls far less than a \
         normal divide, got {c_read}"
    );
    assert_eq!(cpu.onchip.divu.dvcr & 1, 1, "OVF flag set");
}
