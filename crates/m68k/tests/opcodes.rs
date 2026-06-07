//! MC68EC000 opcode integration tests (increment 1).
//!
//! Each test plants opcode words at 0x1000, steps the CPU once (or a few
//! times for branch/return sequences), and asserts the post-state. CPUs are
//! built through `m68k::harness::MemBus`.

use m68k::Cpu;
use m68k::bus::{AccessKind, Bus};
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

// ---- increment 2: logic / immediate / compare / shift / DBcc ----------

#[test]
fn and_l_register_to_register() {
    // AND.L D1, D0 (<ea>,Dn) → 1100 000 010 000 001 = 0xC081
    let (mut cpu, mut bus) = boot(&[0xC081], |c| {
        c.regs.d[0] = 0xFF00_FF00;
        c.regs.d[1] = 0x0F0F_0F0F;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0F00_0F00);
    assert!(!cpu.regs.sr.v && !cpu.regs.sr.c);
}

#[test]
fn or_w_to_memory() {
    // OR.W D0, (A0) → 1000 000 101 010 000 = 0x8150
    let (mut cpu, mut bus) = boot(&[0x8150], |c| {
        c.regs.d[0] = 0x0000_0F0F;
        c.regs.a[0] = 0x3000;
    });
    bus.write_word(0x3000, 0xF0F0);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, m68k::AccessKind::Data).0, 0xFFFF);
}

#[test]
fn eor_l_data_register() {
    // EOR.L D0, D1 (Dn,<ea>) → 1011 000 110 000 001 = 0xB181
    let (mut cpu, mut bus) = boot(&[0xB181], |c| {
        c.regs.d[0] = 0xFFFF_FFFF;
        c.regs.d[1] = 0x0F0F_0F0F;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[1], 0xF0F0_F0F0);
}

#[test]
fn cmp_w_sets_flags_without_writing() {
    // CMP.W D1, D0 → 1011 000 001 000 001 = 0xB041 (D0 - D1)
    let (mut cpu, mut bus) = boot(&[0xB041], |c| {
        c.regs.d[0] = 0x0000_5000;
        c.regs.d[1] = 0x0000_5000;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_5000, "CMP does not write");
    assert!(cpu.regs.sr.z, "equal operands set Z");
}

#[test]
fn addi_l_immediate_to_register() {
    // ADDI.L #0x1000, D0 → 0000 011 0 10 000 000 = 0x0680 + long imm
    let (mut cpu, mut bus) = boot(&[0x0680, 0x0000, 0x1000], |c| c.regs.d[0] = 0x2345);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_3345);
}

#[test]
fn cmpi_w_compares_immediate() {
    // CMPI.W #0x20, D0 → 0000 110 0 01 000 000 = 0x0C40 + word imm
    let (mut cpu, mut bus) = boot(&[0x0C40, 0x0020], |c| c.regs.d[0] = 0x0000_0020);
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.z, "0x20 - 0x20 == 0");
}

#[test]
fn andi_to_ccr_clears_flags() {
    // ANDI #0x00, CCR → 0x023C + word imm (low byte = mask)
    let (mut cpu, mut bus) = boot(&[0x023C, 0x0000], |c| {
        c.regs.sr.c = true;
        c.regs.sr.z = true;
        c.regs.sr.n = true;
    });
    cpu.step(&mut bus);
    assert!(!cpu.regs.sr.c && !cpu.regs.sr.z && !cpu.regs.sr.n);
}

#[test]
fn swap_exchanges_register_halves() {
    // SWAP D0 → 0x4840
    let (mut cpu, mut bus) = boot(&[0x4840], |c| c.regs.d[0] = 0x1234_5678);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x5678_1234);
}

#[test]
fn ext_w_sign_extends_byte() {
    // EXT.W D0 → 0x4880
    let (mut cpu, mut bus) = boot(&[0x4880], |c| c.regs.d[0] = 0x0000_0080);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 0xFF80, "byte 0x80 → word 0xFF80");
}

#[test]
fn neg_l_negates() {
    // NEG.L D0 → 0100 0100 10 000 000 = 0x4480
    let (mut cpu, mut bus) = boot(&[0x4480], |c| c.regs.d[0] = 0x0000_0001);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0xFFFF_FFFF);
    assert!(cpu.regs.sr.n && cpu.regs.sr.c);
}

#[test]
fn not_w_complements() {
    // NOT.W D0 → 0100 0110 01 000 000 = 0x4640
    let (mut cpu, mut bus) = boot(&[0x4640], |c| c.regs.d[0] = 0x1111_0F0F);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x1111_F0F0, "only low word complemented");
}

#[test]
fn exg_data_registers() {
    // EXG D0, D1 → 0xC141
    let (mut cpu, mut bus) = boot(&[0xC141], |c| {
        c.regs.d[0] = 0xAAAA;
        c.regs.d[1] = 0xBBBB;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0xBBBB);
    assert_eq!(cpu.regs.d[1], 0xAAAA);
}

#[test]
fn lsl_l_immediate_count_sets_carry() {
    // LSL.L #1, D0 → 1110 001 1 10 0 01 000 = 0xE388
    let (mut cpu, mut bus) = boot(&[0xE388], |c| c.regs.d[0] = 0x8000_0000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_0000);
    assert!(cpu.regs.sr.c && cpu.regs.sr.x, "bit shifted out → C and X");
    assert!(cpu.regs.sr.z);
}

#[test]
fn asr_w_keeps_sign() {
    // ASR.W #1, D0 → 1110 001 0 01 0 00 000 = 0xE240
    let (mut cpu, mut bus) = boot(&[0xE240], |c| c.regs.d[0] = 0x0000_8000);
    cpu.step(&mut bus);
    assert_eq!(
        cpu.regs.d[0] & 0xFFFF,
        0xC000,
        "arithmetic shift keeps sign"
    );
}

#[test]
fn ror_b_rotates() {
    // ROR.B #1, D0 → 1110 001 0 00 0 11 000 = 0xE218
    let (mut cpu, mut bus) = boot(&[0xE218], |c| c.regs.d[0] = 0x0000_0001);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFF, 0x80, "LSB rotated to MSB");
    assert!(cpu.regs.sr.c);
}

#[test]
fn dbra_loops_until_counter_expires() {
    // DBRA D0, * (loop to self): DBcc with cond F (false) = DBRA.
    // 0101 0001 11001 000 = 0x51C8. The displacement is relative to the
    // extension word at 0x1002, so -2 lands back on the DBRA opcode (0x1000).
    let (mut cpu, mut bus) = boot(&[0x51C8, 0xFFFE], |c| c.regs.d[0] = 2);
    // First iteration: 2 → 1, branch taken (counter != -1).
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 1);
    assert_eq!(cpu.regs.pc, 0x1000, "branched back to the loop top");
    // Re-run from the top a couple more times.
    cpu.step(&mut bus); // 1 → 0, branch
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 0);
    cpu.step(&mut bus); // 0 → 0xFFFF, fall through
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 0xFFFF);
    assert_eq!(cpu.regs.pc, 0x1004, "fell through past the DBRA");
}

#[test]
fn scc_sets_byte_on_condition() {
    // SEQ D0 → 0101 0111 11 000 000 = 0x57C0 (cond Eq)
    let (mut cpu, mut bus) = boot(&[0x57C0], |c| c.regs.sr.z = true);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFF, 0xFF, "Scc true → 0xFF");
}

// ---- increment 3: MUL / DIV / bit ops / MOVEM / LINK / X-ops / BCD / TAS ----

