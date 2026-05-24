//! SH-2 architectural register file.

/// Status Register. SH-2 layout (bits not listed are reserved / 0):
///
/// ```text
///   31..10  reserved
///   9       M
///   8       Q
///   7..4    I[3:0]  interrupt mask
///   1       S       saturation enable for MAC
///   0       T       true/false flag
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Sr(pub u32);

impl Sr {
    pub const T: u32 = 1 << 0;
    pub const S: u32 = 1 << 1;
    pub const I_MASK: u32 = 0b1111 << 4;
    pub const Q: u32 = 1 << 8;
    pub const M: u32 = 1 << 9;
    pub const WRITE_MASK: u32 = Self::T | Self::S | Self::I_MASK | Self::Q | Self::M;

    #[inline]
    pub fn t(self) -> bool {
        self.0 & Self::T != 0
    }
    #[inline]
    pub fn set_t(&mut self, v: bool) {
        if v {
            self.0 |= Self::T;
        } else {
            self.0 &= !Self::T;
        }
    }

    #[inline]
    pub fn s(self) -> bool {
        self.0 & Self::S != 0
    }
    #[inline]
    pub fn set_s(&mut self, v: bool) {
        if v {
            self.0 |= Self::S;
        } else {
            self.0 &= !Self::S;
        }
    }

    #[inline]
    pub fn q(self) -> bool {
        self.0 & Self::Q != 0
    }
    #[inline]
    pub fn set_q(&mut self, v: bool) {
        if v {
            self.0 |= Self::Q;
        } else {
            self.0 &= !Self::Q;
        }
    }

    #[inline]
    pub fn m(self) -> bool {
        self.0 & Self::M != 0
    }
    #[inline]
    pub fn set_m(&mut self, v: bool) {
        if v {
            self.0 |= Self::M;
        } else {
            self.0 &= !Self::M;
        }
    }

    /// Interrupt mask level 0..=15.
    #[inline]
    pub fn imask(self) -> u8 {
        ((self.0 & Self::I_MASK) >> 4) as u8
    }
    #[inline]
    pub fn set_imask(&mut self, lvl: u8) {
        self.0 = (self.0 & !Self::I_MASK) | (((lvl as u32) & 0xF) << 4);
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Registers {
    /// General-purpose R0..R15. R15 is the stack pointer.
    pub r: [u32; 16],
    pub pc: u32,
    /// Procedure register (return address for BSR/JSR).
    pub pr: u32,
    /// Global base register.
    pub gbr: u32,
    /// Vector base register.
    pub vbr: u32,
    pub mach: u32,
    pub macl: u32,
    pub sr: Sr,
}

impl Registers {
    /// Architectural reset state. PC and SP are loaded from the reset vector
    /// at 0x00000000 by `Cpu::reset`, not here.
    pub fn new_at_reset() -> Self {
        let mut sr = Sr::default();
        sr.set_imask(0xF); // all interrupts masked on reset
        Self {
            r: [0; 16],
            pc: 0,
            pr: 0,
            gbr: 0,
            vbr: 0,
            mach: 0,
            macl: 0,
            sr,
        }
    }
}
