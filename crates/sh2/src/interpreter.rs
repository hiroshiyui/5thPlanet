//! Top-level CPU state and instruction dispatch.
//!
//! Task #4 lands the full SH-2 opcode set. Cycle counts are the base values
//! from *SH-1/SH-2 Software Manual* Appendix A; pipeline interlocks that
//! refine them land in task #5. Exception handling (TRAPA stack frame, RTE
//! pop, slot-illegal / address-error vectoring) gets its full plumbing in
//! task #7 — what's here is enough for software TRAPA/RTE round-trips on a
//! well-behaved fixture.

use crate::bus::{AccessKind, Bus};
use crate::cache::{self, Cache};
use crate::decoder::decode;
use crate::isa::Op;
use crate::onchip::OnChip;
use crate::pipeline::Pipeline;
use crate::regs::{Registers, Sr};

/// Decode an SH-2 logical address into (physical Saturn-bus address,
/// cacheable?). Cached and cache-through regions alias the same physical
/// memory; the cacheable flag tells the CPU whether to consult its cache.
/// Addresses outside both regions pass through unmodified — those are
/// SH-2 control areas the bus typically returns open-bus for.
#[inline]
const fn classify(addr: u32) -> (u32, bool) {
    match addr {
        0x0000_0000..=0x1FFF_FFFF => (addr, true),
        0x2000_0000..=0x3FFF_FFFF => (addr & 0x1FFF_FFFF, false),
        _ => (addr, false),
    }
}

/// SH7604 Cache Control Register. 8-bit, byte-accessed. It lives in the
/// on-chip address window but controls [`Cache`], not [`OnChip`], so the
/// memory path routes it here explicitly rather than letting the generic
/// `OnChip::owns` dispatch swallow it. (*SH7604 Hardware Manual* §8, CCR.)
const CCR_ADDR: u32 = 0xFFFF_FE92;

