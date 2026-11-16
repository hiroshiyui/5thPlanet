//! SH7604 interrupt controller (INTC).
//!
//! Owns the on-chip-peripheral interrupt priority registers and a pending
//! bitmap of fired-but-not-yet-acknowledged interrupts. The CPU consults
//! [`Intc::next_pending`] at each instruction boundary; if any pending
//! interrupt's priority exceeds SR.imask the CPU vectors through.
//!
//! Selected register map (full set lives at 0xFFFFFE60 / 0xFFFFFEE0+):
//!
//! ```text
//!   IPRA  (FFFFFEE2, 16-bit)  priority nibbles: DIVU | DMAC | WDT/REF | -
//!   IPRB  (FFFFFE60, 16-bit)  priority nibbles: SCI  | FRT  | -       | -
//!   VCRA..VCRWDT, VCRDIV, VCRDMA0/1 — per-source 8-bit vector numbers
//!   ICR   (FFFFFEE0, 16-bit)  bit 15 NMIL, bit 8 NMIE, bit 0 VECMD
//! ```
//!
//! M1 implements IPRA, IPRB, ICR, and the priority/pending bookkeeping;
//! VCR* are stored verbatim for read-back and queried by [`Intc::vector_for`].

/// On-chip interrupt sources the SH-2 core needs to know about.
/// External (Saturn-side, NMI, IRL) sources arrive via [`Intc::raise`] too.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Source {
    Nmi,
    UserBreak,
    DivuOvf,
    DmacCh0,
    DmacCh1,
    Wdt,
    BscRef,
    SciEri,
    SciRxi,
    SciTxi,
    SciTei,
    FrtIci,
    FrtOcia,
    FrtOcib,
    FrtOvi,
    /// External-line interrupt (Saturn IRL1..IRL15). The level is the
    /// numeric value 1..=15.
    External(u8),
}

/// The SH7604 interrupt controller: the on-chip priority registers plus a
/// pending bitmap the CPU samples at each instruction boundary (see the module
/// header). [`Intc::raise`] flags a source; [`Intc::acknowledge`] clears it.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Intc {
    pub ipra: u16,
    pub iprb: u16,
    pub icr: u16,
    pub vcrwdt: u16,
    pub vcrdiv: u32,
    pub vcrdma0: u32,
    pub vcrdma1: u32,
    pub vcra: u16,
    pub vcrb: u16,
    pub vcrc: u16,
    pub vcrd: u16,
    /// Bit-set of pending sources, indexed by `Source::ord()` slot. A bit
    /// being set means the source has fired but the CPU hasn't accepted
    /// it yet. Cleared by [`Intc::acknowledge`].
    pending: u32,
    /// Level for [`Source::External`]; only valid when the External slot
    /// is set in `pending`. SH-2 IRL inputs are level-triggered.
    ext_level: u8,
    /// Vector latched for the pending [`Source::External`]. In auto-vector
    /// mode this is `64 + level`; in external-vector-fetch mode (how the
    /// Saturn drives the SH-2 — the SCU presents a fixed vector per source
    /// during the interrupt-acknowledge cycle) it is whatever the asserting
    /// device supplied via [`Intc::raise_external`].
    ext_vector: u8,
    /// Cached highest-priority pending source (NMI folded as level 16),
    /// **ignoring** SR.imask — recomputed only when the pending set, external
    /// level, or a priority register changes (all rare), so the per-instruction
    /// [`Intc::next_pending`] is O(1) instead of scanning every source. Kept in
    /// lockstep by [`Intc::recompute_best`] at every mutation point; serialized
    /// with the rest so it stays consistent across save/load. (Was a 17%-of-
    /// runtime hotspot when recomputed per instruction.)
    best: Option<(Source, u8)>,
}

impl Intc {
    pub fn new() -> Self {
        Self::default()
    }

