//! SCU-DSP execution: one-instruction-per-`step` dispatch.
//!
//! Faithful to the SCU User's Manual, with bit-level semantics cross-checked
//! against MAME's `scudsp` core (`op_alu`, the X/Y/D1 buses, MVI/JMP/LPS/BTM/
//! END, and the flag bit positions, which are subtle). The operation word is
//! genuinely VLIW: a single word can run an ALU op plus up to four parallel
//! data moves, and the slots interact (X/Y feed the multiplier; Y's `MOV
//! ALU,A` reads the ALU result this same word computed). Slots execute in
//! Mednafen `GeneralInstr` order (`scu_dsp_gen.cpp`): ALU → X-bus → Y-bus →
//! D1-bus, with every slot's CT-pointer post-increment merged and applied
//! after the D1 slot.
//!
//! DMA transfers are *decoded and queued* here ([`Dsp::take_dma`]); the actual
//! A/B-bus transfer is performed by the SCU host (it needs the system bus).

use crate::decoder::decode;
use crate::isa::{AluOp, Op};
use crate::regs::{
    DATA_RAM_BANKS, DATA_RAM_WORDS_PER_BANK, PROGRAM_WORDS, Registers, sign_extend48,
};

/// Serde codec for the `[[u32; 64]; 4]` data RAM: a flat 256-element tuple
/// (no `alloc`, so this works in the crate's `no_std` build). Needed because
/// the inner dimension (64) exceeds serde's built-in array impls and
/// serde-big-array only covers a single dimension.
#[cfg(feature = "serde")]
mod data_ram_serde {
    use crate::regs::{DATA_RAM_BANKS, DATA_RAM_WORDS_PER_BANK};
    use serde::de::{SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserializer, Serializer};

    type Ram = [[u32; DATA_RAM_WORDS_PER_BANK]; DATA_RAM_BANKS];
    const N: usize = DATA_RAM_BANKS * DATA_RAM_WORDS_PER_BANK;

    /// Serialize the `[[u32; 64]; 4]` data RAM as one flat `N`-element tuple
    /// (serde-big-array is 1-D only, so this no_std codec flattens both dims).
    pub fn serialize<S: Serializer>(ram: &Ram, s: S) -> Result<S::Ok, S::Error> {
        let mut t = s.serialize_tuple(N)?;
        for bank in ram {
            for word in bank {
                t.serialize_element(word)?;
            }
        }
        t.end()
    }

    /// Inverse of the serializer above: read the flat `N`-word tuple back into
    /// the banked data RAM.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Ram, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ram;
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(f, "{N} SCU-DSP data-RAM words")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Ram, A::Error> {
                let mut ram = [[0u32; DATA_RAM_WORDS_PER_BANK]; DATA_RAM_BANKS];
                for (i, slot) in ram.iter_mut().flatten().enumerate() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(i, &self))?;
                }
                Ok(ram)
            }
        }
        d.deserialize_tuple(N, V)
    }
}

/// A DMA transfer the DSP requested but hasn't performed (it moves data
/// between DSP data RAM and the A/B-bus, which only the host can reach).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DmaRequest {
    /// `true` = DSP RAM → A/B-bus (via WA0); `false` = A/B-bus → DSP RAM (RA0).
    pub from_dsp: bool,
    /// Data-RAM bank (0..3) or, for `from_dsp == false`, the destination bank
    /// (4 = program RAM, per the dest-DMA table).
    pub dsp_bank: u8,
    /// Number of 32-bit words to transfer.
    pub size: u32,
    /// Address increment (in bytes) applied to RA0/WA0 per word.
    pub add: u32,
    /// Whether to write the post-transfer address back to RA0/WA0 (`hold==0`).
    pub update_addr: bool,
}

