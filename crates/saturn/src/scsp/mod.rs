//! Saturn Custom Sound Processor (SCSP) — host wiring for the sound MC68EC000.
//!
//! The SCSP owns the 512 KiB sound RAM, its control/slot register bank, and
//! the hosted [`m68k::Cpu`] sound CPU. The main SH-2 sees the sound RAM at
//! `0x05A0_0000` and the registers at `0x05B0_0000`; the 68k — driven by the
//! Saturn scheduler — sees the same RAM at `0x00_0000` and the registers at
//! `0x10_0000`, running the sound program the main CPU stages into RAM.
//!
//! On power-on the 68k is held in reset; SMPC `SNDON` releases it (reloading
//! SSP/PC from sound-RAM vectors 0/1) and `SNDOFF` re-holds it.
//!
//! **Scope (M5 task #3 — host wiring):** RAM + registers + the hosted,
//! schedulable, resettable 68k. The slot/FM synthesis engine, the SCSP DSP,
//! the mixer/DAC, and the timer/sound interrupt sources (which feed the 68k's
//! `raise_interrupt`) are the audio milestone (M6).

use crate::memory::Ram;
use m68k::bus::{AccessKind, Bus};

pub const SOUND_RAM_BYTES: usize = 512 * 1024;
pub const REG_BYTES: usize = 0x1000;

/// SCSP M68K clock — 11.2896 MHz (the sound 68k runs at half the 22.5792 MHz
/// SCSP master clock). Used to pace the 68k against the SH-2 cycle stream.
pub const SCSP_CLOCK_HZ: u64 = 11_289_600;
const SH2_CLOCK_HZ: u64 = 28_636_360;

/// Flat SCSP register bank (control + 32 slots + DSP), big-endian, mirrored.
#[derive(Clone, Debug)]
pub struct ScspRegs {
    raw: [u8; REG_BYTES],
}

impl Default for ScspRegs {
    fn default() -> Self {
        Self::new()
    }
}

impl ScspRegs {
    pub fn new() -> Self {
        Self {
            raw: [0; REG_BYTES],
        }
    }
    #[inline]
    fn idx(o: u32) -> usize {
        (o as usize) & (REG_BYTES - 1)
    }
    pub fn read8(&self, o: u32) -> u8 {
        self.raw[Self::idx(o)]
    }
    pub fn read16(&self, o: u32) -> u16 {
        u16::from_be_bytes([self.read8(o), self.read8(o + 1)])
    }
    pub fn read32(&self, o: u32) -> u32 {
        ((self.read16(o) as u32) << 16) | self.read16(o + 2) as u32
    }
    pub fn write8(&mut self, o: u32, v: u8) {
        self.raw[Self::idx(o)] = v;
    }
    pub fn write16(&mut self, o: u32, v: u16) {
        let b = v.to_be_bytes();
        self.write8(o, b[0]);
        self.write8(o + 1, b[1]);
    }
    pub fn write32(&mut self, o: u32, v: u32) {
        self.write16(o, (v >> 16) as u16);
        self.write16(o + 2, v as u16);
    }
}

#[derive(Clone, Debug)]
pub struct Scsp {
    /// 512 KiB sound RAM, shared between the SH-2 (at 0x05A0_0000) and the 68k.
    pub ram: Ram,
    /// Control + slot + DSP registers.
    pub regs: ScspRegs,
    /// The hosted sound CPU.
    pub cpu: m68k::Cpu,
    /// True once the 68k is released from reset (SMPC `SNDON`).
    pub running: bool,
    /// Sub-SH-2-cycle accumulator for the SH-2 → 68k clock conversion.
    frac: u64,
}

impl Default for Scsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Scsp {
    pub fn new() -> Self {
        Self {
            ram: Ram::new(SOUND_RAM_BYTES),
            regs: ScspRegs::new(),
            cpu: m68k::Cpu::new(),
            running: false,
            frac: 0,
        }
    }

    /// Release the 68k from reset: reload SSP/PC from the sound-RAM vector
    /// table and start running (SMPC `SNDON`).
    pub fn start(&mut self) {
        {
            let Scsp { ram, regs, cpu, .. } = &mut *self;
            let mut bus = M68kView { ram, regs };
            cpu.reset(&mut bus);
        }
        self.running = true;
    }

    /// Re-hold the 68k in reset (SMPC `SNDOFF`).
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Raise an interrupt line into the sound 68k (timer/sound sources will
    /// call this once the audio engine lands).
    pub fn raise_interrupt(&mut self, level: u8) {
        self.cpu.raise_interrupt(level);
    }

    /// Advance the 68k by the share of `sh2_cycles` its slower clock earns.
    /// No-op while the 68k is held in reset.
    pub fn run(&mut self, sh2_cycles: u64) {
        if !self.running {
            return;
        }
        self.frac += sh2_cycles.saturating_mul(SCSP_CLOCK_HZ);
        let mut budget = (self.frac / SH2_CLOCK_HZ) as i64;
        self.frac %= SH2_CLOCK_HZ;

        let Scsp { ram, regs, cpu, .. } = &mut *self;
        let mut bus = M68kView { ram, regs };
        while budget > 0 {
            let c = cpu.step(&mut bus) as i64;
            budget -= c.max(1); // never spin on a zero-cost step
        }
    }
}

/// The 68k's memory view: sound RAM mirrored over `0x00_0000..0x0F_FFFF`, the
/// SCSP registers at `0x10_0000..0x10_0FFF`, open bus elsewhere.
struct M68kView<'a> {
    ram: &'a mut Ram,
    regs: &'a mut ScspRegs,
}

