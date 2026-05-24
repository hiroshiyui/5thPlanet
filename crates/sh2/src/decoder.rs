//! 16-bit opcode -> [`Op`] decoder.
//!
//! Dispatched by top nibble, then by the bottom nibble or sub-opcode field
//! depending on the family. This matches the structure of the SH-2 manual's
//! encoding tables (Appendix A) — keep it open next to this file when
//! cross-checking bit patterns.
//!
//! Decoding is a pure function (no state) so it can be table-built later if
//! profiling shows it matters. M1 is accuracy-first; a match is clearer.

use crate::isa::{Op, disp4, disp12, imm8, m_field, n_field, uimm8};

pub fn decode(w: u16) -> Op {
    match (w >> 12) & 0xF {
        0x0 => decode_0(w),
        0x1 => Op::MovLS4 {
            rn: n_field(w),
            rm: m_field(w),
            disp: disp4(w),
        },
        0x2 => decode_2(w),
        0x3 => decode_3(w),
        0x4 => decode_4(w),
        0x5 => Op::MovLL4 {
            rn: n_field(w),
            rm: m_field(w),
            disp: disp4(w),
        },
        0x6 => decode_6(w),
        0x7 => Op::AddI {
            rn: n_field(w),
            imm: imm8(w),
        },
        0x8 => decode_8(w),
        0x9 => Op::MovWPcRel {
            rn: n_field(w),
            disp: uimm8(w),
        },
        0xA => Op::Bra { disp: disp12(w) },
        0xB => Op::Bsr { disp: disp12(w) },
        0xC => decode_c(w),
        0xD => Op::MovLPcRel {
            rn: n_field(w),
            disp: uimm8(w),
        },
        0xE => Op::MovI {
            rn: n_field(w),
            imm: imm8(w),
        },
        _ => Op::Illegal(w),
    }
}

fn decode_0(w: u16) -> Op {
    let n = n_field(w);
    let m = m_field(w);
    // Several "0000xxxx 00000xxx" no-operand opcodes share their slot with
    // n/m-bearing forms; check those first.
    match w {
        0x0008 => return Op::Clrt,
        0x0009 => return Op::Nop,
        0x000B => return Op::Rts,
        0x0018 => return Op::Sett,
        0x0019 => return Op::Div0u,
        0x001B => return Op::Sleep,
        0x0028 => return Op::Clrmac,
        0x002B => return Op::Rte,
        _ => {}
    }
    match w & 0xF {
        0x2 => match (w >> 4) & 0xF {
            0x0 => Op::StcSr { rn: n },
            0x1 => Op::StcGbr { rn: n },
            0x2 => Op::StcVbr { rn: n },
            _ => Op::Illegal(w),
        },
        0x3 => match (w >> 4) & 0xF {
            0x0 => Op::Bsrf { rm: n },
            0x2 => Op::Braf { rm: n },
            _ => Op::Illegal(w),
        },
        0x4 => Op::MovBSX { rn: n, rm: m },
        0x5 => Op::MovWSX { rn: n, rm: m },
        0x6 => Op::MovLSX { rn: n, rm: m },
        0x7 => Op::MulL { rn: n, rm: m },
        0xA => match (w >> 4) & 0xF {
            0x0 => Op::StsMach { rn: n },
            0x1 => Op::StsMacl { rn: n },
            0x2 => Op::StsPr { rn: n },
            _ => Op::Illegal(w),
        },
        0xC => Op::MovBLX { rn: n, rm: m },
        0xD => Op::MovWLX { rn: n, rm: m },
        0xE => Op::MovLLX { rn: n, rm: m },
        0xF => Op::MacL { rn: n, rm: m },
        0x9 if ((w >> 4) & 0xF) == 0x2 => Op::Movt { rn: n },
        _ => Op::Illegal(w),
    }
}

fn decode_2(w: u16) -> Op {
    let n = n_field(w);
    let m = m_field(w);
    match w & 0xF {
        0x0 => Op::MovBS { rn: n, rm: m },
        0x1 => Op::MovWS { rn: n, rm: m },
        0x2 => Op::MovLS { rn: n, rm: m },
        0x4 => Op::MovBM { rn: n, rm: m },
        0x5 => Op::MovWM { rn: n, rm: m },
        0x6 => Op::MovLM { rn: n, rm: m },
        0x7 => Op::Div0s { rn: n, rm: m },
        0x8 => Op::Tst { rn: n, rm: m },
        0x9 => Op::And { rn: n, rm: m },
        0xA => Op::Xor { rn: n, rm: m },
        0xB => Op::Or { rn: n, rm: m },
        0xC => Op::CmpStr { rn: n, rm: m },
        0xD => Op::Xtrct { rn: n, rm: m },
        0xE => Op::MuluW { rn: n, rm: m },
        0xF => Op::MulsW { rn: n, rm: m },
        _ => Op::Illegal(w),
    }
}

