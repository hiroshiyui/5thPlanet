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

// ---- more ALU coverage (Or/Xor/Ad2/Sr/Rr/Rl + flag edges) ----

/// Run an ALU op over preset ACL/PL and return the post-state.
fn run_alu(acl: u32, pl: u32, alu_code: u32) -> Dsp {
    let mut d = Dsp::new();
    d.regs.acl = acl;
    d.regs.pl = pl;
    d.load_program(0, &[op_alu(alu_code), END]);
    d.start(0);
    d.run_until_stopped(16);
    d
}

#[test]
fn alu_or_sets_s_and_clears_c() {
    // 0x8000_0000 | 1 → negative result; OR clears C, sets S.
    let d = run_alu(0x8000_0000, 1, 0x2);
    assert_eq!(d.regs.alu as u64 & 0xFFFF_FFFF, 0x8000_0001);
    assert!(d.regs.flags.s, "OR result negative → S set");
    assert!(!d.regs.flags.c, "OR always clears C");
    // HW quirk (MAME): Z reflects the non-negative result only, so a negative
    // OR result reports Z=false even though it's plainly non-zero.
    assert!(!d.regs.flags.z);
}

#[test]
fn alu_or_zero_result_sets_z() {
    let d = run_alu(0, 0, 0x2);
    assert!(d.regs.flags.z, "0 | 0 == 0, non-negative → Z set");
    assert!(!d.regs.flags.s);
}

#[test]
fn alu_xor_computes_and_sets_flags() {
    let d = run_alu(0xF0F0_F0F0, 0x0F0F_0F0F, 0x3);
    assert_eq!(d.regs.alu as u64 & 0xFFFF_FFFF, 0xFFFF_FFFF);
    assert!(d.regs.flags.s, "result has MSB set → S");
    assert!(!d.regs.flags.z);
    assert!(!d.regs.flags.c);

    let z = run_alu(0xAAAA_AAAA, 0xAAAA_AAAA, 0x3);
    assert!(z.regs.flags.z, "x ^ x == 0 → Z");
}

#[test]
fn alu_add_sets_carry_and_overflow() {
    // 0x8000_0000 + 0x8000_0000 = 0x1_0000_0000 (carry out of bit 31, two
    // negatives summing positive → signed overflow V).
    let d = run_alu(0x8000_0000, 0x8000_0000, 0x4);
    assert!(d.regs.flags.c, "carry out of bit 31");
    assert!(d.regs.flags.v, "neg + neg → positive: signed overflow");
}

#[test]
fn alu_sub_sets_borrow_and_sign() {
    // 0 - 1 borrows past bit 31 (C set) and the result is negative (S set).
    let borrow = run_alu(0, 1, 0x5);
    assert!(borrow.regs.flags.c, "0 - 1 borrows → C");
    assert!(borrow.regs.flags.s, "result is negative");
    assert!(!borrow.regs.flags.z);

    // Equal operands → zero result clears S/C and sets Z.
    let zero = run_alu(5, 5, 0x5);
    assert!(zero.regs.flags.z);
    assert!(!zero.regs.flags.s);
    assert!(!zero.regs.flags.c);
}

#[test]
fn alu_sr_arithmetic_shift_preserves_sign() {
    // SR shifts right 1, preserving the MSB (arithmetic). LSB → C.
    let d = run_alu(0x8000_0001, 0, 0x8);
    assert_eq!(
        d.regs.alu as u64 & 0xFFFF_FFFF,
        0xC000_0000,
        "MSB preserved, value halved"
    );
    assert!(d.regs.flags.c, "old LSB (1) shifted out → C");
    assert!(d.regs.flags.s, "still negative");
}

#[test]
fn alu_rr_rotate_right_wraps_lsb_to_msb() {
    // RR of 1 → 0x8000_0000, with old LSB into C.
    let d = run_alu(0x0000_0001, 0, 0x9);
    assert_eq!(d.regs.alu as u64 & 0xFFFF_FFFF, 0x8000_0000);
    assert!(d.regs.flags.c, "old LSB → C");
    assert!(d.regs.flags.s);
}

#[test]
fn alu_rl_rotate_left_wraps_msb_to_lsb() {
    // RL of 0x8000_0000 → 1, with old MSB into C.
    let d = run_alu(0x8000_0000, 0, 0xB);
    assert_eq!(d.regs.alu as u64 & 0xFFFF_FFFF, 0x0000_0001);
    assert!(d.regs.flags.c, "old MSB → C");
    assert!(!d.regs.flags.s);
}

