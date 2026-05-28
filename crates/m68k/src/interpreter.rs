//! MC68EC000 interpreter.
//!
//! Decode-and-execute in one pass: the 68000's variable-length encoding
//! (opcode word + extension words, some consumed while resolving an
//! effective address) makes that more natural than a pre-decoded table.
//! `step` fetches the opcode word, dispatches on its top nibble, resolves
//! operands (reading any extension words via [`Cpu::fetch16`]/[`fetch32`]),
//! executes, and returns the cycles consumed.
//!
//! **Scope:** nearly the full user-mode ISA — MOVE/MOVEA/MOVEQ; the
//! ADD/SUB/AND/OR/EOR/CMP families incl. immediate, quick, address, and
//! extend (ADDX/SUBX) forms; MULU/MULS and DIVU/DIVS; ABCD/SBCD/NBCD;
//! NEG/NEGX/NOT/CLR/TST/TAS; the bit ops (BTST/BCHG/BCLR/BSET, static and
//! dynamic); EXT/SWAP/EXG; shifts/rotates; MOVEM and LINK/UNLK; the branch
//! group (BRA/BSR/Bcc/DBcc/Scc), RTS, JMP/JSR; and MOVE to/from CCR/SR.
//! The **exception model** is in too: TRAP/TRAPV/CHK, zero-divide,
//! illegal + line-A/F, privilege violation, STOP/RESET/RTE/RTR, and
//! external-interrupt dispatch (autovector, `SR.imask`-gated, level-7 NMI).
//! Still to come: address-error/bus-error stack frames (the 68000's longer
//! group-0 frame), and precise long-operation (MUL/DIV/shift) timing tables.
//!
//! **Cycle model:** each memory word access costs the 68000's 4-clock bus
//! cycle (8 for a long = two words), accumulated in [`Cpu::cycles`] along
//! with any host wait-states. REVIEW(magic): long-operation internal
//! penalties and the exact per-instruction timing tables (M68000 User's
//! Manual Appendix) are a later refinement — this counts bus traffic only.

use crate::bus::{AccessKind, Bus};
use crate::isa::{Cond, Size};
use crate::regs::Registers;

/// A resolved effective address: a register slot, a computed memory
/// address, or an already-fetched immediate.
#[derive(Clone, Copy, Debug)]
enum Ea {
    DataReg(usize),
    AddrReg(usize),
    Mem(u32),
    Imm(u32),
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cpu {
    pub regs: Registers,
    /// Total clock cycles consumed since construction.
    pub cycles: u64,
    /// Set by STOP (and on a halting double fault); the scheduler skips a
    /// stopped core until an interrupt wakes it.
    pub stopped: bool,
    /// Highest pending external interrupt level (0 = none, 1..7). The host
    /// (the SCSP, later) raises it; the CPU takes it at an instruction
    /// boundary when the level beats `SR.imask` (level 7 is non-maskable).
    pub pending_irq: u8,
}

/// Fixed 68000 exception vector numbers (vector address = number × 4).
mod vector {
    pub const ILLEGAL: u32 = 4;
    pub const ZERO_DIVIDE: u32 = 5;
    pub const CHK: u32 = 6;
    pub const TRAPV: u32 = 7;
    pub const PRIVILEGE: u32 = 8;
    pub const LINE_A: u32 = 10;
    pub const LINE_F: u32 = 11;
    pub const AUTOVECTOR_BASE: u32 = 24; // + level → 25..31
    pub const TRAP_BASE: u32 = 32; // + n → 32..47
}

impl Cpu {
    pub fn new() -> Self {
        Self::default()
    }

    /// Power-on reset: enter supervisor mode, mask all interrupts, load SSP
    /// from vector 0 and PC from vector 1 (M68000 manual §6.2.1).
    pub fn reset(&mut self, bus: &mut impl Bus) {
        self.regs.sr.supervisor = true;
        self.regs.sr.trace = false;
        self.regs.sr.imask = 7;
        let (ssp, _) = bus.read32(0x0000_0000, AccessKind::Data);
        let (pc, _) = bus.read32(0x0000_0004, AccessKind::Data);
        self.regs.ssp = ssp;
        self.regs.a[7] = ssp;
        self.regs.pc = pc;
        self.stopped = false;
        self.pending_irq = 0;
    }

    /// Raise an external interrupt at `level` (1..7); the CPU takes it at the
    /// next instruction boundary once it outranks `SR.imask`.
    pub fn raise_interrupt(&mut self, level: u8) {
        if level > self.pending_irq {
            self.pending_irq = level;
        }
    }

    /// Enter exception processing for `vector`: save SR, switch to supervisor
    /// (clearing T), push the PC (long) then SR (word) on the supervisor
    /// stack, and vector through `[vector × 4]`. The 68000 has no VBR — the
    /// table is fixed at the bottom of memory.
    fn take_exception(&mut self, vector: u32, bus: &mut impl Bus) {
        let saved_sr = self.regs.sr.to_u16();
        self.regs.set_supervisor(true);
        self.regs.sr.trace = false;
        self.push32(self.regs.pc, bus);
        self.push16(saved_sr, bus);
        self.regs.pc = self.read_mem(vector * 4, Size::Long, bus);
        self.stopped = false;
    }

    /// Take a pending interrupt if it outranks the mask (level 7 always).
    /// Returns true if one was taken.
    fn service_interrupt(&mut self, bus: &mut impl Bus) -> bool {
        let level = self.pending_irq;
        if level == 0 || (level != 7 && level <= self.regs.sr.imask) {
            return false;
        }
        self.pending_irq = 0;
        self.take_exception(vector::AUTOVECTOR_BASE + level as u32, bus);
        self.regs.sr.imask = level;
        true
    }

    // ---- fetch helpers ------------------------------------------------

    fn fetch16(&mut self, bus: &mut impl Bus) -> u16 {
        let (v, s) = bus.read16(self.regs.pc, AccessKind::Fetch);
        self.regs.pc = self.regs.pc.wrapping_add(2);
        self.cycles += 4 + s as u64;
        v
    }

    fn fetch32(&mut self, bus: &mut impl Bus) -> u32 {
        let hi = self.fetch16(bus) as u32;
        let lo = self.fetch16(bus) as u32;
        (hi << 16) | lo
    }

    // ---- memory access (with the 4-clock bus-cycle model) -------------

    fn read_mem(&mut self, addr: u32, size: Size, bus: &mut impl Bus) -> u32 {
        match size {
            Size::Byte => {
                let (v, s) = bus.read8(addr, AccessKind::Data);
                self.cycles += 4 + s as u64;
                v as u32
            }
            Size::Word => {
                let (v, s) = bus.read16(addr, AccessKind::Data);
                self.cycles += 4 + s as u64;
                v as u32
            }
            Size::Long => {
                let (v, s) = bus.read32(addr, AccessKind::Data);
                self.cycles += 8 + s as u64;
                v
            }
        }
    }

    fn write_mem(&mut self, addr: u32, size: Size, val: u32, bus: &mut impl Bus) {
        match size {
            Size::Byte => self.cycles += 4 + bus.write8(addr, val as u8, AccessKind::Data) as u64,
            Size::Word => self.cycles += 4 + bus.write16(addr, val as u16, AccessKind::Data) as u64,
            Size::Long => self.cycles += 8 + bus.write32(addr, val, AccessKind::Data) as u64,
        }
    }

    // ---- effective-address resolution ---------------------------------

