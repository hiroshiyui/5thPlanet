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
//! Real hardware spends ~39 cycles on a 32/32 divide (≈6 on overflow); the
//! division itself is computed eagerly here, but the **timing** is modelled
//! faithfully (M13 D1): a triggering write records the latency in
//! [`Divu::pending_latency`], the interpreter schedules
//! [`Pipeline::schedule_divide`](crate::pipeline::Pipeline::schedule_divide),
//! and a read of any DIVU register before the divider retires stalls the CPU
//! (Mednafen `divide_finish_timestamp`; `sh7095.inc` `DIVU_S32_S32`).

/// Latency of a successful divide (cycles), from the trigger write to a
/// readable result — the canonical SH7604 figure (Mednafen `+1+39`).
const DIVIDE_LATENCY: u32 = 39;
/// Latency to the result/flag when the divide overflows (divide-by-zero or a
/// quotient that doesn't fit) — Mednafen settles these in ~6 cycles.
const OVERFLOW_LATENCY: u32 = 6;

/// The SH7604 hardware divider (DIVU) — see the module header for the register
/// map. A `DVDNT`/`DVDNTL` write launches the 32/32 (or 64/32) division.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Divu {
    pub dvsr: u32,
    pub dvdnth: u32,
    pub dvdntl: u32,
    pub dvcr: u32,
    pub vcrdiv: u32,
    /// Set by a triggering write to the latency (in CPU cycles) the divider
    /// will take; the interpreter consumes it via [`Divu::take_pending_latency`]
    /// right after the on-chip write to arm the pipeline divide-ready stall.
    pub pending_latency: Option<u32>,
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

    /// Take (and clear) the latency a just-triggered divide will run for, so
    /// the interpreter can arm the pipeline divide-ready stall. `None` when no
    /// divide was triggered by the preceding write.
    #[inline]
    pub fn take_pending_latency(&mut self) -> Option<u32> {
        self.pending_latency.take()
    }

    /// 32-bit signed dividend (DVDNT) ÷ 32-bit signed divisor (DVSR).
    /// Mednafen `DIVU_S32_S32` (`sh7095.inc`).
    fn divide_32(&mut self) {
        let dividend = self.dvdntl as i32;
        let divisor = self.dvsr as i32;

        // Divide-by-zero is the ONLY 32/32 overflow. (`i32::MIN / -1` is NOT an
        // overflow on the SH7604 — it's the defined result DVDNT=0x80000000,
        // DVDNTH=0, which `wrapping_div`/`wrapping_rem` produce in the normal
        // path below.) On overflow the high half takes the arithmetic-shifted
        // dividend and the low half a defined saturated quotient (or, with
        // OVFIE set, the hardware's partial result) — not the stale dividend.
        if divisor == 0 {
            self.dvcr |= 1; // OVF
            self.dvdnth = (dividend >> 29) as u32;
            self.dvdntl = if self.dvcr & 2 == 0 {
                0x7FFF_FFFFu32.wrapping_add((dividend < 0) as u32)
            } else {
                ((dividend as u32) << 3) | (((!dividend) >> 31) as u32 & 7)
            };
            self.pending_latency = Some(OVERFLOW_LATENCY);
            return;
        }

        let q = dividend.wrapping_div(divisor);
        let r = dividend.wrapping_rem(divisor);
        self.dvdntl = q as u32;
        self.dvdnth = r as u32;
        self.pending_latency = Some(DIVIDE_LATENCY);
    }

    /// 64-bit signed dividend (DVDNTH:DVDNTL) ÷ 32-bit signed divisor.
    /// Mednafen `DIVU_S64_S32` (`sh7095.inc`).
    fn divide_64(&mut self) {
        let divisor = self.dvsr as i32;
        let dividend = (((self.dvdnth as u64) << 32) | self.dvdntl as u64) as i64;

        // Overflow if: divisor 0, the i64::MIN / -1 special, or the quotient
        // falls outside the asymmetric [-(2^31-1), 2^31-1] — except the exact
        // 2^31 / (negative divisor) / zero-remainder case, which is defined.
        // Divisor 0, or the i64::MIN / -1 special, are unconditional overflows.
        let min_over_neg1 = dividend as u64 == 1u64 << 63 && divisor as u32 == 0xFFFF_FFFF;
        let overflow = if divisor == 0 || min_over_neg1 {
            true
        } else {
            let q = dividend.wrapping_div(divisor as i64);
            // The exact 2^31 / (negative divisor) / zero-remainder case is defined.
            if q == 2_147_483_648 && divisor < 0 && dividend.wrapping_rem(divisor as i64) == 0 {
                false
            } else {
                !(-2_147_483_647..=2_147_483_647).contains(&q)
            }
        };

        if overflow {
            self.dvcr |= 1;
            let tmp = divu64_partial(dividend as u64, self.dvsr);
            self.dvdnth = (tmp >> 32) as u32;
            self.dvdntl = if self.dvcr & 2 != 0 {
                tmp as u32
            } else {
                let neg = (((dividend >> 32) as i32) ^ divisor) < 0;
                0x7FFF_FFFFu32.wrapping_add(neg as u32)
            };
            self.pending_latency = Some(OVERFLOW_LATENCY);
        } else {
            let q = dividend.wrapping_div(divisor as i64);
            let r = dividend.wrapping_rem(divisor as i64);
            self.dvdntl = q as u32;
            self.dvdnth = r as u32;
            self.pending_latency = Some(DIVIDE_LATENCY);
        }
    }
}

