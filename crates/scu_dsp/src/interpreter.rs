//! SCU-DSP execution: one-instruction-per-`step` dispatch.
//!
//! Faithful to the SCU User's Manual, with bit-level semantics cross-checked
//! against MAME's `scudsp` core (`op_alu`, the X/Y/D1 buses, MVI/JMP/LPS/BTM/
//! END, and the flag bit positions, which are subtle). The operation word is
//! genuinely VLIW: a single word can run an ALU op plus up to four parallel
//! data moves, and the slots interact (X/Y feed the multiplier; Y's `MOV
//! ALU,A` reads the ALU result this same word computed). Slots execute in
//! hardware order: ALU → X-bus → Y-bus → CT-pointer post-increment → D1-bus.
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

    pub fn serialize<S: Serializer>(ram: &Ram, s: S) -> Result<S::Ok, S::Error> {
        let mut t = s.serialize_tuple(N)?;
        for bank in ram {
            for word in bank {
                t.serialize_element(word)?;
            }
        }
        t.end()
    }

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
    /// Set when RX or RY is loaded this word; triggers the RX×RY multiply at
    /// the end of the instruction (matches MAME's `m_update_mul`).
    update_mul: bool,
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
    pub fn new() -> Self {
        Self {
            regs: Registers::new(),
            program: [0; PROGRAM_WORDS],
            data_ram: [[0; DATA_RAM_WORDS_PER_BANK]; DATA_RAM_BANKS],
            delay: None,
            update_mul: false,
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
        self.update_mul = false;

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

        if self.update_mul {
            self.regs.mul = (self.regs.rx as i32 as i64) * (self.regs.ry as i32 as i64);
            self.update_mul = false;
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
        // ALU: operands are ACL and PL (32-bit signed). The result lands in
        // the 48-bit ALU register; only AD2 touches its upper 16 bits.
        let acl = self.regs.acl as i32 as i64;
        let pl = self.regs.pl as i32 as i64;
        let alu_raw = (self.regs.alu as u64) & 0xFFFF_FFFF_FFFF;
        let keep_hi = alu_raw & 0xFFFF_0000_0000;
        let set32 = |lo: i64, regs: &mut Registers| {
            regs.alu = sign_extend48(keep_hi | (lo as u32 as u64));
        };
        match AluOp::from_bits(op >> 26) {
            AluOp::Nop | AluOp::Unknown => {}
            AluOp::And => {
                let i3 = acl & pl;
                set32(i3, &mut self.regs);
                self.regs.flags.z = i3 == 0;
                self.regs.flags.c = false;
                self.regs.flags.s = i3 < 0;
            }
            AluOp::Or => {
                let i3 = acl | pl;
                set32(i3, &mut self.regs);
                self.regs.flags.c = false;
                self.regs.flags.s = i3 < 0;
                // HW quirk (MAME): Z reflects the non-negative result only.
                self.regs.flags.z = if i3 < 0 { false } else { i3 == 0 };
            }
            AluOp::Xor => {
                let i3 = acl ^ pl;
                set32(i3, &mut self.regs);
                self.regs.flags.z = i3 == 0;
                self.regs.flags.c = false;
                self.regs.flags.s = i3 < 0;
            }
            AluOp::Add => {
                let i3 = acl + pl;
                set32(i3, &mut self.regs);
                self.regs.flags.z = (i3 & 0xFFFF_FFFF_FFFF) == 0;
                self.regs.flags.s = i3 & 0x1_0000_0000_0000 != 0;
                self.regs.flags.c = i3 & 0x1_0000_0000 != 0;
                self.regs.flags.v = (i3 ^ acl) & (i3 ^ pl) & 0x8000_0000 != 0;
            }
            AluOp::Sub => {
                let i3 = acl - pl;
                set32(i3, &mut self.regs);
                self.regs.flags.z = i3 == 0;
                self.regs.flags.c = i3 & 0x1_0000_0000 != 0;
                self.regs.flags.s = i3 < 0;
                self.regs.flags.v = (pl ^ acl) & (pl ^ i3) & 0x8000_0000 != 0;
            }
            AluOp::Ad2 => {
                let sum = self.regs.p48() + self.regs.ac48();
                let raw = (sum as u64) & 0xFFFF_FFFF_FFFF;
                self.regs.alu = sign_extend48(raw);
                self.regs.flags.z = raw == 0;
                self.regs.flags.s = raw & 0x8000_0000_0000 != 0;
                self.regs.flags.c = (sum as u64) & 0x1_0000_0000_0000 != 0;
                let i1 = self.regs.p48();
                let i2 = self.regs.ac48();
                self.regs.flags.v = (sum ^ i1) & (sum ^ i2) & 0x8000_0000_0000 != 0;
            }
            AluOp::Sr => {
                let aclu = self.regs.acl;
                let i3 = ((aclu >> 1) | (aclu & 0x8000_0000)) as i32 as i64;
                set32(i3, &mut self.regs);
                self.regs.flags.z = i3 == 0;
                self.regs.flags.s = i3 < 0;
                self.regs.flags.c = aclu & 0x8000_0000 != 0;
            }
            AluOp::Rr => {
                let aclu = self.regs.acl;
                let i3 = ((aclu >> 1) & 0x7FFF_FFFF | (aclu << 31) & 0x8000_0000) as i32 as i64;
                set32(i3, &mut self.regs);
                self.regs.flags.z = i3 == 0;
                self.regs.flags.s = i3 < 0;
                self.regs.flags.c = aclu & 0x1 != 0;
            }
            AluOp::Sl => {
                let aclu = self.regs.acl;
                let i3 = (aclu << 1) as i32 as i64;
                set32(i3, &mut self.regs);
                self.regs.flags.z = i3 == 0;
                self.regs.flags.s = i3 < 0;
                self.regs.flags.c = aclu & 0x8000_0000 != 0;
            }
            AluOp::Rl => {
                let aclu = self.regs.acl;
                let i3 = ((aclu << 1) & 0xFFFF_FFFE | (aclu >> 31) & 0x1) as i32 as i64;
                set32(i3, &mut self.regs);
                self.regs.flags.z = i3 == 0;
                self.regs.flags.s = i3 < 0;
                self.regs.flags.c = aclu & 0x8000_0000 != 0;
            }
            AluOp::Rl8 => {
                let aclu = self.regs.acl;
                let i3 = aclu.rotate_left(8) as i32 as i64;
                set32(i3, &mut self.regs);
                self.regs.flags.z = i3 == 0;
                self.regs.flags.s = i3 < 0;
                self.regs.flags.c = aclu & 0x0100_0000 != 0;
            }
        }

        // CT post-increments for MC0..3 source reads are deferred until after
        // both X- and Y-buses have read, so an MC bank read by both slots
        // increments only once (matches MAME's `update_ct[]`).
        let mut update_ct = [false; DATA_RAM_BANKS];

        // X-bus. Bit 25: MOV [s],X (load RX). Bits 24..23: MOV MUL,P / MOV [s],P.
        if op & 0x0200_0000 != 0 {
            let mem = ((op >> 20) & 0x7) as usize;
            self.regs.rx = self.source_value_no_inc(mem & 3);
            if mem & 4 != 0 {
                update_ct[mem & 3] = true;
            }
            self.update_mul = true;
        }
        match (op >> 23) & 0x3 {
            0x2 => {
                // MOV MUL,P
                let m = self.regs.mul as u64;
                self.regs.ph = ((m >> 32) & 0xFFFF) as u32;
                self.regs.pl = m as u32;
            }
            0x3 => {
                // MOV [s],P
                let mem = ((op >> 20) & 0x7) as usize;
                self.regs.pl = self.source_value_no_inc(mem & 3);
                self.regs.ph = if (self.regs.pl as i32) < 0 { 0xFFFF } else { 0 };
                if mem & 4 != 0 {
                    update_ct[mem & 3] = true;
                }
            }
            _ => {}
        }

        // Y-bus. Bit 19: MOV [s],Y (load RY). Bits 18..17: CLR A / MOV ALU,A / MOV [s],A.
        if op & 0x0008_0000 != 0 {
            let mem = ((op >> 14) & 0x7) as usize;
            self.regs.ry = self.source_value_no_inc(mem & 3);
            if mem & 4 != 0 {
                update_ct[mem & 3] = true;
            }
            self.update_mul = true;
        }
        match (op >> 17) & 0x3 {
            0x1 => {
                // CLR A
                self.regs.acl = 0;
                self.regs.ach = 0;
            }
            0x2 => {
                // MOV ALU,A
                let a = self.regs.alu as u64;
                self.regs.ach = ((a >> 32) & 0xFFFF) as u32;
                self.regs.acl = a as u32;
            }
            0x3 => {
                // MOV [s],A
                let mem = ((op >> 14) & 0x7) as usize;
                self.regs.acl = self.source_value_no_inc(mem & 3);
                self.regs.ach = if (self.regs.acl as i32) < 0 {
                    0xFFFF
                } else {
                    0
                };
                if mem & 4 != 0 {
                    update_ct[mem & 3] = true;
                }
            }
            _ => {}
        }

        for (bank, &inc) in update_ct.iter().enumerate() {
            if inc {
                self.regs.ct[bank] = (self.regs.ct[bank] + 1) & 0x3F;
            }
        }

        // D1-bus. Bits 13..12: 1 = MOV imm8,[d]; 3 = MOV [s],[d].
        match (op >> 12) & 0x3 {
            0x1 => {
                let dest = ((op >> 8) & 0xF) as u8;
                let imm = (op & 0xFF) as i8 as i32 as u32;
                self.set_dest_reg(dest, imm);
            }
            0x3 => {
                let dest = ((op >> 8) & 0xF) as u8;
                let v = self.source_reg_value((op & 0xF) as u8);
                self.set_dest_reg(dest, v);
            }
            _ => {}
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

    /// MAME `get_source_mem_reg_value`: D1-bus source — 0..7 = memory,
    /// 9 = ALL (ALU low 32), 0xA = ALH (ALU bits 47..16).
    fn source_reg_value(&mut self, mode: u8) -> u32 {
        match mode {
            0..=7 => self.source_value(mode),
            0x9 => (self.regs.alu as u64) as u32,
            0xA => (((self.regs.alu as u64) >> 16) & 0xFFFF_FFFF) as u32,
            _ => 0,
        }
    }

    /// MAME `set_dest_mem_reg`: D1-bus / DMA destination selector (4-bit).
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

        let (size, add) = if op & 0x2000 != 0 {
            // Length from a data-RAM source register.
            let sz = self.source_value(op as u8 & 0xF);
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
