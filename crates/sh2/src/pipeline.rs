//! 5-stage pipeline tracker (IF/ID/EX/MA/WB).
//!
//! Filled out in task #5. M1 wants per-instruction cycle accuracy, including
//! load-use stalls, multiply latency, and branch delay-slot costs.

#[derive(Clone, Debug, Default)]
pub struct Pipeline {
    /// Total cycles executed since reset.
    pub cycles: u64,
    /// Cycle at which each GPR's pending write retires. Used by the
    /// scoreboard to detect load-use interlocks.
    pub reg_ready: [u64; 16],
    /// Cycle at which MACH/MACL's pending multiply result retires.
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
}
