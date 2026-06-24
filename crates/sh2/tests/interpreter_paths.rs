//! Coverage for less-exercised `interpreter.rs` paths: the MAC saturation
//! (S=1) arms, RTE delay-slot semantics, SLEEP cycle cost, `Cpu::reset`
//! reset-vector load, memory-routing classify() branches (cached /
//! cache-through / associative-purge / on-chip / CCR), and the BSC
//! master/slave bit reached through the CPU memory path. Every assertion
//! is a concrete value derived from the SH-2 software / SH7604 hardware
//! manual semantics the code cites.

use sh2::{Cpu, Lookup};
use sh2::harness::MemBus;
use sh2::regs::Sr;

const PC0: u32 = 0x0000_1000;
const SP0: u32 = 0x0000_8000;

fn make(prog: &[u16]) -> (Cpu, MemBus) {
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, prog);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[15] = SP0;
    (cpu, bus)
}

// ---------------------------------------------------------------------------
// MAC saturation (S-bit) arms — only the unsaturated forms are tested
// elsewhere (opcodes_arith.rs).
// ---------------------------------------------------------------------------

#[test]
fn mac_l_saturates_to_48_bit_signed_range_when_s_set() {
    // CLRMAC ; MAC.L @R1+,@R2+ with S=1. A product near the 48-bit positive
    // edge plus a large pre-seeded MACH:MACL must clamp to (1<<47)-1.
    let (mut cpu, mut bus) = make(&[0x0028, 0x021F]); // CLRMAC ; MAC.L @R1+,@R2+ (n=2,m=1)
    bus.write_u32(0x3000, 0x7FFF_FFFF);
    bus.write_u32(0x4000, 0x7FFF_FFFF);
    cpu.regs.r[1] = 0x3000;
    cpu.regs.r[2] = 0x4000;
    cpu.regs.sr.set_s(true);
    cpu.step(&mut bus); // CLRMAC
    // Pre-seed the accumulator just under the cap so the add overflows it.
    cpu.regs.mach = 0x0000_7FFF;
    cpu.regs.macl = 0xFFFF_FFFF;
    cpu.step(&mut bus); // MAC.L
    // 0x7FFFFFFF * 0x7FFFFFFF = 0x3FFFFFFF00000001; added to 0x7FFF_FFFFFFFF it
    // far exceeds (1<<47)-1, so the result clamps to the positive max.
    let result = ((cpu.regs.mach as u64) << 32) | cpu.regs.macl as u64;
    assert_eq!(result, (1u64 << 47) - 1, "clamped to +max 48-bit signed");
    assert_eq!(cpu.regs.r[1], 0x3004, "Rm post-incremented");
    assert_eq!(cpu.regs.r[2], 0x4004, "Rn post-incremented");
}

#[test]
fn mac_l_clamps_to_negative_floor_when_s_set() {
    let (mut cpu, mut bus) = make(&[0x021F]); // MAC.L @R1+,@R2+ (n=2,m=1)
    bus.write_u32(0x3000, 0x7FFF_FFFF); // large positive
    bus.write_u32(0x4000, 0x8000_0000); // large negative → negative product
    cpu.regs.r[1] = 0x3000;
    cpu.regs.r[2] = 0x4000;
    cpu.regs.sr.set_s(true);
    // Seed the accumulator already deep in the negative range.
    cpu.regs.mach = 0xFFFF_8000; // sign-extended -(1<<47) region
    cpu.regs.macl = 0x0000_0000;
    cpu.step(&mut bus);
    let result = ((cpu.regs.mach as u64) << 32) | cpu.regs.macl as u64;
    assert_eq!(result as i64, -(1i64 << 47), "clamped to -floor 48-bit signed");
}

