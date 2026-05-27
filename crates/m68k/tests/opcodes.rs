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