#[test]
fn alu_ad2_adds_full_48bit_p_and_ac() {
    // AD2 = PH:PL + ACH:ACL across all 48 bits.
    let mut d = Dsp::new();
    d.regs.ph = 0x0001;
    d.regs.pl = 0x0000_0002;
    d.regs.ach = 0x0010;
    d.regs.acl = 0x0000_0003;
    d.load_program(0, &[op_alu(0x6), END]); // AD2
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.alu, 0x0011_0000_0005);
    assert!(!d.regs.flags.z);
    assert!(!d.regs.flags.s);
}

#[test]
fn alu_unknown_code_is_a_noop() {
    // ALU code 0x7 is undefined → no ALU register / flag change.
    let mut d = Dsp::new();
    d.regs.acl = 0x1234;
    d.regs.alu = 0x5555;
    d.load_program(0, &[op_alu(0x7), END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.alu, 0x5555, "unknown ALU op leaves ALU untouched");
}

// ---- X/Y-bus MOV [s],P and MOV [s],A ----

#[test]
fn x_bus_mov_s_p_sign_extends_into_ph() {
    // MOV [s],P (bits 24..23 = 0b11): PL = src, PH = sign extension.
    let mut d = Dsp::new();
    d.data_ram[0][0] = 0x8000_0000; // negative
    let mov_s_p = (0b11u32 << 23) | (MC0 << 20);
    d.load_program(0, &[op_alu(0) | mov_s_p, END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.pl, 0x8000_0000);
    assert_eq!(d.regs.ph, 0xFFFF, "negative source sign-extends PH");
    assert_eq!(d.regs.ct[0], 1, "MC source auto-incremented CT0");
}

#[test]
fn y_bus_mov_s_a_sign_extends_into_ach() {
    // MOV [s],A (bits 18..17 = 0b11): ACL = src, ACH = sign extension.
    let mut d = Dsp::new();
    d.data_ram[1][0] = 0x0000_00FF; // positive
    // Y-bus source field is bits 16..14; bank 1, MC (auto-inc) = 4|1 = 5.
    let mov_s_a = (0b11u32 << 17) | (5 << 14);
    d.load_program(0, &[op_alu(0) | mov_s_a, END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.acl, 0xFF);
    assert_eq!(d.regs.ach, 0, "positive source → ACH zero");
    assert_eq!(d.regs.ct[1], 1, "MC source auto-incremented CT1");
}

#[test]
fn mc_bank_read_by_both_x_and_y_increments_ct_once() {
    // X-bus and Y-bus both read MC0 in one word; per MAME the CT post-increment
    // happens once, not twice.
    let mut d = Dsp::new();
    d.data_ram[0][0] = 0x11;
    let word = op_alu(0) | mov_s_x(MC0) | mov_s_y(MC0);
    d.load_program(0, &[word, END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.rx, 0x11);
    assert_eq!(d.regs.ry, 0x11);
    assert_eq!(d.regs.ct[0], 1, "shared MC bank increments CT0 only once");
}

// ---- D1-bus source: ALL (0x9) / ALH (0xA) ----

#[test]
fn d1_source_all_reads_alu_low32() {
    // ALL (D1 source 0x9) = ALU low 32 bits.
    let mut d = Dsp::new();
    d.regs.alu = 0x0000_1234_5678_9ABC_u64 as i64;
    // MOV [ALL],RX
    d.load_program(0, &[op_alu(0) | d1_mov_s_d(DEST_RX, 0x9), END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.rx, 0x5678_9ABC);
}

#[test]
fn d1_source_alh_reads_alu_bits_47_16() {
    // ALH (D1 source 0xA) = ALU bits 47..16.
    let mut d = Dsp::new();
    d.regs.alu = 0x1234_5678_9ABC_u64 as i64;
    d.load_program(0, &[op_alu(0) | d1_mov_s_d(DEST_RX, 0xA), END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.rx, 0x1234_5678);
}

#[test]
fn d1_source_undefined_reads_zero() {
    // D1 source 0xB is not a defined source → reads 0.
    let mut d = Dsp::new();
    d.regs.rx = 0xDEAD;
    d.load_program(0, &[op_alu(0) | d1_mov_s_d(DEST_RX, 0xB), END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.rx, 0, "undefined D1 source yields 0");
}

// ---- D1/MVI destination selectors not covered elsewhere ----

#[test]
fn d1_dest_ra0_wa0_top_and_ct1_3() {
    // Exercise the dest-table entries 0x6 RA0, 0x7 WA0, 0xB TOP, 0xD/E/F CT1..3.
    let d = run(&[
        op_alu(0) | d1_mov_imm(0x6, 0x12), // RA0
        op_alu(0) | d1_mov_imm(0x7, 0x34), // WA0
        op_alu(0) | d1_mov_imm(0xB, 0x56), // TOP
        op_alu(0) | d1_mov_imm(0xD, 0x07), // CT1
        op_alu(0) | d1_mov_imm(0xE, 0x08), // CT2
        op_alu(0) | d1_mov_imm(0xF, 0x09), // CT3
        END,
    ]);
    assert_eq!(d.regs.ra0, 0x12);
    assert_eq!(d.regs.wa0, 0x34);
    assert_eq!(d.regs.top, 0x56);
    assert_eq!(d.regs.ct[1], 0x07);
    assert_eq!(d.regs.ct[2], 0x08);
    assert_eq!(d.regs.ct[3], 0x09);
}

#[test]
fn d1_imm_to_pl_sign_extends_ph() {
    // D1 dest 0x5 (PL) sign-extends PH from the value's sign, like MOV [s],P.
    let d = run(&[op_alu(0) | d1_mov_imm(DEST_PL, 0xFF), END]); // imm8 0xFF → -1
    assert_eq!(d.regs.pl, 0xFFFF_FFFF);
    assert_eq!(d.regs.ph, 0xFFFF);
}

#[test]
fn d1_dest_undefined_is_ignored() {
    // Dest 0x8 is not in the table → write silently dropped, no panic/change.
    let mut d = Dsp::new();
    d.regs.rx = 0x4242;
    d.load_program(0, &[op_alu(0) | d1_mov_imm(0x8, 0x99), END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(
        d.regs.rx, 0x4242,
        "undefined dest leaves registers untouched"
    );
}

// ---- MVI: conditional + MVI to PC ----

#[test]
fn conditional_mvi_taken_and_not_taken() {
    // Conditional MVI sets bit 25; cond in bits 25..19. cond 0x21 = test-true Z.
    let cond_mvi = |dest: u32, cond: u32, imm: i32| -> u32 {
        (0b10u32 << 30) | (dest << 26) | (1 << 25) | (cond << 19) | ((imm as u32) & 0x0007_FFFF)
    };

    let mut taken = Dsp::new();
    taken.regs.flags.z = true;
    taken.load_program(0, &[cond_mvi(DEST_RX, 0x21, 0x55), END]);
    taken.start(0);
    taken.run_until_stopped(16);
    assert_eq!(taken.regs.rx, 0x55, "Z set → conditional MVI taken");

    let mut nt = Dsp::new();
    nt.regs.flags.z = false;
    nt.regs.rx = 0xAA;
    nt.load_program(0, &[cond_mvi(DEST_RX, 0x21, 0x55), END]);
    nt.start(0);
    nt.run_until_stopped(16);
    assert_eq!(nt.regs.rx, 0xAA, "Z clear → conditional MVI skipped");
}

#[test]
fn mvi_to_pc_jumps_with_delay_slot_and_sets_top() {
    // MVI dest 0xC = PC: a jump. The next word is the delay slot; TOP latches
    // the return address (the PC after the MVI).
    let d = run(&[
        mvi(0xC, 4),                           // jump to 4
        op_alu(0) | d1_mov_imm(DEST_RX, 0x77), // delay slot, runs once
        NOP,
        NOP,
        END, // halts here (index 4)
    ]);
    assert_eq!(d.regs.rx, 0x77, "MVI-to-PC delay slot executed");
    assert_eq!(d.regs.top, 1, "TOP latched the post-MVI PC");
    assert_eq!(d.regs.pc, 5, "halted one past END at 4");
}

// ---- condition_met: all condition codes via conditional JMP ----

#[test]
fn jmp_conditions_cover_each_flag() {
    // Helper: does a "test-true" conditional JMP fire for the given flags?
    // cond low nibble selects the flag; 0x20 = test-true polarity. Taken →
    // delay slot (NOP) then END at target 3 → halt pc 4; not-taken → fall to
    // END at index 1 → halt pc 2.
    fn jumps(cond: u32, set: impl FnOnce(&mut Dsp)) -> bool {
        let mut d = Dsp::new();
        set(&mut d);
        d.load_program(0, &[jmp_cond(cond, 3), NOP, END, END]);
        d.start(0);
        d.run_until_stopped(16);
        let taken = d.regs.pc == 4;
        assert!(taken || d.regs.pc == 3, "unexpected halt pc {}", d.regs.pc);
        taken
    }

    // 0x21 Z, 0x22 S, 0x23 ZS, 0x24 C, 0x28 T0 — all test-true.
    assert!(jumps(0x21, |d| d.regs.flags.z = true));
    assert!(jumps(0x22, |d| d.regs.flags.s = true));
    assert!(jumps(0x23, |d| d.regs.flags.z = true));
    assert!(jumps(0x23, |d| d.regs.flags.s = true), "ZS = Z OR S");
    assert!(jumps(0x24, |d| d.regs.flags.c = true));
    assert!(jumps(0x28, |d| d.regs.flags.t0 = true));

    // Cleared flags → not taken under test-true polarity.
    assert!(!jumps(0x21, |_| {}));
    assert!(!jumps(0x28, |_| {}));
}

#[test]
fn jmp_condition_polarity_negates_the_test() {
    // Without bit 0x20 the test is negated: "if Z clear". cond 0x01 = !Z.
    fn jumps_neg_z(z: bool) -> bool {
        let mut d = Dsp::new();
        d.regs.flags.z = z;
        d.load_program(0, &[jmp_cond(0x01, 3), NOP, END, END]);
        d.start(0);
        d.run_until_stopped(16);
        d.regs.pc == 4
    }
    assert!(jumps_neg_z(false), "Z clear → !Z taken");
    assert!(!jumps_neg_z(true), "Z set → !Z not taken");
}

#[test]
fn jmp_condition_default_code_is_false() {
    // cond low nibble 0x0 (no flag) → result false; with test-true polarity it
    // never fires.
    let mut d = Dsp::new();
    d.load_program(0, &[jmp_cond(0x20, 3), NOP, END, END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.pc, 3, "cond 0 (false) under test-true → not taken");
}

// ---- LPS / BTM with LOP == 0 (no-op) ----

#[test]
fn lps_with_zero_lop_is_a_noop() {
    // LOP=0 → LPS does nothing (no repeat, no delay, no underflow of LOP).
    let mut d = Dsp::new();
    d.regs.lop = 0;
    d.load_program(0, &[LPS, op_alu(0) | d1_mov_imm(DEST_MC0, 1), END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.lop, 0, "LOP stays 0 (no wraparound)");
    // The next word still executes once as the normal next instruction.
    assert_eq!(d.regs.ct[0], 1);
}

#[test]
fn btm_with_zero_lop_is_a_noop() {
    let mut d = Dsp::new();
    d.regs.lop = 0;
    d.regs.top = 0;
    d.load_program(0, &[BTM, END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.lop, 0);
    assert!(d.stopped(), "fell through BTM to END");
}

// ---- DMA decode variants ----

#[test]
fn dma_from_dsp_with_hold_and_add_select() {
    // from_dsp (bit 12), hold (bit 14) → update_addr false, add_sel 3 → add 16,
    // bank 2, size 4.
    let mut d = Dsp::new();
    let dma = (0b11u32 << 30)   // control class, sub-op 00 (DMA)
        | (1 << 12)             // from_dsp
        | (1 << 14)             // hold
        | (3 << 15)             // add_sel = 3 → add 16
        | (2 << 8)              // bank 2
        | 4; // size
    d.load_program(0, &[dma, END]);
    d.start(0);
    d.step();
    assert_eq!(
        d.take_dma(),
        Some(DmaRequest {
            from_dsp: true,
            dsp_bank: 2,
            size: 4,
            add: 16,
            update_addr: false,
        })
    );
}

#[test]
fn dma_add_select_table() {
    // Map each add_sel code to its byte increment (immediate-size form).
    let add_for = |sel: u32| -> u32 {
        let mut d = Dsp::new();
        let dma = (0b11u32 << 30) | (sel << 15) | 1;
        d.load_program(0, &[dma, END]);
        d.start(0);
        d.step();
        d.take_dma().unwrap().add
    };
    assert_eq!(add_for(0), 0);
    assert_eq!(add_for(1), 4);
    assert_eq!(add_for(2), 4);
    assert_eq!(add_for(3), 16);
    assert_eq!(add_for(4), 16);
    assert_eq!(add_for(5), 64);
    assert_eq!(add_for(6), 128);
    assert_eq!(add_for(7), 256);
}

#[test]
fn dma_length_from_register_source() {
    // Bit 0x2000 set → size comes from a data-RAM source register, and add is
    // 0 when add_sel==0, else 4. Source MC0 (4) reads data_ram[0][CT0] then
    // auto-increments.
    let mut d = Dsp::new();
    d.data_ram[0][0] = 12;
    let dma = (0b11u32 << 30)   // DMA
        | (1 << 15)             // add_sel != 0 → add 4
        | 0x2000                // length-from-register
        | MC0; // source selector in low 4 bits
    d.load_program(0, &[dma, END]);
    d.start(0);
    d.step();
    let req = d.take_dma().unwrap();
    assert_eq!(req.size, 12, "size read from data RAM");
    assert_eq!(req.add, 4);
    assert_eq!(d.regs.ct[0], 1, "MC0 length source auto-incremented CT0");
}

#[test]
fn dma_length_from_register_with_zero_add_select() {
    let mut d = Dsp::new();
    d.data_ram[0][0] = 3;
    let dma = (0b11u32 << 30) | 0x2000 | MC0; // add_sel 0
    d.load_program(0, &[dma, END]);
    d.start(0);
    d.step();
    assert_eq!(d.take_dma().unwrap().add, 0, "add_sel 0 → no increment");
}

// ---- illegal op + program loading ----

#[test]
fn illegal_class_word_is_a_noop_then_continues() {
    // Class 01 (top bits 01) is illegal: executes with no effect, PC advances.
    let illegal = 0b01u32 << 30;
    let mut d = Dsp::new();
    d.regs.rx = 0x1357;
    d.load_program(0, &[illegal, END]);
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.rx, 0x1357, "illegal op had no effect");
    assert!(d.stopped(), "reached END after the illegal word");
}

#[test]
fn load_program_drops_words_past_program_ram() {
    // Loading at the very end clips anything past the 256-word window.
    let mut d = Dsp::new();
    d.load_program(255, &[0xAAAA_AAAA, 0xBBBB_BBBB, 0xCCCC_CCCC]);
    assert_eq!(d.program[255], 0xAAAA_AAAA);
    // Words 256, 257 are out of range → dropped, no panic.
}

#[test]
fn start_resets_execution_state() {
    // start() clears end/end-interrupt and arms the executing flag at addr.
    let mut d = Dsp::new();
    d.regs.flags.end = true;
    d.end_interrupt_pending = true;
    d.start(7);
    assert_eq!(d.regs.pc, 7);
    assert!(d.regs.flags.exec, "executing flag armed");
    assert!(!d.regs.flags.end, "end flag cleared");
    assert!(!d.end_interrupt_pending, "end-interrupt cleared");
    assert!(!d.stopped());
}

// ---- H3: ALU flags on native 32-bit width + sticky V, DMA count==0 ----

#[test]
fn alu_add_carry_and_zero_use_native_32bit_width() {
    // 0x80000000 + 0x80000000 = 0x1_0000_0000: low 32 bits are 0 (Z) with a
    // carry out of bit 31 (C). The old sign-extended-i64 math gave Z=0.
    let mut d = Dsp::new();
    d.regs.acl = 0x8000_0000;
    d.regs.pl = 0x8000_0000;
    d.load_program(0, &[op_alu(0x4), END]); // ADD
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.alu as u32, 0);
    assert!(d.regs.flags.z, "native 32-bit result is zero");
    assert!(d.regs.flags.c, "carry out of bit 31");
}

#[test]
fn alu_sub_borrow_uses_native_32bit_width() {
    // 0x80000000 - 1 = 0x7FFFFFFF does NOT borrow; the old i64 math set C.
    let mut d = Dsp::new();
    d.regs.acl = 0x8000_0000;
    d.regs.pl = 1;
    d.load_program(0, &[op_alu(0x5), END]); // SUB
    d.start(0);
    d.run_until_stopped(16);
    assert_eq!(d.regs.alu as u32, 0x7FFF_FFFF);
    assert!(!d.regs.flags.c, "0x80000000 - 1 does not borrow");
}

#[test]
fn alu_overflow_flag_is_sticky() {
    // ADD overflows (V set); a following non-overflowing SUB must NOT clear it
    // (sticky V — cleared only by a status-register read). Old `=` reset it.
    let mut d = Dsp::new();
    d.regs.acl = 0x7FFF_FFFF;
    d.regs.pl = 1;
    d.load_program(0, &[op_alu(0x4), op_alu(0x5), END]); // ADD (overflow) then SUB
    d.start(0);
    d.run_until_stopped(16);
    assert!(
        d.regs.flags.v,
        "the ADD overflow survives a later non-overflowing op"
    );
}

#[test]
fn dma_count_zero_transfers_256_words() {
    let mut d = Dsp::new();
    let dma = (0b11u32 << 30) | 0; // size field 0 → 256
    d.load_program(0, &[dma, END]);
    d.start(0);
    d.step();
    assert_eq!(d.take_dma().unwrap().size, 256, "count 0 means 256 words");
}