#[test]
fn mac_w_saturates_macl_and_sets_overflow_flag_when_s_set() {
    // With S=1 MAC.W accumulates into MACL only, with 32-bit saturation, and
    // records the overflow in MACH bit 0 (SH7604 manual).
    let (mut cpu, mut bus) = make(&[0x421F]); // MAC.W @R1+,@R2+ (n=2,m=1)
    bus.write_u16(0x3000, 0x7FFF); // +32767
    bus.write_u16(0x4000, 0x7FFF); // +32767 → +1073676289 product
    cpu.regs.r[1] = 0x3000;
    cpu.regs.r[2] = 0x4000;
    cpu.regs.sr.set_s(true);
    cpu.regs.macl = 0x7FFF_FFFF; // already at i32::MAX → adding overflows
    cpu.regs.mach = 0;
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.macl, i32::MAX as u32, "saturated to i32::MAX");
    assert_eq!(cpu.regs.mach & 1, 1, "overflow recorded in MACH bit 0");
}

#[test]
fn mac_w_saturates_to_i32_min_for_negative_overflow_when_s_set() {
    let (mut cpu, mut bus) = make(&[0x421F]);
    bus.write_u16(0x3000, 0x8000); // -32768
    bus.write_u16(0x4000, 0x7FFF); // +32767 → negative product
    cpu.regs.r[1] = 0x3000;
    cpu.regs.r[2] = 0x4000;
    cpu.regs.sr.set_s(true);
    cpu.regs.macl = i32::MIN as u32; // already at the floor
    cpu.regs.mach = 0;
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.macl, i32::MIN as u32, "saturated to i32::MIN");
    assert_eq!(cpu.regs.mach & 1, 1, "overflow recorded");
}

// ---------------------------------------------------------------------------
// RTE — delay-slot semantics: the slot executes before the popped PC takes
// effect, and SR is restored masked to the writable bits.
// ---------------------------------------------------------------------------

#[test]
fn rte_executes_delay_slot_then_resumes_popped_pc_and_sr() {
    // Stack (from SP0): [SP0] = resume PC, [SP0+4] = saved SR.
    // RTE ; SETT (delay slot) ; ... ; resume target = MOV #9,R3.
    let (mut cpu, mut bus) = make(&[
        0x002B, // RTE
        0x0018, // SETT — the delay slot (must run before the resume PC)
    ]);
    let resume = 0x0000_2000u32;
    bus.write_u32(SP0, resume); // popped PC
    bus.write_u32(SP0 + 4, 0x0000_0002); // saved SR with S=1, T=0
    bus.write_u16(resume, 0xE309); // MOV #9, R3 at the resume target

    cpu.step(&mut bus); // RTE: pops PC+SR, sets pending_branch
    assert_eq!(cpu.regs.r[15], SP0 + 8, "two longwords popped");
    assert!(cpu.regs.sr.s(), "SR.S restored from the stack");

    cpu.step(&mut bus); // SETT delay slot runs, THEN PC := resume
    assert!(cpu.regs.sr.t(), "delay slot SETT took effect");
    assert_eq!(cpu.regs.pc, resume, "resumed at the popped PC after the slot");

    cpu.step(&mut bus); // MOV #9, R3 at the resume target
    assert_eq!(cpu.regs.r[3], 9, "execution continued at the resume PC");
}

#[test]
fn rte_masks_saved_sr_to_writable_bits() {
    // A saved SR with reserved bits set must be masked by Sr::WRITE_MASK.
    let (mut cpu, mut bus) = make(&[0x002B, 0x0009]); // RTE ; NOP slot
    bus.write_u32(SP0, 0x0000_3000);
    bus.write_u32(SP0 + 4, 0xFFFF_FFFF); // all bits — only writable survive
    cpu.step(&mut bus); // RTE
    assert_eq!(cpu.regs.sr.0, Sr::WRITE_MASK, "reserved SR bits dropped");
}

// ---------------------------------------------------------------------------
// SLEEP — modelled as a 3-cycle instruction (interpreter.rs base cost).
// ---------------------------------------------------------------------------

#[test]
fn sleep_costs_three_cycles() {
    let (mut cpu, mut bus) = make(&[0x001B]); // SLEEP
    let c = cpu.step(&mut bus);
    assert_eq!(c, 3, "SLEEP base cost is 3 cycles");
}

// ---------------------------------------------------------------------------
// Cpu::reset — loads PC and SP from the reset vector at 0x0/0x4.
// ---------------------------------------------------------------------------

