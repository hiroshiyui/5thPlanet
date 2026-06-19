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

/// One decoded SH-2 instruction — a single variant per distinct encoding, with
/// operand fields (`rn`/`rm`/`imm`/`disp`) pre-extracted so the interpreter
/// never re-parses the raw word. The classifier methods
/// (`reads_reg`/`load_dest`/`is_illegal_in_slot`/…) drive the pipeline
/// scoreboard; extend them in lockstep when adding an encoding.
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
    /// True iff this op reads architectural register `R[r]` as an input
    /// (compute input, address base, post-modified base, or implicit R0).
    /// Used by the pipeline interlock to detect load-use stalls.
    pub fn reads_reg(&self, r: Reg) -> bool {
        use Op::*;
        let n = |x: &Reg| *x == r;
        let m = |x: &Reg| *x == r;
        let r0 = || r == 0;
        match self {
            // Pure no-read or branch-target arithmetic only.
            Nop | Clrt | Sett | Clrmac | Sleep | Div0u | Rte | Rts
            | Bra { .. } | Bsr { .. }
            | Bf { .. } | Bt { .. } | BfS { .. } | BtS { .. }
            | Trapa { .. } | Mova { .. } | MovI { .. }
            | MovWPcRel { .. } | MovLPcRel { .. } | Movt { .. }
            | Illegal(_) => false,

            // Both rn and rm read (compute-style 2-op).
            Add { rn, rm } | Sub { rn, rm } | Addc { rn, rm } | Addv { rn, rm }
            | Subc { rn, rm } | Subv { rn, rm } | And { rn, rm } | Or { rn, rm }
            | Xor { rn, rm } | Tst { rn, rm } | CmpEq { rn, rm } | CmpHs { rn, rm }
            | CmpGe { rn, rm } | CmpHi { rn, rm } | CmpGt { rn, rm } | CmpStr { rn, rm }
            | MulL { rn, rm } | DmulsL { rn, rm } | DmuluL { rn, rm }
            | Div1 { rn, rm } | Div0s { rn, rm }
            | MulsW { rn, rm } | MuluW { rn, rm }
            | Xtrct { rn, rm }
            | MacL { rn, rm } | MacW { rn, rm }
            | MovRR { rn, rm } => n(rn) || m(rm),

            // rn read-modify-write (add-immediate).
            AddI { rn, .. } => n(rn),
            CmpEqI { .. } => r0(),

            // Single-source rn (cmp/test, shift/rotate, decrement, TAS).
            CmpPl { rn } | CmpPz { rn } | Dt { rn }
            | Shll { rn } | Shlr { rn } | Shal { rn } | Shar { rn }
            | Rotl { rn } | Rotr { rn } | Rotcl { rn } | Rotcr { rn }
            | Shll2 { rn } | Shlr2 { rn } | Shll8 { rn } | Shlr8 { rn }
            | Shll16 { rn } | Shlr16 { rn } | Tas { rn } => n(rn),

            // Loads read rm (and possibly post-modify it); write rn.
            MovBL { rm, .. } | MovWL { rm, .. } | MovLL { rm, .. }
            | MovBP { rm, .. } | MovWP { rm, .. } | MovLP { rm, .. }
            | MovLL4 { rm, .. }
            | MovBL0 { rm, .. } | MovWL0 { rm, .. } => m(rm),

            // R0-indexed loads read R0 and rm; write rn.
            MovBLX { rm, .. } | MovWLX { rm, .. } | MovLLX { rm, .. } => r0() || m(rm),

            // GBR-disp loads target R0 only — no GP read.
            MovBLG { .. } | MovWLG { .. } | MovLLG { .. } => false,

            // Stores read rn (addr base, possibly pre-modified) and rm (data).
            MovBS { rn, rm } | MovWS { rn, rm } | MovLS { rn, rm }
            | MovBM { rn, rm } | MovWM { rn, rm } | MovLM { rn, rm }
            | MovLS4 { rn, rm, .. } => n(rn) || m(rm),

            // R0-disp stores: read R0 (data) + rn (base).
            MovBS0 { rn, .. } | MovWS0 { rn, .. } => r0() || n(rn),

            // R0-indexed stores: read R0 (idx) + rn (base) + rm (data).
            MovBSX { rn, rm } | MovWSX { rn, rm } | MovLSX { rn, rm } => {
                r0() || n(rn) || m(rm)
            }

            // GBR-disp stores: read R0 only.
            MovBSG { .. } | MovWSG { .. } | MovLSG { .. } => r0(),

            // Unary register transforms (write rn from rm).
            SwapB { rm, .. } | SwapW { rm, .. }
            | ExtsB { rm, .. } | ExtsW { rm, .. } | ExtuB { rm, .. } | ExtuW { rm, .. }
            | Neg { rm, .. } | Negc { rm, .. } | Not { rm, .. } => m(rm),

            // Logical R0-immediate / GBR-byte: read R0.
            AndI { .. } | OrI { .. } | XorI { .. } | TstI { .. }
            | AndBG { .. } | OrBG { .. } | XorBG { .. } | TstBG { .. } => r0(),

            // Branches with register targets.
            Braf { rm } | Bsrf { rm } | Jmp { rm } | Jsr { rm } => m(rm),

            // LDC/LDS register forms: read rm.
            LdcSr { rm } | LdcGbr { rm } | LdcVbr { rm }
            | LdsMach { rm } | LdsMacl { rm } | LdsPr { rm } => m(rm),

            // LDC.L/LDS.L: read rm (the post-inc base).
            LdcLSr { rm } | LdcLGbr { rm } | LdcLVbr { rm }
            | LdsLMach { rm } | LdsLMacl { rm } | LdsLPr { rm } => m(rm),

            // STC/STS register forms: write rn only.
            StcSr { .. } | StcGbr { .. } | StcVbr { .. }
            | StsMach { .. } | StsMacl { .. } | StsPr { .. } => false,

            // STC.L/STS.L: read rn (pre-decrement base).
            StcLSr { rn } | StcLGbr { rn } | StcLVbr { rn }
            | StsLMach { rn } | StsLMacl { rn } | StsLPr { rn } => n(rn),
        }
    }

    /// If this op is a load whose result lands in a GP register, return
    /// that register. The instruction immediately following stalls 1 cycle
    /// if it reads the same register (SH-2 1-cycle load-use latency).
    pub fn load_dest(&self) -> Option<Reg> {
        use Op::*;
        match self {
            MovBL { rn, .. } | MovWL { rn, .. } | MovLL { rn, .. }
            | MovBP { rn, .. } | MovWP { rn, .. } | MovLP { rn, .. }
            | MovLL4 { rn, .. }
            | MovWPcRel { rn, .. } | MovLPcRel { rn, .. }
            | MovBLX { rn, .. } | MovWLX { rn, .. } | MovLLX { rn, .. } => Some(*rn),
            // Loads with implicit R0 destination.
            MovBL0 { .. } | MovWL0 { .. }
            | MovBLG { .. } | MovWLG { .. } | MovLLG { .. } => Some(0),
            _ => None,
        }
    }

    /// If this op starts a multiplier operation, return the *additional*
    /// cycles past instruction retire before MACH/MACL is committed.
    ///
    /// For M1 every multiply returns `Some(0)` — the SH7604 multiplier
    /// pipeline's full latency is folded into the base issue cost we
    /// already charge (`MUL.L` = 2 cycles, `MAC.L` = 3 cycles, etc.), so
    /// back-to-back `MUL → STS` doesn't add an extra stall on top.
    /// The scoreboard hook stays in place so a future refinement can
    /// model multiplier–consumer overlap without re-plumbing the dispatch.
    pub fn multiply_latency(&self) -> Option<u32> {
        use Op::*;
        match self {
            MulL { .. } | DmulsL { .. } | DmuluL { .. }
            | MulsW { .. } | MuluW { .. }
            | MacL { .. } | MacW { .. } => Some(0),
            _ => None,
        }
    }

    /// True iff this op reads MACH or MACL as an architectural source.
    pub fn reads_mac(&self) -> bool {
        use Op::*;
        matches!(
            self,
            StsMach { .. } | StsMacl { .. } | StsLMach { .. } | StsLMacl { .. }
        )
    }

    /// True if this instruction must not appear in a delay slot
    /// (SH-2 software manual §6, "Delayed Branch Instructions").
    /// Only the *branch* family qualifies — Bx/BxS, BRA/BRAF, BSR/BSRF,
    /// JMP/JSR, RTS/RTE, TRAPA — matching Mednafen's slot decode
    /// (`sh7095_opdefs.inc` `OP_SLOT_ILLEGAL`). The PC-relative fetches
    /// (`MOV.W/L @(disp,PC)`, `MOVA`) are **legal** in a slot; their PC
    /// base becomes the branch destination + 2 when the branch is taken
    /// (see `Cpu::pcrel_base`) — VF2's character loader runs `BF/S` with
    /// `MOV.L @(disp,PC)` in the slot, and flagging it vectored the game
    /// into the BIOS fatal halt. Notably `LDC Rm,SR` / `LDC.L @Rm+,SR`
    /// are also **legal** in a delay slot — `RTS; LDC Rm,SR` is the
    /// standard "restore SR on return" idiom the Saturn BIOS relies on;
    /// flagging it as illegal vectors the BIOS into its dead-wait handler.
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
