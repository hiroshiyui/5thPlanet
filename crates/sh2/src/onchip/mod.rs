//! SH7604 on-chip module registers (FFFF FE00 .. FFFF FFFF).
//!
//! [`OnChip`] aggregates every on-chip peripheral and routes byte/halfword
//! /word accesses based on the address. The CPU owns one of these and
//! consults it first for any access into the on-chip range; only addresses
//! outside the range reach the external [`crate::bus::Bus`] impl.
//!
//! Address layout (selected):
//!
//! ```text
//!   FFFFFE00..0F  SCI       — serial communication interface (stub)
//!   FFFFFE10..1F  FRT       — free-running timer
//!   FFFFFE60..6F  INTC IPRB and VCRx
//!   FFFFFE80..9F  WDT       — watchdog timer (stub)
//!   FFFFFEE0..FF  INTC ICR / IPRA / VCRWDT
//!   FFFFFF00..1F  DIVU      — hardware divider
//!   FFFFFF40..7F  UBC       — user break controller (stub)
//!   FFFFFF80..BF  DMAC      — channels 0/1 + DMAOR + VCRDMA
//!   FFFFFFC0..FF  BSC       — bus state controller (stub)
//! ```

pub mod bsc;
pub mod divu;
pub mod dmac;
pub mod frt;
pub mod intc;
pub mod sci;
pub mod ubc;
pub mod wdt;

pub use intc::{Intc, Source};

use bsc::Bsc;
use divu::Divu;
use dmac::Dmac;
use frt::Frt;
use sci::Sci;
use ubc::Ubc;
use wdt::Wdt;

/// First on-chip-mapped address.
pub const ONCHIP_BASE: u32 = 0xFFFF_FE00;

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OnChip {
    pub sci: Sci,
    pub frt: Frt,
    pub intc: Intc,
    pub wdt: Wdt,
    pub divu: Divu,
    pub ubc: Ubc,
    pub dmac: Dmac,
    pub bsc: Bsc,
}

impl OnChip {
    pub fn new() -> Self {
        Self::default()
    }

    /// True iff `addr` falls within the SH7604 on-chip-peripheral range.
    #[inline]
    pub fn owns(addr: u32) -> bool {
        addr >= ONCHIP_BASE
    }

    pub fn read8(&mut self, addr: u32) -> u8 {
        match addr & 0x1FF {
            0x000..=0x00F => self.sci.read8(addr & 0xF),
            0x010..=0x01F => self.frt.read8(addr & 0xF),
            0x060..=0x06F => intc_read8(&self.intc, addr & 0xF, /*ipra*/ false),
            0x080..=0x09F => self.wdt.read8(addr & 0x1F),
            0x0E0..=0x0FF => intc_read8(&self.intc, addr & 0x1F, /*ipra*/ true),
            0x100..=0x11F => (self.divu.read32(addr & 0x1F) >> (8 * (3 - (addr & 3)))) as u8,
            0x140..=0x17F => self.ubc.read8(addr & 0x1F),
            0x180..=0x1BF => dmac_read8(&self.dmac, addr & 0x3F),
            0x1C0..=0x1FF => self.bsc.read8(addr & 0x1F),
            _ => 0,
        }
    }

    pub fn write8(&mut self, addr: u32, val: u8) {
        match addr & 0x1FF {
            0x000..=0x00F => self.sci.write8(addr & 0xF, val),
            0x010..=0x01F => self.frt.write8(addr & 0xF, val),
            0x060..=0x06F => intc_write8(&mut self.intc, addr & 0xF, val, false),
            0x080..=0x09F => self.wdt.write8(addr & 0x1F, val),
            0x0E0..=0x0FF => intc_write8(&mut self.intc, addr & 0x1F, val, true),
            0x100..=0x11F => {
                // DIVU is 32-bit-only on hw; byte writes are mostly nonsensical
                // but software occasionally touches the low byte of DVCR. We
                // perform a read-modify-write at the corresponding 32-bit slot.
                let off = addr & 0x1C;
                let shift = 8 * (3 - (addr & 3));
                let cur = self.divu.read32(off);
                let mask = !(0xFFu32 << shift);
                self.divu
                    .write32(off, (cur & mask) | ((val as u32) << shift));
            }
            0x140..=0x17F => self.ubc.write8(addr & 0x1F, val),
            0x180..=0x1BF => dmac_write8(&mut self.dmac, addr & 0x3F, val),
            0x1C0..=0x1FF => self.bsc.write8(addr & 0x1F, val),
            _ => {}
        }
    }

