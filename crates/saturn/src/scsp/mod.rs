//! Saturn Custom Sound Processor (SCSP) — host wiring + timers/interrupts.
//!
//! The SCSP owns the 512 KiB sound RAM, its control/slot/DSP register bank,
//! and the hosted [`m68k::Cpu`] sound CPU. The main SH-2 sees the sound RAM at
//! `0x05A0_0000` and the registers at `0x05B0_0000`; the 68k — driven by the
//! Saturn scheduler — sees the same RAM at `0x00_0000` and the registers at
//! `0x10_0000`, running the sound program the main CPU stages into RAM.
//!
//! On power-on the 68k is held in reset; SMPC `SNDON` releases it and `SNDOFF`
//! re-holds it.
//!
//! **Timers + interrupts (M6, increment 1):** three programmable timers
//! (A/B/C) tick at the sample clock ÷ 2^prescale; on overflow they set their
//! `SCIPD` pending bit, raising the 68k's interrupt line at the level encoded
//! by `SCILV0..2` (gated by `SCIEB`). Timer A also pends the *main-CPU* sound
//! interrupt (`MCIPD`/`MCIEB`), which the aggregate forwards to the SCU. This
//! is what makes the hosted 68k an interrupt-driven sound engine.
//!
//! Still to come (M6): the slot/FM synthesis engine, the SCSP DSP, the
//! mixer/DAC, MIDI, and SDL2 audio output.

use crate::memory::Ram;
use m68k::bus::{AccessKind, Bus};

pub const SOUND_RAM_BYTES: usize = 512 * 1024;
pub const REG_BYTES: usize = 0x1000;

/// SCSP M68K clock — 11.2896 MHz (half the 22.5792 MHz SCSP master clock).
pub const SCSP_CLOCK_HZ: u64 = 11_289_600;
/// SCSP sample clock — 44.1 kHz (master clock ÷ 512), driving the timers.
pub const SCSP_SAMPLE_HZ: u64 = 44_100;
const SH2_CLOCK_HZ: u64 = 28_636_360;

// Control-register byte offsets within the 0x1000 register space.
const TIMA: u32 = 0x418;
const TIMB: u32 = 0x41A;
const TIMC: u32 = 0x41C;
const SCIEB: u32 = 0x41E;
const SCIPD: u32 = 0x420;
const SCIRE: u32 = 0x422;
const SCILV0: u32 = 0x424;
const SCILV1: u32 = 0x426;
const SCILV2: u32 = 0x428;
const MCIEB: u32 = 0x42A;
const MCIPD: u32 = 0x42C;
const MCIRE: u32 = 0x42E;

// Interrupt-source bits shared by SCIEB/SCIPD and MCIEB/MCIPD.
const INT_MIDI: u16 = 0x008; // bit 3
const INT_TIMER_A: u16 = 0x040; // bit 6
const INT_TIMER_B: u16 = 0x080; // bit 7
const INT_TIMER_C: u16 = 0x100; // bit 8

pub const NUM_SLOTS: usize = 32;
/// Slot-register block: 32 slots × 0x20 bytes at the base of the register space.
const SLOT_STRIDE: u32 = 0x20;
/// Phase fractional bits (the SCSP's address accumulator is 12.12-ish: the top
/// bits index the waveform sample, the low `SHIFT` bits interpolate).
const PHASE_SHIFT: u32 = 12;
const SOUND_RAM_MASK: u32 = (SOUND_RAM_BYTES as u32) - 1;

/// One PCM slot's runtime state (the configuration lives in the register bank;
/// this is the per-sample playback state set up at key-on).
#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    active: bool,
    backwards: bool,
    /// Phase accumulator: `cur >> PHASE_SHIFT` is the sample index past SA.
    cur: u32,
    nxt: u32,
    /// Phase increment per output sample (from OCT/FNS).
    step: u32,
}

/// One SCSP timer: an 8-bit up-counter incremented every `2^prescale` samples.
#[derive(Clone, Debug, Default)]
struct Timer {
    count: u16,
    subtick: u32,
    last_reg: u16,
}