/// One emulated SCU-DSP instance.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Dsp {
    pub regs: Registers,
    // PROGRAM_WORDS (256) exceeds serde's built-in 32-element array impls.
    #[cfg_attr(feature = "serde", serde(with = "serde_big_array::BigArray"))]
    pub program: [u32; PROGRAM_WORDS],
    // The inner `[u32; 64]` also exceeds 32, which serde-big-array's 1-D
    // helper can't reach, so the 4×64 data RAM uses a flat tuple codec.
    #[cfg_attr(feature = "serde", serde(with = "data_ram_serde"))]
    pub data_ram: [[u32; DATA_RAM_WORDS_PER_BANK]; DATA_RAM_BANKS],
    /// Delay-slot address: the instruction after a taken jump/loop executes
    /// once before control resumes at the target (the DSP has a 1-slot delay).
    delay: Option<u8>,
    /// A DMA the DSP requested; drained and executed by the host.
    pub pending_dma: Option<DmaRequest>,
    /// Set by `ENDI`; sampled and cleared by the SCU host to raise its
    /// DSP-end interrupt source.
    pub end_interrupt_pending: bool,
}

impl Default for Dsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Dsp {
    /// A fresh SCU-DSP at reset (program and data RAM cleared, registers zeroed).
    pub fn new() -> Self {
        Self {
            regs: Registers::new(),
            program: [0; PROGRAM_WORDS],
            data_ram: [[0; DATA_RAM_WORDS_PER_BANK]; DATA_RAM_BANKS],
            delay: None,
            pending_dma: None,
            end_interrupt_pending: false,
        }
    }

    /// True while the DSP is halted (the `EXF` execute flag is clear).
    #[inline]
    pub fn stopped(&self) -> bool {
        !self.regs.flags.exec
    }

    /// Load microcode into program RAM. Words past the 256-word limit are
    /// dropped (the PC is 8-bit, so they're unreachable anyway).
    pub fn load_program(&mut self, base: usize, words: &[u32]) {
        for (i, &w) in words.iter().enumerate() {
            let idx = base + i;
            if idx >= PROGRAM_WORDS {
                break;
            }
            self.program[idx] = w;
        }
    }

    /// Start execution at program address `addr` (sets the execute flag).
    pub fn start(&mut self, addr: u8) {
        self.regs.pc = addr;
        self.regs.flags.exec = true;
        self.regs.flags.end = false;
        self.delay = None;
        self.end_interrupt_pending = false;
    }

    /// Pop a queued DMA request for the host to perform.
    pub fn take_dma(&mut self) -> Option<DmaRequest> {
        self.pending_dma.take()
    }

    /// Advance one instruction. No-op while stopped.
    pub fn step(&mut self) {
        if self.stopped() {
            return;
        }
        // Fetch: a pending delay-slot instruction runs once without advancing
        // the PC; otherwise fetch at PC and post-increment.
        let word = if let Some(d) = self.delay.take() {
            self.program[d as usize]
        } else {
            let w = self.program[self.regs.pc as usize];
            self.regs.pc = self.regs.pc.wrapping_add(1);
            w
        };

        match decode(word) {
            Op::Operation(w) => self.exec_operation(w),
            Op::Mvi(w) => self.exec_mvi(w),
            Op::Dma(w) => self.exec_dma(w),
            Op::Jmp(w) => self.exec_jmp(w),
            Op::Loop(w) => self.exec_loop(w),
            Op::End(w) => self.exec_end(w),
            Op::Illegal(_) => {}
        }
    }

    /// Run until stopped, capped at `max_steps` (guards against a hung
    /// microcode loop hanging the host). Returns steps executed.
    pub fn run_until_stopped(&mut self, max_steps: u32) -> u32 {
        let mut steps = 0;
        while !self.stopped() && steps < max_steps {
            self.step();
            steps += 1;
        }
        steps
    }

    // ---- operation word (VLIW) -------------------------------------------