    pub fn read16(&mut self, addr: u32) -> u16 {
        ((self.read8(addr) as u16) << 8) | self.read8(addr + 1) as u16
    }

    pub fn write16(&mut self, addr: u32, val: u16) {
        // The WDT registers are written through a guarded 16-bit access (high
        // byte = magic key); route the whole halfword rather than splitting it
        // into two bytes, which would lose the key.
        if (0x080..=0x09F).contains(&(addr & 0x1FF)) {
            self.wdt.write16(addr & 0x1F, val);
            return;
        }
        self.write8(addr, (val >> 8) as u8);
        self.write8(addr + 1, val as u8);
    }

    pub fn read32(&mut self, addr: u32) -> u32 {
        // DIVU and DMAC have native 32-bit registers; other addresses fall
        // back to byte aggregation.
        match addr & 0x1FF {
            0x100..=0x11F => self.divu.read32(addr & 0x1F),
            0x180..=0x1BF => dmac_read32(&self.dmac, addr & 0x3F),
            _ => {
                ((self.read8(addr) as u32) << 24)
                    | ((self.read8(addr + 1) as u32) << 16)
                    | ((self.read8(addr + 2) as u32) << 8)
                    | self.read8(addr + 3) as u32
            }
        }
    }

    pub fn write32(&mut self, addr: u32, val: u32) {
        match addr & 0x1FF {
            0x100..=0x11F => self.divu.write32(addr & 0x1F, val),
            0x180..=0x1BF => dmac_write32(&mut self.dmac, addr & 0x3F, val),
            _ => {
                self.write8(addr, (val >> 24) as u8);
                self.write8(addr + 1, (val >> 16) as u8);
                self.write8(addr + 2, (val >> 8) as u8);
                self.write8(addr + 3, val as u8);
            }
        }
    }

    /// Advance the time-driven on-chip timers (FRT + WDT) by `cycles` CPU
    /// clocks. The CPU calls this once per instruction with the cycles that
    /// instruction consumed, so the free-running counter and watchdog track
    /// real elapsed time.
    pub fn advance_timers(&mut self, cycles: u32) {
        self.frt.tick(cycles);
        self.wdt.tick(cycles);
    }

    /// Refresh the level-triggered on-chip interrupt pending bits — FRT
    /// (input-capture / compare-match A,B / overflow), WDT (interval-mode
    /// overflow) and DMAC (per-channel transfer-end) — from each peripheral's
    /// current flag + enable state. Called once per instruction after the
    /// timers advance and any DMA runs, so the INTC reflects fresh device
    /// flags at the next instruction boundary. A flag cleared by software
    /// (FTCSR W1C, CHCR W0C of TE) drops the pending bit on the next refresh.
    pub fn refresh_interrupts(&mut self) {
        let (tier, ftcsr) = (self.frt.tier, self.frt.ftcsr);
        self.intc
            .set_pending(Source::FrtIci, tier & 0x80 != 0 && ftcsr & 0x80 != 0);
        self.intc
            .set_pending(Source::FrtOcia, tier & 0x08 != 0 && ftcsr & 0x08 != 0);
        self.intc
            .set_pending(Source::FrtOcib, tier & 0x04 != 0 && ftcsr & 0x04 != 0);
        self.intc
            .set_pending(Source::FrtOvi, tier & 0x02 != 0 && ftcsr & 0x02 != 0);
        self.intc
            .set_pending(Source::Wdt, self.wdt.interrupt_active());
        // CHCR transfer-end interrupt: TE (bit 1) AND IE (bit 2) both set.
        for (ch, src) in [(0usize, Source::DmacCh0), (1, Source::DmacCh1)] {
            self.intc
                .set_pending(src, self.dmac.channels[ch].chcr & 0b110 == 0b110);
        }
    }
}

