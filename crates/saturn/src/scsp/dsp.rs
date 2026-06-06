//! SCSP DSP — the 128-step effect microprogram (reverb / echo / EQ).
//!
//! A small VLIW signal processor: each of up to 128 steps is a 64-bit
//! instruction (`MPRO`) running a multiply-accumulate over 24-bit data, with
//! a 13-bit coefficient table (`COEF`), temp registers (`TEMP`), memory I/O
//! registers (`MEMS`), and a delay line in sound RAM addressed via `MADRS`.
//! The mixer feeds slot effect-sends into the input mix (`MIXS`) and reads the
//! effect outputs (`EFREG`) back out.
//!
//! Ported from MAME's `scspdsp.cpp` (the algorithm follows the SCSP DSP
//! documentation). Coefficients/program/data are loaded by the CPU through the
//! SCSP register bank; `step` runs one full pass per output sample.

use crate::memory::Ram;
use serde_big_array::BigArray;

const SOUND_RAM_MASK: u32 = (super::SOUND_RAM_BYTES as u32) - 1;

/// Sign-extend the low `bits` of `v`.
#[inline]
fn sext(v: i32, bits: u32) -> i32 {
    let s = 32 - bits;
    (v << s) >> s
}

/// Expand the SCSP's 16-bit "floating" delay-RAM word to a 24-bit value
/// (Mednafen `scsp.inc` `dspfloat_to_int`): 11-bit mantissa, 4-bit exponent,
/// sign — the exponent is a right-shift recovered on read.
fn dspfloat_to_int(inv: u16) -> i32 {
    // (int32)((inv & 0x8000) << 16) >> 1  ==  0 or 0xC000_0000.
    let sign_xor: u32 = if inv & 0x8000 != 0 { 0xC000_0000 } else { 0 };
    let exp = ((inv >> 11) & 0xF) as u32;
    let mut ret = (inv & 0x7FF) as u32;
    if exp < 12 {
        ret |= 0x800;
    }
    ret <<= 19; // 11 + 8
    ret ^= sign_xor;
    let shifted = (ret as i32) >> (8 + exp.min(11));
    shifted & 0xFF_FFFF
}