impl Timer {
    /// Advance by `samples`; returns true on each overflow (8-bit wrap). A
    /// register rewrite reloads the counter from the new `TIMx` value.
    fn tick(&mut self, reg: u16, samples: u32) -> bool {
        if reg != self.last_reg {
            self.last_reg = reg;
            self.count = reg & 0xFF;
            self.subtick = 0;
        }
        let prescale = 1u32 << ((reg >> 8) & 7);
        self.subtick += samples;
        let mut overflowed = false;
        while self.subtick >= prescale {
            self.subtick -= prescale;
            self.count += 1;
            if self.count > 0xFF {
                self.count = reg & 0xFF;
                overflowed = true;
            }
        }
        overflowed
    }
}

/// SCSP control + slot + DSP registers, with timer state and the derived
/// interrupt lines. Register reads are plain; writes to the interrupt-control
/// window have side effects (pending/reset/recompute).
#[derive(Clone, Debug)]
pub struct ScspCtrl {
    raw: [u8; REG_BYTES],
    timers: [Timer; 3],
    slots: [Slot; NUM_SLOTS],
    /// Current 68k interrupt-line level (0 = none); level-triggered.
    asserted_level: u8,
    /// Main-CPU sound interrupt pending (forwarded to the SCU).
    main_pending: bool,
}

impl Default for ScspCtrl {
    fn default() -> Self {
        Self::new()
    }
}

