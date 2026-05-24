//! Coverage for logical ops (AND/OR/XOR/NOT/TST) in register, R0-immediate,
//! and `*.B #imm,@(R0,GBR)` forms, plus TAS.B (task #4).

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
fn and_or_xor_not_register_forms() {
    let mut bus = MemBus::new(64 * 1024);
    // AND R1,R2 -> 0x2219 ; OR R1,R3 -> 0x231B ; XOR R1,R4 -> 0x241A ; NOT R1,R5 -> 0x6517
    let mut c = cpu(&mut bus, &[0x2219, 0x231B, 0x241A, 0x6517]);
    c.regs.r[1] = 0x0F0F_0F0F;
    c.regs.r[2] = 0xFFFF_FFFF;
    c.regs.r[3] = 0xF000_F000;
    c.regs.r[4] = 0xAAAA_AAAA;
    c.step(&mut bus);
    assert_eq!(c.regs.r[2], 0x0F0F_0F0F);
    c.step(&mut bus);
    assert_eq!(c.regs.r[3], 0xFF0F_FF0F);
    c.step(&mut bus);
    assert_eq!(c.regs.r[4], 0xA5A5_A5A5);
    c.step(&mut bus);
    assert_eq!(c.regs.r[5], !0x0F0F_0F0F);
}

#[test]
fn and_or_xor_immediate_target_r0() {
    let mut bus = MemBus::new(64 * 1024);
    // AND #0x0F,R0 -> 0xC90F ; OR #0xF0,R0 -> 0xCBF0 ; XOR #0xFF,R0 -> 0xCAFF
    let mut c = cpu(&mut bus, &[0xC90F, 0xCBF0, 0xCAFF]);
    c.regs.r[0] = 0xABCD_EFAA;
    c.step(&mut bus);
    assert_eq!(c.regs.r[0], 0x0000_000A);
    c.step(&mut bus);
    assert_eq!(c.regs.r[0], 0x0000_00FA);
    c.step(&mut bus);
    assert_eq!(c.regs.r[0], 0x0000_0005);
}

#[test]
fn tst_register_and_immediate() {
    let mut bus = MemBus::new(64 * 1024);
    // TST R1,R2 -> 0x2218 ; TST #0x01,R0 -> 0xC801
    let mut c = cpu(&mut bus, &[0x2218, 0xC801]);
    c.regs.r[1] = 0xFF00_0000;
    c.regs.r[2] = 0x00FF_FFFF;
    c.step(&mut bus); // overlap is 0 -> T=1
    assert!(c.regs.sr.t());

    c.regs.r[0] = 0xAA;
    c.step(&mut bus); // 0xAA & 0x01 = 0 -> T=1
    assert!(c.regs.sr.t());
}

#[test]
fn gbr_indirect_immediate_byte_ops() {
    let mut bus = MemBus::new(64 * 1024);
    // AND.B #0x0F, @(R0,GBR) -> 0xCD0F
    // OR.B  #0xF0, @(R0,GBR) -> 0xCFF0
    // XOR.B #0xFF, @(R0,GBR) -> 0xCEFF
    // TST.B #0x80, @(R0,GBR) -> 0xCC80
    let mut c = cpu(&mut bus, &[0xCD0F, 0xCFF0, 0xCEFF, 0xCC80]);
    c.regs.gbr = 0x4000;
    c.regs.r[0] = 5;
    bus.as_mut_slice()[0x4005] = 0xAB;
    c.step(&mut bus); // AND -> 0xAB & 0x0F = 0x0B
    assert_eq!(bus.as_slice()[0x4005], 0x0B);
    c.step(&mut bus); // OR  -> 0x0B | 0xF0 = 0xFB
    assert_eq!(bus.as_slice()[0x4005], 0xFB);
    c.step(&mut bus); // XOR -> 0xFB ^ 0xFF = 0x04
    assert_eq!(bus.as_slice()[0x4005], 0x04);
    c.step(&mut bus); // TST -> 0x04 & 0x80 = 0 -> T=1
    assert!(c.regs.sr.t());
}

#[test]
fn tas_sets_t_on_zero_and_sets_msb() {
    let mut bus = MemBus::new(64 * 1024);
    // TAS.B @R1 -> 0x411B
    let mut c = cpu(&mut bus, &[0x411B, 0x411B]);
    c.regs.r[1] = 0x4000;
    bus.as_mut_slice()[0x4000] = 0x00;
    c.step(&mut bus);
    assert!(c.regs.sr.t(), "byte was zero -> T=1");
    assert_eq!(bus.as_slice()[0x4000], 0x80);

    bus.as_mut_slice()[0x4000] = 0x40;
    c.step(&mut bus);
    assert!(!c.regs.sr.t(), "byte was non-zero -> T=0");
    assert_eq!(bus.as_slice()[0x4000], 0xC0, "MSB set in-place");
}