#[test]
fn mulu_w_unsigned_16x16_to_32() {
    // MULU.W D1, D0 → 0xC0C1
    let (mut cpu, mut bus) = boot(&[0xC0C1], |c| {
        c.regs.d[0] = 0x0000_0003;
        c.regs.d[1] = 0x0000_0004;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 12);
    assert!(!cpu.regs.sr.z && !cpu.regs.sr.n);
}

#[test]
fn muls_w_signed() {
    // MULS.W D1, D0 → 0xC1C1
    let (mut cpu, mut bus) = boot(&[0xC1C1], |c| {
        c.regs.d[0] = 0x0000_FFFF; // -1 (word)
        c.regs.d[1] = 0x0000_0002;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0xFFFF_FFFE, "-1 × 2 = -2");
    assert!(cpu.regs.sr.n);
}

#[test]
fn divu_w_quotient_and_remainder() {
    // DIVU.W D1, D0 → 0x80C1
    let (mut cpu, mut bus) = boot(&[0x80C1], |c| {
        c.regs.d[0] = 17;
        c.regs.d[1] = 5;
    });
    cpu.step(&mut bus);
    // remainder (2) in high word, quotient (3) in low word.
    assert_eq!(cpu.regs.d[0], (2 << 16) | 3);
}

#[test]
fn divs_w_signed_quotient_and_remainder() {
    // DIVS.W D1, D0 → 0x81C1
    let (mut cpu, mut bus) = boot(&[0x81C1], |c| {
        c.regs.d[0] = (-17i32) as u32;
        c.regs.d[1] = 5;
    });
    cpu.step(&mut bus);
    // -17 / 5 = -3 rem -2 → (0xFFFE << 16) | 0xFFFD.
    assert_eq!(cpu.regs.d[0], 0xFFFE_FFFD);
    assert!(cpu.regs.sr.n);
}

#[test]
fn divu_overflow_sets_v() {
    // 0x10000 / 1 overflows a 16-bit quotient.
    let (mut cpu, mut bus) = boot(&[0x80C1], |c| {
        c.regs.d[0] = 0x0001_0000;
        c.regs.d[1] = 1;
    });
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.v, "quotient > 16 bits → V");
}

#[test]
fn btst_dynamic_sets_z_from_bit() {
    // BTST D1, D0 → 0x0300 (bit number in D1)
    let (mut cpu, mut bus) = boot(&[0x0300], |c| {
        c.regs.d[0] = 0x0000_0002; // bit 1 set
        c.regs.d[1] = 1;
    });
    cpu.step(&mut bus);
    assert!(!cpu.regs.sr.z, "bit 1 is set → Z clear");
    assert_eq!(cpu.regs.d[0], 0x0000_0002, "BTST does not modify");
}

#[test]
fn bset_static_sets_bit_and_reports_old() {
    // BSET #3, D0 → 0x08C0 + bit number word
    let (mut cpu, mut bus) = boot(&[0x08C0, 0x0003], |c| c.regs.d[0] = 0);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_0008, "bit 3 set");
    assert!(cpu.regs.sr.z, "old bit was 0 → Z set");
}

#[test]
fn bclr_static_clears_bit() {
    // BCLR #2, D0 → 0x0880 + word
    let (mut cpu, mut bus) = boot(&[0x0880, 0x0002], |c| c.regs.d[0] = 0x0F);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0B, "bit 2 cleared");
    assert!(!cpu.regs.sr.z, "old bit was 1");
}

#[test]
fn btst_on_memory_uses_byte_modulo_8() {
    // BTST #9, (A0) → 0x0810 + word (bit 9 & 7 = bit 1 of the byte)
    let (mut cpu, mut bus) = boot(&[0x0810, 0x0009], |c| c.regs.a[0] = 0x3000);
    bus.write8(0x3000, 0x02, m68k::AccessKind::Data);
    cpu.step(&mut bus);
    assert!(!cpu.regs.sr.z, "byte bit 1 set");
}

#[test]
fn movem_store_predecrement_then_load_postincrement_round_trips() {
    // MOVEM.L D0/D1, -(A7) → 0x48E7 mask 0xC000 ; then MOVEM.L (A7)+, D0/D1.
    let (mut cpu, mut bus) = boot(&[0x48E7, 0xC000, 0x4CDF, 0x0003], |c| {
        c.regs.d[0] = 0x1111_1111;
        c.regs.d[1] = 0x2222_2222;
    });
    cpu.step(&mut bus); // store
    assert_eq!(cpu.regs.a[7], 0x2000 - 8, "two longs pushed");
    assert_eq!(
        bus.read32(0x1FF8, m68k::AccessKind::Data).0,
        0x1111_1111,
        "D0 lowest"
    );
    assert_eq!(
        bus.read32(0x1FFC, m68k::AccessKind::Data).0,
        0x2222_2222,
        "D1 next"
    );
    // Clobber, then restore via load.
    cpu.regs.d[0] = 0;
    cpu.regs.d[1] = 0;
    cpu.step(&mut bus); // load
    assert_eq!(cpu.regs.d[0], 0x1111_1111);
    assert_eq!(cpu.regs.d[1], 0x2222_2222);
    assert_eq!(cpu.regs.a[7], 0x2000, "stack restored");
}

#[test]
fn link_and_unlk_build_and_collapse_a_frame() {
    // LINK A6, #-8 → 0x4E56 + 0xFFF8 ; UNLK A6 → 0x4E5E.
    let (mut cpu, mut bus) = boot(&[0x4E56, 0xFFF8, 0x4E5E], |c| {
        c.regs.a[6] = 0xDEAD_BEEF;
    });
    cpu.step(&mut bus); // LINK
    assert_eq!(cpu.regs.a[7], 0x2000 - 4 - 8, "old A6 pushed + frame grown");
    assert_eq!(cpu.regs.a[6], 0x2000 - 4, "A6 = frame pointer");
    cpu.step(&mut bus); // UNLK
    assert_eq!(cpu.regs.a[7], 0x2000, "stack restored");
    assert_eq!(cpu.regs.a[6], 0xDEAD_BEEF, "A6 restored");
}

#[test]
fn addx_l_adds_with_extend() {
    // ADDX.L D1, D0 → 0xD181
    let (mut cpu, mut bus) = boot(&[0xD181], |c| {
        c.regs.d[0] = 0x10;
        c.regs.d[1] = 0x20;
        c.regs.sr.x = true;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x31);
}

#[test]
fn subx_l_subtracts_with_extend() {
    // SUBX.L D1, D0 → 0x9181
    let (mut cpu, mut bus) = boot(&[0x9181], |c| {
        c.regs.d[0] = 0x30;
        c.regs.d[1] = 0x10;
        c.regs.sr.x = true;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x1F);
}

#[test]
fn negx_l_negates_with_borrow() {
    // NEGX.L D0 → 0x4080
    let (mut cpu, mut bus) = boot(&[0x4080], |c| {
        c.regs.d[0] = 1;
        c.regs.sr.x = false;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0xFFFF_FFFF);
}

#[test]
fn abcd_adds_packed_bcd() {
    // ABCD D1, D0 → 0xC101
    let (mut cpu, mut bus) = boot(&[0xC101], |c| {
        c.regs.d[0] = 0x25;
        c.regs.d[1] = 0x18;
        c.regs.sr.x = false;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFF, 0x43, "25 + 18 = 43 (BCD)");
}

#[test]
fn sbcd_subtracts_packed_bcd() {
    // SBCD D1, D0 → 0x8101
    let (mut cpu, mut bus) = boot(&[0x8101], |c| {
        c.regs.d[0] = 0x42;
        c.regs.d[1] = 0x18;
        c.regs.sr.x = false;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFF, 0x24, "42 - 18 = 24 (BCD)");
}

#[test]
fn tas_sets_flags_then_msb() {
    // TAS D0 → 0x4AC0
    let (mut cpu, mut bus) = boot(&[0x4AC0], |c| c.regs.d[0] = 0);
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.z, "byte was zero");
    assert_eq!(cpu.regs.d[0] & 0xFF, 0x80, "bit 7 set after test");
}

// ---- increment 4: exception model (traps, interrupts, privilege, RTE) ----

#[test]
fn trap_vectors_through_the_table() {
    // TRAP #0 → 0x4E40 ; vector 32 at byte 0x80.
    let (mut cpu, mut bus) = boot(&[0x4E40], |c| c.regs.sr.supervisor = true);
    bus.write_long(32 * 4, 0x0000_3000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x3000, "vectored to the TRAP #0 handler");
    assert!(cpu.regs.sr.supervisor, "exception enters supervisor mode");
    assert_eq!(cpu.regs.a[7], 0x2000 - 6, "SR + PC frame pushed");
}

#[test]
fn external_interrupt_is_taken_when_it_outranks_the_mask() {
    let (mut cpu, mut bus) = boot(&[0x4E71], |c| c.regs.sr.supervisor = true);
    bus.write_long((24 + 4) * 4, 0x0000_4000); // autovector level 4
    cpu.raise_interrupt(4);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x4000, "serviced the IRQ instead of the NOP");
    assert_eq!(cpu.regs.sr.imask, 4, "mask raised to the serviced level");
    assert_eq!(cpu.pending_irq, 0, "pending cleared on acknowledge");
}

#[test]
fn masked_interrupt_is_ignored() {
    let (mut cpu, mut bus) = boot(&[0x4E71], |c| {
        c.regs.sr.supervisor = true;
        c.regs.sr.imask = 5;
    });
    cpu.raise_interrupt(3); // 3 <= 5 → masked
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x1002, "NOP ran; IRQ stayed pending");
    assert_eq!(cpu.pending_irq, 3);
}

#[test]
fn level7_interrupt_is_non_maskable() {
    let (mut cpu, mut bus) = boot(&[0x4E71], |c| {
        c.regs.sr.supervisor = true;
        c.regs.sr.imask = 7;
    });
    bus.write_long((24 + 7) * 4, 0x0000_4444);
    cpu.raise_interrupt(7);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x4444, "level 7 ignores the mask");
}

#[test]
fn rte_restores_sr_and_pc() {
    // RTE → 0x4E73. Frame: SR at SSP, PC at SSP+2.
    let (mut cpu, mut bus) = boot(&[0x4E73], |c| {
        c.regs.sr.supervisor = true;
        c.regs.a[7] = 0x1F00;
    });
    bus.write_word(0x1F00, 0x2700); // restored SR: still supervisor, mask 7
    bus.write_long(0x1F02, 0x0000_5000); // restored PC
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x5000);
    assert_eq!(cpu.regs.a[7], 0x1F06, "supervisor frame popped");
    assert!(
        cpu.regs.sr.supervisor && cpu.regs.sr.imask == 7,
        "SR restored"
    );
}

