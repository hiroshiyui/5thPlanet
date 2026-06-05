//! Opt-in native HLE of the SEGA Saturn BIOS 68k sound driver (ADR-0012).
//!
//! The SCSP **synthesis** stays LLE — it is cross-validated to 0.2 % steady-state
//! vs Mednafen (the sine test ROM in `crates/saturn/tests/audio_pipeline.rs`), so
//! there is nothing to high-level-emulate there. Only the hosted MC68EC000
//! *driver* — the program that parses the BGM sequence and programs the SCSP
//! slots — is reimplemented natively in this module. It is **opt-in**
//! ([`super::Scsp::enable_hle_driver`]); the LLE 68k driver is the default and
//! remains the oracle. When active, [`super::Scsp::run`] skips the 68k entirely
//! and drives this sequencer on the sample / Timer-B cadence, while still
//! producing every 44.1 kHz output sample and ticking the hardware timers (so
//! audio + the main-CPU sound interrupt are unaffected).
//!
//! **Milestone 1 (this module's current scope):** prove the HLE → LLE-synthesis
//! boundary end-to-end by keying a single voice from the sequence's first
//! note-on. The full delta-time sequence player, the 32-slot voice allocator,
//! the CC/pitch-bend handling and the instrument bank are M2–M5 (see the ADR and
//! `doc/bios-bgm-diagnosis.md`).

use super::{Ram, SLOT_STRIDE, ScspCtrl};

/// Timer-B sequence-tick period, in 44.1 kHz samples. The BIOS driver reloads
/// `TIMB` so its sequence ISR fires at ~this rate (measured ~88 samples/tick);
/// the HLE tracks the cadence itself, because the 68k driver that would reload
/// `TIMB` no longer runs. Refined against Mednafen `SS_KYONEX` tick stamps in M2+.
const TIMER_B_PERIOD: u32 = 88;

/// Sound-RAM byte offset of the BGM sequence the BIOS stages for the CD-player
/// panel. The data here is byte-identical to Mednafen (`doc/bios-bgm-diagnosis.md`):
/// a MIDI-like stream of delta-times + status bytes (`0xBn` CC, `0x4n` note-on,
/// `0xCn` program-change, `0xEn` pitch-bend).
const SEQ_BASE: u32 = 0x18200;

/// Sound-RAM byte offset of the first BGM voice's PCM sample — the slot SA
/// Mednafen keys at the panel-BGM start (`doc/bios-bgm-diagnosis.md`). M1 keys a
/// voice against this; M2–M5 derive each note's SA from the instrument bank.
const BGM_SAMPLE_SA: u32 = 0x10740;

/// Native reimplementation of the 68k sound driver (ADR-0012). Holds only M1
/// state for now (the cadence accumulator + a one-shot guard); M2+ grow it with
/// the sequence cursor, the 16-channel MIDI state and the voice allocator.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct HleSoundDriver {
    /// Samples accumulated toward the next Timer-B sequence tick.
    tick_acc: u32,
    /// M1: whether the single demonstration voice has been keyed yet.
    started: bool,
}

impl HleSoundDriver {
    /// Advance one 44.1 kHz output sample, firing a sequencer tick on the Timer-B
    /// cadence. Called once per produced sample from [`super::Scsp::run`]'s HLE
    /// branch, only while the driver is "released" (i.e. after SMPC `SNDON`).
    pub(crate) fn tick(&mut self, ram: &mut Ram, ctrl: &mut ScspCtrl) {
        self.tick_acc += 1;
        if self.tick_acc < TIMER_B_PERIOD {
            return;
        }
        self.tick_acc = 0;
        self.seq_tick(ram, ctrl);
    }

    /// One Timer-B tick of the sequencer. **M1:** key ONE voice from the
    /// sequence's first note-on, proving the HLE → synthesis boundary. M2 replaces
    /// this with the full delta-time event interpreter.
    fn seq_tick(&mut self, ram: &mut Ram, ctrl: &mut ScspCtrl) {
        if self.started {
            return;
        }
        let Some((note, _vel)) = first_note_on(ram) else {
            return; // the BIOS hasn't staged the sequence yet — wait for it
        };
        self.started = true;
        program_voice(ctrl, 0, BGM_SAMPLE_SA, note);
    }
}

/// Scan the head of the sequence for the first note-on (event high nibble `0x4`,
/// low nibble = channel), returning `(note, velocity)`. A deliberate M1 stub for
/// the full delta-time parser (M2).
fn first_note_on(ram: &Ram) -> Option<(u8, u8)> {
    for off in 0..64u32 {
        if ram.read8(SEQ_BASE + off) & 0xF0 == 0x40 {
            let note = ram.read8(SEQ_BASE + off + 1);
            let vel = ram.read8(SEQ_BASE + off + 2);
            if vel != 0 {
                return Some((note, vel));
            }
        }
    }
    None
}

/// Program SCSP slot `i` for a looped PCM note at sample address `sa` and key it
/// on — the native equivalent of the 68k driver's slot-register writes. Mirrors
/// the proven recipe in `crates/saturn/tests/audio_pipeline.rs` and the
/// `keyon_slot0` test helper: write the config words, then word 0
/// (`KYONEX|KYONB|LPCTL|SA-hi`) **last** to trigger key-on across the slots. M1
/// plays at native rate (OCT/FNS = 0); M2–M4 set pitch/level/pan from the note +
/// channel state.
fn program_voice(ctrl: &mut ScspCtrl, i: usize, sa: u32, _note: u8) {
    let base = i as u32 * SLOT_STRIDE;
    ctrl.write16(base + 0x02, sa as u16); // SA low
    ctrl.write16(base + 0x04, 0x0000); // LSA = 0
    ctrl.write16(base + 0x06, 0x7FFF); // LEA (long sustain for M1)
    ctrl.write16(base + 0x08, 0x001F); // AR = max, D1R/D2R = 0 → hold full
    ctrl.write16(base + 0x0A, 0x0000);
    ctrl.write16(base + 0x0C, 0x0000); // TL = 0 (full level)
    ctrl.write16(base + 0x10, 0x0000); // OCT/FNS = 0 (native playback rate)
    ctrl.write16(base + 0x14, 0x0000); // ISEL/IMXL = 0 (no DSP send)
    ctrl.write16(base + 0x16, 0xE000); // DISDL = 7, DIPAN = centre
    // word 0 last: KYONEX | KYONB | LPCTL=forward-loop (0x20) | SA-high nibble.
    let hi = ((sa >> 16) & 0xF) as u16;
    ctrl.write16(base, 0x1800 | 0x0020 | hi);
}
