//! SH7604 hardware divider (DIVU).
//!
//! Memory map (offsets inside the on-chip block, base 0xFFFFFF00):
//!
//! ```text
//!   00  DVSR    divisor (signed 32-bit)
//!   04  DVDNT   dividend / quotient mirror — writing triggers 32/32 division
//!   08  DVCR    control: bit 0 OVF (status), bit 1 OVFIE (overflow IRQ enable)
//!   0C  VCRDIV  interrupt vector number for overflow (low 8 bits)
//!   10  DVDNTH  high half of the 64-bit dividend / remainder after divide
//!   14  DVDNTL  low half — writing triggers 64/32 division
//!   18  DVDNTUH mirror of DVDNTH
//!   1C  DVDNTUL mirror of DVDNTL
//! ```
//!
//! On overflow (divide-by-zero or a quotient that doesn't fit 32 bits signed)
//! the DVCR.OVF status bit is set; if DVCR.OVFIE is also set, the divider
//! requests the overflow interrupt — armed level-triggered by
//! [`OnChip::refresh_interrupts`](super::OnChip::refresh_interrupts) via
//! [`Source::DivuOvf`](super::intc::Source::DivuOvf) at the IPRA-programmed
//! level with the VCRDIV vector (M13 D1).
//!
//! Real hardware spends 39 cycles on a divide; this completes it synchronously.
//! A future timing pass can defer the result by N cycles via the pipeline
//! scoreboard if a game is observed to poll DVCR.OVF before reading the
//! quotient.

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Divu {
    pub dvsr: u32,
    pub dvdnth: u32,
    pub dvdntl: u32,
    pub dvcr: u32,
    pub vcrdiv: u32,
}

impl Divu {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read32(&self, offset: u32) -> u32 {
        match offset & 0x1F {
            0x00 => self.dvsr,
            0x04 => self.dvdntl,
            0x08 => self.dvcr,
            0x0C => self.vcrdiv,
            0x10 | 0x18 => self.dvdnth,
            0x14 | 0x1C => self.dvdntl,
            _ => 0,
        }
    }

    pub fn write32(&mut self, offset: u32, val: u32) {
        match offset & 0x1F {
            0x00 => self.dvsr = val,
            0x04 => {
                self.dvdntl = val;
                self.divide_32();
            }
            0x08 => self.dvcr = val & 0x3,
            0x0C => self.vcrdiv = val & 0xFFFF_007F,
            0x10 | 0x18 => self.dvdnth = val,
            0x14 | 0x1C => {
                self.dvdntl = val;
                self.divide_64();
            }
            _ => {}
        }
    }

    /// 32-bit signed dividend (DVDNT) ÷ 32-bit signed divisor (DVSR).
    fn divide_32(&mut self) {
        let dividend = self.dvdntl as i32;
        let divisor = self.dvsr as i32;

        if divisor == 0 || (dividend == i32::MIN && divisor == -1) {
            self.dvcr |= 1; // OVF
            return;
        }

        let q = dividend.wrapping_div(divisor);
        let r = dividend.wrapping_rem(divisor);
        self.dvdntl = q as u32;
        self.dvdnth = r as u32;
    }

    /// 64-bit signed dividend (DVDNTH:DVDNTL) ÷ 32-bit signed divisor.
    fn divide_64(&mut self) {
        let dividend = ((self.dvdnth as u64) << 32 | self.dvdntl as u64) as i64;
        let divisor = (self.dvsr as i32) as i64;

        if divisor == 0 {
            self.dvcr |= 1;
            return;
        }
        // Quotient must fit in 32 bits signed; otherwise OVF.
        let q = dividend.wrapping_div(divisor);
        if q > i32::MAX as i64 || q < i32::MIN as i64 {
            self.dvcr |= 1;
            return;
        }
        let r = dividend.wrapping_rem(divisor);
        self.dvdntl = q as i32 as u32;
        self.dvdnth = r as i32 as u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divide_32_positive() {
        let mut d = Divu::new();
        d.write32(0x00, 7);
        d.write32(0x04, 50);
        assert_eq!(d.dvdntl as i32, 7);
        assert_eq!(d.dvdnth as i32, 1);
        assert_eq!(d.dvcr & 1, 0);
    }

    #[test]
    fn divide_32_negative_dividend_truncates_toward_zero() {
        let mut d = Divu::new();
        d.write32(0x00, 7);
        d.write32(0x04, (-50i32) as u32);
        // Hardware divide truncates toward zero: -50 / 7 = -7 r -1.
        assert_eq!(d.dvdntl as i32, -7);
        assert_eq!(d.dvdnth as i32, -1);
    }

    #[test]
    fn divide_by_zero_sets_overflow_flag() {
        let mut d = Divu::new();
        d.write32(0x00, 0);
        d.write32(0x04, 100);
        assert_eq!(d.dvcr & 1, 1);
    }

    #[test]
    fn int_min_divided_by_minus_one_overflows() {
        let mut d = Divu::new();
        d.write32(0x00, (-1i32) as u32);
        d.write32(0x04, i32::MIN as u32);
        assert_eq!(d.dvcr & 1, 1);
    }

    #[test]
    fn divide_64_round_trip() {
        let mut d = Divu::new();
        d.write32(0x00, 1000);
        d.write32(0x10, 0);
        d.write32(0x14, 12_345_678);
        assert_eq!(d.dvdntl as i32, 12_345);
        assert_eq!(d.dvdnth as i32, 678);
    }

    #[test]
    fn divide_64_overflow_when_quotient_doesnt_fit() {
        let mut d = Divu::new();
        d.write32(0x00, 1);
        d.write32(0x10, 1);
        d.write32(0x14, 0); // 1:0 / 1 = 2^32 → overflow
        assert_eq!(d.dvcr & 1, 1);
    }
}
