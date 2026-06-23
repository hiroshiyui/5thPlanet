//! Free-running-timer (FRT) coverage beyond the inline `frt::tests`: OCRB
//! compare-match, the φ/32 and φ/128 prescaler periods, CCLRA clear-on-OCRA-
//! match periodic mode, OCRB selection through TOCR, the FTCSR write-0-clear
//! of each individual status flag, and the OVF/OCFB interrupt arming.
//!
//! These drive the real lazy/event-scheduled timer through [`OnChip`]
//! (`advance_timers` is the delta→absolute shim over `frt_wdt_update`), so they
//! exercise the prescaler shift-math + FRC engine end-to-end. Values follow the
//! SH7604 hardware-manual FRT semantics cited in `onchip/frt.rs`.

use sh2::OnChip;

// FRT register block base + byte offsets (absolute on-chip addresses).
const FRT: u32 = 0xFFFF_FE10;
const TIER: u32 = FRT; // +0x00
const FTCSR: u32 = FRT + 0x01;
const OCR: u32 = FRT + 0x04; // OCRA or OCRB depending on TOCR.OCRS
const TCR: u32 = FRT + 0x06;
const TOCR: u32 = FRT + 0x07;
const FICR: u32 = FRT + 0x08;

#[test]
fn ocrb_compare_match_sets_ocfb() {
    let mut o = OnChip::new();
    // Select OCRB (TOCR.OCRS, bit 4) before writing the compare value.
    o.write8(TOCR, 0x10);
    o.write16(OCR, 0x0020); // OCRB = 0x20
    o.advance_timers(0x20 * 8); // φ/8 (default TCR): 0x20 ticks → FRC = 0x20
    assert_eq!(o.frt.frc, 0x0020);
    assert_eq!(o.frt.ftcsr & 0x04, 0x04, "OCFB set on the OCRB match");
    assert_eq!(o.frt.ftcsr & 0x08, 0x00, "OCFA not set (OCRA still 0, never reached)");
}

#[test]
fn ocrb_selection_round_trips_through_tocr() {
    // With OCRS set the OCR window addresses OCRB; clearing it addresses OCRA.
    let mut o = OnChip::new();
    o.write8(TOCR, 0x10); // select OCRB
    o.write16(OCR, 0xBEEF);
    assert_eq!(o.frt.ocrb, 0xBEEF, "write hit OCRB");
    assert_eq!(o.read16(OCR), 0xBEEF, "read returns OCRB");

    o.write8(TOCR, 0x00); // select OCRA
    o.write16(OCR, 0x1234);
    assert_eq!(o.frt.ocra, 0x1234, "write hit OCRA");
    assert_eq!(o.frt.ocrb, 0xBEEF, "OCRB untouched");
    assert_eq!(o.read16(OCR), 0x1234, "read returns OCRA");
}

#[test]
fn prescaler_phi_over_32_period() {
    let mut o = OnChip::new();
    o.write8(TCR, 0x01); // CKS=1 → φ/32
    o.advance_timers(31);
    assert_eq!(o.frt.frc, 0, "31 cycles below the φ/32 threshold");
    o.advance_timers(1);
    assert_eq!(o.frt.frc, 1, "32 accumulated cycles → one FRC tick");
    o.advance_timers(64);
    assert_eq!(o.frt.frc, 3, "64 more cycles → two more ticks");
}

#[test]
fn prescaler_phi_over_128_period() {
    let mut o = OnChip::new();
    o.write8(TCR, 0x02); // CKS=2 → φ/128
    o.advance_timers(127);
    assert_eq!(o.frt.frc, 0, "below the φ/128 threshold");
    o.advance_timers(1);
    assert_eq!(o.frt.frc, 1, "128 cycles → one tick");
}

#[test]
fn external_clock_cks3_freezes_the_counter() {
    // CKS=3 selects the external FTCI clock (undriven on the Saturn): the FRC
    // does not advance from φ — matches Mednafen/Yabause (cf. onchip/frt.rs).
    let mut o = OnChip::new();
    o.write8(TCR, 0x03);
    o.advance_timers(100_000);
    assert_eq!(o.frt.frc, 0, "CKS=3 external clock: FRC frozen");
}

#[test]
fn cclra_clears_counter_on_ocra_match_for_periodic_timer() {
    // CCLRA (FTCSR bit 0) zeroes FRC on an OCRA match, giving an OCRA-period
    // free-running reload. At φ/8 (default TCR), 4 FRC ticks = 32 cycles land
    // FRC on OCRA=4 → match/clear → 0; 8 more cycles resume counting at 1.
    let mut o = OnChip::new();
    o.write16(OCR, 0x0004); // OCRA = 4
    o.write8(FTCSR, 0x01); // CCLRA
    o.advance_timers(4 * 8); // FRC hits 4 → OCFA → cleared to 0
    assert_eq!(o.frt.frc, 0, "FRC reloaded to 0 on the OCRA match");
    assert_eq!(o.frt.ftcsr & 0x08, 0x08, "OCFA still flagged");
    o.advance_timers(8);
    assert_eq!(o.frt.frc, 1, "counting resumes from 0");
}

#[test]
fn cclra_single_large_delta_does_not_skip_the_match() {
    // A lazy update that crosses several OCRA periods in ONE call must still
    // fire/reset at each boundary (the segmented advance), not jump past it.
    // OCRA=4, CCLRA, φ/8: 5 ticks = 40 cycles in one go → 1,2,3,4(reset 0),1.
    let mut o = OnChip::new();
    o.write16(OCR, 0x0004);
    o.write8(FTCSR, 0x01); // CCLRA
    o.advance_timers(5 * 8);
    assert_eq!(o.frt.frc, 1, "matched+reset mid-jump, not FRC=5");
    assert_eq!(o.frt.ftcsr & 0x08, 0x08, "OCFA flagged");
}