    /// Flag interrupt `src` as pending; it fires once its priority exceeds
    /// SR.imask. External lines use auto-vector `64 + level` unless a device
    /// supplied its own vector.
    pub fn raise(&mut self, src: Source) {
        if let Source::External(level) = src {
            // Auto-vector mode: vector = 64 + level (SH7604 default for IRL
            // inputs). Devices that present their own vector use
            // [`Intc::raise_external`] instead.
            self.ext_level = level.min(15);
            self.ext_vector = 64 + level.min(15);
        }
        self.pending |= 1 << src.ord();
        self.recompute_best();
    }

    /// Recompute the cached highest-priority pending source (ignoring SR.imask;
    /// NMI folded as level 16). Must be called after **every** change to the
    /// pending set, `ext_level`, or a priority register (IPRA/IPRB) — it is the
    /// sole keeper of the [`Intc::best`] cache. Equivalent to the old inline
    /// per-instruction scan: since priority *is* the level, the global-highest
    /// source (then mask-filtered by `next_pending`) is the same source the old
    /// "highest among those above the mask" loop selected.
    fn recompute_best(&mut self) {
        // NMI is non-maskable (fixed level 16) and outranks everything.
        if self.pending & (1 << Source::Nmi.ord()) != 0 {
            self.best = Some((Source::Nmi, 16));
            return;
        }
        let mut best: Option<(Source, u8)> = None;
        for src in ALL_SOURCES {
            let src = if matches!(src, Source::External(_)) {
                Source::External(self.ext_level)
            } else {
                *src
            };
            if self.pending & (1 << src.ord()) == 0 {
                continue;
            }
            let lvl = self.priority_of(src);
            if best.is_none_or(|(_, l)| lvl > l) {
                best = Some((src, lvl));
            }
        }
        self.best = best;
    }

    /// Reset the INTC registers to their SH7604 reset values (Mednafen
    /// `INTC_Reset`: IPRA/IPRB/VCRA-D/VCRWDT/ICR = 0) and drop every pending
    /// internal request (NMI / user break / on-chip; the on-chip ones are
    /// re-derived from their now-reset device flags by the caller). The DMAC
    /// and DIVU vector registers (VCRDMA0/1, VCRDIV) live in those modules on
    /// the SH7604 and are **not** INTC-reset; the external IRL request is a
    /// pin level the host owns, so it is kept.
    pub fn reset_registers(&mut self) {
        self.ipra = 0;
        self.iprb = 0;
        self.icr = 0;
        self.vcrwdt = 0;
        self.vcra = 0;
        self.vcrb = 0;
        self.vcrc = 0;
        self.vcrd = 0;
        self.pending &= 1 << Source::External(0).ord();
        self.recompute_best();
    }

    /// Recompute the pending cache after a priority-register (IPRA/IPRB) write,
    /// which changes [`Self::priority_of`] for the affected sources. Called by
    /// the on-chip register write path.
    pub fn refresh_priorities(&mut self) {
        self.recompute_best();
    }

    /// Assert an external (IRL) interrupt at `level`, latching an explicit
    /// `vector` rather than the auto-vector `64 + level`. This models the
    /// SH7604's external-vector-fetch mode: the asserting device (on the
    /// Saturn, the SCU) presents the vector number during the
    /// interrupt-acknowledge cycle. The Saturn SCU's vectors are fixed at
    /// 0x40 + source index, independent of priority level.
    pub fn raise_external(&mut self, level: u8, vector: u8) {
        self.ext_level = level.min(15);
        self.ext_vector = vector;
        self.pending |= 1 << Source::External(0).ord();
        self.recompute_best();
    }

    /// Clear `src` from the pending set once the CPU has vectored to it.
    pub fn acknowledge(&mut self, src: Source) {
        self.pending &= !(1 << src.ord());
        self.recompute_best();
    }

    /// Drive a level-triggered on-chip source's pending bit directly from the
    /// device's current flag state. Unlike [`Intc::raise`] (edge-style, set
    /// once), this is called every step so the pending bit tracks the flag —
    /// e.g. an FRT compare-match interrupt asserts while OCFA is set and the
    /// match-enable is on, and clears the instant software W1C-clears OCFA.
    pub fn set_pending(&mut self, src: Source, active: bool) {
        // This is called every step for level-triggered sources, so skip the
        // cache recompute when the bit is already in the requested state (the
        // overwhelmingly common case — the flag rarely flips).
        let bit = 1 << src.ord();
        if (self.pending & bit != 0) == active {
            return;
        }
        if active {
            self.pending |= bit;
        } else {
            self.pending &= !bit;
        }
        self.recompute_best();
    }