impl ScspCtrl {
    pub fn new() -> Self {
        Self {
            raw: [0; REG_BYTES],
            timers: Default::default(),
            slots: [Slot::default(); NUM_SLOTS],
            asserted_level: 0,
            main_pending: false,
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

    /// Store a 16-bit register without running write side effects.
    fn store16(&mut self, o: u32, v: u16) {
        let b = v.to_be_bytes();
        self.raw[Self::idx(o)] = b[0];
        self.raw[Self::idx(o + 1)] = b[1];
    }

    pub fn write8(&mut self, o: u32, v: u8) {
        // Fold a byte write into the containing 16-bit register so the side
        // effects see the full value.
        let aligned = o & !1;
        let cur = self.read16(aligned);
        let nv = if o & 1 == 0 {
            (cur & 0x00FF) | ((v as u16) << 8)
        } else {
            (cur & 0xFF00) | v as u16
        };
        self.write16(aligned, nv);
    }
    pub fn write16(&mut self, o: u32, v: u16) {
        self.store16(o, v);
        // A write touching a slot's first word (data[0]) with KYONEX set
        // executes key-on/off across all slots.
        if o < NUM_SLOTS as u32 * SLOT_STRIDE
            && (o & (SLOT_STRIDE - 1)) <= 1
            && self.read16(o & !(SLOT_STRIDE - 1)) & 0x1000 != 0
        {
            self.key_on_execute();
        }
        match o & !1 {
            SCIRE => {
                // Clear the written pending bits, then re-evaluate.
                let cleared = self.read16(SCIPD) & !v;
                self.store16(SCIPD, cleared);
                self.recompute_irq();
            }
            MCIRE => {
                let cleared = self.read16(MCIPD) & !v;
                self.store16(MCIPD, cleared);
                self.recompute_main();
            }
            SCIEB | SCIPD | SCILV0 | SCILV1 | SCILV2 => self.recompute_irq(),
            MCIEB | MCIPD => self.recompute_main(),
            _ => {}
        }
    }
    pub fn write32(&mut self, o: u32, v: u32) {
        self.write16(o, (v >> 16) as u16);
        self.write16(o + 2, v as u16);
    }

    /// Advance the three timers by `samples`, pending interrupts on overflow.
    fn tick_timers(&mut self, samples: u32) {
        if samples == 0 {
            return;
        }
        let regs = [self.read16(TIMA), self.read16(TIMB), self.read16(TIMC)];
        let bits = [INT_TIMER_A, INT_TIMER_B, INT_TIMER_C];
        let mut scipd = false;
        let mut mcipd = false;
        for i in 0..3 {
            if self.timers[i].tick(regs[i], samples) {
                self.store16(SCIPD, self.read16(SCIPD) | bits[i]);
                scipd = true;
                if i == 0 {
                    // Timer A also pends the main-CPU sound interrupt.
                    self.store16(MCIPD, self.read16(MCIPD) | INT_TIMER_A);
                    mcipd = true;
                }
            }
        }
        if scipd {
            self.recompute_irq();
        }
        if mcipd {
            self.recompute_main();
        }
    }

    /// The 68k interrupt level for source bit `bit`, assembled from SCILV0..2.
    fn decode_sci(&self, bit: u32) -> u8 {
        let g = |off: u32| ((self.read16(off) >> bit) & 1) as u8;
        g(SCILV0) | (g(SCILV1) << 1) | (g(SCILV2) << 2)
    }

    /// Recompute the asserted 68k IRQ level from pending & enabled sources.
    fn recompute_irq(&mut self) {
        let active = self.read16(SCIPD) & self.read16(SCIEB);
        self.asserted_level = if active & INT_TIMER_A != 0 {
            self.decode_sci(6)
        } else if active & INT_TIMER_B != 0 {
            self.decode_sci(7)
        } else if active & INT_TIMER_C != 0 {
            self.decode_sci(8)
        } else if active & INT_MIDI != 0 {
            self.decode_sci(3)
        } else {
            0
        };
    }

    fn recompute_main(&mut self) {
        self.main_pending = self.read16(MCIPD) & self.read16(MCIEB) != 0;
    }

    // ---- slot (PCM) engine --------------------------------------------

    /// Slot `i`'s register word `k` (data[k] in the SCSP slot layout).
    fn slot_reg(&self, i: usize, k: u32) -> u16 {
        self.read16(i as u32 * SLOT_STRIDE + k * 2)
    }

    /// Phase increment for slot `i` from OCT (signed octave) and FNS, per the
    /// SCSP pitch formula: `(FNS + 0x400) << (oct + PHASE_SHIFT - 10)`.
    fn slot_step(&self, i: usize) -> u32 {
        let reg = self.slot_reg(i, 0x8);
        let oct = (reg >> 11) & 0xF;
        let fns = (reg & 0x3FF) as u32;
        let octave = ((oct ^ 8) as i32 - 8) + PHASE_SHIFT as i32 - 10;
        let mut fnv = fns + (1 << 10);
        if octave >= 0 {
            fnv <<= octave;
        } else {
            fnv >>= -octave;
        }
        fnv
    }

    /// Execute key-on/off for all slots based on each one's KYONB bit, then
    /// clear the KYONEX strobe wherever it's set.
    fn key_on_execute(&mut self) {
        for i in 0..NUM_SLOTS {
            let data0 = self.slot_reg(i, 0);
            if data0 & 0x0800 != 0 {
                self.start_slot(i);
            } else {
                self.slots[i].active = false;
            }
            // Clear KYONEX (bit 12) so the strobe is one-shot.
            if data0 & 0x1000 != 0 {
                self.store16(i as u32 * SLOT_STRIDE, data0 & !0x1000);
            }
        }
    }

    fn start_slot(&mut self, i: usize) {
        let step = self.slot_step(i);
        self.slots[i] = Slot {
            active: true,
            backwards: false,
            cur: 0,
            nxt: 1 << PHASE_SHIFT,
            step,
        };
    }

    pub fn slot_active(&self, i: usize) -> bool {
        self.slots[i].active
    }

    /// Produce one output sample (signed 16-bit, pre-envelope) for slot `i`,
    /// advancing its phase and applying the loop mode. Reads waveform data
    /// from `ram` (the SCSP sound RAM). Returns 0 for an inactive slot.
    pub fn slot_sample(&mut self, i: usize, ram: &Ram) -> i16 {
        if !self.slots[i].active {
            return 0;
        }
        let data0 = self.slot_reg(i, 0);
        let pcm8 = data0 & 0x0010 != 0;
        let lpctl = (data0 >> 5) & 3;
        let sa = ((data0 as u32 & 0xF) << 16) | self.slot_reg(i, 1) as u32;
        let lsa = self.slot_reg(i, 2) as u32;
        let lea = self.slot_reg(i, 3) as u32;

        let (cur, nxt) = (self.slots[i].cur, self.slots[i].nxt);
        let frac = (cur & ((1 << PHASE_SHIFT) - 1)) as i32;
        let one = 1i32 << PHASE_SHIFT;
        // Linearly interpolate the current and next waveform samples.
        let (p1, p2) = if pcm8 {
            let a1 = cur >> PHASE_SHIFT;
            let a2 = nxt >> PHASE_SHIFT;
            (
                ((ram.read8((sa + a1) & SOUND_RAM_MASK) as i8 as i32) << 8),
                ((ram.read8((sa + a2) & SOUND_RAM_MASK) as i8 as i32) << 8),
            )
        } else {
            let a1 = (cur >> (PHASE_SHIFT - 1)) & !1;
            let a2 = (nxt >> (PHASE_SHIFT - 1)) & !1;
            (
                ram.read16((sa + a1) & SOUND_RAM_MASK) as i16 as i32,
                ram.read16((sa + a2) & SOUND_RAM_MASK) as i16 as i32,
            )
        };
        let sample = ((p1 * (one - frac) + p2 * frac) >> PHASE_SHIFT) as i16;

        // Advance the phase and re-derive the next-sample address.
        let slot = &mut self.slots[i];
        if slot.backwards {
            slot.cur = slot.cur.wrapping_sub(slot.step);
        } else {
            slot.cur = slot.cur.wrapping_add(slot.step);
        }
        slot.nxt = slot.cur.wrapping_add(1 << PHASE_SHIFT);

        // Loop handling on the new current address (sample index).
        let addr = slot.cur >> PHASE_SHIFT;
        match lpctl {
            0 => {
                if addr >= lsa && addr >= lea {
                    slot.active = false;
                }
            }
            1 => {
                if addr >= lea {
                    slot.cur = (lsa << PHASE_SHIFT) + (slot.cur - (lea << PHASE_SHIFT));
                    slot.nxt = slot.cur + (1 << PHASE_SHIFT);
                }
            }
            2 => {
                // Reverse loop.
                if addr >= lsa && !slot.backwards {
                    slot.cur = (lea << PHASE_SHIFT) - (slot.cur - (lsa << PHASE_SHIFT));
                    slot.backwards = true;
                } else if slot.backwards && (addr < lsa || slot.cur & 0x8000_0000 != 0) {
                    slot.cur = (lea << PHASE_SHIFT) - ((lsa << PHASE_SHIFT).wrapping_sub(slot.cur));
                }
                slot.nxt = slot.cur.wrapping_add(1 << PHASE_SHIFT);
            }
            _ => {
                // Ping-pong (alternating) loop.
                if addr >= lea && !slot.backwards {
                    slot.cur = (lea << PHASE_SHIFT) - (slot.cur - (lea << PHASE_SHIFT));
                    slot.backwards = true;
                } else if slot.backwards && (addr < lsa || slot.cur & 0x8000_0000 != 0) {
                    slot.cur = (lsa << PHASE_SHIFT) + ((lsa << PHASE_SHIFT).wrapping_sub(slot.cur));
                    slot.backwards = false;
                }
                slot.nxt = slot.cur.wrapping_add(1 << PHASE_SHIFT);
            }
        }
        sample
    }
}

#[derive(Clone, Debug)]
pub struct Scsp {
    /// 512 KiB sound RAM, shared between the SH-2 (at 0x05A0_0000) and the 68k.
    pub ram: Ram,
    /// Control + slot + DSP registers, timers, and interrupt state.
    pub ctrl: ScspCtrl,
    /// The hosted sound CPU.
    pub cpu: m68k::Cpu,
    /// True once the 68k is released from reset (SMPC `SNDON`).
    pub running: bool,
    /// Sub-SH-2-cycle accumulators for the 68k-clock and sample-clock rates.
    frac: u64,
    sample_frac: u64,
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
            ctrl: ScspCtrl::new(),
            cpu: m68k::Cpu::new(),
            running: false,
            frac: 0,
            sample_frac: 0,
        }
    }

