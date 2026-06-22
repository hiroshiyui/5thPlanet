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
//!   multi-layer NBG/RBG compositor (`vdp2::renderer`) with rotation,
//!   colour calculation, windows, and per-line scroll/zoom.
//! - [`vdp1`] — sprite/polygon engine: a full command-list plotter
//!   (`vdp1::plotter`) into a double-buffered frame buffer.
//! - [`scsp`] — Sound Processor: 32-slot FM/PCM engine + SCSP-DSP +
//!   the hosted MC68EC000 (`m68k` crate) in sound RAM.
//! - [`cd_block`] — CD-block, high-level-emulated: the host-interface
//!   command protocol is done; the full HLE engine is M7.
//! - [`scheduler`] — `SchedEntity` trait + linear-scan `Scheduler`.
//!   `Sh2Entity` wraps `sh2::Cpu`; `CdBlockEntity` is the CD-block's
//!   sub-frame periodic-firmware timer; `SaturnEntity` is the
//!   heterogeneous enum the live scheduler runs.
//! - [`system`] — `Saturn` aggregate: owns bus + scheduler, runs the
//!   headless main loop / `run_frame`, maintains VDP2 raster timing,
//!   and drains queued peripheral commands between scheduler batches.
//! - [`disc`] — disc-image media (ISO / CUE-BIN / CCD) + TOC.
//! - [`diagnostics`] — built-in self-diagnostics (the `jupiter doctor`
//!   subcommand + the OSD screen): tiny from-reset checks of the CPU and
//!   each chip, plus heuristic boot/compatibility checks.
//!
//! # Milestone status
//!
//! - M1–M12 complete; M13 is a fidelity/feature backlog. Two commercial
//!   games are fully playable: *Virtua Fighter 2* and *Doukyuusei ~if~*.
//!   Highlights: HLE CD-block + cartridge (M7), save states + battery
//!   backup RAM (M8), the in-window OSD (M9), live disc + CDDA (M10),
//!   commercial-game boot (M11), and the cycle-accuracy push (M12).
//!
//! See `doc/roadmap.md` in the repo root for task-by-task state.

pub mod bus;
pub mod cartridge;
pub mod cd_block;
pub mod diagnostics;
pub mod disc;
pub mod memory;
pub mod scheduler;
pub mod scsp;
pub mod savestate;
pub mod scu;
pub mod smpc;
pub mod system;
pub mod vdp1;
pub mod vdp2;

pub use bus::SaturnBus;
pub use cartridge::Cartridge;
pub use cd_block::CdBlock;
pub use memory::{BiosRom, Ram, StubRegisterBank};
pub use savestate::SaveStateError;
pub use scheduler::{CdBlockEntity, EntityId, SaturnEntity, SchedEntity, Scheduler, Sh2Entity};
pub use scsp::Scsp;
pub use scu::{DmaRequest, Scu, Source as ScuSource};
pub use smpc::{Command as SmpcCommand, Smpc};
pub use system::Saturn;
pub use vdp1::{Vdp1, Vdp1Regs};
pub use vdp2::{Cram, Vdp2, Vdp2Regs, Vram};
