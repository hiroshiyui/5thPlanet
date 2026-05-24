//! Coverage for byte/word MOV addressing modes, GBR-disp, R0-indexed,
//! SWAP, XTRCT, MOVT, and MOVA (task #4).

use sh2::Cpu;
use sh2::harness::MemBus;

const PC0: u32 = 0x0000_1000;

fn cpu(bus: &mut MemBus, program: &[u16]) -> Cpu {
    bus.load_program(PC0, program);
    let mut c = Cpu::new();
    c.regs.pc = PC0;
    c.regs.r[15] = 0x0000_8000;
    c
}

#[test]
fn mov_byte_load_sign_extends() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.B @R1, R2  -> 0110nnnnmmmm0000 -> 0x6210
    let mut c = cpu(&mut bus, &[0x6210]);
    bus.as_mut_slice()[0x2000] = 0xFE; // -2
    c.regs.r[1] = 0x2000;
    c.step(&mut bus);
    assert_eq!(c.regs.r[2], 0xFFFF_FFFE);
}

#[test]
fn mov_word_store_truncates() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.W R1, @R2 -> 0010nnnnmmmm0001 -> 0x2211
    let mut c = cpu(&mut bus, &[0x2211]);
    c.regs.r[1] = 0xCAFE_BABE;
    c.regs.r[2] = 0x3000;
    c.step(&mut bus);
    // Big-endian halfword at 0x3000 == 0xBABE
    assert_eq!(&bus.as_slice()[0x3000..0x3002], &[0xBA, 0xBE]);
}

#[test]
fn mov_byte_predec_postinc_round_trip() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.B R1, @-R2 -> 0x2214 ; MOV.B @R2+, R3 -> 0x6324
    let mut c = cpu(&mut bus, &[0x2214, 0x6324]);
    c.regs.r[1] = 0x5A;
    c.regs.r[2] = 0x2001;
    c.step(&mut bus);
    assert_eq!(c.regs.r[2], 0x2000);
    c.step(&mut bus);
    assert_eq!(c.regs.r[3], 0x5A);
    assert_eq!(c.regs.r[2], 0x2001);
}

#[test]
fn mov_r0_disp_store_load() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.B R0, @(3, R1) -> 1000 0000 0001 0011 = 0x8013
    // MOV.B @(3, R1), R0 -> 1000 0100 0001 0011 = 0x8413
    let mut c = cpu(&mut bus, &[0x8013, 0x8413]);
    c.regs.r[0] = 0x77;
    c.regs.r[1] = 0x4000;
    c.step(&mut bus);
    assert_eq!(bus.as_slice()[0x4003], 0x77);
    c.regs.r[0] = 0; // clear, prove load restores it
    c.step(&mut bus);
    assert_eq!(c.regs.r[0], 0x77);
}

#[test]
fn mov_word_pc_relative_sign_extends() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.W @(1,PC), R3 -> 1001 0011 0000 0001 = 0x9301
    // Literal addr = PC + 4 + 1*2 = PC + 6.
    let mut c = cpu(&mut bus, &[0x9301]);
    bus.write_u16(PC0 + 6, 0xFFFE);
    c.step(&mut bus);
    assert_eq!(c.regs.r[3], 0xFFFF_FFFE);
}

#[test]
fn mov_l_r0_indexed() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.L R1, @(R0,R2) -> 0000nnnnmmmm0110 -> 0x0216
    // MOV.L @(R0,R2), R3 -> 0000nnnnmmmm1110 -> 0x032E
    let mut c = cpu(&mut bus, &[0x0216, 0x032E]);
    c.regs.r[0] = 0x10;
    c.regs.r[1] = 0xABCD_1234;
    c.regs.r[2] = 0x3000;
    c.step(&mut bus);
    assert_eq!(
        &bus.as_slice()[0x3010..0x3014],
        &[0xAB, 0xCD, 0x12, 0x34]
    );
    c.step(&mut bus);
    assert_eq!(c.regs.r[3], 0xABCD_1234);
}

#[test]
fn mov_gbr_disp() {
    let mut bus = MemBus::new(64 * 1024);
    // MOV.L R0, @(2, GBR) -> 11000010 dddddddd = 0xC202 ; load back -> 0xC602
    let mut c = cpu(&mut bus, &[0xC202, 0xC602]);
    c.regs.gbr = 0x5000;
    c.regs.r[0] = 0xDEAD_BEEF;
    c.step(&mut bus); // store at GBR + 8
    assert_eq!(
        &bus.as_slice()[0x5008..0x500C],
        &[0xDE, 0xAD, 0xBE, 0xEF]
    );
    c.regs.r[0] = 0;
    c.step(&mut bus);
    assert_eq!(c.regs.r[0], 0xDEAD_BEEF);
}

#[test]
fn mova_computes_pc_relative_address_into_r0() {
    let mut bus = MemBus::new(64 * 1024);
    // MOVA @(2, PC), R0 -> 11000111 00000010 = 0xC702
    // R0 should become (PC + 4 + 2*4) & ~3 = PC + 12.
    let mut c = cpu(&mut bus, &[0xC702]);
    c.step(&mut bus);
    assert_eq!(c.regs.r[0], PC0 + 12);
}

#[test]
fn movt_copies_t_bit() {
    let mut bus = MemBus::new(64 * 1024);
    // SETT ; MOVT R3 (0x0329) ; CLRT ; MOVT R4 (0x0429)
    let mut c = cpu(&mut bus, &[0x0018, 0x0329, 0x0008, 0x0429]);
    c.step(&mut bus);
    c.step(&mut bus);
    assert_eq!(c.regs.r[3], 1);
    c.step(&mut bus);
    c.step(&mut bus);
    assert_eq!(c.regs.r[4], 0);
}

#[test]
fn swap_b_swaps_low_two_bytes() {
    let mut bus = MemBus::new(64 * 1024);
    // SWAP.B R1, R2 -> 0110nnnnmmmm1000 -> 0x6218
    let mut c = cpu(&mut bus, &[0x6218]);
    c.regs.r[1] = 0x1122_AABB;
    c.step(&mut bus);
    assert_eq!(c.regs.r[2], 0x1122_BBAA);
}

#[test]
fn swap_w_swaps_halfwords() {
    let mut bus = MemBus::new(64 * 1024);
    // SWAP.W R1, R2 -> 0x6219
    let mut c = cpu(&mut bus, &[0x6219]);
    c.regs.r[1] = 0x1234_5678;
    c.step(&mut bus);
    assert_eq!(c.regs.r[2], 0x5678_1234);
}

#[test]
fn xtrct_takes_middle_32_bits() {
    let mut bus = MemBus::new(64 * 1024);
    // XTRCT R1, R2 -> 0010nnnnmmmm1101 -> 0x221D
    let mut c = cpu(&mut bus, &[0x221D]);
    c.regs.r[1] = 0xAAAA_BBBB;
    c.regs.r[2] = 0xCCCC_DDDD;
    c.step(&mut bus);
    // Rn = (Rm.low << 16) | (Rn.high)
    assert_eq!(c.regs.r[2], 0xBBBB_CCCC);
}
