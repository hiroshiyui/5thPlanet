//! Host bus interface for the MC68EC000 core.
//!
//! Like the `sh2` crate, the CPU asks the bus for every external access and
//! the bus returns the value plus a stall count, so wait-state math lives
//! outside the core. The 68000 is **big-endian**; word and long accesses are
//! composed from / decomposed to bytes by the caller's `read16`/`read32`
//! using `from_be_bytes` / `to_be_bytes`.

/// Why the CPU is performing this access (lets the host distinguish opcode
/// fetch from data, e.g. for the SCSP bus arbiter later).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessKind {
    /// Instruction or extension-word fetch.
    Fetch,
    /// Operand read/write by an executing instruction.
    Data,
}

/// External memory bus seen by the 68000 core. Each method returns the cycles
/// the CPU should stall waiting for the access (0 if ready immediately).
pub trait Bus {
    fn read8(&mut self, addr: u32, kind: AccessKind) -> (u8, u32);
    fn read16(&mut self, addr: u32, kind: AccessKind) -> (u16, u32);
    fn read32(&mut self, addr: u32, kind: AccessKind) -> (u32, u32);

    fn write8(&mut self, addr: u32, val: u8, kind: AccessKind) -> u32;
    fn write16(&mut self, addr: u32, val: u16, kind: AccessKind) -> u32;
    fn write32(&mut self, addr: u32, val: u32, kind: AccessKind) -> u32;
}
