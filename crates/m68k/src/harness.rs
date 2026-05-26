//! Test harness: a flat big-endian RAM implementing [`Bus`].
//!
//! New opcode integration tests under `crates/m68k/tests/` should build CPUs
//! through this rather than hand-rolling a parallel bus mock, mirroring the
//! `sh2::harness::MemBus` convention.

use crate::bus::{AccessKind, Bus};
use alloc::vec;
use alloc::vec::Vec;

/// Flat little-endian-free RAM. All accesses are zero-stall.
pub struct MemBus {
    pub ram: Vec<u8>,
}

impl MemBus {
    /// A zero-filled RAM of `size` bytes.
    pub fn new(size: usize) -> Self {
        Self {
            ram: vec![0u8; size],
        }
    }

    /// Load `bytes` at `addr` (for planting code/vectors before a run).
    pub fn load(&mut self, addr: u32, bytes: &[u8]) {
        let a = addr as usize;
        self.ram[a..a + bytes.len()].copy_from_slice(bytes);
    }

    /// Write a big-endian 16-bit word (convenience for planting opcodes).
    pub fn write_word(&mut self, addr: u32, val: u16) {
        self.load(addr, &val.to_be_bytes());
    }

    /// Write a big-endian 32-bit long (convenience for planting vectors).
    pub fn write_long(&mut self, addr: u32, val: u32) {
        self.load(addr, &val.to_be_bytes());
    }
}

impl Bus for MemBus {
    fn read8(&mut self, addr: u32, _: AccessKind) -> (u8, u32) {
        (self.ram[addr as usize], 0)
    }
    fn read16(&mut self, addr: u32, _: AccessKind) -> (u16, u32) {
        let a = addr as usize;
        (u16::from_be_bytes([self.ram[a], self.ram[a + 1]]), 0)
    }
    fn read32(&mut self, addr: u32, _: AccessKind) -> (u32, u32) {
        let a = addr as usize;
        (
            u32::from_be_bytes([
                self.ram[a],
                self.ram[a + 1],
                self.ram[a + 2],
                self.ram[a + 3],
            ]),
            0,
        )
    }
    fn write8(&mut self, addr: u32, val: u8, _: AccessKind) -> u32 {
        self.ram[addr as usize] = val;
        0
    }
    fn write16(&mut self, addr: u32, val: u16, _: AccessKind) -> u32 {
        self.load(addr, &val.to_be_bytes());
        0
    }
    fn write32(&mut self, addr: u32, val: u32, _: AccessKind) -> u32 {
        self.load(addr, &val.to_be_bytes());
        0
    }
}