    /// Release the 68k from reset (SMPC `SNDON`): reload SSP/PC from the
    /// sound-RAM vectors and start running.
    pub fn start(&mut self) {
        {
            let Scsp { ram, ctrl, cpu, .. } = &mut *self;
            let mut bus = M68kView { ram, ctrl };
            cpu.reset(&mut bus);
        }
        self.running = true;
    }

    /// Re-hold the 68k in reset (SMPC `SNDOFF`).
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Whether PCM slot `i` is currently playing.
    pub fn slot_active(&self, i: usize) -> bool {
        self.ctrl.slot_active(i)
    }

    /// One output sample (pre-envelope) for slot `i`, reading the shared sound
    /// RAM and advancing the slot's phase. The mixer (M6 task #4) sums these.
    pub fn slot_sample(&mut self, i: usize) -> i16 {
        let Scsp { ctrl, ram, .. } = self;
        ctrl.slot_sample(i, ram)
    }

    /// Pop the main-CPU sound interrupt (the aggregate forwards it to the SCU
    /// `SoundRequest` source). Stays asserted while `MCIPD & MCIEB` holds.
    pub fn take_main_interrupt(&mut self) -> bool {
        self.ctrl.main_pending
    }

    /// Advance the timers and the 68k by the share of `sh2_cycles` the SCSP's
    /// clocks earn. No-op while the 68k is held in reset.
    pub fn run(&mut self, sh2_cycles: u64) {
        if !self.running {
            return;
        }
        // Sample clock → timers.
        self.sample_frac += sh2_cycles.saturating_mul(SCSP_SAMPLE_HZ);
        let samples = (self.sample_frac / SH2_CLOCK_HZ) as u32;
        self.sample_frac %= SH2_CLOCK_HZ;
        self.ctrl.tick_timers(samples);

        // 68k clock → instruction stepping.
        self.frac += sh2_cycles.saturating_mul(SCSP_CLOCK_HZ);
        let mut budget = (self.frac / SH2_CLOCK_HZ) as i64;
        self.frac %= SH2_CLOCK_HZ;

        let Scsp { ram, ctrl, cpu, .. } = &mut *self;
        while budget > 0 {
            // Present the level-triggered SCSP IRQ line at each boundary.
            cpu.pending_irq = ctrl.asserted_level;
            let mut bus = M68kView {
                ram: &mut *ram,
                ctrl: &mut *ctrl,
            };
            budget -= (cpu.step(&mut bus) as i64).max(1);
        }
    }
}