    /// Resolve the EA encoded by `mode`/`reg`, consuming any extension
    /// words and applying post-increment / pre-decrement side effects.
    fn resolve_ea(&mut self, mode: u16, reg: u16, size: Size, bus: &mut impl Bus) -> Ea {
        let r = reg as usize;
        match mode {
            0 => Ea::DataReg(r),
            1 => Ea::AddrReg(r),
            2 => Ea::Mem(self.regs.a[r]),
            3 => {
                // (An)+
                let addr = self.regs.a[r];
                self.regs.a[r] = addr.wrapping_add(self.inc_size(r, size));
                Ea::Mem(addr)
            }
            4 => {
                // -(An)
                let addr = self.regs.a[r].wrapping_sub(self.inc_size(r, size));
                self.regs.a[r] = addr;
                Ea::Mem(addr)
            }
            5 => {
                // (d16,An)
                let d = self.fetch16(bus) as i16 as i32;
                Ea::Mem(self.regs.a[r].wrapping_add(d as u32))
            }
            6 => {
                // (d8,An,Xn)
                let base = self.regs.a[r];
                Ea::Mem(self.brief_index(base, bus))
            }
            7 => match reg {
                0 => Ea::Mem(self.fetch16(bus) as i16 as i32 as u32), // (xxx).W
                1 => Ea::Mem(self.fetch32(bus)),                      // (xxx).L
                2 => {
                    // (d16,PC) — base is the address of the extension word.
                    let base = self.regs.pc;
                    let d = self.fetch16(bus) as i16 as i32;
                    Ea::Mem(base.wrapping_add(d as u32))
                }
                3 => {
                    // (d8,PC,Xn)
                    let base = self.regs.pc;
                    Ea::Mem(self.brief_index(base, bus))
                }
                4 => {
                    // #imm
                    let v = match size {
                        Size::Byte => (self.fetch16(bus) & 0xFF) as u32,
                        Size::Word => self.fetch16(bus) as u32,
                        Size::Long => self.fetch32(bus),
                    };
                    Ea::Imm(v)
                }
                _ => Ea::Imm(0),
            },
            _ => unreachable!("EA mode is 3 bits"),
        }
    }

    /// Post-inc/pre-dec step: byte access to A7 still moves by 2 to keep the
    /// stack word-aligned (M68000 manual §2.3).
    fn inc_size(&self, reg: usize, size: Size) -> u32 {
        if reg == 7 && size == Size::Byte {
            2
        } else {
            size.bytes()
        }
    }

    /// Resolve a 68000 brief-format index word: `base + disp8 + Xn`, with the
    /// index taken as word (sign-extended) or long per the W/L bit.
    fn brief_index(&mut self, base: u32, bus: &mut impl Bus) -> u32 {
        let ext = self.fetch16(bus);
        let disp = (ext as i8) as i32;
        let ri = ((ext >> 12) & 7) as usize;
        let raw = if ext & 0x8000 != 0 {
            self.regs.a[ri]
        } else {
            self.regs.d[ri]
        };
        let idx = if ext & 0x0800 != 0 {
            raw
        } else {
            raw as u16 as i16 as i32 as u32
        };
        base.wrapping_add(disp as u32).wrapping_add(idx)
    }

    fn read_ea(&mut self, ea: Ea, size: Size, bus: &mut impl Bus) -> u32 {
        match ea {
            Ea::DataReg(i) => self.regs.d[i] & size.mask(),
            Ea::AddrReg(i) => self.regs.a[i] & size.mask(),
            Ea::Mem(addr) => self.read_mem(addr, size, bus),
            Ea::Imm(v) => v & size.mask(),
        }
    }

    fn write_ea(&mut self, ea: Ea, size: Size, val: u32, bus: &mut impl Bus) {
        match ea {
            Ea::DataReg(i) => {
                self.regs.d[i] = (self.regs.d[i] & !size.mask()) | (val & size.mask());
            }
            Ea::AddrReg(i) => {
                // Address-register writes always extend to the full 32 bits.
                self.regs.a[i] = match size {
                    Size::Word => val as u16 as i16 as i32 as u32,
                    _ => val,
                };
            }
            Ea::Mem(addr) => self.write_mem(addr, size, val, bus),
            Ea::Imm(_) => {} // not a valid write target
        }
    }

    // ---- flag helpers -------------------------------------------------

    fn set_logic_flags(&mut self, val: u32, size: Size) {
        self.regs.sr.n = val & size.msb() != 0;
        self.regs.sr.z = val & size.mask() == 0;
        self.regs.sr.v = false;
        self.regs.sr.c = false;
    }

    /// `dst + src`, set NZVCX. Returns the size-masked result.
    // The carry/overflow expressions are the textbook sign-bit derivations;
    // clippy's "minimal" rewrite obscures that, so keep the explicit form.
    #[allow(clippy::nonminimal_bool)]
    fn add_flags(&mut self, src: u32, dst: u32, size: Size) -> u32 {
        let (mask, msb) = (size.mask(), size.msb());
        let (s, d) = (src & mask, dst & mask);
        let res = s.wrapping_add(d) & mask;
        let (sm, dm, rm) = (s & msb != 0, d & msb != 0, res & msb != 0);
        self.regs.sr.c = (sm && dm) || (!rm && dm) || (sm && !rm);
        self.regs.sr.x = self.regs.sr.c;
        self.regs.sr.v = (sm && dm && !rm) || (!sm && !dm && rm);
        self.regs.sr.n = rm;
        self.regs.sr.z = res == 0;
        res
    }

    /// `dst - src`, set NZVC (and X when `affect_x`). Returns the result.
    #[allow(clippy::nonminimal_bool)]
    fn sub_flags(&mut self, src: u32, dst: u32, size: Size, affect_x: bool) -> u32 {
        let (mask, msb) = (size.mask(), size.msb());
        let (s, d) = (src & mask, dst & mask);
        let res = d.wrapping_sub(s) & mask;
        let (sm, dm, rm) = (s & msb != 0, d & msb != 0, res & msb != 0);
        let borrow = (sm && !dm) || (rm && !dm) || (sm && rm);
        self.regs.sr.c = borrow;
        if affect_x {
            self.regs.sr.x = borrow;
        }
        self.regs.sr.v = (!sm && dm && !rm) || (sm && !dm && rm);
        self.regs.sr.n = rm;
        self.regs.sr.z = res == 0;
        res
    }

    fn cond(&self, c: Cond) -> bool {
        let sr = &self.regs.sr;
        c.test(sr.c, sr.v, sr.z, sr.n)
    }

    /// `dst + src + X` with the multi-precision rules: NZVCX, but Z is sticky
    /// (only ever cleared, so a multi-word zero stays Z).
    fn addx(&mut self, src: u32, dst: u32, size: Size) -> u32 {
        let (mask, msb) = (size.mask(), size.msb());
        let (s, d, x) = (src & mask, dst & mask, self.regs.sr.x as u64);
        let full = s as u64 + d as u64 + x;
        let res = (full as u32) & mask;
        let (sm, dm, rm) = (s & msb != 0, d & msb != 0, res & msb != 0);
        self.regs.sr.c = full > mask as u64;
        self.regs.sr.x = self.regs.sr.c;
        self.regs.sr.v = (sm && dm && !rm) || (!sm && !dm && rm);
        self.regs.sr.n = rm;
        if res != 0 {
            self.regs.sr.z = false;
        }
        res
    }

    /// `dst - src - X`, NZVCX with the same sticky-Z rule as [`addx`].
    fn subx(&mut self, src: u32, dst: u32, size: Size) -> u32 {
        let (mask, msb) = (size.mask(), size.msb());
        let (s, d, x) = (src & mask, dst & mask, self.regs.sr.x as i64);
        let diff = d as i64 - s as i64 - x;
        let res = (diff as u32) & mask;
        let (sm, dm, rm) = (s & msb != 0, d & msb != 0, res & msb != 0);
        self.regs.sr.c = diff < 0;
        self.regs.sr.x = diff < 0;
        self.regs.sr.v = (!sm && dm && !rm) || (sm && !dm && rm);
        self.regs.sr.n = rm;
        if res != 0 {
            self.regs.sr.z = false;
        }
        res
    }

