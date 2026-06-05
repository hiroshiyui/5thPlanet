//! Opt-in native HLE of the SEGA Saturn BIOS 68k sound driver (ADR-0012).
//!
//! The SCSP **synthesis** stays LLE — it is cross-validated to 0.2 % steady-state
//! vs Mednafen, including the slot-to-slot FM phase modulation (see
//! [`super::ScspCtrl::fm_modalizer`]). Only the hosted MC68EC000 *driver* — the
//! program that walks the BGM sequence and programs the SCSP slots — is
//! reimplemented natively here. It is **opt-in** ([`super::Scsp::enable_hle_driver`]);
//! the LLE 68k driver is the default and remains the oracle. When active,
//! [`super::Scsp::run`] skips the 68k entirely and drives this sequencer on the
//! sample / Timer-B cadence, while still producing every 44.1 kHz output sample.
//!
//! **What it reproduces** (reverse-engineered from the audio-CD CD-player driver,
//! `doc/bios-bgm-diagnosis.md`, cross-checked against Mednafen `SS_KEYON`/`SS_SEQREAD`
//! + the driver disassembly at `0x4D22`/`0x4E94`):
//!
//! * The custom MIDI-like **sequence** at sound-RAM `0x18200`: per-event the parser
//!   reads a status byte — `< 0x80` = note-on (5 bytes: status, note, velocity,
//!   gate, delta; channel = `status & 0x1F`), `0xBn` = control-change (4 bytes),
//!   `0xCn` = program-change (3 bytes), `0xEn` = pitch-bend (3 bytes), `0x83` =
//!   end-of-tick — and advances time by the trailing delta on the Timer-B cadence.
//! * Each note keys a **4-operator FM voice**: voice `v` (round-robin, 0..8) owns
//!   slots `{v, v+8, v+16, v+24}` — three modulators feeding one audible carrier
//!   (slot `v+24`). The operators' pitch follows the note (sample base-note 55,
//!   `2^(semitones/12)`) plus a per-operator semitone offset; their level, FM
//!   routing (reg 7 `MDL`/`MDXSL`/`MDYSL`) and envelope come from the instrument
//!   bank captured from the reference.

use super::{Ram, SLOT_STRIDE, ScspCtrl};

/// Timer-B sequence-tick period, in 44.1 kHz samples. The BIOS driver reloads
/// `TIMB` so its sequence ISR fires at ~this rate (measured ~88 samples/tick); the
/// HLE tracks the cadence itself, since the 68k driver that would reload `TIMB` no
/// longer runs.
const TIMER_B_PERIOD: u32 = 88;

/// Sound-RAM byte offset where the BGM sequence's *playback* events begin (after
/// the per-channel setup header), and the BGM voices' shared PCM sample.
const SEQ_START: u32 = 0x18226;
const BGM_SAMPLE_SA: u32 = 0x10740;
const BGM_LSA: u16 = 0x00A9;
const BGM_LEA: u16 = 0x0152;

/// MIDI note that plays [`BGM_SAMPLE_SA`] at its native rate (OCT/FNS = 0). Solved
/// from the reference: the carrier OCT/FNS fit `2^((note-55)/12)` exactly.
const SAMPLE_BASE_NOTE: i32 = 55;

/// One FM operator of an instrument: a semitone offset from the played note plus
/// the SCSP slot config the driver programs (level, FM modulation, envelope, and
/// whether it sends to the direct output — only the carrier does).
#[derive(Clone, Copy)]
struct Operator {
    note_off: i32,
    tl: u16,      // reg 6 total level
    mdl: u16,     // reg 7 modulation level (>4 → FM)
    mdx: u16,     // reg 7 MDXSL
    mdy: u16,     // reg 7 MDYSL
    ar: u16,      // reg 4 attack rate
    d1r: u16,     // reg 4 decay-1 rate
    d2r: u16,     // reg 4 decay-2 rate
    rr: u16,      // reg 5 release rate
    direct: bool, // reg 0xB: routes to the audible direct output (the carrier)
}

