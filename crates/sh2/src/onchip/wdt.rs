//! SH7604 watchdog timer (WDT).
//!
//! An 8-bit up-counter clocked by a prescaled internal clock. In **interval-
//! timer mode** (WTCSR.WT/IT = 0) a counter overflow sets WTCSR.OVF and raises
//! the WDT interrupt (ITI); in **watchdog mode** (WT/IT = 1) an overflow
//! latches RSTCSR.WOVF and would assert a chip reset.
//!
//! Register map (offsets from base 0xFFFFFE80):
//!
//! ```text
//!   00  WTCSR   control/status  (R)  bit7 OVF, bit6 WT/IT, bit5 TME, 2-0 CKS
//!   01  WTCNT   counter         (R)  8-bit up-counter
//!   03  RSTCSR  reset ctrl/sts  (R)  bit7 WOVF, bit6 RSTE, bit5 RSTS
//! ```
//!
//! Writes use a guarded 16-bit access at 0xFFFFFE80 / 0xFFFFFE82 whose high
//! byte is a magic key (the low byte is the data): 0x5A selects WTCNT /
//! RSTCSR-data, 0xA5 selects WTCSR / RSTCSR-WOVF-clear. Plain byte writes do
//! not satisfy the key and are ignored — matching the hardware's guard
//! against spurious writes. We model the timer and the interval-mode
//! interrupt; a watchdog-mode reset only latches WOVF (forcing a real system
//! reset needs host cooperation, which the SH-2 core alone can't do).

/// CKS[2:0] → prescaler **shift** (φ/2 … φ/8192 = 2^1 … 2^13; Mednafen
/// `wdt_cstab`). WTCNT ticks once per `1<<shift` φ cycles; the lazy model
/// derives the tick count from elapsed cycles (`(now>>shift)-(lastts>>shift)`).
const CKS_SHIFTS: [u32; 8] = [1, 6, 7, 8, 9, 10, 12, 13];

/// The SH7604 watchdog timer (WDT): an 8-bit up-counter usable as an interval
/// timer (ITI on overflow) or a true watchdog. See the module header.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Wdt {
    pub wtcsr: u8,
    pub wtcnt: u8,
    pub rstcsr: u8,
}

impl Wdt {
    pub fn new() -> Self {
        Self::default()
    }

    /// True while the counter is running (TME set). The caller
    /// ([`super::OnChip::frt_wdt_update`]) gates `clock_wtcnt` on this.
    pub(super) fn counting(&self) -> bool {
        self.wtcsr & 0x20 != 0
    }

    /// WTCNT prescaler shift from CKS2-0. Only meaningful while [`Wdt::counting`].
    pub(super) const fn shift(wtcsr: u8) -> u32 {
        CKS_SHIFTS[(wtcsr & 0x07) as usize]
    }

    /// Advance WTCNT by one prescaler tick: set OVF on wrap (and, in watchdog
    /// mode, latch RSTCSR.WOVF — a real reset isn't forced; see the module
    /// note). Returns whether OVF was set this tick. Called
    /// `(now>>shift)-(lastts>>shift)` times by [`super::OnChip::frt_wdt_update`].
    pub(super) fn clock_wtcnt(&mut self) -> bool {
        let (n, overflowed) = self.wtcnt.overflowing_add(1);
        self.wtcnt = n;
        if overflowed {
            self.wtcsr |= 0x80; // OVF
            if self.wtcsr & 0x40 != 0 {
                self.rstcsr |= 0x80; // WOVF (watchdog mode)
            }
            return true;
        }
        false
    }

    /// Whether the interval-timer interrupt (ITI) is asserted: interval mode
    /// (WT/IT = 0) with the overflow flag set.
    pub fn interrupt_active(&self) -> bool {
        self.wtcsr & 0x40 == 0 && self.wtcsr & 0x80 != 0
    }

