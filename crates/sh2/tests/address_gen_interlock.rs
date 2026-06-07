//! Address-generation interlock (M13 D3).
//!
//! On the SH7604 a register loaded from memory is not available to the
//! *immediately following* instruction; if that instruction uses the register
//! to **generate an address** (base, index, post-modified base) the pipeline
//! stalls one cycle — the same 1-cycle load-use latency the SH-2 applies to a
//! compute operand. Mednafen unifies the two (every register read runs the
//! `WB_EX_CHECK` write-back scoreboard); ours unifies them through
//! [`Op::reads_reg`] covering address-base operands. These tests pin the stall
//! across each address-base addressing mode so the coverage can't silently
//! regress.

use sh2::Cpu;
use sh2::harness::MemBus;

const PC0: u32 = 0x0000_1000;
const LOAD_R5: u16 = 0x6512; // MOV.L @R1,R5  — loads R5 (the producer)

/// Run `[MOV.L @R1,R5, consumer]` and return the consumer's cycle cost. R1
/// points at a cell holding 0x3000, so R5 is loaded with the address 0x3000,
/// which is itself a valid (4-byte-aligned, in-bounds) memory cell.
fn consumer_cost(consumer: u16, preset: impl FnOnce(&mut Cpu)) -> u32 {
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[LOAD_R5, consumer]);
    bus.write_u32(0x2000, 0x3000); // value loaded into R5
    bus.write_u32(0x3000, 0xCAFE); // what R5 then points at
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = 0x2000;
    preset(&mut cpu);
    cpu.step(&mut bus); // load R5
    cpu.step(&mut bus) // consumer — its cost is the value under test
}

#[test]
fn register_indirect_load_base_stalls() {
    // MOV.L @R5,R6 — R5 (just loaded) is the address base.
    assert_eq!(consumer_cost(0x6652, |_| {}), 2, "1 interlock + 1 base");
}

#[test]
fn displacement_load_base_stalls() {
    // MOV.L @(0,R5),R6 — R5 is the displacement base.
    assert_eq!(consumer_cost(0x5650, |_| {}), 2);
}

#[test]
fn r0_indexed_load_base_stalls() {
    // MOV.L @(R0,R5),R6 — R5 is the indexed base (R0 = 0).
    assert_eq!(consumer_cost(0x065E, |c| c.regs.r[0] = 0), 2);
}

#[test]
fn store_base_stalls() {
    // MOV.L R6,@R5 — R5 is the store address base.
    assert_eq!(consumer_cost(0x2562, |c| c.regs.r[6] = 0xBEEF), 2);
}

#[test]
fn post_increment_load_base_stalls() {
    // MOV.L @R5+,R6 — R5 is the post-incremented base.
    assert_eq!(consumer_cost(0x6656, |_| {}), 2);
}

#[test]
fn unrelated_address_base_does_not_stall() {
    // Control: the consumer addresses through R7 (preset, not just loaded), so
    // there is no interlock — only the 1-cycle base load cost.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[LOAD_R5, 0x6672]); // MOV.L @R7,R6
    bus.write_u32(0x2000, 0x3000);
    bus.write_u32(0x3000, 0xCAFE);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = 0x2000;
    cpu.regs.r[7] = 0x3000;
    cpu.step(&mut bus);
    assert_eq!(cpu.step(&mut bus), 1, "no interlock when the base wasn't just loaded");
}
