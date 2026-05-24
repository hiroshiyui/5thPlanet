//! In-memory bus fixture for tests and the ROM regression harness.
//!
//! Mirrors a flat 32-bit address space backed by a single `Vec<u8>`. Wait
//! states are always zero; for cycle-accurate Saturn bus modeling use the
//! `saturn` crate's bus instead.

use alloc::vec;
use alloc::vec::Vec;

use crate::bus::{AccessKind, Bus};

/// Flat little-/big-endian-correct RAM. SH-2 is big-endian.
#[derive(Clone, Debug)]
pub struct MemBus {
    mem: Vec<u8>,
}

impl MemBus {
    /// Construct a bus with `size` bytes of zeroed RAM. Addresses outside
    /// `0..size` wrap (modulo `size`) — keeps fixtures simple.
    pub fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    /// Construct from an existing image (e.g. a ROM dump).
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { mem: bytes }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.mem
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.mem
    }

    /// Write a 16-bit instruction word (big-endian) at `addr`. Convenience
    /// for loading code into fixtures.
    pub fn write_u16(&mut self, addr: u32, val: u16) {
        let idx = self.wrap(addr);
        self.mem[idx] = (val >> 8) as u8;
        self.mem[idx + 1] = val as u8;
    }

    pub fn write_u32(&mut self, addr: u32, val: u32) {
        let idx = self.wrap(addr);
        self.mem[idx] = (val >> 24) as u8;
        self.mem[idx + 1] = (val >> 16) as u8;
        self.mem[idx + 2] = (val >> 8) as u8;
        self.mem[idx + 3] = val as u8;
    }

    /// Load a sequence of 16-bit opcodes contiguously starting at `addr`.
    pub fn load_program(&mut self, addr: u32, words: &[u16]) {
        for (i, w) in words.iter().enumerate() {
            self.write_u16(addr + (i as u32) * 2, *w);
        }
    }

    fn wrap(&self, addr: u32) -> usize {
        (addr as usize) % self.mem.len()
    }
}

impl Bus for MemBus {
    fn read8(&mut self, addr: u32, _kind: AccessKind) -> (u8, u32) {
        (self.mem[self.wrap(addr)], 0)
    }
    fn read16(&mut self, addr: u32, _kind: AccessKind) -> (u16, u32) {
        let i = self.wrap(addr);
        (u16::from_be_bytes([self.mem[i], self.mem[i + 1]]), 0)
    }
    fn read32(&mut self, addr: u32, _kind: AccessKind) -> (u32, u32) {
        let i = self.wrap(addr);
        (
            u32::from_be_bytes([
                self.mem[i],
                self.mem[i + 1],
                self.mem[i + 2],
                self.mem[i + 3],
            ]),
            0,
        )
    }
    fn write8(&mut self, addr: u32, val: u8, _kind: AccessKind) -> u32 {
        let i = self.wrap(addr);
        self.mem[i] = val;
        0
    }
    fn write16(&mut self, addr: u32, val: u16, _kind: AccessKind) -> u32 {
        let i = self.wrap(addr);
        let b = val.to_be_bytes();
        self.mem[i] = b[0];
        self.mem[i + 1] = b[1];
        0
    }
    fn write32(&mut self, addr: u32, val: u32, _kind: AccessKind) -> u32 {
        let i = self.wrap(addr);
        let b = val.to_be_bytes();
        self.mem[i] = b[0];
        self.mem[i + 1] = b[1];
        self.mem[i + 2] = b[2];
        self.mem[i + 3] = b[3];
        0
    }
}
