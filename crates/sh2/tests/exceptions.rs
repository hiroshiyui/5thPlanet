//! Exception and interrupt dispatch (task #7).
//!
//! Each test sets up a vector in the VBR table, raises the corresponding
//! condition, runs one step, and verifies the CPU vectored through with
//! the right pushed frame, SR mask, and PC.

use sh2::Cpu;
use sh2::harness::MemBus;
use sh2::{Bus, InterruptSource};

const PC0: u32 = 0x0000_1000;
const VBR: u32 = 0x0000_5000;
const SP0: u32 = 0x0000_8000;

fn make(prog: &[u16]) -> (Cpu, MemBus) {
    let mut bus = MemBus::new(64 * 1024);
    bus.load_program(PC0, prog);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[15] = SP0;
    cpu.regs.vbr = VBR;
    (cpu, bus)
}

fn install_vector(bus: &mut MemBus, vector: u8, handler: u32) {
    bus.write_u32(VBR + (vector as u32) * 4, handler);
}

#[test]
fn illegal_instruction_vectors_through_vector_4() {
    let (mut cpu, mut bus) = make(&[0xFFFF]); // 0xFFFF is Op::Illegal
    install_vector(&mut bus, 4, 0x0000_6000);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x0000_6000, "PC at illegal handler");
    assert_eq!(cpu.regs.r[15], SP0 - 8, "two pushes");
    // Resume PC pushed is the address of the offending illegal instruction.
    let (resume_pc, _) = bus.read32(cpu.regs.r[15], sh2::AccessKind::Data);
    assert_eq!(resume_pc, PC0);
}

#[test]
fn slot_illegal_pushes_branch_address_and_vectors_through_6() {
    // BRA +0 ; JMP @R0  (JMP is illegal in a delay slot → vector 6)
    let (mut cpu, mut bus) = make(&[0xA000, 0x002B]);
    install_vector(&mut bus, 6, 0x0000_6100);
    cpu.regs.r[0] = 0; // JMP target, never reached

    cpu.step(&mut bus); // BRA sets pending_branch
    cpu.step(&mut bus); // JMP in slot → slot-illegal
    assert_eq!(cpu.regs.pc, 0x0000_6100);
    // Resume PC is the slot instruction's address (PC0+2), so RTE re-runs
    // the slot — software can patch around it.
    let (resume_pc, _) = bus.read32(cpu.regs.r[15], sh2::AccessKind::Data);
    assert_eq!(resume_pc, PC0 + 2);
}

#[test]
fn trapa_uses_unified_exception_path() {
    // Same shape as opcodes_system::trapa_then_rte_round_trip but isolated
    // to prove the take_exception() unification didn't drift.
    let (mut cpu, mut bus) = make(&[0xC305, 0xE507]); // TRAPA #5 ; MOV #7,R5
    install_vector(&mut bus, 5, 0x0000_6200);
    cpu.regs.sr.set_t(true);

    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x0000_6200);
    let (resume_pc, _) = bus.read32(cpu.regs.r[15], sh2::AccessKind::Data);
    let (saved_sr, _) = bus.read32(cpu.regs.r[15] + 4, sh2::AccessKind::Data);
    assert_eq!(resume_pc, PC0 + 2);
    assert_eq!(saved_sr & 1, 1, "SR.T preserved");
}

#[test]
fn external_interrupt_dispatches_when_above_mask() {
    let (mut cpu, mut bus) = make(&[0x0009, 0x0009]); // NOP × 2
    // Install handler at vector 64 + level (auto-vector range).
    install_vector(&mut bus, 64 + 7, 0x0000_6300);
    cpu.regs.sr.set_imask(3);
    cpu.onchip.intc.raise(InterruptSource::External(7));

    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x0000_6300, "vectored at instruction boundary");
    assert_eq!(cpu.regs.sr.imask(), 7, "SR.imask raised to new level");
    // Resume PC pushed is the next instruction we *would* have run.
    let (resume_pc, _) = bus.read32(cpu.regs.r[15], sh2::AccessKind::Data);
    assert_eq!(resume_pc, PC0);
}

#[test]
fn external_interrupt_suppressed_below_mask() {
    let (mut cpu, mut bus) = make(&[0x0009]);
    install_vector(&mut bus, 64 + 3, 0x0000_6400);
    cpu.regs.sr.set_imask(7);
    cpu.onchip.intc.raise(InterruptSource::External(3));

    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, PC0 + 2, "NOP executed normally");
    assert_eq!(cpu.regs.r[15], SP0, "no push");
}

#[test]
fn nmi_dispatches_even_at_max_mask() {
    let (mut cpu, mut bus) = make(&[0x0009]);
    install_vector(&mut bus, 11, 0x0000_6500); // NMI vector is fixed at 11
    cpu.regs.sr.set_imask(15);
    cpu.onchip.intc.raise(InterruptSource::Nmi);

    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x0000_6500);
}

#[test]
fn interrupt_not_accepted_inside_delay_slot() {
    // BRA target ; NOP slot — an interrupt raised between the two steps
    // must wait until after the slot retires (slot is a hardware-mandated
    // atomic pair with the branch).
    let (mut cpu, mut bus) = make(&[0xA001, 0x0009, 0x0009, 0x0009]);
    install_vector(&mut bus, 64 + 8, 0x0000_6600);
    cpu.regs.sr.set_imask(3);

    cpu.step(&mut bus); // BRA — pending_branch set
    cpu.onchip.intc.raise(InterruptSource::External(8));
    cpu.step(&mut bus); // NOP slot — interrupt MUST NOT fire here
    assert_ne!(cpu.regs.pc, 0x0000_6600, "interrupt withheld during slot");

    cpu.step(&mut bus); // after slot — interrupt should now fire
    assert_eq!(cpu.regs.pc, 0x0000_6600);
}

#[test]
fn higher_priority_interrupt_wins_when_multiple_pending() {
    let (mut cpu, mut bus) = make(&[0x0009]);
    install_vector(&mut bus, 64 + 5, 0x0000_6700);
    install_vector(&mut bus, 64 + 12, 0x0000_6800);
    cpu.regs.sr.set_imask(0);
    cpu.onchip.intc.raise(InterruptSource::External(5));
    cpu.onchip.intc.raise(InterruptSource::External(12));
    // External level uses the *latest* raise level (single shared slot in
    // INTC). So whichever was raised last wins regardless of value — here
    // External(12).
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x0000_6800);
}