    fn exec_operation(&mut self, op: u32) {
        // Faithful port of Mednafen `GeneralInstr` (`scu_dsp_gen.cpp`). The ALU
        // is **not** a persistent register: it starts each operation word as a
        // copy of the 48-bit accumulator (`DSPR48 ALU = DSP.AC`), so a 32-bit
        // op only replaces its low 32 bits (the upper 16 come from ACH), and an
        // ALU NOP yields AC unchanged (a `MOV ALU,A` under NOP is a no-op). The
        // result is kept in `regs.alu` for the D1 `ALL`/`ALH` sources and for
        // observers.
        let ac_raw = ((self.regs.ach as u64 & 0xFFFF) << 32) | self.regs.acl as u64;
        let pl = self.regs.pl;
        let mut alu: u64 = ac_raw;
        let mut alu_l = alu as u32;
        let alu_op = AluOp::from_bits(op >> 26);
        {
            let f = &mut self.regs.flags;
            let zs32 = |f: &mut crate::regs::Flags, v: u32| {
                f.s = (v as i32) < 0;
                f.z = v == 0;
            };
            match alu_op {
                AluOp::Nop | AluOp::Unknown => {}
                AluOp::And => {
                    alu_l &= pl;
                    f.c = false;
                    zs32(f, alu_l);
                }
                AluOp::Or => {
                    alu_l |= pl;
                    f.c = false;
                    zs32(f, alu_l);
                }
                AluOp::Xor => {
                    alu_l ^= pl;
                    f.c = false;
                    zs32(f, alu_l);
                }
                AluOp::Add => {
                    // V is sticky (`|=`), cleared only by a status read.
                    let tmp = alu_l as u64 + pl as u64;
                    f.v |= !(alu_l ^ pl) & (alu_l ^ tmp as u32) & 0x8000_0000 != 0;
                    f.c = (tmp >> 32) & 1 != 0;
                    alu_l = tmp as u32;
                    zs32(f, alu_l);
                }
                AluOp::Sub => {
                    let tmp = (alu_l as u64).wrapping_sub(pl as u64);
                    f.v |= (alu_l ^ pl) & (alu_l ^ tmp as u32) & 0x8000_0000 != 0;
                    f.c = (tmp >> 32) & 1 != 0;
                    alu_l = tmp as u32;
                    zs32(f, alu_l);
                }
                AluOp::Ad2 => {
                    // Unsigned 48-bit sum of the masked operands: C is bit 48 of
                    // that sum (NOT of a sign-extended i64 sum, which drops the
                    // carry out of e.g. -1 + 1). S/Z on the 48-bit result.
                    const M48: u64 = 0xFFFF_FFFF_FFFF;
                    let p_raw = ((self.regs.ph as u64 & 0xFFFF) << 32) | pl as u64;
                    let tmp = (alu & M48) + (p_raw & M48);
                    f.v |= ((!(alu ^ p_raw) & (alu ^ tmp)) >> 47) & 1 != 0;
                    f.c = (tmp >> 48) & 1 != 0;
                    f.s = tmp & 0x8000_0000_0000 != 0;
                    f.z = tmp & M48 == 0;
                    alu = tmp & M48;
                    alu_l = alu as u32;
                }
                AluOp::Sr => {
                    // Arithmetic shift right: C = the bit shifted OUT (bit 0).
                    f.c = alu_l & 1 != 0;
                    alu_l = ((alu_l as i32) >> 1) as u32;
                    zs32(f, alu_l);
                }
                AluOp::Rr => {
                    let c = alu_l & 1;
                    f.c = c != 0;
                    alu_l = (alu_l >> 1) | (c << 31);
                    zs32(f, alu_l);
                }
                AluOp::Sl => {
                    f.c = alu_l >> 31 != 0;
                    alu_l <<= 1;
                    zs32(f, alu_l);
                }
                AluOp::Rl => {
                    let c = alu_l >> 31;
                    f.c = c != 0;
                    alu_l = (alu_l << 1) | c;
                    zs32(f, alu_l);
                }
                AluOp::Rl8 => {
                    f.c = (alu_l >> 24) & 1 != 0;
                    alu_l = alu_l.rotate_left(8);
                    zs32(f, alu_l);
                }
            }
        }
        if !matches!(alu_op, AluOp::Ad2) {
            alu = (alu & 0xFFFF_0000_0000) | alu_l as u64;
        }
        self.regs.alu = sign_extend48(alu);

        // Data-RAM banks read by any slot this word (`dr_read`) and the deferred
        // per-bank CT post-increments (`ct_inc`, OR-merged so a bank read by
        // several slots increments once). Mednafen applies all increments
        // together after the D1 slot.
        let mut dr_read = [false; DATA_RAM_BANKS];
        let mut ct_inc = [false; DATA_RAM_BANKS];

        // X-bus. x_op = bits 25..23: bit 2 loads RX; low bits 2 = MOV MUL,P,
        // 3 = MOV [s],P. The product is computed on demand from the RX/RY in
        // effect at the start of the word — BEFORE this word's X-bus RX load —
        // so an RX/RY written by a D1 move or MVI is never stale.
        let x_op = (op >> 23) & 0x7;
        if x_op & 0x3 == 0x2 {
            let m = (self.regs.rx as i32 as i64) * (self.regs.ry as i32 as i64);
            self.regs.mul = m;
            self.regs.ph = ((m as u64 >> 32) & 0xFFFF) as u32;
            self.regs.pl = m as u32;
        }
        if x_op >= 0x3 {
            let s = ((op >> 20) & 0x7) as usize;
            let bank = s & 3;
            let v = self.source_value_no_inc(bank);
            dr_read[bank] = true;
            ct_inc[bank] |= s & 4 != 0;
            if x_op & 0x3 == 0x3 {
                self.regs.pl = v;
                self.regs.ph = if (v as i32) < 0 { 0xFFFF } else { 0 };
            }
            if x_op & 0x4 != 0 {
                self.regs.rx = v;
            }
        }

        // Y-bus. y_op = bits 19..17: bit 2 loads RY; low bits 1 = CLR A,
        // 2 = MOV ALU,A, 3 = MOV [s],A.
        let y_op = (op >> 17) & 0x7;
        match y_op & 0x3 {
            0x1 => {
                self.regs.acl = 0;
                self.regs.ach = 0;
            }
            0x2 => {
                self.regs.ach = ((alu >> 32) & 0xFFFF) as u32;
                self.regs.acl = alu as u32;
            }
            _ => {}
        }
        if y_op >= 0x3 {
            let s = ((op >> 14) & 0x7) as usize;
            let bank = s & 3;
            let v = self.source_value_no_inc(bank);
            dr_read[bank] = true;
            ct_inc[bank] |= s & 4 != 0;
            if y_op & 0x3 == 0x3 {
                self.regs.acl = v;
                self.regs.ach = if (v as i32) < 0 { 0xFFFF } else { 0 };
            }
            if y_op & 0x4 != 0 {
                self.regs.ry = v;
            }
        }

        // D1-bus. Bits 13..12: bit 0 = active; bit 1 = register/RAM source
        // (`MOV [s],[d]`) vs the sign-extended imm8 (`MOV #imm,[d]`).
        let d1_op = (op >> 12) & 0x3;
        if d1_op & 0x1 != 0 {
            let d = ((op >> 8) & 0xF) as usize;
            let mut src = (op & 0xFF) as i8 as i32 as u32;
            if d1_op & 0x2 != 0 {
                let s = (op & 0xF) as usize;
                src = match s {
                    0x0..=0x7 => {
                        let bank = s & 3;
                        dr_read[bank] = true;
                        // An MCn source auto-increments CTn, except when the
                        // destination is the same bank (Mednafen `d != n`).
                        if s & 4 != 0 && d != bank {
                            ct_inc[bank] = true;
                        }
                        self.source_value_no_inc(bank)
                    }
                    0x9 => alu as u32,
                    0xA => (alu >> 16) as u32,
                    // 0x8 and 0xB..=0xF read all-ones (Mednafen).
                    _ => 0xFFFF_FFFF,
                };
            }
            match d {
                0x0..=0x3 => {
                    // A write to a bank another slot read this same word is
                    // suppressed (no write, no CT increment) — Mednafen
                    // `if(!(dr_read & bank))`.
                    if !dr_read[d] {
                        let idx = (self.regs.ct[d] as usize) & 0x3F;
                        self.data_ram[d][idx] = src;
                        ct_inc[d] = true;
                    }
                }
                0x4 => self.regs.rx = src,
                0x5 => {
                    self.regs.pl = src;
                    self.regs.ph = if (src as i32) < 0 { 0xFFFF } else { 0 };
                }
                0x6 => self.regs.ra0 = src,
                0x7 => self.regs.wa0 = src,
                0xA => self.regs.lop = (src & 0x0FFF) as u16,
                0xB => self.regs.top = src as u8,
                0xC..=0xF => {
                    // A CT write replaces the pointer and cancels that bank's
                    // pending increment.
                    let bank = d - 0xC;
                    self.regs.ct[bank] = (src as u8) & 0x3F;
                    ct_inc[bank] = false;
                }
                _ => {}
            }
        }

        for (bank, &inc) in ct_inc.iter().enumerate() {
            if inc {
                self.regs.ct[bank] = (self.regs.ct[bank] + 1) & 0x3F;
            }
        }
    }