/// The 68k's memory view: sound RAM over `0x00_0000..0x0F_FFFF`, the SCSP
/// registers at `0x10_0000..0x10_0FFF`, open bus elsewhere.
struct M68kView<'a> {
    ram: &'a mut Ram,
    ctrl: &'a mut ScspCtrl,
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
            (self.ctrl.read8(addr - 0x10_0000), 0)
        } else if addr < 0x10_0000 {
            (self.ram.read8(addr), 0)
        } else {
            (0, 0)
        }
    }
    fn read16(&mut self, addr: u32, _: AccessKind) -> (u16, u32) {
        if Self::is_reg(addr) {
            (self.ctrl.read16(addr - 0x10_0000), 0)
        } else if addr < 0x10_0000 {
            (self.ram.read16(addr), 0)
        } else {
            (0, 0)
        }
    }
    fn read32(&mut self, addr: u32, _: AccessKind) -> (u32, u32) {
        if Self::is_reg(addr) {
            (self.ctrl.read32(addr - 0x10_0000), 0)
        } else if addr < 0x10_0000 {
            (self.ram.read32(addr), 0)
        } else {
            (0, 0)
        }
    }
    fn write8(&mut self, addr: u32, val: u8, _: AccessKind) -> u32 {
        if Self::is_reg(addr) {
            self.ctrl.write8(addr - 0x10_0000, val);
        } else if addr < 0x10_0000 {
            self.ram.write8(addr, val);
        }
        0
    }
    fn write16(&mut self, addr: u32, val: u16, _: AccessKind) -> u32 {
        if Self::is_reg(addr) {
            self.ctrl.write16(addr - 0x10_0000, val);
        } else if addr < 0x10_0000 {
            self.ram.write16(addr, val);
        }
        0
    }
    fn write32(&mut self, addr: u32, val: u32, _: AccessKind) -> u32 {
        if Self::is_reg(addr) {
            self.ctrl.write32(addr - 0x10_0000, val);
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
        s.ctrl.write16(0x010, 0xBEEF); // slot register (no side effect)
        assert_eq!(s.ctrl.read16(0x010), 0xBEEF);
        assert_eq!(s.ctrl.read16(0x010 + 0x1000), 0xBEEF, "1000-byte mirror");
    }

    #[test]
    fn held_in_reset_until_started() {
        let mut s = Scsp::new();
        assert!(!s.running);
        s.run(10_000);
        assert_eq!(s.cpu.regs.pc, 0);
    }

    #[test]
    fn start_loads_vectors_and_runs_a_program() {
        let mut s = Scsp::new();
        s.ram.write32(0, 0x0001_0000); // SSP
        s.ram.write32(4, 0x0000_2000); // PC
        s.ram.write16(0x2000, 0x7007); // MOVEQ #7, D0
        s.ram.write16(0x2002, 0x60FE); // BRA self
        s.start();
        assert_eq!(s.cpu.regs.a[7], 0x0001_0000);
        assert_eq!(s.cpu.regs.pc, 0x2000);
        s.run(2_000);
        assert_eq!(s.cpu.regs.d[0], 7, "68k ran from sound RAM");
    }

    #[test]
    fn timer_a_overflow_raises_the_68k_interrupt_line() {
        let mut ctrl = ScspCtrl::new();
        // Route timer A (bit 6) to 68k level 4: SCILV2 bit6 set (4 = 100b).
        ctrl.write16(SCILV2, INT_TIMER_A);
        ctrl.write16(SCIEB, INT_TIMER_A); // enable timer A
        // Prescale 0 (÷1), TIMx = 0xFF → overflows after a single sample.
        ctrl.write16(TIMA, 0x00FF);
        ctrl.tick_timers(2);
        assert_eq!(ctrl.read16(SCIPD) & INT_TIMER_A, INT_TIMER_A, "pending set");
        assert_eq!(
            ctrl.asserted_level, 4,
            "IRQ line at the SCILV-encoded level"
        );
    }

    #[test]
    fn disabled_timer_interrupt_does_not_assert() {
        let mut ctrl = ScspCtrl::new();
        ctrl.write16(SCILV0, INT_TIMER_A); // level 1
        ctrl.write16(TIMA, 0x00FF);
        // SCIEB left 0 → masked.
        ctrl.tick_timers(2);
        assert_eq!(ctrl.asserted_level, 0, "masked source does not assert");
    }

    #[test]
    fn scire_clears_pending_and_drops_the_line() {
        let mut ctrl = ScspCtrl::new();
        ctrl.write16(SCILV0, INT_TIMER_A);
        ctrl.write16(SCIEB, INT_TIMER_A);
        ctrl.write16(TIMA, 0x00FF);
        ctrl.tick_timers(2);
        assert_ne!(ctrl.asserted_level, 0);
        ctrl.write16(SCIRE, INT_TIMER_A); // acknowledge
        assert_eq!(ctrl.read16(SCIPD) & INT_TIMER_A, 0, "pending cleared");
        assert_eq!(ctrl.asserted_level, 0, "line dropped");
    }

    #[test]
    fn timer_a_pends_the_main_cpu_sound_interrupt() {
        let mut ctrl = ScspCtrl::new();
        ctrl.write16(MCIEB, INT_TIMER_A); // enable main-CPU timer-A interrupt
        ctrl.write16(TIMA, 0x00FF);
        assert!(!ctrl.main_pending);
        ctrl.tick_timers(2);
        assert!(ctrl.main_pending, "MCIPD & MCIEB → main interrupt");
    }

    // ---- slot (PCM) engine ----

    /// Key on slot 0 with the given config words, after planting params.
    /// `data0_flags` carries PCM8B/LPCTL/SA-hi; KYONB|KYONEX are added here.
    fn keyon_slot0(s: &mut Scsp, data0_flags: u16, sa: u32, lsa: u16, lea: u16, octfns: u16) {
        s.ctrl.write16(0x02, sa as u16); // SA low
        s.ctrl.write16(0x04, lsa); // LSA
        s.ctrl.write16(0x06, lea); // LEA
        s.ctrl.write16(0x10, octfns); // OCT/FNS
        // data[0]: KYONEX|KYONB | flags | SA high nibble.
        let hi = ((sa >> 16) & 0xF) as u16;
        s.ctrl.write16(0x00, 0x1800 | data0_flags | hi);
    }

    #[test]
    fn key_on_activates_the_slot() {
        let mut s = Scsp::new();
        assert!(!s.slot_active(0));
        keyon_slot0(&mut s, 0, 0x100, 0, 0x100, 0);
        assert!(s.slot_active(0), "KYONEX with KYONB started the slot");
    }

    #[test]
    fn slot_plays_16bit_pcm_at_native_rate() {
        let mut s = Scsp::new();
        s.ram.write16(0x100, 0x1111);
        s.ram.write16(0x102, 0x2222);
        s.ram.write16(0x104, 0x3333);
        keyon_slot0(&mut s, 0, 0x100, 0, 0x100, 0); // 16-bit, OCT/FNS 0 → 1:1
        assert_eq!(s.slot_sample(0), 0x1111);
        assert_eq!(s.slot_sample(0), 0x2222);
        assert_eq!(s.slot_sample(0), 0x3333);
    }

    #[test]
    fn slot_plays_8bit_pcm_scaled_to_16() {
        let mut s = Scsp::new();
        s.ram.write8(0x200, 0x40); // +64 → 0x4000
        s.ram.write8(0x201, 0xC0); // -64 → 0xC000 (sign-extended << 8)
        keyon_slot0(&mut s, 0x0010, 0x200, 0, 0x100, 0); // PCM8B (bit 4)
        assert_eq!(s.slot_sample(0), 0x4000);
        assert_eq!(s.slot_sample(0), (0xC000u16) as i16);
    }

    #[test]
    fn pitch_octave_doubles_the_playback_rate() {
        let mut s = Scsp::new();
        for n in 0..4u32 {
            s.ram.write16(0x100 + n * 2, (n as u16) * 0x1000);
        }
        // OCT = 1 → step doubles → skips every other sample (0, 2, ...).
        keyon_slot0(&mut s, 0, 0x100, 0, 0x100, 1 << 11);
        assert_eq!(s.slot_sample(0), 0x0000);
        assert_eq!(s.slot_sample(0), 0x2000);
    }

    #[test]
    fn normal_loop_wraps_at_lea_back_to_lsa() {
        let mut s = Scsp::new();
        s.ram.write16(0x100, 0x0A0A);
        s.ram.write16(0x102, 0x0B0B);
        s.ram.write16(0x104, 0x0C0C);
        // LSA = 1, LEA = 3 → after sample index 2, wrap to index 1.
        keyon_slot0(&mut s, 0x0020, 0x100, 1, 3, 0); // LPCTL = 1 (bits 6:5)
        assert_eq!(s.slot_sample(0), 0x0A0A); // idx 0
        assert_eq!(s.slot_sample(0), 0x0B0B); // idx 1
        assert_eq!(s.slot_sample(0), 0x0C0C); // idx 2 → wrap to 1
        assert_eq!(s.slot_sample(0), 0x0B0B, "looped back to LSA");
    }

    #[test]
    fn no_loop_stops_at_lea() {
        let mut s = Scsp::new();
        s.ram.write16(0x100, 0x7777);
        keyon_slot0(&mut s, 0, 0x100, 0, 2, 0); // LEA = 2, LPCTL = 0
        s.slot_sample(0); // idx 0
        s.slot_sample(0); // idx 1 → addr reaches 2 (>= LEA) → stop
        assert!(!s.slot_active(0), "no-loop slot stops at LEA");
    }

    #[test]
    fn key_off_silences_the_slot() {
        let mut s = Scsp::new();
        keyon_slot0(&mut s, 0, 0x100, 0, 0x100, 0);
        assert!(s.slot_active(0));
        // Clear KYONB, strobe KYONEX → key off.
        s.ctrl.write16(0x00, 0x1000);
        assert!(!s.slot_active(0));
        assert_eq!(s.slot_sample(0), 0, "silent once keyed off");
    }

    #[test]
    fn running_68k_takes_a_timer_interrupt() {
        let mut s = Scsp::new();
        // The 68k boots with imask = 7; the program first lowers it (MOVE
        // #0x2000,SR → supervisor, mask 0) so the level-4 timer interrupt can
        // be taken — then imask = 4 keeps the handler from re-entering itself.
        s.ram.write32(0, 0x0001_0000); // SSP
        s.ram.write32(4, 0x0000_2000); // PC
        s.ram.write32(28 * 4, 0x0000_3000); // level-4 autovector
        s.ram.write16(0x2000, 0x46FC); // MOVE #imm, SR
        s.ram.write16(0x2002, 0x2000); //   imm: supervisor, mask 0
        s.ram.write16(0x2004, 0x60FE); // main loop: BRA self
        s.ram.write16(0x3000, 0x7A55); // handler: MOVEQ #0x55, D5
        s.ram.write16(0x3002, 0x4E73); // RTE
        // Timer A → level 4 (SCILV2 bit 6), enabled, fast overflow.
        s.ctrl.write16(SCILV2, INT_TIMER_A);
        s.ctrl.write16(SCIEB, INT_TIMER_A);
        s.ctrl.write16(TIMA, 0x00FF);
        s.start();
        // Run enough to accrue a sample (so the timer overflows) + steps.
        s.run(200_000);
        assert_eq!(s.cpu.regs.d[5], 0x55, "68k serviced the timer interrupt");
    }
}
