//! SH7604 free-running timer (FRT). 16-bit counter clocked by a prescaled
//! internal clock; many Saturn games use it for fine-grained timing.
//!
//! Register map (offsets from base 0xFFFFFE10):
//!
//! ```text
//!   00  TIER     Timer Interrupt Enable Register      (8-bit)
//!   01  FTCSR    Free-running Timer Control/Status    (8-bit)
//!   02  FRC      Free Running Counter                 (16-bit, BE word access)
//!   04  OCRA/B   Output Compare Register A or B       (16-bit; selected by FTCSR.OCRS)
//!   06  TCR      Timer Control Register               (8-bit, prescaler bits 0..1)
//!   07  TOCR     Timer Output Compare Control         (8-bit)
//!   08  FICR     Input Capture Register               (16-bit, read-only on hw)
//! ```
//!
//! M1 implements: register reads/writes, counter tick with TCR-selected
//! prescaler (φ/8, φ/32, φ/128 — CKS1-0 = 0/1/2), and output compare A/B match
//! flags (OCFA/OCFB in FTCSR). Overflow flag OVF set on FRC wrap. Edge capture
//! and the external clock source (CKS1-0 = 3, the FTCI pin) are out of scope:
//! CKS=3 freezes the φ-driven counter (the Saturn doesn't drive FTCI), matching
//! Mednafen/Yabause.

/// The SH7604 free-running timer (FRT): a 16-bit up-counter with output-compare
/// and input-capture. Its input-capture pin (FTI) is the Saturn's inter-CPU
/// wake; see the module header for the register map.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Frt {
    pub tier: u8,
    pub ftcsr: u8,
    pub frc: u16,
    pub ocra: u16,
    pub ocrb: u16,
    pub tcr: u8,
    pub tocr: u8,
    pub ficr: u16,
    /// Sub-cycle prescaler counter — accumulates instruction cycles and
    /// emits FRC ticks at the prescaler period.
    pre: u32,
    /// Cached φ prescaler period (8/32/128) decoded from `TCR` bits 1..0, so
    /// [`Frt::tick`] (called every instruction) need not re-decode `TCR` each
    /// time. Written whenever `TCR` is written; `0` is a "stale/uncomputed"
    /// sentinel — no real period is 0 — used after `Default`/savestate-load to
    /// force one lazy recompute. Hence `#[serde(skip)]` derived state that
    /// self-corrects, mirroring [`super::OnChip`]'s `intc_sig`. (The external-
    /// clock mode CKS=3 is handled in [`Frt::tick`], not via this field.)
    #[cfg_attr(feature = "serde", serde(skip))]
    period: u32,
}

impl Frt {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode the `TCR` prescaler bits (1..0) to the φ-derived FRC clock period
    /// in CPU cycles: φ/8, φ/32, φ/128 for CKS1-0 = 0/1/2 (SH7604 HW manual,
    /// FRT TCR). Never 0, so the caller can use 0 as the "not yet cached"
    /// sentinel for [`Frt::period`]. CKS1-0 = 3 selects the external FTCI clock
    /// and is handled in [`Frt::tick`] (the counter freezes), so it never
    /// reaches here — the `_` arm folds into φ/128 but is unused.
    const fn period_for(tcr: u8) -> u32 {
        match tcr & 0x03 {
            0 => 8,   // φ/8
            1 => 32,  // φ/32
            _ => 128, // φ/128 (CKS=3 external is filtered in tick, not decoded here)
        }
    }

    /// Advance the timer by `cycles` CPU cycles. Performs all bookkeeping:
    /// prescaler, counter increment, OCFA/OCFB match, OVF on wrap.
    pub fn tick(&mut self, cycles: u32) {
        // CKS1-0 = 3 selects the external clock input (FTCI). The Saturn does
        // not drive it, so φ must NOT advance the FRC (matches Mednafen
        // `FRT_WDT_Recalc_NET`'s `(TCR&3)!=3` guard and Yabause's "external
        // input clock not implemented"). The counter simply freezes.
        if self.tcr & 0x03 == 0x03 {
            return;
        }
        // `period` is cached on the TCR write; the `== 0` sentinel (after
        // Default/savestate-load) forces one lazy recompute (see the field
        // doc), turning the per-instruction path into a well-predicted zero
        // check + load instead of re-decoding TCR every call.
        if self.period == 0 {
            self.period = Self::period_for(self.tcr);
        }
        let period = self.period;
        self.pre = self.pre.saturating_add(cycles);
        while self.pre >= period {
            self.pre -= period;
            let (new_frc, overflowed) = self.frc.overflowing_add(1);
            self.frc = new_frc;
            if overflowed {
                self.ftcsr |= 0x02; // OVF
            }
            if self.frc == self.ocra {
                self.ftcsr |= 0x08; // OCFA
                // CCLRA (FTCSR bit 0): clear the counter on an OCRA match, so
                // OCRA + the OCIA interrupt give a periodic timer.
                if self.ftcsr & 0x01 != 0 {
                    self.frc = 0;
                }
            }
            if self.frc == self.ocrb {
                self.ftcsr |= 0x04; // OCFB
            }
        }
    }