#[test]
fn reset_loads_pc_and_sp_from_the_reset_vector() {
    // Run a BRA first so a branch is pending, then prove reset clears it.
    let (mut cpu, mut bus) = make(&[0xA00F]); // BRA +0x1E
    bus.write_u32(0x0000_0000, 0x0000_4321); // initial PC
    bus.write_u32(0x0000_0004, 0x0000_BEEF); // initial SP
    cpu.regs.r[3] = 0x1234;
    cpu.step(&mut bus); // BRA — pending_branch now Some
    assert!(cpu.next_is_delay_slot(), "branch pending before reset");

    cpu.reset(&mut bus);
    assert_eq!(cpu.regs.pc, 0x0000_4321, "PC from vector[0]");
    assert_eq!(cpu.regs.r[15], 0x0000_BEEF, "SP from vector[1]");
    assert_eq!(cpu.regs.sr.imask(), 0xF, "reset masks all interrupts");
    assert!(!cpu.next_is_delay_slot(), "pending branch cleared by reset");
}

// ---------------------------------------------------------------------------
// Memory routing — the classify()/is_assoc_purge()/on-chip/CCR branches in
// mem_read*/mem_write* reached through the public step interface.
// ---------------------------------------------------------------------------

#[test]
fn associative_purge_read_returns_open_bus_and_never_touches_the_bus() {
    // MOV.L @R1,R2 with R1 in region 2 (0x4xxx_xxxx). The read must NOT reach
    // the bus and must return open bus (!0); the matching line is purged.
    let (mut cpu, mut bus) = make(&[0x6212]); // MOV.L @R1,R2
    cpu.regs.r[1] = 0x4000_4000; // associative-purge alias of 0x4000
    bus.write_u32(0x4000, 0x1234_5678); // would be the value if it reached the bus
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[2], 0xFFFF_FFFF, "assoc-purge read is open bus");
}

#[test]
fn assoc_purge_byte_read_in_region_5_is_open_bus() {
    // MOV.B @R1,R2 with R1 in region 5 (0xAxxx_xxxx) — the byte-width path.
    let (mut cpu, mut bus) = make(&[0x6210]); // MOV.B @R1,R2
    cpu.regs.r[1] = 0xA000_4000;
    bus.write_u32(0x4000, 0xAABB_CCDD);
    cpu.step(&mut bus);
    // !0 byte sign-extended → 0xFFFF_FFFF.
    assert_eq!(cpu.regs.r[2], 0xFFFF_FFFF, "byte assoc-purge read is open bus");
}

#[test]
fn cache_through_alias_writes_reach_physical_memory() {
    // MOV.L R2,@R1 through the 0x2xxx_xxxx alias writes to the masked
    // physical address (low 29 bits), so the bus sees 0x4000.
    let (mut cpu, mut bus) = make(&[0x2122]); // MOV.L R2,@R1
    cpu.regs.r[1] = 0x2000_4000; // cache-through alias of 0x4000
    cpu.regs.r[2] = 0xCAFE_BABE;
    cpu.step(&mut bus);
    assert_eq!(
        &bus.as_slice()[0x4000..0x4004],
        &[0xCA, 0xFE, 0xBA, 0xBE],
        "cache-through write landed at the masked physical address"
    );
}

#[test]
fn ccr_is_reachable_through_the_cpu_memory_path() {
    // MOV.B R2,@R1 to CCR (0xFFFFFE92) enables the cache; a MOV.B @R1,R3
    // reads it back. CCR is special-cased ahead of OnChip::owns.
    let (mut cpu, mut bus) = make(&[0x2120, 0x6310]); // MOV.B R2,@R1 ; MOV.B @R1,R3
    cpu.regs.r[1] = 0xFFFF_FE92; // CCR
    cpu.regs.r[2] = 0x01; // CE — cache enable
    cpu.step(&mut bus); // write CCR
    assert_eq!(cpu.cache.ccr() & 0x01, 0x01, "CE bit set in the cache");
    cpu.step(&mut bus); // read CCR back
    assert_eq!(cpu.regs.r[3] & 0x01, 0x01, "CCR read back through mem path");
}

