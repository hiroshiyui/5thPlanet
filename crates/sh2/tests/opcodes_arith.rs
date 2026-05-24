//! Coverage for ADDC/ADDV/SUBC/SUBV/NEG/NEGC, DT, CMP/PL/PZ/STR, EXT,
//! multiplies (MUL.L, MULS/U.W, DMULS/U.L), MAC, DIV0/DIV1 (task #4).

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
fn addc_chains_carry_via_t() {
    let mut bus = MemBus::new(64 * 1024);
    // CLRT ; ADDC R1,R2 ; ADDC R3,R4
    // -> 0x0008 ; 0x321E ; 0x343E
    let mut c = cpu(&mut bus, &[0x0008, 0x321E, 0x343E]);
    // First add: 0xFFFFFFFF + 1 = 0 with carry
    c.regs.r[1] = 1;
    c.regs.r[2] = 0xFFFF_FFFF;
    c.regs.r[3] = 5;
    c.regs.r[4] = 10;

    c.step(&mut bus); // CLRT
    c.step(&mut bus); // ADDC R1,R2
    assert_eq!(c.regs.r[2], 0);
    assert!(c.regs.sr.t(), "carry must propagate to T");
    c.step(&mut bus); // ADDC R3,R4 with T=1
    assert_eq!(c.regs.r[4], 16);
    assert!(!c.regs.sr.t());
}

#[test]
fn addv_sets_t_on_signed_overflow() {
    let mut bus = MemBus::new(64 * 1024);
    // ADDV R1,R2 -> 0x321F
    let mut c = cpu(&mut bus, &[0x321F]);
    c.regs.r[1] = 1;
    c.regs.r[2] = i32::MAX as u32;
    c.step(&mut bus);
    assert!(c.regs.sr.t(), "signed overflow must set T");
    assert_eq!(c.regs.r[2], i32::MIN as u32);
}

#[test]
fn subc_borrow_into_t() {
    let mut bus = MemBus::new(64 * 1024);
    // SETT ; SUBC R1,R2  -> 0x0018, 0x321A
    let mut c = cpu(&mut bus, &[0x0018, 0x321A]);
    c.regs.r[1] = 5;
    c.regs.r[2] = 5; // 5 - 5 - 1 = -1, borrow out
    c.step(&mut bus); // SETT
    c.step(&mut bus); // SUBC
    assert_eq!(c.regs.r[2], 0xFFFF_FFFF);
    assert!(c.regs.sr.t(), "borrow must set T");
}

#[test]
fn subv_sets_t_on_signed_underflow() {
    let mut bus = MemBus::new(64 * 1024);
    // SUBV R1,R2 -> 0x321B
    let mut c = cpu(&mut bus, &[0x321B]);
    c.regs.r[1] = 1;
    c.regs.r[2] = i32::MIN as u32;
    c.step(&mut bus);
    assert!(c.regs.sr.t());
    assert_eq!(c.regs.r[2], i32::MAX as u32);
}

#[test]
fn neg_negc() {
    let mut bus = MemBus::new(64 * 1024);
    // NEG R1,R2 -> 0x621B ; CLRT ; NEGC R1,R2 -> 0x621A
    let mut c = cpu(&mut bus, &[0x621B, 0x0008, 0x621A]);
    c.regs.r[1] = 5;
    c.step(&mut bus); // NEG
    assert_eq!(c.regs.r[2], 0u32.wrapping_sub(5));
    c.step(&mut bus); // CLRT
    c.step(&mut bus); // NEGC R1,R2 -> 0 - 5 - 0
    assert_eq!(c.regs.r[2], 0u32.wrapping_sub(5));
    assert!(c.regs.sr.t(), "0 - nonzero borrows");
}

#[test]
fn dt_decrements_and_sets_t_at_zero() {
    let mut bus = MemBus::new(64 * 1024);
    // DT R1 -> 0x4110
    let mut c = cpu(&mut bus, &[0x4110, 0x4110]);
    c.regs.r[1] = 2;
    c.step(&mut bus); // R1=1, T=0
    assert_eq!(c.regs.r[1], 1);
    assert!(!c.regs.sr.t());
    c.step(&mut bus); // R1=0, T=1
    assert_eq!(c.regs.r[1], 0);
    assert!(c.regs.sr.t());
}