#[test]
fn stop_parks_until_an_interrupt_wakes_it() {
    // STOP #0x2000 → 0x4E72 (SR = supervisor, mask 0).
    let (mut cpu, mut bus) = boot(&[0x4E72, 0x2000], |c| c.regs.sr.supervisor = true);
    cpu.step(&mut bus); // STOP
    assert!(cpu.stopped);
    let parked_pc = cpu.regs.pc;
    cpu.step(&mut bus); // idles
    assert_eq!(cpu.regs.pc, parked_pc, "no progress while stopped");
    bus.write_long((24 + 4) * 4, 0x0000_6000);
    cpu.raise_interrupt(4);
    cpu.step(&mut bus);
    assert!(!cpu.stopped, "interrupt woke the CPU");
    assert_eq!(cpu.regs.pc, 0x6000);
}

#[test]
fn privileged_instruction_in_user_mode_traps() {
    // STOP in user mode → privilege violation (vector 8).
    let (mut cpu, mut bus) = boot(&[0x4E72, 0x0000], |c| {
        c.regs.sr.supervisor = false;
        c.regs.ssp = 0x2000;
    });
    bus.write_long(8 * 4, 0x0000_7000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x7000, "vectored to the privilege handler");
    assert!(cpu.regs.sr.supervisor, "now in supervisor mode");
}

#[test]
fn illegal_instruction_traps() {
    // 0x4AFC is the reserved ILLEGAL opcode → vector 4.
    let (mut cpu, mut bus) = boot(&[0x4AFC], |c| c.regs.sr.supervisor = true);
    bus.write_long(4 * 4, 0x0000_8000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x8000);
}

#[test]
fn divide_by_zero_traps() {
    // DIVU.W D1, D0 with D1 = 0 → vector 5.
    let (mut cpu, mut bus) = boot(&[0x80C1], |c| {
        c.regs.sr.supervisor = true;
        c.regs.d[0] = 10;
        c.regs.d[1] = 0;
    });
    bus.write_long(5 * 4, 0x0000_9000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x9000);
}

#[test]
fn movep_l_stores_to_alternating_bytes() {
    // MOVEP.L D1, (0, A2) — opmode 111, mode 001, areg 2 → 0x03CA, disp 0.
    let (mut cpu, mut bus) = boot(&[0x03CA, 0x0000], |c| {
        c.regs.d[1] = 0xAABB_CCDD;
        c.regs.a[2] = 0x3000;
    });
    cpu.step(&mut bus);
    // High byte first, every other byte; odd bytes untouched.
    assert_eq!(bus.read8(0x3000, AccessKind::Data).0, 0xAA);
    assert_eq!(bus.read8(0x3002, AccessKind::Data).0, 0xBB);
    assert_eq!(bus.read8(0x3004, AccessKind::Data).0, 0xCC);
    assert_eq!(bus.read8(0x3006, AccessKind::Data).0, 0xDD);
    assert_eq!(
        bus.read8(0x3001, AccessKind::Data).0,
        0x00,
        "odd byte skipped"
    );
}

#[test]
fn movep_w_loads_from_alternating_bytes() {
    // MOVEP.W (0, A1), D3 — opmode 100, mode 001, areg 1 → 0x0709, disp 0.
    let (mut cpu, mut bus) = boot(&[0x0709, 0x0000], |c| {
        c.regs.a[1] = 0x3000;
        c.regs.d[3] = 0xFFFF_0000; // high word must be preserved
    });
    bus.write_word(0x3000, 0x1299); // even byte 0x12 read, odd 0x99 skipped
    bus.write_word(0x3002, 0x3477); // even byte 0x34 read
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[3], 0xFFFF_1234, "low word from even bytes only");
}

#[test]
fn memory_asl_shifts_a_word_in_place() {
    // ASL (A0) — memory single-bit, kind AS, left, EA (A0) → 0xE1D0.
    let (mut cpu, mut bus) = boot(&[0xE1D0], |c| c.regs.a[0] = 0x3000);
    bus.write_word(0x3000, 0x4001);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, AccessKind::Data).0, 0x8002);
    assert!(cpu.regs.sr.n, "result negative");
    assert!(cpu.regs.sr.v, "MSB changed → overflow");
    assert!(!cpu.regs.sr.c, "bit shifted out was 0");
}

#[test]
fn memory_ror_rotates_a_word_in_place() {
    // ROR (A0) — kind RO (3), right, EA (A0) → 0xE6D0.
    let (mut cpu, mut bus) = boot(&[0xE6D0], |c| c.regs.a[0] = 0x3000);
    bus.write_word(0x3000, 0x0001);
    cpu.step(&mut bus);
    assert_eq!(
        bus.read16(0x3000, AccessKind::Data).0,
        0x8000,
        "bit 0 → bit 15"
    );
    assert!(cpu.regs.sr.c, "rotated-out bit in carry");
}

// ---- increment 5: effective-address modes (coverage of resolve_ea) ----

#[test]
fn ea_predecrement_decrements_then_addresses() {
    // MOVE.L D0, -(A0) → 0010 000 100 000 000 = 0x2100.
    let (mut cpu, mut bus) = boot(&[0x2100], |c| {
        c.regs.d[0] = 0xCAFE_F00D;
        c.regs.a[0] = 0x3004;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[0], 0x3000, "long predecrement by 4");
    assert_eq!(bus.read32(0x3000, AccessKind::Data).0, 0xCAFE_F00D);
}

#[test]
fn ea_displacement_an_addresses_base_plus_disp() {
    // MOVE.W (4,A0), D0 → 0011 000 000 101 000 = 0x3028 ; disp = 4.
    let (mut cpu, mut bus) = boot(&[0x3028, 0x0004], |c| c.regs.a[0] = 0x3000);
    bus.write_word(0x3004, 0x1357);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 0x1357);
}

