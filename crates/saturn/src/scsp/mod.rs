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
//! **Slots + envelope (M6, increments 2–3):** 32 PCM slots play waveforms
//! from sound RAM (OCT/FNS pitch, 8/16-bit, interpolation, the four loop
//! modes); each has an ADSR envelope generator (rate-scaled attack/decay/
//! release + decay level) whose log output, scaled by TL, multiplies the
//! slot. `slot_sample` yields the raw PCM and `eg_advance` the EG×TL
//! multiplier — the mixer (next) pairs them and pans to L/R.
//!
//! Still to come (M6): the mixer/DAC, SDL2 audio output, the SCSP DSP, MIDI.

mod dsp;

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

/// Envelope-volume fixed-point shift (the EG counter is `0..0x3FF << EG_SHIFT`).
const EG_SHIFT: u32 = 16;

/// The four ADSR phases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EgState {
    Attack,
    Decay1,
    Decay2,
    #[default]
    Release,
}

/// Per-slot envelope-generator state. Rates are cached at key-on (they depend
/// on OCT/KRS); `volume` is the log loudness index (`0` silent … `0x3FF` full).
#[derive(Clone, Copy, Debug, Default)]
struct Eg {
    state: EgState,
    volume: i32,
    ar: i32,
    d1r: i32,
    d2r: i32,
    rr: i32,
    dl: i32,
    eghold: bool,
}

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
    eg: Eg,
}

/// Attack / decay envelope time constants (ms) for each of the 64 rates
/// (SCSP/YMF292 tables, via MAME's `scsp.cpp`).
#[rustfmt::skip]
const AR_TIMES: [f64; 64] = [
    100000.0, 100000.0, 8100.0, 6900.0, 6000.0, 4800.0, 4000.0, 3400.0, 3000.0, 2400.0, 2000.0,
    1700.0, 1500.0, 1200.0, 1000.0, 860.0, 760.0, 600.0, 500.0, 430.0, 380.0, 300.0, 250.0, 220.0,
    190.0, 150.0, 130.0, 110.0, 95.0, 76.0, 63.0, 55.0, 47.0, 38.0, 31.0, 27.0, 24.0, 19.0, 15.0,
    13.0, 12.0, 9.4, 7.9, 6.8, 6.0, 4.7, 3.8, 3.4, 3.0, 2.4, 2.0, 1.8, 1.6, 1.3, 1.1, 0.93, 0.85,
    0.65, 0.53, 0.44, 0.40, 0.35, 0.0, 0.0,
];
#[rustfmt::skip]
const DR_TIMES: [f64; 64] = [
    100000.0, 100000.0, 118200.0, 101300.0, 88600.0, 70900.0, 59100.0, 50700.0, 44300.0, 35500.0,
    29600.0, 25300.0, 22200.0, 17700.0, 14800.0, 12700.0, 11100.0, 8900.0, 7400.0, 6300.0, 5500.0,
    4400.0, 3700.0, 3200.0, 2800.0, 2200.0, 1800.0, 1600.0, 1400.0, 1100.0, 920.0, 790.0, 690.0,
    550.0, 460.0, 390.0, 340.0, 270.0, 230.0, 200.0, 170.0, 140.0, 110.0, 98.0, 85.0, 68.0, 57.0,
    49.0, 43.0, 34.0, 28.0, 25.0, 22.0, 18.0, 14.0, 12.0, 11.0, 8.5, 7.1, 6.1, 5.4, 4.3, 3.6, 3.1,
];

/// Precomputed envelope tables (built once, shared): per-sample volume steps
/// for attack/decay rates, the log→linear envelope curve, and the TL
/// (total-level) attenuation curve. All linear values are `0..1 << PHASE_SHIFT`.
struct EgTables {
    ar: [i32; 64],
    dr: [i32; 64],
    eg: [i32; 1024],
    tl: [i32; 256],
}