    pub fn read8(&self, offset: u32) -> u8 {
        match offset & 0x1F {
            0x00 => self.wtcsr,
            0x01 => self.wtcnt,
            0x03 => self.rstcsr,
            // Unused bytes in the WDT window read as 1s on hardware.
            _ => 0xFF,
        }
    }

    /// Guarded 16-bit register write (high byte = key, low byte = data).
    pub fn write16(&mut self, offset: u32, val: u16) {
        let key = (val >> 8) as u8;
        let data = val as u8;
        match offset & 0x1F {
            0x00 => match key {
                0x5A => self.wtcnt = data, // WTCNT
                // WTCSR: OVF is write-0-to-clear only; the other bits load.
                0xA5 => self.wtcsr = (data & 0x7F) | (self.wtcsr & data & 0x80),
                _ => {}
            },
            0x02 => match key {
                0x5A => self.rstcsr = (self.rstcsr & 0x80) | (data & 0x60), // RSTE|RSTS
                0xA5 => self.rstcsr &= !0x80,                               // clear WOVF
                _ => {}
            },
            _ => {}
        }
    }

    /// Byte writes don't satisfy the key protocol — ignored, as on hardware.
    pub fn write8(&mut self, _offset: u32, _val: u8) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enable(w: &mut Wdt, interval: bool, cks: u8) {
        // WTCSR = TME(0x20) | (interval ? 0 : WT/IT 0x40) | CKS.
        let mode = if interval { 0x00 } else { 0x40 };
        w.write16(0x00, 0xA500 | (0x20 | mode | (cks & 7)) as u16);
    }

    fn clock(w: &mut Wdt, n: u32) {
        for _ in 0..n {
            w.clock_wtcnt();
        }
    }

    #[test]
    fn counting_gate_and_shift_decode() {
        let mut w = Wdt::new();
        assert!(!w.counting(), "TME clear → not counting (OnChip gates clock_wtcnt on this)");
        enable(&mut w, true, 0);
        assert!(w.counting(), "TME set → counting");
        assert_eq!(Wdt::shift(0), 1, "φ/2");
        assert_eq!(Wdt::shift(7), 13, "φ/8192");
    }

    #[test]
    fn interval_mode_overflow_sets_ovf_and_asserts_interrupt() {
        let mut w = Wdt::new();
        enable(&mut w, true, 0); // φ/2 (interval mode)
        w.write16(0x00, 0x5A00 | 0xFE); // WTCNT = 0xFE
        assert!(!w.interrupt_active());
        clock(&mut w, 2); // 0xFE → 0xFF → 0x00 (overflow)
        assert_eq!(w.wtcnt, 0x00);
        assert_eq!(w.wtcsr & 0x80, 0x80, "OVF set");
        assert!(w.interrupt_active(), "interval ITI asserted");
    }

    #[test]
    fn watchdog_mode_overflow_latches_wovf_not_interrupt() {
        let mut w = Wdt::new();
        enable(&mut w, false, 0); // watchdog mode, φ/2
        w.write16(0x00, 0x5A00 | 0xFF); // WTCNT = 0xFF
        clock(&mut w, 1); // one count → overflow
        assert_eq!(w.rstcsr & 0x80, 0x80, "WOVF latched");
        assert!(!w.interrupt_active(), "watchdog mode raises no ITI");
    }

    #[test]
    fn ovf_is_write_zero_to_clear() {
        let mut w = Wdt::new();
        w.wtcsr = 0x80; // OVF set
        // Write WTCSR with OVF=0 → clears it; TME stays as written.
        w.write16(0x00, 0xA500 | 0x20);
        assert_eq!(w.wtcsr & 0x80, 0, "OVF cleared by writing 0");
    }

    #[test]
    fn byte_writes_are_ignored() {
        let mut w = Wdt::new();
        w.write8(0x00, 0xFF);
        assert_eq!(w.wtcsr, 0, "byte write ignored (no key)");
    }
}