// ---- INTC byte-level register helpers ----
// IPRA lives at FFFFFEE2 (offset 0x0E2 from ONCHIP_BASE; we mask to 0x1FF
// then to 0x1F because IPRA/ICR/VCRWDT all sit in the 0x0E0..=0x0FF range).
fn intc_read8(i: &Intc, off: u32, ipra_block: bool) -> u8 {
    if ipra_block {
        match off {
            0x00 => (i.icr >> 8) as u8,
            0x01 => i.icr as u8,
            0x02 => (i.ipra >> 8) as u8,
            0x03 => i.ipra as u8,
            0x04 => (i.vcrwdt >> 8) as u8,
            0x05 => i.vcrwdt as u8,
            _ => 0,
        }
    } else {
        // IPRB block
        match off {
            0x00 => (i.iprb >> 8) as u8,
            0x01 => i.iprb as u8,
            0x02 => (i.vcra >> 8) as u8,
            0x03 => i.vcra as u8,
            0x04 => (i.vcrb >> 8) as u8,
            0x05 => i.vcrb as u8,
            0x06 => (i.vcrc >> 8) as u8,
            0x07 => i.vcrc as u8,
            0x08 => (i.vcrd >> 8) as u8,
            0x09 => i.vcrd as u8,
            _ => 0,
        }
    }
}

fn intc_write8(i: &mut Intc, off: u32, val: u8, ipra_block: bool) {
    let v = val as u16;
    if ipra_block {
        match off {
            0x00 => i.icr = (i.icr & 0x00FF) | (v << 8),
            0x01 => i.icr = (i.icr & 0xFF00) | v,
            0x02 => i.ipra = (i.ipra & 0x00FF) | (v << 8),
            0x03 => i.ipra = (i.ipra & 0xFF00) | v,
            0x04 => i.vcrwdt = (i.vcrwdt & 0x00FF) | (v << 8),
            0x05 => i.vcrwdt = (i.vcrwdt & 0xFF00) | v,
            _ => {}
        }
    } else {
        match off {
            0x00 => i.iprb = (i.iprb & 0x00FF) | (v << 8),
            0x01 => i.iprb = (i.iprb & 0xFF00) | v,
            0x02 => i.vcra = (i.vcra & 0x00FF) | (v << 8),
            0x03 => i.vcra = (i.vcra & 0xFF00) | v,
            0x04 => i.vcrb = (i.vcrb & 0x00FF) | (v << 8),
            0x05 => i.vcrb = (i.vcrb & 0xFF00) | v,
            0x06 => i.vcrc = (i.vcrc & 0x00FF) | (v << 8),
            0x07 => i.vcrc = (i.vcrc & 0xFF00) | v,
            0x08 => i.vcrd = (i.vcrd & 0x00FF) | (v << 8),
            0x09 => i.vcrd = (i.vcrd & 0xFF00) | v,
            _ => {}
        }
    }
}

// ---- DMAC register helpers ----
fn dmac_read32(d: &Dmac, off: u32) -> u32 {
    match off {
        0x00 => d.channels[0].sar,
        0x04 => d.channels[0].dar,
        0x08 => d.channels[0].tcr,
        0x0C => d.channels[0].chcr,
        0x10 => d.channels[1].sar,
        0x14 => d.channels[1].dar,
        0x18 => d.channels[1].tcr,
        0x1C => d.channels[1].chcr,
        0x30 => d.dmaor,
        _ => 0,
    }
}

fn dmac_write32(d: &mut Dmac, off: u32, val: u32) {
    match off {
        0x00 => d.channels[0].sar = val,
        0x04 => d.channels[0].dar = val,
        0x08 => d.channels[0].tcr = val & 0x00FF_FFFF,
        0x0C => d.channels[0].chcr = val,
        0x10 => d.channels[1].sar = val,
        0x14 => d.channels[1].dar = val,
        0x18 => d.channels[1].tcr = val & 0x00FF_FFFF,
        0x1C => d.channels[1].chcr = val,
        0x30 => d.dmaor = val & 0xFFFF,
        _ => {}
    }
}

fn dmac_read8(d: &Dmac, off: u32) -> u8 {
    let word = dmac_read32(d, off & !3);
    (word >> (8 * (3 - (off & 3)))) as u8
}

