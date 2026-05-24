//! SCU-DSP architectural register file.
//!
//! The DSP has program memory + four banks of data RAM + a handful of
//! named architectural registers. Per the SCU manual:
//!
//! ```text
//!   Program RAM     256 × 32-bit       (microcode, loaded by host)
//!   Data RAM CT0..3 4 × 64 × 32-bit    (general purpose; per-bank pointer auto-inc/dec)
//!   PC              8-bit              (program counter, 0..255)
//!   TOP             8-bit              (loop top address)
//!   LOP             12-bit             (loop counter)
//!   RX, RY          32-bit each        (multiplier inputs)
//!   P               48-bit signed      (multiplier output, sign-extended in registers)
//!   ACL, ACH        32-bit each        (accumulator low / high; ACH:ACL is 64-bit)
//!   CT0..CT3        6-bit each         (data-RAM pointers within each bank)
//!   MD0..MD3        32-bit each        (latched output of data-RAM reads)
//!   Z, S, C, V, T0  flags              (zero/sign/carry/overflow/loop-end)
//! ```

pub const PROGRAM_WORDS: usize = 256;
pub const DATA_RAM_WORDS_PER_BANK: usize = 64;
pub const DATA_RAM_BANKS: usize = 4;

/// Architectural flags packed into one byte. `Z/S/C/V` mirror the
/// ALU-result flags; `T0` is the "end of loop" indicator the LPS
/// instruction tests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Flags {
    pub z: bool,
    pub s: bool,
    pub c: bool,
    pub v: bool,
    pub t0: bool,
}

#[derive(Clone, Debug)]
pub struct Registers {
    pub pc: u8,
    pub top: u8,
    pub lop: u16,
    pub rx: u32,
    pub ry: u32,
    pub p: i64,
    pub acl: u32,
    pub ach: u32,
    /// Per-bank data-RAM pointers (6 bits each, stored as u8 for ease).
    pub ct: [u8; DATA_RAM_BANKS],
    pub md: [u32; DATA_RAM_BANKS],
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
            p: 0,
            acl: 0,
            ach: 0,
            ct: [0; DATA_RAM_BANKS],
            md: [0; DATA_RAM_BANKS],
            flags: Flags::default(),
        }
    }

    /// Combined 64-bit view of ACH:ACL.
    #[inline]
    pub fn ac(&self) -> u64 {
        ((self.ach as u64) << 32) | self.acl as u64
    }
    #[inline]
    pub fn set_ac(&mut self, v: u64) {
        self.acl = v as u32;
        self.ach = (v >> 32) as u32;
    }
}

impl Default for Registers {
    fn default() -> Self {
        Self::new()
    }
}
