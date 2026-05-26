//! SCU-DSP opcode-level integration tests.
//!
//! Each test loads a tiny microcode snippet, runs to END, and asserts the
//! post-state. Encoding helpers mirror the real word layout (SCU manual /
//! MAME `scudsp`): an Operation word issues an ALU op plus parallel X/Y/D1
//! data-move slots, and jumps/loops have a one-instruction **delay slot**
//! (the word after a taken jump executes once before control resumes).

use scu_dsp::Dsp;
use scu_dsp::interpreter::DmaRequest;

// ---- operation-word (class 00) builders ----
const OP: u32 = 0; // class 00 in bits 31..30
/// ALU-only operation word (ALU code in bits 29..26).
fn op_alu(alu: u32) -> u32 {
    OP | (alu << 26)
}
/// Y-bus `MOV ALU,A` (bits 18..17 = 0b10) — copy the 48-bit ALU result into ACH:ACL.
const MOV_ALU_A: u32 = 0b10 << 17;
/// X-bus `MOV [s],X` (bit 25) with source `s` (bits 22..20) → load RX.
fn mov_s_x(src: u32) -> u32 {
    (1 << 25) | (src << 20)
}
/// Y-bus `MOV [s],Y` (bit 19) with source `s` (bits 16..14) → load RY.
fn mov_s_y(src: u32) -> u32 {
    (1 << 19) | (src << 14)
}
/// X-bus `MOV MUL,P` (bits 24..23 = 0b10).
const MOV_MUL_P: u32 = 0b10 << 23;
/// Y-bus `CLR A` (bits 18..17 = 0b01).
const CLR_A: u32 = 0b01 << 17;
/// D1-bus `MOV #imm8,[d]` (bits 13..12 = 0b01).
fn d1_mov_imm(dest: u32, imm: u8) -> u32 {
    (0b01 << 12) | (dest << 8) | imm as u32
}
/// D1-bus `MOV [s],[d]` (bits 13..12 = 0b11).
fn d1_mov_s_d(dest: u32, src: u32) -> u32 {
    (0b11 << 12) | (dest << 8) | src
}

// ---- control builders ----
/// MVI #imm,[d] (unconditional). dest in bits 29..26, 25-bit signed immediate.
fn mvi(dest: u32, imm: i32) -> u32 {
    (0b10 << 30) | (dest << 26) | ((imm as u32) & 0x01FF_FFFF)
}
fn jmp(target: u32) -> u32 {
    (0b11 << 30) | (0b01 << 28) | target
}
/// Conditional JMP. `cond` is the 6-bit condition (e.g. 0x21 = "if Z").
fn jmp_cond(cond: u32, target: u32) -> u32 {
    (0b11 << 30) | (0b01 << 28) | (cond << 19) | target
}
const LPS: u32 = (0b11 << 30) | (0b10 << 28) | (1 << 27);
const BTM: u32 = (0b11 << 30) | (0b10 << 28);
const END: u32 = (0b11 << 30) | (0b11 << 28);
const ENDI: u32 = (0b11 << 30) | (0b11 << 28) | (1 << 27);
const NOP: u32 = 0;

// Source/dest selector constants (4-bit dest table; 3-bit source for X/Y).
const MC0: u32 = 4; // source: bank 0, auto-increment CT0
const DEST_MC0: u32 = 0x0;
const DEST_RX: u32 = 0x4;
const DEST_PL: u32 = 0x5;
const DEST_LOP: u32 = 0xA;
const DEST_CT0: u32 = 0xC;

fn run(prog: &[u32]) -> Dsp {
    let mut d = Dsp::new();
    d.load_program(0, prog);
    d.start(0);
    d.run_until_stopped(64);
    d
}

#[test]
fn starts_stopped_and_step_is_noop_until_started() {
    let mut d = Dsp::new();
    assert!(d.stopped());
    let pc = d.regs.pc;
    d.step();
    d.step();
    assert_eq!(d.regs.pc, pc);
}

#[test]
fn mvi_loads_registers() {
    // MVI's destination table covers 0..0xA (MC0-3/RX/PL/RA0/WA0/LOP) plus
    // PC (0xC); CT and TOP are D1-bus-only, so they're not tested here.
    let d = run(&[
        mvi(DEST_PL, 0x1234),
        mvi(DEST_RX, -1),
        mvi(DEST_LOP, 0xFFF),
        END,
    ]);
    assert_eq!(d.regs.pl, 0x1234);
    assert_eq!(d.regs.rx, 0xFFFF_FFFF);
    assert_eq!(d.regs.lop, 0x0FFF);
}

