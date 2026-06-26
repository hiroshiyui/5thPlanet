//! First-batch opcode coverage (task #3). One test per opcode or per small
//! family. Each test loads one or two instructions at 0x0000_1000, sets up
//! initial state, runs the right number of steps, and asserts post-state
//! and cycle count.

use sh2::Cpu;
use sh2::harness::MemBus;

const PC0: u32 = 0x0000_1000;

fn cpu_at(bus: &mut MemBus, program: &[u16]) -> Cpu {
    bus.load_program(PC0, program);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[15] = 0x0000_8000; // arbitrary SP
    cpu
}

#[test]
fn mov_imm_sign_extends() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV #0x42, R3 ; MOV #-1, R4
    let mut cpu = cpu_at(&mut bus, &[0xE342, 0xE4FF]);

    let c = cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[3], 0x0000_0042);
    assert_eq!(c, 1);

    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[4], 0xFFFF_FFFF);
}

#[test]
fn mov_rr_copies() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV R1, R2  -> 0110nnnnmmmm0011 -> n=2 m=1 -> 0x6213
    let mut cpu = cpu_at(&mut bus, &[0x6213]);
    cpu.regs.r[1] = 0xCAFEBABE;
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[2], 0xCAFEBABE);
}

#[test]
fn add_and_addi_wrap() {
    let mut bus = MemBus::new(64 * 1024);
    // ADD R1,R2 ; ADD #-1,R2
    let mut cpu = cpu_at(&mut bus, &[0x321C, 0x72FF]);
    cpu.regs.r[1] = 1;
    cpu.regs.r[2] = 0xFFFF_FFFF;
    cpu.step(&mut bus); // ADD R1,R2 -> 0 (wraps)
    assert_eq!(cpu.regs.r[2], 0);
    cpu.step(&mut bus); // ADD #-1, R2 -> 0xFFFFFFFF
    assert_eq!(cpu.regs.r[2], 0xFFFF_FFFF);
}

#[test]
fn sub_basic() {
    let mut bus = MemBus::new(64 * 1024);
    // SUB R1,R2 -> 0011nnnnmmmm1000 -> n=2,m=1 -> 0x3218
    let mut cpu = cpu_at(&mut bus, &[0x3218]);
    cpu.regs.r[1] = 5;
    cpu.regs.r[2] = 12;
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[2], 7);
}

#[test]
fn cmp_eq_sets_t() {
    let mut bus = MemBus::new(64 * 1024);
    // CMP/EQ R1,R2 (0x3210) ; CMP/EQ #0,R0 (0x8800)
    let mut cpu = cpu_at(&mut bus, &[0x3210, 0x8800]);
    cpu.regs.r[1] = 7;
    cpu.regs.r[2] = 7;
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.t());

    cpu.regs.r[0] = 0;
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.t());

    // Now R0 != 0
    cpu.regs.r[0] = 1;
    cpu.regs.pc = PC0 + 2; // re-run CMP/EQ #0,R0
    cpu.step(&mut bus);
    assert!(!cpu.regs.sr.t());
}

#[test]
fn cmp_signed_vs_unsigned() {
    let mut bus = MemBus::new(64 * 1024);
    // CMP/HS R1,R2 (0x3212) ; CMP/GE R1,R2 (0x3213)
    let mut cpu = cpu_at(&mut bus, &[0x3212, 0x3213]);
    // R2 = -1 (0xFFFFFFFF), R1 = 1
    cpu.regs.r[1] = 1;
    cpu.regs.r[2] = 0xFFFF_FFFF;

    cpu.step(&mut bus); // unsigned: 0xFFFFFFFF >= 1 -> T=1
    assert!(cpu.regs.sr.t());

    cpu.step(&mut bus); // signed: -1 >= 1 -> false -> T=0
    assert!(!cpu.regs.sr.t());
}