fn decode_3(w: u16) -> Op {
    let n = n_field(w);
    let m = m_field(w);
    match w & 0xF {
        0x0 => Op::CmpEq { rn: n, rm: m },
        0x2 => Op::CmpHs { rn: n, rm: m },
        0x3 => Op::CmpGe { rn: n, rm: m },
        0x4 => Op::Div1 { rn: n, rm: m },
        0x5 => Op::DmuluL { rn: n, rm: m },
        0x6 => Op::CmpHi { rn: n, rm: m },
        0x7 => Op::CmpGt { rn: n, rm: m },
        0x8 => Op::Sub { rn: n, rm: m },
        0xA => Op::Subc { rn: n, rm: m },
        0xB => Op::Subv { rn: n, rm: m },
        0xC => Op::Add { rn: n, rm: m },
        0xD => Op::DmulsL { rn: n, rm: m },
        0xE => Op::Addc { rn: n, rm: m },
        0xF => Op::Addv { rn: n, rm: m },
        _ => Op::Illegal(w),
    }
}

fn decode_4(w: u16) -> Op {
    let n = n_field(w);
    // Bottom byte fully disambiguates within top-nibble = 4.
    match w & 0xFF {
        0x00 => Op::Shll { rn: n },
        0x01 => Op::Shlr { rn: n },
        0x02 => Op::StsLMach { rn: n },
        0x03 => Op::StcLSr { rn: n },
        0x04 => Op::Rotl { rn: n },
        0x05 => Op::Rotr { rn: n },
        0x06 => Op::LdsLMach { rm: n },
        0x07 => Op::LdcLSr { rm: n },
        0x08 => Op::Shll2 { rn: n },
        0x09 => Op::Shlr2 { rn: n },
        0x0A => Op::LdsMach { rm: n },
        0x0B => Op::Jsr { rm: n },
        0x0E => Op::LdcSr { rm: n },
        0x10 => Op::Dt { rn: n },
        0x11 => Op::CmpPz { rn: n },
        0x12 => Op::StsLMacl { rn: n },
        0x13 => Op::StcLGbr { rn: n },
        0x15 => Op::CmpPl { rn: n },
        0x16 => Op::LdsLMacl { rm: n },
        0x17 => Op::LdcLGbr { rm: n },
        0x18 => Op::Shll8 { rn: n },
        0x19 => Op::Shlr8 { rn: n },
        0x1A => Op::LdsMacl { rm: n },
        0x1B => Op::Tas { rn: n },
        0x1E => Op::LdcGbr { rm: n },
        0x20 => Op::Shal { rn: n },
        0x21 => Op::Shar { rn: n },
        0x22 => Op::StsLPr { rn: n },
        0x23 => Op::StcLVbr { rn: n },
        0x24 => Op::Rotcl { rn: n },
        0x25 => Op::Rotcr { rn: n },
        0x26 => Op::LdsLPr { rm: n },
        0x27 => Op::LdcLVbr { rm: n },
        0x28 => Op::Shll16 { rn: n },
        0x29 => Op::Shlr16 { rn: n },
        0x2A => Op::LdsPr { rm: n },
        0x2B => Op::Jmp { rm: n },
        0x2E => Op::LdcVbr { rm: n },
        // MAC.W @Rm+,@Rn+
        x if x & 0xF == 0xF => Op::MacW {
            rn: n,
            rm: m_field(w),
        },
        _ => Op::Illegal(w),
    }
}

fn decode_6(w: u16) -> Op {
    let n = n_field(w);
    let m = m_field(w);
    match w & 0xF {
        0x0 => Op::MovBL { rn: n, rm: m },
        0x1 => Op::MovWL { rn: n, rm: m },
        0x2 => Op::MovLL { rn: n, rm: m },
        0x3 => Op::MovRR { rn: n, rm: m },
        0x4 => Op::MovBP { rn: n, rm: m },
        0x5 => Op::MovWP { rn: n, rm: m },
        0x6 => Op::MovLP { rn: n, rm: m },
        0x7 => Op::Not { rn: n, rm: m },
        0x8 => Op::SwapB { rn: n, rm: m },
        0x9 => Op::SwapW { rn: n, rm: m },
        0xA => Op::Negc { rn: n, rm: m },
        0xB => Op::Neg { rn: n, rm: m },
        0xC => Op::ExtuB { rn: n, rm: m },
        0xD => Op::ExtuW { rn: n, rm: m },
        0xE => Op::ExtsB { rn: n, rm: m },
        0xF => Op::ExtsW { rn: n, rm: m },
        _ => Op::Illegal(w),
    }
}

