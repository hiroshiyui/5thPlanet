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

#[derive(Clone, Debug, Default)]
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
}

impl Intc {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn raise(&mut self, src: Source) {
        if let Source::External(level) = src {
            self.ext_level = level.min(15);
        }
        self.pending |= 1 << src.ord();
    }

    pub fn acknowledge(&mut self, src: Source) {
        self.pending &= !(1 << src.ord());
    }

    /// Return the highest-priority pending source whose level is > `mask`.
    /// `None` means no interrupt should be taken this cycle.
    pub fn next_pending(&self, sr_imask: u8) -> Option<(Source, u8)> {
        if self.pending & (1 << Source::Nmi.ord()) != 0 {
            return Some((Source::Nmi, 16));
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
            if lvl > sr_imask && best.map(|(_, l)| lvl > l).unwrap_or(true) {
                best = Some((src, lvl));
            }
        }
        best
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
    /// external auto-vectors live at 64+level.
    pub fn vector_for(&self, src: Source) -> u8 {
        match src {
            Source::Nmi => 11,
            Source::UserBreak => 12,
            Source::External(level) => 64 + level,
            Source::DivuOvf => self.vcrdiv as u8,
            Source::DmacCh0 => self.vcrdma0 as u8,
            Source::DmacCh1 => self.vcrdma1 as u8,
            Source::Wdt => (self.vcrwdt >> 8) as u8,
            Source::BscRef => self.vcrwdt as u8,
            Source::SciEri => (self.vcra >> 8) as u8,
            Source::SciRxi => self.vcra as u8,
            Source::SciTxi => (self.vcrb >> 8) as u8,
            Source::SciTei => self.vcrb as u8,
            Source::FrtIci => (self.vcrc >> 8) as u8,
            Source::FrtOcia => self.vcrc as u8,
            Source::FrtOcib => (self.vcrd >> 8) as u8,
            Source::FrtOvi => self.vcrd as u8,
        }
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

/// Template list; `External` is replaced with `External(ext_level)` at
/// scan time inside [`Intc::next_pending`].
const ALL_SOURCES: &[Source] = &[
    Source::UserBreak,
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
    Source::External(0),
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
    fn external_irl_level_is_priority() {
        let mut i = Intc::new();
        i.raise(Source::External(9));
        let (src, lvl) = i.next_pending(5).unwrap();
        assert_eq!(src, Source::External(9));
        assert_eq!(lvl, 9);
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
}
