//! MC68EC000 opcode integration tests (increment 1).
//!
//! Each test plants opcode words at 0x1000, steps the CPU once (or a few
//! times for branch/return sequences), and asserts the post-state. CPUs are
//! built through `m68k::harness::MemBus`.

use m68k::Cpu;
use m68k::bus::Bus;
use m68k::harness::MemBus;

/// Build a bus with `words` planted at 0x1000 and a CPU pointed there with a
/// stack at 0x2000. `setup` seeds registers before the first step.
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

#[test]
fn reset_loads_ssp_and_pc_from_the_vector_table() {
    let mut bus = MemBus::new(0x1_0000);
    bus.write_long(0x0000, 0x0000_2000); // initial SSP
    bus.write_long(0x0004, 0x0000_1234); // initial PC
    let mut cpu = Cpu::new();
    cpu.reset(&mut bus);
    assert_eq!(cpu.regs.a[7], 0x0000_2000, "SSP from vector 0");
    assert_eq!(cpu.regs.pc, 0x0000_1234, "PC from vector 1");
    assert!(cpu.regs.sr.supervisor, "reset enters supervisor mode");
    assert_eq!(cpu.regs.sr.imask, 7, "reset masks all interrupts");
}

#[test]
fn moveq_sign_extends_and_sets_flags() {
    // MOVEQ #-2, D3  → 0x7600 | 0xFE
    let (mut cpu, mut bus) = boot(&[0x7600 | 0x00FE], |_| {});
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[3], 0xFFFF_FFFE);
    assert!(cpu.regs.sr.n, "negative");
    assert!(!cpu.regs.sr.z);
    assert!(!cpu.regs.sr.v && !cpu.regs.sr.c);
}

#[test]
fn move_l_immediate_to_data_register() {
    // MOVE.L #0xDEADBEEF, D0  → 0x203C + long immediate
    let (mut cpu, mut bus) = boot(&[0x203C, 0xDEAD, 0xBEEF], |_| {});
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0xDEAD_BEEF);
    assert!(cpu.regs.sr.n, "MSB set");
}

#[test]
fn move_w_register_to_register_preserves_upper_word() {
    // MOVE.W D1, D2 → 0011 010 000 000 001 = 0x3401
    let (mut cpu, mut bus) = boot(&[0x3401], |c| {
        c.regs.d[1] = 0x1111_8000;
        c.regs.d[2] = 0xAAAA_BBBB;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[2], 0xAAAA_8000, "only low word replaced");
    assert!(cpu.regs.sr.n, "0x8000 is negative as a word");
}

#[test]
fn move_l_register_to_memory_indirect() {
    // MOVE.L D0, (A0) → 0010 000 010 000 000 = 0x2080
    let (mut cpu, mut bus) = boot(&[0x2080], |c| {
        c.regs.d[0] = 0x1234_5678;
        c.regs.a[0] = 0x3000;
    });
    cpu.step(&mut bus);
    assert_eq!(bus.read32(0x3000, m68k::AccessKind::Data).0, 0x1234_5678);
}

#[test]
fn move_b_postincrement_advances_the_pointer() {
    // MOVE.B (A0)+, D0 → 0001 000 000 011 000 = 0x1018
    let (mut cpu, mut bus) = boot(&[0x1018], |c| {
        c.regs.a[0] = 0x3000;
    });
    bus.write8(0x3000, 0x7F, m68k::AccessKind::Data);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFF, 0x7F);
    assert_eq!(cpu.regs.a[0], 0x3001, "byte post-increment by 1");
}

