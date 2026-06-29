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
    /// Per status flag (ICF/OCFA/OCFB/OVF): set when software has read that flag
    /// as 1, so a subsequent write-0 may clear it. The inverse of Mednafen's
    /// `FTCSRM` "set-by-hardware-but-not-yet-read" shadow (`sh7095.h:369`): our
    /// bit 1 = clearable ⇔ Mednafen's bit 0 = unprotected. A hardware *set*
    /// ([`Self::raise_status`]/[`Self::input_capture`]) drops the bit back to
    /// protected (Mednafen `FTCSRM |= bit`); a *read* marks all set flags
    /// clearable (Mednafen `FTCSRM = 0`). Do not serialize: a tiny RMW latch, and
    /// older save states must keep decoding with the same layout — loading 0
    /// (nothing read yet ⇒ nothing clearable) is the safe, conservative default.
    #[cfg_attr(feature = "serde", serde(skip))]
    ftcsr_read_ones: u8,
}

impl Frt {
    const FTCSR_STATUS: u8 = 0b1000_1110; // ICF|OCFA|OCFB|OVF

    pub fn new() -> Self {
        Self::default()
    }

    /// FRT prescaler **shift** (a power of two) from TCR CKS1-0: φ/8 → 3,
    /// φ/32 → 5, φ/128 → 7 (SH7604 HW manual; Mednafen `3 + ((TCR&3)<<1)`).
    /// Only valid for CKS1-0 ∈ {0,1,2}; CKS=3 selects the external FTCI clock
    /// and the caller ([`super::OnChip::frt_wdt_update`]) skips FRC ticking,
    /// so this is never called with CKS=3. FRC ticks once per `1<<shift` φ
    /// cycles — the lazy model derives the tick count from elapsed cycles
    /// (`(now>>shift) - (lastts>>shift)`) instead of a per-cycle accumulator.
    pub(super) const fn shift(tcr: u8) -> u32 {
        3 + ((tcr as u32 & 0x03) << 1)
    }

    /// Advance FRC by one prescaler tick (Mednafen `FRT_ClockFRC` + `FRT_CheckOCR`):
    /// bump the counter, set OVF on wrap, OCFA/OCFB on a compare-match (with
    /// CCLRA resetting FRC to 0 on the OCRA match for a periodic timer). Returns
    /// whether any FTCSR status flag was set this tick, so the caller can fold
    /// it into an interrupt recalc. Called `(now>>shift)-(lastts>>shift)` times
    /// per update by [`super::OnChip::frt_wdt_update`].
    pub(super) fn clock_frc(&mut self) -> bool {
        let mut flagged = false;
        let (new_frc, overflowed) = self.frc.overflowing_add(1);
        self.frc = new_frc;
        if overflowed {
            self.raise_status(0x02); // OVF
            flagged = true;
        }
        if self.frc == self.ocra {
            self.raise_status(0x08); // OCFA
            flagged = true;
            // CCLRA (FTCSR bit 0): clear the counter on an OCRA match, so
            // OCRA + the OCIA interrupt give a periodic timer.
            if self.ftcsr & 0x01 != 0 {
                self.frc = 0;
            }
        }
        if self.frc == self.ocrb {
            self.raise_status(0x04); // OCFB
            flagged = true;
        }
        flagged
    }

    /// Raise an FTCSR status flag from hardware and re-protect it from a write-0
    /// clear until the next FTCSR read observes it. Mirrors Mednafen setting the
    /// `FTCSRM` shadow alongside the flag (`sh7095.inc:1068-1069`): because our
    /// [`Self::ftcsr_read_ones`] is the *inverse* (set ⇒ clearable), a fresh
    /// hardware set must drop the bit back to protected (`&= !bit`). Without
    /// this, a flag set again *after* software read it but *before* it wrote the
    /// clearing 0 was wrongly clearable — diverging from the SH7604/Mednafen
    /// "write-0-after-read-1" protocol.
    fn raise_status(&mut self, bit: u8) {
        self.ftcsr |= bit;
        self.ftcsr_read_ones &= !bit;
    }