/// One Hitachi SH-2 (SH7604) core.
#[derive(Clone, Debug)]
pub struct Cpu {
    pub regs: Registers,
    pub pipeline: Pipeline,
    pub cache: Cache,
    /// SH7604 on-chip peripherals (FFFFFE00..FFFFFFFF). Memory accesses
    /// to that range are routed here by [`Cpu::mem_read32`] et al. before
    /// reaching the external [`Bus`].
    pub onchip: OnChip,
    /// When `Some`, the next instruction is a delay-slot fetch; after it
    /// executes PC is overwritten with the contained target.
    pub(crate) pending_branch: Option<u32>,
    /// True only while the slot instruction itself is executing.
    pub(crate) in_delay_slot: bool,
    /// The destination register of the most recently retired load. The
    /// very next instruction that reads it pays a 1-cycle stall, then
    /// this is cleared regardless.
    pub(crate) load_dest_pending: Option<u8>,
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
            onchip: OnChip::new(),
            pending_branch: None,
            in_delay_slot: false,
            load_dest_pending: None,
        }
    }

    /// Power-on reset. Loads PC and R15 from the reset vector.
    pub fn reset(&mut self, bus: &mut impl Bus) {
        self.regs = Registers::new_at_reset();
        self.pipeline = Pipeline::new();
        self.pending_branch = None;
        self.in_delay_slot = false;
        self.load_dest_pending = None;
        let (pc, _) = self.mem_read32(0x0000_0000, AccessKind::Data, bus);
        let (sp, _) = self.mem_read32(0x0000_0004, AccessKind::Data, bus);
        self.regs.pc = pc;
        self.regs.r[15] = sp;
    }

    /// Fetch + decode + execute one instruction. Returns total cycles
    /// (instruction issue cost + bus stalls + interlock stalls).
    pub fn step(&mut self, bus: &mut impl Bus) -> u32 {
        // ---- Interrupt boundary: check pending INTC sources first ----
        // SH-2 only accepts interrupts at instruction boundaries (never
        // inside a delay slot — that's a hardware invariant).
        if self.pending_branch.is_none()
            && let Some((src, level)) = self.onchip.intc.next_pending(self.regs.sr.imask())
        {
            let vector = self.onchip.intc.vector_for(src);
            self.onchip.intc.acknowledge(src);
            // External (level-triggered) sources stay asserted until the
            // line drops; on-chip sources are edge-triggered and we just
            // cleared the bit above. The Saturn-side glue will re-raise
            // External(level) on every step while IRL is held.
            let cost = self.take_exception(vector, Some(level), bus);
            self.pipeline.advance(cost);
            return cost;
        }

        let instr_pc = self.regs.pc;
        let (word, fetch_stall) = self.mem_read16(instr_pc, AccessKind::Fetch, bus);
        self.regs.pc = instr_pc.wrapping_add(2);
        let op = decode(word);

        // ---- Pre-dispatch interlocks ----
        let mut interlock_stall = 0u32;
        if let Some(loaded) = self.load_dest_pending.take()
            && op.reads_reg(loaded)
        {
            interlock_stall += 1;
        }
        if op.reads_mac() {
            interlock_stall += self.pipeline.stall_for_mac();
        }

        let was_pending = self.pending_branch.is_some();
        self.in_delay_slot = was_pending;

        // ---- Slot-illegal instruction ----
        // A delay-slot containing a branch / SR-mutating op / PC-fetching
        // op raises vector 6. The pushed PC is the *branch* address
        // (instr_pc - 2), so RTE restarts the branch with consistent state.
        if was_pending && op.is_illegal_in_slot() {
            self.pending_branch = None;
            self.in_delay_slot = false;
            self.regs.pc = instr_pc; // un-advance: re-fetch the slot on return
            let cost = self.take_exception(6, None, bus);
            self.pipeline.advance(interlock_stall + fetch_stall + cost);
            return interlock_stall + fetch_stall + cost;
        }

        // ---- General illegal instruction ----
        if matches!(op, Op::Illegal(_)) {
            self.regs.pc = instr_pc; // RTE returns to the offending op
            let cost = self.take_exception(4, None, bus);
            self.pipeline.advance(interlock_stall + fetch_stall + cost);
            return interlock_stall + fetch_stall + cost;
        }

        let exec_cycles = self.execute(op, instr_pc, bus);

        if was_pending
            && let Some(target) = self.pending_branch.take()
        {
            self.regs.pc = target;
        }
        self.in_delay_slot = false;

        // ---- Post-dispatch scoreboard updates ----
        if let Some(rn) = op.load_dest() {
            self.load_dest_pending = Some(rn);
        }
        if let Some(lat) = op.multiply_latency() {
            self.pipeline
                .schedule_mac(lat + exec_cycles + interlock_stall);
        }

        let total = interlock_stall + fetch_stall + exec_cycles;
        self.pipeline.advance(total);
        total
    }

    /// Push SR then PC on the stack and vector through `VBR + vector*4`.
    /// Returns the bus-stall cycles incurred; the caller adds the fixed
    /// 5-cycle exception overhead. If `set_imask` is `Some(lvl)` the SR
    /// interrupt mask is raised to it after the push (interrupt entry).
    fn take_exception(
        &mut self,
        vector: u8,
        set_imask: Option<u8>,
        bus: &mut impl Bus,
    ) -> u32 {
        let mut sp = self.regs.r[15];
        sp = sp.wrapping_sub(4);
        let s1 = self.mem_write32(sp, self.regs.sr.0, AccessKind::Data, bus);
        sp = sp.wrapping_sub(4);
        let s2 = self.mem_write32(sp, self.regs.pc, AccessKind::Data, bus);
        self.regs.r[15] = sp;
        if let Some(lvl) = set_imask {
            self.regs.sr.set_imask(lvl);
        }
        let vec_addr = self.regs.vbr.wrapping_add((vector as u32) << 2);
        let (target, s3) = self.mem_read32(vec_addr, AccessKind::Data, bus);
        self.regs.pc = target;
        // Reset interlocks; the handler executes from a fresh pipeline
        // state, just like after a branch.
        self.pending_branch = None;
        self.load_dest_pending = None;
        5 + s1 + s2 + s3
    }

    fn execute(&mut self, op: Op, instr_pc: u32, bus: &mut impl Bus) -> u32 {
        use Op::*;
        match op {
            // ============================================================
            // System control
            // ============================================================
            Nop => 1,
            Clrt => {
                self.regs.sr.set_t(false);
                1
            }
            Sett => {
                self.regs.sr.set_t(true);
                1
            }
            Clrmac => {
                self.regs.mach = 0;
                self.regs.macl = 0;
                1
            }
            // SLEEP halts the CPU until an interrupt or NMI. For M1 we model
            // it as a 3-cycle NOP — wake-up plumbing arrives with task #7.
            Sleep => 3,

            // ============================================================
            // Data transfer
            // ============================================================
            MovI { rn, imm } => {
                self.regs.r[rn as usize] = imm as i32 as u32;
                1
            }
            MovWPcRel { rn, disp } => {
                let addr = instr_pc.wrapping_add(4).wrapping_add((disp as u32) << 1);
                let (val, s) = self.mem_read16(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i16 as i32 as u32;
                1 + s
            }
            MovLPcRel { rn, disp } => {
                let addr = (instr_pc.wrapping_add(4).wrapping_add((disp as u32) << 2)) & !3;
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val;
                1 + s
            }
            MovRR { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize];
                1
            }

            MovBS { rn, rm } => {
                let addr = self.regs.r[rn as usize];
                let val = self.regs.r[rm as usize] as u8;
                let s = self.mem_write8(addr, val, AccessKind::Data, bus);
                1 + s
            }
            MovWS { rn, rm } => {
                let addr = self.regs.r[rn as usize];
                let val = self.regs.r[rm as usize] as u16;
                let s = self.mem_write16(addr, val, AccessKind::Data, bus);
                1 + s
            }
            MovLS { rn, rm } => {
                let addr = self.regs.r[rn as usize];
                let val = self.regs.r[rm as usize];
                let s = self.mem_write32(addr, val, AccessKind::Data, bus);
                1 + s
            }
            MovBL { rn, rm } => {
                let (val, s) = self.mem_read8(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i8 as i32 as u32;
                1 + s
            }
            MovWL { rn, rm } => {
                let (val, s) = self.mem_read16(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i16 as i32 as u32;
                1 + s
            }
            MovLL { rn, rm } => {
                let (val, s) = self.mem_read32(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val;
                1 + s
            }
            MovBM { rn, rm } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(1);
                let s = self.mem_write8(addr, self.regs.r[rm as usize] as u8, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            MovWM { rn, rm } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(2);
                let s = self.mem_write16(addr, self.regs.r[rm as usize] as u16, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            MovLM { rn, rm } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            MovBP { rn, rm } => {
                let (val, s) = self.mem_read8(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i8 as i32 as u32;
                if rn != rm {
                    self.regs.r[rm as usize] = self.regs.r[rm as usize].wrapping_add(1);
                }
                1 + s
            }
            MovWP { rn, rm } => {
                let (val, s) = self.mem_read16(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i16 as i32 as u32;
                if rn != rm {
                    self.regs.r[rm as usize] = self.regs.r[rm as usize].wrapping_add(2);
                }
                1 + s
            }
            MovLP { rn, rm } => {
                let (val, s) = self.mem_read32(self.regs.r[rm as usize], AccessKind::Data, bus);
                self.regs.r[rn as usize] = val;
                if rn != rm {
                    self.regs.r[rm as usize] = self.regs.r[rm as usize].wrapping_add(4);
                }
                1 + s
            }

            MovBS0 { rn, disp } => {
                let addr = self.regs.r[rn as usize].wrapping_add(disp as u32);
                let s = self.mem_write8(addr, self.regs.r[0] as u8, AccessKind::Data, bus);
                1 + s
            }
            MovWS0 { rn, disp } => {
                let addr = self.regs.r[rn as usize].wrapping_add((disp as u32) << 1);
                let s = self.mem_write16(addr, self.regs.r[0] as u16, AccessKind::Data, bus);
                1 + s
            }
            MovLS4 { rn, rm, disp } => {
                let addr = self.regs.r[rn as usize].wrapping_add((disp as u32) << 2);
                let s = self.mem_write32(addr, self.regs.r[rm as usize], AccessKind::Data, bus);
                1 + s
            }
            MovBL0 { rm, disp } => {
                let addr = self.regs.r[rm as usize].wrapping_add(disp as u32);
                let (val, s) = self.mem_read8(addr, AccessKind::Data, bus);
                self.regs.r[0] = val as i8 as i32 as u32;
                1 + s
            }
            MovWL0 { rm, disp } => {
                let addr = self.regs.r[rm as usize].wrapping_add((disp as u32) << 1);
                let (val, s) = self.mem_read16(addr, AccessKind::Data, bus);
                self.regs.r[0] = val as i16 as i32 as u32;
                1 + s
            }
            MovLL4 { rn, rm, disp } => {
                let addr = self.regs.r[rm as usize].wrapping_add((disp as u32) << 2);
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val;
                1 + s
            }

            MovBSX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rn as usize]);
                let s = self.mem_write8(addr, self.regs.r[rm as usize] as u8, AccessKind::Data, bus);
                1 + s
            }
            MovWSX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rn as usize]);
                let s = self.mem_write16(addr, self.regs.r[rm as usize] as u16, AccessKind::Data, bus);
                1 + s
            }
            MovLSX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rn as usize]);
                let s = self.mem_write32(addr, self.regs.r[rm as usize], AccessKind::Data, bus);
                1 + s
            }
            MovBLX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rm as usize]);
                let (val, s) = self.mem_read8(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i8 as i32 as u32;
                1 + s
            }
            MovWLX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rm as usize]);
                let (val, s) = self.mem_read16(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val as i16 as i32 as u32;
                1 + s
            }
            MovLLX { rn, rm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.r[rm as usize]);
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = val;
                1 + s
            }

            MovBSG { disp } => {
                let addr = self.regs.gbr.wrapping_add(disp as u32);
                let s = self.mem_write8(addr, self.regs.r[0] as u8, AccessKind::Data, bus);
                1 + s
            }
            MovWSG { disp } => {
                let addr = self.regs.gbr.wrapping_add((disp as u32) << 1);
                let s = self.mem_write16(addr, self.regs.r[0] as u16, AccessKind::Data, bus);
                1 + s
            }
            MovLSG { disp } => {
                let addr = self.regs.gbr.wrapping_add((disp as u32) << 2);
                let s = self.mem_write32(addr, self.regs.r[0], AccessKind::Data, bus);
                1 + s
            }
            MovBLG { disp } => {
                let addr = self.regs.gbr.wrapping_add(disp as u32);
                let (val, s) = self.mem_read8(addr, AccessKind::Data, bus);
                self.regs.r[0] = val as i8 as i32 as u32;
                1 + s
            }
            MovWLG { disp } => {
                let addr = self.regs.gbr.wrapping_add((disp as u32) << 1);
                let (val, s) = self.mem_read16(addr, AccessKind::Data, bus);
                self.regs.r[0] = val as i16 as i32 as u32;
                1 + s
            }
            MovLLG { disp } => {
                let addr = self.regs.gbr.wrapping_add((disp as u32) << 2);
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[0] = val;
                1 + s
            }

            Mova { disp } => {
                self.regs.r[0] = (instr_pc.wrapping_add(4).wrapping_add((disp as u32) << 2)) & !3;
                1
            }
            Movt { rn } => {
                self.regs.r[rn as usize] = self.regs.sr.t() as u32;
                1
            }
            SwapB { rn, rm } => {
                let m = self.regs.r[rm as usize];
                self.regs.r[rn as usize] =
                    (m & 0xFFFF_0000) | ((m & 0xFF) << 8) | ((m & 0xFF00) >> 8);
                1
            }
            SwapW { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize].rotate_left(16);
                1
            }
            Xtrct { rn, rm } => {
                let m = self.regs.r[rm as usize];
                let n = self.regs.r[rn as usize];
                self.regs.r[rn as usize] = ((m & 0xFFFF) << 16) | ((n >> 16) & 0xFFFF);
                1
            }

            // ============================================================
            // Arithmetic
            // ============================================================
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
            Addc { rn, rm } => {
                let t_in = self.regs.sr.t() as u32;
                let (s1, c1) = self.regs.r[rn as usize].overflowing_add(self.regs.r[rm as usize]);
                let (s2, c2) = s1.overflowing_add(t_in);
                self.regs.r[rn as usize] = s2;
                self.regs.sr.set_t(c1 || c2);
                1
            }
            Addv { rn, rm } => {
                let (s, ov) =
                    (self.regs.r[rn as usize] as i32).overflowing_add(self.regs.r[rm as usize] as i32);
                self.regs.r[rn as usize] = s as u32;
                self.regs.sr.set_t(ov);
                1
            }
            Sub { rn, rm } => {
                self.regs.r[rn as usize] =
                    self.regs.r[rn as usize].wrapping_sub(self.regs.r[rm as usize]);
                1
            }
            Subc { rn, rm } => {
                let t_in = self.regs.sr.t() as u32;
                let (s1, b1) = self.regs.r[rn as usize].overflowing_sub(self.regs.r[rm as usize]);
                let (s2, b2) = s1.overflowing_sub(t_in);
                self.regs.r[rn as usize] = s2;
                self.regs.sr.set_t(b1 || b2);
                1
            }
            Subv { rn, rm } => {
                let (s, ov) =
                    (self.regs.r[rn as usize] as i32).overflowing_sub(self.regs.r[rm as usize] as i32);
                self.regs.r[rn as usize] = s as u32;
                self.regs.sr.set_t(ov);
                1
            }
            Neg { rn, rm } => {
                self.regs.r[rn as usize] = 0u32.wrapping_sub(self.regs.r[rm as usize]);
                1
            }
            Negc { rn, rm } => {
                let t_in = self.regs.sr.t() as u32;
                let (s1, b1) = 0u32.overflowing_sub(self.regs.r[rm as usize]);
                let (s2, b2) = s1.overflowing_sub(t_in);
                self.regs.r[rn as usize] = s2;
                self.regs.sr.set_t(b1 || b2);
                1
            }
            Dt { rn } => {
                let v = self.regs.r[rn as usize].wrapping_sub(1);
                self.regs.r[rn as usize] = v;
                self.regs.sr.set_t(v == 0);
                1
            }

            CmpEqI { imm } => {
                self.regs.sr.set_t(self.regs.r[0] == (imm as i32 as u32));
                1
            }
            CmpEq { rn, rm } => {
                self.regs
                    .sr
                    .set_t(self.regs.r[rn as usize] == self.regs.r[rm as usize]);
                1
            }
            CmpHs { rn, rm } => {
                self.regs
                    .sr
                    .set_t(self.regs.r[rn as usize] >= self.regs.r[rm as usize]);
                1
            }
            CmpGe { rn, rm } => {
                self.regs
                    .sr
                    .set_t((self.regs.r[rn as usize] as i32) >= (self.regs.r[rm as usize] as i32));
                1
            }
            CmpHi { rn, rm } => {
                self.regs
                    .sr
                    .set_t(self.regs.r[rn as usize] > self.regs.r[rm as usize]);
                1
            }
            CmpGt { rn, rm } => {
                self.regs
                    .sr
                    .set_t((self.regs.r[rn as usize] as i32) > (self.regs.r[rm as usize] as i32));
                1
            }
            CmpPl { rn } => {
                self.regs.sr.set_t((self.regs.r[rn as usize] as i32) > 0);
                1
            }
            CmpPz { rn } => {
                self.regs.sr.set_t((self.regs.r[rn as usize] as i32) >= 0);
                1
            }
            CmpStr { rn, rm } => {
                // T = any byte of (Rn ^ Rm) is zero — i.e. any matching byte.
                let x = self.regs.r[rn as usize] ^ self.regs.r[rm as usize];
                let any_zero_byte = (x & 0xFF00_0000) == 0
                    || (x & 0x00FF_0000) == 0
                    || (x & 0x0000_FF00) == 0
                    || (x & 0x0000_00FF) == 0;
                self.regs.sr.set_t(any_zero_byte);
                1
            }

            ExtsB { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize] as i8 as i32 as u32;
                1
            }
            ExtsW { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize] as i16 as i32 as u32;
                1
            }
            ExtuB { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize] & 0xFF;
                1
            }
            ExtuW { rn, rm } => {
                self.regs.r[rn as usize] = self.regs.r[rm as usize] & 0xFFFF;
                1
            }

            // ---- Multiplies ----
            MulL { rn, rm } => {
                self.regs.macl = self.regs.r[rn as usize].wrapping_mul(self.regs.r[rm as usize]);
                // Manual: 2–4 cycles. Use 2 as base; pipeline interlock model
                // (task #5) will refine this with MAC-ready scoreboard.
                2
            }
            MulsW { rn, rm } => {
                let m = self.regs.r[rm as usize] as i16 as i32;
                let n = self.regs.r[rn as usize] as i16 as i32;
                self.regs.macl = (m.wrapping_mul(n)) as u32;
                1
            }
            MuluW { rn, rm } => {
                let m = (self.regs.r[rm as usize] as u16) as u32;
                let n = (self.regs.r[rn as usize] as u16) as u32;
                self.regs.macl = m.wrapping_mul(n);
                1
            }
            DmulsL { rn, rm } => {
                let prod = (self.regs.r[rn as usize] as i32 as i64)
                    .wrapping_mul(self.regs.r[rm as usize] as i32 as i64);
                self.regs.macl = prod as u32;
                self.regs.mach = (prod >> 32) as u32;
                2
            }
            DmuluL { rn, rm } => {
                let prod = (self.regs.r[rn as usize] as u64)
                    .wrapping_mul(self.regs.r[rm as usize] as u64);
                self.regs.macl = prod as u32;
                self.regs.mach = (prod >> 32) as u32;
                2
            }
            MacL { rn, rm } => self.exec_mac_l(rn, rm, bus),
            MacW { rn, rm } => self.exec_mac_w(rn, rm, bus),

            // ---- Division ----
            Div0u => {
                self.regs.sr.set_m(false);
                self.regs.sr.set_q(false);
                self.regs.sr.set_t(false);
                1
            }
            Div0s { rn, rm } => {
                let m = (self.regs.r[rm as usize] as i32) < 0;
                let q = (self.regs.r[rn as usize] as i32) < 0;
                self.regs.sr.set_m(m);
                self.regs.sr.set_q(q);
                self.regs.sr.set_t(m != q);
                1
            }
            Div1 { rn, rm } => {
                self.exec_div1(rn, rm);
                1
            }

            // ============================================================
            // Logical
            // ============================================================
            And { rn, rm } => {
                self.regs.r[rn as usize] &= self.regs.r[rm as usize];
                1
            }
            AndI { imm } => {
                self.regs.r[0] &= imm as u32;
                1
            }
            AndBG { imm } => self.exec_logical_bg(imm, bus, |a, b| a & b),
            Or { rn, rm } => {
                self.regs.r[rn as usize] |= self.regs.r[rm as usize];
                1
            }
            OrI { imm } => {
                self.regs.r[0] |= imm as u32;
                1
            }
            OrBG { imm } => self.exec_logical_bg(imm, bus, |a, b| a | b),
            Xor { rn, rm } => {
                self.regs.r[rn as usize] ^= self.regs.r[rm as usize];
                1
            }
            XorI { imm } => {
                self.regs.r[0] ^= imm as u32;
                1
            }
            XorBG { imm } => self.exec_logical_bg(imm, bus, |a, b| a ^ b),
            Not { rn, rm } => {
                self.regs.r[rn as usize] = !self.regs.r[rm as usize];
                1
            }
            Tst { rn, rm } => {
                self.regs
                    .sr
                    .set_t((self.regs.r[rn as usize] & self.regs.r[rm as usize]) == 0);
                1
            }
            TstI { imm } => {
                self.regs.sr.set_t((self.regs.r[0] & imm as u32) == 0);
                1
            }
            TstBG { imm } => {
                let addr = self.regs.r[0].wrapping_add(self.regs.gbr);
                let (val, s) = self.mem_read8(addr, AccessKind::Data, bus);
                self.regs.sr.set_t((val & imm) == 0);
                3 + s
            }
            Tas { rn } => {
                let addr = self.regs.r[rn as usize];
                let (val, sr) = self.mem_read8(addr, AccessKind::Data, bus);
                self.regs.sr.set_t(val == 0);
                let sw = self.mem_write8(addr, val | 0x80, AccessKind::Data, bus);
                4 + sr + sw
            }

            // ============================================================
            // Shifts / rotates
            // ============================================================
            Shll { rn } | Shal { rn } => {
                let v = self.regs.r[rn as usize];
                self.regs.sr.set_t(v & 0x8000_0000 != 0);
                self.regs.r[rn as usize] = v << 1;
                1
            }
            Shlr { rn } => {
                let v = self.regs.r[rn as usize];
                self.regs.sr.set_t(v & 1 != 0);
                self.regs.r[rn as usize] = v >> 1;
                1
            }
            Shar { rn } => {
                let v = self.regs.r[rn as usize];
                self.regs.sr.set_t(v & 1 != 0);
                self.regs.r[rn as usize] = ((v as i32) >> 1) as u32;
                1
            }
            Rotl { rn } => {
                let v = self.regs.r[rn as usize];
                self.regs.sr.set_t(v & 0x8000_0000 != 0);
                self.regs.r[rn as usize] = v.rotate_left(1);
                1
            }
            Rotr { rn } => {
                let v = self.regs.r[rn as usize];
                self.regs.sr.set_t(v & 1 != 0);
                self.regs.r[rn as usize] = v.rotate_right(1);
                1
            }
            Rotcl { rn } => {
                let v = self.regs.r[rn as usize];
                let new_msb = v & 0x8000_0000 != 0;
                self.regs.r[rn as usize] = (v << 1) | (self.regs.sr.t() as u32);
                self.regs.sr.set_t(new_msb);
                1
            }
            Rotcr { rn } => {
                let v = self.regs.r[rn as usize];
                let new_lsb = v & 1 != 0;
                self.regs.r[rn as usize] = ((self.regs.sr.t() as u32) << 31) | (v >> 1);
                self.regs.sr.set_t(new_lsb);
                1
            }
            Shll2 { rn } => {
                self.regs.r[rn as usize] <<= 2;
                1
            }
            Shlr2 { rn } => {
                self.regs.r[rn as usize] >>= 2;
                1
            }
            Shll8 { rn } => {
                self.regs.r[rn as usize] <<= 8;
                1
            }
            Shlr8 { rn } => {
                self.regs.r[rn as usize] >>= 8;
                1
            }
            Shll16 { rn } => {
                self.regs.r[rn as usize] <<= 16;
                1
            }
            Shlr16 { rn } => {
                self.regs.r[rn as usize] >>= 16;
                1
            }

            // ============================================================
            // Branches
            // ============================================================
            Bra { disp } => {
                self.pending_branch = Some(
                    instr_pc
                        .wrapping_add(4)
                        .wrapping_add(((disp as i32) << 1) as u32),
                );
                2
            }
            Bsr { disp } => {
                self.regs.pr = instr_pc.wrapping_add(4);
                self.pending_branch = Some(
                    instr_pc
                        .wrapping_add(4)
                        .wrapping_add(((disp as i32) << 1) as u32),
                );
                2
            }
            Braf { rm } => {
                self.pending_branch =
                    Some(instr_pc.wrapping_add(4).wrapping_add(self.regs.r[rm as usize]));
                2
            }
            Bsrf { rm } => {
                self.regs.pr = instr_pc.wrapping_add(4);
                self.pending_branch =
                    Some(instr_pc.wrapping_add(4).wrapping_add(self.regs.r[rm as usize]));
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
                    self.pending_branch = Some(
                        instr_pc
                            .wrapping_add(4)
                            .wrapping_add(((disp as i32) << 1) as u32),
                    );
                    2
                } else {
                    1
                }
            }
            BfS { disp } => {
                if !self.regs.sr.t() {
                    self.pending_branch = Some(
                        instr_pc
                            .wrapping_add(4)
                            .wrapping_add(((disp as i32) << 1) as u32),
                    );
                    2
                } else {
                    1
                }
            }

            // ============================================================
            // Control-register transfer
            // ============================================================
            LdcSr { rm } => {
                self.regs.sr.0 = self.regs.r[rm as usize] & Sr::WRITE_MASK;
                1
            }
            LdcGbr { rm } => {
                self.regs.gbr = self.regs.r[rm as usize];
                1
            }
            LdcVbr { rm } => {
                self.regs.vbr = self.regs.r[rm as usize];
                1
            }
            LdcLSr { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.sr.0 = val & Sr::WRITE_MASK;
                3 + s
            }
            LdcLGbr { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.gbr = val;
                3 + s
            }
            LdcLVbr { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.vbr = val;
                3 + s
            }
            StcSr { rn } => {
                self.regs.r[rn as usize] = self.regs.sr.0;
                1
            }
            StcGbr { rn } => {
                self.regs.r[rn as usize] = self.regs.gbr;
                1
            }
            StcVbr { rn } => {
                self.regs.r[rn as usize] = self.regs.vbr;
                1
            }
            StcLSr { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.sr.0, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                2 + s
            }
            StcLGbr { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.gbr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                2 + s
            }
            StcLVbr { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.vbr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                2 + s
            }
            LdsMach { rm } => {
                self.regs.mach = self.regs.r[rm as usize];
                1
            }
            LdsMacl { rm } => {
                self.regs.macl = self.regs.r[rm as usize];
                1
            }
            LdsPr { rm } => {
                self.regs.pr = self.regs.r[rm as usize];
                1
            }
            LdsLMach { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.mach = val;
                1 + s
            }
            LdsLMacl { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.macl = val;
                1 + s
            }
            LdsLPr { rm } => {
                let addr = self.regs.r[rm as usize];
                let (val, s) = self.mem_read32(addr, AccessKind::Data, bus);
                self.regs.r[rm as usize] = addr.wrapping_add(4);
                self.regs.pr = val;
                1 + s
            }
            StsMach { rn } => {
                self.regs.r[rn as usize] = self.regs.mach;
                1
            }
            StsMacl { rn } => {
                self.regs.r[rn as usize] = self.regs.macl;
                1
            }
            StsPr { rn } => {
                self.regs.r[rn as usize] = self.regs.pr;
                1
            }
            StsLMach { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.mach, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            StsLMacl { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.macl, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }
            StsLPr { rn } => {
                let addr = self.regs.r[rn as usize].wrapping_sub(4);
                let s = self.mem_write32(addr, self.regs.pr, AccessKind::Data, bus);
                self.regs.r[rn as usize] = addr;
                1 + s
            }

            // ============================================================
            // Exception primitives
            // ============================================================
            Trapa { imm } => {
                // self.regs.pc is already instr_pc + 2 (advanced in step()),
                // so take_exception pushes the correct resume address.
                let cost = self.take_exception(imm, None, bus);
                3 + cost
            }
            Rte => {
                // Pop PC then SR. RTE has a delay slot; the slot's PC is the
                // instruction after RTE, but the architecture says the slot
                // executes *before* the popped PC takes effect, so we mirror
                // the branch path: set pending_branch to the popped PC.
                let sp = self.regs.r[15];
                let (new_pc, s1) = self.mem_read32(sp, AccessKind::Data, bus);
                let (new_sr, s2) = self.mem_read32(sp.wrapping_add(4), AccessKind::Data, bus);
                self.regs.r[15] = sp.wrapping_add(8);
                self.regs.sr.0 = new_sr & Sr::WRITE_MASK;
                self.pending_branch = Some(new_pc);
                4 + s1 + s2
            }

            Illegal(_) => {
                // Intercepted in step() before reaching execute(): vector 4
                // is taken immediately. Reaching this arm means the
                // step-level guard regressed.
                unreachable!("Op::Illegal must be handled in step(), not execute()");
            }
        }
    }

    // ----------------------------------------------------------------------
    // Memory routing
    // ----------------------------------------------------------------------
    //
    // Every CPU memory access goes through these helpers. SH7604 address
    // space is partitioned by the top 3 bits:
    //
    //   0x00000000..0x1FFFFFFF  cached area      → probe cache, miss-fills line
    //   0x20000000..0x3FFFFFFF  cache-through    → bypass cache, present masked addr
    //   0xFFFFFE00..0xFFFFFFFF  on-chip peripherals (intercepted before bus)
    //   anything else           pass through to bus untouched (control regions)
    //
    // The cache uses the masked physical address (low 29 bits) for tag
    // matching so a cached and cache-through access to the same physical
    // memory see the same line storage.

    #[inline]
    pub(crate) fn mem_read8(&mut self, addr: u32, kind: AccessKind, bus: &mut impl Bus) -> (u8, u32) {
        if addr == CCR_ADDR {
            return (self.cache.ccr(), 0);
        }
        if OnChip::owns(addr) {
            return (self.onchip.read8(addr), 0);
        }
        let (phys, cacheable) = classify(addr);
        if cacheable
            && let Some((line, stall)) = self.cache_fill(phys, kind, bus)
        {
            return (cache::extract_u8(&line, phys), stall);
        }
        bus.read8(phys, kind)
    }

    #[inline]
    pub(crate) fn mem_read16(&mut self, addr: u32, kind: AccessKind, bus: &mut impl Bus) -> (u16, u32) {
        if OnChip::owns(addr) {
            return (self.onchip.read16(addr), 0);
        }
        let (phys, cacheable) = classify(addr);
        if cacheable
            && let Some((line, stall)) = self.cache_fill(phys, kind, bus)
        {
            return (cache::extract_u16(&line, phys), stall);
        }
        bus.read16(phys, kind)
    }

    #[inline]
    pub(crate) fn mem_read32(&mut self, addr: u32, kind: AccessKind, bus: &mut impl Bus) -> (u32, u32) {
        if OnChip::owns(addr) {
            return (self.onchip.read32(addr), 0);
        }
        let (phys, cacheable) = classify(addr);
        if cacheable
            && let Some((line, stall)) = self.cache_fill(phys, kind, bus)
        {
            return (cache::extract_u32(&line, phys), stall);
        }
        bus.read32(phys, kind)
    }

    #[inline]
    pub(crate) fn mem_write8(&mut self, addr: u32, val: u8, kind: AccessKind, bus: &mut impl Bus) -> u32 {
        if addr == CCR_ADDR {
            self.cache.set_ccr(val);
            return 0;
        }
        if OnChip::owns(addr) {
            self.onchip.write8(addr, val);
            return 0;
        }
        let (phys, cacheable) = classify(addr);
        if cacheable {
            // Write-through: update the cached line if resident, then
            // always reach the bus.
            self.cache.write_through_u8(phys, val);
        }
        bus.write8(phys, val, kind)
    }

    #[inline]
    pub(crate) fn mem_write16(&mut self, addr: u32, val: u16, kind: AccessKind, bus: &mut impl Bus) -> u32 {
        if OnChip::owns(addr) {
            self.onchip.write16(addr, val);
            return 0;
        }
        let (phys, cacheable) = classify(addr);
        if cacheable {
            self.cache.write_through_u16(phys, val);
        }
        bus.write16(phys, val, kind)
    }

    #[inline]
    pub(crate) fn mem_write32(&mut self, addr: u32, val: u32, kind: AccessKind, bus: &mut impl Bus) -> u32 {
        if OnChip::owns(addr) {
            self.onchip.write32(addr, val);
            return 0;
        }
        let (phys, cacheable) = classify(addr);
        if cacheable {
            self.cache.write_through_u32(phys, val);
        }
        bus.write32(phys, val, kind)
    }

    /// Cache miss-fill. Returns `Some((line, stall))` if the cache is
    /// active and we successfully obtained a line (hit or freshly filled);
    /// `None` if the cache is disabled or this access kind is masked off
    /// (ID/OD) — in which case the caller bypasses to the bus directly.
    ///
    /// On a miss we fetch the full 16-byte line aligned to `phys & !0xF`
    /// via four sequential `bus.read32` calls (the SH7604 burst is four
    /// 32-bit beats), install it, and return the populated line.
    fn cache_fill(
        &mut self,
        phys: u32,
        kind: AccessKind,
        bus: &mut impl Bus,
    ) -> Option<([u8; cache::LINE_BYTES], u32)> {
        let lookup = match kind {
            AccessKind::Fetch => self.cache.lookup_fetch(phys),
            AccessKind::Data | AccessKind::Dma => self.cache.lookup_data(phys),
        };
        match lookup {
            cache::Lookup::Hit(line) => Some((line, 0)),
            cache::Lookup::Bypass => None,
            cache::Lookup::Miss => {
                let base = phys & !0xF;
                let mut line = [0u8; cache::LINE_BYTES];
                let mut stall = 0u32;
                for chunk in 0..4u32 {
                    let (val, s) = bus.read32(base + chunk * 4, kind);
                    let off = (chunk * 4) as usize;
                    line[off..off + 4].copy_from_slice(&val.to_be_bytes());
                    stall += s;
                }
                self.cache.install(phys, line);
                Some((line, stall))
            }
        }
    }

    // ----------------------------------------------------------------------
    // Helpers
    // ----------------------------------------------------------------------

    /// AND.B/OR.B/XOR.B #imm,@(R0,GBR). 3 cycles plus bus stalls.
    fn exec_logical_bg(
        &mut self,
        imm: u8,
        bus: &mut impl Bus,
        f: fn(u8, u8) -> u8,
    ) -> u32 {
        let addr = self.regs.r[0].wrapping_add(self.regs.gbr);
        let (val, sr) = self.mem_read8(addr, AccessKind::Data, bus);
        let sw = self.mem_write8(addr, f(val, imm), AccessKind::Data, bus);
        3 + sr + sw
    }

    /// Non-restoring division step. Faithful port of the SH-2 software
    /// manual algorithm (§6, DIV1). Operates on Rn (dividend high half)
    /// using Rm as the divisor.
    fn exec_div1(&mut self, rn: u8, rm: u8) {
        let old_q = self.regs.sr.q();
        let m = self.regs.sr.m();
        let t_in = self.regs.sr.t();
        let divisor = self.regs.r[rm as usize];

        let new_q = self.regs.r[rn as usize] & 0x8000_0000 != 0;
        let shifted = (self.regs.r[rn as usize] << 1) | (t_in as u32);

        let (result, q) = if !old_q {
            if !m {
                let r = shifted.wrapping_sub(divisor);
                let tmp1 = r > shifted;
                let q = if new_q { !tmp1 } else { tmp1 };
                (r, q)
            } else {
                let r = shifted.wrapping_add(divisor);
                let tmp1 = r < shifted;
                let q = if new_q { tmp1 } else { !tmp1 };
                (r, q)
            }
        } else if !m {
            let r = shifted.wrapping_add(divisor);
            let tmp1 = r < shifted;
            let q = if new_q { !tmp1 } else { tmp1 };
            (r, q)
        } else {
            let r = shifted.wrapping_sub(divisor);
            let tmp1 = r > shifted;
            let q = if new_q { tmp1 } else { !tmp1 };
            (r, q)
        };

        self.regs.r[rn as usize] = result;
        self.regs.sr.set_q(q);
        self.regs.sr.set_t(q == m);
    }

    /// MAC.L @Rm+,@Rn+. Signed 32×32 multiply, 64-bit accumulate into
    /// MACH:MACL. S-bit saturation is implemented to the 48-bit signed
    /// range (per SH7604 manual); rare in practice but exercised by some
    /// DSP-heavy code paths.
    fn exec_mac_l(&mut self, rn: u8, rm: u8, bus: &mut impl Bus) -> u32 {
        let addr_m = self.regs.r[rm as usize];
        let (sm, s1) = self.mem_read32(addr_m, AccessKind::Data, bus);
        self.regs.r[rm as usize] = addr_m.wrapping_add(4);

        let addr_n = self.regs.r[rn as usize];
        let (sn, s2) = self.mem_read32(addr_n, AccessKind::Data, bus);
        self.regs.r[rn as usize] = addr_n.wrapping_add(4);

        let prod = (sm as i32 as i64).wrapping_mul(sn as i32 as i64);
        let acc = ((self.regs.mach as u64) << 32) | (self.regs.macl as u64);
        let sum = (acc as i64).wrapping_add(prod);

        let final_sum = if self.regs.sr.s() {
            const MAX: i64 = (1i64 << 47) - 1;
            const MIN: i64 = -(1i64 << 47);
            sum.clamp(MIN, MAX)
        } else {
            sum
        };
        self.regs.macl = final_sum as u32;
        self.regs.mach = (final_sum >> 32) as u32;
        3 + s1 + s2
    }

    /// MAC.W @Rm+,@Rn+. Signed 16×16 multiply. With S=0 the 32-bit product
    /// is added to MACH:MACL as a 64-bit signed value. With S=1 it
    /// accumulates into MACL only with 32-bit saturation (MACH retains the
    /// "overflow occurred" indicator in bit 0, per the SH7604 manual).
    fn exec_mac_w(&mut self, rn: u8, rm: u8, bus: &mut impl Bus) -> u32 {
        let addr_m = self.regs.r[rm as usize];
        let (sm, s1) = self.mem_read16(addr_m, AccessKind::Data, bus);
        self.regs.r[rm as usize] = addr_m.wrapping_add(2);

        let addr_n = self.regs.r[rn as usize];
        let (sn, s2) = self.mem_read16(addr_n, AccessKind::Data, bus);
        self.regs.r[rn as usize] = addr_n.wrapping_add(2);

        let prod = (sm as i16 as i32).wrapping_mul(sn as i16 as i32);

        if !self.regs.sr.s() {
            let acc = ((self.regs.mach as u64) << 32) | (self.regs.macl as u64);
            let sum = (acc as i64).wrapping_add(prod as i64);
            self.regs.macl = sum as u32;
            self.regs.mach = (sum >> 32) as u32;
        } else {
            let (sum, ov) = (self.regs.macl as i32).overflowing_add(prod);
            if ov {
                // Saturate and set the overflow flag in MACH bit 0.
                self.regs.macl = if prod < 0 { i32::MIN as u32 } else { i32::MAX as u32 };
                self.regs.mach |= 1;
            } else {
                self.regs.macl = sum as u32;
            }
        }
        3 + s1 + s2
    }
}