    /// Trigger a free-running-timer input capture (FTI edge): latch FRC into
    /// FICR and set the input-capture flag ICF (FTCSR bit 7). On the Saturn the
    /// FTI of each SH-2 is driven by the *other* CPU writing a word to a fixed
    /// region, so this is the inter-CPU "wake/dispatch" signal — see
    /// `SaturnBus`/`Saturn::drain_input_capture`. Returns whether the input-
    /// capture interrupt is enabled (TIER.ICIE, bit 7) so the caller can raise it.
    pub fn input_capture(&mut self) -> bool {
        self.ficr = self.frc;
        self.ftcsr |= 0x80; // ICF
        self.tier & 0x80 != 0
    }

    pub fn read8(&self, offset: u32) -> u8 {
        match offset & 0x0F {
            0x00 => self.tier,
            0x01 => self.ftcsr,
            0x02 => (self.frc >> 8) as u8,
            0x03 => self.frc as u8,
            0x04 => (self.ocr_active() >> 8) as u8,
            0x05 => self.ocr_active() as u8,
            0x06 => self.tcr,
            0x07 => self.tocr,
            0x08 => (self.ficr >> 8) as u8,
            0x09 => self.ficr as u8,
            _ => 0,
        }
    }

    pub fn write8(&mut self, offset: u32, val: u8) {
        match offset & 0x0F {
            0x00 => self.tier = val,
            0x01 => {
                // SH7604 FRT FTCSR status flags (ICF bit7, OCFA bit3, OCFB bit2,
                // OVF bit1) are **write-0-to-clear after a read-1**, NOT W1C: the
                // hardware clears a flag when software writes 0 to it after
                // having read it as 1. Model the common, sufficient form
                // (write-0-clears: `new = old & written` for the status bits) —
                // a flag is kept when 1 is written, cleared when 0 is written.
                // CCLRA (bit 0) is an ordinary read/write control bit.
                //
                // The previous W1C was wrong and load-bearing: software (e.g. the
                // SH-2 inter-CPU FRT input-capture handshake) clears ICF by
                // writing 0 to it; under W1C that write was ignored, so ICF
                // stayed stuck set and an ICF-polling wait loop never actually
                // waited — it spun through, reading shared state at the wrong
                // time (the Doukyuusei intro slave crash).
                const STATUS: u8 = 0b1000_1110; // ICF|OCFA|OCFB|OVF
                self.ftcsr = (self.ftcsr & val & STATUS) | (val & 0x01);
            }
            0x02 => self.frc = ((val as u16) << 8) | (self.frc & 0x00FF),
            0x03 => self.frc = (self.frc & 0xFF00) | val as u16,
            0x04 => self.write_ocr_high(val),
            0x05 => self.write_ocr_low(val),
            0x06 => {
                self.tcr = val;
                self.period = Self::period_for(val); // refresh the cached φ prescaler (CKS=3 filtered in tick)
            }
            0x07 => self.tocr = val,
            _ => {} // FICR is read-only.
        }
    }

    pub fn read16(&self, offset: u32) -> u16 {
        let hi = self.read8(offset) as u16;
        let lo = self.read8(offset + 1) as u16;
        (hi << 8) | lo
    }

    pub fn write16(&mut self, offset: u32, val: u16) {
        self.write8(offset, (val >> 8) as u8);
        self.write8(offset + 1, val as u8);
    }

