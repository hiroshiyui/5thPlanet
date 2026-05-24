//! Top-level Saturn aggregate: bus + scheduler + master/slave SH-2.
//!
//! This is the surface the frontend will hold in M3+. For M2 it stays
//! deliberately thin — `new`/`reset`/`run_for` plus typed accessors for
//! each CPU. Anything chip-specific (VDP2, SCSP, SMPC commands) gets
//! added as a method on `Saturn` when the corresponding peripheral
//! lands, so the frontend doesn't have to reach across module boundaries.

use sh2::Cpu;

use crate::SaturnBus;
use crate::scheduler::{EntityId, Scheduler, Sh2Entity};
use crate::smpc::Command as SmpcCommand;

/// Max scheduler cycles between SMPC-pending checks. Small enough that
/// BIOS code polling SF after a command doesn't spin for a meaningful
/// fraction of a frame; large enough that the inner-loop overhead of
/// poking SMPC every tick isn't paid 28 million times per second.
const SMPC_POLL_QUANTUM: u64 = 256;

/// One emulated SEGA Saturn — a Saturn-shaped memory map populated with
/// a caller-supplied BIOS image, plus master and slave SH-2 cores wired
/// into a shared event-driven scheduler.
pub struct Saturn {
    pub bus: SaturnBus,
    pub scheduler: Scheduler<Sh2Entity>,
    master_id: EntityId,
    slave_id: EntityId,
}

impl Saturn {
    /// Construct with a real BIOS image. Both CPUs start with default
    /// register state; call [`reset`] to load PC/SP from the BIOS reset
    /// vector before stepping.
    pub fn new(bios: Vec<u8>) -> Self {
        let bus = SaturnBus::new(bios);
        let mut scheduler = Scheduler::new();
        let master_id = scheduler.add(Sh2Entity::new(Cpu::new()));
        let slave_id = scheduler.add(Sh2Entity::new(Cpu::new()));
        Self {
            bus,
            scheduler,
            master_id,
            slave_id,
        }
    }

    /// Construct with an all-zero BIOS — convenient for tests that
    /// don't need a real boot image.
    pub fn with_blank_bios() -> Self {
        Self::new(vec![0u8; 512 * 1024])
    }

    /// Pull PC and SP for both CPUs from the BIOS reset vector
    /// (`0x00000000` for PC, `0x00000004` for SP) and clear pipeline
    /// state. On real hardware the slave is held in reset until the
    /// master writes the SMPC `SETSL` command — for M2 we bring both
    /// up immediately; SMPC-driven slave hold-down arrives in M3.
    pub fn reset(&mut self) {
        // Destructure self into disjoint borrows so the bus borrow doesn't
        // collide with the scheduler-entity borrow.
        let Self {
            bus,
            scheduler,
            master_id,
            slave_id,
        } = self;
        scheduler.entity_mut(*master_id).cpu.reset(bus);
        scheduler.entity_mut(*slave_id).cpu.reset(bus);
        // Real Saturn power-on: slave is held in reset until the BIOS
        // sends SMPC SETSL. Mirror that here.
        scheduler.entity_mut(*slave_id).set_halted(true);
        scheduler.entity_mut(*master_id).set_halted(false);
    }

    /// Halt the slave SH-2. Triggered by SMPC `SSHOFF`.
    pub fn halt_slave(&mut self) {
        self.scheduler.entity_mut(self.slave_id).set_halted(true);
    }

    /// Release the slave SH-2 from halt. Triggered by SMPC `SSHON`.
    /// The slave resumes from whatever PC/SP it was last left at — on
    /// real hardware this is the BIOS reset vector, which is also what
    /// our [`reset`] sets up.
    pub fn release_slave(&mut self) {
        self.scheduler.entity_mut(self.slave_id).set_halted(false);
    }

    pub fn slave_is_halted(&self) -> bool {
        self.scheduler.entity(self.slave_id).is_halted()
    }

    pub fn master(&self) -> &Cpu {
        &self.scheduler.entity(self.master_id).cpu
    }
    pub fn master_mut(&mut self) -> &mut Cpu {
        &mut self.scheduler.entity_mut(self.master_id).cpu
    }
    pub fn slave(&self) -> &Cpu {
        &self.scheduler.entity(self.slave_id).cpu
    }
    pub fn slave_mut(&mut self) -> &mut Cpu {
        &mut self.scheduler.entity_mut(self.slave_id).cpu
    }

    /// Advance global time by at least `cycles` cycles, interleaving
    /// the two CPUs by deadline order and polling SMPC for queued
    /// commands every [`SMPC_POLL_QUANTUM`] cycles.
    pub fn run_for(&mut self, cycles: u64) {
        let target = self.now().saturating_add(cycles);
        while self.now() < target {
            let remaining = target - self.now();
            let batch = remaining.min(SMPC_POLL_QUANTUM);
            self.scheduler.run_for(batch, &mut self.bus);
            self.drain_smpc();
        }
    }

    /// Pop any command queued by SMPC and apply its emulator-wide side
    /// effect. Called from `run_for` between scheduler batches.
    fn drain_smpc(&mut self) {
        while let Some(cmd) = self.bus.smpc.take_pending() {
            match cmd {
                SmpcCommand::SshOn => self.release_slave(),
                SmpcCommand::SshOff => self.halt_slave(),
                // Other commands are recognised but have no M3 side
                // effect — they'll get real implementations as the
                // corresponding subsystems land in M3/M4.
                _ => {}
            }
            self.bus.smpc.mark_command_done();
        }
    }

    /// Global cycle as tracked by the scheduler.
    pub fn now(&self) -> u64 {
        self.scheduler.now()
    }
}