#[test]
fn ea_brief_index_word_sign_extends_index() {
    // MOVE.W (2,A0,D1.W), D0 → 0011 000 000 110 000 = 0x3030.
    // Brief ext: index D1 (word, no scale), disp8 = 2 → 0x1002.
    let (mut cpu, mut bus) = boot(&[0x3030, 0x1002], |c| {
        c.regs.a[0] = 0x3000;
        c.regs.d[1] = 0x0000_FFFE; // word index -2
    });
    // effective address = 0x3000 + 2 + (-2) = 0x3000.
    bus.write_word(0x3000, 0xBEEF);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 0xBEEF);
}

#[test]
fn ea_brief_index_long_uses_full_index() {
    // MOVE.W (0,A0,A1.L), D0 → 0x3030 ; ext: A-reg index 1, long (bit11), disp 0.
    // ext = 1000 1 000 0 0000000 = 0x9800.
    let (mut cpu, mut bus) = boot(&[0x3030, 0x9800], |c| {
        c.regs.a[0] = 0x3000;
        c.regs.a[1] = 0x0000_0010;
    });
    bus.write_word(0x3010, 0x4242);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 0x4242);
}

#[test]
fn ea_absolute_word_sign_extends() {
    // MOVE.W (xxx).W, D0 → 0011 000 000 111 000 = 0x3038 ; abs.W = 0x0040.
    let (mut cpu, mut bus) = boot(&[0x3038, 0x0040], |_| {});
    bus.write_word(0x0040, 0x9876);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 0x9876);
}

#[test]
fn ea_pc_displacement_addresses_relative_to_ext_word() {
    // MOVE.W (d16,PC), D0 → 0011 000 000 111 010 = 0x303A ; disp = 0x10.
    let (mut cpu, mut bus) = boot(&[0x303A, 0x0010], |_| {});
    // ext word at 0x1002; target = 0x1002 + 0x10.
    bus.write_word(0x1012, 0x0F0F);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 0x0F0F);
}

#[test]
fn ea_pc_brief_index_addresses_relative() {
    // MOVE.W (d8,PC,D1.W), D0 → 0x303B ; ext: D1 word, disp8 = 0x10.
    let (mut cpu, mut bus) = boot(&[0x303B, 0x1010], |c| c.regs.d[1] = 4);
    // base = 0x1002, target = 0x1002 + 0x10 + 4 = 0x1016.
    bus.write_word(0x1016, 0xABCD);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 0xABCD);
}

#[test]
fn ea_immediate_byte_takes_low_byte() {
    // ADD.B #imm,D0 via ADDI is elsewhere; use OR.B #imm,D0 source = immediate.
    // OR.B (xxx imm), D0 → 1000 000 000 111 100 = 0x803C ; imm word 0x00F0.
    let (mut cpu, mut bus) = boot(&[0x803C, 0x00F0], |c| c.regs.d[0] = 0x0000_000F);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFF, 0xFF, "byte immediate OR");
}

// ---- increment 6: ALU direction / ADDA-SUBA distinction (regression) ----

#[test]
fn add_b_to_memory_direction() {
    // ADD.B D0, (A0) → 1101 000 100 010 000 = 0xD110 (to-ea direction).
    let (mut cpu, mut bus) = boot(&[0xD110], |c| {
        c.regs.d[0] = 0x05;
        c.regs.a[0] = 0x3000;
    });
    bus.write8(0x3000, 0x10, AccessKind::Data);
    cpu.step(&mut bus);
    assert_eq!(bus.read8(0x3000, AccessKind::Data).0, 0x15);
}

#[test]
fn sub_l_to_memory_direction_sets_borrow() {
    // SUB.L D0, (A0) → 1001 000 110 010 000 = 0x9190.
    let (mut cpu, mut bus) = boot(&[0x9190], |c| {
        c.regs.d[0] = 0x0000_0002;
        c.regs.a[0] = 0x3000;
    });
    bus.write_long(0x3000, 0x0000_0001);
    cpu.step(&mut bus);
    assert_eq!(bus.read32(0x3000, AccessKind::Data).0, 0xFFFF_FFFF);
    assert!(cpu.regs.sr.c && cpu.regs.sr.x, "borrow sets C and X");
}

#[test]
fn adda_l_dn_to_an_does_not_decode_as_addx() {
    // Regression for the ADDA.L Dn,An vs ADDX bug: ADDA.L D0,A1 → 0xD3C0.
    // opmode 111 (long), addressing field 000 (Dn) — must accumulate the address.
    let (mut cpu, mut bus) = boot(&[0xD3C0], |c| {
        c.regs.d[0] = 0x0000_0100;
        c.regs.a[1] = 0x0000_2000;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[1], 0x0000_2100, "ADDA.L Dn,An accumulates");
}

#[test]
fn suba_l_dn_to_an_does_not_decode_as_subx() {
    // SUBA.L D0,A1 → 0x93C0. Same shared-pattern guard as ADDA.
    let (mut cpu, mut bus) = boot(&[0x93C0], |c| {
        c.regs.d[0] = 0x0000_0100;
        c.regs.a[1] = 0x0000_2000;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[1], 0x0000_1F00, "SUBA.L Dn,An subtracts");
}

#[test]
fn suba_w_sign_extends_source() {
    // SUBA.W D0,A1 → 0x92C0. Word source -1 sign-extends to a full long subtract.
    let (mut cpu, mut bus) = boot(&[0x92C0], |c| {
        c.regs.d[0] = 0x0000_FFFF; // -1 word
        c.regs.a[1] = 0x0000_1000;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[1], 0x0000_1001, "A1 -= (-1)");
}

#[test]
fn adda_l_memory_source_accumulates() {
    // ADDA.L (A0),A1 → opmode 111, mode 010 → 0xD3D0.
    let (mut cpu, mut bus) = boot(&[0xD3D0], |c| {
        c.regs.a[0] = 0x3000;
        c.regs.a[1] = 0x0000_1000;
    });
    bus.write_long(0x3000, 0x0000_0234);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[1], 0x0000_1234);
}

#[test]
fn addx_memory_predecrement_form() {
    // ADDX.L -(A1),-(A0) → 1101 000 1 10 00 1 001 = 0xD189.
    let (mut cpu, mut bus) = boot(&[0xD189], |c| {
        c.regs.a[0] = 0x3008; // dest -(A0) → 0x3004
        c.regs.a[1] = 0x3004; // src  -(A1) → 0x3000
        c.regs.sr.x = true;
    });
    bus.write_long(0x3000, 0x0000_0010); // source
    bus.write_long(0x3004, 0x0000_0020); // dest
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[0], 0x3004);
    assert_eq!(cpu.regs.a[1], 0x3000);
    assert_eq!(
        bus.read32(0x3004, AccessKind::Data).0,
        0x31,
        "0x20 + 0x10 + X"
    );
}

#[test]
fn subx_memory_predecrement_keeps_sticky_zero() {
    // SUBX.B -(A1),-(A0) → 1001 000 1 00 00 1 001 = 0x9109.
    let (mut cpu, mut bus) = boot(&[0x9109], |c| {
        c.regs.a[0] = 0x3001;
        c.regs.a[1] = 0x3001;
        c.regs.sr.x = false;
        c.regs.sr.z = true; // sticky-Z: equal bytes leave Z set
    });
    bus.write8(0x3000, 0x05, AccessKind::Data); // both read 0x05
    cpu.step(&mut bus);
    assert_eq!(bus.read8(0x3000, AccessKind::Data).0, 0x00, "5 - 5 - 0 = 0");
    assert!(cpu.regs.sr.z, "sticky Z preserved on zero result");
}

// ---- increment 7: immediate group to memory + to SR ----

#[test]
fn ori_b_to_memory() {
    // ORI.B #0xF0, (A0) → 0x0010 + word imm 0x00F0.
    let (mut cpu, mut bus) = boot(&[0x0010, 0x00F0], |c| c.regs.a[0] = 0x3000);
    bus.write8(0x3000, 0x0F, AccessKind::Data);
    cpu.step(&mut bus);
    assert_eq!(bus.read8(0x3000, AccessKind::Data).0, 0xFF);
    assert!(cpu.regs.sr.n);
}

#[test]
fn andi_w_to_memory() {
    // ANDI.W #0x0FF0, (A0) → 0x0250 + word imm.
    let (mut cpu, mut bus) = boot(&[0x0250, 0x0FF0], |c| c.regs.a[0] = 0x3000);
    bus.write_word(0x3000, 0xFFFF);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, AccessKind::Data).0, 0x0FF0);
}

