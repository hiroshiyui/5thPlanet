//! 5-stage pipeline cycle accounting and interlock scoreboard.
//!
//! The SH7604 has an in-order 5-stage pipeline (IF/ID/EX/MA/WB). Most
//! interlocks the manual lists fall out of correct base-cycle accounting
//! (e.g. branches, multi-cycle multiplies), but two need explicit
//! scoreboarding to reproduce hardware-visible cycle counts:
//!
//! * **Load-use stall** — a register loaded from memory is not available
//!   to the immediately following instruction; the consumer stalls 1 cycle.
//!   This subsumes the **address-generation interlock** (M13 D3): a load
//!   feeding the next instruction's address base/index/post-modified base
//!   stalls the same 1 cycle, because [`crate::isa::Op::reads_reg`] reports
//!   address-base operands as register reads. Mednafen unifies the two the
//!   same way (every register read runs the `WB_EX_CHECK` write-back
//!   scoreboard, `sh7095_ops.inc`); a fully per-register absolute-readiness
//!   port (`WB_until = MA_until + 1`) would need the deferred per-access
//!   timestamping and produces no observable change at this granularity.
//! * **MAC read stall** — a `STS MACH/MACL` issued before the multiplier
//!   pipeline has retired its previous multiply waits until the result is
//!   committed.
//! * **Divide read stall** — a read of any DIVU register before the hardware
//!   divider retires (~39 cycles) waits for it ([`Pipeline::divide_ready`]).
//!
//! This module owns the global cycle counter and the per-interlock state.
//! [`crate::interpreter::Cpu::step`] consults it pre-dispatch (to decide
//! whether to stall the incoming instruction) and updates it post-dispatch
//! (to record what the just-issued instruction left pending).

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Pipeline {
    /// Total cycles executed since reset.
    pub cycles: u64,
    /// Absolute cycle at which MACH/MACL becomes readable by a `STS`. Used
    /// to stall reads issued before the multiplier finishes.
    pub mac_ready: u64,
    /// Absolute cycle at which the hardware divider (DIVU) result becomes
    /// readable. The SH7604 divider runs autonomously (~39 cycles for a 32/32
    /// divide, ~6 on overflow); a read of *any* DIVU register before it retires
    /// stalls the CPU until this cycle (Mednafen `divide_finish_timestamp`).
    pub divide_ready: u64,
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

    /// Mark the hardware divider as retiring `latency` cycles from *now* (the
    /// cycle the DIVU trigger register was written).
    #[inline]
    pub fn schedule_divide(&mut self, latency: u32) {
        self.divide_ready = self.cycles + latency as u64;
    }

    /// Cycles a DIVU-register read must stall for the divider to retire, or 0
    /// if it has already finished. Unlike [`Self::stall_for_mac`] this does
    /// **not** mutate `cycles` — the stall is returned through the memory
    /// access's wait-state count and accumulated once by the caller.
    #[inline]
    pub fn stall_for_divide(&self) -> u32 {
        self.divide_ready.saturating_sub(self.cycles) as u32
    }
}