    /// Trigger a free-running-timer input capture (FTI edge): latch FRC into
    /// FICR and set the input-capture flag ICF (FTCSR bit 7). On the Saturn the
    /// FTI of each SH-2 is driven by the *other* CPU writing a word to a fixed
    /// region, so this is the inter-CPU "wake/dispatch" signal — see
    /// `SaturnBus`/`Saturn::drain_input_capture`. Returns whether the input-
    /// capture interrupt is enabled (TIER.ICIE, bit 7) so the caller can raise it.
    pub fn input_capture(&mut self) -> bool {
        // FICR latches the counter on every capture edge (Mednafen
        // `sh7095.inc:1065`), but ICF + its write-0 protection re-arm only on a
        // *fresh* set — Mednafen guards with `if(!(FTCSR & 0x80))`
        // (`sh7095.inc:1066`) so a second edge while ICF is still pending does
        // not re-protect an already-asserted flag.
        self.ficr = self.frc;
        if self.ftcsr & 0x80 == 0 {
            self.raise_status(0x80); // ICF (+ re-protect from write-0)
        }
        self.tier & 0x80 != 0
    }

    pub fn read8(&mut self, offset: u32) -> u8 {
        match offset & 0x0F {
            0x00 => self.tier,
            0x01 => {
                self.ftcsr_read_ones |= self.ftcsr & Self::FTCSR_STATUS;
                self.ftcsr
            }
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
                // hardware clears a flag when software writes 0 to it only after
                // having read that same flag as 1. A flag set by an FTI edge after
                // the read but before this write must survive the write.
                // CCLRA (bit 0) is an ordinary read/write control bit.
                //
                // The previous W1C was wrong and load-bearing: software (e.g. the
                // SH-2 inter-CPU FRT input-capture handshake) clears ICF by
                // writing 0 to it; under W1C that write was ignored, so ICF
                // stayed stuck set and an ICF-polling wait loop never actually
                // waited — it spun through, reading shared state at the wrong
                // time (the Doukyuusei intro slave crash).
                let clear = (!val) & self.ftcsr_read_ones & Self::FTCSR_STATUS;
                self.ftcsr = (self.ftcsr & Self::FTCSR_STATUS & !clear) | (val & 0x01);
                self.ftcsr_read_ones &= self.ftcsr & Self::FTCSR_STATUS;
            }
            0x02 => self.frc = ((val as u16) << 8) | (self.frc & 0x00FF),
            0x03 => self.frc = (self.frc & 0xFF00) | val as u16,
            0x04 => self.write_ocr_high(val),
            0x05 => self.write_ocr_low(val),
            0x06 => self.tcr = val,
            0x07 => self.tocr = val,
            _ => {} // FICR is read-only.
        }
    }

    pub fn read16(&mut self, offset: u32) -> u16 {
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

    /// Drive `n` prescaler ticks (the per-cycle→tick conversion is the lazy
    /// model's job in `OnChip::frt_wdt_update`; here we exercise the FRC engine
    /// directly, one tick at a time).
    fn clock(f: &mut Frt, n: u32) {
        for _ in 0..n {
            f.clock_frc();
        }
    }

    #[test]
    fn shift_decodes_the_prescaler() {
        // CKS1-0 → power-of-two prescaler shift (φ/8, φ/32, φ/128). CKS=3
        // (external) is filtered by the caller and never decoded here.
        assert_eq!(Frt::shift(0), 3, "φ/8");
        assert_eq!(Frt::shift(1), 5, "φ/32");
        assert_eq!(Frt::shift(2), 7, "φ/128");
    }

    #[test]
    fn counter_clocks_and_overflows() {
        let mut f = Frt::new();
        clock(&mut f, 5);
        assert_eq!(f.frc, 5);
        f.frc = 0xFFFE;
        clock(&mut f, 3); // 0xFFFF → 0x0000 (OVF) → 0x0001
        assert_eq!(f.frc, 0x0001);
        assert_eq!(f.ftcsr & 0x02, 0x02, "OVF set on wrap");
    }

    #[test]
    fn output_compare_a_match_sets_ocfa_and_cclra_reloads() {
        let mut f = Frt::new();
        f.write16(0x04, 0x0010); // OCRA = 0x10
        clock(&mut f, 0x10);
        assert_eq!(f.frc, 0x0010);
        assert_eq!(f.ftcsr & 0x08, 0x08, "OCFA on the match");
        // With CCLRA set, the OCRA match reloads FRC to 0 (periodic timer).
        let mut g = Frt::new();
        g.write16(0x04, 0x0004); // OCRA = 4
        g.ftcsr |= 0x01; // CCLRA
        clock(&mut g, 5); // 1,2,3,4(→match→reset 0),1
        assert_eq!(g.frc, 1, "FRC reloaded to 0 on the OCRA match, then +1");
        assert_eq!(g.ftcsr & 0x08, 0x08, "OCFA still flagged");
    }

    #[test]
    fn write_zero_clears_only_status_flags_previously_read_as_one() {
        // SH7604 FRT FTCSR: status flags (ICF/OCFA/OCFB/OVF) are cleared by
        // writing 0 after reading 1 — NOT W1C. Writing 1 keeps the flag.
        let mut f = Frt::new();
        f.ftcsr = 0x0E; // OCFA | OCFB | OVF set
        assert_eq!(f.read8(0x01), 0x0E);
        f.write8(0x01, 0x01); // status bits = 0 → clear all; CCLRA (bit 0) = 1
        assert_eq!(f.ftcsr, 0x01, "write-0 clears the status flags; CCLRA set");

        // Writing 1 to a status flag does NOT clear it (cannot set either).
        f.ftcsr = 0x0E;
        assert_eq!(f.read8(0x01), 0x0E);
        f.write8(0x01, 0x0E); // status bits = 1 → kept
        assert_eq!(f.ftcsr, 0x0E, "write-1 keeps the status flags (not W1C)");

        // A flag raised after a read that did not observe that flag must not be
        // lost by the write half of a read/modify/write status clear.
        let mut g = Frt::new();
        g.ftcsr = 0x0E; // no ICF yet
        assert_eq!(g.read8(0x01), 0x0E);
        g.input_capture();
        g.write8(0x01, 0x00);
        assert_eq!(
            g.ftcsr & 0x80,
            0x80,
            "new ICF pulse survives a zero write when ICF was not read as one"
        );
    }

    #[test]
    fn hardware_reset_after_read_re_protects_a_flag_from_write_zero() {
        // SH7604/Mednafen "write-0-after-read-1": a status flag set AGAIN by
        // hardware after software observed it (read 1) but before software wrote
        // the clearing 0 must SURVIVE that write — the fresh set re-protects it
        // (Mednafen `FTCSRM |= bit`). Regression for the sticky `ftcsr_read_ones`
        // that cleared the re-set flag.
        let mut f = Frt::new();
        f.ocra = 0xFFFF; // park both output compares so only OVF fires on the wrap
        f.ocrb = 0xFFFF;
        f.frc = 0xFFFF;
        clock(&mut f, 1); // wrap → OVF set
        assert_eq!(f.ftcsr & 0x02, 0x02, "OVF set on the first wrap");
        assert_eq!(
            f.read8(0x01) & 0x02,
            0x02,
            "software reads OVF=1 (now clearable)"
        );
        f.frc = 0xFFFF;
        clock(&mut f, 1); // wrap again → OVF re-set before the clearing write
        f.write8(0x01, 0x00); // software's read/modify/write status clear
        assert_eq!(
            f.ftcsr & 0x02,
            0x02,
            "the re-set OVF is re-protected and survives the write-0"
        );

        // The same for ICF via an input-capture edge after a read.
        let mut g = Frt::new();
        g.input_capture(); // ICF set
        assert_eq!(g.read8(0x01) & 0x80, 0x80, "software reads ICF=1");
        // A second edge while ICF is STILL set must not re-protect (Mednafen
        // guard): the in-flight read/clear of the original pulse still clears it.
        g.input_capture();
        g.write8(0x01, 0x00);
        assert_eq!(
            g.ftcsr & 0x80,
            0x00,
            "an unguarded re-pulse does not block the clear"
        );
    }

    #[test]
    fn input_capture_sets_icf_and_latches_frc() {
        let mut f = Frt::new();
        f.write16(0x04, 0xFFFF); // OCRA out of the way
        clock(&mut f, 0x40); // advance FRC to 0x40
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
