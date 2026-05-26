//! MC68EC000 interpreter.
//!
//! Decode-and-execute in one pass: the 68000's variable-length encoding
//! (opcode word + extension words, some consumed while resolving an
//! effective address) makes that more natural than a pre-decoded table.
//! `step` fetches the opcode word, dispatches on its top nibble, resolves
//! operands (reading any extension words via [`Cpu::fetch16`]/[`fetch32`]),
//! executes, and returns the cycles consumed.
//!
//! **Scope (increment 1):** the data-movement and control-flow core —
//! MOVE/MOVEA/MOVEQ, ADD/SUB/ADDA/SUBA/ADDQ/SUBQ, CLR/TST, LEA, NOP, the
//! branch group (BRA/BSR/Bcc), RTS, and JMP/JSR. The logical/immediate/
//! shift/multiply/BCD groups, DBcc/Scc, and the full exception model are
//! later increments.
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
pub struct Cpu {
    pub regs: Registers,
    /// Total clock cycles consumed since construction.
    pub cycles: u64,
    /// Set by STOP (and on a halting double fault); the scheduler skips a
    /// stopped core until an interrupt wakes it.
    pub stopped: bool,
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

    fn push32(&mut self, val: u32, bus: &mut impl Bus) {
        self.regs.a[7] = self.regs.a[7].wrapping_sub(4);
        self.write_mem(self.regs.a[7], Size::Long, val, bus);
    }

    fn pop32(&mut self, bus: &mut impl Bus) -> u32 {
        let v = self.read_mem(self.regs.a[7], Size::Long, bus);
        self.regs.a[7] = self.regs.a[7].wrapping_add(4);
        v
    }

    // ---- the main step ------------------------------------------------

    /// Execute one instruction; returns the cycles it took.
    pub fn step(&mut self, bus: &mut impl Bus) -> u32 {
        let start = self.cycles;
        let op = self.fetch16(bus);
        match op >> 12 {
            0x0 => self.op_immediate(op, bus),
            0x1..=0x3 => self.op_move(op, bus),
            0x4 => self.op_4(op, bus),
            0x5 => self.op_5(op, bus),
            0x6 => self.op_branch(op, bus),
            0x7 => self.op_moveq(op),
            0x8 => self.op_logic(op, |a, b| a | b, bus),
            0x9 => self.op_addsub(op, false, bus),
            0xB => self.op_cmp_eor(op, bus),
            0xC => self.op_and_group(op, bus),
            0xD => self.op_addsub(op, true, bus),
            0xE => self.op_shift(op, bus),
            _ => { /* still unimplemented (multiply/BCD/bit ops) — NOP for now */ }
        }
        (self.cycles - start) as u32
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
    fn op_4(&mut self, op: u16, bus: &mut impl Bus) {
        match op {
            0x4E71 => return, // NOP
            0x4E75 => {
                // RTS
                self.regs.pc = self.pop32(bus);
                return;
            }
            _ => {}
        }

        let mode = (op >> 3) & 7;
        let reg = op & 7;

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
        // MOVE to SR (0x46C0|ea): word source → SR (privileged; the
        // privilege trap arrives with the exception model).
        if op & 0xFFC0 == 0x46C0 {
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

    /// The immediate group (0x0): ORI/ANDI/SUBI/ADDI/EORI/CMPI, plus the
    /// ORI/ANDI/EORI-to-CCR/SR special forms. Bit-manipulation and MOVEP
    /// (bit 8 set) are a later increment.
    fn op_immediate(&mut self, op: u16, bus: &mut impl Bus) {
        if op & 0x0100 != 0 {
            return; // BTST/BCHG/BCLR/BSET/MOVEP — not yet
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

    /// OR/AND register↔EA (`f` is the bitwise op). The opmode's high bit
    /// selects the direction; opmode 3/7 (MUL/DIV) is not yet implemented.
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

    /// The 0xC group: AND, or EXG when the encoding matches. (MULU/MULS and
    /// ABCD are later increments.)
    fn op_and_group(&mut self, op: u16, bus: &mut impl Bus) {
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

    /// Shift/rotate group (0xE): the register-target forms. Memory shift-by-1
    /// is a later increment.
    fn op_shift(&mut self, op: u16, bus: &mut impl Bus) {
        let Some(size) = Size::from_op_bits(op >> 6) else {
            return; // ss == 3 → memory shift, not yet
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
}