    /// Packed-BCD `dst + src + X` (byte), setting C/X and sticky Z.
    fn bcd_add(&mut self, src: u32, dst: u32) -> u32 {
        let x = self.regs.sr.x as u32;
        let mut lo = (src & 0xF) + (dst & 0xF) + x;
        let mut hi = ((src >> 4) & 0xF) + ((dst >> 4) & 0xF);
        if lo > 9 {
            lo -= 10;
            hi += 1;
        }
        let carry = hi > 9;
        if carry {
            hi -= 10;
        }
        let res = ((hi << 4) | lo) & 0xFF;
        self.regs.sr.c = carry;
        self.regs.sr.x = carry;
        if res != 0 {
            self.regs.sr.z = false;
        }
        self.regs.sr.n = res & 0x80 != 0;
        res
    }

    /// Packed-BCD `dst - src - X` (byte), setting C/X (borrow) and sticky Z.
    fn bcd_sub(&mut self, src: u32, dst: u32) -> u32 {
        let x = self.regs.sr.x as i32;
        let mut lo = (dst & 0xF) as i32 - (src & 0xF) as i32 - x;
        let mut hi = ((dst >> 4) & 0xF) as i32 - ((src >> 4) & 0xF) as i32;
        if lo < 0 {
            lo += 10;
            hi -= 1;
        }
        let borrow = hi < 0;
        if borrow {
            hi += 10;
        }
        let res = (((hi << 4) | lo) as u32) & 0xFF;
        self.regs.sr.c = borrow;
        self.regs.sr.x = borrow;
        if res != 0 {
            self.regs.sr.z = false;
        }
        self.regs.sr.n = res & 0x80 != 0;
        res
    }

    /// BTST/BCHG/BCLR/BSET (`kind` 0..3) of bit `bitnum`. Dn targets are
    /// 32-bit (bit mod 32); memory targets are byte (bit mod 8). Z reflects
    /// the *pre-modification* bit; BTST never writes back.
    fn op_bit(&mut self, op: u16, kind: u16, bitnum: u32, bus: &mut impl Bus) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        if mode == 0 {
            let r = reg as usize;
            let mask = 1u32 << (bitnum & 31);
            self.regs.sr.z = self.regs.d[r] & mask == 0;
            match kind {
                1 => self.regs.d[r] ^= mask,
                2 => self.regs.d[r] &= !mask,
                3 => self.regs.d[r] |= mask,
                _ => {}
            }
        } else {
            let mask = 1u32 << (bitnum & 7);
            let ea = self.resolve_ea(mode, reg, Size::Byte, bus);
            let v = self.read_ea(ea, Size::Byte, bus);
            self.regs.sr.z = v & mask == 0;
            let nv = match kind {
                1 => v ^ mask,
                2 => v & !mask,
                3 => v | mask,
                _ => return,
            };
            self.write_ea(ea, Size::Byte, nv, bus);
        }
    }

    fn push32(&mut self, val: u32, bus: &mut impl Bus) {
        self.regs.a[7] = self.regs.a[7].wrapping_sub(4);
        self.write_mem(self.regs.a[7], Size::Long, val, bus);
    }

    fn pop32(&mut self, bus: &mut impl Bus) -> u32 {
        let v = self.read_mem(self.regs.a[7], Size::Long, bus);
        self.regs.a[7] = self.regs.a[7].wrapping_add(4);
        v
    }

    fn push16(&mut self, val: u16, bus: &mut impl Bus) {
        self.regs.a[7] = self.regs.a[7].wrapping_sub(2);
        self.write_mem(self.regs.a[7], Size::Word, val as u32, bus);
    }

    fn pop16(&mut self, bus: &mut impl Bus) -> u16 {
        let v = self.read_mem(self.regs.a[7], Size::Word, bus) as u16;
        self.regs.a[7] = self.regs.a[7].wrapping_add(2);
        v
    }

    // ---- the main step ------------------------------------------------

    /// Execute one instruction; returns the cycles it took.
    pub fn step(&mut self, bus: &mut impl Bus) -> u32 {
        let start = self.cycles;

        // Interrupts are sampled at the instruction boundary.
        if self.service_interrupt(bus) {
            return (self.cycles - start) as u32;
        }
        // STOP parks the CPU until an interrupt arrives.
        if self.stopped {
            self.cycles += 4;
            return (self.cycles - start) as u32;
        }

        let instr_pc = self.regs.pc;
        let op = self.fetch16(bus);
        match op >> 12 {
            0x0 => self.op_immediate(op, bus),
            0x1..=0x3 => self.op_move(op, bus),
            0x4 => self.op_4(op, instr_pc, bus),
            0x5 => self.op_5(op, bus),
            0x6 => self.op_branch(op, bus),
            0x7 => self.op_moveq(op),
            0x8 => self.op_or_group(op, bus),
            0x9 => self.op_addsub(op, false, bus),
            0xB => self.op_cmp_eor(op, bus),
            0xC => self.op_and_group(op, bus),
            0xD => self.op_addsub(op, true, bus),
            0xE => self.op_shift(op, bus),
            // Line-A (0xA) and line-F (0xF) emulator traps push the faulting
            // instruction's address and vector through 10 / 11.
            0xA => self.fault(vector::LINE_A, instr_pc, bus),
            _ => self.fault(vector::LINE_F, instr_pc, bus),
        }
        (self.cycles - start) as u32
    }

    /// Take a fault whose stacked PC must point at the *faulting* instruction
    /// (illegal / line-A / line-F), not the following one.
    fn fault(&mut self, vector: u32, instr_pc: u32, bus: &mut impl Bus) {
        self.regs.pc = instr_pc;
        self.take_exception(vector, bus);
    }

    /// True if in supervisor mode; otherwise take the privilege-violation
    /// fault (vector 8) and return false.
    fn require_supervisor(&mut self, instr_pc: u32, bus: &mut impl Bus) -> bool {
        if self.regs.sr.supervisor {
            true
        } else {
            self.fault(vector::PRIVILEGE, instr_pc, bus);
            false
        }
    }

    // ---- instruction groups -------------------------------------------

    /// MOVE.B/W/L and MOVEA (dest mode 1).
    fn op_move(&mut self, op: u16, bus: &mut impl Bus) {
        let size = Size::from_move_bits(op >> 12).expect("MOVE size");
        let src_mode = (op >> 3) & 7;
        let src_reg = op & 7;
        let src_ea = self.resolve_ea(src_mode, src_reg, size, bus);
        let val = self.read_ea(src_ea, size, bus);

        let dst_reg = (op >> 9) & 7;
        let dst_mode = (op >> 6) & 7;
        if dst_mode == 1 {
            // MOVEA: sign-extend word to long, no flag change.
            let ext = match size {
                Size::Word => val as u16 as i16 as i32 as u32,
                _ => val,
            };
            self.regs.a[dst_reg as usize] = ext;
            return;
        }
        let dst_ea = self.resolve_ea(dst_mode, dst_reg, size, bus);
        self.write_ea(dst_ea, size, val, bus);
        self.set_logic_flags(val, size);
    }

