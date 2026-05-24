//! SEGA Saturn system: bus, scheduler, peripherals.
//!
//! M2 brings up the bus and the dual-SH-2 coscheduling glue. VDP1/2,
//! SCU, SCSP, CD block and frontend are queued for M3+.

pub mod bus;
pub mod memory;
pub mod scheduler;
pub mod scu;
pub mod smpc;
pub mod system;

pub use bus::SaturnBus;
pub use memory::{BiosRom, Ram, StubRegisterBank};
pub use scheduler::{EntityId, SchedEntity, Scheduler, Sh2Entity};
pub use scu::{DmaRequest, Scu};
pub use smpc::{Command as SmpcCommand, Smpc};
pub use system::Saturn;