#[test]
fn ccr_word_access_sets_cp_and_preserves_ce() {
    // Sangokushi V's menu transition uses this word sequence to purge SH-2
    // cache lines before publishing VDP1 display-list work:
    //   MOV.W @CCR,R0 ; OR #0x10,R0 ; MOV.W R0,@CCR
    let (mut cpu, mut bus) = make(&[0x6011, 0xCB10, 0x2101]);
    cpu.regs.r[1] = 0xFFFF_FE92; // CCR
    cpu.cache.set_ccr(0x01); // CE

    let cached_addr = 0x0000_4000;
    cpu.cache.install(cached_addr, [0xA5; 16]);
    assert!(matches!(cpu.cache.lookup_data(cached_addr), Lookup::Hit(_)));
    let purges_before = cpu.cache.dbg_purges();

    cpu.step(&mut bus); // MOV.W @CCR,R0
    assert_eq!(cpu.regs.r[0], 0x0101, "word read mirrors CCR into both bytes (SH7604 / Mednafen CCR|CCR<<8)");
    cpu.step(&mut bus); // OR #0x10,R0
    assert_eq!(cpu.regs.r[0] & 0xFF, 0x11, "CP bit requested");
    cpu.step(&mut bus); // MOV.W R0,@CCR

    assert_eq!(cpu.cache.ccr(), 0x01, "CP is write-only; CE remains enabled");
    assert_eq!(cpu.cache.dbg_purges(), purges_before + 1, "word write triggered CP purge");
    assert_eq!(cpu.cache.lookup_data(cached_addr), Lookup::Miss, "resident line was purged");
}

#[test]
fn onchip_register_reached_through_mov_l_routes_to_onchip_not_bus() {
    // MOV.L R2,@R1 to a DMAC register (0xFFFFFF8C, CHCR0) routes to OnChip;
    // a read-back via MOV.L @R1,R3 returns it. Confirms mem_read32/write32
    // take the OnChip::owns branch.
    let (mut cpu, mut bus) = make(&[0x2122, 0x6312]); // MOV.L R2,@R1 ; MOV.L @R1,R3
    cpu.regs.r[1] = 0xFFFF_FF8C; // DMAC CHCR0
    cpu.regs.r[2] = 0x0000_ABCD;
    cpu.step(&mut bus); // write
    assert_eq!(cpu.onchip.dmac.channels[0].chcr, 0x0000_ABCD, "routed to OnChip");
    cpu.step(&mut bus); // read back
    assert_eq!(cpu.regs.r[3], 0x0000_ABCD, "read back via on-chip routing");
}

// ---------------------------------------------------------------------------
// BSC master/slave bit reached through the CPU memory path (set_bsc_slave).
// BCR1 is at 0xFFFFFFE0; the master/slave bit (BCR1 bit 15) lands in the
// byte at 0xFFFFFFE2 — bit 7 of that byte.
// ---------------------------------------------------------------------------

#[test]
fn bcr1_master_slave_bit_visible_via_byte_read() {
    let (mut cpu, mut bus) = make(&[0x6210]); // MOV.B @R1,R2
    cpu.regs.r[1] = 0xFFFF_FFE2; // byte holding BCR1 bit 15
    // Master (default): bit 7 clear.
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[2] & 0x80, 0x00, "master reads BCR1 bit15 = 0");

    let (mut cpu, mut bus) = make(&[0x6210]);
    cpu.regs.r[1] = 0xFFFF_FFE2;
    cpu.set_bsc_slave(true);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[2] & 0x80, 0x80, "slave reads BCR1 bit15 = 1");
}

#[test]
fn bcr1_master_slave_bit_is_read_only() {
    // Software writing 0xFF to the BCR1 high byte cannot clear the slave bit.
    let (mut cpu, mut bus) = make(&[0x2120, 0x6310]); // MOV.B R2,@R1 ; MOV.B @R1,R3
    cpu.regs.r[1] = 0xFFFF_FFE2;
    cpu.regs.r[2] = 0x00; // attempt to write the master/slave byte to 0
    cpu.set_bsc_slave(true);
    cpu.step(&mut bus); // write 0
    cpu.step(&mut bus); // read back
    assert_eq!(
        cpu.regs.r[3] & 0x80,
        0x80,
        "master/slave bit stays 1 despite a write of 0"
    );
}
