//! MC68000 programmer's model: eight data registers, eight address
//! registers (A7 is the stack pointer, banked between user and supervisor),
//! the program counter, and the status register.
//!
//! Status register layout (M68000 User's Manual §1.3):
//!
//! ```text
//!   15  T - S -- I2 I1 I0 --- --- X  N  Z  V  C
//!   ^trace  ^supervisor  ^int mask        ^^^^^ condition codes (CCR)
//! ```

/// Condition codes + system byte, kept as named fields and packed/unpacked
/// to the 16-bit SR on demand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Sr {
    pub c: bool,
    pub v: bool,
    pub z: bool,
    pub n: bool,
    pub x: bool,
    pub supervisor: bool,
    pub trace: bool,
    /// Interrupt priority mask I2..I0 (0..7).
    pub imask: u8,
}

impl Sr {
    /// Pack to the 16-bit SR word.
    pub fn to_u16(self) -> u16 {
        let mut v = 0u16;
        v |= self.c as u16;
        v |= (self.v as u16) << 1;
        v |= (self.z as u16) << 2;
        v |= (self.n as u16) << 3;
        v |= (self.x as u16) << 4;
        v |= ((self.imask & 7) as u16) << 8;
        v |= (self.supervisor as u16) << 13;
        v |= (self.trace as u16) << 15;
        v
    }

    /// Unpack from a 16-bit SR word. Does **not** perform the A7 bank swap;
    /// callers changing the S bit must go through [`Registers::set_supervisor`].
    pub fn from_u16(v: u16) -> Self {
        Self {
            c: v & 0x0001 != 0,
            v: v & 0x0002 != 0,
            z: v & 0x0004 != 0,
            n: v & 0x0008 != 0,
            x: v & 0x0010 != 0,
            imask: ((v >> 8) & 7) as u8,
            supervisor: v & 0x2000 != 0,
            trace: v & 0x8000 != 0,
        }
    }

    /// The low byte (condition codes only).
    pub fn ccr(self) -> u8 {
        (self.to_u16() & 0x1F) as u8
    }

    /// Replace the condition codes, leaving the system byte intact.
    pub fn set_ccr(&mut self, v: u8) {
        self.c = v & 0x01 != 0;
        self.v = v & 0x02 != 0;
        self.z = v & 0x04 != 0;
        self.n = v & 0x08 != 0;
        self.x = v & 0x10 != 0;
    }
}

/// The 68000 register file: data D0..D7, address A0..A7 (A7 = the active stack
/// pointer), PC, SR, and the inactive USP/SSP swapped with A7 on an S-bit change.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Registers {
    pub d: [u32; 8],
    /// A0..A7; `a[7]` is the *active* stack pointer.
    pub a: [u32; 8],
    pub pc: u32,
    pub sr: Sr,
    /// The inactive stack pointers, swapped with `a[7]` on an S-bit change.
    pub usp: u32,
    pub ssp: u32,
}

impl Registers {
    /// An all-zero register file.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the supervisor bit, banking A7 between USP and SSP as the 68000
    /// does. A no-op when the bit is unchanged.
    pub fn set_supervisor(&mut self, s: bool) {
        if s == self.sr.supervisor {
            return;
        }
        if self.sr.supervisor {
            self.ssp = self.a[7];
        } else {
            self.usp = self.a[7];
        }
        self.a[7] = if s { self.ssp } else { self.usp };
        self.sr.supervisor = s;
    }
}