    // ---- data-RAM / register access --------------------------------------

    /// Read a data-RAM bank at its current CT pointer **without** the MC
    /// auto-increment (used by the operation-word X/Y slots, which defer the
    /// increment). `bank` is 0..3.
    fn source_value_no_inc(&self, bank: usize) -> u32 {
        self.data_ram[bank & 3][(self.regs.ct[bank & 3] as usize) & 0x3F]
    }

    /// MAME `get_source_mem_value`: 0..3 = M0..3 (no increment), 4..7 = MC0..3
    /// (read then auto-increment the CT pointer). Used by the D1-bus / DMA.
    fn source_value(&mut self, mode: u8) -> u32 {
        let bank = (mode & 3) as usize;
        let v = self.data_ram[bank][(self.regs.ct[bank] as usize) & 0x3F];
        if mode & 4 != 0 {
            self.regs.ct[bank] = (self.regs.ct[bank] + 1) & 0x3F;
        }
        v
    }

    /// MAME `set_dest_mem_reg`: MVI / DMA destination selector (4-bit). (The
    /// operation word's D1 slot has its own inline Mednafen port in
    /// `exec_operation`, which adds the same-word `dr_read` suppression.)
    fn set_dest_reg(&mut self, mode: u8, value: u32) {
        match mode {
            0x0..=0x3 => {
                // MC0..3: write at CT, auto-increment.
                let bank = mode as usize;
                let idx = (self.regs.ct[bank] as usize) & 0x3F;
                self.data_ram[bank][idx] = value;
                self.regs.ct[bank] = (self.regs.ct[bank] + 1) & 0x3F;
            }
            0x4 => self.regs.rx = value,
            0x5 => {
                self.regs.pl = value;
                self.regs.ph = if (value as i32) < 0 { 0xFFFF } else { 0 };
            }
            0x6 => self.regs.ra0 = value,
            0x7 => self.regs.wa0 = value,
            0xA => self.regs.lop = (value & 0x0FFF) as u16,
            0xB => self.regs.top = value as u8,
            0xC => self.regs.ct[0] = (value as u8) & 0x3F,
            0xD => self.regs.ct[1] = (value as u8) & 0x3F,
            0xE => self.regs.ct[2] = (value as u8) & 0x3F,
            0xF => self.regs.ct[3] = (value as u8) & 0x3F,
            _ => {}
        }
    }