#[test]
fn cmp_pl_pz_str() {
    let mut bus = MemBus::new(64 * 1024);
    // CMP/PL R1 -> 0x4115 ; CMP/PZ R1 -> 0x4111 ; CMP/STR R1,R2 -> 0x221C
    let mut c = cpu(&mut bus, &[0x4115, 0x4111, 0x221C]);

    c.regs.r[1] = 0;
    c.step(&mut bus); // PL: 0 > 0? no
    assert!(!c.regs.sr.t());
    c.step(&mut bus); // PZ: 0 >= 0? yes
    assert!(c.regs.sr.t());

    // CMP/STR: any equal byte
    c.regs.r[1] = 0x11_22_33_44;
    c.regs.r[2] = 0x99_22_AA_BB; // byte 1 (0x22) matches
    c.step(&mut bus);
    assert!(c.regs.sr.t());
}

#[test]
fn exts_extu() {
    let mut bus = MemBus::new(64 * 1024);
    // EXTS.B R1,R2 -> 0x621E ; EXTU.B R1,R3 -> 0x631C
    // EXTS.W R1,R4 -> 0x641F ; EXTU.W R1,R5 -> 0x651D
    let mut c = cpu(&mut bus, &[0x621E, 0x631C, 0x641F, 0x651D]);
    c.regs.r[1] = 0xAABB_CCFF;
    c.step(&mut bus); // EXTS.B
    assert_eq!(c.regs.r[2], 0xFFFF_FFFF);
    c.step(&mut bus); // EXTU.B
    assert_eq!(c.regs.r[3], 0x0000_00FF);
    c.step(&mut bus); // EXTS.W
    assert_eq!(c.regs.r[4], 0xFFFF_CCFF);
    c.step(&mut bus); // EXTU.W
    assert_eq!(c.regs.r[5], 0x0000_CCFF);
}

#[test]
fn mul_l_low_32_bits() {
    let mut bus = MemBus::new(64 * 1024);
    // MUL.L R1,R2 -> 0000nnnnmmmm0111 -> 0x0217
    let mut c = cpu(&mut bus, &[0x0217]);
    c.regs.r[1] = 7;
    c.regs.r[2] = 9;
    c.step(&mut bus);
    assert_eq!(c.regs.macl, 63);
}

#[test]
fn muls_w_sign_extends() {
    let mut bus = MemBus::new(64 * 1024);
    // MULS.W R1,R2 -> 0010nnnnmmmm1111 -> 0x221F
    let mut c = cpu(&mut bus, &[0x221F]);
    c.regs.r[1] = 0x0000_FFFF; // -1 as i16
    c.regs.r[2] = 0x0000_0003;
    c.step(&mut bus);
    assert_eq!(c.regs.macl as i32, -3);
}

#[test]
fn mulu_w_zero_extends() {
    let mut bus = MemBus::new(64 * 1024);
    // MULU.W R1,R2 -> 0x221E
    let mut c = cpu(&mut bus, &[0x221E]);
    c.regs.r[1] = 0x0000_FFFF;
    c.regs.r[2] = 0x0000_0003;
    c.step(&mut bus);
    assert_eq!(c.regs.macl, 0x2_FFFD);
}

#[test]
fn dmuls_l_64_bit_signed() {
    let mut bus = MemBus::new(64 * 1024);
    // DMULS.L R1,R2 -> 0011nnnnmmmm1101 -> 0x321D
    let mut c = cpu(&mut bus, &[0x321D]);
    c.regs.r[1] = 0xFFFF_FFFF; // -1
    c.regs.r[2] = 2;
    c.step(&mut bus);
    // -1 * 2 = -2 as i64 = 0xFFFFFFFF_FFFFFFFE
    assert_eq!(c.regs.mach, 0xFFFF_FFFF);
    assert_eq!(c.regs.macl, 0xFFFF_FFFE);
}

#[test]
fn dmulu_l_64_bit_unsigned() {
    let mut bus = MemBus::new(64 * 1024);
    // DMULU.L R1,R2 -> 0011nnnnmmmm0101 -> 0x3215
    let mut c = cpu(&mut bus, &[0x3215]);
    c.regs.r[1] = 0xFFFF_FFFF;
    c.regs.r[2] = 2;
    c.step(&mut bus);
    // 0xFFFF_FFFF * 2 = 0x1_FFFF_FFFE
    assert_eq!(c.regs.mach, 1);
    assert_eq!(c.regs.macl, 0xFFFF_FFFE);
}

