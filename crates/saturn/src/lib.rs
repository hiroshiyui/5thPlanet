//! SEGA Saturn system: bus, scheduler, peripherals.
//!
//! This crate is the layer between the chip cores ([`sh2`], and the
//! standalone `scu_dsp`) and a frontend. It owns the Saturn-shaped
//! memory map ([`SaturnBus`]), the event-driven [`Scheduler`] that
//! decides which chip advances next, and the on-board peripherals
//! modeled so far.
//!
//! # Quick tour
//!
//! - [`bus`] — `SaturnBus` impls `sh2::Bus`, dispatches accesses by
//!   address to the typed regions in [`memory`] and the peripherals.
//! - [`memory`] — `BiosRom`, `Ram`, `StubRegisterBank` region backings.
//! - [`smpc`] — System Manager + Peripheral Control. Slave SH-2
//!   hold/release and the INTBACK/NMIREQ command set live here.
//! - [`scu`] — System Control Unit: three DMA channels, timers, IMS/
//!   IST interrupt aggregator, A-bus configuration, version. The DSP
//!   itself is the standalone `scu_dsp` crate.
//! - [`vdp2`] — background generator: registers + VRAM + CRAM + a
//!   minimal NBG0 renderer. [`vdp1`] and [`cd_block`] are address-space
//!   presence stubs (no plotter / no SH-1 yet).
//! - [`scheduler`] — `SchedEntity` trait + linear-scan `Scheduler`.
//!   `Sh2Entity` is the concrete adapter wrapping `sh2::Cpu`.
//! - [`system`] — `Saturn` aggregate: owns bus + scheduler, runs the
//!   headless main loop / `run_frame`, maintains VDP2 raster timing,
//!   and drains queued peripheral commands between scheduler batches.
//!
//! # Milestone status
//!
//! - M2 (bus + scheduler + dual SH-2) and M3 (SCU + SMPC + VDP2
//!   minimal + SCU-DSP + SDL2 scaffolding) complete.
//! - M4 (BIOS-to-splash) active: SMPC INTBACK timing, VDP1/CD-block
//!   presence stubs, VDP2 register-decode fidelity, and VDP2 raster
//!   timing have landed; remaining is raster-timing precision.
//!
//! See `doc/roadmap.md` in the repo root for task-by-task state.

pub mod bus;
pub mod cd_block;
pub mod memory;
pub mod scheduler;
pub mod scu;
pub mod smpc;
pub mod system;
pub mod vdp1;
pub mod vdp2;

pub use bus::SaturnBus;
pub use cd_block::CdBlock;
pub use memory::{BiosRom, Ram, StubRegisterBank};
pub use scheduler::{EntityId, SchedEntity, Scheduler, Sh2Entity};
pub use scu::{DmaRequest, Scu, Source as ScuSource};
pub use smpc::{Command as SmpcCommand, Smpc};
pub use system::Saturn;
pub use vdp1::{Vdp1, Vdp1Regs};
pub use vdp2::{Cram, Vdp2, Vdp2Regs, Vram};
