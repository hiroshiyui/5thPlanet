//! Decoded SCU-DSP instruction representation.
//!
//! The real DSP word is a VLIW-style 32-bit format with parallel slots
//! (ALU + bus + multiplier + jump). M3 only implements the *standalone*
//! op forms that the BIOS init paths and most early test microcode use;
//! parallel issue is queued for a later refinement.
//!
//! Instruction encoding summary (high 2 bits = major class):
//!
//! ```text
//!   00xx xxxx xxxx xxxx xxxx xxxx xxxx xxxx   Operation (ALU + bus + jump)
//!   10xx xxxx xxxx xxxx xxxx xxxx xxxx xxxx   MVI (move immediate)
//!   11xx xxxx xxxx xxxx xxxx xxxx xxxx xxxx   Specialized (END, JMP, DMA, etc.)
//! ```

/// ALU operation selected by an OPN-class instruction. Bits 31..26.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AluOp {
    /// NOP — no ALU work.
    Nop,
    And,
    Or,
    Xor,
    Add,
    Sub,
    /// AD2 — `ACH:ACL += P` (multiply-accumulate finalization).
    Ad2,
    /// SR — shift ACL right 1, fill MSB with previous MSB (arithmetic).
    Sr,
    /// SL — shift ACL left 1, fill LSB with 0.
    Sl,
    /// RR — rotate ACL right through C.
    Rr,
    /// RL — rotate ACL left through C.
    Rl,
}

/// Decoded SCU-DSP instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Op {
    /// Operation-class instruction with a standalone ALU op. Parallel
    /// bus / multiplier / jump slots are not yet decoded — they read
    /// as zero in M3.
    Operation { alu: AluOp },
    /// `MVI #imm, dest` — move sign-extended immediate to a register
    /// or data-RAM pointer. `dest` is a 4-bit destination selector.
    Mvi { dest: u8, imm: i32 },
    /// `JMP cond, target` — conditional jump to an 8-bit program-RAM
    /// address. `cond` is a 4-bit code; 0 = unconditional.
    Jmp { cond: u8, target: u8 },
    /// `END` — set the loop-end flag and stop the DSP.
    End,
    /// `ENDI` — `END` that also raises a DSP-end interrupt request
    /// (the SCU INTC source).
    Endi,
    /// `NOP` — explicit no-op (distinct from Operation { alu: Nop }
    /// because some encodings use the dedicated NOP form).
    Nop,
    /// Encoding the decoder did not recognise. The interpreter treats
    /// it as a no-op for now; future revisions will surface it as an
    /// illegal-instruction event.
    Unknown(u32),
}