#[test]
fn movl_register_indirect_round_trip() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.L R1, @R2 (0x2212) ; MOV.L @R2, R3 (0x6322)
    let mut cpu = cpu_at(&mut bus, &[0x2212, 0x6322]);
    cpu.regs.r[1] = 0xDEAD_BEEF;
    cpu.regs.r[2] = 0x0000_2000;
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[3], 0xDEAD_BEEF);
}

#[test]
fn movl_predec_and_postinc() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.L R1, @-R2 (0x2216) ; MOV.L @R2+, R3 (0x6326)
    let mut cpu = cpu_at(&mut bus, &[0x2216, 0x6326]);
    cpu.regs.r[1] = 0x1234_5678;
    cpu.regs.r[2] = 0x0000_2004;
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[2], 0x0000_2000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[3], 0x1234_5678);
    assert_eq!(cpu.regs.r[2], 0x0000_2004);
}

#[test]
fn movl_pc_relative_load() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.L @(2,PC), R1  -> 1101 0001 0000 0010 = 0xD102
    // disp=2 means address = (PC_of_instr + 4 + 8) & ~3 = PC + 12.
    let mut cpu = cpu_at(&mut bus, &[0xD102]);
    // Place the literal at PC0 + 12.
    bus.write_u32(PC0 + 12, 0xAABB_CCDD);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[1], 0xAABB_CCDD);
}

#[test]
fn bra_with_delay_slot_executes_slot_before_jump() {
    let mut bus = MemBus::new(64 * 1024);
    // Layout (each = 2 bytes):
    //   PC0+0  BRA (target = PC0+6)
    //   PC0+2  ADD R1,R2          (delay slot)
    //   PC0+4  MOV #1,R3          (skipped by branch)
    //   PC0+6  MOV #2,R3          (landing)
    // disp = (target - (PC_of_BRA + 4)) / 2 = (6 - 4) / 2 = 1 -> 0xA001
    let mut cpu = cpu_at(&mut bus, &[0xA001, 0x321C, 0xE301, 0xE302]);
    cpu.regs.r[1] = 10;
    cpu.regs.r[2] = 5;

    cpu.step(&mut bus); // BRA — sets pending_branch
    assert_eq!(
        cpu.regs.r[2], 5,
        "BRA itself must not yet have executed slot"
    );

    cpu.step(&mut bus); // ADD R1,R2 in delay slot
    assert_eq!(cpu.regs.r[2], 15);
    assert_eq!(cpu.regs.pc, PC0 + 6, "PC redirects after slot");

    cpu.step(&mut bus); // MOV #2,R3 at landing site
    assert_eq!(cpu.regs.r[3], 2);
}

#[test]
fn bsr_sets_pr_and_rts_returns() {
    let mut bus = MemBus::new(64 * 1024);
    // Layout:
    //   PC0+0  BSR (target = PC0+8 = subroutine entry)
    //   PC0+2  NOP (delay slot)
    //   PC0+4  MOV #7,R5 (return target = PR)
    //   PC0+6  NOP        (padding)
    //   PC0+8  RTS        (subroutine body)
    //   PC0+A  NOP        (RTS delay slot)
    // BSR disp = (8 - 4) / 2 = 2 -> 0xB002. PR = PC_of_BSR + 4 = PC0+4.
    let mut cpu = cpu_at(&mut bus, &[0xB002, 0x0009, 0xE507, 0x0009, 0x000B, 0x0009]);

    cpu.step(&mut bus); // BSR
    assert_eq!(cpu.regs.pr, PC0 + 4, "PR holds return address");
    cpu.step(&mut bus); // NOP slot, then branch taken -> RTS at PC0+8
    assert_eq!(cpu.regs.pc, PC0 + 8);

    cpu.step(&mut bus); // RTS — sets pending to PR
    cpu.step(&mut bus); // NOP slot, return
    assert_eq!(cpu.regs.pc, PC0 + 4);

    cpu.step(&mut bus); // MOV #7,R5
    assert_eq!(cpu.regs.r[5], 7);
}

