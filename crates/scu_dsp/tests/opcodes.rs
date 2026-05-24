//! SCU-DSP opcode-level integration tests.
//!
//! Each test loads a tiny microcode snippet, runs the DSP to its END,
//! and asserts the post-state. The encoding helpers mirror the bit
//! layouts the decoder match-arms recognise; if you change the
//! decoder, update the helpers in lock-step.

use scu_dsp::{Dsp, PROGRAM_WORDS};

/// Class 00 — Operation. ALU code in bits 29..26.
fn op_word(alu: u32) -> u32 {
    (0b00 << 30) | (alu << 26)
}
/// Class 10 — MVI. dest in bits 29..26; 25-bit signed immediate in low 25.
fn mvi_word(dest: u32, imm: i32) -> u32 {
    let imm25 = (imm as u32) & 0x01FF_FFFF;
    (0b10 << 30) | (dest << 26) | imm25
}
/// Class 11 — Specialized. Selector in bits 29..26.
fn sp_word(sel: u32) -> u32 {
    (0b11 << 30) | (sel << 26)
}
/// JMP cond, target: class 11, selector 0001, cond in 22..19, target in 7..0.
fn jmp_word(cond: u32, target: u32) -> u32 {
    sp_word(0b0001) | (cond << 19) | target
}

const NOP: u32 = 0; // class 00, alu=0
const END: u32 = (0b11 << 30) | (0b1000 << 26);
const ENDI: u32 = (0b11 << 30) | (0b1001 << 26);

#[test]
fn dsp_starts_stopped_and_step_is_a_noop_until_start_called() {
    let mut d = Dsp::new();
    let before_pc = d.regs.pc;
    d.step();
    d.step();
    assert!(d.stopped);
    assert_eq!(d.regs.pc, before_pc);
}

#[test]
fn mvi_to_acl_then_end_terminates_at_one_past_end() {
    let mut d = Dsp::new();
    // MVI dest=0 (ACL), imm=0xABCD → ACL = 0xABCD; then END.
    d.load_program(0, &[mvi_word(0, 0x0000_ABCD), END]);
    d.start(0);
    let steps = d.run_until_stopped(16);
    assert_eq!(steps, 2);
    assert_eq!(d.regs.acl, 0xABCD);
    assert!(d.stopped);
}

#[test]
fn add_op_consumes_md0_and_writes_flags() {
    let mut d = Dsp::new();
    d.regs.acl = 10;
    d.regs.md[0] = 5;
    // Operation { alu: Add } → ACL = ACL + MD0 = 15
    d.load_program(0, &[op_word(4), END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.acl, 15);
    assert!(!d.regs.flags.z);
    assert!(!d.regs.flags.s);
}

#[test]
fn sub_to_zero_sets_z_flag() {
    let mut d = Dsp::new();
    d.regs.acl = 7;
    d.regs.md[0] = 7;
    d.load_program(0, &[op_word(5), END]); // SUB
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.acl, 0);
    assert!(d.regs.flags.z);
}

#[test]
fn unconditional_jmp_changes_pc() {
    let mut d = Dsp::new();
    d.load_program(0, &[jmp_word(0, 4), NOP, NOP, NOP, END]);
    d.start(0);
    d.run_until_stopped(16);
    // END halts the DSP at its own address — PC = the END at index 4.
    assert_eq!(d.regs.pc, 4);
    assert!(d.stopped);
}

#[test]
fn conditional_jmp_z_taken_when_zero_flag_set() {
    let mut d = Dsp::new();
    d.regs.flags.z = true;
    d.load_program(0, &[jmp_word(1, 2), END, END]);
    d.start(0);
    d.run_until_stopped(16);
    // JMP-if-Z to 2; END halts there.
    assert_eq!(d.regs.pc, 2);
}

#[test]
fn conditional_jmp_z_falls_through_when_zero_flag_clear() {
    let mut d = Dsp::new();
    d.regs.flags.z = false;
    d.load_program(0, &[jmp_word(1, 4), END, END]);
    d.start(0);
    d.run_until_stopped(16);
    // JMP not taken → step retires the jump, advances to 1 (END), halts.
    assert_eq!(d.regs.pc, 1);
}

#[test]
fn endi_sets_end_interrupt_pending() {
    let mut d = Dsp::new();
    d.load_program(0, &[ENDI]);
    d.start(0);
    d.run_until_stopped(16);
    assert!(d.stopped);
    assert!(d.end_interrupt_pending, "ENDI must raise the end-interrupt request");
}

#[test]
fn end_does_not_raise_interrupt() {
    let mut d = Dsp::new();
    d.load_program(0, &[END]);
    d.start(0);
    d.run_until_stopped(16);
    assert!(d.stopped);
    assert!(!d.end_interrupt_pending);
}

#[test]
fn ad2_finalises_multiply_accumulate_into_acl_ach() {
    let mut d = Dsp::new();
    d.regs.set_ac(100);
    d.regs.p = 23;
    d.load_program(0, &[op_word(6), END]); // AD2
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.ac(), 123);
}

#[test]
fn mvi_to_ct0_through_3_clamps_to_six_bit_pointer() {
    let mut d = Dsp::new();
    d.load_program(
        0,
        &[
            mvi_word(4, 0x42), // CT0 (only low 6 bits stored)
            mvi_word(5, 0xFF),
            mvi_word(6, 0x21),
            mvi_word(7, 0x00),
            END,
        ],
    );
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.ct[0], 0x02, "0x42 & 0x3F = 0x02");
    assert_eq!(d.regs.ct[1], 0x3F);
    assert_eq!(d.regs.ct[2], 0x21);
    assert_eq!(d.regs.ct[3], 0x00);
}

#[test]
fn run_until_stopped_caps_at_max_steps() {
    let mut d = Dsp::new();
    // Tight infinite loop: JMP unconditional back to 0.
    d.load_program(0, &[jmp_word(0, 0)]);
    d.start(0);
    let steps = d.run_until_stopped(64);
    assert_eq!(steps, 64);
    assert!(!d.stopped, "still running because we capped, not because of END");
}

#[test]
fn program_load_silently_truncates_past_256_words() {
    let mut d = Dsp::new();
    let oversized: alloc::vec::Vec<u32> = (0..PROGRAM_WORDS as u32 + 4).collect();
    // Need an alloc dep for vec — drop test if no_std issues.
    d.load_program(0, &oversized);
    assert_eq!(d.program[PROGRAM_WORDS - 1], (PROGRAM_WORDS - 1) as u32);
}

extern crate alloc;