static EG_TABLES: std::sync::LazyLock<EgTables> = std::sync::LazyLock::new(|| {
    let scale = (1u32 << EG_SHIFT) as f64;
    let mut ar = [0i32; 64];
    let mut dr = [0i32; 64];
    for i in 2..64 {
        ar[i] = if AR_TIMES[i] != 0.0 {
            ((1023.0 * 1000.0) / (44100.0 * AR_TIMES[i]) * scale) as i32
        } else {
            1024 << EG_SHIFT
        };
        dr[i] = ((1023.0 * 1000.0) / (44100.0 * DR_TIMES[i]) * scale) as i32;
    }
    let lin = (1u32 << PHASE_SHIFT) as f64;
    let mut eg = [0i32; 1024];
    for (i, e) in eg.iter_mut().enumerate() {
        let db = (3.0 * (i as f64 - 1023.0)) / 32.0;
        *e = (10f64.powf(db / 20.0) * lin) as i32;
    }
    let mut tl = [0i32; 256];
    for (i, t) in tl.iter_mut().enumerate() {
        let steps = [0.4, 0.8, 1.5, 3.0, 6.0, 12.0, 24.0, 48.0];
        let db: f64 = steps
            .iter()
            .enumerate()
            .filter(|(b, _)| i & (1 << b) != 0)
            .map(|(_, d)| -d)
            .sum();
        *t = (10f64.powf(db / 20.0) * lin) as i32;
    }
    EgTables { ar, dr, eg, tl }
});

