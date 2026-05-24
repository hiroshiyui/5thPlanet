//! SEGA Saturn system: bus, scheduler, peripherals.
//!
//! M2 brings up the bus and the dual-SH-2 coscheduling glue. VDP1/2,
//! SCU, SCSP, CD block and frontend are queued for M3+.

pub mod bus;
pub mod memory;
pub mod scheduler;

pub use bus::SaturnBus;
pub use memory::{BiosRom, Ram, StubRegisterBank};
pub use scheduler::{EntityId, SchedEntity, Scheduler, Sh2Entity};
