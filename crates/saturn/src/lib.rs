//! SEGA Saturn system: bus, scheduler, peripherals.
//!
//! This crate is the layer between the chip cores (currently just
//! [`sh2`], later `scu_dsp` / future VDP / SCSP / CD crates) and a
//! frontend. It owns the Saturn-shaped memory map ([`SaturnBus`]), the
//! event-driven [`Scheduler`] that decides which chip advances next,
//! and the on-board peripherals modeled so far.
//!
//! # Quick tour
//!
//! - [`bus`] — `SaturnBus` impls `sh2::Bus`, dispatches accesses by
//!   address to the typed regions in [`memory`] and the peripherals.
//! - [`memory`] — `BiosRom`, `Ram`, `StubRegisterBank` region backings.
//! - [`smpc`] — System Manager + Peripheral Control. Slave SH-2
//!   hold/release lives here.
//! - [`scu`] — System Control Unit: three DMA channels, timers, IMS/
//!   IST, A-bus configuration, version. SCU-DSP control window storage
//!   is here; the DSP itself moves to its own crate in M3 task #4.
//! - [`scheduler`] — `SchedEntity` trait + linear-scan `Scheduler`.
//!   `Sh2Entity` is the concrete adapter wrapping `sh2::Cpu`.
//! - [`system`] — `Saturn` aggregate: owns bus + scheduler, runs the
//!   headless main loop, drains queued peripheral commands between
//!   scheduler batches.
//!
//! # Milestone status
//!
//! - M2 (bus + scheduler + dual SH-2) complete.
//! - M3 (SCU + SMPC + VDP2 + SDL2 + BIOS-to-splash) active. SMPC and
//!   SCU DMA done; SCU INTC, SCU-DSP, VDP2, and the frontend wiring
//!   land in the remaining M3 tasks.
//!
//! See `doc/roadmap.md` in the repo root for task-by-task state.

pub mod bus;
pub mod memory;
pub mod scheduler;
pub mod scu;
pub mod smpc;
pub mod system;
pub mod vdp2;

pub use bus::SaturnBus;
pub use memory::{BiosRom, Ram, StubRegisterBank};
pub use scheduler::{EntityId, SchedEntity, Scheduler, Sh2Entity};
pub use scu::{DmaRequest, Scu, Source as ScuSource};
pub use smpc::{Command as SmpcCommand, Smpc};
pub use system::Saturn;
pub use vdp2::{Cram, Vdp2, Vdp2Regs, Vram};
