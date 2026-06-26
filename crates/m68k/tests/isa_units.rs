//! Unit coverage for the shared ISA primitives (`Size`, `Cond`) and the
//! register model (`Sr` pack/unpack, A7 banking). These are pure functions the
//! opcode tests exercise only incidentally; here they are checked directly
//! against the M68000 User's Manual encoding/condition tables.

use m68k::isa::{Cond, Size};
use m68k::regs::{Registers, Sr};

#[test]
fn size_from_move_bits_covers_all_encodings() {
    // MOVE size field (bits 13-12): 1=byte, 3=word, 2=long; 0 is invalid.
    assert_eq!(Size::from_move_bits(1), Some(Size::Byte));
    assert_eq!(Size::from_move_bits(3), Some(Size::Word));
    assert_eq!(Size::from_move_bits(2), Some(Size::Long));
    assert_eq!(Size::from_move_bits(0), None);
}

#[test]
fn size_from_op_bits_covers_all_encodings() {
    // Most groups (bits 7-6): 0=byte, 1=word, 2=long; 3 is the special form.
    assert_eq!(Size::from_op_bits(0), Some(Size::Byte));
    assert_eq!(Size::from_op_bits(1), Some(Size::Word));
    assert_eq!(Size::from_op_bits(2), Some(Size::Long));
    assert_eq!(Size::from_op_bits(3), None);
}

#[test]
fn size_byte_width_mask_and_msb() {
    assert_eq!(Size::Byte.bytes(), 1);
    assert_eq!(Size::Word.bytes(), 2);
    assert_eq!(Size::Long.bytes(), 4);
    assert_eq!(Size::Byte.mask(), 0xFF);
    assert_eq!(Size::Word.mask(), 0xFFFF);
    assert_eq!(Size::Long.mask(), 0xFFFF_FFFF);
    assert_eq!(Size::Byte.msb(), 0x80);
    assert_eq!(Size::Word.msb(), 0x8000);
    assert_eq!(Size::Long.msb(), 0x8000_0000);
}

#[test]
fn size_sign_extend_each_width() {
    assert_eq!(Size::Byte.sign_extend(0x80), -128);
    assert_eq!(Size::Byte.sign_extend(0x7F), 127);
    assert_eq!(Size::Word.sign_extend(0x8000), -32768);
    assert_eq!(Size::Word.sign_extend(0x0001), 1);
    assert_eq!(Size::Long.sign_extend(0xFFFF_FFFF), -1);
}

#[test]
fn cond_from_bits_maps_every_code() {
    use Cond::*;
    let expected = [T, F, Hi, Ls, Cc, Cs, Ne, Eq, Vc, Vs, Pl, Mi, Ge, Lt, Gt, Le];
    for (bits, &c) in expected.iter().enumerate() {
        assert_eq!(Cond::from_bits(bits as u16), c, "code {bits:#x}");
    }
    // The mask is bits & 0xF, so 0x1F folds to Le (0xF).
    assert_eq!(Cond::from_bits(0x1F), Cond::Le);
}

#[test]
fn cond_test_unconditional_and_simple_flags() {
    // T/F are flag-independent; the single-flag conditions track their bit.
    assert!(Cond::T.test(false, false, false, false));
    assert!(!Cond::F.test(true, true, true, true));
    assert!(Cond::Cs.test(true, false, false, false));
    assert!(Cond::Cc.test(false, false, false, false));
    assert!(Cond::Vs.test(false, true, false, false));
    assert!(Cond::Vc.test(false, false, false, false));
    assert!(Cond::Eq.test(false, false, true, false));
    assert!(Cond::Ne.test(false, false, false, false));
    assert!(Cond::Mi.test(false, false, false, true));
    assert!(Cond::Pl.test(false, false, false, false));
}