#[test]
fn subi_l_to_memory_sets_flags() {
    // SUBI.L #1, (A0) → 0x0490 + long imm.
    let (mut cpu, mut bus) = boot(&[0x0490, 0x0000, 0x0001], |c| c.regs.a[0] = 0x3000);
    bus.write_long(0x3000, 0x0000_0000);
    cpu.step(&mut bus);
    assert_eq!(bus.read32(0x3000, AccessKind::Data).0, 0xFFFF_FFFF);
    assert!(cpu.regs.sr.c && cpu.regs.sr.n);
}

#[test]
fn addi_b_to_memory() {
    // ADDI.B #0x10, (A0) → 0x0610 + word imm.
    let (mut cpu, mut bus) = boot(&[0x0610, 0x0010], |c| c.regs.a[0] = 0x3000);
    bus.write8(0x3000, 0x20, AccessKind::Data);
    cpu.step(&mut bus);
    assert_eq!(bus.read8(0x3000, AccessKind::Data).0, 0x30);
}

#[test]
fn eori_w_to_memory() {
    // EORI.W #0xFFFF, (A0) → 0x0A50 + word imm.
    let (mut cpu, mut bus) = boot(&[0x0A50, 0xFFFF], |c| c.regs.a[0] = 0x3000);
    bus.write_word(0x3000, 0x0F0F);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, AccessKind::Data).0, 0xF0F0);
}

#[test]
fn cmpi_l_to_memory_does_not_modify() {
    // CMPI.L #0x5, (A0) → 0x0C90 + long imm.
    let (mut cpu, mut bus) = boot(&[0x0C90, 0x0000, 0x0005], |c| c.regs.a[0] = 0x3000);
    bus.write_long(0x3000, 0x0000_0005);
    cpu.step(&mut bus);
    assert_eq!(bus.read32(0x3000, AccessKind::Data).0, 0x0000_0005, "no write");
    assert!(cpu.regs.sr.z, "equal → Z");
}

#[test]
fn ori_to_sr_is_privileged_and_sets_system_byte() {
    // ORI #0x0700, SR → 0x007C + word imm (raise interrupt mask to 7).
    let (mut cpu, mut bus) = boot(&[0x007C, 0x0700], |c| {
        c.regs.sr.supervisor = true;
        c.regs.sr.imask = 0;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.sr.imask, 7, "OR into the SR system byte");
}

#[test]
fn eori_to_sr_toggles_bits() {
    // EORI #0x2000, SR → 0x0A7C + word imm: toggles the supervisor bit.
    let (mut cpu, mut bus) = boot(&[0x0A7C, 0x2000], |c| {
        c.regs.sr.supervisor = true;
        c.regs.ssp = 0x2000;
        c.regs.usp = 0x4000;
    });
    cpu.step(&mut bus);
    assert!(!cpu.regs.sr.supervisor, "S bit toggled off");
    assert_eq!(cpu.regs.a[7], 0x4000, "A7 banked to USP");
}

// ---- increment 8: 0x4 group remainder (RTR / RESET / TRAPV / CHK / etc) ----

#[test]
fn rtr_restores_ccr_and_pc_keeping_system_byte() {
    // RTR → 0x4E77. Frame: CCR word at SSP, PC at SSP+2.
    let (mut cpu, mut bus) = boot(&[0x4E77], |c| {
        c.regs.sr.supervisor = true;
        c.regs.sr.imask = 5;
        c.regs.a[7] = 0x1F00;
    });
    bus.write_word(0x1F00, 0x001F); // all CCR bits set
    bus.write_long(0x1F02, 0x0000_5000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x5000);
    assert!(cpu.regs.sr.c && cpu.regs.sr.v && cpu.regs.sr.z && cpu.regs.sr.n && cpu.regs.sr.x);
    assert_eq!(cpu.regs.sr.imask, 5, "system byte untouched by RTR");
}

#[test]
fn reset_instruction_is_a_nop_for_the_core() {
    // RESET → 0x4E70 (privileged).
    let (mut cpu, mut bus) = boot(&[0x4E70], |c| {
        c.regs.sr.supervisor = true;
        c.regs.d[0] = 0x99;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x1002, "RESET advances past the opcode");
    assert_eq!(cpu.regs.d[0], 0x99, "core state untouched");
}

#[test]
fn trapv_traps_only_when_overflow_set() {
    // TRAPV → 0x4E76, vector 7 at 0x1C.
    let (mut cpu, mut bus) = boot(&[0x4E76], |c| {
        c.regs.sr.supervisor = true;
        c.regs.sr.v = true;
    });
    bus.write_long(7 * 4, 0x0000_3000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x3000, "V set → TRAPV taken");

    // V clear → falls through.
    let (mut cpu, mut bus) = boot(&[0x4E76], |c| {
        c.regs.sr.supervisor = true;
        c.regs.sr.v = false;
    });
    bus.write_long(7 * 4, 0x0000_3000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x1002, "V clear → no trap");
}

#[test]
fn chk_traps_when_value_exceeds_bound() {
    // CHK D0,D1 → 0100 001 110 000 000 = 0x4380 (Dn = D1, ea = D0 bound).
    let (mut cpu, mut bus) = boot(&[0x4380], |c| {
        c.regs.sr.supervisor = true;
        c.regs.d[0] = 0x0000_0005; // bound
        c.regs.d[1] = 0x0000_0009; // value > bound
    });
    bus.write_long(6 * 4, 0x0000_2000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x2000, "value > bound → CHK trap");
    assert!(!cpu.regs.sr.n, "N clear when value > bound");
}

#[test]
fn chk_traps_on_negative_value() {
    let (mut cpu, mut bus) = boot(&[0x4380], |c| {
        c.regs.sr.supervisor = true;
        c.regs.d[0] = 0x0000_0010; // bound
        c.regs.d[1] = 0x0000_FFFF; // -1 word → negative
    });
    bus.write_long(6 * 4, 0x0000_2000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x2000, "negative value → CHK trap");
    assert!(cpu.regs.sr.n, "N set when value < 0");
}

#[test]
fn chk_in_bounds_falls_through() {
    let (mut cpu, mut bus) = boot(&[0x4380], |c| {
        c.regs.sr.supervisor = true;
        c.regs.d[0] = 0x0000_0010;
        c.regs.d[1] = 0x0000_0008;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x1002, "0 <= value <= bound → no trap");
}

#[test]
fn ext_l_sign_extends_word_to_long() {
    // EXT.L D0 → 0x48C0.
    let (mut cpu, mut bus) = boot(&[0x48C0], |c| c.regs.d[0] = 0x0000_8000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0xFFFF_8000, "word 0x8000 → long 0xFFFF8000");
    assert!(cpu.regs.sr.n);
}

#[test]
fn nbcd_negates_packed_bcd() {
    // NBCD D0 → 0x4800. 0 - 0x12 - 0 = 0x88 (BCD ten's complement) + borrow.
    let (mut cpu, mut bus) = boot(&[0x4800], |c| {
        c.regs.d[0] = 0x12;
        c.regs.sr.x = false;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFF, 0x88, "NBCD of 0x12");
    assert!(cpu.regs.sr.c, "borrow out");
}

#[test]
fn move_from_sr_stores_status_word() {
    // MOVE SR,D0 → 0x40C0.
    let (mut cpu, mut bus) = boot(&[0x40C0], |c| {
        c.regs.sr.supervisor = true;
        c.regs.sr.imask = 3;
        c.regs.sr.z = true;
    });
    cpu.step(&mut bus);
    let expected = cpu.regs.sr.to_u16() as u32;
    assert_eq!(cpu.regs.d[0] & 0xFFFF, expected & 0xFFFF);
}

#[test]
fn move_to_ccr_sets_condition_codes() {
    // MOVE #imm,CCR → 0x44FC + word imm (immediate source).
    let (mut cpu, mut bus) = boot(&[0x44FC, 0x001F], |c| {
        c.regs.sr.imask = 4;
        c.regs.sr.supervisor = true;
    });
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.c && cpu.regs.sr.v && cpu.regs.sr.z && cpu.regs.sr.n && cpu.regs.sr.x);
    assert_eq!(cpu.regs.sr.imask, 4, "system byte untouched by MOVE to CCR");
}

#[test]
fn move_to_sr_is_privileged() {
    // MOVE #0x2700,SR → 0x46FC + word imm. In user mode → privilege trap.
    let (mut cpu, mut bus) = boot(&[0x46FC, 0x2700], |c| {
        c.regs.sr.supervisor = false;
        c.regs.ssp = 0x2000;
    });
    bus.write_long(8 * 4, 0x0000_7000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x7000, "MOVE to SR in user mode traps");

    // In supervisor mode it loads the SR.
    let (mut cpu, mut bus) = boot(&[0x46FC, 0x2300], |c| c.regs.sr.supervisor = true);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.sr.imask, 3, "SR system byte loaded");
}

// ---- increment 9: NEG/NOT memory, EXG variants, CMPA/CMPM ----

#[test]
fn not_w_to_memory_complements_in_place() {
    // NOT.W (A0) → 0x4650.
    let (mut cpu, mut bus) = boot(&[0x4650], |c| c.regs.a[0] = 0x3000);
    bus.write_word(0x3000, 0x0F0F);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, AccessKind::Data).0, 0xF0F0);
    assert!(cpu.regs.sr.n);
}