/// Mednafen `DIVU64_Partial` — the 3-step non-restoring division loop that
/// yields the SH7604's defined high half (remainder) on a 64/32 overflow.
fn divu64_partial(mut dividend: u64, divisor: u32) -> u64 {
    let mut q = dividend >> 63 != 0;
    let m = divisor >> 31 != 0;
    for _ in 0..3 {
        if q == m {
            dividend = dividend.wrapping_sub((divisor as u64) << 32);
        } else {
            dividend = dividend.wrapping_add((divisor as u64) << 32);
        }
        q = dividend >> 63 != 0;
        dividend <<= 1;
        dividend |= (q ^ true ^ m) as u64;
    }
    dividend
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
    fn divide_by_zero_overflows_with_a_saturated_quotient() {
        // OVFIE clear: a positive dividend saturates to +max, a negative one to
        // +min; the high half is the arithmetic-shifted dividend (Mednafen).
        let mut d = Divu::new();
        d.write32(0x00, 0);
        d.write32(0x04, 100);
        assert_eq!(d.dvcr & 1, 1, "OVF set");
        assert_eq!(d.dvdntl, 0x7FFF_FFFF, "saturated to +max, not the dividend");
        assert_eq!(d.dvdnth, 100 >> 29);
        let mut d = Divu::new();
        d.write32(0x00, 0);
        d.write32(0x04, (-100i32) as u32);
        assert_eq!(d.dvdntl, 0x8000_0000, "negative dividend saturates to +min");
    }

    #[test]
    fn int_min_divided_by_minus_one_is_defined_not_overflow() {
        // The SH7604 defines DVDNT = 0x80000000, DVDNTH = 0 here (no OVF).
        let mut d = Divu::new();
        d.write32(0x00, (-1i32) as u32);
        d.write32(0x04, i32::MIN as u32);
        assert_eq!(d.dvcr & 1, 0, "not an overflow");
        assert_eq!(d.dvdntl, 0x8000_0000);
        assert_eq!(d.dvdnth, 0);
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
        // OVFIE clear: the low half saturates (not the stale dividend). The
        // dividend high word (1) ^ divisor (1) = 0 ≥ 0 → +max.
        assert_eq!(d.dvdntl, 0x7FFF_FFFF);
    }

    #[test]
    fn divide_64_quotient_2pow31_with_negative_divisor_is_defined() {
        // dividend = 2^31 * (-2) = -2^32, divisor = -2 → quotient = 2^31,
        // remainder 0: the special non-overflow case (DVDNTL = 0x80000000).
        let mut d = Divu::new();
        d.write32(0x00, (-2i32) as u32); // DVSR
        d.write32(0x10, 0xFFFF_FFFF); // DVDNTH (high)
        d.write32(0x14, 0x0000_0000); // DVDNTL (low) → dividend = -2^32
        assert_eq!(d.dvcr & 1, 0, "defined, not an overflow");
        assert_eq!(d.dvdntl, 0x8000_0000, "quotient 2^31");
        assert_eq!(d.dvdnth, 0, "zero remainder");
    }

    #[test]
    fn triggering_a_divide_arms_the_latency_a_plain_write_does_not() {
        let mut d = Divu::new();
        // Writing DVSR alone is just a register store — no divide, no latency.
        d.write32(0x00, 7);
        assert_eq!(d.take_pending_latency(), None);
        // Writing DVDNT triggers a 32/32 divide → the full latency.
        d.write32(0x04, 50);
        assert_eq!(d.take_pending_latency(), Some(DIVIDE_LATENCY));
        assert_eq!(d.take_pending_latency(), None, "consumed exactly once");
        // A divide-by-zero overflow settles faster.
        d.write32(0x00, 0);
        d.write32(0x04, 50);
        assert_eq!(d.take_pending_latency(), Some(OVERFLOW_LATENCY));
    }
}