/// Compress a 24-bit value to the SCSP's 16-bit floating delay-RAM word
/// (Mednafen `scsp.inc` `int_to_dspfloat`).
fn int_to_dspfloat(inv: i32) -> u16 {
    let invsl8 = (inv as u32) << 8;
    let sign_xor = ((invsl8 as i32) >> 31) as u32; // 0 or 0xFFFF_FFFF
    let exp = (((invsl8 ^ sign_xor) << 1) | (1 << 19)).leading_zeros();
    let shift = exp - if exp == 12 { 1 } else { 0 };
    let mut ret = ((invsl8 as i32) >> (19 - shift)) as u32;
    ret &= 0x87FF;
    ret |= exp << 11;
    ret as u16
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Dsp {
    #[serde(with = "BigArray")]
    pub coef: [i16; 64],
    pub madrs: [u16; 32],
    #[serde(with = "BigArray")]
    pub mpro: [u16; 128 * 4],
    #[serde(with = "BigArray")]
    pub temp: [i32; 128],
    pub mems: [i32; 32],
    pub mixs: [i32; 16],
    pub exts: [i16; 2],
    pub efreg: [i16; 16],
    /// Debug high-water mark: max |EFREG[i]| ever written (never reset). Lets
    /// sdbg see which effect outputs the DSP actually produces, past the
    /// zero-crossings that frame-boundary snapshots catch.
    #[serde(skip)]
    pub efreg_hw: [i32; 16],
    /// Debug high-water mark of the DSP input mix (max |MIXS[i]| seen).
    #[serde(skip)]
    pub mixs_hw: [i32; 16],
    pub rbp: u32,
    pub rbl: u32,
    dec: u32,
    stopped: bool,
    last_step: usize,
    /// Pipeline registers that carry across steps *and* samples (Mednafen
    /// `SS_SCSP::DSP` fields). `sft_reg` is the 26-bit accumulator; the shifter
    /// reads the *previous* step's value. `inputs_reg` is the raw INPUTS latch
    /// (IRA 0x32-0x3F leave it unchanged). The pending-read/write pair models the
    /// delay-RAM access *latency*: the address is computed on the MRT/MWT step,
    /// the access resolves on a later step.
    sft_reg: i32,
    inputs_reg: i32,
    frc_reg: i32,
    y_reg: i32,
    adrs_reg: u16,
    read_pending: u8,
    read_value: i32,
    write_pending: bool,
    write_value: u16,
    rw_addr: u32,
}

impl Default for Dsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Dsp {
    pub fn new() -> Self {
        Self {
            coef: [0; 64],
            madrs: [0; 32],
            mpro: [0; 128 * 4],
            temp: [0; 128],
            mems: [0; 32],
            mixs: [0; 16],
            exts: [0; 2],
            efreg: [0; 16],
            efreg_hw: [0; 16],
            mixs_hw: [0; 16],
            rbp: 0,
            rbl: 8 * 1024,
            dec: 0,
            stopped: true,
            last_step: 0,
            sft_reg: 0,
            inputs_reg: 0,
            frc_reg: 0,
            y_reg: 0,
            adrs_reg: 0,
            read_pending: 0,
            read_value: 0,
            write_pending: false,
            write_value: 0,
            rw_addr: 0,
        }
    }

    /// Recompute the program length (highest non-zero step) and un-stop the
    /// DSP. Called after the microprogram is (re)loaded.
    pub fn start(&mut self) {
        self.stopped = false;
        self.last_step = 0;
        for i in (0..128).rev() {
            let p = &self.mpro[i * 4..i * 4 + 4];
            if p[0] | p[1] | p[2] | p[3] != 0 {
                self.last_step = i + 1;
                break;
            }
        }
    }

    /// Whether a microprogram is loaded and the DSP is processing.
    pub fn running(&self) -> bool {
        !self.stopped && self.last_step > 0
    }

    /// Add a slot's effect-send (already scaled by the caller) into input-mix
    /// channel `sel`. `MIXS` accumulates **modulo 20 bits** (Mednafen `scsp.inc`
    /// `DSP.MIXS[idx] = (... ) & 0xFFFFF`); without the wrap an over-range sum
    /// feeds the DSP a value `inputs = MIXS << 4` sign-extends wrongly, which
    /// saturates the reverb feedback into self-oscillation.
    pub fn set_sample(&mut self, sample: i32, sel: usize) {
        let sel = sel & 0xF;
        self.mixs[sel] = self.mixs[sel].wrapping_add(sample) & 0xF_FFFF;
        let a = self.mixs[sel].abs();
        if a > self.mixs_hw[sel] {
            self.mixs_hw[sel] = a;
        }
    }

    /// Run one full 128-step pass for one output sample, using `ram` as the
    /// delay line. Modeled faithfully on Mednafen's `SS_SCSP` DSP step
    /// (`scsp.inc`): the shifter reads the *previous* step's 26-bit accumulator;
    /// the Y-select value is captured *before* YRL/FRCL latch their new values;
    /// delay-RAM access is *deferred* (the address is latched on the MRT/MWT step
    /// and the access resolves a step later); and the pipeline registers persist
    /// across samples. `MIXS` is cleared afterward (consumed per sample).
    pub fn step(&mut self, ram: &mut Ram) {
        if self.stopped || self.last_step == 0 {
            self.mixs.fill(0);
            return;
        }
        self.efreg.fill(0);
        let rbl_mask = self.rbl.wrapping_sub(1);

        for step in 0..128 {
            let ip = &self.mpro[step * 4..step * 4 + 4];
            let tra = ((ip[0] >> 8) & 0x7F) as u32;
            let twt = ip[0] >> 7 & 1;
            let twa = (ip[0] & 0x7F) as u32;
            let xsel = ip[1] >> 15 & 1;
            let ysel = ip[1] >> 13 & 3;
            let ira = (ip[1] >> 6 & 0x3F) as usize;
            let iwt = ip[1] >> 5 & 1;
            let iwa = (ip[1] & 0x1F) as usize;
            let table = ip[2] >> 15 & 1;
            let mwt = ip[2] >> 14 & 1;
            let mrt = ip[2] >> 13 & 1;
            let ewt = ip[2] >> 12 & 1;
            let ewa = (ip[2] >> 8 & 0xF) as usize;
            let adrl = ip[2] >> 7 & 1;
            let frcl = ip[2] >> 6 & 1;
            let shft0 = ip[2] >> 4 & 1;
            let shft1 = ip[2] >> 5 & 1;
            let yrl = ip[2] >> 3 & 1;
            let negb = ip[2] >> 2 & 1;
            let zero = ip[2] >> 1 & 1;
            let bsel = ip[2] & 1;
            let nofl = ip[3] >> 15 & 1;
            let cra = (ip[3] >> 9 & 0x3F) as usize;
            let masa = (ip[3] >> 2 & 0x1F) as usize;
            let adrgb = ip[3] >> 1 & 1;
            let nxaddr = ip[3] & 1;

            // INPUTS latch (raw). IRA 0x32-0x3F leave the latch unchanged.
            if ira & 0x20 != 0 {
                if ira & 0x10 != 0 {
                    if ira & 0xE == 0 {
                        self.inputs_reg = (self.exts[ira & 1] as i32) << 8;
                    }
                } else {
                    self.inputs_reg = self.mixs[ira & 0xF] << 4;
                }
            } else {
                self.inputs_reg = self.mems[ira & 0x1F];
            }
            let inputs = sext(self.inputs_reg, 24);

            // Capture the Y-select operand *before* YRL/FRCL latch new values.
            let y_in = match ysel {
                0 => self.frc_reg,
                1 => (self.coef[cra] as i32) >> 3,
                2 => (self.y_reg >> 11) & 0x1FFF,
                _ => (self.y_reg >> 4) & 0xFFF,
            };
            if yrl != 0 {
                self.y_reg = self.inputs_reg & 0xFF_FFFF;
            }

            // Shifter on the previous step's 26-bit accumulator.
            let mut shifter = ((sext(self.sft_reg, 26) as u32) << (shft0 ^ shft1)) as i32;
            if shft1 == 0 {
                shifter = shifter.clamp(-0x80_0000, 0x7F_FFFF);
            }
            let shifter = (shifter as u32 & 0xFF_FFFF) as i32;

            if frcl != 0 {
                self.frc_reg = if shft0 & shft1 != 0 {
                    shifter & 0xFFF
                } else {
                    (shifter >> 11) & 0x1FFF
                };
            }

            // Multiply-accumulate → new 26-bit accumulator (B = TEMP or the old
            // accumulator, selected by BSEL).
            let temp_rd = sext(
                self.temp[((tra.wrapping_add(self.dec)) & 0x7F) as usize],
                24,
            );
            let x = if xsel != 0 { inputs } else { temp_rd };
            let product = (((sext(y_in, 13) as i64) * (x as i64)) >> 12) as i32;
            let mut sga = if bsel != 0 { self.sft_reg } else { temp_rd };
            if negb != 0 {
                sga = sga.wrapping_neg();
            }
            if zero != 0 {
                sga = 0;
            }
            self.sft_reg = product.wrapping_add(sga) & 0x3FF_FFFF;

            if ewt != 0 {
                self.efreg[ewa] = (shifter >> 8) as i16;
                let a = (self.efreg[ewa] as i32).abs();
                if a > self.efreg_hw[ewa] {
                    self.efreg_hw[ewa] = a;
                }
            }
            if twt != 0 {
                self.temp[((twa.wrapping_add(self.dec)) & 0x7F) as usize] = shifter;
            }
            if iwt != 0 {
                self.mems[iwa] = self.read_value;
            }

            // Resolve the delay-RAM access latched on a previous step (the
            // access has latency — Mednafen's ReadPending/WritePending).
            if self.read_pending != 0 {
                let w = ram.read16((self.rw_addr << 1) & SOUND_RAM_MASK);
                self.read_value = if self.read_pending == 2 {
                    ((w as i32) << 8) & 0xFF_FFFF
                } else {
                    dspfloat_to_int(w)
                };
                self.read_pending = 0;
            } else if self.write_pending {
                ram.write16((self.rw_addr << 1) & SOUND_RAM_MASK, self.write_value);
                self.write_pending = false;
            }

            // Compute this step's delay-RAM address and latch a pending access.
            let mut addr = self.madrs[masa] as u32;
            addr = addr.wrapping_add(nxaddr as u32);
            if adrgb != 0 {
                addr = addr.wrapping_add(sext(self.adrs_reg as i32, 12) as u32);
            }
            if table == 0 {
                addr = addr.wrapping_add(self.dec) & rbl_mask;
            } else {
                addr &= 0xFFFF;
            }
            self.rw_addr = addr.wrapping_add(self.rbp << 12) & 0x7_FFFF;
            if mrt != 0 {
                self.read_pending = 1 + nofl as u8;
            }
            if mwt != 0 {
                self.write_pending = true;
                self.write_value = if nofl != 0 {
                    (shifter >> 8) as u16
                } else {
                    int_to_dspfloat(shifter)
                };
            }

            if adrl != 0 {
                self.adrs_reg = if shft0 & shft1 != 0 {
                    (shifter >> 12) as u16
                } else {
                    ((inputs >> 16) & 0xFFF) as u16
                };
            }
        }

        // MDEC_CT counts down once per sample, wrapping at the ring length.
        if self.dec == 0 {
            self.dec = self.rbl;
        }
        self.dec = self.dec.wrapping_sub(1);
        self.mixs.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Ram;

    #[test]
    fn dspfloat_round_trip_is_close() {
        // dspfloat operates in 24-bit signed space (the shifter output domain).
        // Lossy (11-bit mantissa), but the round-trip stays within the mantissa's
        // relative error.
        for v in [
            0i32, 1, -1, 0x1000, -0x1000, 0x7FFF, -0x8000, 0x7F_FFFF, -0x80_0000,
        ] {
            let r = sext(dspfloat_to_int(int_to_dspfloat(v & 0xFF_FFFF)), 24);
            assert!(
                (r - v).abs() <= (v.abs() >> 10) + 1,
                "dspfloat round-trip {v} → {r}"
            );
        }
    }

    #[test]
    fn empty_program_is_silent_and_stopped() {
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.set_sample(0x4000, 0);
        dsp.step(&mut ram);
        assert!(dsp.efreg.iter().all(|&e| e == 0), "no program → no output");
    }

    #[test]
    fn passes_a_mix_input_through_to_an_effect_output() {
        // The shifter reads ACC from *before* the current step's MAC, so a
        // pass-through needs two steps: step 0 computes ACC = X·Y, step 1
        // shifts that ACC out to EFREG[0].
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        // Y = COEF[0] >> 3 (sign-extended to 13 bits); 0x7FF8 → Y = 0xFFF.
        dsp.coef[0] = 0x7FF8u16 as i16;
        // Step 0: X = MIXS[0] (IRA 0x20, XSEL=1), Y = COEF[0] (YSEL=1), B = 0
        //   (ZERO), SHIFT = 3.
        let s0_ip1 = (1 << 15) | (1 << 13) | (0x20 << 6);
        let s0_ip2 = (3 << 4) | (1 << 1);
        // Step 1: SHIFT = 3, ZERO, EWT → EFREG[0].
        let s1_ip2 = (1 << 12) | (3 << 4) | (1 << 1);
        dsp.mpro[0..4].copy_from_slice(&[0, s0_ip1, s0_ip2, 0]);
        dsp.mpro[4..8].copy_from_slice(&[0, 0, s1_ip2, 0]);
        dsp.start();
        dsp.set_sample(0x10000, 0); // MIXS[0] (<<4 → 24-bit inside the step)
        dsp.step(&mut ram);
        assert_ne!(dsp.efreg[0], 0, "mix input reached the effect output");
    }

    #[test]
    fn mixs_accumulates_modulo_20_bits() {
        // Regression (boot-jingle reverb noise): the DSP input mix wraps at 20
        // bits per accumulate (Mednafen `scsp.inc` `& 0xFFFFF`). Without the
        // wrap (and with the caller's old `voice << IMXL` 8×-too-hot send) the
        // reverb feedback saturated into self-oscillating static.
        let mut dsp = Dsp::new();
        dsp.set_sample(0x8_0000, 0);
        assert_eq!(dsp.mixs[0], 0x8_0000);
        dsp.set_sample(0x8_0000, 0); // 0x10_0000 wraps to 0
        assert_eq!(dsp.mixs[0], 0, "MIXS accumulates modulo 20 bits");
    }
}
