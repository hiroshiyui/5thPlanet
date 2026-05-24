//! Coverage for SHLL/SHLR/SHAL/SHAR, ROTL/R, ROTCL/R, and the multi-bit
//! SHLLn/SHLRn variants (task #4).

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
fn shll_pushes_msb_into_t() {
    let mut bus = MemBus::new(64 * 1024);
    // SHLL R1 -> 0x4100
    let mut c = cpu(&mut bus, &[0x4100]);
    c.regs.r[1] = 0x8000_0001;
    c.step(&mut bus);
    assert_eq!(c.regs.r[1], 0x0000_0002);
    assert!(c.regs.sr.t());
}

#[test]
fn shlr_pushes_lsb_into_t() {
    let mut bus = MemBus::new(64 * 1024);
    // SHLR R1 -> 0x4101
    let mut c = cpu(&mut bus, &[0x4101]);
    c.regs.r[1] = 0x8000_0001;
    c.step(&mut bus);
    assert_eq!(c.regs.r[1], 0x4000_0000);
    assert!(c.regs.sr.t());
}

#[test]
fn shar_arithmetic_right() {
    let mut bus = MemBus::new(64 * 1024);
    // SHAR R1 -> 0x4121
    let mut c = cpu(&mut bus, &[0x4121]);
    c.regs.r[1] = 0x8000_0000;
    c.step(&mut bus);
    assert_eq!(c.regs.r[1], 0xC000_0000);
    assert!(!c.regs.sr.t());
}

#[test]
fn rotl_rotr_no_carry() {
    let mut bus = MemBus::new(64 * 1024);
    // ROTL R1 -> 0x4104 ; ROTR R1 -> 0x4105
    let mut c = cpu(&mut bus, &[0x4104, 0x4105]);
    c.regs.r[1] = 0x8000_0001;
    c.step(&mut bus); // ROTL
    assert_eq!(c.regs.r[1], 0x0000_0003);
    assert!(c.regs.sr.t());
    c.step(&mut bus); // ROTR
    assert_eq!(c.regs.r[1], 0x8000_0001);
    assert!(c.regs.sr.t());
}

#[test]
fn rotcl_rotcr_chain_through_t() {
    let mut bus = MemBus::new(64 * 1024);
    // CLRT ; ROTCL R1 -> 0x0008, 0x4124
    let mut c = cpu(&mut bus, &[0x0008, 0x4124]);
    c.regs.r[1] = 0x8000_0001;
    c.step(&mut bus); // CLRT
    c.step(&mut bus); // ROTCL: T(0) shifts in at bit 0, bit31 -> T
    assert_eq!(c.regs.r[1], 0x0000_0002);
    assert!(c.regs.sr.t());
}

#[test]
fn shlln_shlrn_multi_bit_shifts() {
    let mut bus = MemBus::new(64 * 1024);
    // SHLL2 R1 -> 0x4108 ; SHLL8 R1 -> 0x4118 ; SHLL16 R1 -> 0x4128
    // SHLR2 R2 -> 0x4209 ; SHLR8 R2 -> 0x4219 ; SHLR16 R2 -> 0x4229
    let mut c = cpu(
        &mut bus,
        &[0x4108, 0x4118, 0x4128, 0x4209, 0x4219, 0x4229],
    );
    c.regs.r[1] = 0x0000_0001;
    c.step(&mut bus);
    assert_eq!(c.regs.r[1], 4);
    c.step(&mut bus);
    assert_eq!(c.regs.r[1], 0x400);
    c.step(&mut bus);
    assert_eq!(c.regs.r[1], 0x0400_0000);

    c.regs.r[2] = 0xFFFF_FFFF;
    c.step(&mut bus);
    assert_eq!(c.regs.r[2], 0x3FFF_FFFF);
    c.step(&mut bus);
    assert_eq!(c.regs.r[2], 0x003F_FFFF);
    c.step(&mut bus);
    assert_eq!(c.regs.r[2], 0x0000_003F);
}