#[test]
fn ftcsr_write_zero_clears_an_individual_flag_and_keeps_the_others() {
    // Status flags are write-0-to-clear *after a read-1*, not W1C: a flag only
    // clears if it was read as 1 since the last clear (SH7604 FTCSR latch,
    // modeled by `ftcsr_read_ones`). Read FTCSR first to latch all four, then
    // write a byte with OCFA=1 (kept) and OCFB=0 (cleared) leaving only OCFA.
    let mut o = OnChip::new();
    o.frt.ftcsr = 0b1000_1110; // ICF | OCFA | OCFB | OVF all set
    let _ = o.read8(FTCSR); // read-1 latches the status bits for the clear
    // Write 1 to ICF and OCFA, 0 to OCFB and OVF.
    o.write8(FTCSR, 0b1000_1000);
    assert_eq!(
        o.frt.ftcsr, 0b1000_1000,
        "kept ICF+OCFA (wrote 1), cleared OCFB+OVF (wrote 0)"
    );
}

#[test]
fn input_capture_latches_and_icie_gates_the_interrupt_return() {
    let mut o = OnChip::new();
    o.write16(OCR, 0xFFFF); // keep OCRA out of the way
    o.advance_timers(0x55 * 8); // φ/8 (default TCR): advance FRC to 0x55
    assert_eq!(o.frt.frc, 0x55);
    assert!(!o.frt.input_capture(), "ICIE clear → no interrupt requested");
    assert_eq!(o.frt.ficr, 0x55, "FRC latched into FICR");
    assert_eq!(o.frt.ftcsr & 0x80, 0x80, "ICF set");
    // FICR is read-only — a write to its offsets is dropped.
    o.write8(FICR, 0x00);
    o.write8(FICR + 1, 0x00);
    assert_eq!(o.frt.ficr, 0x55, "FICR write-protected");

    o.write8(TIER, 0x80); // ICIE
    assert!(o.frt.input_capture(), "ICIE set → interrupt requested");
}

// ---- OnChip-level interrupt arming (refresh_interrupts) ----

#[test]
fn overflow_arms_the_ovi_interrupt_only_when_tier_ovie_set() {
    use sh2::InterruptSource;
    let mut o = OnChip::new();
    o.write16(0xFFFF_FE60, 0x0700); // IPRB FRT priority = 7
    o.frt.frc = 0xFFFF;
    o.frt.tier = 0x02; // OVIE — overflow interrupt enable
    o.advance_timers(8); // φ/8 (default TCR): one FRC tick wraps → OVF
    assert_eq!(o.frt.ftcsr & 0x02, 0x02, "OVF set on wrap");
    o.refresh_interrupts();
    assert_eq!(
        o.intc.next_pending(0),
        Some((InterruptSource::FrtOvi, 7)),
        "OVI armed at the IPRB priority"
    );
    // Clearing OVF drops the request next refresh — but only after a read-1
    // (SH7604 FTCSR latch): read FTCSR to latch the flags, then write 0.
    let _ = o.read8(0xFFFF_FE11);
    o.write8(0xFFFF_FE11, 0x00);
    o.refresh_interrupts();
    assert_eq!(o.intc.next_pending(0), None, "OVF cleared → request dropped");
}

#[test]
fn ocfb_arms_the_ocib_interrupt_at_the_frt_priority() {
    use sh2::InterruptSource;
    let mut o = OnChip::new();
    o.write16(0xFFFF_FE60, 0x0500); // IPRB FRT priority = 5
    o.write8(0xFFFF_FE17, 0x10); // TOCR.OCRS → select OCRB
    o.write16(0xFFFF_FE14, 0x0003); // OCRB = 3
    o.write8(0xFFFF_FE10, 0x04); // TIER.OCIBE (bit 2)
    o.advance_timers(3 * 8); // φ/8 (default TCR): FRC reaches 3 → OCFB
    o.refresh_interrupts();
    assert_eq!(
        o.intc.next_pending(0),
        Some((InterruptSource::FrtOcib, 5)),
        "OCIB asserted while OCFB is set"
    );
}

// ---- Event scheduling (next_ts) ----

#[test]
fn next_ts_schedules_the_nearest_ocra_match() {
    // With OCRA = 0x10 at φ/8 from a fresh epoch, the next event is the OCRA
    // compare-match 0x10 ticks = 0x80 cycles out (Mednafen FRT_WDT_Recalc_NET).
    let mut o = OnChip::new();
    o.write16(OCR, 0x0010); // OCRA = 0x10 (TOCR.OCRS clear → OCRA)
    o.advance_timers(0); // force a recalc at cycle 0
    assert_eq!(o.timer_next_ts(), 0x10 * 8, "next_ts at the OCRA match cycle");
}

#[test]
fn next_ts_is_never_when_external_clock_and_wdt_off() {
    // CKS=3 (external, FRC frozen) with the WDT disabled → no event will ever
    // fire, so next_ts is u64::MAX and the step gate never wakes the timer.
    let mut o = OnChip::new();
    o.write8(TCR, 0x03); // external clock
    o.advance_timers(0); // recalc
    assert_eq!(o.timer_next_ts(), u64::MAX, "no live timer → never");
    o.advance_timers(1_000_000);
    assert_eq!(o.frt.frc, 0, "FRC stays frozen under the external clock");
}