    /// The 0x4 group: NOP/RTS/JMP/JSR/LEA/CLR/TST.
    fn op_4(&mut self, op: u16, instr_pc: u32, bus: &mut impl Bus) {
        match op {
            0x4E71 => return, // NOP
            0x4E75 => {
                // RTS
                self.regs.pc = self.pop32(bus);
                return;
            }
            0x4E73 => {
                // RTE (privileged): restore SR then PC.
                if !self.require_supervisor(instr_pc, bus) {
                    return;
                }
                let sr = self.pop16(bus);
                self.regs.pc = self.pop32(bus);
                self.write_sr(sr);
                return;
            }
            0x4E77 => {
                // RTR: restore CCR (low byte) then PC; system byte unchanged.
                let ccr = self.pop16(bus);
                self.regs.sr.set_ccr((ccr & 0x1F) as u8);
                self.regs.pc = self.pop32(bus);
                return;
            }
            0x4E72 => {
                // STOP #imm (privileged): load SR, park until an interrupt.
                let imm = self.fetch16(bus);
                if !self.require_supervisor(instr_pc, bus) {
                    return;
                }
                self.write_sr(imm);
                self.stopped = true;
                return;
            }
            0x4E70 => {
                // RESET (privileged): asserts the peripheral reset line — a
                // no-op for the core itself.
                self.require_supervisor(instr_pc, bus);
                return;
            }
            0x4E76 => {
                // TRAPV: trap on overflow.
                if self.regs.sr.v {
                    self.take_exception(vector::TRAPV, bus);
                }
                return;
            }
            0x4AFC => {
                // ILLEGAL (the architecturally-reserved illegal opcode).
                self.fault(vector::ILLEGAL, instr_pc, bus);
                return;
            }
            _ => {}
        }

        // TRAP #n (0x4E40..0x4E4F): software trap, vectors 32..47.
        if op & 0xFFF0 == 0x4E40 {
            self.take_exception(vector::TRAP_BASE + (op & 0xF) as u32, bus);
            return;
        }

        let mode = (op >> 3) & 7;
        let reg = op & 7;

        // CHK <ea>,Dn (opmode 110): trap 6 if Dn.W is < 0 or > the bound.
        if (op >> 6) & 7 == 0b110 {
            let dn = ((op >> 9) & 7) as usize;
            let ea = self.resolve_ea(mode, reg, Size::Word, bus);
            let bound = self.read_ea(ea, Size::Word, bus) as i16 as i32;
            let val = self.regs.d[dn] as u16 as i16 as i32;
            if val < 0 {
                self.regs.sr.n = true;
                self.take_exception(vector::CHK, bus);
            } else if val > bound {
                self.regs.sr.n = false;
                self.take_exception(vector::CHK, bus);
            }
            return;
        }

        // SWAP Dn (0x4840|reg): exchange the register halves.
        if op & 0xFFF8 == 0x4840 {
            let r = (op & 7) as usize;
            let v = self.regs.d[r].rotate_left(16);
            self.regs.d[r] = v;
            self.set_logic_flags(v, Size::Long);
            return;
        }
        // EXT.W (0x4880|reg): sign-extend byte→word.
        if op & 0xFFF8 == 0x4880 {
            let r = (op & 7) as usize;
            let v = (self.regs.d[r] as u8 as i8 as i16 as u16) as u32;
            self.regs.d[r] = (self.regs.d[r] & 0xFFFF_0000) | v;
            self.set_logic_flags(v, Size::Word);
            return;
        }
        // EXT.L (0x48C0|reg): sign-extend word→long.
        if op & 0xFFF8 == 0x48C0 {
            let r = (op & 7) as usize;
            let v = self.regs.d[r] as u16 as i16 as i32 as u32;
            self.regs.d[r] = v;
            self.set_logic_flags(v, Size::Long);
            return;
        }
        // LINK An,#disp (0x4E50|An): push An, frame-point it, grow the stack.
        if op & 0xFFF8 == 0x4E50 {
            let an = (op & 7) as usize;
            let disp = self.fetch16(bus) as i16 as i32;
            self.push32(self.regs.a[an], bus);
            self.regs.a[an] = self.regs.a[7];
            self.regs.a[7] = self.regs.a[7].wrapping_add(disp as u32);
            return;
        }
        // UNLK An (0x4E58|An): collapse the frame.
        if op & 0xFFF8 == 0x4E58 {
            let an = (op & 7) as usize;
            self.regs.a[7] = self.regs.a[an];
            self.regs.a[an] = self.pop32(bus);
            return;
        }
        // MOVEM (0100 1d00 1s mmmrrr): register-list load/store.
        if op & 0xFB80 == 0x4880 {
            self.op_movem(op, bus);
            return;
        }
        // NBCD <ea> (0x4800|ea): 0 - <ea> - X, packed BCD.
        if op & 0xFFC0 == 0x4800 {
            let ea = self.resolve_ea(mode, reg, Size::Byte, bus);
            let d = self.read_ea(ea, Size::Byte, bus);
            let r = self.bcd_sub(d, 0);
            self.write_ea(ea, Size::Byte, r, bus);
            return;
        }
        // TAS <ea> (0x4AC0|ea): test byte, set N/Z, then set bit 7.
        if op & 0xFFC0 == 0x4AC0 {
            let ea = self.resolve_ea(mode, reg, Size::Byte, bus);
            let v = self.read_ea(ea, Size::Byte, bus);
            self.set_logic_flags(v, Size::Byte);
            self.write_ea(ea, Size::Byte, v | 0x80, bus);
            return;
        }
        // MOVE from SR (0x40C0|ea): store the 16-bit SR to the EA.
        if op & 0xFFC0 == 0x40C0 {
            let ea = self.resolve_ea(mode, reg, Size::Word, bus);
            let v = self.regs.sr.to_u16() as u32;
            self.write_ea(ea, Size::Word, v, bus);
            return;
        }
        // MOVE to CCR (0x44C0|ea): low byte of the word source → CCR.
        if op & 0xFFC0 == 0x44C0 {
            let ea = self.resolve_ea(mode, reg, Size::Word, bus);
            let v = self.read_ea(ea, Size::Word, bus);
            self.regs.sr.set_ccr((v & 0x1F) as u8);
            return;
        }
        // MOVE to SR (0x46C0|ea): word source → SR (privileged).
        if op & 0xFFC0 == 0x46C0 {
            if !self.require_supervisor(instr_pc, bus) {
                return;
            }
            let ea = self.resolve_ea(mode, reg, Size::Word, bus);
            let v = self.read_ea(ea, Size::Word, bus);
            self.write_sr(v as u16);
            return;
        }

        // LEA An,<ea>: 0100 AAA 111 mmmrrr
        if op & 0x01C0 == 0x01C0 && (op & 0xF000) == 0x4000 && (op >> 6) & 7 == 7 {
            let an = (op >> 9) & 7;
            if let Ea::Mem(addr) = self.resolve_ea(mode, reg, Size::Long, bus) {
                self.regs.a[an as usize] = addr;
            }
            return;
        }

        match (op >> 6) & 0x3F {
            // JMP <ea> = 0100 1110 11 mmmrrr ; JSR = 0100 1110 10 mmmrrr
            0b111011 => {
                if let Ea::Mem(addr) = self.resolve_ea(mode, reg, Size::Long, bus) {
                    self.regs.pc = addr;
                }
            }
            0b111010 => {
                if let Ea::Mem(addr) = self.resolve_ea(mode, reg, Size::Long, bus) {
                    let ret = self.regs.pc;
                    self.push32(ret, bus);
                    self.regs.pc = addr;
                }
            }
            _ => {
                // CLR / TST (0100 0010 ss = CLR ; 0100 1010 ss = TST)
                let class = (op >> 8) & 0xF;
                if let Some(size) = Size::from_op_bits(op >> 6) {
                    match class {
                        0x2 => {
                            // CLR
                            let ea = self.resolve_ea(mode, reg, size, bus);
                            // CLR still reads the EA on real hardware, but the
                            // observable effect is a zero write + flags.
                            self.write_ea(ea, size, 0, bus);
                            self.regs.sr.n = false;
                            self.regs.sr.z = true;
                            self.regs.sr.v = false;
                            self.regs.sr.c = false;
                        }
                        0xA => {
                            // TST
                            let ea = self.resolve_ea(mode, reg, size, bus);
                            let v = self.read_ea(ea, size, bus);
                            self.set_logic_flags(v, size);
                        }
                        0x4 => {
                            // NEG: 0 - dst
                            let ea = self.resolve_ea(mode, reg, size, bus);
                            let d = self.read_ea(ea, size, bus);
                            let r = self.sub_flags(d, 0, size, true);
                            self.write_ea(ea, size, r, bus);
                        }
                        0x6 => {
                            // NOT: ones-complement
                            let ea = self.resolve_ea(mode, reg, size, bus);
                            let r = (!self.read_ea(ea, size, bus)) & size.mask();
                            self.write_ea(ea, size, r, bus);
                            self.set_logic_flags(r, size);
                        }
                        0x0 => {
                            // NEGX: 0 - dst - X
                            let ea = self.resolve_ea(mode, reg, size, bus);
                            let d = self.read_ea(ea, size, bus);
                            let r = self.subx(d, 0, size);
                            self.write_ea(ea, size, r, bus);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Write the full 16-bit SR, banking A7 on an S-bit change and keeping
    /// all status fields consistent. Used by MOVE/ANDI/ORI/EORI to SR.
    fn write_sr(&mut self, nv: u16) {
        self.regs.set_supervisor(nv & 0x2000 != 0);
        self.regs.sr.set_ccr((nv & 0x1F) as u8);
        self.regs.sr.imask = ((nv >> 8) & 7) as u8;
        self.regs.sr.trace = nv & 0x8000 != 0;
    }

    /// The 0x5 group: ADDQ/SUBQ when the size field is byte/word/long, or
    /// Scc/DBcc when it is 0b11.
    fn op_5(&mut self, op: u16, bus: &mut impl Bus) {
        if (op >> 6) & 3 == 3 {
            // DBcc Dn,disp16 = 0101 cccc 11001 rrr ; otherwise Scc <ea>.
            let cond = Cond::from_bits((op >> 8) & 0xF);
            if op & 0x00F8 == 0x00C8 {
                let reg = (op & 7) as usize;
                let base = self.regs.pc;
                let disp = self.fetch16(bus) as i16 as i32;
                if self.cond(cond) {
                    return; // condition true → loop terminates, no decrement
                }
                let counter = (self.regs.d[reg] as u16).wrapping_sub(1);
                self.regs.d[reg] = (self.regs.d[reg] & 0xFFFF_0000) | counter as u32;
                if counter != 0xFFFF {
                    self.regs.pc = base.wrapping_add(disp as u32);
                }
            } else {
                // Scc <ea> — byte set to 0xFF if true, else 0x00.
                let mode = (op >> 3) & 7;
                let reg = op & 7;
                let ea = self.resolve_ea(mode, reg, Size::Byte, bus);
                let v = if self.cond(cond) { 0xFF } else { 0x00 };
                self.write_ea(ea, Size::Byte, v, bus);
            }
            return;
        }
        let size = Size::from_op_bits(op >> 6).expect("size checked above");
        let mut data = ((op >> 9) & 7) as u32;
        if data == 0 {
            data = 8;
        }
        let is_sub = op & 0x0100 != 0;
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let ea = self.resolve_ea(mode, reg, size, bus);

        // ADDQ/SUBQ on an address register acts on the full 32 bits and
        // sets no flags.
        if let Ea::AddrReg(i) = ea {
            let cur = self.regs.a[i];
            self.regs.a[i] = if is_sub {
                cur.wrapping_sub(data)
            } else {
                cur.wrapping_add(data)
            };
            return;
        }

        let dst = self.read_ea(ea, size, bus);
        let res = if is_sub {
            self.sub_flags(data, dst, size, true)
        } else {
            self.add_flags(data, dst, size)
        };
        self.write_ea(ea, size, res, bus);
    }

    /// BRA / BSR / Bcc (0110 cccc dddddddd).
    fn op_branch(&mut self, op: u16, bus: &mut impl Bus) {
        let cond_bits = (op >> 8) & 0xF;
        // Displacement base is the PC after the opcode word.
        let base = self.regs.pc;
        let disp = (op & 0xFF) as i8 as i32;
        let disp = if disp == 0 {
            self.fetch16(bus) as i16 as i32
        } else {
            disp
        };
        let target = base.wrapping_add(disp as u32);

        match cond_bits {
            0x0 => self.regs.pc = target, // BRA
            0x1 => {
                // BSR
                let ret = self.regs.pc;
                self.push32(ret, bus);
                self.regs.pc = target;
            }
            _ => {
                if self.cond(Cond::from_bits(cond_bits)) {
                    self.regs.pc = target;
                }
            }
        }
    }

    /// MOVEQ #data,Dn (0111 ddd 0 iiiiiiii).
    fn op_moveq(&mut self, op: u16) {
        let reg = ((op >> 9) & 7) as usize;
        let val = (op & 0xFF) as i8 as i32 as u32;
        self.regs.d[reg] = val;
        self.set_logic_flags(val, Size::Long);
    }

    /// ADD/SUB and ADDA/SUBA. `is_add` picks the group (0x9=SUB, 0xD=ADD).
    fn op_addsub(&mut self, op: u16, is_add: bool, bus: &mut impl Bus) {
        let reg = ((op >> 9) & 7) as usize;
        let mode = (op >> 3) & 7;
        let ea_reg = op & 7;
        let opmode = (op >> 6) & 7;

        // ADDX/SUBX: bit 8 set with the addressing field (bits 5..4) zero.
        if op & 0x0130 == 0x0100 {
            self.op_addx_subx(op, is_add, bus);
            return;
        }

        // ADDA/SUBA: opmode 011 (word) or 111 (long) — target is An.
        if opmode == 0b011 || opmode == 0b111 {
            let size = if opmode == 0b011 {
                Size::Word
            } else {
                Size::Long
            };
            let ea = self.resolve_ea(mode, ea_reg, size, bus);
            let raw = self.read_ea(ea, size, bus);
            // Word source is sign-extended to 32 bits; no flags affected.
            let src = size.sign_extend(raw) as u32;
            let cur = self.regs.a[reg];
            self.regs.a[reg] = if is_add {
                cur.wrapping_add(src)
            } else {
                cur.wrapping_sub(src)
            };
            return;
        }

        let Some(size) = Size::from_op_bits(opmode) else {
            return;
        };
        let to_ea = opmode & 0b100 != 0; // direction: 1 → <ea> = <ea> op Dn
        let ea = self.resolve_ea(mode, ea_reg, size, bus);

        if to_ea {
            let dst = self.read_ea(ea, size, bus);
            let src = self.regs.d[reg];
            let res = if is_add {
                self.add_flags(src, dst, size)
            } else {
                self.sub_flags(src, dst, size, true)
            };
            self.write_ea(ea, size, res, bus);
        } else {
            let src = self.read_ea(ea, size, bus);
            let dst = self.regs.d[reg];
            let res = if is_add {
                self.add_flags(src, dst, size)
            } else {
                self.sub_flags(src, dst, size, true)
            };
            self.regs.d[reg] = (dst & !size.mask()) | (res & size.mask());
        }
    }

    /// ADDX/SUBX: `Dx op Dy` (register) or `-(Ax) op -(Ay)` (memory), with
    /// extend and multi-precision sticky Z.
    fn op_addx_subx(&mut self, op: u16, is_add: bool, bus: &mut impl Bus) {
        let size = Size::from_op_bits(op >> 6).unwrap_or(Size::Byte);
        let rx = ((op >> 9) & 7) as usize;
        let ry = (op & 7) as usize;
        let xfn = |cpu: &mut Self, s, d| {
            if is_add {
                cpu.addx(s, d, size)
            } else {
                cpu.subx(s, d, size)
            }
        };
        if op & 0x0008 != 0 {
            // -(Ay), -(Ax): predecrement the source then the destination.
            let s = {
                let ea = self.resolve_ea(4, ry as u16, size, bus);
                self.read_ea(ea, size, bus)
            };
            let ea = self.resolve_ea(4, rx as u16, size, bus);
            let d = self.read_ea(ea, size, bus);
            let r = xfn(self, s, d);
            self.write_ea(ea, size, r, bus);
        } else {
            let r = xfn(self, self.regs.d[ry], self.regs.d[rx]);
            self.regs.d[rx] = (self.regs.d[rx] & !size.mask()) | (r & size.mask());
        }
    }

    /// MOVEM — move a register list to/from memory. Word transfers sign-extend
    /// into the 32-bit registers on load. The `-(An)` store walks the list in
    /// reverse (A7..D0); every other mode walks D0..A7 with ascending address.
    fn op_movem(&mut self, op: u16, bus: &mut impl Bus) {
        let to_mem = op & 0x0400 == 0; // bit 10: 0 = registers→memory
        let size = if op & 0x40 != 0 {
            Size::Long
        } else {
            Size::Word
        };
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let mask = self.fetch16(bus);
        let bytes = size.bytes();

        if to_mem && mode == 4 {
            // Predecrement store: bit i selects A7..D0 as i goes 0..15.
            let mut addr = self.regs.a[reg as usize];
            for i in 0..16 {
                if mask & (1 << i) == 0 {
                    continue;
                }
                let val = if i < 8 {
                    self.regs.a[7 - i]
                } else {
                    self.regs.d[15 - i]
                };
                addr = addr.wrapping_sub(bytes);
                self.write_mem(addr, size, val, bus);
            }
            self.regs.a[reg as usize] = addr;
            return;
        }

        if !to_mem && mode == 3 {
            // Postincrement load.
            let mut addr = self.regs.a[reg as usize];
            for i in 0..16 {
                if mask & (1 << i) == 0 {
                    continue;
                }
                let v = self.movem_load(addr, size, bus);
                self.set_movem_reg(i, v);
                addr = addr.wrapping_add(bytes);
            }
            self.regs.a[reg as usize] = addr;
            return;
        }

        // Control-addressing modes: ascending address, list order D0..A7.
        let Ea::Mem(mut addr) = self.resolve_ea(mode, reg, size, bus) else {
            return;
        };
        for i in 0..16 {
            if mask & (1 << i) == 0 {
                continue;
            }
            if to_mem {
                let val = self.movem_reg(i);
                self.write_mem(addr, size, val, bus);
            } else {
                let v = self.movem_load(addr, size, bus);
                self.set_movem_reg(i, v);
            }
            addr = addr.wrapping_add(bytes);
        }
    }

    /// List-order register read for MOVEM (bit i: 0..7 = D0..D7, 8..15 = A0..A7).
    fn movem_reg(&self, i: usize) -> u32 {
        if i < 8 {
            self.regs.d[i]
        } else {
            self.regs.a[i - 8]
        }
    }
    /// List-order register write, sign-extending word loads to 32 bits.
    fn set_movem_reg(&mut self, i: usize, v: u32) {
        if i < 8 {
            self.regs.d[i] = v;
        } else {
            self.regs.a[i - 8] = v;
        }
    }
    fn movem_load(&mut self, addr: u32, size: Size, bus: &mut impl Bus) -> u32 {
        let v = self.read_mem(addr, size, bus);
        if size == Size::Word {
            v as u16 as i16 as i32 as u32
        } else {
            v
        }
    }

    /// The immediate group (0x0): ORI/ANDI/SUBI/ADDI/EORI/CMPI, the bit ops
    /// (static + dynamic), and the ORI/ANDI/EORI-to-CCR/SR special forms.
    fn op_immediate(&mut self, op: u16, bus: &mut impl Bus) {
        // Static bit ops (0000 1000 kk mmmrrr): bit number in the next word.
        if op & 0x0F00 == 0x0800 {
            let kind = (op >> 6) & 3;
            let bitnum = self.fetch16(bus) as u32;
            self.op_bit(op, kind, bitnum, bus);
            return;
        }
        // Dynamic bit ops (bit 8 set): bit number in Dn (bits 11..9).
        if op & 0x0100 != 0 {
            if (op >> 3) & 7 == 1 {
                self.op_movep(op, bus);
                return;
            }
            let kind = (op >> 6) & 3;
            let bitnum = self.regs.d[((op >> 9) & 7) as usize];
            self.op_bit(op, kind, bitnum, bus);
            return;
        }
        let ttt = (op >> 9) & 7;
        let Some(size) = Size::from_op_bits(op >> 6) else {
            return;
        };
        let imm = match size {
            Size::Byte => (self.fetch16(bus) & 0xFF) as u32,
            Size::Word => self.fetch16(bus) as u32,
            Size::Long => self.fetch32(bus),
        };
        let mode = (op >> 3) & 7;
        let ea_reg = op & 7;

        // ORI/ANDI/EORI #imm,CCR (byte) or #imm,SR (word): EA slot is the
        // immediate-mode encoding (mode 7, reg 4).
        if mode == 7 && ea_reg == 4 {
            match size {
                Size::Byte => {
                    let cur = self.regs.sr.ccr();
                    let nv = match ttt {
                        0 => cur | imm as u8,
                        1 => cur & imm as u8,
                        5 => cur ^ imm as u8,
                        _ => return,
                    };
                    self.regs.sr.set_ccr(nv);
                }
                Size::Word => {
                    let cur = self.regs.sr.to_u16();
                    let nv = match ttt {
                        0 => cur | imm as u16,
                        1 => cur & imm as u16,
                        5 => cur ^ imm as u16,
                        _ => return,
                    };
                    self.write_sr(nv);
                }
                Size::Long => {}
            }
            return;
        }

        let ea = self.resolve_ea(mode, ea_reg, size, bus);
        match ttt {
            0 => {
                let r = (self.read_ea(ea, size, bus) | imm) & size.mask();
                self.write_ea(ea, size, r, bus);
                self.set_logic_flags(r, size);
            }
            1 => {
                let r = (self.read_ea(ea, size, bus) & imm) & size.mask();
                self.write_ea(ea, size, r, bus);
                self.set_logic_flags(r, size);
            }
            2 => {
                let d = self.read_ea(ea, size, bus);
                let r = self.sub_flags(imm, d, size, true);
                self.write_ea(ea, size, r, bus);
            }
            3 => {
                let d = self.read_ea(ea, size, bus);
                let r = self.add_flags(imm, d, size);
                self.write_ea(ea, size, r, bus);
            }
            5 => {
                let r = (self.read_ea(ea, size, bus) ^ imm) & size.mask();
                self.write_ea(ea, size, r, bus);
                self.set_logic_flags(r, size);
            }
            6 => {
                let d = self.read_ea(ea, size, bus);
                self.sub_flags(imm, d, size, false); // CMPI — no write
            }
            _ => {}
        }
    }

    /// MOVEP.W/L — move between a data register and *alternating* bytes of
    /// memory at `(d16, Ay)`, high byte first (a peripheral-access idiom for
    /// byte-wide devices on a 16-bit bus). Opmode (bits 8..6): 100 = W mem→Dx,
    /// 101 = L mem→Dx, 110 = W Dx→mem, 111 = L Dx→mem.
    fn op_movep(&mut self, op: u16, bus: &mut impl Bus) {
        let dreg = ((op >> 9) & 7) as usize;
        let areg = (op & 7) as usize;
        let opmode = (op >> 6) & 7;
        let disp = self.fetch16(bus) as i16 as i32;
        let mut addr = self.regs.a[areg].wrapping_add(disp as u32);
        let long = opmode & 1 != 0; // 101 / 111
        let to_mem = opmode & 0b010 != 0; // 110 / 111
        let nbytes = if long { 4 } else { 2 };
        if to_mem {
            let val = self.regs.d[dreg];
            for i in 0..nbytes {
                let shift = (nbytes - 1 - i) * 8;
                self.cycles += bus.write8(addr, (val >> shift) as u8, AccessKind::Data) as u64;
                addr = addr.wrapping_add(2);
            }
        } else {
            let mut val = 0u32;
            for i in 0..nbytes {
                let (b, s) = bus.read8(addr, AccessKind::Data);
                self.cycles += s as u64;
                val |= (b as u32) << ((nbytes - 1 - i) * 8);
                addr = addr.wrapping_add(2);
            }
            if long {
                self.regs.d[dreg] = val;
            } else {
                self.regs.d[dreg] = (self.regs.d[dreg] & 0xFFFF_0000) | (val & 0xFFFF);
            }
        }
        self.cycles += if long { 24 } else { 16 };
    }

    /// OR/AND register↔EA (`f` is the bitwise op). The opmode's high bit
    /// selects the direction. (The MUL/DIV opmodes 011/111 are dispatched by
    /// the 0xC / 0x8 group handlers, not here.)
    fn op_logic(&mut self, op: u16, f: fn(u32, u32) -> u32, bus: &mut impl Bus) {
        let reg = ((op >> 9) & 7) as usize;
        let opmode = (op >> 6) & 7;
        let mode = (op >> 3) & 7;
        let ea_reg = op & 7;
        let Some(size) = Size::from_op_bits(opmode) else {
            return; // 011/111 → MULU/MULS or DIVU/DIVS
        };
        let to_ea = opmode & 0b100 != 0;
        let ea = self.resolve_ea(mode, ea_reg, size, bus);
        if to_ea {
            let dst = self.read_ea(ea, size, bus);
            let r = f(self.regs.d[reg], dst) & size.mask();
            self.write_ea(ea, size, r, bus);
            self.set_logic_flags(r, size);
        } else {
            let src = self.read_ea(ea, size, bus);
            let r = f(src, self.regs.d[reg]) & size.mask();
            self.regs.d[reg] = (self.regs.d[reg] & !size.mask()) | r;
            self.set_logic_flags(r, size);
        }
    }

    /// The 0xC group: MULU/MULS, ABCD, EXG, or AND.
    fn op_and_group(&mut self, op: u16, bus: &mut impl Bus) {
        let opmode = (op >> 6) & 7;
        // MULU.W (011) / MULS.W (111): Dn.W × <ea>.W → Dn.L.
        if opmode == 0b011 || opmode == 0b111 {
            self.op_mul(op, opmode == 0b111, bus);
            return;
        }
        // ABCD: opmode 100 (bit8 set) with the addressing field zero.
        if op & 0x01F0 == 0x0100 {
            self.op_bcd(op, true, bus);
            return;
        }
        match op & 0xF1F8 {
            0xC140 => {
                let (x, y) = (((op >> 9) & 7) as usize, (op & 7) as usize);
                self.regs.d.swap(x, y);
                return;
            }
            0xC148 => {
                let (x, y) = (((op >> 9) & 7) as usize, (op & 7) as usize);
                self.regs.a.swap(x, y);
                return;
            }
            0xC188 => {
                let (x, y) = (((op >> 9) & 7) as usize, (op & 7) as usize);
                core::mem::swap(&mut self.regs.d[x], &mut self.regs.a[y]);
                return;
            }
            _ => {}
        }
        self.op_logic(op, |a, b| a & b, bus);
    }

    /// The 0x8 group: DIVU/DIVS, SBCD, or OR.
    fn op_or_group(&mut self, op: u16, bus: &mut impl Bus) {
        let opmode = (op >> 6) & 7;
        if opmode == 0b011 || opmode == 0b111 {
            self.op_div(op, opmode == 0b111, bus);
            return;
        }
        if op & 0x01F0 == 0x0100 {
            self.op_bcd(op, false, bus);
            return;
        }
        self.op_logic(op, |a, b| a | b, bus);
    }

    /// MULU.W / MULS.W: `Dn.W × <ea>.W → Dn.L`. N from bit 31, Z, V=C=0.
    fn op_mul(&mut self, op: u16, signed: bool, bus: &mut impl Bus) {
        let reg = ((op >> 9) & 7) as usize;
        let ea = self.resolve_ea((op >> 3) & 7, op & 7, Size::Word, bus);
        let src = self.read_ea(ea, Size::Word, bus) & 0xFFFF;
        let dn = self.regs.d[reg] & 0xFFFF;
        let result = if signed {
            ((src as u16 as i16 as i32) * (dn as u16 as i16 as i32)) as u32
        } else {
            src * dn
        };
        self.regs.d[reg] = result;
        self.regs.sr.n = result & 0x8000_0000 != 0;
        self.regs.sr.z = result == 0;
        self.regs.sr.v = false;
        self.regs.sr.c = false;
    }

    /// DIVU.W / DIVS.W: `Dn.L / <ea>.W` → quotient in the low word, remainder
    /// in the high word. Overflow sets V and leaves Dn; divide-by-zero is a
    /// no-op here (the zero-divide trap arrives with the exception model).
    fn op_div(&mut self, op: u16, signed: bool, bus: &mut impl Bus) {
        let reg = ((op >> 9) & 7) as usize;
        let ea = self.resolve_ea((op >> 3) & 7, op & 7, Size::Word, bus);
        let divisor = self.read_ea(ea, Size::Word, bus) & 0xFFFF;
        if divisor == 0 {
            self.take_exception(vector::ZERO_DIVIDE, bus);
            return;
        }
        let dividend = self.regs.d[reg];
        self.regs.sr.c = false;
        if signed {
            let dvs = divisor as u16 as i16 as i32;
            let q = (dividend as i32) / dvs;
            let r = (dividend as i32) % dvs;
            if !(-32768..=32767).contains(&q) {
                self.regs.sr.v = true;
                return;
            }
            self.regs.d[reg] = ((r as u32) << 16) | (q as u16 as u32);
            self.regs.sr.n = (q as i16) < 0;
            self.regs.sr.z = q == 0;
        } else {
            let q = dividend / divisor;
            let r = dividend % divisor;
            if q > 0xFFFF {
                self.regs.sr.v = true;
                return;
            }
            self.regs.d[reg] = (r << 16) | (q & 0xFFFF);
            self.regs.sr.n = q & 0x8000 != 0;
            self.regs.sr.z = q == 0;
        }
        self.regs.sr.v = false;
    }

    /// ABCD / SBCD: packed-BCD add/subtract with extend, on a Dn pair or a
    /// `-(Ay),-(Ax)` byte pair.
    fn op_bcd(&mut self, op: u16, is_add: bool, bus: &mut impl Bus) {
        let rx = ((op >> 9) & 7) as usize;
        let ry = (op & 7) as usize;
        if op & 0x0008 != 0 {
            let s = {
                let ea = self.resolve_ea(4, ry as u16, Size::Byte, bus);
                self.read_ea(ea, Size::Byte, bus)
            };
            let ea = self.resolve_ea(4, rx as u16, Size::Byte, bus);
            let d = self.read_ea(ea, Size::Byte, bus);
            let r = if is_add {
                self.bcd_add(s, d)
            } else {
                self.bcd_sub(s, d)
            };
            self.write_ea(ea, Size::Byte, r, bus);
        } else {
            let (s, d) = (self.regs.d[ry] & 0xFF, self.regs.d[rx] & 0xFF);
            let r = if is_add {
                self.bcd_add(s, d)
            } else {
                self.bcd_sub(s, d)
            };
            self.regs.d[rx] = (self.regs.d[rx] & !0xFF) | (r & 0xFF);
        }
    }

    /// The 0xB group: CMP <ea>,Dn ; CMPA <ea>,An ; EOR Dn,<ea> ; CMPM.
    fn op_cmp_eor(&mut self, op: u16, bus: &mut impl Bus) {
        let reg = ((op >> 9) & 7) as usize;
        let opmode = (op >> 6) & 7;
        let mode = (op >> 3) & 7;
        let ea_reg = op & 7;

        if opmode == 0b011 || opmode == 0b111 {
            // CMPA — compares the full 32 bits, word source sign-extended.
            let size = if opmode == 0b011 {
                Size::Word
            } else {
                Size::Long
            };
            let ea = self.resolve_ea(mode, ea_reg, size, bus);
            let src = size.sign_extend(self.read_ea(ea, size, bus)) as u32;
            self.sub_flags(src, self.regs.a[reg], Size::Long, false);
            return;
        }

        let Some(size) = Size::from_op_bits(opmode) else {
            return;
        };
        if opmode & 0b100 != 0 {
            if mode == 1 {
                // CMPM (Ay)+,(Ax)+ → compares (Ax)+ - (Ay)+.
                let s = {
                    let ea = self.resolve_ea(3, ea_reg, size, bus);
                    self.read_ea(ea, size, bus)
                };
                let d = {
                    let ea = self.resolve_ea(3, reg as u16, size, bus);
                    self.read_ea(ea, size, bus)
                };
                self.sub_flags(s, d, size, false);
            } else {
                // EOR Dn,<ea>
                let ea = self.resolve_ea(mode, ea_reg, size, bus);
                let r = (self.read_ea(ea, size, bus) ^ self.regs.d[reg]) & size.mask();
                self.write_ea(ea, size, r, bus);
                self.set_logic_flags(r, size);
            }
        } else {
            // CMP <ea>,Dn
            let ea = self.resolve_ea(mode, ea_reg, size, bus);
            let src = self.read_ea(ea, size, bus);
            self.sub_flags(src, self.regs.d[reg], size, false);
        }
    }

    /// Shift/rotate group (0xE). The register-target forms (size byte/word/
    /// long, programmable count) are here; size field 11 selects the
    /// memory-operand single-bit form, handled by [`Self::op_shift_mem`].
    fn op_shift(&mut self, op: u16, bus: &mut impl Bus) {
        let Some(size) = Size::from_op_bits(op >> 6) else {
            self.op_shift_mem(op, bus);
            return;
        };
        let reg = (op & 7) as usize;
        let left = op & 0x0100 != 0;
        let kind = (op >> 3) & 3; // 0=ASx 1=LSx 2=ROXx 3=ROx
        let count = if op & 0x0020 != 0 {
            // register count, mod 64
            self.regs.d[((op >> 9) & 7) as usize] & 0x3F
        } else {
            // immediate count: 0 means 8
            let c = (op >> 9) & 7;
            if c == 0 { 8 } else { c as u32 }
        };
        let _ = bus;

        let mask = size.mask();
        let msb = size.msb();
        let mut val = self.regs.d[reg] & mask;
        let mut carry = false;
        let mut overflow = false;

        for _ in 0..count {
            match (kind, left) {
                (0, true) | (1, true) => {
                    // ASL / LSL
                    carry = val & msb != 0;
                    let next = (val << 1) & mask;
                    if kind == 0 && (next & msb != 0) != (val & msb != 0) {
                        overflow = true;
                    }
                    val = next;
                }
                (0, false) => {
                    // ASR — keep the sign bit
                    carry = val & 1 != 0;
                    let sign = val & msb;
                    val = (val >> 1) | sign;
                }
                (1, false) => {
                    // LSR
                    carry = val & 1 != 0;
                    val >>= 1;
                }
                (3, true) => {
                    // ROL
                    carry = val & msb != 0;
                    val = ((val << 1) | carry as u32) & mask;
                }
                (3, false) => {
                    // ROR
                    carry = val & 1 != 0;
                    val = (val >> 1) | (if carry { msb } else { 0 });
                }
                (2, true) => {
                    // ROXL through X
                    let xin = self.regs.sr.x as u32;
                    carry = val & msb != 0;
                    val = ((val << 1) | xin) & mask;
                    self.regs.sr.x = carry;
                }
                (2, false) => {
                    // ROXR through X
                    let xin = self.regs.sr.x as u32;
                    carry = val & 1 != 0;
                    val = (val >> 1) | (if xin != 0 { msb } else { 0 });
                    self.regs.sr.x = carry;
                }
                _ => {}
            }
        }

        self.regs.d[reg] = (self.regs.d[reg] & !mask) | (val & mask);
        self.regs.sr.n = val & msb != 0;
        self.regs.sr.z = val & mask == 0;
        self.regs.sr.v = if kind == 0 { overflow } else { false };
        // C is the last bit shifted out; a zero-count shift clears C (and for
        // ASx/LSx/ROx leaves X untouched — ROXx already updated X above).
        if count == 0 {
            self.regs.sr.c = false;
        } else {
            self.regs.sr.c = carry;
            if kind != 2 && kind != 3 {
                // ASx / LSx also load X with the last bit out.
                self.regs.sr.x = carry;
            }
        }
    }

    /// Memory shift/rotate by one bit (size field 11). Operates on a word at
    /// the EA; the kind is in bits 10..9 (0=ASx 1=LSx 2=ROXx 3=ROx) and bit 8
    /// selects the direction.
    fn op_shift_mem(&mut self, op: u16, bus: &mut impl Bus) {
        let kind = (op >> 9) & 3;
        let left = op & 0x0100 != 0;
        let mode = (op >> 3) & 7;
        let ea_reg = op & 7;
        let ea = self.resolve_ea(mode, ea_reg, Size::Word, bus);
        let mut val = self.read_ea(ea, Size::Word, bus) & 0xFFFF;
        let (mask, msb) = (0xFFFFu32, 0x8000u32);
        let mut overflow = false;
        let carry;
        match (kind, left) {
            (0, true) | (1, true) => {
                // ASL / LSL
                carry = val & msb != 0;
                let next = (val << 1) & mask;
                overflow = kind == 0 && (next & msb != 0) != (val & msb != 0);
                val = next;
            }
            (0, false) => {
                // ASR — preserve sign
                carry = val & 1 != 0;
                val = (val >> 1) | (val & msb);
            }
            (1, false) => {
                // LSR
                carry = val & 1 != 0;
                val >>= 1;
            }
            (3, true) => {
                // ROL
                carry = val & msb != 0;
                val = ((val << 1) | carry as u32) & mask;
            }
            (3, false) => {
                // ROR
                carry = val & 1 != 0;
                val = (val >> 1) | (if carry { msb } else { 0 });
            }
            (2, true) => {
                // ROXL through X
                let xin = self.regs.sr.x as u32;
                carry = val & msb != 0;
                val = ((val << 1) | xin) & mask;
                self.regs.sr.x = carry;
            }
            _ => {
                // ROXR through X
                let xin = self.regs.sr.x as u32;
                carry = val & 1 != 0;
                val = (val >> 1) | (if xin != 0 { msb } else { 0 });
                self.regs.sr.x = carry;
            }
        }
        self.write_ea(ea, Size::Word, val & mask, bus);
        self.regs.sr.n = val & msb != 0;
        self.regs.sr.z = val & mask == 0;
        self.regs.sr.v = if kind == 0 { overflow } else { false };
        self.regs.sr.c = carry;
        if kind != 2 && kind != 3 {
            self.regs.sr.x = carry;
        }
    }
}
