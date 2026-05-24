//! In-memory bus fixture for tests and the ROM regression harness.
//!
//! Mirrors a flat 32-bit address space backed by a single `Vec<u8>`. Wait
//! states are always zero; for cycle-accurate Saturn bus modeling use the
//! `saturn` crate's bus instead.
//!
//! Also exposes [`state_digest`] — a 64-bit FNV-1a fingerprint of CPU
//! state plus selected memory regions. Used by the ROM regression suite
//! to detect silent behavioural drift across refactors.

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

/// Memory region (`start..end`) included in a state digest.
pub type Region = core::ops::Range<u32>;

/// 64-bit FNV-1a fingerprint of CPU architectural state plus the union of
/// the named memory regions, in the order supplied. Stable across runs
/// and platforms; safe to commit as a golden value.
///
/// Hashes (in order): R0..R15, PC, PR, GBR, VBR, MACH, MACL, SR, then the
/// byte slice for each region read via `MemBus`.
pub fn state_digest(cpu: &crate::Cpu, bus: &MemBus, regions: &[Region]) -> u64 {
    let mut h = Fnv64::new();
    for r in &cpu.regs.r {
        h.eat_u32(*r);
    }
    h.eat_u32(cpu.regs.pc);
    h.eat_u32(cpu.regs.pr);
    h.eat_u32(cpu.regs.gbr);
    h.eat_u32(cpu.regs.vbr);
    h.eat_u32(cpu.regs.mach);
    h.eat_u32(cpu.regs.macl);
    h.eat_u32(cpu.regs.sr.0);
    for region in regions {
        let mem = bus.as_slice();
        for addr in region.clone() {
            let idx = (addr as usize) % mem.len();
            h.eat_u8(mem[idx]);
        }
    }
    h.finish()
}

/// 64-bit FNV-1a hash.
struct Fnv64(u64);

impl Fnv64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01B3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }
    fn eat_u8(&mut self, b: u8) {
        self.0 ^= b as u64;
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }
    fn eat_u32(&mut self, v: u32) {
        for b in v.to_be_bytes() {
            self.eat_u8(b);
        }
    }
    fn finish(self) -> u64 {
        self.0
    }
}