#[test]
fn neg_b_to_memory() {
    // NEG.B (A0) → 0x4410.
    let (mut cpu, mut bus) = boot(&[0x4410], |c| c.regs.a[0] = 0x3000);
    bus.write8(0x3000, 0x01, AccessKind::Data);
    cpu.step(&mut bus);
    assert_eq!(bus.read8(0x3000, AccessKind::Data).0, 0xFF, "0 - 1 = -1");
    assert!(cpu.regs.sr.c);
}

#[test]
fn negx_b_to_memory_with_extend() {
    // NEGX.B (A0) → 0x4010, X set → 0 - 0 - 1 = -1.
    let (mut cpu, mut bus) = boot(&[0x4010], |c| {
        c.regs.a[0] = 0x3000;
        c.regs.sr.x = true;
    });
    bus.write8(0x3000, 0x00, AccessKind::Data);
    cpu.step(&mut bus);
    assert_eq!(bus.read8(0x3000, AccessKind::Data).0, 0xFF);
}

#[test]
fn exg_address_registers() {
    // EXG A0,A1 → 0xC149.
    let (mut cpu, mut bus) = boot(&[0xC149], |c| {
        c.regs.a[0] = 0x1111_1111;
        c.regs.a[1] = 0x2222_2222;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[0], 0x2222_2222);
    assert_eq!(cpu.regs.a[1], 0x1111_1111);
}

#[test]
fn exg_data_and_address_register() {
    // EXG D0,A1 → 0xC189.
    let (mut cpu, mut bus) = boot(&[0xC189], |c| {
        c.regs.d[0] = 0xAAAA_AAAA;
        c.regs.a[1] = 0xBBBB_BBBB;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0xBBBB_BBBB);
    assert_eq!(cpu.regs.a[1], 0xAAAA_AAAA);
}

#[test]
fn cmpa_w_sign_extends_and_compares_full_long() {
    // CMPA.W D0,A1 → 1011 001 011 000 000 = 0xB2C0.
    let (mut cpu, mut bus) = boot(&[0xB2C0], |c| {
        c.regs.d[0] = 0x0000_FFFF; // -1 word → sign-extended
        c.regs.a[1] = 0xFFFF_FFFF; // -1 long
    });
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.z, "-1 == -1 → Z");
}

#[test]
fn cmpa_l_compares_full_long() {
    // CMPA.L D0,A1 → opmode 111 → 0xB3C0.
    let (mut cpu, mut bus) = boot(&[0xB3C0], |c| {
        c.regs.d[0] = 0x0000_0010;
        c.regs.a[1] = 0x0000_0008;
    });
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.n, "8 - 16 < 0 → N");
    assert!(cpu.regs.sr.c, "borrow");
}

#[test]
fn cmpm_compares_and_post_increments_both() {
    // CMPM.W (A1)+,(A0)+ → 1011 000 101 001 001 = 0xB149.
    let (mut cpu, mut bus) = boot(&[0xB149], |c| {
        c.regs.a[0] = 0x3000; // Ax dest
        c.regs.a[1] = 0x3010; // Ay src
    });
    bus.write_word(0x3010, 0x1234); // source
    bus.write_word(0x3000, 0x1234); // dest
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.z, "equal operands");
    assert_eq!(cpu.regs.a[0], 0x3002, "Ax post-incremented");
    assert_eq!(cpu.regs.a[1], 0x3012, "Ay post-incremented");
}

#[test]
fn divs_overflow_sets_v_and_leaves_dn() {
    // DIVS.W with quotient out of i16 range → V, Dn unchanged.
    // 0x4000_0000 / 1 = 0x4000_0000 >> exceeds i16.
    let (mut cpu, mut bus) = boot(&[0x81C1], |c| {
        c.regs.d[0] = 0x4000_0000;
        c.regs.d[1] = 0x0000_0001;
    });
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.v, "signed quotient overflow → V");
    assert_eq!(cpu.regs.d[0], 0x4000_0000, "Dn left unmodified on overflow");
}

// ---- increment 10: shift/rotate register forms (ROXL/ROXR/ROL/ROR, count) ----