#[test]
fn movea_w_sign_extends_into_the_full_address_register() {
    // MOVEA.W #0x8000, A0 → 0x307C + word immediate
    let (mut cpu, mut bus) = boot(&[0x307C, 0x8000], |c| {
        c.regs.sr.z = true; // MOVEA must not touch flags
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[0], 0xFFFF_8000, "word sign-extended to long");
    assert!(cpu.regs.sr.z, "MOVEA leaves CCR untouched");
}

#[test]
fn add_l_register_to_register_sets_carry_and_overflow() {
    // ADD.L D1, D0 → 1101 000 010 000 001 = 0xD081
    let (mut cpu, mut bus) = boot(&[0xD081], |c| {
        c.regs.d[0] = 0x8000_0000;
        c.regs.d[1] = 0x8000_0000;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_0000);
    assert!(cpu.regs.sr.z);
    assert!(cpu.regs.sr.c, "carry out of the top");
    assert!(cpu.regs.sr.v, "two negatives summed to a positive");
    assert!(cpu.regs.sr.x, "X follows C for ADD");
}

#[test]
fn sub_w_to_data_register_borrow() {
    // SUB.W D1, D0 → 1001 000 001 000 001 = 0x9041
    let (mut cpu, mut bus) = boot(&[0x9041], |c| {
        c.regs.d[0] = 0x0000_0001;
        c.regs.d[1] = 0x0000_0002;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 0xFFFF, "1 - 2 = -1");
    assert!(cpu.regs.sr.c, "borrow");
    assert!(cpu.regs.sr.n);
}

#[test]
fn adda_w_sign_extends_source_and_skips_flags() {
    // ADDA.W D0, A1 → 1101 001 011 000 000 = 0xD2C0
    let (mut cpu, mut bus) = boot(&[0xD2C0], |c| {
        c.regs.d[0] = 0x0000_FFFF; // -1 as a word
        c.regs.a[1] = 0x0000_1000;
        c.regs.sr.c = true;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[1], 0x0000_0FFF, "A1 += sign-extended(-1)");
    assert!(cpu.regs.sr.c, "ADDA leaves flags untouched");
}

#[test]
fn addq_l_to_data_register() {
    // ADDQ.L #1, D0 → 0x5280
    let (mut cpu, mut bus) = boot(&[0x5280], |c| c.regs.d[0] = 0xFF);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x100);
}

#[test]
fn addq_w_to_address_register_is_full_width_no_flags() {
    // ADDQ.W #8, A0 → 0x5048
    let (mut cpu, mut bus) = boot(&[0x5048], |c| {
        c.regs.a[0] = 0x0000_1000;
        c.regs.sr.z = true;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[0], 0x0000_1008);
    assert!(cpu.regs.sr.z, "address ADDQ sets no flags");
}

#[test]
fn clr_l_zeroes_and_sets_z() {
    // CLR.L D0 → 0x4280
    let (mut cpu, mut bus) = boot(&[0x4280], |c| c.regs.d[0] = 0xDEAD_BEEF);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0);
    assert!(cpu.regs.sr.z && !cpu.regs.sr.n);
}

#[test]
fn tst_w_sets_negative_without_modifying() {
    // TST.W D0 → 0x4A40
    let (mut cpu, mut bus) = boot(&[0x4A40], |c| c.regs.d[0] = 0x0000_8000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_8000, "TST does not modify");
    assert!(cpu.regs.sr.n);
    assert!(!cpu.regs.sr.z);
}

#[test]
fn bra_short_is_pc_relative_to_the_extension_point() {
    // BRA *+0x12 → 0x6010 (disp8 = 0x10, base = 0x1002)
    let (mut cpu, mut bus) = boot(&[0x6010], |_| {});
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x1002 + 0x10);
}

#[test]
fn beq_taken_only_when_zero_set() {
    // BEQ *+0x10 → 0x6710
    let (mut cpu, mut bus) = boot(&[0x6710], |c| c.regs.sr.z = true);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x1002 + 0x10, "BEQ taken when Z");

    let (mut cpu, mut bus) = boot(&[0x6710], |c| c.regs.sr.z = false);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x1002, "BEQ falls through when !Z");
}

#[test]
fn bsr_then_rts_round_trips_through_the_stack() {
    // BSR.W +0x10 (word form: 0x6100 + disp) at 0x1000; RTS at 0x1012.
    let (mut cpu, mut bus) = boot(&[0x6100, 0x0010], |_| {});
    bus.write_word(0x1012, 0x4E75); // RTS
    cpu.step(&mut bus); // BSR
    assert_eq!(cpu.regs.pc, 0x1002 + 0x10, "BSR target");
    assert_eq!(cpu.regs.a[7], 0x2000 - 4, "return pushed");
    assert_eq!(
        bus.read32(cpu.regs.a[7], m68k::AccessKind::Data).0,
        0x1004,
        "return = address after the BSR"
    );
    cpu.step(&mut bus); // RTS
    assert_eq!(cpu.regs.pc, 0x1004);
    assert_eq!(cpu.regs.a[7], 0x2000, "stack restored");
}

#[test]
fn jmp_absolute_long_sets_pc() {
    // JMP (xxx).L → 0x4EF9 + 32-bit target
    let (mut cpu, mut bus) = boot(&[0x4EF9, 0x0000, 0x1500], |_| {});
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x0000_1500);
}

#[test]
fn jsr_absolute_long_pushes_return_and_jumps() {
    // JSR (xxx).L → 0x4EB9 + target
    let (mut cpu, mut bus) = boot(&[0x4EB9, 0x0000, 0x1500], |_| {});
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x0000_1500);
    assert_eq!(cpu.regs.a[7], 0x2000 - 4);
    assert_eq!(bus.read32(cpu.regs.a[7], m68k::AccessKind::Data).0, 0x1006);
}

#[test]
fn lea_pc_relative_computes_the_address() {
    // LEA (d16,PC), A0 → 0x41FA + disp16. Base = address of the disp word.
    let (mut cpu, mut bus) = boot(&[0x41FA, 0x0020], |_| {});
    cpu.step(&mut bus);
    // disp word is at 0x1002; target = 0x1002 + 0x20.
    assert_eq!(cpu.regs.a[0], 0x1002 + 0x20);
}

#[test]
fn nop_only_advances_pc() {
    let (mut cpu, mut bus) = boot(&[0x4E71], |c| c.regs.d[0] = 0x42);
    let cyc = cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x1002);
    assert_eq!(cpu.regs.d[0], 0x42);
    assert_eq!(cyc, 4, "NOP is a single prefetch");
}