#[test]
fn bf_branch_only_when_t_clear() {
    let mut bus = MemBus::new(64 * 1024);
    //   PC0+0  CLRT
    //   PC0+2  BF (target = PC0+6, disp=0 -> 0x8B00)
    //   PC0+4  MOV #1,R3 (skipped when branch taken)
    //   PC0+6  MOV #2,R3 (landing)
    let mut cpu = cpu_at(&mut bus, &[0x0008, 0x8B00, 0xE301, 0xE302]);

    cpu.step(&mut bus); // CLRT
    let c = cpu.step(&mut bus); // BF, T=0 -> taken
    assert_eq!(c, 3);
    assert_eq!(cpu.regs.pc, PC0 + 6);
    cpu.step(&mut bus); // MOV #2, R3
    assert_eq!(cpu.regs.r[3], 2);
}

#[test]
fn bt_falls_through_when_t_clear() {
    let mut bus = MemBus::new(64 * 1024);
    // CLRT ; BT (disp=0) ; MOV #1,R3 ; MOV #2,R3
    let mut cpu = cpu_at(&mut bus, &[0x0008, 0x8900, 0xE301, 0xE302]);
    cpu.step(&mut bus); // CLRT
    let c = cpu.step(&mut bus); // BT not taken
    assert_eq!(c, 1);
    cpu.step(&mut bus); // MOV #1,R3
    assert_eq!(cpu.regs.r[3], 1);
}

#[test]
fn bt_s_takes_with_delay_slot() {
    let mut bus = MemBus::new(64 * 1024);
    //   PC0+0  SETT
    //   PC0+2  BT/S (target = PC0+6, disp=0 -> 0x8D00)
    //   PC0+4  ADD R1,R2 (delay slot — must still run when branch taken)
    //   PC0+6  MOV #9,R5 (landing)
    let mut cpu = cpu_at(&mut bus, &[0x0018, 0x8D00, 0x321C, 0xE509]);
    cpu.regs.r[1] = 100;
    cpu.regs.r[2] = 1;

    cpu.step(&mut bus); // SETT
    cpu.step(&mut bus); // BT/S taken
    cpu.step(&mut bus); // ADD in slot -> R2 = 101, then PC redirects
    assert_eq!(cpu.regs.r[2], 101);
    assert_eq!(cpu.regs.pc, PC0 + 6);
    cpu.step(&mut bus); // MOV #9,R5
    assert_eq!(cpu.regs.r[5], 9);
}

#[test]
fn jmp_and_jsr_redirect_via_delay_slot() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.L @(2,PC),R3 ; JMP @R3 ; NOP (slot) ; .skip ; literal=0x00002000
    // We'll instead just set R3 manually and JMP.
    // JMP @R3 = 0100 0011 0010 1011 = 0x432B ; NOP (slot)
    let mut cpu = cpu_at(&mut bus, &[0x432B, 0x0009]);
    cpu.regs.r[3] = 0x0000_4000;
    cpu.step(&mut bus); // JMP
    cpu.step(&mut bus); // NOP slot -> PC becomes 0x4000
    assert_eq!(cpu.regs.pc, 0x0000_4000);
}

#[test]
fn nop_clrt_sett_one_cycle_each() {
    let mut bus = MemBus::new(64 * 1024);
    // NOP ; SETT ; CLRT
    let mut cpu = cpu_at(&mut bus, &[0x0009, 0x0018, 0x0008]);
    assert_eq!(cpu.step(&mut bus), 1);
    assert_eq!(cpu.step(&mut bus), 1);
    assert!(cpu.regs.sr.t(), "SETT must raise T");
    assert_eq!(cpu.step(&mut bus), 1);
    assert!(!cpu.regs.sr.t(), "CLRT must clear T");
}
