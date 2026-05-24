//! 5-stage pipeline cycle accounting and interlock scoreboard.
//!
//! The SH7604 has an in-order 5-stage pipeline (IF/ID/EX/MA/WB). Most
//! interlocks the manual lists fall out of correct base-cycle accounting
//! (e.g. branches, multi-cycle multiplies), but two need explicit
//! scoreboarding to reproduce hardware-visible cycle counts:
//!
//! * **Load-use stall** — a register loaded from memory is not available
//!   to the immediately following instruction; the consumer stalls 1 cycle.
//! * **MAC read stall** — a `STS MACH/MACL` issued before the multiplier
//!   pipeline has retired its previous multiply waits until the result is
//!   committed.
//!
//! This module owns the global cycle counter and the per-interlock state.
//! [`crate::interpreter::Cpu::step`] consults it pre-dispatch (to decide
//! whether to stall the incoming instruction) and updates it post-dispatch
//! (to record what the just-issued instruction left pending).

#[derive(Clone, Debug, Default)]
pub struct Pipeline {
    /// Total cycles executed since reset.
    pub cycles: u64,
    /// Absolute cycle at which MACH/MACL becomes readable by a `STS`. Used
    /// to stall reads issued before the multiplier finishes.
    pub mac_ready: u64,
}

impl Pipeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the global cycle counter by `n`.
    #[inline]
    pub fn advance(&mut self, n: u32) {
        self.cycles = self.cycles.saturating_add(n as u64);
    }

    /// If `mac_ready` is in the future, return the stall needed and bring
    /// `cycles` up to it. Otherwise return 0.
    #[inline]
    pub fn stall_for_mac(&mut self) -> u32 {
        if self.mac_ready > self.cycles {
            let s = (self.mac_ready - self.cycles) as u32;
            self.cycles = self.mac_ready;
            s
        } else {
            0
        }
    }

    /// Mark the multiplier as retiring `latency` cycles from *now*.
    #[inline]
    pub fn schedule_mac(&mut self, latency: u32) {
        self.mac_ready = self.cycles + latency as u64;
    }
}
