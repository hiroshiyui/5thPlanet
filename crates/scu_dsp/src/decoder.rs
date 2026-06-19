//! 32-bit DSP word → [`Op`] classifier.
//!
//! Only the top-level class is decided here; the VLIW operation word and the
//! control-op operands are decoded where they execute (`interpreter.rs`),
//! which keeps the slot interactions in one place. Dispatch matches MAME's
//! `execute_run`: top two bits select the class, and for the 11-class the
//! next two bits (29..28) select DMA / JMP / LPS-BTM / END.

use crate::isa::Op;

/// Classify one 32-bit DSP word into its top-level [`Op`]; the operand fields
/// are decoded at execution time (see the module header).
pub fn decode(word: u32) -> Op {
    match (word >> 30) & 0b11 {
        0b00 => Op::Operation(word),
        0b01 => Op::Illegal(word),
        0b10 => Op::Mvi(word),
        // 0b11 — control class, sub-selected by bits 29..28.
        _ => match (word >> 28) & 0b11 {
            0b00 => Op::Dma(word),
            0b01 => Op::Jmp(word),
            0b10 => Op::Loop(word),
            _ => Op::End(word),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_each_top_level_class() {
        assert_eq!(decode(0x0000_0000), Op::Operation(0x0000_0000));
        assert_eq!(decode(0x4000_0000), Op::Illegal(0x4000_0000));
        assert_eq!(decode(0x8000_0000), Op::Mvi(0x8000_0000));
    }

    #[test]
    fn classifies_control_subops() {
        // 11 00 → DMA, 11 01 → JMP, 11 10 → LOOP, 11 11 → END.
        assert_eq!(decode(0xC000_0000), Op::Dma(0xC000_0000));
        assert_eq!(decode(0xD000_0000), Op::Jmp(0xD000_0000));
        assert_eq!(decode(0xE000_0000), Op::Loop(0xE000_0000));
        assert_eq!(decode(0xF000_0000), Op::End(0xF000_0000));
    }
}
