//! 32-bit DSP word → [`Op`] decoder.
//!
//! The M3 subset covers the standalone forms of the operation classes
//! the SCU manual lists at the start of its instruction reference. As
//! the renderer / DSP-using games surface needs for parallel issue
//! and the rest of the special-class ops, extend the match arms.

use crate::isa::{AluOp, Op};

pub fn decode(word: u32) -> Op {
    let class = (word >> 30) & 0b11;
    match class {
        0b00 => decode_operation(word),
        0b10 => decode_mvi(word),
        0b11 => decode_specialized(word),
        _ => Op::Unknown(word),
    }
}

fn decode_operation(word: u32) -> Op {
    // ALU op selector lives in bits 29..26.
    let alu_bits = (word >> 26) & 0b1111;
    let alu = match alu_bits {
        0 => AluOp::Nop,
        1 => AluOp::And,
        2 => AluOp::Or,
        3 => AluOp::Xor,
        4 => AluOp::Add,
        5 => AluOp::Sub,
        6 => AluOp::Ad2,
        7 => AluOp::Sr,
        8 => AluOp::Sl,
        9 => AluOp::Rr,
        10 => AluOp::Rl,
        _ => return Op::Unknown(word),
    };
    Op::Operation { alu }
}

fn decode_mvi(word: u32) -> Op {
    // MVI layout (high 2 bits = 10):
    //   bits 29..26  destination selector
    //   bits 24..0   25-bit signed immediate (sign-extended on use)
    let dest = ((word >> 26) & 0b1111) as u8;
    let raw = (word & 0x01FF_FFFF) as i32; // low 25 bits
    // Sign-extend from 25 bits.
    let imm = (raw << 7) >> 7;
    Op::Mvi { dest, imm }
}

fn decode_specialized(word: u32) -> Op {
    // Specialized instructions sit in the 11xx class. The next 4 bits
    // identify the specific instruction; the rest are operands. Only
    // the M3 subset is recognised — END / ENDI / NOP / JMP. Other
    // specialised forms (BTM, LPS, DMA, etc.) come later.
    match (word >> 26) & 0b1111 {
        0b0000 => Op::Nop, // "NOP" specialised form
        0b1000 => Op::End,
        0b1001 => Op::Endi,
        0b0001 => {
            let cond = ((word >> 19) & 0b1111) as u8;
            let target = (word & 0xFF) as u8;
            Op::Jmp { cond, target }
        }
        _ => Op::Unknown(word),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a class-00 operation word with ALU code `alu` in bits 29..26.
    fn op_word(alu: u32) -> u32 {
        (0b00 << 30) | (alu << 26)
    }
    /// Build a class-11 specialised word with selector `sel` in bits 29..26.
    fn sp_word(sel: u32) -> u32 {
        (0b11 << 30) | (sel << 26)
    }

    #[test]
    fn decodes_each_known_alu_op() {
        let cases = [
            (0, AluOp::Nop),
            (1, AluOp::And),
            (2, AluOp::Or),
            (3, AluOp::Xor),
            (4, AluOp::Add),
            (5, AluOp::Sub),
            (6, AluOp::Ad2),
            (7, AluOp::Sr),
            (8, AluOp::Sl),
            (9, AluOp::Rr),
            (10, AluOp::Rl),
        ];
        for (code, expected) in cases {
            assert_eq!(decode(op_word(code)), Op::Operation { alu: expected });
        }
    }

    #[test]
    fn decodes_mvi_sign_extends_25_bit_immediate() {
        // 25-bit field with the top bit set → negative.
        let word = (0b10 << 30) | (3 << 26) | 0x01FF_FFFF;
        match decode(word) {
            Op::Mvi { dest, imm } => {
                assert_eq!(dest, 3);
                assert_eq!(imm, -1);
            }
            other => panic!("expected MVI, got {other:?}"),
        }
    }

    #[test]
    fn decodes_end_and_endi() {
        assert_eq!(decode(sp_word(0b1000)), Op::End);
        assert_eq!(decode(sp_word(0b1001)), Op::Endi);
    }

    #[test]
    fn decodes_nop_specialised() {
        assert_eq!(decode(sp_word(0b0000)), Op::Nop);
    }

    #[test]
    fn decodes_unconditional_jump() {
        let word = (0b11 << 30) | (0b0001 << 26) | (0 << 19) | 0x42;
        assert_eq!(decode(word), Op::Jmp { cond: 0, target: 0x42 });
    }

    #[test]
    fn unknown_class_or_subop_becomes_unknown() {
        let w = (0b01 << 30) | 0x1234;
        assert_eq!(decode(w), Op::Unknown(w));
    }
}