/// The BIOS CD-player BGM instrument (program 7): three modulators (a fifth above
/// the note, at three octaves) feeding one carrier. Captured per-operator from
/// Mednafen `SS_KEYON`. Operators map to slots `{v, v+8, v+16, v+24}` of a voice.
const INSTRUMENT_PROG7: [Operator; 4] = [
    Operator {
        note_off: -5,
        tl: 0x04,
        mdl: 0x7,
        mdx: 0x00,
        mdy: 0x00,
        ar: 0x00,
        d1r: 0x00,
        d2r: 0x00,
        rr: 0x14,
        direct: false,
    },
    Operator {
        note_off: 7,
        tl: 0x04,
        mdl: 0x9,
        mdx: 0x18,
        mdy: 0x18,
        ar: 0x1F,
        d1r: 0x00,
        d2r: 0x05,
        rr: 0x14,
        direct: false,
    },
    Operator {
        note_off: -17,
        tl: 0x52,
        mdl: 0x7,
        mdx: 0x18,
        mdy: 0x18,
        ar: 0x04,
        d1r: 0x11,
        d2r: 0x05,
        rr: 0x14,
        direct: false,
    },
    Operator {
        note_off: 0,
        tl: 0x15,
        mdl: 0xA,
        mdx: 0x08,
        mdy: 0x38,
        ar: 0x08,
        d1r: 0x00,
        d2r: 0x00,
        rr: 0x16,
        direct: true,
    },
];

/// Native reimplementation of the 68k sound driver (ADR-0012).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct HleSoundDriver {
    /// Samples accumulated toward the next Timer-B sequence tick.
    tick_acc: u32,
    /// Whether the sequence has been armed since the driver was released.
    started: bool,
    /// Read cursor into the sound-RAM sequence.
    seq_pos: u32,
    /// Timer-B ticks remaining before the next event is processed (delta-time).
    delta: u32,
    /// The parser hit an event it doesn't model yet — stop rather than desync.
    stopped: bool,
    /// Per-channel program (instrument), set by program-change.
    program: [u8; 16],
    /// Round-robin next voice index (0..8).
    next_voice: u8,
}

impl HleSoundDriver {
    /// Advance one 44.1 kHz output sample; fire a sequence tick on the Timer-B
    /// cadence. Called per produced sample from [`super::Scsp::run`]'s HLE branch,
    /// only while the driver is released (after SMPC `SNDON`).
    pub(crate) fn tick(&mut self, ram: &mut Ram, ctrl: &mut ScspCtrl) {
        self.tick_acc += 1;
        if self.tick_acc < TIMER_B_PERIOD {
            return;
        }
        self.tick_acc = 0;
        self.seq_tick(ram, ctrl);
    }

    /// One Timer-B tick: count down the delta-time, and when it elapses, process
    /// the sequence's next run of (simultaneous) events.
    fn seq_tick(&mut self, ram: &mut Ram, ctrl: &mut ScspCtrl) {
        if self.stopped {
            return;
        }
        if !self.started {
            // Wait until the BIOS has staged the sequence (a note-on appears).
            if ram.read8(SEQ_START) == 0 && ram.read8(SEQ_START + 1) == 0 {
                return;
            }
            self.started = true;
            self.seq_pos = SEQ_START;
        }
        if self.delta > 0 {
            self.delta -= 1;
            return;
        }
        // Process events until one carries a non-zero delta (advance time), the
        // tick ends (0x83), or an unmodelled event is hit.
        for _ in 0..64 {
            let status = ram.read8(self.seq_pos);
            if status < 0x80 {
                // Note-on: status, note, velocity, gate, delta.
                let chan = (status & 0x1F) as usize;
                let note = ram.read8(self.seq_pos + 1) as i32;
                let delta = ram.read8(self.seq_pos + 4) as u32;
                self.seq_pos += 5;
                self.key_voice(ctrl, chan, note);
                if delta > 0 {
                    self.delta = delta;
                    return;
                }
            } else if status & 0xF0 == 0xB0 {
                // Control-change: status, controller, value, delta.
                let delta = ram.read8(self.seq_pos + 3) as u32;
                self.seq_pos += 4;
                if delta > 0 {
                    self.delta = delta;
                    return;
                }
            } else if status & 0xF0 == 0xC0 {
                // Program-change: status, program, delta.
                let chan = (status & 0x1F) as usize;
                self.program[chan & 0xF] = ram.read8(self.seq_pos + 1);
                let delta = ram.read8(self.seq_pos + 2) as u32;
                self.seq_pos += 3;
                if delta > 0 {
                    self.delta = delta;
                    return;
                }
            } else if status & 0xF0 == 0xE0 {
                // Pitch-bend: status, value, delta.
                let delta = ram.read8(self.seq_pos + 2) as u32;
                self.seq_pos += 3;
                if delta > 0 {
                    self.delta = delta;
                    return;
                }
            } else if status == 0x83 {
                // End-of-tick marker.
                self.seq_pos += 1;
                return;
            } else {
                // An event the parser doesn't model yet — stop cleanly.
                self.stopped = true;
                return;
            }
        }
    }