    /// Return the highest-priority pending source whose level is > `mask`.
    /// `None` means no interrupt should be taken this cycle.
    pub fn next_pending(&self, sr_imask: u8) -> Option<(Source, u8)> {
        // O(1): the highest-priority pending source is cached (see `best` /
        // `recompute_best`); only the per-call SR.imask test remains. Because
        // priority == level, mask-filtering the global highest yields the same
        // source as the old "highest among those above the mask" scan: if the
        // highest is ≤ mask then every source is, so both return None.
        self.best.filter(|&(_, lvl)| lvl > sr_imask)
    }

    /// Priority level (0..=15) configured for `src` via IPRA/IPRB. Sources
    /// with no programmable priority return fixed/derived values.
    pub fn priority_of(&self, src: Source) -> u8 {
        match src {
            Source::Nmi => 16,
            Source::UserBreak => 15,
            Source::External(level) => level,
            // IPRA: bits 15-12 DIVU, 11-8 DMAC, 7-4 WDT/REF.
            Source::DivuOvf => ((self.ipra >> 12) & 0xF) as u8,
            Source::DmacCh0 | Source::DmacCh1 => ((self.ipra >> 8) & 0xF) as u8,
            Source::Wdt | Source::BscRef => ((self.ipra >> 4) & 0xF) as u8,
            // IPRB: bits 15-12 SCI, 11-8 FRT.
            Source::SciEri | Source::SciRxi | Source::SciTxi | Source::SciTei => {
                ((self.iprb >> 12) & 0xF) as u8
            }
            Source::FrtIci | Source::FrtOcia | Source::FrtOcib | Source::FrtOvi => {
                ((self.iprb >> 8) & 0xF) as u8
            }
        }
    }

    /// Vector number for `src`. NMI is fixed at 11; UserBreak at 12;
    /// external interrupts use the latched `ext_vector` (auto-vector
    /// `64+level`, or the device-supplied vector from [`raise_external`]).
    ///
    /// On-chip vectors follow Mednafen `GetPendingInt` (`sh7095.inc`), which
    /// matches the SH7604 HW manual's VCR layout: FRT **ICI** = VCRC[14:8],
    /// **OCIA and OCIB share** VCRC[6:0] (one "OCI" vector), **OVI** = VCRD[14:8]
    /// (VCRD's low byte is reserved). Every VCR-derived vector is masked to
    /// 7 bits (`& 0x7F`), as the hardware vector fields are 7 bits wide.
    pub fn vector_for(&self, src: Source) -> u8 {
        let v = match src {
            Source::Nmi => return 11,
            Source::UserBreak => return 12,
            Source::External(_) => return self.ext_vector,
            Source::DivuOvf => self.vcrdiv as u16,
            Source::DmacCh0 => self.vcrdma0 as u16,
            Source::DmacCh1 => self.vcrdma1 as u16,
            Source::Wdt => self.vcrwdt >> 8,
            Source::BscRef => self.vcrwdt,
            Source::SciEri => self.vcra >> 8,
            Source::SciRxi => self.vcra,
            Source::SciTxi => self.vcrb >> 8,
            Source::SciTei => self.vcrb,
            Source::FrtIci => self.vcrc >> 8,
            Source::FrtOcia | Source::FrtOcib => self.vcrc,
            Source::FrtOvi => self.vcrd >> 8,
        };
        (v & 0x7F) as u8
    }
}

impl Source {
    /// Stable ordinal for use in the pending bitmap.
    fn ord(self) -> u32 {
        match self {
            Source::Nmi => 0,
            Source::UserBreak => 1,
            Source::DivuOvf => 2,
            Source::DmacCh0 => 3,
            Source::DmacCh1 => 4,
            Source::Wdt => 5,
            Source::BscRef => 6,
            Source::SciEri => 7,
            Source::SciRxi => 8,
            Source::SciTxi => 9,
            Source::SciTei => 10,
            Source::FrtIci => 11,
            Source::FrtOcia => 12,
            Source::FrtOcib => 13,
            Source::FrtOvi => 14,
            Source::External(_) => 15,
        }
    }
}