#[test]
fn mac_l_accumulates_signed_64_bit() {
    let mut bus = MemBus::new(64 * 1024);
    // CLRMAC ; MAC.L @R1+, @R2+
    // -> 0x0028 ; 0000nnnnmmmm1111 with n=2,m=1 = 0x021F
    let mut c = cpu(&mut bus, &[0x0028, 0x021F]);
    bus.write_u32(0x3000, 3);
    bus.write_u32(0x4000, 7);
    c.regs.r[1] = 0x3000;
    c.regs.r[2] = 0x4000;
    c.step(&mut bus); // CLRMAC
    c.step(&mut bus); // MAC.L
    assert_eq!(c.regs.macl, 21);
    assert_eq!(c.regs.mach, 0);
    assert_eq!(c.regs.r[1], 0x3004);
    assert_eq!(c.regs.r[2], 0x4004);
}

#[test]
fn mac_w_accumulates_unsaturated() {
    let mut bus = MemBus::new(64 * 1024);
    // CLRMAC ; MAC.W @R1+, @R2+
    // MAC.W -> 0100nnnnmmmm1111 with n=2,m=1 = 0x421F
    let mut c = cpu(&mut bus, &[0x0028, 0x421F]);
    bus.write_u16(0x3000, 0xFFFE); // -2 signed
    bus.write_u16(0x4000, 0x0003);
    c.regs.r[1] = 0x3000;
    c.regs.r[2] = 0x4000;
    c.step(&mut bus);
    c.step(&mut bus);
    // -2 * 3 = -6 sign-extended into 64-bit MACH:MACL
    assert_eq!(c.regs.macl, 0xFFFF_FFFA);
    assert_eq!(c.regs.mach, 0xFFFF_FFFF);
    assert_eq!(c.regs.r[1], 0x3002);
    assert_eq!(c.regs.r[2], 0x4002);
}

#[test]
fn div0s_sets_m_q_t_from_sign_bits() {
    let mut bus = MemBus::new(64 * 1024);
    // DIV0S R1,R2 -> 0010nnnnmmmm0111 -> 0x2217
    let mut c = cpu(&mut bus, &[0x2217]);
    c.regs.r[1] = 0x8000_0000; // negative divisor
    c.regs.r[2] = 0x0000_0001; // positive dividend
    c.step(&mut bus);
    assert!(c.regs.sr.m());
    assert!(!c.regs.sr.q());
    assert!(c.regs.sr.t(), "signs differ -> T=1");
}

#[test]
fn div0u_clears_m_q_t() {
    let mut bus = MemBus::new(64 * 1024);
    // SETT ; DIV0U
    let mut c = cpu(&mut bus, &[0x0018, 0x0019]);
    c.regs.sr.set_m(true);
    c.regs.sr.set_q(true);
    c.step(&mut bus);
    c.step(&mut bus);
    assert!(!c.regs.sr.m());
    assert!(!c.regs.sr.q());
    assert!(!c.regs.sr.t());
}

#[test]
fn div1_single_step_matches_manual_trace() {
    // Hand-traced against the SH-2 software manual non-restoring divide
    // step, starting from DIV0U state (M=Q=T=0):
    //   R1 (divisor) = 2, R2 (working) = 8.
    //   new_q = bit 31 of R2 = 0; shifted = 16.
    //   Branch `!old_q && !M`: r = 16-2 = 14; tmp1 = (14 > 16) = false;
    //   q = new_q ? !tmp1 : tmp1 = false. T = (q == M) = true.
    let mut bus = MemBus::new(64 * 1024);
    // DIV0U ; DIV1 R1,R2 -> 0x0019, 0x3214
    let mut c = cpu(&mut bus, &[0x0019, 0x3214]);
    c.regs.r[1] = 2;
    c.regs.r[2] = 8;
    c.step(&mut bus); // DIV0U
    c.step(&mut bus); // DIV1
    assert_eq!(c.regs.r[2], 14);
    assert!(!c.regs.sr.q());
    assert!(c.regs.sr.t());
}