#[test]
fn cond_test_compound_conditions() {
    // Hi = !C & !Z ; Ls = C | Z.
    assert!(Cond::Hi.test(false, false, false, false));
    assert!(!Cond::Hi.test(false, false, true, false));
    assert!(Cond::Ls.test(true, false, false, false));
    assert!(Cond::Ls.test(false, false, true, false));

    // Signed: Ge = N==V, Lt = N!=V, Gt = !Z & (N==V), Le = Z | (N!=V).
    assert!(Cond::Ge.test(false, false, false, false)); // N==V (both 0)
    assert!(Cond::Ge.test(false, true, false, true)); // N==V (both 1)
    assert!(Cond::Lt.test(false, false, false, true)); // N!=V
    assert!(Cond::Gt.test(false, false, false, false)); // !Z & N==V
    assert!(!Cond::Gt.test(false, false, true, false)); // Z set → not Gt
    assert!(Cond::Le.test(false, false, true, false)); // Z set
    assert!(Cond::Le.test(false, false, false, true)); // N!=V
}

#[test]
fn sr_round_trips_through_u16() {
    // Pack every field, unpack, and confirm the value survives the trip.
    let sr = Sr {
        c: true,
        v: false,
        z: true,
        n: false,
        x: true,
        supervisor: true,
        trace: true,
        imask: 5,
    };
    let packed = sr.to_u16();
    let back = Sr::from_u16(packed);
    assert_eq!(back, sr);
    // Bit positions per the manual: X=4, S=13, T=15, mask=8..10.
    assert_eq!(packed & 0x0010, 0x0010, "X at bit 4");
    assert_eq!(packed & 0x2000, 0x2000, "S at bit 13");
    assert_eq!(packed & 0x8000, 0x8000, "T at bit 15");
    assert_eq!((packed >> 8) & 7, 5, "imask at bits 8..10");
}

#[test]
fn sr_from_u16_decodes_individual_bits() {
    let sr = Sr::from_u16(0xA70F);
    assert!(sr.trace, "0x8000");
    assert!(sr.supervisor, "0x2000");
    assert_eq!(sr.imask, 7, "0x0700");
    assert!(sr.n && sr.z && sr.v && sr.c, "low CCR bits");
    assert!(!sr.x, "X (0x10) clear in 0x...0F");
}

#[test]
fn sr_ccr_accessors_isolate_the_low_byte() {
    let mut sr = Sr {
        supervisor: true,
        imask: 4,
        ..Default::default()
    };
    sr.set_ccr(0x1F);
    assert!(sr.c && sr.v && sr.z && sr.n && sr.x);
    assert_eq!(sr.ccr(), 0x1F);
    assert!(sr.supervisor, "set_ccr leaves the system byte intact");
    assert_eq!(sr.imask, 4);
}

#[test]
fn registers_new_is_zeroed() {
    let r = Registers::new();
    assert_eq!(r.d, [0; 8]);
    assert_eq!(r.a, [0; 8]);
    assert_eq!(r.pc, 0);
}

#[test]
fn set_supervisor_banks_a7_both_directions() {
    let mut r = Registers::new();
    r.sr.supervisor = true;
    r.ssp = 0x2000;
    r.usp = 0x4000;
    r.a[7] = 0x2000;

    // Supervisor → user: stash SSP, load USP.
    r.set_supervisor(false);
    assert_eq!(r.a[7], 0x4000, "A7 = USP in user mode");
    assert_eq!(r.ssp, 0x2000, "SSP preserved");

    // Change A7 in user mode, then go back to supervisor.
    r.a[7] = 0x4400;
    r.set_supervisor(true);
    assert_eq!(r.usp, 0x4400, "USP stashed on the way up");
    assert_eq!(r.a[7], 0x2000, "A7 = SSP restored");
}

#[test]
fn set_supervisor_is_a_noop_when_unchanged() {
    let mut r = Registers::new();
    r.sr.supervisor = true;
    r.a[7] = 0x2000;
    r.ssp = 0x9999; // a stale value that must NOT clobber A7
    r.set_supervisor(true);
    assert_eq!(r.a[7], 0x2000, "no bank swap when the S bit is unchanged");
}