#[test]
fn d1_bus_loads_ct0() {
    // CT0 is written via the D1-bus dest table (0xC), not MVI.
    let d = run(&[op_alu(0) | d1_mov_imm(DEST_CT0, 0x21), END]);
    assert_eq!(d.regs.ct[0], 0x21);
}

#[test]
fn mvi_to_mc0_writes_data_ram_and_increments_ct() {
    let d = run(&[mvi(DEST_MC0, 0xABCD), mvi(DEST_MC0, 0x1234), END]);
    assert_eq!(d.data_ram[0][0], 0xABCD);
    assert_eq!(d.data_ram[0][1], 0x1234);
    assert_eq!(d.regs.ct[0], 2, "CT0 auto-incremented twice");
}

#[test]
fn alu_add_writes_alu_and_y_bus_moves_it_to_acl() {
    // ACL=10, PL=5; ADD computes ALU=15, MOV ALU,A copies it into ACH:ACL.
    let mut d = Dsp::new();
    d.regs.acl = 10;
    d.load_program(0, &[mvi(DEST_PL, 5), op_alu(0x4) | MOV_ALU_A, END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.alu as u64 & 0xFFFF_FFFF, 15);
    assert_eq!(d.regs.acl, 15);
    assert!(!d.regs.flags.z);
}

#[test]
fn alu_sub_to_zero_sets_z() {
    let mut d = Dsp::new();
    d.regs.acl = 7;
    d.load_program(0, &[mvi(DEST_PL, 7), op_alu(0x5), END]); // SUB
    d.start(0);
    d.run_until_stopped(16);
    assert!(d.regs.flags.z);
    assert_eq!(d.regs.alu as u64 & 0xFFFF_FFFF, 0);
}

#[test]
fn alu_logic_ops() {
    let mut d = Dsp::new();
    d.regs.acl = 0b1100;
    d.load_program(0, &[mvi(DEST_PL, 0b1010), op_alu(0x1), END]); // AND
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.alu as u64 & 0xFFFF_FFFF, 0b1000);
}

#[test]
fn alu_shifts_set_carry() {
    // SL of 0x8000_0000 → carry out of MSB, result 0.
    let mut d = Dsp::new();
    d.regs.acl = 0x8000_0000;
    d.load_program(0, &[op_alu(0xA), END]); // SL
    d.start(0);
    d.run_until_stopped(16);
    assert!(d.regs.flags.c, "SL shifted MSB out → C set");
    assert_eq!(d.regs.alu as u64 & 0xFFFF_FFFF, 0);
}

#[test]
fn rl8_rotates_left_eight() {
    let mut d = Dsp::new();
    d.regs.acl = 0x1234_5678;
    d.load_program(0, &[op_alu(0xF), END]); // RL8
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.alu as u64 & 0xFFFF_FFFF, 0x3456_7812);
}

#[test]
fn multiplier_computes_rx_times_ry_and_mov_mul_p() {
    // data RAM: M0[0]=6, M0[1]=7. MOV [MC0],X loads RX=6 (CT0→1), then a
    // second word MOV [MC0],Y loads RY=7; MUL = 42. Then MOV MUL,P.
    let mut d = Dsp::new();
    d.data_ram[0][0] = 6;
    d.data_ram[0][1] = 7;
    d.load_program(
        0,
        &[
            op_alu(0) | mov_s_x(MC0), // RX = M0[0]=6, CT0→1
            op_alu(0) | mov_s_y(MC0), // RY = M0[1]=7, CT0→2  → MUL = 42
            op_alu(0) | MOV_MUL_P,    // PH:PL = MUL
            END,
        ],
    );
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.rx, 6);
    assert_eq!(d.regs.ry, 7);
    assert_eq!(d.regs.mul, 42);
    assert_eq!(d.regs.pl, 42);
    assert_eq!(d.regs.ct[0], 2);
}

#[test]
fn clr_a_zeroes_accumulator() {
    let mut d = Dsp::new();
    d.regs.acl = 0xDEAD;
    d.regs.ach = 0xBEEF;
    d.load_program(0, &[op_alu(0) | CLR_A, END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.acl, 0);
    assert_eq!(d.regs.ach, 0);
}

#[test]
fn d1_bus_moves_immediate_and_memory() {
    // MOV #0x55,RX (D1 imm), then MOV [M0],CT0... simpler: imm to RX, and
    // MOV [MC0],PL (D1 mem→reg).
    let mut d = Dsp::new();
    d.data_ram[0][0] = 0x99;
    d.load_program(
        0,
        &[
            op_alu(0) | d1_mov_imm(DEST_RX, 0x55),
            op_alu(0) | d1_mov_s_d(DEST_PL, MC0),
            END,
        ],
    );
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.rx, 0x55);
    assert_eq!(d.regs.pl, 0x99);
    assert_eq!(d.regs.ct[0], 1, "D1 [MC0] read auto-incremented CT0");
}

