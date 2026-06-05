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
mod hle;

use crate::memory::Ram;
use m68k::bus::{AccessKind, Bus};
use serde_big_array::BigArray;

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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum EgState {
    Attack,
    Decay1,
    Decay2,
    #[default]
    Release,
}

/// Per-slot envelope-generator state. Rates are cached at key-on (they depend
/// on OCT/KRS); `volume` is the log loudness index (`0` silent … `0x3FF` full).
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Eg {
    state: EgState,
    volume: i32,
    /// Attack right-shift: the rise step is `(0x3FF - volume) >> ar_shift`, an
    /// **exponential** approach to full (the real SCSP / Mednafen attack curve,
    /// `EnvLevel += ~EnvLevel >> srac`), not a fixed linear step. Smaller =
    /// faster. Derived from the attack rate in [`Self::compute_eg`].
    ar_shift: u32,
    d1r: i32,
    d2r: i32,
    rr: i32,
    dl: i32,
    eghold: bool,
}

/// One PCM slot's runtime state (the configuration lives in the register bank;
/// this is the per-sample playback state set up at key-on).
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Slot {
    active: bool,
    backwards: bool,
    /// Phase accumulator: `cur >> PHASE_SHIFT` is the sample index past SA.
    cur: u32,
    nxt: u32,
    /// Phase increment per output sample (from OCT/FNS).
    step: u32,
    eg: Eg,
    /// LFO phase counter (8-bit), advanced by [`ScspCtrl::run_lfo`]. The
    /// PLFO/ALFO waveforms are derived from it. (SCSP LFO; Mednafen `LFOCounter`.)
    lfo_counter: u8,
    /// Countdown to the next `lfo_counter` increment; reloaded from LFOF.
    /// (Mednafen `LFOTimeCounter`.)
    lfo_timer: i32,
    /// This sample's PLFO contribution to the phase increment (signed, added to
    /// [`Self::step`] in `slot_sample`). 0 when the pitch LFO is off — keeping
    /// the no-LFO path byte-identical.
    step_mod: i32,
    /// This sample's ALFO amplitude-attenuation offset, added to the EG index
    /// in `eg_advance`. 0 when the amplitude LFO is off.
    alfo: i32,
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

/// One SCSP timer, modelled on Mednafen (`scsp.inc` `Timers[]` + `RunSample`): a
/// **free-running 8-bit** up-counter clocked once every `2^Control` output
/// samples (the prescale; the *which sample* is aligned to the global sample
/// counter, see [`ScspCtrl::tick_timers`]). `TIMx` is loaded into the counter
/// **only on a register write** (a one-shot `reload`); the interrupt pends when
/// the counter **reaches `0xFF`**, after which it wraps `0xFF→0x00` and keeps
/// free-running (steady-state period 256) — it does **not** auto-reload from
/// `TIMx`. (Getting this wrong — auto-reloading, or overflowing one clock late at
/// `0x100` — shifts the timer cadence and drifts the sound driver's per-voice
/// dividers out of phase vs the reference.)
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Timer {
    counter: u8,
    /// Prescale exponent (`TIMx` bits 10-8): clock every `2^control` samples.
    control: u8,
    /// `-1` = none pending; else the value to load into `counter` on the next
    /// clock (latched on a `TIMx` register write).
    reload: i32,
}

impl Default for Timer {
    fn default() -> Self {
        // Power-on: no reload pending (matches Mednafen `Reset`, `Reload = -1`).
        Self {
            counter: 0,
            control: 0,
            reload: -1,
        }
    }
}

impl Timer {
    /// A `TIMx` register write: latch the prescale (bits 10-8) and a one-shot
    /// reload of the counter (bits 7-0).
    fn write(&mut self, reg: u16) {
        self.control = ((reg >> 8) & 7) as u8;
        self.reload = (reg & 0xFF) as i32;
    }

    /// Advance one clock (call only on a `DoClock` sample for this timer's
    /// prescale); returns `true` when the counter reaches `0xFF` — the overflow
    /// that pends the timer interrupt.
    fn clock(&mut self) -> bool {
        if self.reload >= 0 {
            self.counter = self.reload as u8;
            self.reload = -1;
        } else {
            self.counter = self.counter.wrapping_add(1);
        }
        self.counter == 0xFF
    }
}

/// Debug snapshot of a slot's key playback parameters (sdbg `scsp`): the
/// register-derived config (sample address / loop / pitch / pan / total level)
/// plus the live phase and envelope state. Lets a garbled-audio diagnosis tell a
/// mis-programmed slot (wrong SA/pitch/loop from the sound driver) from a render
/// bug (sane params, bad output).
#[derive(Clone, Copy, Debug)]
pub struct SlotDebug {
    pub active: bool,
    /// Sample start byte address into sound RAM (20-bit).
    pub sa: u32,
    /// Loop start / end (sample index past SA).
    pub lsa: u16,
    pub lea: u16,
    /// 8-bit PCM (else 16-bit).
    pub pcm8: bool,
    /// Loop control: 0 none, 1 normal, 2 reverse, 3 ping-pong.
    pub lpctl: u8,
    /// Pitch: signed octave (−8..+7) and 10-bit fine.
    pub oct: i8,
    pub fns: u16,
    /// Live phase accumulator + increment (`cur >> 12` = sample index).
    pub step: u32,
    pub cur: u32,
    /// Envelope phase ("ATK"/"D1"/"D2"/"REL") and log volume (0 silent … 0x3FF).
    pub eg_state: &'static str,
    pub eg_volume: i32,
    /// Direct output: send level (0 muted … 7 full) and pan (bit4 = side).
    pub disdl: u8,
    pub dipan: u8,
    /// Total level attenuation (0 loudest … 0xFF).
    pub tl: u16,
    /// Effect send to the DSP: input level (IMXL, 0 = none) + input select (ISEL).
    pub imxl: u8,
    pub isel: u8,
    /// Effect return from the DSP: output level (EFSDL, 0 = none) + pan (EFPAN).
    pub efsdl: u8,
    pub efpan: u8,
}

/// SCSP control + slot + DSP registers, with timer state and the derived
/// interrupt lines. Register reads are plain; writes to the interrupt-control
/// window have side effects (pending/reset/recompute).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ScspCtrl {
    #[serde(with = "BigArray")]
    raw: [u8; REG_BYTES],
    timers: [Timer; 3],
    /// Global output-sample counter (Mednafen `SampleCounter`): drives the
    /// timer-clock alignment so a timer's `2^Control` prescale lands on the same
    /// samples as the reference, independent of when `TIMx` was written.
    sample_counter: u64,
    slots: [Slot; NUM_SLOTS],
    /// 17-bit Galois LFSR — the SCSP's shared noise source (LFO noise waveform
    /// and the noise sound-source). Clocked once per slot-cycle (32×/output
    /// sample), matching Mednafen `LFSR`. Resets to 1.
    lfsr: u32,
    /// The effect DSP (reverb/echo); fed by slot effect-sends, mixed back in.
    dsp: dsp::Dsp,
    /// Current 68k interrupt-line level (0 = none); level-triggered.
    asserted_level: u8,
    /// Main-CPU sound interrupt pending (forwarded to the SCU).
    main_pending: bool,
    /// Debug-only lifetime counters: how many times KYONEX was strobed
    /// (`key_on_execute`) and how many slot starts (`start_slot`) resulted.
    /// Distinguishes "driver never tries to play" from "key-on fails". Not
    /// machine state — skipped in save states.
    #[serde(skip)]
    dbg_keyon_execs: u32,
    #[serde(skip)]
    dbg_slot_starts: u32,
    /// Debug-only: lifetime timer-overflow counts [A,B,C], tick_timers calls, and
    /// total samples ticked — to see whether the SCSP timers keep firing (the
    /// sound driver polls Timer A; if it stops overflowing the driver stalls).
    #[serde(skip)]
    dbg_timer_of: [u32; 3],
    #[serde(skip)]
    dbg_tt_calls: u32,
    #[serde(skip)]
    dbg_tt_samples: u32,
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
            sample_counter: 0,
            slots: [Slot::default(); NUM_SLOTS],
            lfsr: 1,
            dsp: dsp::Dsp::new(),
            asserted_level: 0,
            main_pending: false,
            dbg_keyon_execs: 0,
            dbg_slot_starts: 0,
            dbg_timer_of: [0; 3],
            dbg_tt_calls: 0,
            dbg_tt_samples: 0,
        }
    }

    /// Debug: (KYONEX strobes, slot starts) seen over the run.
    pub fn dbg_keyon_counts(&self) -> (u32, u32) {
        (self.dbg_keyon_execs, self.dbg_slot_starts)
    }

    /// Debug: ([Timer A,B,C overflow counts], tick_timers calls, samples ticked).
    pub fn dbg_timer_counts(&self) -> ([u32; 3], u32, u32) {
        (self.dbg_timer_of, self.dbg_tt_calls, self.dbg_tt_samples)
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

    /// Debug: the current 68k IRQ state — `(asserted_level, SCIEB, SCIPD)`.
    /// Lets a debugger see whether the sound driver enabled a timer interrupt
    /// (SCIEB) and whether one is pending (SCIPD) — i.e. whether the 68k should
    /// be woken from its idle spin to service sound commands.
    pub fn irq_state(&self) -> (u8, u16, u16) {
        (self.asserted_level, self.read16(SCIEB), self.read16(SCIPD))
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
            // Latch the prescale + one-shot counter reload (the bank still holds
            // the written value for reads; the timer logic uses the latch).
            TIMA => self.timers[0].write(v),
            TIMB => self.timers[1].write(v),
            TIMC => self.timers[2].write(v),
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
        self.dbg_tt_calls = self.dbg_tt_calls.wrapping_add(1);
        self.dbg_tt_samples = self.dbg_tt_samples.wrapping_add(samples);
        let bits = [INT_TIMER_A, INT_TIMER_B, INT_TIMER_C];
        let mut scipd = false;
        let mut mcipd = false;
        // Per output sample: each timer is clocked when the global sample
        // counter's low `Control` bits are 0 (Mednafen `DoClock =
        // !(SampleCounter & ((1<<Control)-1))`), so its `2^Control` prescale is
        // phase-locked to the sample clock, not to the `TIMx` write.
        for _ in 0..samples {
            let sc = self.sample_counter;
            self.sample_counter = self.sample_counter.wrapping_add(1);
            for (i, &bit) in bits.iter().enumerate() {
                if sc & ((1u64 << self.timers[i].control) - 1) != 0 {
                    continue; // not a clock edge for this prescale
                }
                if self.timers[i].clock() {
                    self.dbg_timer_of[i] = self.dbg_timer_of[i].wrapping_add(1);
                    self.store16(SCIPD, self.read16(SCIPD) | bit);
                    scipd = true;
                    if i == 0 {
                        // Timer A also pends the main-CPU sound interrupt.
                        self.store16(MCIPD, self.read16(MCIPD) | INT_TIMER_A);
                        mcipd = true;
                    }
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

    /// Advance slot `i`'s low-frequency oscillator one output sample and stash
    /// this sample's PLFO phase delta ([`Slot::step_mod`]) and ALFO amplitude
    /// offset ([`Slot::alfo`]). Faithful to Mednafen `RunLFO`/`GetPLFO`/`GetALFO`
    /// (`scsp.inc`): the pitch LFO reads the **pre-advance** counter and the
    /// amplitude LFO the **post-advance** counter; the noise waveform is drawn
    /// from the shared [`Self::lfsr`]. The LFO register is slot word 9 —
    /// `[15] LFORE | [14:10] LFOF | [9:8] PLFOWS | [7:5] PLFOS | [4:3] ALFOWS |
    /// [2:0] ALFOS`. With both mod levels 0 the deltas stay 0, so the no-LFO
    /// path is byte-identical.
    fn run_lfo(&mut self, i: usize) {
        let srv = self.slot_reg(i, 9);
        let alfo_level = (srv & 0x7) as u32;
        let alfo_wave = (srv >> 3) & 0x3;
        let plfo_level = (srv >> 5) & 0x7;
        let plfo_wave = (srv >> 8) & 0x3;
        let lfo_freq = ((srv >> 10) & 0x1F) as u32;
        let lfo_reset = (srv >> 15) & 1 != 0;
        let lfsr = self.lfsr as u8;
        let reg8 = self.slot_reg(i, 8);
        let fns = (reg8 & 0x3FF) as u32;

        // --- PLFO (pitch), from the pre-advance counter ---
        let counter = self.slots[i].lfo_counter;
        let plfo = if plfo_level == 0 {
            0
        } else {
            let raw = match plfo_wave {
                0 => (counter & !1) as i8 as i32, // saw
                1 => (if counter & 0x80 != 0 { 0x80u8 } else { 0x7E }) as i8 as i32, // square
                2 => {
                    let t = (counter & 0x3F)
                        ^ if counter & 0x40 != 0 { 0x3F } else { 0 }
                        ^ if counter & 0x80 != 0 { 0x7F } else { 0 };
                    t.wrapping_shl(1) as i8 as i32 // triangle
                }
                _ => (lfsr & !1) as i8 as i32, // noise
            };
            let scaled = raw >> (7 - plfo_level);
            ((0x40 ^ (fns >> 4)) as i32 * scaled) >> 6
        };
        // PLFO adds to (FNS+0x400) *inside* the octave shift, so its phase-step
        // contribution is `plfo` put through the same shift as [`Self::slot_step`].
        let octave = (((reg8 >> 11) & 0xF) as i32 ^ 8) - 8 + PHASE_SHIFT as i32 - 10;
        self.slots[i].step_mod = if octave >= 0 {
            plfo << octave
        } else {
            plfo >> -octave
        };

        // --- RunLFO: advance the phase counter, then optional reset ---
        {
            let s = &mut self.slots[i];
            s.lfo_timer -= 1;
            if s.lfo_timer <= 0 {
                s.lfo_counter = s.lfo_counter.wrapping_add(1);
                s.lfo_timer = (((8 - (lfo_freq & 0x3)) << 7) >> (lfo_freq >> 2)) as i32 - 4;
            }
            if lfo_reset {
                s.lfo_counter = 0;
            }
        }

        // --- ALFO (amplitude attenuation), from the post-advance counter ---
        let counter = self.slots[i].lfo_counter;
        self.slots[i].alfo = if alfo_level == 0 {
            0
        } else {
            let raw: u32 = match alfo_wave {
                0 => (counter & !1) as u32,                                         // saw
                1 => ((counter as i8 >> 7) as u8 & !1) as u32, // square (0x00 or 0xFE)
                2 => (counter ^ (counter as i8 >> 7) as u8).wrapping_shl(1) as u32, // triangle
                _ => (lfsr & !1) as u32,                       // noise
            };
            (raw >> (7 - alfo_level)) as i32
        };
    }

    /// Execute key-on/off for all slots based on each one's KYONB bit, then
    /// clear the KYONEX strobe wherever it's set.
    ///
    /// **Edge-triggered**, matching Mednafen (`scsp.inc:1496`,
    /// `if(KeyExecute && (EnvPhase == RELEASE) == KeyBit)`): a KYONEX strobe acts
    /// only on a *transition*. A slot is "off" (re-keyable) iff it is inactive or
    /// already releasing. KYONB=1 starts an *off* slot; it must **not** restart a
    /// slot that is still in Attack/Decay (re-strobing an already-playing voice
    /// does nothing). KYONB=0 releases a *playing* slot. Unconditionally calling
    /// `start_slot` for every KYONB=1 slot on every strobe (the old behaviour)
    /// let menu SFX the BIOS re-strobes with KYONB still set pile up at full
    /// volume — all 32 slots stuck in Decay2 — and clip to a growing buzz.
    fn key_on_execute(&mut self) {
        self.dbg_keyon_execs += 1;
        for i in 0..NUM_SLOTS {
            let data0 = self.slot_reg(i, 0);
            let kyonb = data0 & 0x0800 != 0;
            let off = !self.slots[i].active || self.slots[i].eg.state == EgState::Release;
            if kyonb {
                if off {
                    self.start_slot(i);
                }
            } else if !off {
                // Key-off a playing slot: enter the release phase (the slot keeps
                // playing until the release envelope decays to silence).
                self.slots[i].eg.state = EgState::Release;
            }
            // Clear KYONEX (bit 12) so the strobe is one-shot.
            if data0 & 0x1000 != 0 {
                self.store16(i as u32 * SLOT_STRIDE, data0 & !0x1000);
            }
        }
    }

    fn start_slot(&mut self, i: usize) {
        self.dbg_slot_starts += 1;
        let step = self.slot_step(i);
        let eg = self.compute_eg(i);
        self.slots[i] = Slot {
            active: true,
            backwards: false,
            cur: 0,
            nxt: 1 << PHASE_SHIFT,
            step,
            eg,
            lfo_counter: 0,
            // Mednafen seeds LFOTimeCounter to 1 at key-on, so the LFO advances
            // on the next sample.
            lfo_timer: 1,
            step_mod: 0,
            alfo: 0,
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
        // Attack is exponential (`volume += (0x3FF - volume) >> ar_shift`), so we
        // need a shift, not a linear step. Pick it so the exponential reaches
        // full in roughly the same sample count the old linear step (`ar_step`)
        // took — preserving the rate while fixing the *shape*: at max AR the old
        // step covered the whole range in one sample (an instant jump); the
        // exponential ramps over ~16 samples like the real SCSP / Mednafen.
        let ar_step = ar(reg4 & 0x1F, &t.ar);
        let lin_samples = ((0x280i64 << EG_SHIFT) / (ar_step as i64).max(1)).max(1) as u64;
        let ar_shift = (lin_samples.ilog2() + 1).clamp(2, 15);
        Eg {
            state: EgState::Attack,
            volume: 0x17F << EG_SHIFT,
            ar_shift,
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
        let mut deactivate = false;
        // EG_Update: advance volume / state, yield the raw EG value.
        let raw = {
            let eg = &mut self.slots[i].eg;
            match eg.state {
                EgState::Attack => {
                    // Exponential rise toward full: step a rate-determined
                    // fraction of the remaining distance (the SCSP attack
                    // curve), snapping the last bit once the step rounds to 0.
                    let remaining = (0x3FF << EG_SHIFT) - eg.volume;
                    let inc = remaining >> eg.ar_shift;
                    eg.volume += if inc > 0 { inc } else { remaining };
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
        // All phases — attack included — index the log→linear (dB) envelope
        // curve: the EG counter is an attenuation step, not a linear amplitude.
        // Mednafen runs `EnvLevel` through the same dB conversion at every phase
        // (scsp.inc RunSample), so a mid-attack `volume` of 0x17F (= attenuation
        // 0x280) maps to ≈ -60 dB, a near-silent start that ramps up — not the
        // 37 %-amplitude jump the old linear `raw` produced.
        // ALFO (amplitude LFO) adds attenuation in the same dB domain as the EG
        // index (Mednafen sums it into `vlevel` before the log→linear lookup);
        // `alfo` is 0 unless the LFO is active, so the no-LFO path is unchanged
        // (the index is already in `0..=0x3FF`, so the clamp matches the old mask).
        let alfo = self.slots[i].alfo;
        let eg_idx = ((raw >> (PHASE_SHIFT - 10)) + alfo).clamp(0, 0x3FF) as usize;
        let eg_mul = t.eg[eg_idx];
        (eg_mul * t.tl[tl]) >> PHASE_SHIFT
    }

    pub fn slot_active(&self, i: usize) -> bool {
        self.slots[i].active
    }

    /// Snapshot slot `i`'s playback parameters for debugging (see [`SlotDebug`]).
    pub fn slot_debug(&self, i: usize) -> SlotDebug {
        let data0 = self.slot_reg(i, 0);
        let pitch = self.slot_reg(i, 8);
        let oct_raw = ((pitch >> 11) & 0xF) as i32;
        let regb = self.slot_reg(i, 0xB);
        let s = &self.slots[i];
        SlotDebug {
            active: s.active,
            sa: ((data0 as u32 & 0xF) << 16) | self.slot_reg(i, 1) as u32,
            lsa: self.slot_reg(i, 2),
            lea: self.slot_reg(i, 3),
            pcm8: data0 & 0x0010 != 0,
            lpctl: ((data0 >> 5) & 3) as u8,
            oct: ((oct_raw ^ 8) - 8) as i8,
            fns: pitch & 0x3FF,
            step: s.step,
            cur: s.cur,
            eg_state: match s.eg.state {
                EgState::Attack => "ATK",
                EgState::Decay1 => "D1",
                EgState::Decay2 => "D2",
                EgState::Release => "REL",
            },
            eg_volume: s.eg.volume,
            disdl: ((regb >> 13) & 7) as u8,
            dipan: ((regb >> 8) & 0x1F) as u8,
            tl: self.slot_reg(i, 6) & 0xFF,
            imxl: (self.slot_reg(i, 0xA) & 7) as u8,
            isel: ((self.slot_reg(i, 0xA) >> 3) & 0xF) as u8,
            efsdl: ((regb >> 5) & 7) as u8,
            efpan: (regb & 0x1F) as u8,
        }
    }

    /// Debug: whether the effect DSP is running, plus its 16 output registers
    /// (EFREG). Lets sdbg confirm whether audible output is coming through the
    /// DSP effect path (for slots with their direct output muted, DISDL=0).
    pub fn dsp_state(&self) -> (bool, [i16; 16], [i32; 16], [i32; 16]) {
        (
            self.dsp.running(),
            self.dsp.efreg,
            self.dsp.efreg_hw,
            self.dsp.mixs_hw,
        )
    }

    /// Debug: the distinct EFREG output indices the loaded DSP microprogram
    /// writes (its EWT instructions' EWA fields, bits 27-24). A slot's effect
    /// return reads `EFREG[slot]`, so this shows which slots' returns the program
    /// actually feeds — i.e. whether a DSP-routed voice (e.g. slot 0) is ever
    /// produced at all.
    pub fn dsp_ewt_targets(&self) -> Vec<u8> {
        let mut t = Vec::new();
        for step in 0..128 {
            let ip2 = self.dsp.mpro[step * 4 + 2];
            if (ip2 >> 12) & 1 != 0 {
                let ewa = ((ip2 >> 8) & 0xF) as u8;
                if !t.contains(&ewa) {
                    t.push(ewa);
                }
            }
        }
        t.sort_unstable();
        t
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

        // Advance the phase and re-derive the next-sample address. `step_mod`
        // is this sample's PLFO (pitch-LFO) contribution — 0 unless the LFO is
        // active, so the no-LFO path is unchanged.
        let slot = &mut self.slots[i];
        let step = slot.step.wrapping_add(slot.step_mod as u32);
        if slot.backwards {
            slot.cur = slot.cur.wrapping_sub(step);
        } else {
            slot.cur = slot.cur.wrapping_add(step);
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
        // The slot effect-return reads the DSP outputs (EFREG) produced from the
        // *previous* sample's sends — Mednafen's one-sample effect pipeline
        // (`scsp.inc`: the sends collected this pass feed `DSP.step` below, whose
        // EFREG the next sample returns). Snapshot before the sends mutate it.
        let efreg = self.dsp.efreg;
        // Range loop: `i` indexes the slots/EFREG *and* is passed to the
        // `&mut self` slot methods, so it can't be an iterator.
        #[allow(clippy::needless_range_loop)]
        for i in 0..NUM_SLOTS {
            // The shared noise LFSR clocks once per slot-cycle — 32× per output
            // sample, regardless of slot activity (Mednafen `scsp.inc`: the
            // 17-bit Galois step `(LFSR>>1) | (((LFSR>>5)^LFSR)&1)<<16`).
            self.lfsr = (self.lfsr >> 1) | ((((self.lfsr >> 5) ^ self.lfsr) & 1) << 16);
            if !self.slots[i].active {
                continue;
            }
            // Advance the slot's LFO (sets this sample's PLFO phase delta + ALFO
            // attenuation, consumed by `slot_sample`/`eg_advance`).
            self.run_lfo(i);
            let pcm = self.slot_sample(i, ram) as i32;
            let voice = (pcm * self.eg_advance(i)) >> PHASE_SHIFT;
            let reg_b = self.slot_reg(i, 0xB);
            // Direct output: DISDL send level (bits 15-13) + DIPAN (12-8).
            let didx = ((((reg_b >> 13) & 7) << 5) | ((reg_b >> 8) & 0x1F)) as usize;
            l += (voice * lp[didx]) >> PHASE_SHIFT;
            r += (voice * rp[didx]) >> PHASE_SHIFT;
            // Effect return: this slot's DSP output `EFREG[i]` (slots 0..15) at
            // EFSDL send level (bits 7-5) + EFPAN (4-0). Same pan/level table as
            // the direct path (Mednafen derives both via `SDL_PAN_ToVolume`;
            // mixed `EFREG[slot] * EffectVolume` per slot, `scsp.inc:1669`).
            if dsp_on && i < 16 {
                let eidx = ((((reg_b >> 5) & 7) << 5) | (reg_b & 0x1F)) as usize;
                let eff = efreg[i] as i32;
                l += (eff * lp[eidx]) >> PHASE_SHIFT;
                r += (eff * rp[eidx]) >> PHASE_SHIFT;
            }
            // Effect send: route the voice into the DSP input mix MIXS[ISEL] at
            // the IMXL level (reg 0xA).
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
            // Configure the delay ring from RBL/RBP (reg 0x402) and run the
            // effect program; its EFREG outputs are read by the next sample's
            // per-slot effect-return above.
            let rbc = self.read16(0x402);
            self.dsp.rbp = (rbc & 0x3F) as u32;
            self.dsp.rbl = 0x2000u32 << ((rbc >> 7) & 3);
            self.dsp.step(ram);
        }
        // The pan gains carry ×4 headroom (FIX(4·…)); undo it and clamp.
        (
            (l >> 2).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            (r >> 2).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        )
    }
}

/// Frozen value-trace for the BGM interpreter diff:
/// `(frozen, seq_ticks, sample_at_first_tick, sample_at_trigger, ring)`. Records
/// the per-instruction `(pc, cycle, d4, d7)` ring across the driver until the
/// first enqueue, then freezes — `cycle` is the 68k's accumulated clock so a
/// tail-aligned **cycle-exact** lockstep vs Mednafen finds the first instruction
/// whose cost diverges (the m68k cycle-accounting root of the BGM-trigger lead).
/// `seq_ticks` counts seq-tick entries (`0x40F2`) and the two sample-counter
/// snapshots (at the first seq-tick and at the enqueue) give the Timer-B
/// **period** `(s_trig − s_first)/(seq_ticks−1)` — a zero-point-independent rate
/// to compare vs the reference, disambiguating a trigger-time gap from a
/// seq-tick-rate gap (M12 task 2). See [`Scsp::enable_68k_itrace`].
type ITrace = (
    bool,
    u32,
    u64,
    u64,
    std::collections::VecDeque<(u32, u64, u32, u32)>,
);

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    /// Carry of the 68k-cycle budget across batches. The 68k steps whole
    /// instructions, so the last step of a batch overshoots `budget` by part of
    /// an instruction; that overshoot (a negative leftover) is carried into the
    /// next batch's budget so the 68k runs **exactly** 256 cycles per produced
    /// sample on average — not ~270 (it would otherwise discard the overshoot
    /// every tiny event-clamped batch and creep ~5.7% ahead of the audio/timer
    /// clock, advancing the sound driver's sequence too early). Mednafen avoids
    /// this by running the 68k to the exact sample edge; the carry is our
    /// whole-instruction equivalent.
    budget_carry: i64,
    /// 68k cycles accumulated **since the last produced sample edge**, carried
    /// **across batches**. Batches are clamped to arbitrary event edges, so if
    /// this reset to 0 each batch the sample/timer/IRQ edges would re-phase to
    /// every batch boundary instead of tracking the absolute sample clock — a
    /// sub-sample phase error that shifts when the SCSP raises Timer-A/B SCIPD
    /// to the 68k (the lockstep-found root: the driver polls SCIPD and our wrong
    /// phase splits its control flow). Persisting it matches Mednafen's absolute
    /// `next_scsp_time`.
    sample_acc: i64,
    /// Generated 44.1 kHz output, interleaved L,R. The frontend drains it each
    /// frame; capped so headless runs (which never drain) don't grow unbounded.
    out: Vec<i16>,
    /// Debug-only ring of recent 68k PCs (consecutive duplicates collapsed), for
    /// the `sdbg` `t68` sound-driver trace. `#[serde(skip)]` — not machine state.
    #[serde(skip)]
    pc_trace: Option<std::collections::VecDeque<u32>>,
    /// Debug-only 68k breakpoint: `(pc, optional (reg, val) guard)`. The reg index
    /// is 0-7 = D0-D7, 8-15 = A0-A7. Captures [`M68kBpHit`] the first time the 68k
    /// is about to execute `pc` (with the guard satisfied). `#[serde(skip)]`.
    #[serde(skip)]
    bp68: Option<(u32, Option<(u8, u32)>)>,
    #[serde(skip)]
    bp68_hit: Option<M68kBpHit>,
    /// Debug-only set of *every* distinct 68k PC executed since enabled (unlike
    /// `pc_trace`, which is a bounded ring). Answers "does the sound driver ever
    /// reach routine X?" over a whole run. `#[serde(skip)]` — not machine state.
    #[serde(skip)]
    pc_seen: Option<std::collections::BTreeSet<u32>>,
    /// Debug-only multi-hit 68k register log: `(watch_pc, [d0,d1,d2,d3,a6] per
    /// hit)`. Captures the value stream at a hot PC (e.g. the BGM enqueue) for a
    /// reference diff vs Mednafen — unlike `bp68` (first hit only). `#[serde(skip)]`.
    #[serde(skip)]
    enq_log: Option<(u32, Vec<[u32; 5]>)>,
    /// Debug-only *instruction-boundary* value trace, a frozen ring of
    /// `(pc, d4, d7)` over `[0x4000,0x4C40)` that records until the first
    /// `0x4B9A` enqueue then freezes (`(frozen, ring)`). Dumped + diffed vs
    /// Mednafen's `SS_ITRACE` to find the exact instruction where a register
    /// value first diverges on an otherwise-identical PC path. `#[serde(skip)]`.
    #[serde(skip)]
    itrace: Option<ITrace>,
    /// Debug-only **signal scope** (the cross-emulator "oscilloscope"): at each
    /// 68k execution of `trigger_pc`, sample a set of sound-RAM channels into a
    /// row. One row per trigger hit is one *timeframe* (e.g. the seq-tick PC
    /// `0x40F2` → one row per Timer-B tick). Dumped as CSV and overlaid against
    /// the matching mednaref capture to see, per channel and per timeframe,
    /// exactly where ours' and Mednafen's signals diverge — the generalization
    /// of the one-off ENQLOG/itrace/gate probes. `#[serde(skip)]`.
    #[serde(skip)]
    scope: Option<ScopeCap>,
    /// Debug-only 68k **write-watch**: `(addr, last byte, log of (pc, old, new))`.
    /// After each 68k instruction, if the watched sound-RAM byte changed, the
    /// PC of that instruction is logged — finds *who* writes a value the scope
    /// shows diverging (the per-instruction complement of the scope). `#[serde(skip)]`.
    #[serde(skip)]
    wwatch68: Option<(u32, u8, Vec<(u32, u8, u8)>)>,
    /// Debug-only **instruction-lockstep** PC stream: every 68k instruction PC
    /// from the driver's first instruction (no dup-collapse, no range filter),
    /// capped. Diffed line-for-line against a reference's PC trace (MAME
    /// `audiocpu` `.tr`, or Mednafen) from the known-identical reset entry
    /// (`0x1000`) to find the **first** execution divergence — the root of the
    /// value recession (ADR-0012). `#[serde(skip)]`.
    #[serde(skip)]
    pcstream: Option<Vec<(u32, u64)>>,
    /// Opt-in native HLE of the 68k sound driver (ADR-0012). `None` (default) =
    /// the LLE 68k driver runs as the oracle; `Some` replaces it with a native
    /// sequencer (the SCSP *synthesis* is unchanged either way). `#[serde(skip)]`
    /// so the default-LLE path's save-states + serialized layout are untouched.
    #[serde(skip)]
    hle: Option<hle::HleSoundDriver>,
}

/// One cross-emulator signal-scope capture (see [`Scsp::enable_scope`]). Each
/// channel is `(name, sound-RAM byte address, width 1|2|4)`; `rows` holds one
/// sample-vector per `trigger_pc` hit (the timebase), capped at `max`.
#[derive(Clone, Debug)]
pub struct ScopeCap {
    pub trigger_pc: u32,
    pub channels: Vec<(String, u32, u8)>,
    pub rows: Vec<Vec<u32>>,
    pub max: usize,
}

/// A captured 68k register snapshot at a [`Scsp`] 68k breakpoint hit (sdbg `b68`).
#[derive(Clone, Debug, Default)]
pub struct M68kBpHit {
    pub pc: u32,
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub sr_imask: u8,
    pub sr_super: bool,
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
            budget_carry: 0,
            sample_acc: 0,
            out: Vec::new(),
            pc_trace: None,
            pc_seen: None,
            enq_log: None,
            itrace: None,
            scope: None,
            wwatch68: None,
            pcstream: None,
            bp68: None,
            bp68_hit: None,
            hle: None,
        }
    }

    /// Enable the opt-in native HLE sound driver (ADR-0012): a native sequencer
    /// replaces the LLE 68k driver, while the SCSP synthesis stays LLE. Idempotent.
    /// The LLE 68k remains the default + the oracle, so this is opt-in only.
    pub fn enable_hle_driver(&mut self) {
        self.hle.get_or_insert_with(hle::HleSoundDriver::default);
    }

    /// Disable the HLE sound driver, returning to the LLE 68k oracle (the default).
    pub fn disable_hle_driver(&mut self) {
        self.hle = None;
    }

    /// Whether the opt-in HLE sound driver is active.
    pub fn is_hle(&self) -> bool {
        self.hle.is_some()
    }

    /// Arm the instruction-lockstep PC stream (every 68k PC, hard-capped in
    /// [`Self::run`]). Drain with [`Self::take_pcstream`].
    pub fn enable_pcstream(&mut self) {
        self.pcstream = Some(Vec::with_capacity(1 << 20));
    }

    /// Take the captured 68k PC stream (PC + pre-instruction 68k cycle), if armed.
    pub fn take_pcstream(&mut self) -> Vec<(u32, u64)> {
        self.pcstream.take().unwrap_or_default()
    }

    /// Arm the 68k write-watch on sound-RAM byte `addr`: log `(pc, old, new)`
    /// each time a 68k instruction changes it. Drain with [`Self::take_wwatch68`].
    pub fn enable_wwatch68(&mut self, addr: u32) {
        let last = self.ram.read8(addr);
        self.wwatch68 = Some((addr, last, Vec::new()));
    }

    /// Take the 68k write-watch log `(pc, old, new)`, if armed.
    pub fn take_wwatch68(&mut self) -> Vec<(u32, u8, u8)> {
        self.wwatch68.take().map(|(_, _, v)| v).unwrap_or_default()
    }

    /// Arm the cross-emulator signal scope: at each 68k execution of
    /// `trigger_pc`, sample each `(name, sound-RAM addr, width)` channel into a
    /// row (capped at `max` rows). Drain with [`Self::take_scope`].
    pub fn enable_scope(&mut self, trigger_pc: u32, channels: Vec<(String, u32, u8)>, max: usize) {
        self.scope = Some(ScopeCap {
            trigger_pc,
            channels,
            rows: Vec::new(),
            max,
        });
    }

    /// Take the captured signal-scope rows, if armed.
    pub fn take_scope(&mut self) -> Option<ScopeCap> {
        self.scope.take()
    }

    /// Arm (or, with `None`, clear) a 68k breakpoint at `pc`, optionally guarded
    /// so it fires only when `reg == val` (reg 0-7 = D0-D7, 8-15 = A0-A7). Clears
    /// any pending hit. Debug-only; used by sdbg `b68` to break inside the SCSP
    /// sound driver (e.g. at the voice key-on code).
    pub fn set_bp68(&mut self, bp: Option<(u32, Option<(u8, u32)>)>) {
        self.bp68 = bp;
        self.bp68_hit = None;
    }

    /// Take the 68k breakpoint hit's register snapshot, if it fired.
    pub fn take_bp68_hit(&mut self) -> Option<M68kBpHit> {
        self.bp68_hit.take()
    }

    /// Begin recording a ring of recent 68k PCs (debug; see [`pc_trace`]).
    pub fn enable_68k_trace(&mut self) {
        self.pc_trace = Some(std::collections::VecDeque::new());
    }

    /// Drain the recorded 68k PC ring (oldest→newest), if enabled.
    pub fn take_68k_trace(&mut self) -> Vec<u32> {
        match &mut self.pc_trace {
            Some(t) => t.iter().copied().collect(),
            None => Vec::new(),
        }
    }

    /// Begin accumulating the set of every distinct 68k PC executed (debug; see
    /// [`pc_seen`]). Unlike the ring, this never forgets — for "did the driver
    /// ever reach routine X?" over a whole run.
    pub fn enable_68k_footprint(&mut self) {
        self.pc_seen = Some(std::collections::BTreeSet::new());
    }

    /// Snapshot the accumulated distinct-68k-PC footprint (sorted), if enabled.
    pub fn take_68k_footprint(&mut self) -> Vec<u32> {
        match &self.pc_seen {
            Some(s) => s.iter().copied().collect(),
            None => Vec::new(),
        }
    }

    /// Begin logging `[d0,d1,d2,d3,a6]` at every 68k execution of `pc` (debug;
    /// the BGM enqueue-stream diff). Capped internally to bound growth.
    pub fn enable_enq_log(&mut self, pc: u32) {
        self.enq_log = Some((pc, Vec::new()));
    }

    /// Snapshot the captured enqueue register log (oldest→newest), if enabled.
    pub fn take_enq_log(&mut self) -> Vec<[u32; 5]> {
        match &self.enq_log {
            Some((_, v)) => v.clone(),
            None => Vec::new(),
        }
    }

    /// Begin the aligned instruction-boundary value trace (see [`itrace`]).
    pub fn enable_68k_itrace(&mut self) {
        self.itrace = Some((false, 0, 0, 0, std::collections::VecDeque::new()));
    }

    /// Snapshot the frozen `(pc, cycle, d4, d7)` ring (oldest→newest), if enabled.
    pub fn take_68k_itrace(&mut self) -> Vec<(u32, u64, u32, u32)> {
        match &self.itrace {
            Some((.., v)) => v.iter().copied().collect(),
            None => Vec::new(),
        }
    }

    /// The seq-tick (`0x40F2`) count up to the first enqueue, if enabled — the
    /// 68k Timer-interrupt count to compare against the reference.
    pub fn take_68k_seq_ticks(&self) -> u32 {
        match &self.itrace {
            Some((_, n, ..)) => *n,
            None => 0,
        }
    }

    /// `(seq_ticks, sample_at_first_tick, sample_at_trigger)` at the BGM trigger,
    /// if enabled — the inputs to the Timer-B period (M12 task 2).
    pub fn take_68k_trigger_timing(&self) -> (u32, u64, u64) {
        match &self.itrace {
            Some((_, n, s_first, s_trig, _)) => (*n, *s_first, *s_trig),
            None => (0, 0, 0),
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

    /// Snapshot slot `i`'s playback parameters for debugging (see [`SlotDebug`]).
    pub fn slot_debug(&self, i: usize) -> SlotDebug {
        self.ctrl.slot_debug(i)
    }

    /// Debug: effect-DSP running flag + its EFREG output registers + per-index
    /// high-water mark (max |EFREG| ever written).
    pub fn dsp_state(&self) -> (bool, [i16; 16], [i32; 16], [i32; 16]) {
        self.ctrl.dsp_state()
    }

    /// Debug: EFREG indices the loaded DSP microprogram writes (see
    /// [`ScspCtrl::dsp_ewt_targets`]).
    pub fn dsp_ewt_targets(&self) -> Vec<u8> {
        self.ctrl.dsp_ewt_targets()
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
        // The SCSP sample/timer clock free-runs from power-on, *independent* of
        // the 68k's reset state: Mednafen advances one output sample (timer tick
        // + synthesis) per 256-cycle edge even while the SoundCPU is held halted
        // (`SOUND_Update`'s `while(next_scsp_time < run_until) RunSCSP()` else
        // branch). So compute this batch's 44.1 kHz sample quota *before* the
        // 68k-running gate — `sample_counter`, and therefore the Timer-A/B
        // prescale phase (`sc & ((1<<Control)-1)`), must track the **absolute**
        // sample clock, not be re-zeroed at each SNDON. Gating the whole function
        // on `running` (the old behaviour) froze the sample clock through the long
        // pre-SNDON window (audio-CD boot releases the 68k ~60+ frames in), so the
        // driver's Timer A overflowed at a mis-phased sample and its SCIPD-poll
        // wait-loops diverged from the oracle — the root the 68k instruction-
        // lockstep localised to the `move.w (0x420),d0; andi.w #0x40` poll.
        self.sample_frac += sh2_cycles.saturating_mul(SCSP_SAMPLE_HZ);
        let samples = (self.sample_frac / SH2_CLOCK_HZ) as u32;
        self.sample_frac %= SH2_CLOCK_HZ;

        // Opt-in native HLE sound driver (ADR-0012): bypass the 68k entirely. We
        // still produce every output sample (timer tick + mix) so audio + the
        // MCIPD/Timer-A path to the SH-2 are unaffected; the native sequencer is
        // driven on the Timer-B cadence inside `HleSoundDriver::tick`, and only
        // while the (virtual) driver is released — pre-SNDON the BIOS has not
        // staged the sequence yet, matching the LLE path. When `hle` is `None`
        // (the default) this branch is never taken and every path below is
        // byte-identical, so the `bios_boot` golden cannot move.
        if self.hle.is_some() {
            let active = self.running;
            let Scsp {
                ram,
                ctrl,
                out,
                hle,
                ..
            } = &mut *self;
            let hle = hle.as_mut().expect("hle.is_some() checked above");
            for _ in 0..samples {
                ctrl.tick_timers(1);
                if active {
                    hle.tick(ram, ctrl);
                }
                if out.len() < MAX_AUDIO_SAMPLES {
                    let (l, r) = ctrl.mix(ram);
                    out.push(l);
                    out.push(r);
                }
            }
            return;
        }

        if !self.running {
            // 68k held in reset: still advance the sample/timer clock + mixer so
            // the timers and `sample_counter` stay phase-locked for when the 68k
            // is released (Mednafen runs the full `RunSample` while halted).
            let Scsp { ram, ctrl, out, .. } = &mut *self;
            for _ in 0..samples {
                ctrl.tick_timers(1);
                if out.len() < MAX_AUDIO_SAMPLES {
                    let (l, r) = ctrl.mix(ram);
                    out.push(l);
                    out.push(r);
                }
            }
            return;
        }

        // How many 68k cycles this batch earns (the 68k clock is exactly 256× the
        // sample clock, 11.2896 MHz / 44100, so one sample falls due every 256
        // 68k cycles).
        self.frac += sh2_cycles.saturating_mul(SCSP_CLOCK_HZ);
        // Add back the previous batch's whole-instruction overshoot (a negative
        // carry) so the 68k does not creep ahead of the sample/timer clock.
        let mut budget = (self.frac / SH2_CLOCK_HZ) as i64 + self.budget_carry;
        self.frac %= SH2_CLOCK_HZ;

        // Per-sample interleave (Mednafen runs the sound 68k to each output-sample
        // edge — `RunSCSP` scheduled at sample edges): produce one sample (timer
        // tick + mixer) every 256 68k cycles, *interleaved with* the 68k stepping,
        // rather than lumping all the samples then all the 68k. The totals (sample
        // count, timer ticks, 68k cycles) are identical, so the verified Timer-B
        // rate (88.0 samples/seq-tick) is unchanged — only the 68k's phase against
        // the sample/timer clock is corrected (M13 A2 / M12 #3).
        const CYCLES_PER_SAMPLE_68K: i64 = (SCSP_CLOCK_HZ / SCSP_SAMPLE_HZ) as i64; // 256
        let mut samples_left = samples;
        let mut sample_acc: i64 = self.sample_acc; // carried across batches

        let Scsp {
            ram,
            ctrl,
            cpu,
            out,
            pc_trace,
            pc_seen,
            enq_log,
            itrace,
            scope,
            wwatch68,
            pcstream,
            bp68,
            bp68_hit,
            ..
        } = &mut *self;
        while budget > 0 {
            // Debug PC ring (collapses consecutive duplicates so a tight spin
            // doesn't flood it): records the 68k's execution path.
            if let Some(t) = pc_trace.as_mut()
                && t.back() != Some(&cpu.regs.pc)
            {
                t.push_back(cpu.regs.pc);
                if t.len() > 16384 {
                    t.pop_front();
                }
            }
            // Debug footprint: every distinct PC ever executed (unbounded).
            if let Some(s) = pc_seen.as_mut() {
                s.insert(cpu.regs.pc);
            }
            // Debug enqueue-stream log: capture the value regs at the watched PC.
            if let Some((wpc, log)) = enq_log.as_mut()
                && cpu.regs.pc == *wpc
                && log.len() < 8192
            {
                log.push([
                    cpu.regs.d[0],
                    cpu.regs.d[1],
                    cpu.regs.d[2],
                    cpu.regs.d[3],
                    cpu.regs.a[6],
                ]);
            }
            // Debug aligned instruction trace: dup-collapsed instruction PCs in
            // the seq-engine range, armed at the first enqueue (mirrors mednaref
            // SS_ITRACE for a lockstep interpreter diff).
            if let Some((frozen, seq_ticks, s_first, s_trig, ring)) = itrace.as_mut()
                && !*frozen
            {
                let pc = cpu.regs.pc;
                if pc == 0x40F2 {
                    if *seq_ticks == 0 {
                        *s_first = ctrl.sample_counter; // sample at the 1st seq-tick
                    }
                    *seq_ticks += 1; // count seq-tick entries until the enqueue
                }
                if (0x1000..0x5200).contains(&pc) {
                    // Per-instruction PC path across the whole driver (capped
                    // ring), to tail-align vs Mednafen and find where the paths
                    // into the first enqueue split.
                    ring.push_back((pc, cpu.cycles, cpu.regs.d[4], cpu.regs.d[7]));
                    if ring.len() > 6000 {
                        ring.pop_front();
                    }
                    if pc == 0x4B9A {
                        *s_trig = ctrl.sample_counter; // sample at the trigger
                        *frozen = true; // stop at the first enqueue
                    }
                }
            }
            // Instruction-lockstep PC stream: every 68k PC, in order, for a
            // line-for-line diff vs a reference trace from reset entry. Hard cap
            // (≈32 MB) bounds a headless run that never drains.
            if let Some(ps) = pcstream.as_mut()
                && ps.len() < 8_000_000
            {
                // PC + the pre-instruction 68k cycle, so the lockstep can compare
                // cost-per-instruction (delta of consecutive cycles) vs the oracle.
                ps.push((cpu.regs.pc, cpu.cycles));
            }
            // Signal scope: at the trigger PC, sample the configured sound-RAM
            // channels into a row (one row per timeframe).
            if let Some(sc) = scope.as_mut()
                && cpu.regs.pc == sc.trigger_pc
                && sc.rows.len() < sc.max
            {
                // Built-in time axis: the 68k accumulated cycle (low 32 bits) is
                // the first column of every row, so the scope shows *when* each
                // timeframe occurred — the X-axis. Lets the overlay tell a
                // tick-delivery (SCSP timer) divergence from a 68k-logic one.
                let mut row: Vec<u32> = vec![cpu.cycles as u32];
                row.extend(sc.channels.iter().map(|&(_, addr, w)| match w {
                    // width 0 = a checksum (sum of bytes) over a 0x100 region,
                    // for a coarse work-area sweep: any change in the region
                    // shows, so the overlay's first-divergence row points at the
                    // earliest divergent block to then zoom into.
                    0 => {
                        (0..0x100u32).fold(0u32, |s, k| s.wrapping_add(ram.read8(addr + k) as u32))
                    }
                    1 => ram.read8(addr) as u32,
                    2 => ram.read16(addr) as u32,
                    _ => ram.read32(addr),
                }));
                sc.rows.push(row);
            }
            // Debug 68k breakpoint: capture regs the first time the 68k is about
            // to execute the target PC (with any guard satisfied).
            if let Some((bp_pc, guard)) = bp68
                && cpu.regs.pc == *bp_pc
                && bp68_hit.is_none()
            {
                let pass = match guard {
                    Some((ri, v)) => {
                        let r = if (*ri as usize) < 8 {
                            cpu.regs.d[*ri as usize]
                        } else {
                            cpu.regs.a[(*ri as usize) & 7]
                        };
                        r == *v
                    }
                    None => true,
                };
                if pass {
                    *bp68_hit = Some(M68kBpHit {
                        pc: cpu.regs.pc,
                        d: cpu.regs.d,
                        a: cpu.regs.a,
                        sr_imask: cpu.regs.sr.imask,
                        sr_super: cpu.regs.sr.supervisor,
                    });
                }
            }
            // Present the level-triggered SCSP IRQ line at each boundary.
            cpu.pending_irq = ctrl.asserted_level;
            let pre_pc = cpu.regs.pc; // the instruction about to execute (for wwatch68)
            let mut bus = M68kView {
                ram: &mut *ram,
                ctrl: &mut *ctrl,
            };
            let cost = (cpu.step(&mut bus) as i64).max(1);
            budget -= cost;
            // 68k write-watch: did this instruction change the watched byte?
            if let Some((addr, last, log)) = wwatch68.as_mut() {
                let nv = ram.read8(*addr);
                if nv != *last {
                    if log.len() < 4096 {
                        log.push((pre_pc, *last, nv));
                    }
                    *last = nv;
                }
            }
            // Produce every output sample whose 256-cycle edge falls within this
            // 68k step — the timer ticks and the mixer run interleaved with the
            // 68k, not lumped before it.
            sample_acc += cost;
            while sample_acc >= CYCLES_PER_SAMPLE_68K && samples_left > 0 {
                sample_acc -= CYCLES_PER_SAMPLE_68K;
                samples_left -= 1;
                ctrl.tick_timers(1);
                if out.len() < MAX_AUDIO_SAMPLES {
                    let (l, r) = ctrl.mix(ram);
                    out.push(l);
                    out.push(r);
                }
            }
        }
        // Any samples the 68k budget didn't reach (budget < 256×samples) are
        // produced here, so the per-batch sample + timer totals stay exact (and
        // the rate is preserved when the 68k is idle/held short).
        while samples_left > 0 {
            samples_left -= 1;
            ctrl.tick_timers(1);
            if out.len() < MAX_AUDIO_SAMPLES {
                let (l, r) = ctrl.mix(ram);
                out.push(l);
                out.push(r);
            }
        }
        // Carry this batch's 68k-cycle overshoot (`budget` is now ≤ 0) into the
        // next batch so the 68k tracks 256 cy/sample exactly over time.
        self.budget_carry = budget;
        self.sample_acc = sample_acc;
    }
}

/// The 68k's memory view: sound RAM over `0x00_0000..0x0F_FFFF`, the SCSP
/// registers at `0x10_0000..0x10_0FFF`, open bus elsewhere.
struct M68kView<'a> {
    ram: &'a mut Ram,
    ctrl: &'a mut ScspCtrl,
}

/// SCSP sound-RAM/register access penalty charged to the 68k, in 68k clocks
/// **per 16-bit bus cycle** (a long = two cycles = `2 × WAIT`). The MC68EC000
/// has a 16-bit data bus; each access to the SCSP-arbitrated sound RAM (or the
/// SCSP register file) costs the 4-clock bus cycle the m68k core already charges
/// **plus this penalty**. Mirrors Mednafen `sound.cpp` `SoundCPU_BusRead/Write`,
/// which adds `timestamp += 2` after every `SCSP.RW` (data, instruction-fetch,
/// and write alike). Without it the sound 68k runs ~1.5× too fast — the root of
/// the BGM-trigger lead (it advances the sequence too quickly so the trigger
/// fires early and the per-voice divider lands at the wrong phase). Found by a
/// cycle-exact 68k lockstep vs Mednafen (`take_68k_itrace`).
const SCSP_ACCESS_WAIT: u32 = 2;

impl M68kView<'_> {
    #[inline]
    fn is_reg(addr: u32) -> bool {
        (0x10_0000..0x10_1000).contains(&addr)
    }
    /// True for a real SCSP access (sound RAM or register file) — the accesses
    /// that incur [`SCSP_ACCESS_WAIT`]; an out-of-range access is open bus.
    #[inline]
    fn is_scsp(addr: u32) -> bool {
        addr < 0x10_0000 || Self::is_reg(addr)
    }
}

impl Bus for M68kView<'_> {
    fn read8(&mut self, addr: u32, _: AccessKind) -> (u8, u32) {
        let w = if Self::is_scsp(addr) {
            SCSP_ACCESS_WAIT
        } else {
            0
        };
        if Self::is_reg(addr) {
            (self.ctrl.read8(addr - 0x10_0000), w)
        } else if addr < 0x10_0000 {
            (self.ram.read8(addr), w)
        } else {
            (0, 0)
        }
    }
    fn read16(&mut self, addr: u32, _: AccessKind) -> (u16, u32) {
        let w = if Self::is_scsp(addr) {
            SCSP_ACCESS_WAIT
        } else {
            0
        };
        if Self::is_reg(addr) {
            (self.ctrl.read16(addr - 0x10_0000), w)
        } else if addr < 0x10_0000 {
            (self.ram.read16(addr), w)
        } else {
            (0, 0)
        }
    }
    fn read32(&mut self, addr: u32, _: AccessKind) -> (u32, u32) {
        // A long is two 16-bit bus cycles on the 68000 → two penalties.
        let w = if Self::is_scsp(addr) {
            2 * SCSP_ACCESS_WAIT
        } else {
            0
        };
        if Self::is_reg(addr) {
            (self.ctrl.read32(addr - 0x10_0000), w)
        } else if addr < 0x10_0000 {
            (self.ram.read32(addr), w)
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
        if Self::is_scsp(addr) {
            SCSP_ACCESS_WAIT
        } else {
            0
        }
    }
    fn write16(&mut self, addr: u32, val: u16, _: AccessKind) -> u32 {
        if Self::is_reg(addr) {
            self.ctrl.write16(addr - 0x10_0000, val);
        } else if addr < 0x10_0000 {
            self.ram.write16(addr, val);
        }
        if Self::is_scsp(addr) {
            SCSP_ACCESS_WAIT
        } else {
            0
        }
    }
    fn write32(&mut self, addr: u32, val: u32, _: AccessKind) -> u32 {
        if Self::is_reg(addr) {
            self.ctrl.write32(addr - 0x10_0000, val);
        } else if addr < 0x10_0000 {
            self.ram.write32(addr, val);
        }
        if Self::is_scsp(addr) {
            2 * SCSP_ACCESS_WAIT
        } else {
            0
        }
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

    #[test]
    fn timer_free_runs_at_period_256_not_auto_reload() {
        // Mednafen model: `TIMx` loads the counter only on *write*; thereafter
        // the 8-bit counter free-runs (interrupt at 0xFF, wrap to 0x00), so the
        // steady-state period is **256** samples — NOT the `256 - reload` of an
        // auto-reload timer (our old, wrong model). This is the difference that
        // keeps the sound driver's timer-driven dividers in phase with the ref.
        let mut ctrl = ScspCtrl::new();
        ctrl.write16(SCILV0, INT_TIMER_A);
        ctrl.write16(SCIEB, INT_TIMER_A);
        ctrl.write16(TIMA, 0x00F0); // prescale 0, reload 0xF0
        // First overflow: load 0xF0, then count 0xF0→0xFF = clock 16.
        ctrl.tick_timers(15);
        assert_eq!(ctrl.read16(SCIPD) & INT_TIMER_A, 0, "not yet at 15 clocks");
        ctrl.tick_timers(1);
        assert_ne!(
            ctrl.read16(SCIPD) & INT_TIMER_A,
            0,
            "first overflow at clock 16"
        );
        ctrl.write16(SCIRE, INT_TIMER_A); // acknowledge
        // Second overflow: free-running 0xFF→0x00→…→0xFF = 256 clocks later, NOT
        // 16 (which an auto-reload-to-0xF0 timer would give).
        ctrl.tick_timers(255);
        assert_eq!(
            ctrl.read16(SCIPD) & INT_TIMER_A,
            0,
            "free-run: not yet at 255"
        );
        ctrl.tick_timers(1);
        assert_ne!(
            ctrl.read16(SCIPD) & INT_TIMER_A,
            0,
            "second overflow at period 256"
        );
    }

    #[test]
    fn timer_prescale_is_phase_locked_to_the_global_sample_clock() {
        // The `2^Control` prescale lands on samples where the global sample
        // counter's low `Control` bits are 0 — independent of when `TIMx` was
        // written (Mednafen `DoClock = !(SampleCounter & ((1<<Control)-1))`).
        let mut ctrl = ScspCtrl::new();
        ctrl.write16(SCILV0, INT_TIMER_A);
        ctrl.write16(SCIEB, INT_TIMER_A);
        // Advance the global sample clock by 3, *then* arm: prescale 2 (÷4) only
        // clocks on samples 4,8,12,… so from sample 3 the first edge is at 4.
        ctrl.tick_timers(3);
        ctrl.write16(TIMA, 0x02FF); // prescale 2 (÷4), reload 0xFF → fires on its first clock
        ctrl.tick_timers(1); // sample 3 → not a ÷4 edge
        assert_eq!(
            ctrl.read16(SCIPD) & INT_TIMER_A,
            0,
            "sample 3 is not a ÷4 edge"
        );
        ctrl.tick_timers(1); // sample 4 → a ÷4 edge → load 0xFF → overflow
        assert_ne!(
            ctrl.read16(SCIPD) & INT_TIMER_A,
            0,
            "clocks on the global ÷4 edge"
        );
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
    fn lfo_modulates_pitch_and_amplitude() {
        use std::collections::HashSet;
        let mut s = Scsp::new();
        // Active slot with a non-zero FNS (the PLFO depth scales with pitch).
        keyon_slot0(&mut s, 0, 0x100, 0, 0xFFFF, 0x0300); // OCT 0, FNS 0x300
        let reg9 = 0x12;
        let lfof_fast = 31u16 << 10; // LFOF 31 → fastest reload (1), counter ++/sample

        // --- PLFO (pitch): saw waveform, max depth (PLFOS=7, bits 7:5) ---
        s.ctrl.write16(reg9, lfof_fast | (7 << 5));
        let mut mods = Vec::new();
        for _ in 0..32 {
            s.ctrl.run_lfo(0);
            mods.push(s.ctrl.slots[0].step_mod);
        }
        assert!(
            mods.iter().any(|&m| m != 0),
            "PLFO produces a non-zero pitch delta"
        );
        assert!(
            mods.iter().collect::<HashSet<_>>().len() > 1,
            "PLFO modulates the pitch over the LFO phase"
        );

        // --- ALFO (amplitude): saw waveform, max depth (ALFOS=7, bits 2:0) ---
        // Re-key to reset the LFO phase/timer, then drive the amplitude LFO.
        keyon_slot0(&mut s, 0, 0x100, 0, 0xFFFF, 0x0300);
        s.ctrl.write16(reg9, lfof_fast | 7);
        let mut alfos = Vec::new();
        for _ in 0..32 {
            s.ctrl.run_lfo(0);
            alfos.push(s.ctrl.slots[0].alfo);
        }
        assert!(
            alfos.iter().any(|&a| a > 0),
            "ALFO produces a non-zero attenuation"
        );
        assert!(
            alfos.iter().all(|&a| a >= 0),
            "ALFO attenuation is never negative (it darkens, never brightens)"
        );
        assert!(
            alfos.iter().collect::<HashSet<_>>().len() > 1,
            "ALFO modulates the amplitude over the LFO phase"
        );

        // With both LFO depths 0, the deltas are 0 — the no-LFO path is unchanged.
        s.ctrl.write16(reg9, 0);
        s.ctrl.run_lfo(0);
        assert_eq!(s.ctrl.slots[0].step_mod, 0, "no PLFO → no pitch delta");
        assert_eq!(s.ctrl.slots[0].alfo, 0, "no ALFO → no attenuation offset");
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
    fn hle_driver_keys_a_voice_and_produces_audio() {
        // ADR-0012 M1: with the opt-in native HLE driver enabled, the sequence's
        // first note-on keys a voice through the (LLE) synthesis → non-silent
        // output — proving the HLE → synthesis boundary end-to-end, no 68k run.
        let mut s = Scsp::new();
        // Seed a non-zero PCM sample where the HLE keys it (the BGM sample SA),
        // and a note-on at the sequence base (high nibble 0x4, then note + vel).
        for i in 0..64u32 {
            let ph = i as f64 / 64.0 * std::f64::consts::TAU;
            s.ram
                .write16(0x10740 + i * 2, (ph.sin() * 0x4000 as f64) as i16 as u16);
        }
        s.ram.write8(0x18200, 0x40); // note-on, channel 0
        s.ram.write8(0x18201, 0x37); // note number
        s.ram.write8(0x18202, 0x64); // velocity
        s.enable_hle_driver();
        assert!(s.is_hle());
        s.start(); // SNDON → the HLE sequencer is released
        s.run(2_000_000); // many SH-2 cycles → ticks + fills the audio buffer
        let audio = s.take_audio();
        assert!(!audio.is_empty(), "HLE produced audio samples");
        assert!(
            audio.iter().any(|&x| x != 0),
            "HLE keyed a voice → non-silent"
        );
        assert_eq!(s.ctrl.dbg_keyon_counts().1, 1, "exactly one slot started");
    }

    #[test]
    fn hle_enable_then_disable_is_a_noop_on_the_lle_path() {
        // Golden-safety: enabling then disabling the HLE must leave the LLE 68k
        // path byte-identical to one that never touched it (the default oracle).
        let setup = |s: &mut Scsp| {
            s.ram.write32(4, 0x2000);
            s.ram.write16(0x2000, 0x60FE); // 68k BRA self
            keyon_panned(s, 0, 0x2000, 7, 0x00);
        };
        let mut a = Scsp::new();
        setup(&mut a);
        a.start();
        a.run(2_000_000);

        let mut b = Scsp::new();
        setup(&mut b);
        b.enable_hle_driver();
        b.disable_hle_driver();
        assert!(!b.is_hle(), "HLE disabled → back to the LLE oracle");
        b.start();
        b.run(2_000_000);

        assert_eq!(
            a.take_audio(),
            b.take_audio(),
            "enable→disable HLE leaves the LLE output byte-identical"
        );
    }

    #[test]
    fn keyon_is_edge_triggered_no_restart_of_a_playing_slot() {
        // A KYONEX strobe with KYONB still set must NOT restart an already-
        // playing slot (Mednafen's edge guard, `scsp.inc:1496`). The old code
        // called `start_slot` for every KYONB=1 slot on every strobe, so BIOS
        // menu SFX — which the BIOS re-strobes with KYONB still set — piled up at
        // full volume across all 32 slots (stuck in Decay2) and clipped to a
        // growing buzz.
        let mut s = Scsp::new();
        keyon_panned(&mut s, 0, 0x4000, 7, 0x00); // key slot 0
        assert!(s.ctrl.slots[0].active, "slot 0 keyed on");
        let (_, starts1) = s.ctrl.dbg_keyon_counts();
        assert_eq!(starts1, 1, "exactly one slot start so far");
        // Re-strobe KYONEX (bit 12) with KYONB (bit 11) still set: no restart.
        s.ctrl.write16(0, 0x1800);
        let (_, starts2) = s.ctrl.dbg_keyon_counts();
        assert_eq!(
            starts2, 1,
            "re-strobe must not restart the already-playing slot"
        );
        // A genuine key-off (KYONB=0) still releases the slot.
        s.ctrl.write16(0, 0x1000); // KYONEX only, KYONB clear
        assert_eq!(
            s.ctrl.slots[0].eg.state,
            EgState::Release,
            "key-off releases the playing slot"
        );
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
