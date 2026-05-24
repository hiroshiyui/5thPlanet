//! SCU-DSP execution state + one-step dispatch.
//!
//! Models the DSP as a synchronous single-issue machine for M3 — the
//! real chip is VLIW with up to four parallel slots, but no BIOS init
//! path nor the M3 splash test loads microcode that depends on
//! parallelism. When VDP1 / 3D-game support arrives, lift the
//! decoder to emit per-slot ops and have `step` retire them together.

use crate::decoder::decode;
use crate::isa::{AluOp, Op};
use crate::regs::{DATA_RAM_BANKS, DATA_RAM_WORDS_PER_BANK, PROGRAM_WORDS, Registers};

/// One emulated SCU-DSP instance.
#[derive(Clone, Debug)]
pub struct Dsp {
    pub regs: Registers,
    pub program: [u32; PROGRAM_WORDS],
    pub data_ram: [[u32; DATA_RAM_WORDS_PER_BANK]; DATA_RAM_BANKS],
    /// True when an END or ENDI instruction has retired and the DSP
    /// has stopped. The host (SCU) clears this on next program start.
    pub stopped: bool,
    /// True when an ENDI raised a DSP-end interrupt. Sampled by the
    /// SCU host, then cleared.
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
            stopped: true,
            end_interrupt_pending: false,
        }
    }

    /// Load microcode into program RAM. Words past the 256-word limit
    /// are silently dropped — that mirrors hardware where the program
    /// counter is 8-bit and any code beyond 255 isn't reachable anyway.
    pub fn load_program(&mut self, base: usize, words: &[u32]) {
        for (i, &w) in words.iter().enumerate() {
            let idx = base + i;
            if idx >= PROGRAM_WORDS {
                break;
            }
            self.program[idx] = w;
        }
    }

    /// Start execution from program address `addr`. Clears the stopped
    /// flag so subsequent `step` calls advance the PC.
    pub fn start(&mut self, addr: u8) {
        self.regs.pc = addr;
        self.stopped = false;
        self.end_interrupt_pending = false;
    }

    /// Advance one instruction. No-op if [`stopped`] is true.
    pub fn step(&mut self) {
        if self.stopped {
            return;
        }
        let word = self.program[self.regs.pc as usize];
        let next_pc = self.regs.pc.wrapping_add(1);
        let op = decode(word);
        match op {
            Op::Operation { alu } => self.exec_alu(alu),
            Op::Mvi { dest, imm } => self.exec_mvi(dest, imm),
            Op::Jmp { cond, target } => {
                if self.condition_met(cond) {
                    self.regs.pc = target;
                    return;
                }
            }
            Op::End => {
                self.stopped = true;
                return;
            }
            Op::Endi => {
                self.stopped = true;
                self.end_interrupt_pending = true;
                return;
            }
            Op::Nop | Op::Unknown(_) => {}
        }
        self.regs.pc = next_pc;
    }

    /// Run until [`stopped`], capped at `max_steps`. Returns the number
    /// of steps actually executed. The cap protects callers from a
    /// microcode bug that hangs the DSP from also hanging the host.
    pub fn run_until_stopped(&mut self, max_steps: u32) -> u32 {
        let mut steps = 0;
        while !self.stopped && steps < max_steps {
            self.step();
            steps += 1;
        }
        steps
    }

    fn exec_alu(&mut self, alu: AluOp) {
        let acl = self.regs.acl;
        let result = match alu {
            AluOp::Nop => return,
            AluOp::And => acl & self.regs.md[0],
            AluOp::Or => acl | self.regs.md[0],
            AluOp::Xor => acl ^ self.regs.md[0],
            AluOp::Add => acl.wrapping_add(self.regs.md[0]),
            AluOp::Sub => acl.wrapping_sub(self.regs.md[0]),
            AluOp::Ad2 => {
                let sum = (self.regs.ac() as i64).wrapping_add(self.regs.p);
                self.regs.set_ac(sum as u64);
                self.set_flags(self.regs.acl);
                return;
            }
            AluOp::Sr => ((acl as i32) >> 1) as u32,
            AluOp::Sl => acl << 1,
            AluOp::Rr => {
                let c = if self.regs.flags.c { 1u32 << 31 } else { 0 };
                let new = c | (acl >> 1);
                self.regs.flags.c = acl & 1 != 0;
                new
            }
            AluOp::Rl => {
                let c = if self.regs.flags.c { 1 } else { 0 };
                let new = (acl << 1) | c;
                self.regs.flags.c = acl & 0x8000_0000 != 0;
                new
            }
        };
        self.regs.acl = result;
        self.set_flags(result);
    }

    fn set_flags(&mut self, v: u32) {
        self.regs.flags.z = v == 0;
        self.regs.flags.s = (v as i32) < 0;
    }

    /// MVI destination selector. Values follow the SCU manual's
    /// ordering. Only the architectural-register targets the M3
    /// subset needs are wired; others fall through as no-ops.
    fn exec_mvi(&mut self, dest: u8, imm: i32) {
        let v = imm as u32;
        match dest {
            0 => self.regs.acl = v,
            1 => self.regs.ach = v,
            2 => self.regs.rx = v,
            3 => self.regs.ry = v,
            4 => self.regs.ct[0] = (v as u8) & 0x3F,
            5 => self.regs.ct[1] = (v as u8) & 0x3F,
            6 => self.regs.ct[2] = (v as u8) & 0x3F,
            7 => self.regs.ct[3] = (v as u8) & 0x3F,
            8 => self.regs.top = v as u8,
            9 => self.regs.lop = v as u16 & 0x0FFF,
            10 => self.regs.pc = v as u8,
            _ => {}
        }
    }

    /// JMP condition codes (M3 subset). Returns `true` if the jump
    /// should be taken.
    fn condition_met(&self, cond: u8) -> bool {
        match cond {
            0 => true,             // unconditional
            1 => self.regs.flags.z,
            2 => !self.regs.flags.z,
            3 => self.regs.flags.s,
            4 => !self.regs.flags.s,
            5 => self.regs.flags.c,
            6 => !self.regs.flags.c,
            _ => false,
        }
    }
}
