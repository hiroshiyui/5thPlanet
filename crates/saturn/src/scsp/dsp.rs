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

const SOUND_RAM_MASK: u32 = (super::SOUND_RAM_BYTES as u32) - 1;

/// Sign-extend the low `bits` of `v`.
#[inline]
fn sext(v: i32, bits: u32) -> i32 {
    let s = 32 - bits;
    (v << s) >> s
}

/// Compress a 24-bit value to the SCSP's 16-bit floating delay-RAM format.
fn pack(val: i32) -> u16 {
    let sign = (val >> 23) & 1;
    let mut temp = (val ^ (val << 1)) as u32 & 0xFF_FFFF;
    let mut exponent = 0;
    for _ in 0..12 {
        if temp & 0x80_0000 != 0 {
            break;
        }
        temp <<= 1;
        exponent += 1;
    }
    let mut v = if exponent < 12 {
        (val << exponent) & 0x3F_FFFF
    } else {
        val << 11
    };
    v >>= 11;
    v &= 0x7FF;
    v |= sign << 15;
    v |= exponent << 11;
    v as u16
}

/// Expand the 16-bit floating delay-RAM format back to 24-bit.
fn unpack(val: u16) -> i32 {
    let sign = ((val >> 15) & 1) as i32;
    let mut exponent = ((val >> 11) & 0xF) as i32;
    let mantissa = (val & 0x7FF) as i32;
    let mut uval = mantissa << 11;
    if exponent > 11 {
        exponent = 11;
        uval |= sign << 22;
    } else {
        uval |= (sign ^ 1) << 22;
    }
    uval |= sign << 23;
    uval <<= 8;
    uval >>= 8;
    uval >> exponent
}

#[derive(Clone, Debug)]
pub struct Dsp {
    pub coef: [i16; 64],
    pub madrs: [u16; 32],
    pub mpro: [u16; 128 * 4],
    pub temp: [i32; 128],
    pub mems: [i32; 32],
    pub mixs: [i32; 16],
    pub exts: [i16; 2],
    pub efreg: [i16; 16],
    pub rbp: u32,
    pub rbl: u32,
    dec: u32,
    stopped: bool,
    last_step: usize,
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
            rbp: 0,
            rbl: 8 * 1024,
            dec: 0,
            stopped: true,
            last_step: 0,
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

    /// Add a slot's effect-send into input-mix channel `sel`.
    pub fn set_sample(&mut self, sample: i32, sel: usize) {
        self.mixs[sel & 0xF] += sample;
    }