fn dmac_write8(d: &mut Dmac, off: u32, val: u8) {
    let word_off = off & !3;
    let shift = 8 * (3 - (off & 3));
    let cur = dmac_read32(d, word_off);
    let mask = !(0xFFu32 << shift);
    dmac_write32(d, word_off, (cur & mask) | ((val as u32) << shift));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onchip_owns_range_boundary() {
        assert!(OnChip::owns(0xFFFF_FE00));
        assert!(OnChip::owns(0xFFFF_FFFF));
        assert!(!OnChip::owns(0xFFFF_FDFF));
        assert!(!OnChip::owns(0x0000_0000));
    }

    #[test]
    fn divu_dispatch_via_word_access() {
        let mut o = OnChip::new();
        // DVSR at FFFFFF00 (offset 0x100), DVDNT at FFFFFF04 (offset 0x104).
        o.write32(0xFFFF_FF00, 4);
        o.write32(0xFFFF_FF04, 25);
        assert_eq!(o.read32(0xFFFF_FF04) as i32, 6, "quotient lands in DVDNT");
        assert_eq!(o.read32(0xFFFF_FF10) as i32, 1, "remainder lands in DVDNTH");
    }

    #[test]
    fn frt_dispatch_via_byte_access() {
        let mut o = OnChip::new();
        // Write to TCR (offset 0x016 = FFFFFE16) selecting φ/8.
        o.write8(0xFFFF_FE16, 0x01);
        // Tick the FRT directly — register access doesn't auto-tick.
        o.frt.tick(8);
        assert_eq!(o.read16(0xFFFF_FE12), 1, "FRC at FFFFFE12");
    }

    #[test]
    fn intc_ipra_round_trip() {
        let mut o = OnChip::new();
        // IPRA at FFFFFEE2 — write the whole halfword.
        o.write16(0xFFFF_FEE2, 0xABCD);
        assert_eq!(o.intc.ipra, 0xABCD);
        assert_eq!(o.read16(0xFFFF_FEE2), 0xABCD);
    }

    #[test]
    fn dmac_channel_register_round_trip() {
        let mut o = OnChip::new();
        // CHCR0 at FFFFFF8C, CHCR1 at FFFFFF9C.
        o.write32(0xFFFF_FF8C, 0x0000_1234);
        o.write32(0xFFFF_FF9C, 0x0000_5678);
        assert_eq!(o.dmac.channels[0].chcr, 0x0000_1234);
        assert_eq!(o.dmac.channels[1].chcr, 0x0000_5678);
    }

    #[test]
    fn stub_peripherals_round_trip_byte_writes() {
        // UBC / BSC / SCI remain register-storage stubs that round-trip byte
        // writes. (The WDT is now behavioral — keyless byte writes are ignored
        // per its guarded-write protocol; see `wdt::tests`.)
        let mut o = OnChip::new();
        o.write8(0xFFFF_FF40, 0xBB); // UBC
        o.write8(0xFFFF_FFC0, 0xCC); // BSC
        o.write8(0xFFFF_FE00, 0xDD); // SCI
        assert_eq!(o.read8(0xFFFF_FF40), 0xBB);
        assert_eq!(o.read8(0xFFFF_FFC0), 0xCC);
        assert_eq!(o.read8(0xFFFF_FE00), 0xDD);
    }

    #[test]
    fn frt_compare_match_raises_ocia_and_clears_on_write_zero() {
        let mut o = OnChip::new();
        o.write16(0xFFFF_FE60, 0x0700); // IPRB FRT priority (bits 11..8) = 7
        o.write8(0xFFFF_FE10, 0x08); // TIER: OCIAE (output-compare-A int enable)
        o.write16(0xFFFF_FE14, 0x0005); // OCRA = 5
        o.advance_timers(5); // FRC reaches 5 → OCFA
        o.refresh_interrupts();
        assert_eq!(
            o.intc.next_pending(0),
            Some((Source::FrtOcia, 7)),
            "OCIA asserted while OCFA is set"
        );
        // SH7604 FRT: software clears OCFA by writing 0 to it (after reading 1),
        // not W1C; the pending bit drops next refresh.
        o.write8(0xFFFF_FE11, 0x00); // FTCSR: write 0 to the status flags → clear
        o.refresh_interrupts();
        assert_eq!(o.intc.next_pending(0), None, "cleared after write-0");
    }

    #[test]
    fn wdt_interval_overflow_raises_the_wdt_interrupt() {
        let mut o = OnChip::new();
        // IPRA WDT/REF priority (bits 7..4) = 5 so the source can be taken.
        o.write16(0xFFFF_FEE2, 0x0050);
        // WTCSR = TME | interval | φ/2 (0x20); WTCNT = 0xFF.
        o.write16(0xFFFF_FE80, 0xA520);
        o.write16(0xFFFF_FE80, 0x5AFF);
        o.advance_timers(2); // one count → overflow
        o.refresh_interrupts();
        assert_eq!(
            o.intc.next_pending(0),
            Some((Source::Wdt, 5)),
            "WDT interval overflow pending at IPRA priority"
        );
    }
}
