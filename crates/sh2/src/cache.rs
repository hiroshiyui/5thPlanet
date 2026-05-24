//! SH7604 unified 4 KiB, 4-way set-associative cache (16-byte lines, LRU).
//!
//! Geometry, per *SH7604 Hardware Manual* §8:
//!
//! ```text
//!   address bits  31..10  9..4   3..0
//!                  tag    set   offset
//!   4 KiB total / 16 B line / 4 ways = 64 sets
//! ```
//!
//! Control via CCR (cache control register, 8-bit) — bit layout:
//!
//! ```text
//!   bit 0  CE   cache enable
//!   bit 1  ID   instruction-fetch disable
//!   bit 2  OD   data-access disable
//!   bit 3  TW   two-way mode (otherwise 4-way)
//!   bit 4  CP   cache purge (write-only one-shot; invalidates all entries)
//! ```
//!
//! M1 implements geometry, hit/miss probe, LRU replacement, CCR, and CP
//! purge. The cache is *not* yet wired into the interpreter's fetch and
//! data paths — that integration arrives with the Saturn bus in M2 so the
//! cached vs cache-through address regions can be honoured properly.

/// Bytes per cache line. Only referenced via the bit slicing in
/// [`decompose`] — kept named for documentation, hence `pub`.
pub const LINE_BYTES: usize = 16;
const WAYS: usize = 4;
const SETS: usize = 64; // 4096 / (16 * 4)

#[derive(Clone, Copy, Debug, Default)]
struct Line {
    tag: u32,
    valid: bool,
}

#[derive(Clone, Debug)]
pub struct Cache {
    ccr: u8,
    sets: [[Line; WAYS]; SETS],
    /// Per-set access order. Index 0 is the most-recently used way; index
    /// `WAYS-1` is the least-recently used (next eviction victim).
    lru: [[u8; WAYS]; SETS],
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Probe {
    Hit,
    Miss,
    /// Cache is disabled or the access kind is masked off by ID/OD —
    /// the request bypasses the cache and goes straight to the bus.
    Bypass,
}

impl Cache {
    pub fn new() -> Self {
        let mut lru = [[0u8; WAYS]; SETS];
        for row in &mut lru {
            for (i, slot) in row.iter_mut().enumerate() {
                *slot = i as u8;
            }
        }
        Self {
            ccr: 0,
            sets: [[Line::default(); WAYS]; SETS],
            lru,
        }
    }

    #[inline]
    pub fn ccr(&self) -> u8 {
        // CP is a write-only one-shot; always reads back 0.
        self.ccr & !0x10
    }

    /// Apply a write to CCR. Bits CE/ID/OD/TW are stored; CP (bit 4) is
    /// consumed immediately to purge all lines and never observed in the
    /// stored value.
    pub fn set_ccr(&mut self, val: u8) {
        if val & 0x10 != 0 {
            self.purge();
        }
        self.ccr = val & 0x0F;
    }

    #[inline]
    pub fn enabled(&self) -> bool {
        self.ccr & 0x01 != 0
    }
    #[inline]
    pub fn inst_disabled(&self) -> bool {
        self.ccr & 0x02 != 0
    }
    #[inline]
    pub fn data_disabled(&self) -> bool {
        self.ccr & 0x04 != 0
    }
    #[inline]
    pub fn two_way(&self) -> bool {
        self.ccr & 0x08 != 0
    }

    pub fn purge(&mut self) {
        for set in &mut self.sets {
            for line in set {
                line.valid = false;
            }
        }
    }

    /// Probe the cache for `addr` on behalf of an instruction fetch.
    /// On a miss, install the line and evict the LRU way.
    pub fn probe_fetch(&mut self, addr: u32) -> Probe {
        if !self.enabled() || self.inst_disabled() {
            return Probe::Bypass;
        }
        self.probe(addr)
    }

    /// Probe the cache for `addr` on behalf of a data access.
    pub fn probe_data(&mut self, addr: u32) -> Probe {
        if !self.enabled() || self.data_disabled() {
            return Probe::Bypass;
        }
        self.probe(addr)
    }

    fn probe(&mut self, addr: u32) -> Probe {
        let (tag, set_idx) = decompose(addr);
        let active_ways = if self.two_way() { 2 } else { WAYS };
        for way in 0..active_ways {
            if self.sets[set_idx][way].valid && self.sets[set_idx][way].tag == tag {
                self.touch(set_idx, way as u8);
                return Probe::Hit;
            }
        }
        // Miss: install into the LRU way (within active set).
        let victim = self.pick_victim(set_idx, active_ways);
        self.sets[set_idx][victim as usize] = Line { tag, valid: true };
        self.touch(set_idx, victim);
        Probe::Miss
    }