    /// Run one full pass (all program steps) for one output sample, using
    /// `ram` as the delay line. Clears `MIXS` afterward (consumed per sample).
    pub fn step(&mut self, ram: &mut Ram) {
        if self.stopped || self.last_step == 0 {
            self.mixs.fill(0);
            return;
        }
        self.efreg.fill(0);

        let mut acc: i32 = 0;
        let mut memval: i32 = 0;
        let mut frc_reg: i32 = 0;
        let mut y_reg: i32 = 0;
        let mut adrs_reg: u32 = 0;

        for step in 0..self.last_step {
            let ip = &self.mpro[step * 4..step * 4 + 4];
            let tra = ((ip[0] >> 8) & 0x7F) as u32;
            let twt = (ip[0] >> 7) & 1;
            let twa = (ip[0] & 0x7F) as u32;
            let xsel = ip[1] >> 15 & 1;
            let ysel = ip[1] >> 13 & 3;
            let ira = (ip[1] >> 6 & 0x3F) as usize;
            let iwt = ip[1] >> 5 & 1;
            let iwa = (ip[1] & 0x1F) as usize;
            let table = ip[2] >> 15 & 1;
            let mwt = ip[2] >> 14 & 1;
            let mrd = ip[2] >> 13 & 1;
            let ewt = ip[2] >> 12 & 1;
            let ewa = (ip[2] >> 8 & 0xF) as usize;
            let adrl = ip[2] >> 7 & 1;
            let frcl = ip[2] >> 6 & 1;
            let shift = ip[2] >> 4 & 3;
            let yrl = ip[2] >> 3 & 1;
            let negb = ip[2] >> 2 & 1;
            let zero = ip[2] >> 1 & 1;
            let bsel = ip[2] & 1;
            let nofl = ip[3] >> 15 & 1;
            let coef = (ip[3] >> 9 & 0x3F) as usize;
            let masa = (ip[3] >> 2 & 0x1F) as usize;
            let adreb = ip[3] >> 1 & 1;
            let nxadr = ip[3] & 1;

            // INPUTS (24-bit).
            let mut inputs = if ira <= 0x1F {
                self.mems[ira]
            } else if ira <= 0x2F {
                self.mixs[ira - 0x20] << 4
            } else if ira <= 0x31 {
                (self.exts[ira - 0x30] as i32) << 8
            } else {
                return;
            };
            inputs = sext(inputs, 24);
            if iwt != 0 {
                self.mems[iwa] = memval;
                if ira == iwa {
                    inputs = memval;
                }
            }

            // Operands.
            let b = if zero == 0 {
                let mut b = if bsel != 0 {
                    acc
                } else {
                    sext(self.temp[((tra.wrapping_add(self.dec)) & 0x7F) as usize], 24)
                };
                if negb != 0 {
                    b = -b;
                }
                b
            } else {
                0
            };
            let x = if xsel != 0 {
                inputs
            } else {
                sext(self.temp[((tra.wrapping_add(self.dec)) & 0x7F) as usize], 24)
            };
            let mut y = match ysel {
                0 => frc_reg,
                1 => (self.coef[coef] as i32) >> 3,
                2 => (y_reg >> 11) & 0x1FFF,
                _ => (y_reg >> 4) & 0xFFF,
            };
            if yrl != 0 {
                y_reg = inputs;
            }

            // Shifter.
            let shifted = match shift {
                0 => acc.clamp(-0x80_0000, 0x7F_FFFF),
                1 => acc.saturating_mul(2).clamp(-0x80_0000, 0x7F_FFFF),
                2 => sext(acc.wrapping_mul(2), 24),
                _ => sext(acc, 24),
            };

            // Multiply-accumulate (24-bit × 13-bit).
            y = sext(y, 13);
            let v = ((x as i64) * (y as i64)) >> 12;
            acc = (v + b as i64) as i32;

            if twt != 0 {
                self.temp[((twa.wrapping_add(self.dec)) & 0x7F) as usize] = shifted;
            }
            if frcl != 0 {
                frc_reg = if shift == 3 {
                    shifted & 0xFFF
                } else {
                    (shifted >> 11) & 0x1FFF
                };
            }

            // Delay-RAM access (sound RAM), on odd steps only.
            if mrd != 0 || mwt != 0 {
                let mut addr = self.madrs[masa] as u32;
                if table == 0 {
                    addr = addr.wrapping_add(self.dec);
                }
                if adreb != 0 {
                    addr = addr.wrapping_add(adrs_reg & 0xFFF);
                }
                if nxadr != 0 {
                    addr = addr.wrapping_add(1);
                }
                if table == 0 {
                    addr &= self.rbl.wrapping_sub(1);
                } else {
                    addr &= 0xFFFF;
                }
                addr = addr.wrapping_add(self.rbp << 12) << 1;
                let addr = addr & SOUND_RAM_MASK;
                if mrd != 0 && step & 1 != 0 {
                    let w = ram.read16(addr);
                    memval = if nofl != 0 {
                        (w as i32) << 8
                    } else {
                        unpack(w)
                    };
                }
                if mwt != 0 && step & 1 != 0 {
                    let w = if nofl != 0 {
                        (shifted >> 8) as u16
                    } else {
                        pack(shifted)
                    };
                    ram.write16(addr, w);
                }
            }

            if adrl != 0 {
                adrs_reg = if shift == 3 {
                    ((shifted >> 12) & 0xFFF) as u32
                } else {
                    (inputs >> 16) as u32
                };
            }
            if ewt != 0 {
                self.efreg[ewa] = self.efreg[ewa].wrapping_add((shifted >> 8) as i16);
            }
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
    fn pack_unpack_round_trip_is_close() {
        // The float format is lossy, but small magnitudes round-trip exactly.
        for v in [0, 1, -1, 0x1000, -0x1000, 0x7FFF, -0x8000] {
            let r = unpack(pack(v));
            assert!(
                (r - v).abs() <= (v.abs() >> 10) + 1,
                "pack/unpack {v} → {r}"
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
}