#[test]
fn unconditional_jmp_has_a_delay_slot() {
    // JMP 4; the NOP at index 1 (delay slot) runs once; control resumes at 4.
    // Mark the delay slot by having it load RX so we can see it executed.
    let d = run(&[jmp(4), op_alu(0) | d1_mov_imm(DEST_RX, 0x7B), NOP, NOP, END]);
    assert_eq!(d.regs.rx, 0x7B, "delay-slot instruction executed once");
    assert_eq!(d.regs.pc, 5, "halted one past END at index 4");
}

#[test]
fn conditional_jmp_if_z_taken_and_not_taken() {
    // cond 0x21 = bit5 (polarity: test true) + Z. Taken when Z set.
    let mut taken = Dsp::new();
    taken.regs.flags.z = true;
    taken.load_program(0, &[jmp_cond(0x21, 3), NOP, END, END]);
    taken.start(0);
    taken.run_until_stopped(16);
    assert_eq!(taken.regs.pc, 4, "Z set → jumped to 3 (END), halt one past");

    let mut nt = Dsp::new();
    nt.regs.flags.z = false;
    nt.load_program(0, &[jmp_cond(0x21, 3), END, END, END]);
    nt.start(0);
    nt.run_until_stopped(16);
    assert_eq!(nt.regs.pc, 2, "Z clear → fell through to END at index 1");
}

#[test]
fn lps_repeats_next_instruction_lop_times() {
    // LOP=3, LPS, then ADD #1-style increment of RX via D1 imm... use MC0
    // writes to count iterations. LPS re-runs the *next* word LOP times.
    let mut d = Dsp::new();
    d.regs.lop = 3;
    // LPS, then "MOV [MC0]++ <- imm 1" repeated; counts CT0 advances.
    d.load_program(0, &[LPS, op_alu(0) | d1_mov_imm(DEST_MC0, 1), END]);
    d.start(0);
    d.run_until_stopped(64);
    // The delay-slot word runs once, then LPS re-runs it lop times: 1 + 3 = 4
    // executions → CT0 advanced 4 times.
    assert_eq!(d.regs.ct[0], 4);
    assert_eq!(d.regs.lop, 0);
}

#[test]
fn btm_branches_to_top_with_delay_slot() {
    // TOP=5, LOP=1; BTM at 2 → LOP-- and branch to TOP (skipping END at 4),
    // running its delay slot (index 3) once.
    let d = run(&[
        op_alu(0) | d1_mov_imm(0xB, 5), // TOP = 5 (D1 dest 0xB)
        mvi(DEST_LOP, 1),
        BTM,
        op_alu(0) | d1_mov_imm(DEST_RX, 0x11), // delay slot
        END,                                   // skipped
        END,                                   // halts here (TOP)
    ]);
    assert_eq!(d.regs.rx, 0x11, "BTM delay slot executed");
    assert_eq!(d.regs.lop, 0, "BTM decremented LOP");
    assert_eq!(d.regs.pc, 6, "branched to TOP=5 then halted one past");
}

#[test]
fn endi_raises_end_interrupt_end_does_not() {
    let di = run(&[ENDI]);
    assert!(di.stopped());
    assert!(di.end_interrupt_pending);
    assert!(di.regs.flags.end);

    let de = run(&[END]);
    assert!(de.stopped());
    assert!(!de.end_interrupt_pending);
}

#[test]
fn dma_decodes_a_request_and_sets_t0() {
    // DMA D0,M0,#8 : dir=from A/B-bus to DSP (dir bit 12 = 0), bank 0, size 8.
    let mut d = Dsp::new();
    // class 11 (control), sub-op 00 (DMA in bits 29..28), size 8, add_sel 0.
    let dma = (0b11u32 << 30) | 8;
    d.load_program(0, &[dma, END]);
    d.start(0);
    d.step(); // execute the DMA word
    assert!(d.regs.flags.t0, "DMA sets the T0 busy flag");
    assert_eq!(
        d.take_dma(),
        Some(DmaRequest {
            from_dsp: false,
            dsp_bank: 0,
            size: 8,
            add: 0,
            update_addr: true,
        })
    );
}

#[test]
fn run_until_stopped_caps_at_max_steps() {
    let mut d = Dsp::new();
    d.load_program(0, &[jmp(0), NOP]); // tight loop via delay slot
    d.start(0);
    let steps = d.run_until_stopped(64);
    assert_eq!(steps, 64);
    assert!(!d.stopped());
}
