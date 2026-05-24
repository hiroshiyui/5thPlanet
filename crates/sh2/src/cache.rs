//! SH7604 unified 4 KiB, 4-way set-associative cache (16-byte lines, LRU).
//!
//! Filled out in task #6. M1 needs it because most SH-2 code runs cached and
//! the hit/miss distinction is what makes the cycle counts realistic.

#[derive(Clone, Debug, Default)]
pub struct Cache {
    /// CCR (cache control register) value. Bit 0 = cache enable.
    pub ccr: u8,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }
}