impl M68kView<'_> {
    #[inline]
    fn is_reg(addr: u32) -> bool {
        (0x10_0000..0x10_1000).contains(&addr)
    }
}

impl Bus for M68kView<'_> {
    fn read8(&mut self, addr: u32, _: AccessKind) -> (u8, u32) {
        if Self::is_reg(addr) {
            (self.regs.read8(addr - 0x10_0000), 0)
        } else if addr < 0x10_0000 {
            (self.ram.read8(addr), 0)
        } else {
            (0, 0)
        }
    }
    fn read16(&mut self, addr: u32, _: AccessKind) -> (u16, u32) {
        if Self::is_reg(addr) {
            (self.regs.read16(addr - 0x10_0000), 0)
        } else if addr < 0x10_0000 {
            (self.ram.read16(addr), 0)
        } else {
            (0, 0)
        }
    }
    fn read32(&mut self, addr: u32, _: AccessKind) -> (u32, u32) {
        if Self::is_reg(addr) {
            (self.regs.read32(addr - 0x10_0000), 0)
        } else if addr < 0x10_0000 {
            (self.ram.read32(addr), 0)
        } else {
            (0, 0)
        }
    }
    fn write8(&mut self, addr: u32, val: u8, _: AccessKind) -> u32 {
        if Self::is_reg(addr) {
            self.regs.write8(addr - 0x10_0000, val);
        } else if addr < 0x10_0000 {
            self.ram.write8(addr, val);
        }
        0
    }
    fn write16(&mut self, addr: u32, val: u16, _: AccessKind) -> u32 {
        if Self::is_reg(addr) {
            self.regs.write16(addr - 0x10_0000, val);
        } else if addr < 0x10_0000 {
            self.ram.write16(addr, val);
        }
        0
    }
    fn write32(&mut self, addr: u32, val: u32, _: AccessKind) -> u32 {
        if Self::is_reg(addr) {
            self.regs.write32(addr - 0x10_0000, val);
        } else if addr < 0x10_0000 {
            self.ram.write32(addr, val);
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_round_trip_and_mirror() {
        let mut s = Scsp::new();
        s.regs.write16(0x400, 0xBEEF);
        assert_eq!(s.regs.read16(0x400), 0xBEEF);
        // 0x1000-byte window mirrors.
        assert_eq!(s.regs.read16(0x400 + 0x1000), 0xBEEF);
    }

    #[test]
    fn held_in_reset_until_started() {
        let mut s = Scsp::new();
        assert!(!s.running);
        s.run(10_000); // no-op while held
        assert_eq!(s.cpu.regs.pc, 0);
    }

    #[test]
    fn start_loads_vectors_and_runs_a_program() {
        let mut s = Scsp::new();
        // Vector table in sound RAM: SSP, then PC.
        s.ram.write32(0, 0x0001_0000); // initial SSP
        s.ram.write32(4, 0x0000_2000); // initial PC
        // A tiny program at 0x2000: MOVEQ #7, D0 (0x7007) then BRA-self.
        s.ram.write16(0x2000, 0x7007);
        s.ram.write16(0x2002, 0x60FE); // BRA *-0 (tight loop)
        s.start();
        assert!(s.running);
        assert_eq!(s.cpu.regs.a[7], 0x0001_0000, "SSP from RAM vector");
        assert_eq!(s.cpu.regs.pc, 0x2000, "PC from RAM vector");
        // Run enough SH-2 cycles for the 68k to execute MOVEQ.
        s.run(2_000);
        assert_eq!(
            s.cpu.regs.d[0], 7,
            "68k executed its program from sound RAM"
        );
    }

    #[test]
    fn stop_re_holds_the_cpu() {
        let mut s = Scsp::new();
        s.ram.write32(4, 0x0000_2000);
        s.ram.write16(0x2000, 0x60FE); // BRA self
        s.start();
        s.run(2_000);
        let pc = s.cpu.regs.pc;
        s.stop();
        s.run(10_000); // ignored
        assert_eq!(s.cpu.regs.pc, pc, "halted 68k does not advance");
    }

    #[test]
    fn cpu_and_host_share_sound_ram_through_the_register_view() {
        // The 68k sees registers at 0x100000; a write there lands in regs.
        let mut s = Scsp::new();
        let Scsp { ram, regs, .. } = &mut s;
        let mut view = M68kView { ram, regs };
        view.write16(0x10_0402, 0x1234, AccessKind::Data);
        assert_eq!(s.regs.read16(0x402), 0x1234);
    }
}