fn decode_8(w: u16) -> Op {
    // Sub-opcode is bits 11..8.
    match (w >> 8) & 0xF {
        0x0 => Op::MovBS0 {
            rn: m_field(w),
            disp: disp4(w),
        },
        0x1 => Op::MovWS0 {
            rn: m_field(w),
            disp: disp4(w),
        },
        0x4 => Op::MovBL0 {
            rm: m_field(w),
            disp: disp4(w),
        },
        0x5 => Op::MovWL0 {
            rm: m_field(w),
            disp: disp4(w),
        },
        0x8 => Op::CmpEqI { imm: imm8(w) },
        0x9 => Op::Bt { disp: imm8(w) },
        0xB => Op::Bf { disp: imm8(w) },
        0xD => Op::BtS { disp: imm8(w) },
        0xF => Op::BfS { disp: imm8(w) },
        _ => Op::Illegal(w),
    }
}

fn decode_c(w: u16) -> Op {
    match (w >> 8) & 0xF {
        0x0 => Op::MovBSG { disp: uimm8(w) },
        0x1 => Op::MovWSG { disp: uimm8(w) },
        0x2 => Op::MovLSG { disp: uimm8(w) },
        0x3 => Op::Trapa { imm: uimm8(w) },
        0x4 => Op::MovBLG { disp: uimm8(w) },
        0x5 => Op::MovWLG { disp: uimm8(w) },
        0x6 => Op::MovLLG { disp: uimm8(w) },
        0x7 => Op::Mova { disp: uimm8(w) },
        0x8 => Op::TstI { imm: uimm8(w) },
        0x9 => Op::AndI { imm: uimm8(w) },
        0xA => Op::XorI { imm: uimm8(w) },
        0xB => Op::OrI { imm: uimm8(w) },
        0xC => Op::TstBG { imm: uimm8(w) },
        0xD => Op::AndBG { imm: uimm8(w) },
        0xE => Op::XorBG { imm: uimm8(w) },
        0xF => Op::OrBG { imm: uimm8(w) },
        _ => Op::Illegal(w),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_simple_ops() {
        assert_eq!(decode(0x0009), Op::Nop);
        assert_eq!(decode(0x0008), Op::Clrt);
        assert_eq!(decode(0x0018), Op::Sett);
        assert_eq!(decode(0x000B), Op::Rts);
        assert_eq!(decode(0x002B), Op::Rte);
        assert_eq!(decode(0x0019), Op::Div0u);
    }

    #[test]
    fn decodes_mov_imm() {
        // MOV #0x42, R3  -> 1110 0011 0100 0010 = 0xE342
        assert_eq!(decode(0xE342), Op::MovI { rn: 3, imm: 0x42 });
        // negative immediate sign-extends
        assert_eq!(decode(0xE3FE), Op::MovI { rn: 3, imm: -2 });
    }

    #[test]
    fn decodes_add_reg_and_imm() {
        // ADD R1,R2  -> 0011 0010 0001 1100 = 0x321C
        assert_eq!(decode(0x321C), Op::Add { rn: 2, rm: 1 });
        // ADD #5,R3  -> 0111 0011 0000 0101 = 0x7305
        assert_eq!(decode(0x7305), Op::AddI { rn: 3, imm: 5 });
    }

    #[test]
    fn decodes_branches() {
        // BRA -2 (encoded as 12-bit signed disp = 0xFFE -> -2)
        match decode(0xAFFE) {
            Op::Bra { disp } => assert_eq!(disp, -2),
            other => panic!("expected Bra, got {:?}", other),
        }
        // BSR +4 (disp = 0x002)
        match decode(0xB002) {
            Op::Bsr { disp } => assert_eq!(disp, 2),
            other => panic!("expected Bsr, got {:?}", other),
        }
        // BT/S -4 (8-bit signed disp = 0xFE -> -2)
        assert_eq!(decode(0x8DFE), Op::BtS { disp: -2 });
    }

    #[test]
    fn decodes_load_store_modes() {
        // MOV.L R1, @(2,R3)  -> 0001 nnnn mmmm dddd = 0001 0011 0001 0010 = 0x1312
        assert_eq!(
            decode(0x1312),
            Op::MovLS4 {
                rn: 3,
                rm: 1,
                disp: 2,
            }
        );
        // MOV.L @(2,R3), R1 -> 0101 nnnn mmmm dddd
        assert_eq!(
            decode(0x5132),
            Op::MovLL4 {
                rn: 1,
                rm: 3,
                disp: 2,
            }
        );
    }

    #[test]
    fn decodes_jmp_and_jsr() {
        // JMP @R5 -> 0100 0101 0010 1011 = 0x452B
        assert_eq!(decode(0x452B), Op::Jmp { rm: 5 });
        // JSR @R5 -> 0100 0101 0000 1011 = 0x450B
        assert_eq!(decode(0x450B), Op::Jsr { rm: 5 });
    }

    #[test]
    fn unknown_opcodes_are_illegal() {
        assert_eq!(decode(0xFFFF), Op::Illegal(0xFFFF));
    }
}
