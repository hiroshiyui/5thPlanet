//! End-to-end test that the CPU correctly routes memory accesses to the
//! on-chip peripheral block (FFFFFE00..FFFFFFFF) instead of the external
//! bus. Exercises the DIVU through real MOV.L instructions so the whole
//! chain (decoder → interpreter → mem_read*/mem_write* → OnChip → DIVU)
//! is validated together.

use sh2::Cpu;
use sh2::harness::MemBus;
use sh2::{InterruptSource, OnChip};

const PC0: u32 = 0x0000_1000;

#[test]
fn onchip_owns_only_the_top_512_bytes() {
    assert!(!OnChip::owns(0xFFFF_FDFF));
    assert!(OnChip::owns(0xFFFF_FE00));
    assert!(OnChip::owns(0xFFFF_FFFF));
}

#[test]
fn cpu_drives_divu_via_normal_mov_l_instructions() {
    // Program (each line is one 16-bit instruction):
    //
    //   MOV.L  @(0,PC),R1          # R1 = divisor address (0xFFFFFF00)
    //   MOV.L  @(0,PC),R2          # R2 = dividend address (0xFFFFFF04)
    //   MOV.L  @(0,PC),R3          # R3 = divisor value (7)
    //   MOV.L  @(0,PC),R4          # R4 = dividend (50)
    //   MOV.L  R3,@R1              # DVSR ← 7
    //   MOV.L  R4,@R2              # DVDNT ← 50 (triggers divide)
    //   MOV.L  @R2,R5              # R5 ← quotient
    //
    // Each `MOV.L @(0,PC),Rn` reads the 32-bit word at PC+4 (aligned).
    // The literal pool sits in the second half of the program.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(
        PC0,
        &[
            // 0x1000 — instructions
            0xD104, // MOV.L @(4,PC), R1  -> reads at PC0+0x18
            0xD204, // MOV.L @(4,PC), R2  -> reads at PC0+0x1C
            0xD304, // MOV.L @(4,PC), R3  -> reads at PC0+0x20
            0xD404, // MOV.L @(4,PC), R4  -> reads at PC0+0x24
            0x2132, // MOV.L R3, @R1
            0x2242, // MOV.L R4, @R2
            0x6522, // MOV.L @R2, R5
            0x0009, // NOP
        ],
    );
    // Literal pool at PC0+0x18 onward (24 bytes in: 4 ops × 2 = 8 bytes for
    // first chunk, then 8 ops total × 2 = 16 bytes... but PC+4+disp*4 for
    // disp=4 from a given instruction = (instr_pc & ~3) + 4 + 16 =
    // (instr_pc & ~3) + 0x14. With instr_pc=PC0 the address is PC0+0x14.
    // Let me redo: from PC0 (disp=4, MOV.L), base = (PC0 + 4 + 16) & ~3 = PC0+0x14.
    // From PC0+2: base = (PC0+2 + 4 + 16) & ~3 = (PC0 + 0x16) & ~3 = PC0 + 0x14
    //   — same target because of the alignment mask. That means R2 would
    //   overwrite R1. Need different disp values.
    //
    // Simpler: build the literal table manually after writing the program
    // by computing addresses based on the encoded disp.
    bus.write_u32(PC0 + 0x14, 0xFFFF_FF00); // DVSR address
    bus.write_u32(PC0 + 0x18, 0xFFFF_FF04); // DVDNT address — but
    // ...the (0,PC) addressing aligns so disp=4 from PC0 and PC0+2 both
    // map to PC0+0x14. Replace the program with explicit disps:
    bus.load_program(
        PC0,
        &[
            0xD104, // MOV.L @(disp=4, PC), R1
            0xD205, // MOV.L @(disp=5, PC), R2
            0xD306, // MOV.L @(disp=6, PC), R3
            0xD407, // MOV.L @(disp=7, PC), R4
            0x2132, // MOV.L R3, @R1
            0x2242, // MOV.L R4, @R2
            0x6522, // MOV.L @R2, R5
            0x0009, // NOP (padding so literals are 4-byte aligned)
        ],
    );
    // For MOV.L @(disp, PC), Rn at instr_pc:
    //   addr = (instr_pc + 4 + disp*4) & ~3
    // PC0 + 4 + 16 = PC0 + 0x14   (disp=4 from PC0+0)
    // PC0+2 + 4 + 20 = PC0 + 0x1A → mask = PC0 + 0x18 (disp=5 from PC0+2)
    // PC0+4 + 4 + 24 = PC0 + 0x20 (disp=6 from PC0+4)
    // PC0+6 + 4 + 28 = PC0 + 0x26 → mask = PC0 + 0x24 (disp=7 from PC0+6)
    bus.write_u32(PC0 + 0x14, 0xFFFF_FF00);
    bus.write_u32(PC0 + 0x18, 0xFFFF_FF04);
    bus.write_u32(PC0 + 0x20, 7);
    bus.write_u32(PC0 + 0x24, 50);

    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[15] = 0x0000_8000;

    for _ in 0..7 {
        cpu.step(&mut bus);
    }
    assert_eq!(cpu.regs.r[5] as i32, 7, "quotient = 50 / 7 = 7");
    assert_eq!(
        cpu.onchip.divu.dvdnth as i32,
        1,
        "remainder visible in DVDNTH"
    );
}

#[test]
fn external_bus_unaffected_by_onchip_routing() {
    // Verify that a normal MOV.L to a non-on-chip address still hits the
    // external bus and bypasses OnChip.
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, &[0x2122]); // MOV.L R2, @R1
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = 0x4000;
    cpu.regs.r[2] = 0xDEAD_BEEF;
    cpu.step(&mut bus);
    assert_eq!(
        &bus.as_slice()[0x4000..0x4004],
        &[0xDE, 0xAD, 0xBE, 0xEF],
        "external bus saw the write"
    );
    // OnChip untouched — no DIVU register was written.
    assert_eq!(cpu.onchip.divu.dvsr, 0);
}

#[test]
fn intc_raise_and_query_via_public_api() {
    let mut cpu = Cpu::new();
    cpu.onchip.intc.ipra = 0xC000; // DIVU priority = 0xC
    cpu.onchip.intc.raise(InterruptSource::DivuOvf);
    let pending = cpu.onchip.intc.next_pending(8).unwrap();
    assert_eq!(pending.0, InterruptSource::DivuOvf);
    assert_eq!(pending.1, 0xC);
}
