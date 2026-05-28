//! SCU-DSP architectural register file.
//!
//! The DSP has program memory + four banks of data RAM + a set of named
//! architectural registers. Per the SCU User's Manual (and cross-checked
//! against MAME's `scudsp` core for the exact bit semantics):
//!
//! ```text
//!   Program RAM     256 × 32-bit       microcode, loaded by the host
//!   Data RAM CT0..3 4 × 64 × 32-bit    general purpose; per-bank 6-bit pointer
//!   PC              8-bit              program counter (0..255)
//!   TOP             8-bit              loop top (set by JMP-to-PC / used by BTM)
//!   LOP             12-bit             loop counter (LPS/BTM)
//!   RX, RY          32-bit each        multiplier inputs
//!   MUL             48-bit signed      RX × RY product (recomputed when RX/RY load)
//!   PH:PL           16:32-bit          product register (PL low, PH high/sign)
//!   ALU             48-bit             ALU result holding register
//!   ACH:ACL         16:32-bit          accumulator (ACL low, ACH high)
//!   CT0..CT3        6-bit each         data-RAM pointers within each bank
//!   RA0, WA0        32-bit each        DMA read / write address registers
//!   RA              8-bit              host RAM-address port pointer
//!   Z,S,C,V,T0      flags              ALU flags + DMA-busy (T0)
//!   EF, EXF         flags              program-end (host IRQ) / executing
//! ```
//!
//! ACL/PL are the ALU operands; the result lands in [`Registers::alu`] and is
//! moved into ACH:ACL only by the operation word's Y-bus `MOV ALU,A`.

pub const PROGRAM_WORDS: usize = 256;
pub const DATA_RAM_WORDS_PER_BANK: usize = 64;
pub const DATA_RAM_BANKS: usize = 4;

/// Architectural flags. `z/s/c/v` are ALU-result flags; `t0` marks a DMA in
/// progress; `end` is the program-end flag the host reads (raises the SCU
/// DSP-end interrupt); `exec` is the executing flag (cleared by END).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Flags {
    pub z: bool,
    pub s: bool,
    pub c: bool,
    pub v: bool,
    pub t0: bool,
    pub end: bool,
    pub exec: bool,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Registers {
    pub pc: u8,
    pub top: u8,
    pub lop: u16,
    pub rx: u32,
    pub ry: u32,
    /// 48-bit signed RX×RY product (sign-extended in an `i64`).
    pub mul: i64,
    /// Product register low / high (PH holds bits 47..32, sign-extended).
    pub pl: u32,
    pub ph: u32,
    /// 48-bit ALU result holding register (sign-extended in an `i64`).
    pub alu: i64,
    pub acl: u32,
    pub ach: u32,
    /// Per-bank data-RAM pointers (6 bits each).
    pub ct: [u8; DATA_RAM_BANKS],
    /// DMA read / write address registers (in 32-bit words; ×4 for bytes).
    pub ra0: u32,
    pub wa0: u32,
    /// Host RAM-address port pointer (auto-increments on host RA reads/writes).
    pub ra: u8,
    pub flags: Flags,
}

impl Registers {
    pub fn new() -> Self {
        Self {
            pc: 0,
            top: 0,
            lop: 0,
            rx: 0,
            ry: 0,
            mul: 0,
            pl: 0,
            ph: 0,
            alu: 0,
            acl: 0,
            ach: 0,
            ct: [0; DATA_RAM_BANKS],
            ra0: 0,
            wa0: 0,
            ra: 0,
            flags: Flags::default(),
        }
    }

    /// Combined 48-bit ACH:ACL as a sign-extended `i64`.
    #[inline]
    pub fn ac48(&self) -> i64 {
        // ACH is the high 16 bits of the 48-bit accumulator.
        let raw = (((self.ach as u64) & 0xFFFF) << 32) | self.acl as u64;
        sign_extend48(raw)
    }

    /// Combined 48-bit PH:PL as a sign-extended `i64`.
    #[inline]
    pub fn p48(&self) -> i64 {
        let raw = (((self.ph as u64) & 0xFFFF) << 32) | self.pl as u64;
        sign_extend48(raw)
    }
}

/// Sign-extend a 48-bit value held in the low bits of a `u64` to `i64`.
#[inline]
pub fn sign_extend48(raw: u64) -> i64 {
    let v = raw & 0xFFFF_FFFF_FFFF;
    if v & 0x8000_0000_0000 != 0 {
        (v | 0xFFFF_0000_0000_0000) as i64
    } else {
        v as i64
    }
}

impl Default for Registers {
    fn default() -> Self {
        Self::new()
    }
}