    fn pick_victim(&self, set_idx: usize, active_ways: usize) -> u8 {
        // Walk LRU order back-to-front; pick the oldest way still in range.
        for &w in self.lru[set_idx].iter().rev() {
            if (w as usize) < active_ways {
                return w;
            }
        }
        0
    }

    fn touch(&mut self, set_idx: usize, way: u8) {
        let row = &mut self.lru[set_idx];
        if let Some(pos) = row.iter().position(|&w| w == way) {
            row[..=pos].rotate_right(1);
        }
    }
}

#[inline]
fn decompose(addr: u32) -> (u32, usize) {
    let set_idx = ((addr >> 4) & 0x3F) as usize;
    let tag = addr >> 10;
    (tag, set_idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_starts_disabled_and_bypasses() {
        let mut c = Cache::new();
        assert_eq!(c.probe_fetch(0x1234), Probe::Bypass);
    }

    #[test]
    fn enabled_cache_first_access_is_miss_second_is_hit() {
        let mut c = Cache::new();
        c.set_ccr(0x01);
        assert_eq!(c.probe_data(0x6000_0040), Probe::Miss);
        assert_eq!(c.probe_data(0x6000_0040), Probe::Hit);
        // Same line, different offset within the 16-byte line — still a hit.
        assert_eq!(c.probe_data(0x6000_004C), Probe::Hit);
    }

    #[test]
    fn lru_evicts_least_recently_used_way() {
        let mut c = Cache::new();
        c.set_ccr(0x01);
        // All four addresses map to the same set (set 0, varying tags).
        let base: u32 = 0x6000_0000;
        for i in 0..4u32 {
            // Each address bumps the tag by 1 (bit 10+).
            assert_eq!(c.probe_data(base | (i << 10)), Probe::Miss);
        }
        // Set is now full. Touching way 0 (tag 0) marks it MRU.
        assert_eq!(c.probe_data(base), Probe::Hit);
        // A new tag should evict the least-recently-used way (tag 1).
        let new_addr = base | (4 << 10);
        assert_eq!(c.probe_data(new_addr), Probe::Miss);
        assert_eq!(c.probe_data(base | (1 << 10)), Probe::Miss, "tag 1 evicted");
        assert_eq!(c.probe_data(base), Probe::Hit, "tag 0 retained");
    }

    #[test]
    fn two_way_mode_only_uses_two_ways_per_set() {
        let mut c = Cache::new();
        c.set_ccr(0x01 | 0x08); // CE + TW
        let base: u32 = 0x6000_0000;
        // First two distinct tags miss and install into ways 0 and 1.
        assert_eq!(c.probe_data(base), Probe::Miss);
        assert_eq!(c.probe_data(base | (1 << 10)), Probe::Miss);
        // Both should hit now.
        assert_eq!(c.probe_data(base), Probe::Hit);
        assert_eq!(c.probe_data(base | (1 << 10)), Probe::Hit);
        // A third distinct tag must evict whichever of {0,1} is older —
        // in our case tag 0 (we touched tag 1 most recently).
        assert_eq!(c.probe_data(base | (2 << 10)), Probe::Miss);
        assert_eq!(c.probe_data(base), Probe::Miss, "tag 0 evicted from active set");
    }

    #[test]
    fn cp_bit_purges_and_is_write_only() {
        let mut c = Cache::new();
        c.set_ccr(0x01);
        assert_eq!(c.probe_data(0x6000_0000), Probe::Miss);
        assert_eq!(c.probe_data(0x6000_0000), Probe::Hit);
        c.set_ccr(0x01 | 0x10); // CE + CP
        assert_eq!(c.ccr() & 0x10, 0, "CP reads back as 0");
        assert_eq!(c.probe_data(0x6000_0000), Probe::Miss, "purge invalidated lines");
    }

    #[test]
    fn instruction_and_data_masks_route_independently() {
        let mut c = Cache::new();
        c.set_ccr(0x01 | 0x02); // CE + ID
        assert_eq!(c.probe_fetch(0x6000_0000), Probe::Bypass);
        assert_eq!(c.probe_data(0x6000_0000), Probe::Miss);

        let mut c = Cache::new();
        c.set_ccr(0x01 | 0x04); // CE + OD
        assert_eq!(c.probe_data(0x6000_0000), Probe::Bypass);
        assert_eq!(c.probe_fetch(0x6000_0000), Probe::Miss);
    }
}