#[test]
fn roxl_l_rotates_through_extend() {
    // ROXL.L #1,D0 → 1110 001 1 10 1 10 000 = 0xE390.
    let (mut cpu, mut bus) = boot(&[0xE390], |c| {
        c.regs.d[0] = 0x8000_0000;
        c.regs.sr.x = true;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_0001, "MSB out, X rotated into bit 0");
    assert!(cpu.regs.sr.c && cpu.regs.sr.x, "old MSB → C and X");
}

#[test]
fn roxr_l_rotates_through_extend() {
    // ROXR.L #1,D0 → 1110 001 0 10 1 10 000 = 0xE290.
    let (mut cpu, mut bus) = boot(&[0xE290], |c| {
        c.regs.d[0] = 0x0000_0001;
        c.regs.sr.x = false;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_0000, "LSB out, X(0) into MSB");
    assert!(cpu.regs.sr.c && cpu.regs.sr.x, "old LSB → C and X");
}

#[test]
fn rol_l_register_count() {
    // ROL.L D1,D0 → register count form, count = bits 11..9 reg, bit 5 set.
    // 1110 001 1 10 1 01 000 ... actually ROL kind=3 left: 0xE3B8.
    let (mut cpu, mut bus) = boot(&[0xE3B8], |c| {
        c.regs.d[0] = 0x8000_0001;
        c.regs.d[1] = 1; // rotate by 1
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_0003, "MSB wraps to LSB");
    assert!(cpu.regs.sr.c, "rotated-out bit in carry");
}

#[test]
fn ror_l_register_count() {
    // ROR.L D1,D0 → kind 3 right, register count: 0xE2B8.
    let (mut cpu, mut bus) = boot(&[0xE2B8], |c| {
        c.regs.d[0] = 0x0000_0001;
        c.regs.d[1] = 1;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x8000_0000, "LSB wraps to MSB");
    assert!(cpu.regs.sr.c);
}

#[test]
fn lsr_l_immediate_clears_msb() {
    // LSR.L #1,D0 → 1110 001 0 10 0 01 000 = 0xE288 (count bits 11..9 = 001).
    let (mut cpu, mut bus) = boot(&[0xE288], |c| c.regs.d[0] = 0x0000_0003);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_0001);
    assert!(cpu.regs.sr.c && cpu.regs.sr.x, "bit 0 shifted out");
}

#[test]
fn asl_l_register_count_detects_overflow() {
    // ASL.L D1,D0 → 1110 001 1 10 1 00 000 = 0xE3A0, count via D1.
    let (mut cpu, mut bus) = boot(&[0xE3A0], |c| {
        c.regs.d[0] = 0x4000_0000;
        c.regs.d[1] = 1;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x8000_0000);
    assert!(cpu.regs.sr.v, "sign bit changed → V");
    assert!(cpu.regs.sr.n);
}

#[test]
fn shift_by_zero_count_clears_carry() {
    // LSR.L D1,D0 with D1=0 → no shift, C cleared, X untouched.
    // 1110 001 0 10 1 01 000 = 0xE2A8 (count register = D1).
    let (mut cpu, mut bus) = boot(&[0xE2A8], |c| {
        c.regs.d[0] = 0x0000_00FF;
        c.regs.d[1] = 0; // count 0
        c.regs.sr.c = true;
        c.regs.sr.x = true;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0x0000_00FF, "zero-count shift is a no-op");
    assert!(!cpu.regs.sr.c, "zero count clears C");
    assert!(cpu.regs.sr.x, "zero count leaves X untouched");
}

// ---- increment 11: memory shift kinds (LSR/ROXL/ROXR/ASR in memory) ----

#[test]
fn memory_lsr_shifts_word_right() {
    // LSR (A0) → kind 1 (LS) right → 0xE2D0.
    let (mut cpu, mut bus) = boot(&[0xE2D0], |c| c.regs.a[0] = 0x3000);
    bus.write_word(0x3000, 0x0003);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, AccessKind::Data).0, 0x0001);
    assert!(cpu.regs.sr.c && cpu.regs.sr.x);
}

#[test]
fn memory_asr_preserves_sign() {
    // ASR (A0) → kind 0 right → 0xE0D0.
    let (mut cpu, mut bus) = boot(&[0xE0D0], |c| c.regs.a[0] = 0x3000);
    bus.write_word(0x3000, 0x8000);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, AccessKind::Data).0, 0xC000, "sign preserved");
    assert!(cpu.regs.sr.n);
}

#[test]
fn memory_lsl_shifts_word_left() {
    // LSL (A0) → kind 1 left → 0xE3D0.
    let (mut cpu, mut bus) = boot(&[0xE3D0], |c| c.regs.a[0] = 0x3000);
    bus.write_word(0x3000, 0x8001);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, AccessKind::Data).0, 0x0002);
    assert!(cpu.regs.sr.c && cpu.regs.sr.x, "MSB out → C and X");
}

#[test]
fn memory_roxl_rotates_word_through_extend() {
    // ROXL (A0) → kind 2 left → 0xE5D0.
    let (mut cpu, mut bus) = boot(&[0xE5D0], |c| {
        c.regs.a[0] = 0x3000;
        c.regs.sr.x = true;
    });
    bus.write_word(0x3000, 0x0000);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, AccessKind::Data).0, 0x0001, "X into bit 0");
    assert!(!cpu.regs.sr.c, "old MSB was 0");
}

#[test]
fn memory_roxr_rotates_word_through_extend() {
    // ROXR (A0) → kind 2 right → 0xE4D0.
    let (mut cpu, mut bus) = boot(&[0xE4D0], |c| {
        c.regs.a[0] = 0x3000;
        c.regs.sr.x = true;
    });
    bus.write_word(0x3000, 0x0000);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, AccessKind::Data).0, 0x8000, "X into bit 15");
    assert!(!cpu.regs.sr.c, "old LSB was 0");
}

#[test]
fn memory_rol_rotates_word_left() {
    // ROL (A0) → kind 3 left → 0xE7D0.
    let (mut cpu, mut bus) = boot(&[0xE7D0], |c| c.regs.a[0] = 0x3000);
    bus.write_word(0x3000, 0x8000);
    cpu.step(&mut bus);
    assert_eq!(bus.read16(0x3000, AccessKind::Data).0, 0x0001, "MSB wraps");
    assert!(cpu.regs.sr.c);
}

// ---- increment 12: MOVEM control modes + word load sign-extend ----

#[test]
fn movem_store_to_control_mode_walks_ascending() {
    // MOVEM.L D0/A0, (A1) → 0x48D1 mask: D0 bit0, A0 bit8 → 0x0101.
    let (mut cpu, mut bus) = boot(&[0x48D1, 0x0101], |c| {
        c.regs.d[0] = 0x1111_1111;
        c.regs.a[0] = 0x2222_2222;
        c.regs.a[1] = 0x3000;
    });
    cpu.step(&mut bus);
    assert_eq!(bus.read32(0x3000, AccessKind::Data).0, 0x1111_1111, "D0 first");
    assert_eq!(bus.read32(0x3004, AccessKind::Data).0, 0x2222_2222, "A0 next");
}

#[test]
fn movem_load_word_sign_extends_into_registers() {
    // MOVEM.W (A1), D0/D1 → 0x4C91 mask 0x0003.
    let (mut cpu, mut bus) = boot(&[0x4C91, 0x0003], |c| c.regs.a[1] = 0x3000);
    bus.write_word(0x3000, 0x8000); // D0 ← sign-extended
    bus.write_word(0x3002, 0x0001); // D1
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0xFFFF_8000, "word load sign-extends");
    assert_eq!(cpu.regs.d[1], 0x0000_0001);
}

#[test]
fn movem_load_postincrement_advances_pointer() {
    // MOVEM.L (A0)+, D0/D1 → 0x4CD8 mask 0x0003.
    let (mut cpu, mut bus) = boot(&[0x4CD8, 0x0003], |c| c.regs.a[0] = 0x3000);
    bus.write_long(0x3000, 0xAAAA_AAAA);
    bus.write_long(0x3004, 0xBBBB_BBBB);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0], 0xAAAA_AAAA);
    assert_eq!(cpu.regs.d[1], 0xBBBB_BBBB);
    assert_eq!(cpu.regs.a[0], 0x3008, "A0 advanced past both longs");
}

// ---- increment 13: BTST on memory (no write-back) + MOVEA.L ----

#[test]
fn bchg_on_memory_toggles_byte_bit() {
    // BCHG #0, (A0) → 0x0850 + word imm 0.
    let (mut cpu, mut bus) = boot(&[0x0850, 0x0000], |c| c.regs.a[0] = 0x3000);
    bus.write8(0x3000, 0x00, AccessKind::Data);
    cpu.step(&mut bus);
    assert_eq!(bus.read8(0x3000, AccessKind::Data).0, 0x01, "bit toggled set");
    assert!(cpu.regs.sr.z, "old bit was 0");
}

#[test]
fn btst_on_memory_dynamic_does_not_write() {
    // BTST D1, (A0) → 0x0310 (dynamic, kind 0 = test only).
    let (mut cpu, mut bus) = boot(&[0x0310], |c| {
        c.regs.a[0] = 0x3000;
        c.regs.d[1] = 0; // bit 0
    });
    bus.write8(0x3000, 0x01, AccessKind::Data);
    cpu.step(&mut bus);
    assert!(!cpu.regs.sr.z, "bit 0 set");
    assert_eq!(bus.read8(0x3000, AccessKind::Data).0, 0x01, "BTST never writes");
}

#[test]
fn movea_l_loads_full_long_without_flags() {
    // MOVEA.L #imm,A0 → 0x207C + long imm.
    let (mut cpu, mut bus) = boot(&[0x207C, 0x1234, 0x5678], |c| c.regs.sr.z = true);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[0], 0x1234_5678, "full long, no sign-extend");
    assert!(cpu.regs.sr.z, "MOVEA leaves CCR untouched");
}

// ---- increment 14: line-A / line-F emulator traps ----

