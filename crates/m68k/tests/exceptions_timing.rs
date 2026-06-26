//! MC68EC000 exception model + MUL/DIV cycle timing (M13 D4).
//!
//! Covers the additions that completed the 68000 core: the address-error
//! (vector 3) exception with the long group-0 stack frame, the trace exception
//! (vector 9), and the exact data-dependent MUL/DIV cycle tables ported from
//! Mednafen `m68k.cpp`.

use m68k::Cpu;
use m68k::bus::{AccessKind, Bus};
use m68k::harness::MemBus;

fn boot(words: &[u16], setup: impl FnOnce(&mut Cpu)) -> (Cpu, MemBus) {
    let mut bus = MemBus::new(0x1_0000);
    let mut pc = 0x1000u32;
    for &w in words {
        bus.write_word(pc, w);
        pc += 2;
    }
    let mut cpu = Cpu::new();
    cpu.regs.pc = 0x1000;
    cpu.regs.a[7] = 0x2000;
    setup(&mut cpu);
    (cpu, bus)
}

// ---- MUL/DIV cycle timing ------------------------------------------------

/// Step a single MULU/MULS with `D1 = src` (the `<ea>` operand) and return the
/// cycle cost.
fn mul_cost(op: u16, src: u32) -> u32 {
    let (mut cpu, mut bus) = boot(&[op], |c| {
        c.regs.d[0] = 0x0000_0003;
        c.regs.d[1] = src;
    });
    cpu.step(&mut bus)
}

#[test]
fn mulu_cycle_count_adds_two_per_source_set_bit() {
    // MULU.W D1,D0 (0xC0C1): 34 + 2·popcount(src). An all-ones source has 16
    // set bits more than a zero source → +32.
    let zero = mul_cost(0xC0C1, 0x0000);
    let ones = mul_cost(0xC0C1, 0xFFFF);
    assert_eq!(ones - zero, 32, "+2 per set bit, 16 bits = +32");
}

#[test]
fn muls_cycle_count_adds_two_per_source_transition() {
    // MULS.W D1,D0 (0xC1C1): 34 + 2·(0↔1 transitions in src, 0 appended at LSB).
    // A fully alternating source (0x5555) has 16 transitions → +32 over zero.
    let zero = mul_cost(0xC1C1, 0x0000);
    let alt = mul_cost(0xC1C1, 0x5555);
    assert_eq!(alt - zero, 32, "+2 per bit transition, alternating = +32");
}

#[test]
fn divu_divs_costs_land_in_the_68000_documented_ranges() {
    // DIVU.W D1,D0 (0x80C1): the 68000 spends ~76..140 cycles. DIVS (0x81C1) is
    // dearer (sign fix-up). Both include the 4-cycle fetch.
    let divu = {
        let (mut cpu, mut bus) = boot(&[0x80C1], |c| {
            c.regs.d[0] = 17;
            c.regs.d[1] = 5;
        });
        cpu.step(&mut bus)
    };
    let divs = {
        let (mut cpu, mut bus) = boot(&[0x81C1], |c| {
            c.regs.d[0] = (-17i32) as u32;
            c.regs.d[1] = 5;
        });
        cpu.step(&mut bus)
    };
    assert!((70..=150).contains(&divu), "DIVU in range, got {divu}");
    assert!((70..=170).contains(&divs), "DIVS in range, got {divs}");
}

#[test]
fn divu_overflow_is_cheaper_than_a_full_divide() {
    // An immediate overflow (dividend ≥ divisor<<16) exits before the 16-step
    // loop, so it costs far fewer cycles than a normal divide.
    let normal = {
        let (mut cpu, mut bus) = boot(&[0x80C1], |c| {
            c.regs.d[0] = 17;
            c.regs.d[1] = 5;
        });
        cpu.step(&mut bus)
    };
    let overflow = {
        let (mut cpu, mut bus) = boot(&[0x80C1], |c| {
            c.regs.d[0] = 0x0001_0000; // / 1 → quotient 0x10000 doesn't fit 16 bits
            c.regs.d[1] = 1;
        });
        cpu.step(&mut bus)
    };
    assert!(
        overflow < normal,
        "overflow ({overflow}) cheaper than divide ({normal})"
    );
}

