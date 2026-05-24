//! Decoded SH-2 instruction representation.
//!
//! One [`Op`] variant per distinct SH-2 encoding (~142 instructions). Operand
//! fields are pre-extracted by the decoder so the interpreter never has to
//! re-parse the raw 16-bit word.
//!
//! Reference: *SH-1/SH-2 Software Manual* §6 "Instruction Set" (Hitachi).
//! Naming follows the manual's mnemonics in CamelCase, with size suffixes
//! (`B`/`W`/`L`) and addressing-mode hints folded into the variant name.

#![allow(non_snake_case)]

/// Register index (0..15). Held as `u8` for compact storage in [`Op`].
pub type Reg = u8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Op {
    // ---- Data transfer ----
    /// `MOV #imm,Rn` — sign-extend 8-bit imm to 32 bits.
    MovI { rn: Reg, imm: i8 },
    /// `MOV.W @(disp,PC),Rn` — disp is unsigned 8 bits, scaled ×2.
    MovWPcRel { rn: Reg, disp: u8 },
    /// `MOV.L @(disp,PC),Rn` — disp is unsigned 8 bits, scaled ×4.
    MovLPcRel { rn: Reg, disp: u8 },
    MovRR { rn: Reg, rm: Reg },
    MovBS { rn: Reg, rm: Reg },
    MovWS { rn: Reg, rm: Reg },
    MovLS { rn: Reg, rm: Reg },
    MovBL { rn: Reg, rm: Reg },
    MovWL { rn: Reg, rm: Reg },
    MovLL { rn: Reg, rm: Reg },
    MovBM { rn: Reg, rm: Reg }, // pre-decrement store: MOV.B Rm,@-Rn
    MovWM { rn: Reg, rm: Reg },
    MovLM { rn: Reg, rm: Reg },
    MovBP { rn: Reg, rm: Reg }, // post-increment load:  MOV.B @Rm+,Rn
    MovWP { rn: Reg, rm: Reg },
    MovLP { rn: Reg, rm: Reg },
    /// `MOV.B R0,@(disp,Rn)` — 4-bit disp.
    MovBS0 { rn: Reg, disp: u8 },
    /// `MOV.W R0,@(disp,Rn)` — 4-bit disp, scaled ×2.
    MovWS0 { rn: Reg, disp: u8 },
    /// `MOV.L Rm,@(disp,Rn)` — 4-bit disp, scaled ×4.
    MovLS4 { rn: Reg, rm: Reg, disp: u8 },
    /// `MOV.B @(disp,Rm),R0` — 4-bit disp.
    MovBL0 { rm: Reg, disp: u8 },
    /// `MOV.W @(disp,Rm),R0` — 4-bit disp, scaled ×2.
    MovWL0 { rm: Reg, disp: u8 },
    /// `MOV.L @(disp,Rm),Rn` — 4-bit disp, scaled ×4.
    MovLL4 { rn: Reg, rm: Reg, disp: u8 },
    MovBSX { rn: Reg, rm: Reg }, // @(R0,Rn)
    MovWSX { rn: Reg, rm: Reg },
    MovLSX { rn: Reg, rm: Reg },
    MovBLX { rn: Reg, rm: Reg },
    MovWLX { rn: Reg, rm: Reg },
    MovLLX { rn: Reg, rm: Reg },
    MovBSG { disp: u8 }, // R0,@(disp,GBR)
    MovWSG { disp: u8 },
    MovLSG { disp: u8 },
    MovBLG { disp: u8 }, // @(disp,GBR),R0
    MovWLG { disp: u8 },
    MovLLG { disp: u8 },
    Mova { disp: u8 },
    Movt { rn: Reg },
    SwapB { rn: Reg, rm: Reg },
    SwapW { rn: Reg, rm: Reg },
    Xtrct { rn: Reg, rm: Reg },

    // ---- Arithmetic ----
    Add { rn: Reg, rm: Reg },
    AddI { rn: Reg, imm: i8 },
    Addc { rn: Reg, rm: Reg },
    Addv { rn: Reg, rm: Reg },
    CmpEqI { imm: i8 },
    CmpEq { rn: Reg, rm: Reg },
    CmpHs { rn: Reg, rm: Reg },
    CmpGe { rn: Reg, rm: Reg },
    CmpHi { rn: Reg, rm: Reg },
    CmpGt { rn: Reg, rm: Reg },
    CmpPl { rn: Reg },
    CmpPz { rn: Reg },
    CmpStr { rn: Reg, rm: Reg },
    Div1 { rn: Reg, rm: Reg },
    Div0s { rn: Reg, rm: Reg },
    Div0u,
    DmulsL { rn: Reg, rm: Reg },
    DmuluL { rn: Reg, rm: Reg },
    Dt { rn: Reg },
    ExtsB { rn: Reg, rm: Reg },
    ExtsW { rn: Reg, rm: Reg },
    ExtuB { rn: Reg, rm: Reg },
    ExtuW { rn: Reg, rm: Reg },
    MacL { rn: Reg, rm: Reg },
    MacW { rn: Reg, rm: Reg },
    MulL { rn: Reg, rm: Reg },
    MulsW { rn: Reg, rm: Reg },
    MuluW { rn: Reg, rm: Reg },
    Neg { rn: Reg, rm: Reg },
    Negc { rn: Reg, rm: Reg },
    Sub { rn: Reg, rm: Reg },
    Subc { rn: Reg, rm: Reg },
    Subv { rn: Reg, rm: Reg },

    // ---- Logical ----
    And { rn: Reg, rm: Reg },
    AndI { imm: u8 },
    AndBG { imm: u8 },
    Not { rn: Reg, rm: Reg },
    Or { rn: Reg, rm: Reg },
    OrI { imm: u8 },
    OrBG { imm: u8 },
    Tas { rn: Reg },
    Tst { rn: Reg, rm: Reg },
    TstI { imm: u8 },
    TstBG { imm: u8 },
    Xor { rn: Reg, rm: Reg },
    XorI { imm: u8 },
    XorBG { imm: u8 },

    // ---- Shifts ----
    Rotl { rn: Reg },
    Rotr { rn: Reg },
    Rotcl { rn: Reg },
    Rotcr { rn: Reg },
    Shal { rn: Reg },
    Shar { rn: Reg },
    Shll { rn: Reg },
    Shlr { rn: Reg },
    Shll2 { rn: Reg },
    Shlr2 { rn: Reg },
    Shll8 { rn: Reg },
    Shlr8 { rn: Reg },
    Shll16 { rn: Reg },
    Shlr16 { rn: Reg },

    // ---- Branches ----
    /// `BF disp` — sign-extended 8-bit disp, scaled ×2.
    Bf { disp: i8 },
    BfS { disp: i8 },
    Bt { disp: i8 },
    BtS { disp: i8 },
    /// `BRA disp` — sign-extended 12-bit disp, scaled ×2.
    Bra { disp: i16 },
    Braf { rm: Reg },
    Bsr { disp: i16 },
    Bsrf { rm: Reg },
    Jmp { rm: Reg },
    Jsr { rm: Reg },
    Rts,

    // ---- System control ----
    Clrt,
    Clrmac,
    LdcSr { rm: Reg },
    LdcGbr { rm: Reg },
    LdcVbr { rm: Reg },
    LdcLSr { rm: Reg },
    LdcLGbr { rm: Reg },
    LdcLVbr { rm: Reg },
    LdsMach { rm: Reg },
    LdsMacl { rm: Reg },
    LdsPr { rm: Reg },
    LdsLMach { rm: Reg },
    LdsLMacl { rm: Reg },
    LdsLPr { rm: Reg },
    Nop,
    Rte,
    Sett,
    Sleep,
    StcSr { rn: Reg },
    StcGbr { rn: Reg },
    StcVbr { rn: Reg },
    StcLSr { rn: Reg },
    StcLGbr { rn: Reg },
    StcLVbr { rn: Reg },
    StsMach { rn: Reg },
    StsMacl { rn: Reg },
    StsPr { rn: Reg },
    StsLMach { rn: Reg },
    StsLMacl { rn: Reg },
    StsLPr { rn: Reg },
    Trapa { imm: u8 },

    /// Encoding the decoder did not recognize. The interpreter raises
    /// "general illegal instruction" (vector 4).
    Illegal(u16),
}