#[test]
fn line_a_opcode_traps_through_vector_10() {
    // 0xA000 is a line-A opcode → vector 10 (byte 0x28).
    let (mut cpu, mut bus) = boot(&[0xA000], |c| c.regs.sr.supervisor = true);
    bus.write_long(10 * 4, 0x0000_AAAA);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x0000_AAAA);
    // Frame: SR (word) at SSP, then the PC (long) — which must point at the
    // faulting instruction, not the one after it.
    assert_eq!(bus.read32(cpu.regs.a[7] + 2, AccessKind::Data).0, 0x1000);
}

#[test]
fn line_f_opcode_traps_through_vector_11() {
    // 0xF000 is a line-F opcode → vector 11 (byte 0x2C).
    let (mut cpu, mut bus) = boot(&[0xF000], |c| c.regs.sr.supervisor = true);
    bus.write_long(11 * 4, 0x0000_FFFF);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x0000_FFFF);
}

// ---- increment 15: BCD edge / DBcc condition-true / Scc false / addr ADDQ-sub ----

#[test]
fn abcd_with_carry_propagates_to_high_digit() {
    // ABCD with X set and a low-digit carry: 0x09 + 0x08 + 1 = 0x18.
    let (mut cpu, mut bus) = boot(&[0xC101], |c| {
        c.regs.d[0] = 0x09;
        c.regs.d[1] = 0x08;
        c.regs.sr.x = true;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFF, 0x18, "9 + 8 + 1 = 18 BCD");
    assert!(!cpu.regs.sr.c, "no high-digit carry out");
}

#[test]
fn sbcd_with_borrow_from_low_digit() {
    // SBCD: 0x10 - 0x01 - 0 = 0x09 (borrow into the low digit).
    let (mut cpu, mut bus) = boot(&[0x8101], |c| {
        c.regs.d[0] = 0x10;
        c.regs.d[1] = 0x01;
        c.regs.sr.x = false;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFF, 0x09, "10 - 1 = 9 BCD");
    assert!(!cpu.regs.sr.c);
}

#[test]
fn dbcc_condition_true_terminates_without_decrement() {
    // DBEQ D0,disp → 0101 0111 11001 000 = 0x57C8. Z set → loop terminates.
    let (mut cpu, mut bus) = boot(&[0x57C8, 0xFFFE], |c| {
        c.regs.sr.z = true;
        c.regs.d[0] = 5;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFFFF, 5, "condition true → counter untouched");
    assert_eq!(cpu.regs.pc, 0x1004, "fell through past the displacement");
}

#[test]
fn scc_false_clears_byte() {
    // SNE D0 → 0x56C0. Z set → Ne false → 0x00.
    let (mut cpu, mut bus) = boot(&[0x56C0], |c| {
        c.regs.sr.z = true;
        c.regs.d[0] = 0x0000_00FF;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[0] & 0xFF, 0x00, "Scc false → 0x00");
}

#[test]
fn subq_l_from_address_register_is_full_width() {
    // SUBQ.L #1,A0 → 0x5388.
    let (mut cpu, mut bus) = boot(&[0x5388], |c| {
        c.regs.a[0] = 0x0000_1000;
        c.regs.sr.c = true;
    });
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[0], 0x0000_0FFF);
    assert!(cpu.regs.sr.c, "address SUBQ sets no flags");
}

#[test]
fn movep_l_loads_from_alternating_bytes() {
    // MOVEP.L (0,A1),D3 — opmode 101 → 0x0749, disp 0.
    let (mut cpu, mut bus) = boot(&[0x0749, 0x0000], |c| c.regs.a[1] = 0x3000);
    bus.write8(0x3000, 0xDE, AccessKind::Data);
    bus.write8(0x3002, 0xAD, AccessKind::Data);
    bus.write8(0x3004, 0xBE, AccessKind::Data);
    bus.write8(0x3006, 0xEF, AccessKind::Data);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.d[3], 0xDEAD_BEEF, "long from even bytes only");
}

// ---- increment 16: BCD memory form, ANDI/ORI/EORI to CCR/SR variants ----

#[test]
fn abcd_memory_predecrement_form() {
    // ABCD -(A1),-(A0) → 1100 000 100 00 1 001 = 0xC109. The two predecremented
    // pointers must land on distinct bytes.
    let (mut cpu, mut bus) = boot(&[0xC109], |c| {
        c.regs.a[0] = 0x3004; // dest -(A0) → 0x3003
        c.regs.a[1] = 0x3001; // src  -(A1) → 0x3000
        c.regs.sr.x = false;
    });
    bus.write8(0x3000, 0x18, AccessKind::Data); // src
    bus.write8(0x3003, 0x25, AccessKind::Data); // dest
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.a[0], 0x3003);
    assert_eq!(cpu.regs.a[1], 0x3000);
    assert_eq!(
        bus.read8(0x3003, AccessKind::Data).0,
        0x43,
        "25 + 18 = 43 BCD"
    );
}

#[test]
fn sbcd_memory_predecrement_form() {
    // SBCD -(A1),-(A0) → 1000 000 100 00 1 001 = 0x8109.
    let (mut cpu, mut bus) = boot(&[0x8109], |c| {
        c.regs.a[0] = 0x3004; // dest → 0x3003
        c.regs.a[1] = 0x3001; // src  → 0x3000
        c.regs.sr.x = false;
    });
    bus.write8(0x3000, 0x18, AccessKind::Data); // src
    bus.write8(0x3003, 0x42, AccessKind::Data); // dest
    cpu.step(&mut bus);
    assert_eq!(
        bus.read8(0x3003, AccessKind::Data).0,
        0x24,
        "42 - 18 = 24 BCD"
    );
}

#[test]
fn ori_to_ccr_sets_condition_bits() {
    // ORI #0x01,CCR → 0x003C + word imm (set the C bit).
    let (mut cpu, mut bus) = boot(&[0x003C, 0x0001], |c| c.regs.sr.c = false);
    cpu.step(&mut bus);
    assert!(cpu.regs.sr.c, "C set by ORI to CCR");
}

#[test]
fn eori_to_ccr_toggles_condition_bits() {
    // EORI #0x04,CCR → 0x0A3C + word imm (toggle the Z bit).
    let (mut cpu, mut bus) = boot(&[0x0A3C, 0x0004], |c| c.regs.sr.z = true);
    cpu.step(&mut bus);
    assert!(!cpu.regs.sr.z, "Z toggled off by EORI to CCR");
}

#[test]
fn andi_to_sr_masks_system_byte_then_traces() {
    // ANDI #0xD0FF,SR → 0x027C + word imm: keep T (bit 15) and CCR, clear the
    // supervisor bit (bit 13) and the interrupt mask (bits 10..8 absent from the
    // mask). Privileged. Because T was set at the start of the instruction, a
    // trace exception (vector 9) fires the moment it retires — so the masking is
    // verified through the SR that the trace exception stacks.
    let (mut cpu, mut bus) = boot(&[0x027C, 0xD0FF], |c| {
        c.regs.sr.supervisor = true;
        c.regs.sr.trace = true;
        c.regs.sr.imask = 7;
        c.regs.ssp = 0x2000;
        c.regs.usp = 0x4000;
    });
    bus.write_word(0x0024, 0x0000); // trace vector (9 << 2 = 0x24)
    bus.write_word(0x0026, 0x3000);
    cpu.step(&mut bus);

    // The trace exception re-entered supervisor mode (banking A7 back to SSP),
    // cleared T, and vectored through the trace handler.
    assert!(cpu.regs.sr.supervisor, "trace exception entered supervisor");
    assert!(!cpu.regs.sr.trace, "T cleared on trace-exception entry");
    assert_eq!(cpu.regs.pc, 0x3000, "vectored through the trace handler");
    // The stacked SR (at A7) is the post-ANDI value: T kept (0x8000), S and the
    // interrupt mask cleared.
    let stacked_sr = Bus::read16(&mut bus, cpu.regs.a[7], AccessKind::Data).0;
    assert_eq!(stacked_sr & 0x8000, 0x8000, "ANDI kept T (bit 15 in mask)");
    assert_eq!(stacked_sr & 0x2000, 0, "ANDI cleared S (bit 13 not in mask)");
    assert_eq!(stacked_sr & 0x0700, 0, "ANDI cleared the interrupt mask");
}
