//! Exception and interrupt dispatch. Filled out in task #7.
//!
//! Vector numbers per *SH-1/SH-2 Software Manual* §5:
//! - 0: power-on reset (PC)
//! - 1: power-on reset (SP)
//! - 2: manual reset (PC)
//! - 3: manual reset (SP)
//! - 4: general illegal instruction
//! - 6: illegal slot instruction
//! - 9: CPU address error
//! - 10: DMA address error
//! - 11: NMI
//! - 12: user break
//! - 32..63: TRAPA #imm
//! - 64..255: external interrupts (INTC-assigned)

/// Sources that can interrupt execution at an instruction boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Exception {
    GeneralIllegalInstruction,
    SlotIllegalInstruction,
    CpuAddressError,
    DmaAddressError,
    Nmi,
    UserBreak,
    Trapa(u8),
    /// `(vector, level)` from INTC.
    External { vector: u8, level: u8 },
}