impl Op {
    /// True if this instruction must not appear in a delay slot
    /// (SH-2 software manual §6, "Delayed Branch Instructions").
    /// Branches, jumps, returns, TRAPA, and instructions that modify SR
    /// or cross-fetch from PC all qualify.
    pub fn is_illegal_in_slot(&self) -> bool {
        use Op::*;
        matches!(
            self,
            Bf { .. }
                | BfS { .. }
                | Bt { .. }
                | BtS { .. }
                | Bra { .. }
                | Braf { .. }
                | Bsr { .. }
                | Bsrf { .. }
                | Jmp { .. }
                | Jsr { .. }
                | Rts
                | Rte
                | Trapa { .. }
                | LdcSr { .. }
                | LdcLSr { .. }
                | MovWPcRel { .. }
                | MovLPcRel { .. }
                | Mova { .. }
        )
    }
}

// Field-extraction helpers — used by both the decoder and the disassembler.
#[inline]
pub(crate) const fn n_field(w: u16) -> Reg {
    ((w >> 8) & 0xF) as Reg
}
#[inline]
pub(crate) const fn m_field(w: u16) -> Reg {
    ((w >> 4) & 0xF) as Reg
}
#[inline]
pub(crate) const fn imm8(w: u16) -> i8 {
    (w & 0xFF) as i8
}
#[inline]
pub(crate) const fn uimm8(w: u16) -> u8 {
    (w & 0xFF) as u8
}
#[inline]
pub(crate) const fn disp4(w: u16) -> u8 {
    (w & 0xF) as u8
}
#[inline]
pub(crate) const fn disp12(w: u16) -> i16 {
    // 12-bit signed -> i16.
    let v = (w & 0x0FFF) as i16;
    (v << 4) >> 4
}