/// Template list in the SH7604's **default priority order for equal IPR
/// levels** (highest first): user break, IRL, DIVU, DMAC0, DMAC1, WDT, REF,
/// SCI (ERI/RXI/TXI/TEI), FRT (ICI/OCI/OVI). [`Intc::recompute_best`] keeps
/// the first source at the winning level (strict `>`), so this order is the
/// tie-break — in particular an IRL at the same level as an on-chip source
/// wins (Mednafen `GetPendingInt` seeds `ipr = IRL` and lets on-chip sources
/// replace it only when strictly greater). `External` is replaced with
/// `External(ext_level)` at scan time.
const ALL_SOURCES: &[Source] = &[
    Source::UserBreak,
    Source::External(0),
    Source::DivuOvf,
    Source::DmacCh0,
    Source::DmacCh1,
    Source::Wdt,
    Source::BscRef,
    Source::SciEri,
    Source::SciRxi,
    Source::SciTxi,
    Source::SciTei,
    Source::FrtIci,
    Source::FrtOcia,
    Source::FrtOcib,
    Source::FrtOvi,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pending_returns_none() {
        let i = Intc::new();
        assert!(i.next_pending(0).is_none());
    }

    #[test]
    fn nmi_dispatches_even_with_full_mask() {
        let mut i = Intc::new();
        i.raise(Source::Nmi);
        let (src, lvl) = i.next_pending(15).unwrap();
        assert_eq!(src, Source::Nmi);
        assert_eq!(lvl, 16);
    }

    #[test]
    fn divu_priority_taken_from_ipra() {
        let mut i = Intc::new();
        i.ipra = 0xF000;
        i.raise(Source::DivuOvf);
        let (src, lvl) = i.next_pending(7).unwrap();
        assert_eq!(src, Source::DivuOvf);
        assert_eq!(lvl, 0xF);
    }

    #[test]
    fn priority_below_mask_is_suppressed() {
        let mut i = Intc::new();
        i.ipra = 0x0500;
        i.raise(Source::DmacCh0);
        assert!(i.next_pending(7).is_none());
    }

    #[test]
    fn higher_priority_wins() {
        let mut i = Intc::new();
        i.ipra = 0xA050; // DIVU=A, DMAC=0, WDT=5
        i.raise(Source::DivuOvf);
        i.raise(Source::Wdt);
        let (src, lvl) = i.next_pending(0).unwrap();
        assert_eq!(src, Source::DivuOvf);
        assert_eq!(lvl, 0xA);
    }

    #[test]
    fn reprioritising_an_already_pending_source_updates_next_pending() {
        // Guards the `best`/`recompute_best` cache invariant: a priority-register
        // write *after* a source is already pending must update next_pending.
        // (The other tests set IPRA before raise(), so they'd still pass even if
        // refresh_priorities — the hook the IPRA/IPRB write path calls — were
        // dropped; this one would not.)
        let mut i = Intc::new();
        i.ipra = 0x0500; // DMAC priority 5
        i.raise(Source::DmacCh0);
        assert!(i.next_pending(7).is_none(), "5 ≤ mask 7 → suppressed");

        // Raise DMAC's priority to 0xA *after* it's pending, the way an IPRA
        // write does, and re-arm the cache via the same hook intc_write8 calls.
        i.ipra = 0x0A00;
        i.refresh_priorities();
        let (src, lvl) = i.next_pending(7).expect("now 0xA > mask 7");
        assert_eq!((src, lvl), (Source::DmacCh0, 0xA));

        // Lowering it back below the mask must drop it again.
        i.ipra = 0x0300;
        i.refresh_priorities();
        assert!(i.next_pending(7).is_none(), "3 ≤ mask 7 → suppressed again");
    }

    #[test]
    fn external_irl_level_is_priority() {
        let mut i = Intc::new();
        i.raise(Source::External(9));
        let (src, lvl) = i.next_pending(5).unwrap();
        assert_eq!(src, Source::External(9));
        assert_eq!(lvl, 9);
    }

    #[test]
    fn external_auto_vector_is_64_plus_level() {
        let mut i = Intc::new();
        i.raise(Source::External(9));
        let (src, _) = i.next_pending(0).unwrap();
        assert_eq!(i.vector_for(src), 64 + 9);
    }

    #[test]
    fn raise_external_latches_an_explicit_vector() {
        // External-vector-fetch mode: the device supplies a vector that is
        // not 64+level (e.g. the Saturn SCU's VBlank-IN: level 15, vec 0x40).
        let mut i = Intc::new();
        i.raise_external(15, 0x40);
        let (src, lvl) = i.next_pending(0).unwrap();
        assert_eq!(lvl, 15);
        assert_eq!(i.vector_for(src), 0x40);
    }

    #[test]
    fn acknowledge_clears_pending() {
        let mut i = Intc::new();
        i.ipra = 0xF000;
        i.raise(Source::DivuOvf);
        assert!(i.next_pending(0).is_some());
        i.acknowledge(Source::DivuOvf);
        assert!(i.next_pending(0).is_none());
    }
    #[test]
    fn frt_vectors_follow_the_vcrc_vcrd_layout() {
        // SH7604 / Mednafen `GetPendingInt`: ICI = VCRC[14:8], OCIA and OCIB
        // share VCRC[6:0], OVI = VCRD[14:8]; every vector masked to 7 bits.
        let mut i = Intc::new();
        i.vcrc = 0x4152;
        i.vcrd = 0x6300;
        assert_eq!(i.vector_for(Source::FrtIci), 0x41);
        assert_eq!(i.vector_for(Source::FrtOcia), 0x52);
        assert_eq!(i.vector_for(Source::FrtOcib), 0x52, "OCIB shares OCIA's");
        assert_eq!(i.vector_for(Source::FrtOvi), 0x63, "OVI from VCRD high");
        // Bit 7 of each field is not part of the vector.
        i.vcrc = 0xC1D2;
        i.vcrd = 0xE3FF;
        i.vcrwdt = 0xC4C5;
        assert_eq!(i.vector_for(Source::FrtIci), 0x41);
        assert_eq!(i.vector_for(Source::FrtOcia), 0x52);
        assert_eq!(i.vector_for(Source::FrtOvi), 0x63);
        assert_eq!(i.vector_for(Source::Wdt), 0x44);
        assert_eq!(i.vector_for(Source::BscRef), 0x45);
    }

    #[test]
    fn irl_beats_an_on_chip_source_at_equal_priority() {
        // Mednafen `GetPendingInt` seeds ipr = IRL; an on-chip source replaces
        // it only when strictly greater, so at a tie the IRL wins — whichever
        // was raised first.
        let mut i = Intc::new();
        i.ipra = 0x7000; // DIVU priority 7
        i.raise(Source::DivuOvf);
        i.raise_external(7, 0x40);
        assert_eq!(i.next_pending(0), Some((Source::External(7), 7)));

        let mut i = Intc::new();
        i.iprb = 0x0900; // FRT priority 9
        i.raise_external(9, 0x41);
        i.set_pending(Source::FrtOvi, true);
        assert_eq!(i.next_pending(0), Some((Source::External(9), 9)));
        // A strictly higher on-chip priority still wins.
        i.iprb = 0x0A00;
        i.refresh_priorities();
        assert_eq!(i.next_pending(0), Some((Source::FrtOvi, 0xA)));
    }

    #[test]
    fn on_chip_ties_follow_the_default_priority_order() {
        // Equal IPR: DIVU > DMAC0 > DMAC1 > WDT (SH7604 default order).
        let mut i = Intc::new();
        i.ipra = 0x5550;
        i.raise(Source::Wdt);
        i.raise(Source::DmacCh1);
        i.raise(Source::DmacCh0);
        assert_eq!(i.next_pending(0), Some((Source::DmacCh0, 5)));
        i.raise(Source::DivuOvf);
        assert_eq!(i.next_pending(0), Some((Source::DivuOvf, 5)));
    }
}
