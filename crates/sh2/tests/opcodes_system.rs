//! Coverage for LDC/STC, LDS/STS (both register and `.L` indirect forms),
//! BRAF/BSRF, CLRMAC, and a TRAPA/RTE round trip (task #4).

use sh2::harness::MemBus;
use sh2::{Bus, Cpu};

const PC0: u32 = 0x0000_1000;

fn cpu(bus: &mut MemBus, program: &[u16]) -> Cpu {
    bus.load_program(PC0, program);
    let mut c = Cpu::new();
    c.regs.pc = PC0;
    c.regs.r[15] = 0x0000_8000;
    c
}

#[test]
fn ldc_stc_gbr_round_trip() {
    let mut bus = MemBus::new(64 * 1024);
    // LDC R1,GBR -> 0x411E ; STC GBR,R2 -> 0x0212
    let mut c = cpu(&mut bus, &[0x411E, 0x0212]);
    c.regs.r[1] = 0xDEAD_BEEF;
    c.step(&mut bus);
    assert_eq!(c.regs.gbr, 0xDEAD_BEEF);
    c.step(&mut bus);
    assert_eq!(c.regs.r[2], 0xDEAD_BEEF);
}

#[test]
fn ldc_sr_masks_reserved_bits() {
    let mut bus = MemBus::new(64 * 1024);
    // LDC R1,SR -> 0x410E
    let mut c = cpu(&mut bus, &[0x410E]);
    c.regs.r[1] = 0xFFFF_FFFF;
    c.step(&mut bus);
    // Only T|S|I_MASK|Q|M may be set.
    assert_eq!(c.regs.sr.0 & !0x3F3, 0, "reserved bits must be masked off");
    assert!(c.regs.sr.t());
}

#[test]
fn lds_sts_pr_round_trip() {
    let mut bus = MemBus::new(64 * 1024);
    // LDS R1,PR -> 0x412A ; STS PR,R2 -> 0x022A
    let mut c = cpu(&mut bus, &[0x412A, 0x022A]);
    c.regs.r[1] = 0x1234_5678;
    c.step(&mut bus);
    assert_eq!(c.regs.pr, 0x1234_5678);
    c.step(&mut bus);
    assert_eq!(c.regs.r[2], 0x1234_5678);
}

#[test]
fn stsl_predec_storage_of_macl() {
    let mut bus = MemBus::new(64 * 1024);
    // STS.L MACL,@-R1 -> 0x4112
    let mut c = cpu(&mut bus, &[0x4112]);
    c.regs.macl = 0xAABB_CCDD;
    c.regs.r[1] = 0x4004;
    c.step(&mut bus);
    assert_eq!(c.regs.r[1], 0x4000);
    assert_eq!(&bus.as_slice()[0x4000..0x4004], &[0xAA, 0xBB, 0xCC, 0xDD]);
}

#[test]
fn ldsl_postinc_load_of_mach() {
    let mut bus = MemBus::new(64 * 1024);
    // LDS.L @R1+,MACH -> 0x4106
    let mut c = cpu(&mut bus, &[0x4106]);
    bus.write_u32(0x4000, 0x1122_3344);
    c.regs.r[1] = 0x4000;
    c.step(&mut bus);
    assert_eq!(c.regs.mach, 0x1122_3344);
    assert_eq!(c.regs.r[1], 0x4004);
}

#[test]
fn braf_jumps_to_pc_plus_4_plus_rm() {
    let mut bus = MemBus::new(64 * 1024);
    // PC0+0  BRAF R1  (0x0123)
    // PC0+2  NOP      (delay slot)
    // PC0+4  NOP      (skipped by BRAF + 2)
    // PC0+6  MOV #9,R5 (landing)
    let mut c = cpu(&mut bus, &[0x0123, 0x0009, 0x0009, 0xE509]);
    c.regs.r[1] = 2; // target = PC0 + 4 + 2 = PC0 + 6
    c.step(&mut bus); // BRAF
    c.step(&mut bus); // NOP slot, then branch
    assert_eq!(c.regs.pc, PC0 + 6);
    c.step(&mut bus); // MOV #9,R5
    assert_eq!(c.regs.r[5], 9);
}

#[test]
fn bsrf_sets_pr_and_branches() {
    let mut bus = MemBus::new(64 * 1024);
    // BSRF R1 -> 0000mmmm00000011 -> 0x0103 ; NOP slot
    let mut c = cpu(&mut bus, &[0x0103, 0x0009]);
    c.regs.r[1] = 0x40;
    c.step(&mut bus);
    assert_eq!(c.regs.pr, PC0 + 4);
    c.step(&mut bus);
    assert_eq!(c.regs.pc, PC0 + 4 + 0x40);
}

#[test]
fn clrmac_clears_both_halves() {
    let mut bus = MemBus::new(64 * 1024);
    // CLRMAC -> 0x0028
    let mut c = cpu(&mut bus, &[0x0028]);
    c.regs.mach = 0x1111_1111;
    c.regs.macl = 0x2222_2222;
    c.step(&mut bus);
    assert_eq!(c.regs.mach, 0);
    assert_eq!(c.regs.macl, 0);
}

#[test]
fn trapa_then_rte_round_trip() {
    // TRAPA #5 pushes SR+PC, vectors via VBR+20. The handler is a single
    // RTE+NOP that pops them and resumes after TRAPA.
    let mut bus = MemBus::new(64 * 1024);
    // PC0+0  TRAPA #5  (0xC305)
    // PC0+2  MOV #7,R5  (resume target)
    // 0x6000 RTE        (handler)
    // 0x6002 NOP        (RTE slot)
    let mut c = cpu(&mut bus, &[0xC305, 0xE507]);
    bus.write_u16(0x6000, 0x002B); // RTE
    bus.write_u16(0x6002, 0x0009); // NOP slot

    // VBR + 5*4 = 0x5000 + 20 = 0x5014 ; install handler address there.
    c.regs.vbr = 0x5000;
    bus.write_u32(0x5014, 0x6000);

    c.regs.r[15] = 0x7800; // give the stack a known address
    let saved_sp = c.regs.r[15];
    c.regs.sr.set_t(true);

    c.step(&mut bus); // TRAPA
    assert_eq!(c.regs.pc, 0x6000, "vector dispatched");
    assert_eq!(c.regs.r[15], saved_sp - 8, "two 4-byte pushes");
    // Frame: lower word = saved PC, upper word = saved SR.
    let (saved_pc_on_stack, _) = bus.read32(c.regs.r[15], sh2::AccessKind::Data);
    let (saved_sr_on_stack, _) = bus.read32(c.regs.r[15] + 4, sh2::AccessKind::Data);
    assert_eq!(saved_pc_on_stack, PC0 + 2);
    assert_eq!(saved_sr_on_stack & 1, 1, "SR.T saved");

    c.step(&mut bus); // RTE
    c.step(&mut bus); // NOP slot, then jump back
    assert_eq!(c.regs.pc, PC0 + 2);
    assert_eq!(c.regs.r[15], saved_sp);

    c.step(&mut bus); // MOV #7,R5
    assert_eq!(c.regs.r[5], 7);
}