    /// MAME `set_dest_mem_reg_2`: MVI destination selector — like
    /// `set_dest_reg` for 0..0xA, plus 0xC = PC (a jump, with delay slot).
    fn set_dest_reg_mvi(&mut self, mode: u8, value: u32) {
        if mode < 0xB {
            self.set_dest_reg(mode, value);
        } else if mode == 0xC {
            // MOV imm,PC — jump; the next instruction is the delay slot.
            self.delay = Some(self.regs.pc);
            self.regs.top = self.regs.pc;
            self.regs.pc = value as u8;
        }
    }

    // ---- control instructions --------------------------------------------

    fn exec_mvi(&mut self, op: u32) {
        let dest = ((op >> 26) & 0xF) as u8;
        if op & 0x0200_0000 != 0 {
            // Conditional MVI: condition in bits 31..19 (low 6 used).
            if self.condition_met((op >> 19) & 0x7F) {
                let imm = sign_extend(op, 19);
                self.set_dest_reg_mvi(dest, imm);
            }
        } else {
            let imm = sign_extend(op, 25);
            self.set_dest_reg_mvi(dest, imm);
        }
    }

    fn exec_jmp(&mut self, op: u32) {
        let take = if op & 0x3F8_0000 != 0 {
            self.condition_met((op >> 19) & 0x7F)
        } else {
            true
        };
        if take {
            self.delay = Some(self.regs.pc);
            self.regs.pc = (op & 0xFF) as u8;
        }
    }

