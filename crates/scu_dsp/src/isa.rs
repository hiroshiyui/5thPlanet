//! Decoded SCU-DSP instruction representation.
//!
//! The DSP word is a 32-bit format. The top two bits select the class:
//!
//! ```text
//!   00  Operation — VLIW: parallel ALU + X-bus + Y-bus + D1-bus slots
//!   01  (illegal)
//!   10  MVI       — move immediate (optionally conditional)
//!   11  Control   — sub-class in bits 29..28: 00 DMA, 01 JMP, 10 LPS/BTM, 11 END
//! ```
//!
//! The Operation class is genuinely VLIW (up to four data moves issue in one
//! word, and they interact — e.g. an X-bus `MOV [s],X` feeds the multiplier
//! whose product a later `MOV MUL,P` reads). Rather than explode that into a
//! product of sub-fields, [`Op::Operation`] carries the raw word and the
//! interpreter walks the slots in hardware order. The control ops, which are
//! single-purpose, are decoded into structured variants.

/// Decoded SCU-DSP instruction class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Op {
    /// VLIW operation word (class 00): ALU + X/Y/D1 data-move slots. Fields
    /// are decoded in the interpreter (see `exec_operation`).
    Operation(u32),
    /// `MVI #imm,[d]` / `MVI #imm,[d],cond` — move (optionally conditional)
    /// sign-extended immediate to a destination register / RAM pointer.
    Mvi(u32),
    /// DMA between DSP data RAM and the A/B-bus via RA0/WA0.
    Dma(u32),
    /// Conditional/unconditional jump to an 8-bit program address.
    Jmp(u32),
    /// `LPS` (repeat next) / `BTM` (branch to TOP) loop control.
    Loop(u32),
    /// `END` / `ENDI` — stop the DSP (ENDI also raises the DSP-end interrupt).
    End(u32),
    /// Class 01: no defined encoding. Treated as a halt-with-no-effect.
    Illegal(u32),
}

/// ALU operation selector (operation-word bits 29..26). Encoding per the SCU
/// manual / MAME `op_alu`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AluOp {
    Nop,
    And,
    Or,
    Xor,
    Add,
    Sub,
    /// AD2 — `ALU = PH:PL + ACH:ACL` (the only op that updates the full 48 bits).
    Ad2,
    /// SR — arithmetic shift ACL right 1 (MSB preserved).
    Sr,
    /// RR — rotate ACL right 1.
    Rr,
    /// SL — shift ACL left 1.
    Sl,
    /// RL — rotate ACL left 1.
    Rl,
    /// RL8 — rotate ACL left 8.
    Rl8,
    /// Unrecognised ALU code (no effect).
    Unknown,
}

impl AluOp {
    /// Decode the 4-bit ALU field.
    pub fn from_bits(bits: u32) -> Self {
        match bits & 0xF {
            0x0 => AluOp::Nop,
            0x1 => AluOp::And,
            0x2 => AluOp::Or,
            0x3 => AluOp::Xor,
            0x4 => AluOp::Add,
            0x5 => AluOp::Sub,
            0x6 => AluOp::Ad2,
            0x8 => AluOp::Sr,
            0x9 => AluOp::Rr,
            0xA => AluOp::Sl,
            0xB => AluOp::Rl,
            0xF => AluOp::Rl8,
            _ => AluOp::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bits_decodes_every_defined_code() {
        assert_eq!(AluOp::from_bits(0x0), AluOp::Nop);
        assert_eq!(AluOp::from_bits(0x1), AluOp::And);
        assert_eq!(AluOp::from_bits(0x2), AluOp::Or);
        assert_eq!(AluOp::from_bits(0x3), AluOp::Xor);
        assert_eq!(AluOp::from_bits(0x4), AluOp::Add);
        assert_eq!(AluOp::from_bits(0x5), AluOp::Sub);
        assert_eq!(AluOp::from_bits(0x6), AluOp::Ad2);
        assert_eq!(AluOp::from_bits(0x8), AluOp::Sr);
        assert_eq!(AluOp::from_bits(0x9), AluOp::Rr);
        assert_eq!(AluOp::from_bits(0xA), AluOp::Sl);
        assert_eq!(AluOp::from_bits(0xB), AluOp::Rl);
        assert_eq!(AluOp::from_bits(0xF), AluOp::Rl8);
    }

    #[test]
    fn from_bits_maps_undefined_codes_to_unknown() {
        // 0x7 and 0xC..0xE have no defined ALU op.
        for code in [0x7u32, 0xC, 0xD, 0xE] {
            assert_eq!(AluOp::from_bits(code), AluOp::Unknown);
        }
    }

    #[test]
    fn from_bits_masks_to_low_nibble() {
        // Only the low 4 bits select the op; higher bits are ignored.
        assert_eq!(AluOp::from_bits(0xFFFF_FFF4), AluOp::Add);
        assert_eq!(AluOp::from_bits(0x1234_5670 | 0x1), AluOp::And);
    }

    #[test]
    fn op_variants_carry_their_raw_word() {
        // The structured variants are thin wrappers around the raw 32-bit word.
        assert_eq!(Op::Operation(0xDEAD_BEEF), Op::Operation(0xDEAD_BEEF));
        assert_ne!(Op::Mvi(1), Op::Dma(1));
        assert_ne!(Op::Jmp(1), Op::Loop(1));
        assert_ne!(Op::End(1), Op::Illegal(1));
    }
}
