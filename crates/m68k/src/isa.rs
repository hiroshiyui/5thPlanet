//! Shared ISA primitives: operand size and condition codes.
//!
//! The 68000's variable-length encoding (an opcode word followed by 0–N
//! extension words, some consumed while resolving an effective address)
//! makes a flat pre-decoded `Op` table awkward. The interpreter therefore
//! decodes and resolves operands as it executes (see [`crate::interpreter`]);
//! the small shared vocabulary lives here.

/// Operand size for the data-movement and arithmetic groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Byte,
    Word,
    Long,
}

impl Size {
    /// Encoding used by MOVE (bits 13-12): 1=byte, 3=word, 2=long.
    pub fn from_move_bits(bits: u16) -> Option<Size> {
        match bits & 3 {
            1 => Some(Size::Byte),
            3 => Some(Size::Word),
            2 => Some(Size::Long),
            _ => None,
        }
    }

    /// Encoding used by most other groups (bits 7-6): 0=byte, 1=word, 2=long.
    pub fn from_op_bits(bits: u16) -> Option<Size> {
        match bits & 3 {
            0 => Some(Size::Byte),
            1 => Some(Size::Word),
            2 => Some(Size::Long),
            _ => None,
        }
    }

    /// The operand size in bytes (Byte = 1, Word = 2, Long = 4).
    pub fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Long => 4,
        }
    }

    /// Value mask (0xFF / 0xFFFF / 0xFFFF_FFFF).
    pub fn mask(self) -> u32 {
        match self {
            Size::Byte => 0xFF,
            Size::Word => 0xFFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }

    /// Most-significant-bit mask for the size (sign bit).
    pub fn msb(self) -> u32 {
        match self {
            Size::Byte => 0x80,
            Size::Word => 0x8000,
            Size::Long => 0x8000_0000,
        }
    }

    /// Sign-extend a size-masked value to i32.
    pub fn sign_extend(self, v: u32) -> i32 {
        match self {
            Size::Byte => (v as u8 as i8) as i32,
            Size::Word => (v as u16 as i16) as i32,
            Size::Long => v as i32,
        }
    }
}

/// The sixteen condition codes (Bcc / Scc / DBcc), bits 11-8 of the opcode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    T,
    F,
    Hi,
    Ls,
    Cc,
    Cs,
    Ne,
    Eq,
    Vc,
    Vs,
    Pl,
    Mi,
    Ge,
    Lt,
    Gt,
    Le,
}

impl Cond {
    /// Decode the 4-bit condition field (Bcc/Scc/DBcc) into a [`Cond`].
    pub fn from_bits(bits: u16) -> Cond {
        use Cond::*;
        match bits & 0xF {
            0x0 => T,
            0x1 => F,
            0x2 => Hi,
            0x3 => Ls,
            0x4 => Cc,
            0x5 => Cs,
            0x6 => Ne,
            0x7 => Eq,
            0x8 => Vc,
            0x9 => Vs,
            0xA => Pl,
            0xB => Mi,
            0xC => Ge,
            0xD => Lt,
            0xE => Gt,
            _ => Le,
        }
    }

    /// Evaluate the condition against the current condition codes.
    pub fn test(self, c: bool, v: bool, z: bool, n: bool) -> bool {
        use Cond::*;
        match self {
            T => true,
            F => false,
            Hi => !c && !z,
            Ls => c || z,
            Cc => !c,
            Cs => c,
            Ne => !z,
            Eq => z,
            Vc => !v,
            Vs => v,
            Pl => !n,
            Mi => n,
            Ge => n == v,
            Lt => n != v,
            Gt => !z && (n == v),
            Le => z || (n != v),
        }
    }
}
