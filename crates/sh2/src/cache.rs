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
//! Lines store their 16 bytes of data alongside the tag, so a hit can
//! satisfy a read without touching the bus. The write policy is
//! write-through (matches SH7604): writes always reach the bus and, if
//! the line is resident, the cached copy is updated in place via
//! [`Cache::write_through_u8`] et al.
//!
//! **Miss handling is a two-step protocol with the caller**: [`lookup_*`]
//! returns [`Lookup::Miss`] without installing anything, and the caller
//! is then responsible for fetching the 16-byte line from the bus and
//! handing it back via [`install`]. Splitting it this way keeps the
//! cache pure (no Bus dependency) and makes the line-fill timing visible
//! to the interpreter's cycle accounting.

#[cfg(feature = "serde")]
use serde_big_array::BigArray;

/// Bytes per cache line.
pub const LINE_BYTES: usize = 16;
const WAYS: usize = 4;
const SETS: usize = 64; // 4096 / (16 * 4)

#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
struct Line {
    tag: u32,
    valid: bool,
    data: [u8; LINE_BYTES],
}

impl Default for Line {
    fn default() -> Self {
        Self {
            tag: 0,
            valid: false,
            data: [0; LINE_BYTES],
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cache {
    ccr: u8,
    // SETS (64) exceeds serde's built-in 32-element array impls, so the
    // outer dimension goes through serde-big-array; the inner `[_; WAYS]`
    // (4) serializes natively.
    #[cfg_attr(feature = "serde", serde(with = "BigArray"))]
    sets: [[Line; WAYS]; SETS],
    /// Per-set access order. Index 0 is the most-recently used way; index
    /// `WAYS-1` is the least-recently used (next eviction victim).
    #[cfg_attr(feature = "serde", serde(with = "BigArray"))]
    lru: [[u8; WAYS]; SETS],
    /// Debug only: lifetime count of [`Cache::assoc_purge`] invocations. Lets a
    /// coherency investigation see whether software actually relies on the
    /// associative purge. Observer-only; never serialized.
    #[cfg_attr(feature = "serde", serde(skip))]
    dbg_assoc_purges: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of a cache lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lookup {
    /// Line is resident; here are its 16 bytes (caller extracts the
    /// access-sized slice based on the address offset).
    Hit([u8; LINE_BYTES]),
    /// Line is not resident. Caller must fetch from the bus and call
    /// [`Cache::install`] before the data is observable here.
    Miss,
    /// Cache is disabled or this access kind is masked off (ID/OD).
    /// Caller should go straight to the bus and skip [`install`].
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
            dbg_assoc_purges: 0,
        }
    }

    /// Debug only: how many associative purges have been performed.
    pub fn dbg_assoc_purges(&self) -> u64 {
        self.dbg_assoc_purges
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

    /// Associative purge — invalidate the single line whose tag matches `addr`,
    /// across all ways of `addr`'s set. This is the SH7604 cache-purge space
    /// (`0x4000_0000..0x5FFF_FFFF` / `0xA000_0000..0xBFFF_FFFF`): software writes
    /// (or reads) there to drop one specific stale line *without knowing which
    /// way holds it* — e.g. to force a coherent re-read of memory another bus
    /// master (the other SH-2, the SCU DMA) has written, since the SH-2 caches
    /// are not hardware-coherent. Mirrors Mednafen `Cache_AssocPurge`
    /// (`sh7095.inc`): the set index is `addr[9:4]` and the tag is `addr[28:10]`
    /// — the **region bits `[31:29]` are stripped** (Mednafen's `A & (0x7FFFF
    /// << 10)`) so a purge issued through the `0x4xxx_xxxx` alias matches lines
    /// that were installed from the cacheable `0x0xxx_xxxx` region. All four
    /// ways are scanned regardless of two-way mode (matching the hardware).
    /// (*SH7604 Hardware Manual* §8, "Cache Purge".)
    pub fn assoc_purge(&mut self, addr: u32) {
        self.dbg_assoc_purges += 1;
        let (tag, set_idx) = decompose(addr & 0x1FFF_FFFF);
        for line in &mut self.sets[set_idx] {
            if line.valid && line.tag == tag {
                line.valid = false;
            }
        }
    }

    /// Probe the cache for `addr` on behalf of an instruction fetch.
    pub fn lookup_fetch(&mut self, addr: u32) -> Lookup {
        if !self.enabled() || self.inst_disabled() {
            return Lookup::Bypass;
        }
        self.lookup(addr)
    }

    /// Probe the cache for `addr` on behalf of a data access.
    pub fn lookup_data(&mut self, addr: u32) -> Lookup {
        if !self.enabled() || self.data_disabled() {
            return Lookup::Bypass;
        }
        self.lookup(addr)
    }

    fn lookup(&mut self, addr: u32) -> Lookup {
        let (tag, set_idx) = decompose(addr);
        let active_ways = if self.two_way() { 2 } else { WAYS };
        for way in 0..active_ways {
            if self.sets[set_idx][way].valid && self.sets[set_idx][way].tag == tag {
                let data = self.sets[set_idx][way].data;
                self.touch(set_idx, way as u8);
                return Lookup::Hit(data);
            }
        }
        Lookup::Miss
    }

    /// Install a freshly-fetched line. Called by the bus-miss path after
    /// the caller has obtained the 16-byte line from external memory.
    /// Picks the LRU way of the line's set, evicts it, and marks the new
    /// way most-recently used.
    pub fn install(&mut self, addr: u32, data: [u8; LINE_BYTES]) {
        let (tag, set_idx) = decompose(addr);
        let active_ways = if self.two_way() { 2 } else { WAYS };
        let victim = self.pick_victim(set_idx, active_ways);
        self.sets[set_idx][victim as usize] = Line {
            tag,
            valid: true,
            data,
        };
        self.touch(set_idx, victim);
    }

    /// Write-through: if the line containing `addr` is currently resident,
    /// update the byte in place. No-op otherwise. Used by SH-2 stores to
    /// keep cached copies coherent with the write that also went to bus.
    pub fn write_through_u8(&mut self, addr: u32, val: u8) {
        if let Some((set_idx, way)) = self.locate(addr) {
            self.sets[set_idx][way].data[(addr & 0xF) as usize] = val;
        }
    }
    pub fn write_through_u16(&mut self, addr: u32, val: u16) {
        if let Some((set_idx, way)) = self.locate(addr) {
            let off = (addr & 0xF) as usize;
            let b = val.to_be_bytes();
            self.sets[set_idx][way].data[off] = b[0];
            self.sets[set_idx][way].data[off + 1] = b[1];
        }
    }
    pub fn write_through_u32(&mut self, addr: u32, val: u32) {
        if let Some((set_idx, way)) = self.locate(addr) {
            let off = (addr & 0xF) as usize;
            let b = val.to_be_bytes();
            self.sets[set_idx][way].data[off..off + 4].copy_from_slice(&b);
        }
    }

    fn locate(&self, addr: u32) -> Option<(usize, usize)> {
        if !self.enabled() {
            return None;
        }
        let (tag, set_idx) = decompose(addr);
        let active_ways = if self.two_way() { 2 } else { WAYS };
        for way in 0..active_ways {
            if self.sets[set_idx][way].valid && self.sets[set_idx][way].tag == tag {
                return Some((set_idx, way));
            }
        }
        None
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

/// Extract a big-endian `u8` / `u16` / `u32` at `addr` from a 16-byte
/// line returned by [`Lookup::Hit`].
#[inline]
pub fn extract_u8(line: &[u8; LINE_BYTES], addr: u32) -> u8 {
    line[(addr & 0xF) as usize]
}
#[inline]
pub fn extract_u16(line: &[u8; LINE_BYTES], addr: u32) -> u16 {
    let o = (addr & 0xF) as usize;
    u16::from_be_bytes([line[o], line[o + 1]])
}
#[inline]
pub fn extract_u32(line: &[u8; LINE_BYTES], addr: u32) -> u32 {
    let o = (addr & 0xF) as usize;
    u32::from_be_bytes([line[o], line[o + 1], line[o + 2], line[o + 3]])
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

    fn line_with(start_byte: u8) -> [u8; 16] {
        core::array::from_fn(|i| start_byte.wrapping_add(i as u8))
    }

    #[test]
    fn cache_starts_disabled_and_bypasses() {
        let mut c = Cache::new();
        assert_eq!(c.lookup_fetch(0x1234), Lookup::Bypass);
    }

    #[test]
    fn enabled_cache_first_lookup_misses_install_then_hits() {
        let mut c = Cache::new();
        c.set_ccr(0x01);
        let addr = 0x6000_0040;
        assert_eq!(c.lookup_data(addr), Lookup::Miss);
        c.install(addr, line_with(0x10));
        let Lookup::Hit(data) = c.lookup_data(addr) else {
            panic!("expected hit after install");
        };
        assert_eq!(data[0], 0x10);
        // Same line, different offset within 16-byte line → still a hit,
        // and extract_* reads the right byte.
        let Lookup::Hit(data2) = c.lookup_data(addr | 0xC) else {
            panic!("expected hit on same line different offset");
        };
        assert_eq!(extract_u32(&data2, addr | 0xC), 0x1C1D1E1F);
    }

    #[test]
    fn install_evicts_lru_and_returns_new_data() {
        let mut c = Cache::new();
        c.set_ccr(0x01);
        let base: u32 = 0x6000_0000;
        // Fill all four ways of set 0 with distinguishable line data.
        for i in 0..4u32 {
            let addr = base | (i << 10);
            c.lookup_data(addr); // miss
            c.install(addr, line_with(i as u8 * 0x10));
        }
        // Way containing tag 0 is now the LRU (it was filled first and
        // never touched again). Touching tag 0 demotes tag 1 to LRU.
        let Lookup::Hit(d0) = c.lookup_data(base) else {
            panic!();
        };
        assert_eq!(d0[0], 0x00);
        // New install at tag 4 must evict tag 1.
        let evict_addr = base | (4 << 10);
        c.lookup_data(evict_addr); // miss
        c.install(evict_addr, line_with(0xC0));
        assert_eq!(
            c.lookup_data(base | (1 << 10)),
            Lookup::Miss,
            "tag 1 evicted"
        );
        let Lookup::Hit(d0_after) = c.lookup_data(base) else {
            panic!("tag 0 still resident");
        };
        assert_eq!(d0_after[0], 0x00);
    }

    #[test]
    fn write_through_updates_resident_line_only() {
        let mut c = Cache::new();
        c.set_ccr(0x01);
        let addr = 0x6000_0080;
        // Not resident yet — write-through is a silent no-op.
        c.write_through_u8(addr, 0xAA);
        assert_eq!(c.lookup_data(addr), Lookup::Miss);

        // Now install and write-through, then re-read.
        c.install(addr, line_with(0));
        c.write_through_u32(addr, 0xDEAD_BEEF);
        let Lookup::Hit(d) = c.lookup_data(addr) else {
            panic!();
        };
        assert_eq!(extract_u32(&d, addr), 0xDEAD_BEEF);
        // Byte writes land at the correct offset.
        c.write_through_u8(addr | 5, 0x99);
        let Lookup::Hit(d) = c.lookup_data(addr) else {
            panic!();
        };
        assert_eq!(extract_u8(&d, addr | 5), 0x99);
    }

    #[test]
    fn two_way_mode_only_uses_two_ways_per_set() {
        let mut c = Cache::new();
        c.set_ccr(0x01 | 0x08); // CE + TW
        let base: u32 = 0x6000_0000;
        for i in 0..2u32 {
            let a = base | (i << 10);
            c.lookup_data(a);
            c.install(a, line_with(i as u8));
        }
        assert!(matches!(c.lookup_data(base), Lookup::Hit(_)));
        assert!(matches!(c.lookup_data(base | (1 << 10)), Lookup::Hit(_)));
        // Touching tag 1 most recently → tag 0 is LRU within the active 2 ways.
        let a2 = base | (2 << 10);
        c.lookup_data(a2);
        c.install(a2, line_with(2));
        assert_eq!(c.lookup_data(base), Lookup::Miss, "tag 0 evicted in TW mode");
    }

    #[test]
    fn cp_bit_purges_and_is_write_only() {
        let mut c = Cache::new();
        c.set_ccr(0x01);
        let addr = 0x6000_0000;
        c.lookup_data(addr);
        c.install(addr, line_with(0x55));
        assert!(matches!(c.lookup_data(addr), Lookup::Hit(_)));
        c.set_ccr(0x01 | 0x10); // CE + CP
        assert_eq!(c.ccr() & 0x10, 0, "CP reads back as 0");
        assert_eq!(c.lookup_data(addr), Lookup::Miss, "purge invalidated lines");
    }

    #[test]
    fn assoc_purge_invalidates_only_the_matching_line() {
        let mut c = Cache::new();
        c.set_ccr(0x01); // CE
        // Two resident lines from the cacheable region, different sets.
        let a = 0x0600_1230;
        let b = 0x0600_4560;
        c.lookup_data(a);
        c.install(a, line_with(0xAA));
        c.lookup_data(b);
        c.install(b, line_with(0xBB));
        assert!(matches!(c.lookup_data(a), Lookup::Hit(_)));
        assert!(matches!(c.lookup_data(b), Lookup::Hit(_)));

        // Purge `a` via the region-2 alias (0x4000_0000 | a); the region bits
        // must be masked off internally so the tag still matches the line
        // installed from the cacheable region.
        c.assoc_purge(0x4000_0000 | a);
        assert_eq!(c.lookup_data(a), Lookup::Miss, "matching line invalidated");
        assert!(
            matches!(c.lookup_data(b), Lookup::Hit(_)),
            "non-matching line untouched"
        );

        // A purge of an address with no resident line is a harmless no-op,
        // and the region-5 alias (0xA000_0000) works identically.
        c.assoc_purge(0xA000_0000 | 0x0600_9990);
        assert!(matches!(c.lookup_data(b), Lookup::Hit(_)));
    }

    #[test]
    fn instruction_and_data_masks_route_independently() {
        let mut c = Cache::new();
        c.set_ccr(0x01 | 0x02); // CE + ID
        let addr = 0x6000_0000;
        assert_eq!(c.lookup_fetch(addr), Lookup::Bypass);
        assert_eq!(c.lookup_data(addr), Lookup::Miss);
        c.install(addr, line_with(0));
        // Data sees the install, fetch still bypasses.
        assert!(matches!(c.lookup_data(addr), Lookup::Hit(_)));
        assert_eq!(c.lookup_fetch(addr), Lookup::Bypass);

        let mut c = Cache::new();
        c.set_ccr(0x01 | 0x04); // CE + OD
        assert_eq!(c.lookup_data(addr), Lookup::Bypass);
        assert_eq!(c.lookup_fetch(addr), Lookup::Miss);
    }
}