    fn exec_loop(&mut self, op: u32) {
        if self.regs.lop == 0 {
            return;
        }
        self.regs.lop -= 1;
        self.delay = Some(self.regs.pc);
        if op & 0x0800_0000 != 0 {
            // LPS — repeat the next instruction (re-run the delay slot).
            self.regs.pc = self.regs.pc.wrapping_sub(1);
        } else {
            // BTM — branch to TOP.
            self.regs.pc = self.regs.top;
        }
    }

    fn exec_end(&mut self, op: u32) {
        if op & 0x0800_0000 != 0 {
            // ENDI — also raise the program-end interrupt.
            self.regs.flags.end = true;
            self.end_interrupt_pending = true;
        }
        self.regs.flags.exec = false;
    }

    /// Decode a DMA request into [`pending_dma`] for the host to perform, and
    /// set the T0 (DMA-busy) flag. The DSP keeps running; the host clears T0
    /// when it finishes the transfer.
    fn exec_dma(&mut self, op: u32) {
        let hold = (op >> 14) & 1;
        let add_sel = (op >> 15) & 0x7;
        let from_dsp = (op >> 12) & 1 != 0;
        let dsp_bank = ((op >> 8) & 0x3) as u8;

        let (raw_size, add) = if op & 0x2000 != 0 {
            // Length from a data-RAM source register (the count is a u8 field).
            let sz = self.source_value(op as u8 & 0xF) & 0xFF;
            (sz, if add_sel & 0x7 == 0 { 0 } else { 4 })
        } else {
            let sz = op & 0xFF;
            let add = match add_sel {
                0 => 0,
                1 | 2 => 4,
                3 | 4 => 16,
                5 => 64,
                6 => 128,
                _ => 256,
            };
            (sz, add)
        };
        // A transfer count of 0 means 256 words, not 0 (Mednafen `uint8 count`
        // with `do { … } while(--count)` runs 256 iterations when count == 0).
        let size = if raw_size == 0 { 256 } else { raw_size };

        self.regs.flags.t0 = true;
        self.pending_dma = Some(DmaRequest {
            from_dsp,
            dsp_bank,
            size,
            add,
            update_addr: hold == 0,
        });
    }

    /// JMP / MVI / conditional codes (MAME `compute_condition`): low 4 bits
    /// select Z / S / ZS / C / T0; bit 5 (0x20) is the polarity (clear =
    /// negate the test).
    fn condition_met(&self, cond: u32) -> bool {
        let f = &self.regs.flags;
        let result = match cond & 0xF {
            0x1 => f.z,
            0x2 => f.s,
            0x3 => f.z || f.s,
            0x4 => f.c,
            0x8 => f.t0,
            _ => false,
        };
        if cond & 0x20 == 0 { !result } else { result }
    }
}

/// Sign-extend the low `bits` bits of `word` to a 32-bit value.
fn sign_extend(word: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    (((word << shift) as i32) >> shift) as u32
}
