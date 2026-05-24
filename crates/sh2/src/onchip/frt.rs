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
//! prescaler (φ, φ/8, φ/32, φ/128), and output compare A/B match flags
//! (OCFA/OCFB in FTCSR). Overflow flag OVF set on FRC wrap. Edge capture
//! and external clock sources are out of scope.

#[derive(Clone, Debug, Default)]
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
}

impl Frt {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the timer by `cycles` CPU cycles. Performs all bookkeeping:
    /// prescaler, counter increment, OCFA/OCFB match, OVF on wrap.
    pub fn tick(&mut self, cycles: u32) {
        let period = match self.tcr & 0x03 {
            0 => 1,   // φ
            1 => 8,   // φ/8
            2 => 32,  // φ/32
            _ => 128, // φ/128
        };
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
            }
            if self.frc == self.ocrb {
                self.ftcsr |= 0x04; // OCFB
            }
        }
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
                // FTCSR status bits are write-1-to-clear (W1C). The CCLRA
                // bit (bit 0) is read/write.
                let w1c_mask = 0b1111_1110;
                self.ftcsr = (self.ftcsr & !(val & w1c_mask)) | (val & 0x01);
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
        if self.tocr & 0x10 != 0 { self.ocrb } else { self.ocra }
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
    fn counter_increments_at_full_speed_by_default() {
        let mut f = Frt::new();
        // TCR = 0 → φ → 1 cycle per FRC tick.
        f.tick(5);
        assert_eq!(f.frc, 5);
    }

    #[test]
    fn prescaler_divides_clock() {
        let mut f = Frt::new();
        f.write8(0x06, 0x01); // TCR: φ/8
        f.tick(7);
        assert_eq!(f.frc, 0, "below prescaler threshold");
        f.tick(1);
        assert_eq!(f.frc, 1, "8 accumulated cycles == 1 FRC tick");
        f.tick(16);
        assert_eq!(f.frc, 3, "16 more cycles → 2 more ticks");
    }

    #[test]
    fn overflow_sets_ovf_bit_and_continues_counting() {
        let mut f = Frt::new();
        f.frc = 0xFFFE;
        f.tick(3);
        assert_eq!(f.frc, 0x0001);
        assert_eq!(f.ftcsr & 0x02, 0x02);
    }

    #[test]
    fn output_compare_a_match_sets_ocfa() {
        let mut f = Frt::new();
        f.write16(0x04, 0x0010); // OCRA = 0x10
        f.tick(0x10);
        assert_eq!(f.frc, 0x0010);
        assert_eq!(f.ftcsr & 0x08, 0x08);
    }

    #[test]
    fn write_one_clears_status_flags() {
        let mut f = Frt::new();
        f.ftcsr = 0x0E; // OCFA | OCFB | OVF set
        f.write8(0x01, 0x0F); // W1C all status, also set CCLRA
        assert_eq!(f.ftcsr, 0x01);
    }
}