// ---- address-error exception (vector 3, group-0 frame) -------------------

#[test]
fn word_write_to_odd_address_takes_an_address_error() {
    // MOVE.W D0,(A0) with A0 odd → address error (vector 3) with the 14-byte
    // group-0 stack frame.
    let (mut cpu, mut bus) = boot(&[0x3080], |c| {
        c.regs.sr.supervisor = true;
        c.regs.ssp = 0x2000;
        c.regs.a[7] = 0x2000;
        c.regs.a[0] = 0x2001; // odd destination
        c.regs.d[0] = 0xCAFE;
    });
    bus.write_word(0x000C, 0x0000); // address-error vector (3 << 2 = 0xC)
    bus.write_word(0x000E, 0x5000);
    cpu.step(&mut bus);

    assert_eq!(
        cpu.regs.pc, 0x5000,
        "vectored through the address-error handler"
    );
    assert!(cpu.regs.sr.supervisor);
    assert_eq!(cpu.regs.a[7], 0x2000 - 14, "long group-0 frame is 14 bytes");
    assert_eq!(cpu.fault, None, "fault consumed");
    // The data write never reached the (odd) target cell.
    assert_eq!(
        Bus::read16(&mut bus, 0x2000, AccessKind::Data).0,
        0,
        "write aborted"
    );
}

#[test]
fn byte_access_to_odd_address_is_fine() {
    // MOVE.B D0,(A0) with A0 odd is legal — byte accesses have no alignment.
    let (mut cpu, mut bus) = boot(&[0x1080], |c| {
        c.regs.sr.supervisor = true;
        c.regs.a[0] = 0x2001;
        c.regs.d[0] = 0x00CA;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.fault, None, "no address error on a byte access");
    assert_eq!(
        Bus::read8(&mut bus, 0x2001, AccessKind::Data).0,
        0xCA,
        "byte stored at the odd address"
    );
}

#[test]
fn aligned_word_write_does_not_fault() {
    let (mut cpu, mut bus) = boot(&[0x3080], |c| {
        c.regs.sr.supervisor = true;
        c.regs.a[0] = 0x2002; // even
        c.regs.d[0] = 0xBEEF;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.fault, None);
    assert_eq!(Bus::read16(&mut bus, 0x2002, AccessKind::Data).0, 0xBEEF);
}

// ---- trace exception (vector 9) ------------------------------------------

#[test]
fn trace_fires_after_an_instruction_executed_with_t_set() {
    // A NOP executed with T set takes the trace exception afterwards, stacking
    // the PC of the *next* instruction.
    let (mut cpu, mut bus) = boot(&[0x4E71, 0x4E71], |c| {
        c.regs.sr.supervisor = true;
        c.regs.sr.trace = true;
        c.regs.ssp = 0x2000;
        c.regs.a[7] = 0x2000;
    });
    bus.write_word(0x0024, 0x0000); // trace vector (9 << 2 = 0x24)
    bus.write_word(0x0026, 0x3000);
    cpu.step(&mut bus);

    assert_eq!(cpu.regs.pc, 0x3000, "vectored through the trace handler");
    assert!(!cpu.regs.sr.trace, "T cleared on trace-exception entry");
    assert_eq!(
        cpu.regs.a[7],
        0x2000 - 6,
        "normal 6-byte frame (not group-0)"
    );
    // The stacked PC is the instruction *after* the traced NOP.
    assert_eq!(
        Bus::read32(&mut bus, cpu.regs.a[7] + 2, AccessKind::Data).0,
        0x1002,
        "stacked PC points past the traced NOP"
    );
}

#[test]
fn no_trace_when_t_is_clear() {
    let (mut cpu, mut bus) = boot(&[0x4E71], |c| c.regs.sr.supervisor = true);
    cpu.step(&mut bus);
    assert_eq!(
        cpu.regs.pc, 0x1002,
        "ran straight through, no trace exception"
    );
}
