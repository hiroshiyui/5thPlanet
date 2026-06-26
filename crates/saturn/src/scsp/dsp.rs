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

/// The SCSP effect DSP — the 128-step VLIW program plus its
/// coefficient/temp/memory register files and the input/output mix bridges to
/// the slot mixer (see the module header for the field roles). [`Dsp::step`]
/// runs one full pass per output sample.
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
                let w = if self.rw_addr & 0x4_0000 != 0 {
                    0
                } else {
                    ram.read16(self.rw_addr << 1)
                };
                self.read_value = if self.read_pending == 2 {
                    ((w as i32) << 8) & 0xFF_FFFF
                } else {
                    dspfloat_to_int(w)
                };
                self.read_pending = 0;
            } else if self.write_pending {
                if self.rw_addr & 0x4_0000 == 0 {
                    ram.write16(self.rw_addr << 1, self.write_value);
                }
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

    #[test]
    fn mixs_masks_negative_sends_into_the_20_bit_field() {
        // A negative effect-send is stored as its low-20-bit pattern (bit 19 = the
        // input's sign), which the step recovers via `sext(MIXS << 4, 24)`. The
        // mask must never leave a negative `i32` in `mixs` (that would mis-feed
        // the `<<4`).
        let mut dsp = Dsp::new();
        dsp.set_sample(-0x1_0000, 5);
        assert_eq!(dsp.mixs[5], (-0x1_0000i32) & 0xF_FFFF);
        assert!(dsp.mixs[5] >= 0, "MIXS holds the unsigned 20-bit field");
        assert_ne!(dsp.mixs[5] & 0x8_0000, 0, "bit 19 (the sign) is set");
    }

    #[test]
    fn negative_mix_input_stays_negative_through_the_step() {
        // End-to-end sign check for the effect-send path: a negative MIXS input
        // must reach the effect output negative (the `sext(inputs_reg, 24)` at
        // consumption recovers the sign the 20-bit mask folded away). Same
        // pass-through microprogram as `passes_a_mix_input_through_...`.
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.coef[0] = 0x7FF8u16 as i16; // Y = +0xFFF
        let s0_ip1 = (1 << 15) | (1 << 13) | (0x20 << 6);
        let s0_ip2 = (3 << 4) | (1 << 1);
        let s1_ip2 = (1 << 12) | (3 << 4) | (1 << 1);
        dsp.mpro[0..4].copy_from_slice(&[0, s0_ip1, s0_ip2, 0]);
        dsp.mpro[4..8].copy_from_slice(&[0, 0, s1_ip2, 0]);
        dsp.start();
        dsp.set_sample(-0x1_0000, 0); // negative MIXS[0]
        dsp.step(&mut ram);
        assert!(
            dsp.efreg[0] < 0,
            "negative effect-send must stay negative (efreg={})",
            dsp.efreg[0]
        );
    }

    // --- microprogram instruction-word field helpers (mirror `step`'s decode) ---

    /// ip0: TRA(14:8) | TWT(7) | TWA(6:0)
    fn ip0(tra: u16, twt: u16, twa: u16) -> u16 {
        (tra << 8) | (twt << 7) | twa
    }
    /// ip1: XSEL(15) | YSEL(14:13) | IRA(11:6) | IWT(5) | IWA(4:0)
    fn ip1(xsel: u16, ysel: u16, ira: u16, iwt: u16, iwa: u16) -> u16 {
        (xsel << 15) | (ysel << 13) | (ira << 6) | (iwt << 5) | iwa
    }
    /// ip2: TABLE(15) MWT(14) MRT(13) EWT(12) EWA(11:8) ADRL(7) FRCL(6) SHFT1(5)
    /// SHFT0(4) YRL(3) NEGB(2) ZERO(1) BSEL(0)
    #[allow(clippy::too_many_arguments)]
    fn ip2(
        mwt: u16,
        mrt: u16,
        ewt: u16,
        ewa: u16,
        adrl: u16,
        frcl: u16,
        shft1: u16,
        shft0: u16,
        yrl: u16,
        zero: u16,
        bsel: u16,
    ) -> u16 {
        (mwt << 14)
            | (mrt << 13)
            | (ewt << 12)
            | (ewa << 8)
            | (adrl << 7)
            | (frcl << 6)
            | (shft1 << 5)
            | (shft0 << 4)
            | (yrl << 3)
            | (zero << 1)
            | bsel
    }
    /// ip3: NOFL(15) | CRA(14:9) | MASA(6:2) | ADRGB(1) | NXADDR(0)
    fn ip3(nofl: u16, cra: u16, masa: u16, nxaddr: u16) -> u16 {
        (nofl << 15) | (cra << 9) | (masa << 2) | nxaddr
    }

    fn load(dsp: &mut Dsp, prog: &[[u16; 4]]) {
        for (i, w) in prog.iter().enumerate() {
            dsp.mpro[i * 4..i * 4 + 4].copy_from_slice(w);
        }
        dsp.start();
    }

    #[test]
    fn temp_write_then_read_round_trips_through_the_ring() {
        // Step 0: ACC = MIXS[0]·COEF[0]; SHIFT=3, write the shifter to TEMP[5]
        //   (the shifter holds the *previous* ACC, which is 0 on step 0 — so first
        //   prime ACC, then on step 1 store it to TEMP).
        // Plan: s0 compute ACC. s1 SHIFT ACC→TEMP[5] (TWT). s2 read TEMP[5] as X
        //   (XSEL=0, TRA=5), Y=COEF (=+0xFFF) → new ACC. s3 SHIFT→EFREG[0].
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.coef[0] = 0x7FF8u16 as i16; // Y = +0xFFF
        // dec starts at 0, and the address offset `(addr + dec) & 0x7F` uses dec;
        // dec is the same for every step within one pass, so TWA==TRA addresses
        // the same TEMP cell.
        let prog = [
            // s0: X=MIXS[0], Y=COEF[0], ZERO B, SHIFT3 → ACC = MIXS·Y
            [
                0,
                ip1(1, 1, 0x20, 0, 0),
                ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0),
                0,
            ],
            // s1: SHIFT3 (shifts the s0 ACC), TWT→TEMP[5], ZERO keeps ACC sane
            [ip0(0, 1, 5), 0, ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0), 0],
            // s2: X=TEMP[5] (XSEL=0, TRA=5), Y=COEF[0], ZERO B, SHIFT3 → ACC
            [
                ip0(5, 0, 0),
                ip1(0, 1, 0, 0, 0),
                ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0),
                0,
            ],
            // s3: SHIFT3, EWT→EFREG[0]
            [0, 0, ip2(0, 0, 1, 0, 0, 0, 0, 1, 0, 1, 0), 0],
        ];
        load(&mut dsp, &prog);
        dsp.set_sample(0x4000, 0);
        dsp.step(&mut ram);
        // TEMP[5] must have captured the shifter; EFREG[0] is the value read back
        // out of TEMP and shifted — non-zero proves the TEMP write+read both ran.
        assert_ne!(dsp.temp[5], 0, "TEMP cell was written");
        assert_ne!(dsp.efreg[0], 0, "value read back out of TEMP reached EFREG");
    }

    #[test]
    fn unwritten_effect_outputs_remain_latched() {
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.efreg[3] = 0x1234;
        let prog = [[0, 0, ip2(0, 0, 1, 0, 0, 0, 0, 1, 0, 1, 0), 0]];

        load(&mut dsp, &prog);
        dsp.step(&mut ram);

        assert_eq!(
            dsp.efreg[3], 0x1234,
            "EFREG entries without EWT are latched, not cleared per sample"
        );
    }

    #[test]
    fn delay_ram_write_then_read_round_trips_via_madrs() {
        // MWT stores the shifter into delay RAM at MADRS[masa]; a later MRT reads
        // it back into read_value, and IWT latches read_value into MEMS. NOFL=1
        // uses the raw 16-bit (no dsp-float) path so the value is exact.
        // The write/read have one step of latency (the access resolves on the next
        // step), so interleave NOPs.
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.coef[0] = 0x7FF8u16 as i16;
        dsp.madrs[0] = 0x40; // delay-RAM word address (×2 for the byte address)
        dsp.rbl = 0x2000;
        dsp.rbp = 0;
        // Make `table=1` (absolute addressing, mask 0xFFFF) so dec doesn't shift
        // the address between the write step and the read step. table is ip2 bit
        // 15 — add it directly.
        let table = 1u16 << 15;
        let prog = [
            // s0: ACC = MIXS[0]·Y (a known non-zero shifter source next step)
            [
                0,
                ip1(1, 1, 0x20, 0, 0),
                ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0),
                0,
            ],
            // s1: SHIFT3 → shifter, MWT (NOFL raw) to MADRS[0], table absolute
            [
                0,
                0,
                ip2(1, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0) | table,
                ip3(1, 0, 0, 0),
            ],
            // s2: resolve the pending write (NOP body), keep ZERO
            [0, 0, ip2(0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0), 0],
            // s3: MRT (NOFL raw) from MADRS[0], table absolute
            [
                0,
                0,
                ip2(0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0) | table,
                ip3(1, 0, 0, 0),
            ],
            // s4: resolve the pending read → read_value (the IWT/MEMS latch and
            //   the read resolution both run this step, but IWT runs *before* the
            //   resolution block, so MEMS only sees read_value next step).
            [0, 0, ip2(0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0), 0],
            // s5: IWT → MEMS[2] now sees the resolved read_value.
            [
                0,
                ip1(0, 0, 0, 1, 2),
                ip2(0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0),
                0,
            ],
        ];
        load(&mut dsp, &prog);
        dsp.set_sample(0x6000, 0);
        dsp.step(&mut ram);
        // The raw 16-bit shifter byte ((shifter>>8) as u16) was written to delay
        // RAM and read back; MEMS[2] holds the sign-extended-<<8 read value.
        let stored = ram.read16((dsp.madrs[0] as u32) << 1);
        assert_ne!(stored, 0, "delay RAM received the write");
        assert_eq!(
            dsp.mems[2],
            ((stored as i32) << 8) & 0xFF_FFFF,
            "MRT(NOFL) read-back: raw 16-bit << 8, masked to 24 bits, into MEMS[2]"
        );
    }

    #[test]
    fn delay_ram_dummy_half_reads_zero_and_ignores_writes() {
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.stopped = false;
        dsp.last_step = 1;

        ram.write16(0, 0x5678);
        dsp.rw_addr = 0x4_0000;
        dsp.read_pending = 2; // raw 16-bit read path
        dsp.step(&mut ram);
        assert_eq!(dsp.read_value, 0, "dummy delay-RAM half reads as silence");

        dsp.rw_addr = 0x4_0000;
        dsp.write_pending = true;
        dsp.write_value = 0x1234;
        dsp.step(&mut ram);
        assert_eq!(
            ram.read16(0),
            0x5678,
            "dummy delay-RAM writes must not wrap into real sound RAM"
        );
    }

    #[test]
    fn dspfloat_delay_path_round_trips_through_delay_ram() {
        // The default (NOFL=0) delay path compresses the shifter to the 16-bit
        // dsp-float on write and expands it on read — within the float's relative
        // error. Same plan as the raw path but NOFL=0.
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.coef[0] = 0x7FF8u16 as i16;
        dsp.madrs[0] = 0x80;
        dsp.rbl = 0x2000;
        let table = 1u16 << 15;
        let prog = [
            [
                0,
                ip1(1, 1, 0x20, 0, 0),
                ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0),
                0,
            ],
            [
                0,
                0,
                ip2(1, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0) | table,
                ip3(0, 0, 0, 0),
            ],
            [0, 0, ip2(0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0), 0],
            [
                0,
                0,
                ip2(0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0) | table,
                ip3(0, 0, 0, 0),
            ],
            // resolve the read (IWT runs before the resolution block, so MEMS
            // only sees read_value on the following step).
            [0, 0, ip2(0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0), 0],
            [
                0,
                ip1(0, 0, 0, 1, 0),
                ip2(0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0),
                0,
            ],
        ];
        load(&mut dsp, &prog);
        dsp.set_sample(0x6000, 0);
        dsp.step(&mut ram);
        let raw = ram.read16((dsp.madrs[0] as u32) << 1);
        // The stored word decodes (via dspfloat_to_int) to MEMS[0].
        assert_eq!(
            dsp.mems[0],
            dspfloat_to_int(raw),
            "MRT(dsp-float) read-back matches the decoded delay word"
        );
        assert_ne!(
            dsp.mems[0], 0,
            "the dsp-float path carried a non-zero value"
        );
    }

    #[test]
    fn exts_input_reaches_the_effect_output() {
        // IRA 0x30/0x31 select the external inputs EXTS[0]/EXTS[1] (`<<8`); used
        // for the CD/external audio into the DSP. Route EXTS[0] through to EFREG.
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.coef[0] = 0x7FF8u16 as i16; // Y = +0xFFF
        dsp.exts[0] = 0x1234;
        let prog = [
            // s0: X = EXTS[0] (IRA 0x30, XSEL=1), Y=COEF[0], ZERO, SHIFT3
            [
                0,
                ip1(1, 1, 0x30, 0, 0),
                ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0),
                0,
            ],
            // s1: SHIFT3, EWT→EFREG[0]
            [0, 0, ip2(0, 0, 1, 0, 0, 0, 0, 1, 0, 1, 0), 0],
        ];
        load(&mut dsp, &prog);
        dsp.step(&mut ram);
        assert_ne!(dsp.efreg[0], 0, "EXTS[0] external input reached EFREG[0]");
    }

    #[test]
    fn mems_input_feeds_the_mac() {
        // IRA 0x00-0x1F select MEMS[ira] directly as the INPUTS latch. Pre-seed
        // MEMS[7] and route it through.
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.coef[0] = 0x7FF8u16 as i16;
        dsp.mems[7] = 0x20_0000; // a 24-bit-domain value
        let prog = [
            // s0: X = MEMS[7] (IRA 7, XSEL=1), Y=COEF[0], ZERO, SHIFT3
            [
                0,
                ip1(1, 1, 7, 0, 0),
                ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0),
                0,
            ],
            // s1: SHIFT3, EWT→EFREG[0]
            [0, 0, ip2(0, 0, 1, 0, 0, 0, 0, 1, 0, 1, 0), 0],
        ];
        load(&mut dsp, &prog);
        dsp.step(&mut ram);
        assert_ne!(dsp.efreg[0], 0, "MEMS input reached the effect output");
    }

    #[test]
    fn bsel_accumulates_onto_the_running_accumulator() {
        // BSEL=1 selects the accumulator (sft_reg) as the MAC's B addend, so two
        // identical product steps accumulate to ~2× one step (vs ZERO/BSEL=0
        // which replaces B). Compare EFREG with BSEL on vs off.
        fn run(bsel: u16) -> i16 {
            let mut dsp = Dsp::new();
            let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
            dsp.coef[0] = 0x4000u16 as i16; // Y = +0x800
            let macc = |b: u16| ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 0, b); // SHIFT3, BSEL=b
            let prog = [
                // s0: ACC = X·Y (ZERO B to start clean)
                [
                    0,
                    ip1(1, 1, 0x20, 0, 0),
                    ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0),
                    0,
                ],
                // s1: ACC = X·Y + (BSEL? ACC : TEMP[0]==0)
                [0, ip1(1, 1, 0x20, 0, 0), macc(bsel), 0],
                // s2: SHIFT3, EWT→EFREG[0]
                [0, 0, ip2(0, 0, 1, 0, 0, 0, 0, 1, 0, 1, 0), 0],
            ];
            load(&mut dsp, &prog);
            dsp.set_sample(0x2000, 0);
            dsp.step(&mut ram);
            dsp.efreg[0]
        }
        let with = run(1);
        let without = run(0);
        assert_ne!(with, without, "BSEL changes the accumulation");
        // With BSEL the second step adds the first product again → roughly double.
        assert!(
            with.unsigned_abs() > without.unsigned_abs(),
            "BSEL accumulates (|{with}| > |{without}|)"
        );
    }

    #[test]
    fn negb_negates_the_b_operand() {
        // NEGB negates the B addend; with B = accumulator and a positive product,
        // the result flips sign relative to the non-negated case.
        fn run(negb: u16) -> i16 {
            let mut dsp = Dsp::new();
            let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
            dsp.coef[0] = 0x4000u16 as i16;
            // ip2 with NEGB needs the bit-2 field; ip2() takes `zero` at bit1 and
            // `bsel` at bit0 but not negb — add it manually.
            let negb_bit = (negb & 1) << 2;
            let prog = [
                [
                    0,
                    ip1(1, 1, 0x20, 0, 0),
                    ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0),
                    0,
                ],
                // s1: ACC = X·Y + (NEGB? -ACC : +ACC), BSEL=1
                [
                    0,
                    ip1(1, 1, 0x20, 0, 0),
                    ip2(0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1) | negb_bit,
                    0,
                ],
                [0, 0, ip2(0, 0, 1, 0, 0, 0, 0, 1, 0, 1, 0), 0],
            ];
            load(&mut dsp, &prog);
            dsp.set_sample(0x2000, 0);
            dsp.step(&mut ram);
            dsp.efreg[0]
        }
        let plus = run(0);
        let minus = run(1);
        // +ACC accumulates (≈2× product); -ACC subtracts (≈0). They differ.
        assert_ne!(plus, minus, "NEGB changes the B-operand sign");
        assert!(
            minus.unsigned_abs() < plus.unsigned_abs(),
            "negated B cancels the running accumulator (|{minus}| < |{plus}|)"
        );
    }

    #[test]
    fn mdec_ct_counter_advances_the_ring_each_sample() {
        // MDEC_CT (`dec`) counts down once per sample pass, wrapping at RBL — it
        // offsets ring-relative TEMP/delay addresses so the delay line scrolls.
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.rbl = 16;
        // One non-empty step so the DSP runs.
        load(&mut dsp, &[[0, 0, ip2(0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0), 0]]);
        // dec starts 0; after the first step it wraps to rbl then decrements.
        dsp.step(&mut ram);
        assert_eq!(dsp.dec, dsp.rbl - 1, "dec wrapped to RBL then counted down");
        dsp.step(&mut ram);
        assert_eq!(dsp.dec, dsp.rbl - 2, "dec decrements each sample");
    }

    #[test]
    fn empty_microprogram_clears_pending_mix_input() {
        // The early-out path (stopped/last_step==0) still drains MIXS so a send
        // doesn't carry stale into a later (running) sample.
        let mut dsp = Dsp::new();
        let mut ram = Ram::new(super::super::SOUND_RAM_BYTES);
        dsp.set_sample(0x1234, 3);
        assert_eq!(dsp.mixs[3], 0x1234);
        dsp.step(&mut ram); // no program loaded
        assert_eq!(dsp.mixs[3], 0, "MIXS cleared even with no program");
    }
}
