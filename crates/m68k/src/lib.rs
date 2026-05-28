//! Cycle-accurate Motorola MC68EC000 CPU core.
//!
//! The Saturn's SCSP sound subsystem is driven by an MC68EC000. Like the
//! `sh2` crate this core is library-shaped and free of I/O: a host bus owns
//! memory, wait-states, and (later) the SCSP scheduler. The 68000 is
//! big-endian, matching the SH-2 — word/long accesses use `from_be_bytes` /
//! `to_be_bytes`.
//!
//! **Status:** increment 1 of the chip — the data-movement and control-flow
//! core. See [`interpreter`] for the implemented instruction scope.
//!
//! Optional `serde` feature (off by default): derives `Serialize`/`Deserialize`
//! on the CPU state (`Cpu` + registers) for host save states; the Saturn crate
//! enables it.

#![no_std]

extern crate alloc;

pub mod bus;
pub mod harness;
pub mod interpreter;
pub mod isa;
pub mod regs;

pub use bus::{AccessKind, Bus};
pub use interpreter::Cpu;
pub use isa::{Cond, Size};
pub use regs::{Registers, Sr};