    fn ocr_active(&self) -> u16 {
        if self.tocr & 0x10 != 0 {
            self.ocrb
        } else {
            self.ocra
        }
    }
    fn write_ocr_high(&mut self, val: u8) {
        let target = if self.tocr & 0x10 != 0 {
            &mut self.ocrb
        } else {
            &mut self.ocra
        };
        *target = ((val as u16) << 8) | (*target & 0x00FF);
    }
    fn write_ocr_low(&mut self, val: u8) {
        let target = if self.tocr & 0x10 != 0 {
            &mut self.ocrb
        } else {
            &mut self.ocra
        };
        *target = (*target & 0xFF00) | val as u16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_ticks_at_phi_div_8_by_default() {
        // TCR = 0 → φ/8 (the SH7604 FRT's fastest prescale; there is no φ/1).
        let mut f = Frt::new();
        f.tick(7);
        assert_eq!(f.frc, 0, "below the φ/8 threshold");
        f.tick(1);
        assert_eq!(f.frc, 1, "8 cycles == 1 FRC tick");
    }

    #[test]
    fn prescaler_divides_clock() {
        let mut f = Frt::new();
        f.write8(0x06, 0x01); // TCR CKS=1 → φ/32
        f.tick(31);
        assert_eq!(f.frc, 0, "below the φ/32 threshold");
        f.tick(1);
        assert_eq!(f.frc, 1, "32 accumulated cycles == 1 FRC tick");
        f.tick(64);
        assert_eq!(f.frc, 3, "64 more cycles → 2 more ticks");
    }

    #[test]
    fn external_clock_mode_freezes_the_phi_driven_counter() {
        // TCR CKS=3 selects the external FTCI clock, undriven on the Saturn —
        // the FRC must not advance from φ (matches Mednafen/Yabause).
        let mut f = Frt::new();
        f.write8(0x06, 0x03);
        f.tick(10_000);
        assert_eq!(f.frc, 0, "external clock: FRC frozen, no φ ticks");
        // Switching back to a φ prescale resumes counting.
        f.write8(0x06, 0x00); // φ/8
        f.tick(8);
        assert_eq!(f.frc, 1, "φ-driven counting resumes after CKS=3→0");
    }

    #[test]
    fn overflow_sets_ovf_bit_and_continues_counting() {
        let mut f = Frt::new();
        f.frc = 0xFFFE;
        f.tick(24); // φ/8: 3 ticks → 0xFFFF → 0x0000 (OVF) → 0x0001
        assert_eq!(f.frc, 0x0001);
        assert_eq!(f.ftcsr & 0x02, 0x02);
    }

    #[test]
    fn output_compare_a_match_sets_ocfa() {
        let mut f = Frt::new();
        f.write16(0x04, 0x0010); // OCRA = 0x10
        f.tick(0x10 * 8); // φ/8: 0x10 ticks → FRC = 0x10
        assert_eq!(f.frc, 0x0010);
        assert_eq!(f.ftcsr & 0x08, 0x08);
    }

    #[test]
    fn write_zero_clears_status_flags_write_one_keeps() {
        // SH7604 FRT FTCSR: status flags (ICF/OCFA/OCFB/OVF) are cleared by
        // writing 0 after reading 1 — NOT W1C. Writing 1 keeps the flag.
        let mut f = Frt::new();
        f.ftcsr = 0x0E; // OCFA | OCFB | OVF set
        f.write8(0x01, 0x01); // status bits = 0 → clear all; CCLRA (bit 0) = 1
        assert_eq!(f.ftcsr, 0x01, "write-0 clears the status flags; CCLRA set");

        // Writing 1 to a status flag does NOT clear it (cannot set either).
        f.ftcsr = 0x0E;
        f.write8(0x01, 0x0E); // status bits = 1 → kept
        assert_eq!(f.ftcsr, 0x0E, "write-1 keeps the status flags (not W1C)");
    }

    #[test]
    fn cached_period_matches_tcr_and_self_corrects_after_load() {
        // The cached `period` must produce the same FRC behaviour as decoding
        // TCR every tick, and recover correctly when it's been zeroed (the
        // Default/savestate-load sentinel — `period` is #[serde(skip)]).
        let mut f = Frt::new();
        f.write8(0x06, 0x02); // TCR CKS=2 → φ/128 → period 128
        f.tick(127);
        assert_eq!(f.frc, 0, "below the φ/128 threshold");
        f.tick(1);
        assert_eq!(f.frc, 1, "128 accumulated cycles == 1 FRC tick");

        // Simulate a savestate load: TCR is restored but the derived `period`
        // comes back as the 0 sentinel. The next tick must recompute it from
        // the restored TCR (φ/128), not fall back to a faster prescale.
        f.period = 0;
        f.pre = 0;
        f.tick(127);
        assert_eq!(f.frc, 1, "sentinel recompute keeps φ/128 (no fast-tick)");
        f.tick(1);
        assert_eq!(f.frc, 2, "recomputed period still 128");
    }

    #[test]
    fn input_capture_sets_icf_and_latches_frc() {
        let mut f = Frt::new();
        f.write16(0x04, 0xFFFF); // OCRA out of the way
        f.tick(0x40 * 8); // φ/8: advance FRC to 0x40
        assert_eq!(f.frc, 0x40);
        let icie = f.input_capture();
        assert_eq!(f.ftcsr & 0x80, 0x80, "ICF set");
        assert_eq!(f.ficr, 0x40, "FRC latched into FICR");
        assert!(!icie, "ICIE clear by default");
        // ICIE (TIER bit 7) gates the interrupt return.
        f.write8(0x00, 0x80); // TIER.ICIE
        assert!(f.input_capture(), "ICIE set → interrupt requested");
    }
}