/// Per-(DISDL, DIPAN) left/right output gains, `FIX(4 · pan · send-level)`
/// (no TL — that's already folded into the slot's EG output). Indexed by
/// `(DISDL << 5) | DIPAN`.
static PAN_TABLES: std::sync::LazyLock<([i32; 256], [i32; 256])> = std::sync::LazyLock::new(|| {
    // Direct-sound-level attenuation (dB) for DISDL 0..7 (0 = muted).
    const SDLT: [f64; 8] = [-1.0e6, -36.0, -30.0, -24.0, -18.0, -12.0, -6.0, 0.0];
    let lin = (1u32 << PHASE_SHIFT) as f64;
    let mut lp = [0i32; 256];
    let mut rp = [0i32; 256];
    for (disdl, &sdl) in SDLT.iter().enumerate() {
        let fsdl = if disdl != 0 {
            10f64.powf(sdl / 20.0)
        } else {
            0.0
        };
        for dipan in 0..32usize {
            let pdb = [(0x1, 3.0), (0x2, 6.0), (0x4, 12.0), (0x8, 24.0)]
                .iter()
                .filter(|(b, _)| dipan & b != 0)
                .map(|(_, d)| -d)
                .sum::<f64>();
            let pan = if dipan & 0xF == 0xF {
                0.0
            } else {
                10f64.powf(pdb / 20.0)
            };
            // DIPAN bit 4 selects which channel is attenuated.
            let (l, r) = if dipan < 0x10 { (pan, 1.0) } else { (1.0, pan) };
            let idx = (disdl << 5) | dipan;
            lp[idx] = (4.0 * l * fsdl * lin) as i32;
            rp[idx] = (4.0 * r * fsdl * lin) as i32;
        }
    }
    (lp, rp)
});

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
    /// The effect DSP (reverb/echo); fed by slot effect-sends, mixed back in.
    dsp: dsp::Dsp,
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
            dsp: dsp::Dsp::new(),
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
        // DSP program / coefficients / delay-address tables live at
        // 0x700..0xC00; route them into the DSP and recompute its length.
        match o & !1 {
            0x700..=0x77E => self.dsp.coef[((o - 0x700) / 2) as usize] = v as i16,
            0x780..=0x7BE => self.dsp.madrs[((o - 0x780) / 2) as usize] = v,
            0x800..=0xBFE => {
                self.dsp.mpro[((o - 0x800) / 2) as usize] = v;
                self.dsp.start();
            }
            _ => {}
        }
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
            } else if self.slots[i].active {
                // Key-off enters the release phase (the slot keeps playing
                // until the release envelope decays to silence).
                self.slots[i].eg.state = EgState::Release;
            }
            // Clear KYONEX (bit 12) so the strobe is one-shot.
            if data0 & 0x1000 != 0 {
                self.store16(i as u32 * SLOT_STRIDE, data0 & !0x1000);
            }
        }
    }

    fn start_slot(&mut self, i: usize) {
        let step = self.slot_step(i);
        let eg = self.compute_eg(i);
        self.slots[i] = Slot {
            active: true,
            backwards: false,
            cur: 0,
            nxt: 1 << PHASE_SHIFT,
            step,
            eg,
        };
    }

    /// Build the envelope state at key-on: cache the AR/D1R/D2R/RR step sizes
    /// (rate-scaled by OCT/KRS/FNS), the decay level, and EGHOLD.
    fn compute_eg(&self, i: usize) -> Eg {
        let t = &*EG_TABLES;
        let reg4 = self.slot_reg(i, 4); // D2R(15-11) D1R(10-6) EGHOLD(5) AR(4-0)
        let reg5 = self.slot_reg(i, 5); // KRS(13-10) DL(9-5) RR(4-0)
        let reg8 = self.slot_reg(i, 8); // OCT(14-11) FNS(9-0)
        let krs = (reg5 >> 10) & 0xF;
        let oct = (reg8 >> 11) & 0xF;
        let fns = reg8 & 0x3FF;
        let octave = (oct ^ 8) as i32 - 8;
        let rate = if krs != 0xF {
            octave + 2 * krs as i32 + ((fns >> 9) & 1) as i32
        } else {
            0
        };
        let ar =
            |field: u16, tbl: &[i32; 64]| tbl[(rate + ((field as i32) << 1)).clamp(0, 63) as usize];
        Eg {
            state: EgState::Attack,
            volume: 0x17F << EG_SHIFT,
            ar: ar(reg4 & 0x1F, &t.ar),
            d1r: ar((reg4 >> 6) & 0x1F, &t.dr),
            d2r: ar((reg4 >> 11) & 0x1F, &t.dr),
            rr: ar(reg5 & 0x1F, &t.dr),
            dl: 0x1F - ((reg5 >> 5) & 0x1F) as i32,
            eghold: reg4 & 0x20 != 0,
        }
    }

    /// Advance slot `i`'s envelope one output sample and return the linear
    /// output multiplier (EG × TL, `0..1 << PHASE_SHIFT`). Returns 0 for an
    /// inactive slot; a release that decays to zero deactivates the slot.
    pub fn eg_advance(&mut self, i: usize) -> i32 {
        if !self.slots[i].active {
            return 0;
        }
        let tl = (self.slot_reg(i, 6) & 0xFF) as usize;
        let was_attack = self.slots[i].eg.state == EgState::Attack;
        let mut deactivate = false;
        // EG_Update: advance volume / state, yield the raw EG value.
        let raw = {
            let eg = &mut self.slots[i].eg;
            match eg.state {
                EgState::Attack => {
                    eg.volume += eg.ar;
                    if eg.volume >= (0x3FF << EG_SHIFT) {
                        eg.volume = 0x3FF << EG_SHIFT;
                        eg.state = if eg.d1r >= (1024 << EG_SHIFT) {
                            EgState::Decay2
                        } else {
                            EgState::Decay1
                        };
                    }
                    if eg.eghold {
                        0x3FF << (PHASE_SHIFT - 10)
                    } else {
                        (eg.volume >> EG_SHIFT) << (PHASE_SHIFT - 10)
                    }
                }
                EgState::Decay1 => {
                    eg.volume = (eg.volume - eg.d1r).max(0);
                    if (eg.volume >> (EG_SHIFT + 5)) <= eg.dl {
                        eg.state = EgState::Decay2;
                    }
                    (eg.volume >> EG_SHIFT) << (PHASE_SHIFT - 10)
                }
                EgState::Decay2 => {
                    eg.volume = (eg.volume - eg.d2r).max(0);
                    (eg.volume >> EG_SHIFT) << (PHASE_SHIFT - 10)
                }
                EgState::Release => {
                    eg.volume -= eg.rr;
                    if eg.volume <= 0 {
                        eg.volume = 0;
                        deactivate = true;
                    }
                    (eg.volume >> EG_SHIFT) << (PHASE_SHIFT - 10)
                }
            }
        };
        if deactivate {
            self.slots[i].active = false;
        }
        let t = &*EG_TABLES;
        // Attack uses the raw value as a linear ramp; the other phases index
        // the log→linear envelope curve.
        let eg_mul = if was_attack {
            raw
        } else {
            t.eg[((raw >> (PHASE_SHIFT - 10)) as usize) & 0x3FF]
        };
        (eg_mul * t.tl[tl]) >> PHASE_SHIFT
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

    /// Mix all active slots into one stereo output sample. Each slot's PCM is
    /// shaped by its EG × TL, then panned to L/R by DIPAN and scaled by the
    /// direct-sound level DISDL; the sum is brought back into 16-bit range.
    fn mix(&mut self, ram: &mut Ram) -> (i16, i16) {
        let (lp, rp) = &*PAN_TABLES;
        let (mut l, mut r) = (0i32, 0i32);
        let dsp_on = self.dsp.running();
        for i in 0..NUM_SLOTS {
            if !self.slots[i].active {
                continue;
            }
            let pcm = self.slot_sample(i, ram) as i32;
            let voice = (pcm * self.eg_advance(i)) >> PHASE_SHIFT;
            // Direct output: pan + direct-sound level.
            let reg_b = self.slot_reg(i, 0xB);
            let idx = ((((reg_b >> 13) & 7) << 5) | ((reg_b >> 8) & 0x1F)) as usize;
            l += (voice * lp[idx]) >> PHASE_SHIFT;
            r += (voice * rp[idx]) >> PHASE_SHIFT;
            // Effect send: route the voice into the DSP input mix (ISEL) at
            // the IMXL level (reg 0xA). Only when a DSP program is running.
            if dsp_on {
                let reg_a = self.slot_reg(i, 0xA);
                let imxl = (reg_a & 7) as u32;
                if imxl != 0 {
                    let isel = ((reg_a >> 3) & 0xF) as usize;
                    self.dsp.set_sample(voice << imxl, isel);
                }
            }
        }
        if dsp_on {
            // Configure the delay ring from RBL/RBP (reg 0x402), run the
            // effect program, and fold its outputs back (EFREG even→L, odd→R).
            let rbc = self.read16(0x402);
            self.dsp.rbp = (rbc & 0x3F) as u32;
            self.dsp.rbl = 0x2000u32 << ((rbc >> 7) & 3);
            self.dsp.step(ram);
            for (i, &e) in self.dsp.efreg.iter().enumerate() {
                if i & 1 == 0 {
                    l += (e as i32) << 4;
                } else {
                    r += (e as i32) << 4;
                }
            }
        }
        // The pan gains carry ×4 headroom (FIX(4·…)); undo it and clamp.
        (
            (l >> 2).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            (r >> 2).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        )
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
    /// Generated 44.1 kHz output, interleaved L,R. The frontend drains it each
    /// frame; capped so headless runs (which never drain) don't grow unbounded.
    out: Vec<i16>,
}

/// Cap on the buffered audio (interleaved samples ≈ 46 ms) — overrun guard.
const MAX_AUDIO_SAMPLES: usize = 4096;

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
            out: Vec::new(),
        }
    }

    /// Take the generated audio (interleaved L,R, 44.1 kHz). The frontend
    /// queues this to the audio device each frame.
    pub fn take_audio(&mut self) -> Vec<i16> {
        core::mem::take(&mut self.out)
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

    /// Advance slot `i`'s envelope one sample and return the EG × TL output
    /// multiplier (`0..1 << 12`). The mixer pairs this with `slot_sample`.
    pub fn eg_advance(&mut self, i: usize) -> i32 {
        self.ctrl.eg_advance(i)
    }

    /// Generate one 44.1 kHz stereo output sample, mixing all active slots.
    pub fn next_sample(&mut self) -> (i16, i16) {
        let Scsp { ctrl, ram, .. } = self;
        ctrl.mix(ram)
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
        // Sample clock → timers + audio generation.
        self.sample_frac += sh2_cycles.saturating_mul(SCSP_SAMPLE_HZ);
        let samples = (self.sample_frac / SH2_CLOCK_HZ) as u32;
        self.sample_frac %= SH2_CLOCK_HZ;
        self.ctrl.tick_timers(samples);
        for _ in 0..samples {
            if self.out.len() >= MAX_AUDIO_SAMPLES {
                break; // overrun (frontend not draining) — drop the excess
            }
            let (l, r) = {
                let Scsp { ctrl, ram, .. } = &mut *self;
                ctrl.mix(ram)
            };
            self.out.push(l);
            self.out.push(r);
        }

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
    fn envelope_ramps_up_during_attack() {
        let mut s = Scsp::new();
        s.ctrl.write16(0x08, 20); // data[4]: AR = 20 (a gradual attack), EGHOLD off
        keyon_slot0(&mut s, 0, 0x100, 0, 0x100, 0);
        let early = s.eg_advance(0);
        let mut later = early;
        for _ in 0..100 {
            later = s.eg_advance(0);
        }
        assert!(later > early, "attack envelope rises ({early} → {later})");
    }

    #[test]
    fn total_level_attenuates_the_output() {
        // EGHOLD holds the attack at full scale, isolating the TL attenuation.
        let mut loud = Scsp::new();
        loud.ctrl.write16(0x08, 0x20); // data[4]: EGHOLD
        loud.ctrl.write16(0x0C, 0x0000); // data[6]: TL = 0 (no attenuation)
        keyon_slot0(&mut loud, 0, 0x100, 0, 0x100, 0);
        let full = loud.eg_advance(0);

        let mut quiet = Scsp::new();
        quiet.ctrl.write16(0x08, 0x20); // EGHOLD
        quiet.ctrl.write16(0x0C, 0x0080); // TL = 0x80 (−48 dB)
        keyon_slot0(&mut quiet, 0, 0x100, 0, 0x100, 0);
        let attenuated = quiet.eg_advance(0);

        assert!(full > 0xF00, "TL 0 → near full scale");
        assert!(attenuated < full / 4, "TL 0x80 sharply attenuates");
    }

    #[test]
    fn key_off_enters_release_then_silences() {
        let mut s = Scsp::new();
        s.ctrl.write16(0x0A, 0x001F); // data[5]: RR = 31 (fast release)
        keyon_slot0(&mut s, 0, 0x100, 0, 0x100, 0);
        assert!(s.slot_active(0));
        s.ctrl.write16(0x00, 0x1000); // clear KYONB, strobe KYONEX → key off
        assert!(s.slot_active(0), "key-off enters release, still playing");
        for _ in 0..100_000 {
            if !s.slot_active(0) {
                break;
            }
            s.eg_advance(0);
        }
        assert!(!s.slot_active(0), "release decays to silence → slot off");
    }

    // ---- mixer / DAC ----

    /// Key on slot `idx` with a constant 16-bit sample, EGHOLD (full level),
    /// and the given DISDL/DIPAN pan word.
    fn keyon_panned(s: &mut Scsp, idx: usize, value: u16, disdl: u16, dipan: u16) {
        let base = idx as u32 * 0x20;
        // Fill a few words of the slot's waveform at SA = 0x1000 + idx*0x40.
        let sa = 0x1000 + idx as u32 * 0x40;
        for n in 0..8u32 {
            s.ram.write16(sa + n * 2, value);
        }
        s.ctrl.write16(base + 0x02, sa as u16); // SA low
        s.ctrl.write16(base + 0x06, 0x100); // LEA
        s.ctrl.write16(base + 0x08, 0x20); // data[4]: EGHOLD
        s.ctrl.write16(base + 0x16, (disdl << 13) | (dipan << 8)); // DISDL/DIPAN
        s.ctrl.write16(base + 0x10, 0); // OCT/FNS
        s.ctrl.write16(base, 0x1800); // key on (SA high nibble 0)
    }

    #[test]
    fn mixer_centers_a_single_slot_on_both_channels() {
        let mut s = Scsp::new();
        keyon_panned(&mut s, 0, 0x2000, 7, 0x00); // full level, centre pan
        let (l, r) = s.next_sample();
        assert!(l > 0 && r > 0, "centre slot audible on both channels");
        assert_eq!(l, r, "centre pan is symmetric");
    }

    #[test]
    fn mixer_pans_hard_left_and_right() {
        let mut s = Scsp::new();
        keyon_panned(&mut s, 0, 0x2000, 7, 0x1F); // hard left (right muted)
        keyon_panned(&mut s, 1, 0x2000, 7, 0x0F); // hard right (left muted)
        let (l, r) = s.next_sample();
        // Slot 0 → left only, slot 1 → right only.
        assert!(l > 0 && r > 0, "both sides driven by one slot each");
        // With one slot fully on each side, the channels are balanced…
        assert_eq!(l, r);
        // …and removing the right-panned slot drops the right channel.
        let mut s2 = Scsp::new();
        keyon_panned(&mut s2, 0, 0x2000, 7, 0x1F); // hard left only
        let (l2, r2) = s2.next_sample();
        assert!(l2 > 0, "left channel driven");
        assert_eq!(r2, 0, "nothing panned right");
    }

    #[test]
    fn disdl_zero_mutes_the_direct_output() {
        let mut s = Scsp::new();
        keyon_panned(&mut s, 0, 0x2000, 0, 0x00); // DISDL = 0 → muted
        let (l, r) = s.next_sample();
        assert_eq!((l, r), (0, 0), "DISDL 0 sends nothing to the DAC");
    }

    #[test]
    fn silence_when_no_slot_is_active() {
        let mut s = Scsp::new();
        assert_eq!(s.next_sample(), (0, 0));
    }

    #[test]
    fn run_generates_audio_for_an_active_slot() {
        let mut s = Scsp::new();
        s.ram.write32(4, 0x2000); // 68k reset PC
        s.ram.write16(0x2000, 0x60FE); // BRA self
        keyon_panned(&mut s, 0, 0x2000, 7, 0x00); // slot 0 audible, centred
        s.start(); // release the 68k → SCSP runs
        s.run(2_000_000); // many SH-2 cycles → fill the audio buffer
        let audio = s.take_audio();
        assert!(!audio.is_empty(), "audio was generated");
        assert!(audio.iter().any(|&x| x != 0), "output is non-silent");
        assert!(s.take_audio().is_empty(), "buffer drained");
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
