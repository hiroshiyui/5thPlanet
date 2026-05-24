//! SEGA Saturn SCU-DSP — 32-bit vector DSP embedded in the SCU.
//!
//! The DSP is a VLIW-style microcoded processor with its own ISA, 256
//! words of program RAM, four banks of 64 × 32-bit data RAM, a vector
//! multiplier, and an ALU. The Saturn BIOS init paths don't load any
//! DSP microcode by default, but many 3D games hand matrix math off to
//! it; standing this crate up parallel to `sh2` keeps that future work
//! a wire-up exercise rather than a redesign.
//!
//! # M3 scope
//!
//! M3 implements the *standalone* forms of the operation classes the
//! SCU manual lists first: NOP / ALU ops on ACL + MD0 / MVI / END /
//! ENDI / unconditional + flag-tested JMP. The DSP's real VLIW
//! parallel-issue (ALU + bus + multiplier + jump in one 32-bit word)
//! is deliberately *not* modeled yet — the decoder reads only the ALU
//! slot for class-00 words. As VDP1 and 3D games show specific
//! microcode shapes the project needs, extend the decoder to emit
//! all four slots and have `Dsp::step` retire them together.
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
//! INTC. The host-side glue is deferred to a later M3 refinement;
//! this crate stands alone in the interim.

#![no_std]

pub mod decoder;
pub mod interpreter;
pub mod isa;
pub mod regs;

pub use decoder::decode;
pub use interpreter::Dsp;
pub use isa::{AluOp, Op};
pub use regs::{DATA_RAM_BANKS, DATA_RAM_WORDS_PER_BANK, Flags, PROGRAM_WORDS, Registers};
