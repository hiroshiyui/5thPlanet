//! Host bus interface.
//!
//! The CPU asks the bus for every external access. The bus returns the value
//! together with a stall count, so wait-state math lives outside the core and
//! the same `Cpu` instance plugs into any host — test fixture or full Saturn
//! bus — without changes.

/// Why the CPU is performing this access. Lets the host distinguish opcode
/// fetch from data access (cache/prefetch decisions) and CPU-driven access
/// from on-chip DMA (bus arbitration).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessKind {
    /// Instruction fetch by the CPU.
    Fetch,
    /// Data read/write issued by an executing instruction.
    Data,
    /// On-chip DMAC cycle-stealing or burst transfer.
    Dma,
}

/// External memory bus seen by the SH-2 core.
///
/// Each method returns the cycles the CPU should stall waiting for the access
/// to complete (0 if the bus is ready immediately). The CPU accumulates these
/// into its per-instruction cycle total.
pub trait Bus {
    fn read8(&mut self, addr: u32, kind: AccessKind) -> (u8, u32);
    fn read16(&mut self, addr: u32, kind: AccessKind) -> (u16, u32);
    fn read32(&mut self, addr: u32, kind: AccessKind) -> (u32, u32);

    fn write8(&mut self, addr: u32, val: u8, kind: AccessKind) -> u32;
    fn write16(&mut self, addr: u32, val: u16, kind: AccessKind) -> u32;
    fn write32(&mut self, addr: u32, val: u32, kind: AccessKind) -> u32;
}
