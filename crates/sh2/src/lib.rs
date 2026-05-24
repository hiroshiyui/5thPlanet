//! Cycle-accurate Hitachi SH-2 (SH7604) CPU core.
//!
//! Designed to be driven by a host bus that owns memory, wait-states, and the
//! Saturn-wide scheduler. The core itself is library-shaped and free of I/O.

#![no_std]

extern crate alloc;

pub mod bus;
pub mod cache;
pub mod debug;
pub mod decoder;
pub mod exceptions;
pub mod harness;
pub mod interpreter;
pub mod isa;
pub mod onchip;
pub mod pipeline;
pub mod regs;

pub use bus::{AccessKind, Bus};
pub use cache::{Cache, Probe};
pub use interpreter::Cpu;
pub use isa::Op;
pub use onchip::{OnChip, Source as InterruptSource};
pub use regs::{Registers, Sr};
