//! Repro: the BIOS sound-driver note-ring enqueue (0x4B9A) computes a byte
//! offset via `add.w d7,d7; add.w d7,d7; adda.l d7,a2` (index*4 added to the
//! ring base). The BGM-silence root was traced to ours collapsing a 9-entry
//! ring into 2 entries — i.e. this offset coming out as (index*4) mod 8.
//! Mednafen (oracle) produces index*4 = 0,4,8,...,32. This pins whether the
//! core mis-computes that exact sequence.

use m68k::Cpu;
use m68k::harness::MemBus;

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
fn ring_offset_index_times_four() {
    // DE47 = add.w d7,d7 ; DE47 = add.w d7,d7 ; D5C7 = adda.l d7,a2
    for index in 0u32..9 {
        let (mut cpu, mut bus) = boot(&[0xDE47, 0xDE47, 0xD5C7], |c| {
            c.regs.d[7] = index; // clean upper bits (driver does clr.l d7 first)
            c.regs.a[2] = 0x7A00;
        });
        cpu.step(&mut bus); // add.w d7,d7  → 2*index
        cpu.step(&mut bus); // add.w d7,d7  → 4*index
        cpu.step(&mut bus); // adda.l d7,a2 → a2 += 4*index
        assert_eq!(
            cpu.regs.d[7] & 0xFFFF,
            (index * 4) & 0xFFFF,
            "index {index}: d7 word should be index*4"
        );
        assert_eq!(
            cpu.regs.a[2],
            0x7A00 + index * 4,
            "index {index}: ring slot should be base + index*4"
        );
    }
}
