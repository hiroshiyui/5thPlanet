//! Top-level CPU state and instruction dispatch.
//!
//! Task #3 wires up the first batch of opcodes (data transfer, basic ALU,
//! compares, branches with delay slots, system NOP/CLRT/SETT). The remaining
//! ~120 ops land in task #4.

use crate::bus::{AccessKind, Bus};
use crate::cache::Cache;
use crate::decoder::decode;
use crate::isa::Op;
use crate::pipeline::Pipeline;
use crate::regs::Registers;

/// One Hitachi SH-2 (SH7604) core.
#[derive(Clone, Debug)]
pub struct Cpu {
    pub regs: Registers,
    pub pipeline: Pipeline,
    pub cache: Cache,
    /// When `Some`, the next instruction is a delay-slot fetch; after it
    /// executes PC is overwritten with the contained target.
    pub(crate) pending_branch: Option<u32>,
    /// True only while the slot instruction itself is executing.
    pub(crate) in_delay_slot: bool,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            regs: Registers::new_at_reset(),
            pipeline: Pipeline::new(),
            cache: Cache::new(),
            pending_branch: None,
            in_delay_slot: false,
        }
    }

    /// Power-on reset. Loads PC and R15 from the reset vector.
    pub fn reset(&mut self, bus: &mut impl Bus) {
        self.regs = Registers::new_at_reset();
        self.pipeline = Pipeline::new();
        self.pending_branch = None;
        self.in_delay_slot = false;
        let (pc, _) = bus.read32(0x0000_0000, AccessKind::Data);
        let (sp, _) = bus.read32(0x0000_0004, AccessKind::Data);
        self.regs.pc = pc;
        self.regs.r[15] = sp;
    }

    /// Fetch + decode + execute one instruction. Returns total cycles
    /// (instruction issue cost + bus stalls).
    pub fn step(&mut self, bus: &mut impl Bus) -> u32 {
        let instr_pc = self.regs.pc;
        let (word, fetch_stall) = bus.read16(instr_pc, AccessKind::Fetch);
        self.regs.pc = instr_pc.wrapping_add(2);
        let op = decode(word);

        let was_pending = self.pending_branch.is_some();
        self.in_delay_slot = was_pending;
        if was_pending && op.is_illegal_in_slot() {
            // Slot illegal instruction (vector 6). Full handling in task #7;
            // for M1 we discard the branch and continue so trace tooling
            // doesn't wedge.
            self.pending_branch = None;
        }

        let exec_cycles = self.execute(op, instr_pc, bus);

        if was_pending
            && let Some(target) = self.pending_branch.take()
        {
            self.regs.pc = target;
        }
        self.in_delay_slot = false;

        let total = fetch_stall + exec_cycles;
        self.pipeline.advance(total);
        total
    }

    /// Execute one decoded op. Returns the issue cycle cost; bus stalls from
    /// data accesses are accumulated inline.
    fn execute(&mut self, op: Op, instr_pc: u32, bus: &mut impl Bus) -> u32 {
        use Op::*;
        match op {
            // ---- System ----
            Nop => 1,
            Clrt => {
                self.regs.sr.set_t(false);
                1
            }
            Sett => {
                self.regs.sr.set_t(true);
                1
            }

            // ---- Data transfer (subset) ----
            MovI { rn, imm } => {
                self.regs.r[rn as usize] = imm as i32 as u32;
                1
            }
            MovRR { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize];
                1
            }
            MovLS { rn, rm } => {
                let addr = self.regs.r[rn as usize];
                let val = self.regs.r[rm as usize];
                let s = bus.write32(addr, val, AccessKind::Data);
                1 + s
            }
            MovLL { rn, rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = bus.read32(addr, AccessKind::Data);
                self.regs.r[rn as usize] = val;
                1 + s
            }
            MovLM { rn, rm } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let val = self.regs.r[rm as usize];
                let s = bus.write32(addr, val, AccessKind::Data);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            MovLP { rn, rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = bus.read32(addr, AccessKind::Data);
                self.regs.r[rn as usize] = val;
                if rn != rm {
                    self.regs.r[rm as usize] = addr.wrapping_add(4);
                }
                1 + s
            }
            MovLS4 { rn, rm, disp } => {
                let addr = self.regs.r[rn as usize].wrapping_add((disp as u32) << 2);
                let val = self.regs.r[rm as usize];
                let s = bus.write32(addr, val, AccessKind::Data);
                1 + s
            }
            MovLL4 { rn, rm, disp } => {
                let addr = self.regs.r[rm as usize].wrapping_add((disp as u32) << 2);
                let (val, s) = bus.read32(addr, AccessKind::Data);
                self.regs.r[rn as usize] = val;
                1 + s
            }
            MovLPcRel { rn, disp } => {
                // PC-relative address: (PC_of_instr + 4 + disp*4) & ~3.
                let addr = (instr_pc.wrapping_add(4).wrapping_add((disp as u32) << 2)) & !3;
                let (val, s) = bus.read32(addr, AccessKind::Data);
                self.regs.r[rn as usize] = val;
                1 + s
            }

            // ---- Arithmetic (subset) ----
            Add { rn, rm } => {
                self.regs.r[rn as usize] =
                    self.regs.r[rn as usize].wrapping_add(self.regs.r[rm as usize]);
                1
            }
            AddI { rn, imm } => {
                self.regs.r[rn as usize] =
                    self.regs.r[rn as usize].wrapping_add(imm as i32 as u32);
                1
            }
            Sub { rn, rm } => {
                self.regs.r[rn as usize] =
                    self.regs.r[rn as usize].wrapping_sub(self.regs.r[rm as usize]);
                1
            }
            CmpEq { rn, rm } => {
                let t = self.regs.r[rn as usize] == self.regs.r[rm as usize];
                self.regs.sr.set_t(t);
                1
            }
            CmpEqI { imm } => {
                let t = self.regs.r[0] == (imm as i32 as u32);
                self.regs.sr.set_t(t);
                1
            }
            CmpHs { rn, rm } => {
                let t = self.regs.r[rn as usize] >= self.regs.r[rm as usize];
                self.regs.sr.set_t(t);
                1
            }
            CmpGe { rn, rm } => {
                let t = (self.regs.r[rn as usize] as i32) >= (self.regs.r[rm as usize] as i32);
                self.regs.sr.set_t(t);
                1
            }
            CmpHi { rn, rm } => {
                let t = self.regs.r[rn as usize] > self.regs.r[rm as usize];
                self.regs.sr.set_t(t);
                1
            }
            CmpGt { rn, rm } => {
                let t = (self.regs.r[rn as usize] as i32) > (self.regs.r[rm as usize] as i32);
                self.regs.sr.set_t(t);
                1
            }

            // ---- Branches (subset) ----
            // SH-2 PC-relative branch base is `PC_of_instr + 4`.
            Bra { disp } => {
                let target = instr_pc
                    .wrapping_add(4)
                    .wrapping_add(((disp as i32) << 1) as u32);
                self.pending_branch = Some(target);
                2
            }
            Bsr { disp } => {
                self.regs.pr = instr_pc.wrapping_add(4);
                let target = instr_pc
                    .wrapping_add(4)
                    .wrapping_add(((disp as i32) << 1) as u32);
                self.pending_branch = Some(target);
                2
            }
            Jmp { rm } => {
                self.pending_branch = Some(self.regs.r[rm as usize]);
                2
            }
            Jsr { rm } => {
                self.regs.pr = instr_pc.wrapping_add(4);
                self.pending_branch = Some(self.regs.r[rm as usize]);
                2
            }
            Rts => {
                self.pending_branch = Some(self.regs.pr);
                2
            }
            Bt { disp } => {
                if self.regs.sr.t() {
                    self.regs.pc = instr_pc
                        .wrapping_add(4)
                        .wrapping_add(((disp as i32) << 1) as u32);
                    3
                } else {
                    1
                }
            }
            Bf { disp } => {
                if !self.regs.sr.t() {
                    self.regs.pc = instr_pc
                        .wrapping_add(4)
                        .wrapping_add(((disp as i32) << 1) as u32);
                    3
                } else {
                    1
                }
            }
            BtS { disp } => {
                if self.regs.sr.t() {
                    let target = instr_pc
                        .wrapping_add(4)
                        .wrapping_add(((disp as i32) << 1) as u32);
                    self.pending_branch = Some(target);
                    2
                } else {
                    1
                }
            }
            BfS { disp } => {
                if !self.regs.sr.t() {
                    let target = instr_pc
                        .wrapping_add(4)
                        .wrapping_add(((disp as i32) << 1) as u32);
                    self.pending_branch = Some(target);
                    2
                } else {
                    1
                }
            }

            Illegal(_) => {
                // Vector 4 dispatch lands in task #7. For now: stall 1 cycle
                // so test fixtures don't deadlock.
                1
            }

            // Remaining ops land in task #4. Decoded but not yet executed —
            // treat as NOP-ish so a partly-implemented program can still be
            // single-stepped without panicking.
            _ => 1,
        }
    }
}