    /// Key a 4-operator FM voice for `note` (the instrument is the channel's
    /// program). Allocates the next round-robin voice and programs its four
    /// operator slots `{v, v+8, v+16, v+24}`, keying them together.
    fn key_voice(&mut self, ctrl: &mut ScspCtrl, _chan: usize, note: i32) {
        let v = self.next_voice as u32;
        self.next_voice = (self.next_voice + 1) & 7;
        let instr = &INSTRUMENT_PROG7; // the only RE'd instrument so far
        for (op_i, op) in instr.iter().enumerate() {
            let slot = v + op_i as u32 * 8;
            program_operator(ctrl, slot, note, op);
        }
        // Strobe KYONEX on the carrier (slot v+24) — keys all four (they share
        // KYONB), edge-triggered across the slots.
        let carrier = v + 24;
        let hi = ((BGM_SAMPLE_SA >> 16) & 0xF) as u16;
        ctrl.write16(carrier * SLOT_STRIDE, 0x1800 | 0x0020 | hi);
    }
}

/// Convert a MIDI note to the SCSP reg-8 `OCT|FNS` pitch word, relative to the
/// sample's base note ([`SAMPLE_BASE_NOTE`]). `OCT` (4-bit signed, bits 14-11) is
/// the octave; `FNS` (bits 9-0) the fine fraction `round(1024·(2^(frac/12)−1))`.
fn note_to_octfns(note: i32) -> u16 {
    let semis = note - SAMPLE_BASE_NOTE;
    let oct = semis.div_euclid(12).clamp(-8, 7);
    let frac = semis.rem_euclid(12);
    let fns = (1024.0 * (2f64.powf(frac as f64 / 12.0) - 1.0)).round() as u16 & 0x3FF;
    (((oct & 0xF) as u16) << 11) | fns
}

/// Program one SCSP slot as an FM operator and arm it (KYONB, but not KYONEX —
/// the caller strobes KYONEX once to key the whole voice). Writes the full slot
/// register set the 68k driver would: sample/loop, envelope, FM routing (reg 7),
/// pitch, and the direct send (carrier only).
fn program_operator(ctrl: &mut ScspCtrl, slot: u32, note: i32, op: &Operator) {
    let base = slot * SLOT_STRIDE;
    ctrl.write16(base + 0x02, BGM_SAMPLE_SA as u16); // SA low
    ctrl.write16(base + 0x04, BGM_LSA); // LSA
    ctrl.write16(base + 0x06, BGM_LEA); // LEA
    ctrl.write16(base + 0x08, op.ar | (op.d1r << 6) | (op.d2r << 11)); // reg 4: AR/D1R/D2R
    ctrl.write16(base + 0x0A, op.rr); // reg 5: RR (DL/KRS = 0)
    ctrl.write16(base + 0x0C, op.tl); // reg 6: TL
    ctrl.write16(base + 0x0E, (op.mdl << 12) | (op.mdx << 6) | op.mdy); // reg 7: FM
    ctrl.write16(base + 0x10, note_to_octfns(note + op.note_off)); // reg 8: pitch
    ctrl.write16(base + 0x12, 0x0000); // reg 9: LFO off
    ctrl.write16(base + 0x14, 0x0000); // reg 10: no DSP send
    ctrl.write16(base + 0x16, if op.direct { 0xE000 } else { 0x0000 }); // reg 0xB: DISDL/pan
    // reg 0: KYONB | forward-loop | SA-hi (no KYONEX — the carrier strobes it).
    let hi = ((BGM_SAMPLE_SA >> 16) & 0xF) as u16;
    ctrl.write16(base, 0x0800 | 0x0020 | hi);
}
