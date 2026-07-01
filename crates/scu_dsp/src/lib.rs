//! SEGA Saturn SCU-DSP — 32-bit vector DSP embedded in the SCU.
//!
//! The DSP is a VLIW-style microcoded processor with its own ISA, 256
//! words of program RAM, four banks of 64 × 32-bit data RAM, a vector
//! multiplier, and an ALU. The Saturn BIOS init paths don't load any
//! DSP microcode by default, but many 3D games hand matrix math off to
//! it; standing this crate up parallel to `sh2` keeps that future work
//! a wire-up exercise rather than a redesign.
//!
//! # Status
//!
//! The DSP core is complete: the operation word's full VLIW
//! parallel-issue — ALU + X-bus + Y-bus + D1-bus executing in hardware
//! order within one instruction (`interpreter::exec_operation`), feeding
//! the vector multiplier — plus MVI / DMA / LOOP / END / ENDI and
//! unconditional + flag-tested JMP. (An earlier milestone modeled only
//! the standalone ALU slot for class-00 words; the parallel slots landed
//! with the full operation word.)
//!
//! # Layout
//!
//! - [`regs`] — register file (PC, RX/RY, P, ACH:ACL, CT0..3, MD0..3, flags)
//! - [`isa`] — decoded `Op` enum
//! - [`decoder`] — 32-bit word → `Op`
//! - [`interpreter`] — `Dsp` struct with `step` / `run_until_stopped`
//!
//! Owned by the SCU host (`crates/saturn`): the host writes microcode
//! into program RAM through the SCU register window, then writes the
//! "start" bit; `Dsp::run_until_stopped` runs the program to its `END`
//! or `ENDI` instruction; if `ENDI`, the host sees
//! `end_interrupt_pending` and forwards the DSP-end source to the SCU
//! INTC. That host glue is wired (`Saturn::drain_scu_dsp`); the crate
//! still builds and tests standalone.
//!
//! Optional `serde` feature (off by default): derives `Serialize`/
//! `Deserialize` on the DSP state (`Dsp`, registers, program + data RAM) for
//! host save states; the Saturn crate enables it. `no_std` without `alloc`, so
//! the >32-element RAM arrays use serde-big-array / a small flat-tuple codec.

#![no_std]

pub mod decoder;
pub mod interpreter;
pub mod isa;
pub mod regs;

pub use decoder::decode;
pub use interpreter::{DmaRequest, Dsp};
pub use isa::{AluOp, Op};
pub use regs::{DATA_RAM_BANKS, DATA_RAM_WORDS_PER_BANK, Flags, PROGRAM_WORDS, Registers};
