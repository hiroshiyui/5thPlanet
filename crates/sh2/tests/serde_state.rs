//! Save-state completeness for SH-2 state that decides future behaviour
//! (ADR-0027 determinism): a value that changes what the next instruction or
//! register write does must survive a serialize → deserialize round trip.
//! Only built with the `serde` feature (`cargo test -p sh2 --features serde`;
//! `cargo test --workspace` enables it through `saturn`).
#![cfg(feature = "serde")]

use sh2::harness::MemBus;
use sh2::{Cpu, OnChip};

fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(v: &T) -> T {
    let cfg = bincode::config::standard();
    let bytes = bincode::serde::encode_to_vec(v, cfg).expect("encode");
    bincode::serde::decode_from_slice(&bytes, cfg)
        .expect("decode")
        .0
}

const FTCSR: u32 = 0xFFFF_FE11;

#[test]
fn ftcsr_read_observed_latch_survives_a_round_trip() {
    // OVF set by hardware, then observed by a read: the next write-0 must clear
    // it — in the original AND in a state loaded between the read and write.
    let mut o = OnChip::new();
    o.frt.frc = 0xFFFF;
    o.frt.ocra = 0xFFFF;
    o.frt.ocrb = 0xFFFF;
    o.advance_timers(8); // φ/8: one tick → wrap → OVF
    assert_eq!(o.read8(FTCSR) & 0x02, 0x02, "OVF observed");
    let mut loaded = round_trip(&o);
    o.write8(FTCSR, 0x00);
    loaded.write8(FTCSR, 0x00);
    assert_eq!(o.frt.ftcsr & 0x02, 0, "original clears");
    assert_eq!(loaded.frt.ftcsr & 0x02, 0, "loaded copy clears identically");
}

#[test]
fn dmac_te_shadow_and_divu_shadows_survive_a_round_trip() {
    let mut o = OnChip::new();
    o.dmac.latch_te(0); // TE set, not yet read → protected
    o.write32(0xFFFF_FF00, 4);
    o.write32(0xFFFF_FF1C, 0x5555); // DVDNTUL shadow
    let mut loaded = round_trip(&o);
    loaded.write32(0xFFFF_FF8C, 0); // write-0 without a read: TE must survive
    assert_eq!(loaded.dmac.channels[0].chcr & 2, 2);
    assert_eq!(loaded.read32(0xFFFF_FF1C), 0x5555);
}

#[test]
fn sleep_and_interrupt_block_latches_survive_a_round_trip() {
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(0x1000, &[0x400E, 0x001B]); // LDC R0,SR ; SLEEP
    let mut cpu = Cpu::new();
    cpu.regs.pc = 0x1000;
    cpu.regs.r[15] = 0x8000;
    cpu.step(&mut bus); // LDC → next boundary interrupt-blocked
    let loaded = round_trip(&cpu);
    assert!(loaded.interrupt_blocked());
    cpu.step(&mut bus); // SLEEP → powered down
    let loaded = round_trip(&cpu);
    assert!(loaded.is_sleeping());
}
