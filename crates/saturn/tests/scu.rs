//! SCU integration through the Saturn aggregate (M3 task #2).
//!
//! Verifies that a software-triggered DMA actually moves bytes through
//! the real bus once Saturn's `run_for` drains the queue.

use saturn::Saturn;
use saturn::ScuSource;
use saturn::scu::SCU_BASE;
use sh2::bus::{AccessKind, Bus};

/// Channel-0 register addresses on the bus (cache-through alias).
const D0R: u32 = SCU_BASE; // 0x05FE_0000
const D0W: u32 = SCU_BASE + 0x04;
const D0C: u32 = SCU_BASE + 0x08;
const D0AD: u32 = SCU_BASE + 0x0C;
const D0EN: u32 = SCU_BASE + 0x10;
const D0MD: u32 = SCU_BASE + 0x14;
const DGO: u32 = 1 << 8;
/// D*AD for a contiguous copy: read +4 per source word (bit 8), write +2
/// (16-bit B-bus step, code 1).
const AD_CONTIGUOUS: u32 = 0x100 | 0x01;
/// Low work-RAM scratch base used as a legal DMA source (the SCU can't DMA
/// from the BIOS A-bus).
const WRAM: u32 = 0x0020_0000;

/// Plant `n` bytes `0x10, 0x11, …` at work-RAM `base` through the bus.
fn plant(sat: &mut Saturn, base: u32, n: u32) {
    for i in 0..n {
        sat.bus.write8(base + i, (0x10 + i) as u8, AccessKind::Data);
    }
}

fn build() -> Saturn {
    let mut bios = vec![0u8; 512 * 1024];
    // Plant a recognisable byte pattern inside the BIOS image so the
    // DMA can transfer it into work RAM and we can fingerprint it.
    // 256 bytes is enough for the largest M3 DMA test below.
    for (i, slot) in bios.iter_mut().take(0x100).enumerate() {
        *slot = i as u8;
    }
    let mut sat = Saturn::new(bios);
    sat.reset();
    sat
}

#[test]
fn version_register_is_read_through_the_bus() {
    let mut sat = build();
    let (v, _) = sat.bus.read32(SCU_BASE + 0xC8, AccessKind::Data);
    assert_eq!(v, 0x0000_0004);
}

#[test]
fn dma_channel0_copies_a_block_between_work_ram() {
    let mut sat = build();
    plant(&mut sat, WRAM, 0x40);
    // Copy 64 bytes WRAM→WRAM, contiguous, with the address-update bits set.
    sat.bus.write32(D0R, WRAM, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_1000, AccessKind::Data);
    sat.bus.write32(D0C, 0x40, AccessKind::Data);
    sat.bus.write32(D0AD, AD_CONTIGUOUS, AccessKind::Data);
    // D*MD: manual factor (7) + RUP (bit16) + WUP (bit8) so D*R/D*W advance.
    sat.bus
        .write32(D0MD, (1 << 16) | (1 << 8) | 7, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data); // trigger

    sat.run_for(512);

    for i in 0..0x40u32 {
        let (b, _) = sat.bus.read8(0x0020_1000 + i, AccessKind::Data);
        assert_eq!(b, (0x10 + i) as u8, "byte {i:#x} mismatch");
    }
    let (cnt, _) = sat.bus.read32(D0C, AccessKind::Data);
    assert_eq!(cnt, 0, "transfer_count zeroed after DMA");
    let (read_after, _) = sat.bus.read32(D0R, AccessKind::Data);
    let (write_after, _) = sat.bus.read32(D0W, AccessKind::Data);
    assert_eq!(read_after, WRAM + 0x40, "RUP advanced D*R");
    assert_eq!(write_after, 0x0020_1000 + 0x40, "WUP advanced D*W");
}

#[test]
fn timer0_line_compare_triggers_an_armed_timer0_dma_channel() {
    // M13 H1e: Timer-0 (line compare) is SCU DMA start factor 3. An armed
    // factor-3 channel must fire when the raster first reaches scanline T0C —
    // the factor was never wired to the event before. Drive frames and verify
    // the transfer ran.
    //
    // Park the master on a `BRA .` self-loop (reset PC = 8) so a long run only
    // advances the raster — the default all-zero BIOS would fault into an
    // illegal-exception loop and clobber the DMA's source/dest over the frame.
    let mut bios = vec![0u8; 512 * 1024];
    bios[0..4].copy_from_slice(&0x0000_0008u32.to_be_bytes()); // reset PC
    bios[4..8].copy_from_slice(&0x0600_0000u32.to_be_bytes()); // reset SP
    bios[8..10].copy_from_slice(&0xAFFEu16.to_be_bytes()); // BRA .
    bios[10..12].copy_from_slice(&0x0009u16.to_be_bytes()); // NOP (delay slot)
    let mut sat = Saturn::new(bios);
    sat.reset();
    plant(&mut sat, WRAM, 0x40);
    sat.bus.write32(D0R, WRAM, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_1000, AccessKind::Data);
    sat.bus.write32(D0C, 0x40, AccessKind::Data);
    sat.bus.write32(D0AD, AD_CONTIGUOUS, AccessKind::Data);
    // Start factor 3 (Timer-0) + address-update bits; arm — waits for the event.
    sat.bus
        .write32(D0MD, (1 << 16) | (1 << 8) | 3, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    // Enable the SCU timers (T1MD TENB) and set the line compare to an early
    // scanline (a line is ~1700 SH-2 cycles, so line 10 lands well inside the
    // run below).
    sat.bus.write32(SCU_BASE + 0x90, 10, AccessKind::Data); // T0C = line 10
    sat.bus.write32(SCU_BASE + 0x98, 1, AccessKind::Data); // T1MD: TENB

    // Run past scanline 10; the line-compare event fires the factor-3 DMA,
    // which drains at the batch boundary.
    sat.run_for(40_000);

    for i in 0..0x40u32 {
        let (b, _) = sat.bus.read8(0x0020_1000 + i, AccessKind::Data);
        assert_eq!(
            b,
            (0x10 + i) as u8,
            "Timer-0 line-compare fired the factor-3 DMA: byte {i:#x}"
        );
    }
}

#[test]
fn dma_address_registers_hold_when_update_bits_clear() {
    let mut sat = build();
    plant(&mut sat, WRAM, 0x10);
    sat.bus.write32(D0R, WRAM, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_1000, AccessKind::Data);
    sat.bus.write32(D0C, 0x10, AccessKind::Data);
    sat.bus.write32(D0AD, AD_CONTIGUOUS, AccessKind::Data);
    sat.bus.write32(D0MD, 7, AccessKind::Data); // manual, RUP/WUP clear
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    sat.run_for(512);
    // Data still copied, but the address registers keep their programmed values.
    let (b, _) = sat.bus.read8(0x0020_1000, AccessKind::Data);
    assert_eq!(b, 0x10);
    let (read_after, _) = sat.bus.read32(D0R, AccessKind::Data);
    assert_eq!(read_after, WRAM, "RUP clear → D*R unchanged");
}

#[test]
fn dma_with_a_partial_longword_count() {
    let mut sat = build();
    plant(&mut sat, WRAM, 0x10);
    // 6 bytes — three 16-bit transfers, not a whole number of longwords.
    sat.bus.write32(D0R, WRAM, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_2000, AccessKind::Data);
    sat.bus.write32(D0C, 6, AccessKind::Data);
    sat.bus.write32(D0AD, AD_CONTIGUOUS, AccessKind::Data);
    sat.bus.write32(D0MD, 7, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    sat.run_for(512);
    for i in 0..6u32 {
        let (b, _) = sat.bus.read8(0x0020_2000 + i, AccessKind::Data);
        assert_eq!(b, (0x10 + i) as u8);
    }
    let (b6, _) = sat.bus.read8(0x0020_2000 + 6, AccessKind::Data);
    assert_eq!(b6, 0, "byte past the count untouched");
}

#[test]
fn dma_with_zero_count_does_not_trigger() {
    let mut sat = build();
    sat.bus.write32(D0R, WRAM, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_3000, AccessKind::Data);
    sat.bus.write32(D0C, 0, AccessKind::Data);
    sat.bus.write32(D0MD, 7, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    sat.run_for(512);
    let (b, _) = sat.bus.read8(0x0020_3000, AccessKind::Data);
    assert_eq!(b, 0);
}

#[test]
fn channels_1_and_2_have_independent_state() {
    let mut sat = build();
    plant(&mut sat, WRAM + 0x100, 0x10); // ch1 source
    plant(&mut sat, WRAM + 0x200, 0x10); // ch2 source
    // Channel 1 — base 0x20.
    sat.bus
        .write32(SCU_BASE + 0x20, WRAM + 0x100, AccessKind::Data);
    sat.bus
        .write32(SCU_BASE + 0x24, 0x0020_4000, AccessKind::Data);
    sat.bus.write32(SCU_BASE + 0x28, 0x10, AccessKind::Data);
    sat.bus
        .write32(SCU_BASE + 0x2C, AD_CONTIGUOUS, AccessKind::Data);
    sat.bus.write32(SCU_BASE + 0x34, 7, AccessKind::Data); // manual factor
    sat.bus.write32(SCU_BASE + 0x30, DGO, AccessKind::Data);
    // Channel 2 — base 0x40.
    sat.bus
        .write32(SCU_BASE + 0x40, WRAM + 0x200, AccessKind::Data);
    sat.bus
        .write32(SCU_BASE + 0x44, 0x0020_5000, AccessKind::Data);
    sat.bus.write32(SCU_BASE + 0x48, 0x10, AccessKind::Data);
    sat.bus
        .write32(SCU_BASE + 0x4C, AD_CONTIGUOUS, AccessKind::Data);
    sat.bus.write32(SCU_BASE + 0x54, 7, AccessKind::Data);
    sat.bus.write32(SCU_BASE + 0x50, DGO, AccessKind::Data);

    sat.run_for(512);

    for i in 0..0x10u32 {
        let (b1, _) = sat.bus.read8(0x0020_4000 + i, AccessKind::Data);
        assert_eq!(b1, (0x10 + i) as u8, "ch1 byte {i:#x}");
        let (b2, _) = sat.bus.read8(0x0020_5000 + i, AccessKind::Data);
        assert_eq!(b2, (0x10 + i) as u8, "ch2 byte {i:#x}");
    }
}

#[test]
fn dma_from_bios_area_is_refused() {
    // The SCU shares the A-bus with the BIOS ROM and cannot DMA from it.
    let mut sat = build();
    sat.bus.write32(D0R, 0x0000_0010, AccessKind::Data); // BIOS source
    sat.bus.write32(D0W, 0x0020_6800, AccessKind::Data);
    sat.bus.write32(D0C, 0x10, AccessKind::Data);
    sat.bus.write32(D0AD, AD_CONTIGUOUS, AccessKind::Data);
    sat.bus.write32(D0MD, 7, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    sat.run_for(512);
    let (b, _) = sat.bus.read8(0x0020_6800, AccessKind::Data);
    assert_eq!(b, 0, "BIOS-sourced DMA transfers nothing");
}

#[test]
fn dma_indirect_mode_walks_a_table_of_transfers() {
    let mut sat = build();
    // Two source words.
    sat.bus.write32(WRAM, 0xAABB_CCDD, AccessKind::Data);
    sat.bus.write32(WRAM + 0x10, 0x1122_3344, AccessKind::Data);
    // Indirect table at 0x0020_3000: {size, dst, src} triplets; the last
    // entry flags bit 31 of its source word.
    let tbl = 0x0020_3000;
    sat.bus.write32(tbl, 4, AccessKind::Data);
    sat.bus.write32(tbl + 4, 0x0020_4000, AccessKind::Data);
    sat.bus.write32(tbl + 8, WRAM, AccessKind::Data);
    sat.bus.write32(tbl + 0xC, 4, AccessKind::Data);
    sat.bus.write32(tbl + 0x10, 0x0020_4010, AccessKind::Data);
    sat.bus
        .write32(tbl + 0x14, (WRAM + 0x10) | 0x8000_0000, AccessKind::Data); // last
    sat.bus.write32(D0W, tbl, AccessKind::Data); // table address
    sat.bus.write32(D0AD, AD_CONTIGUOUS, AccessKind::Data);
    sat.bus.write32(D0MD, (1 << 24) | 7, AccessKind::Data); // indirect + manual
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    sat.run_for(512);

    let (a, _) = sat.bus.read32(0x0020_4000, AccessKind::Data);
    let (b, _) = sat.bus.read32(0x0020_4010, AccessKind::Data);
    assert_eq!(a, 0xAABB_CCDD, "indirect entry 0");
    assert_eq!(b, 0x1122_3344, "indirect entry 1");
}

#[test]
fn dma_indirect_table_accepts_sh2_cache_through_alias() {
    let mut sat = build();
    sat.bus.write32(WRAM, 0xAABB_CCDD, AccessKind::Data);

    let tbl = 0x0600_3000;
    sat.bus.write32(tbl, 4, AccessKind::Data);
    sat.bus.write32(tbl + 4, 0x0020_4000, AccessKind::Data);
    sat.bus
        .write32(tbl + 8, WRAM | 0x8000_0000, AccessKind::Data);

    sat.bus.write32(D0W, tbl | 0x2000_0000, AccessKind::Data);
    sat.bus.write32(D0AD, AD_CONTIGUOUS, AccessKind::Data);
    sat.bus.write32(D0MD, (1 << 24) | 7, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    sat.run_for(512);

    let (value, _) = sat.bus.read32(0x0020_4000, AccessKind::Data);
    assert_eq!(value, 0xAABB_CCDD);
}

#[test]
fn dma_with_a_hardware_start_factor_waits_for_the_event() {
    let mut sat = build();
    plant(&mut sat, WRAM, 0x10);
    sat.bus.write32(D0R, WRAM, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_7000, AccessKind::Data);
    sat.bus.write32(D0C, 0x10, AccessKind::Data);
    sat.bus.write32(D0AD, AD_CONTIGUOUS, AccessKind::Data);
    sat.bus.write32(D0MD, 0, AccessKind::Data); // start factor 0 = VBlank-IN
    sat.bus.write32(D0EN, DGO, AccessKind::Data); // arm — must NOT fire yet

    // Run a fraction of a frame (before VBlank-IN): the DMA stays armed.
    sat.run_for(4096);
    let (b, _) = sat.bus.read8(0x0020_7000, AccessKind::Data);
    assert_eq!(
        b, 0,
        "factor-0 DMA must not fire on enable, only on VBlank-IN"
    );

    // Run a full frame so VBlank-IN occurs and triggers the transfer.
    sat.run_for(480_000);
    let (b, _) = sat.bus.read8(0x0020_7000, AccessKind::Data);
    assert_eq!(b, 0x10, "VBlank-IN triggered the DMA");
}

#[test]
fn dma_completion_raises_level0_dma_end_through_the_drainer() {
    let mut sat = build();
    sat.bus.write32(D0R, 0x0000_0010, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_3000, AccessKind::Data);
    sat.bus.write32(D0C, 0x40, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    sat.run_for(512);
    // IST bit for Level0DmaEnd should be set; software hasn't cleared it yet.
    let (ist, _) = sat.bus.read32(SCU_BASE + 0xA4, AccessKind::Data);
    assert_ne!(ist & (1 << ScuSource::Level0DmaEnd.bit()), 0);
}

#[test]
fn ist_is_write_zero_clear_via_bus_write() {
    let mut sat = build();
    sat.bus.scu.raise(ScuSource::Timer0);
    sat.bus.scu.raise(ScuSource::HBlankIn);
    // Acknowledge only Timer0 via write-0-clear.
    sat.bus.write32(
        SCU_BASE + 0xA4,
        !(1 << ScuSource::Timer0.bit()),
        AccessKind::Data,
    );
    let (ist, _) = sat.bus.read32(SCU_BASE + 0xA4, AccessKind::Data);
    assert_eq!(
        ist,
        1 << ScuSource::HBlankIn.bit(),
        "Timer0 cleared, HBlankIn retained"
    );
}

#[test]
fn dma_end_propagates_into_master_sh2_intc_as_external_level5() {
    // Configure: SR.imask = 0 so the master accepts any IRL; install a
    // recognisable handler at Level-0 DMA's SCU vector (0x4B, fixed at
    // 0x40 + source index — not the auto-vector 64+level); trigger a
    // Level-0 DMA; run; assert the master vectored to the handler.
    let mut sat = build();
    sat.master_mut().regs.sr.set_imask(0);
    let handler_addr: u32 = 0x0020_6000;
    // VBR is 0 after reset; install the handler at VBR + vector*4.
    // (The vector table lives in BIOS at offset 0, but our test BIOS
    // is just a pattern — we write directly into the bus's BIOS image.
    // The bus drops writes to BIOS, so install via the bus's bios slot.)
    let vec_offset = ScuSource::Level0DmaEnd.vector() as usize * 4;
    sat.bus.bios = saturn::BiosRom::new({
        let mut img = vec![0u8; 512 * 1024];
        for (i, b) in img.iter_mut().take(0x100).enumerate() {
            *b = i as u8;
        }
        img[vec_offset..vec_offset + 4].copy_from_slice(&handler_addr.to_be_bytes());
        img
    });
    // Re-reset so the master picks PC/SP from the (now patterned-anew) BIOS.
    sat.reset();
    sat.release_slave(); // we don't care about the slave here

    // Trigger a DMA so SCU raises Level0DmaEnd.
    sat.bus.write32(D0R, 0x0000_0010, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_3000, AccessKind::Data);
    sat.bus.write32(D0C, 0x10, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data);

    // Give the system several Saturn::run_for batches to drain.
    sat.run_for(2048);

    // Master should have vectored into the handler at handler_addr OR
    // its PC should be progressing inside it. We can't easily tell from
    // outside which exact instruction it's on, but pc != 0 (BIOS reset
    // vector area was empty so master would have hit illegal instruction
    // there too — but the External should have fired first and pushed
    // SR.imask up to 5).
    assert!(
        sat.master().regs.sr.imask() >= 5,
        "master's SR.imask should be raised by an accepted level-5 interrupt; \
         actual = {}",
        sat.master().regs.sr.imask(),
    );
}

#[test]
fn scu_interrupt_is_deferred_while_the_master_is_in_a_delay_slot() {
    // The SH-2 must not accept an interrupt inside a branch delay slot — the
    // per-instruction SCU sample in `step_cpus` is gated on `!next_is_delay_slot()`.
    // The reset vector runs an infinite delayed-branch loop (`BRA .` + `NOP`
    // slot); the Level0DmaEnd handler is installed at its SCU vector, so an
    // accepted interrupt raises SR.imask to that source's level (5).
    let mut img = vec![0u8; 512 * 1024];
    img[0..4].copy_from_slice(&0x0000_0020u32.to_be_bytes()); // reset PC = 0x20
    img[4..8].copy_from_slice(&0x0604_0000u32.to_be_bytes()); // reset SP (high WRAM)
    img[0x20..0x22].copy_from_slice(&0xAFFEu16.to_be_bytes()); // BRA .  (target 0x20)
    img[0x22..0x24].copy_from_slice(&0x0009u16.to_be_bytes()); // NOP    (delay slot)
    let vec_off = ScuSource::Level0DmaEnd.vector() as usize * 4;
    img[vec_off..vec_off + 4].copy_from_slice(&0x0020_6000u32.to_be_bytes());

    let mut sat = Saturn::new(img);
    sat.reset();

    // Execute the BRA with interrupts still masked (reset default imask=0xF) so
    // nothing preempts it; the next instruction is the delay slot.
    sat.debug_step_master();
    assert!(
        sat.master().next_is_delay_slot(),
        "after the delayed branch the master is about to run the delay slot"
    );

    // Now open the gate and make an SCU interrupt pending while in the slot.
    sat.bus.write32(SCU_BASE + 0xA0, 0, AccessKind::Data); // IMS: unmask all
    sat.master_mut().regs.sr.set_imask(0); // accept any IRL
    sat.bus.scu.raise(ScuSource::Level0DmaEnd);

    // Step the delay slot: the interrupt must be DEFERRED, not accepted here.
    sat.debug_step_master();
    assert_eq!(
        sat.master().regs.sr.imask(),
        0,
        "interrupt must NOT be accepted inside the delay slot"
    );
    assert!(
        !sat.master().next_is_delay_slot(),
        "the delay slot was consumed"
    );

    // The next (non-delay-slot) instruction accepts the still-pending edge.
    sat.debug_step_master();
    assert!(
        sat.master().regs.sr.imask() >= 5,
        "the deferred interrupt is accepted at the first non-delay-slot boundary; imask = {}",
        sat.master().regs.sr.imask()
    );
}

// ---- SCU-DSP host integration (increment 2) ----
const PPAF: u32 = SCU_BASE + 0x80;
const PPD: u32 = SCU_BASE + 0x84;

/// Start the DSP at PC 0 via the PPAF control port (LEF loads PC, EXF runs).
fn dsp_start_at_zero(sat: &mut Saturn) {
    sat.bus
        .write32(PPAF, (1 << 15) | (1 << 16), AccessKind::Data);
}

#[test]
fn scu_dsp_runs_program_and_raises_dsp_end_interrupt() {
    let mut sat = build();
    // Program: just ENDI (stop + raise the program-end interrupt).
    let endi = (0b11u32 << 30) | (0b11 << 28) | (1 << 27);
    sat.bus.write32(PPD, endi, AccessKind::Data); // loaded at PC 0
    dsp_start_at_zero(&mut sat);
    sat.run_for(512);
    assert!(sat.bus.scu.dsp.stopped(), "DSP halted at ENDI");
    let (ist, _) = sat.bus.read32(SCU_BASE + 0xA4, AccessKind::Data);
    assert_ne!(
        ist & (1 << ScuSource::DspEnd.bit()),
        0,
        "ENDI must raise the SCU DSP-end interrupt"
    );
}

#[test]
fn scu_dsp_dma_copies_work_ram_into_data_ram() {
    let mut sat = build();
    // Two source words in low work RAM.
    sat.bus.write32(0x0020_1000, 0x1234_5678, AccessKind::Data);
    sat.bus.write32(0x0020_1004, 0x9ABC_DEF0, AccessKind::Data);
    // Microcode: RA0 = source word address; DMA 2 words A/B-bus→data RAM
    // bank 0 (dir 0, add 4); ENDI.
    let ra0 = 0x0020_1000u32 >> 2;
    let mvi_ra0 = (0b10u32 << 30) | (6 << 26) | (ra0 & 0x01FF_FFFF);
    let dma = (0b11u32 << 30) | (1 << 15) | 2; // add_sel=1(→4 bytes), size=2
    let endi = (0b11u32 << 30) | (0b11 << 28) | (1 << 27);
    for w in [mvi_ra0, dma, endi] {
        sat.bus.write32(PPD, w, AccessKind::Data);
    }
    dsp_start_at_zero(&mut sat);
    sat.run_for(2048);
    assert_eq!(sat.bus.scu.dsp.data_ram[0][0], 0x1234_5678);
    assert_eq!(sat.bus.scu.dsp.data_ram[0][1], 0x9ABC_DEF0);
    // RA0 advanced by 2 words (hold=0 → write-back).
    assert_eq!(sat.bus.scu.dsp.regs.ra0, ra0 + 2);
}

/// Regression (BIOS boot-animation BGM): WA0/RA0 carry the SH-2 cache-through
/// region bit — the BIOS programs WA0 as e.g. `0x25A5_0000` (sound RAM
/// `0x05A5_0000 | 0x2000_0000`). `exec_dsp_dma` must strip it to the physical
/// A/B-bus space before the bus access; without the mask the bus matches no
/// region, so the read is open bus (0) and the write is dropped. The boot
/// jingle is staged into VDP1 VRAM and copied to sound RAM by exactly this
/// DSP-RAM → cache-through-sound-RAM DMA, so an unmasked write left the keyed
/// voice playing silence.
#[test]
fn scu_dsp_dma_to_cache_through_address_lands_at_the_physical_region() {
    let mut sat = build();
    // Two words to copy out of DSP data-RAM bank 0.
    sat.bus.scu.dsp.data_ram[0][0] = 0x1234_5678;
    sat.bus.scu.dsp.data_ram[0][1] = 0x9ABC_DEF0;
    // WA0 = cache-through sound RAM 0x25A5_0000 (stored as a >>2 longword addr).
    sat.bus.scu.dsp.regs.wa0 = 0x25A5_0000u32 >> 2;
    // Microcode: DMA DSP-RAM bank 0 → A/B-bus via WA0 (from_dsp=bit 12), 2
    // words, add_sel=1 (→4 bytes); ENDI.
    let dma = (0b11u32 << 30) | (1 << 12) | (1 << 15) | 2;
    let endi = (0b11u32 << 30) | (0b11 << 28) | (1 << 27);
    for w in [dma, endi] {
        sat.bus.write32(PPD, w, AccessKind::Data);
    }
    dsp_start_at_zero(&mut sat);
    sat.run_for(2048);
    // The data must land at the *physical* sound RAM address, not be dropped.
    assert_eq!(sat.bus.read32(0x05A5_0000, AccessKind::Data).0, 0x1234_5678);
    assert_eq!(sat.bus.read32(0x05A5_0004, AccessKind::Data).0, 0x9ABC_DEF0);
}

/// The *read* direction (A/B-bus → DSP data RAM) must strip the cache-through
/// bit too — the sibling of the write-direction regression above. Without the
/// mask the read matches no region and returns open bus (0).
#[test]
fn scu_dsp_dma_read_from_cache_through_address_reads_the_physical_region() {
    let mut sat = build();
    // Seed the physical sound RAM that the cache-through alias points at.
    sat.bus.write32(0x05A6_0000, 0x0BAD_F00D, AccessKind::Data);
    sat.bus.write32(0x05A6_0004, 0xFEED_BEEF, AccessKind::Data);
    // RA0 = cache-through sound RAM 0x25A6_0000 (stored as a >>2 longword addr).
    sat.bus.scu.dsp.regs.ra0 = 0x25A6_0000u32 >> 2;
    // DMA 2 words A/B-bus → data RAM bank 0 (dir 0), add_sel=1 (→4 bytes); ENDI.
    let dma = (0b11u32 << 30) | (1 << 15) | 2;
    let endi = (0b11u32 << 30) | (0b11 << 28) | (1 << 27);
    for w in [dma, endi] {
        sat.bus.write32(PPD, w, AccessKind::Data);
    }
    dsp_start_at_zero(&mut sat);
    sat.run_for(2048);
    // The read must hit the physical region, not return open bus (0).
    assert_eq!(sat.bus.scu.dsp.data_ram[0][0], 0x0BAD_F00D);
    assert_eq!(sat.bus.scu.dsp.data_ram[0][1], 0xFEED_BEEF);
}

/// The address mask is applied only at the bus access — the `update_addr`
/// write-back advances the *register*, which keeps its full cache-through
/// address so a subsequent DMA still aliases correctly.
#[test]
fn scu_dsp_dma_writeback_preserves_the_cache_through_bits_in_wa0() {
    let mut sat = build();
    sat.bus.scu.dsp.data_ram[0][0] = 0x1111_1111;
    sat.bus.scu.dsp.data_ram[0][1] = 0x2222_2222;
    let wa0 = 0x25A7_0000u32 >> 2; // cache-through sound RAM, >>2 longword addr
    sat.bus.scu.dsp.regs.wa0 = wa0;
    // DMA DSP-RAM bank 0 → A/B-bus via WA0 (from_dsp=bit 12), 2 words; ENDI.
    let dma = (0b11u32 << 30) | (1 << 12) | (1 << 15) | 2;
    let endi = (0b11u32 << 30) | (0b11 << 28) | (1 << 27);
    for w in [dma, endi] {
        sat.bus.write32(PPD, w, AccessKind::Data);
    }
    dsp_start_at_zero(&mut sat);
    sat.run_for(2048);
    // WA0 advances by 2 longwords yet retains its cache-through high bit
    // (0x2000_0000 >> 2 = 0x0800_0000).
    assert_eq!(sat.bus.scu.dsp.regs.wa0, wa0 + 2);
    assert_ne!(
        sat.bus.scu.dsp.regs.wa0 & 0x0800_0000,
        0,
        "cache-through bit retained in the register"
    );
    // And the data still landed at the physical region.
    assert_eq!(sat.bus.read32(0x05A7_0000, AccessKind::Data).0, 0x1111_1111);
    assert_eq!(sat.bus.read32(0x05A7_0004, AccessKind::Data).0, 0x2222_2222);
}

#[test]
fn scu_timer0_fires_at_the_compare_line() {
    let mut sat = build();
    // Mask all SCU interrupts so the (garbage-running) master never acks
    // Timer 0, leaving its IST status bit set for us to observe.
    sat.bus.scu.ims = 0xFFFF;
    sat.bus.scu.t0c = 10; // compare at scanline 10
    sat.bus.scu.t1md = 1; // TENB — timer enable
    // Advance past scanline 10 (~10 lines) but stay inside the frame.
    sat.run_for(40_000);
    assert_ne!(
        sat.bus.scu.ist & (1 << 3),
        0,
        "Timer 0 raised when the raster first reaches T0C"
    );
}

#[test]
fn scu_timer0_dormant_when_disabled() {
    let mut sat = build();
    sat.bus.scu.ims = 0xFFFF;
    sat.bus.scu.t0c = 10;
    // T1MD left 0 → timer disabled.
    sat.run_for(40_000);
    assert_eq!(
        sat.bus.scu.ist & (1 << 3),
        0,
        "no Timer 0 interrupt while TENB is clear"
    );
}

#[test]
fn scu_hblank_in_fires_on_the_hblank_edge() {
    let mut sat = build();
    // Mask all SCU interrupts so the master never acks and IST accumulates.
    sat.bus.scu.ims = 0xFFFF;
    // HBlank-IN is independent of the timer enable; a few scanlines suffice.
    sat.run_for(40_000);
    assert_ne!(
        sat.bus.scu.ist & (1 << 2),
        0,
        "HBlank-IN raised on the HBLANK rising edge"
    );
}

#[test]
fn scu_timer1_fires_when_enabled() {
    let mut sat = build();
    sat.bus.scu.ims = 0xFFFF;
    sat.bus.scu.t1md = 1; // TENB, mode 0 (every line)
    sat.bus.scu.t1s = 50; // fire ~50 dots into the line
    sat.run_for(40_000);
    assert_ne!(
        sat.bus.scu.ist & (1 << 4),
        0,
        "Timer 1 fires within the line when enabled"
    );
}

#[test]
fn scu_timer1_dormant_when_disabled() {
    let mut sat = build();
    sat.bus.scu.ims = 0xFFFF;
    sat.bus.scu.t1s = 50; // T1MD left 0 → timer disabled
    sat.run_for(40_000);
    assert_eq!(
        sat.bus.scu.ist & (1 << 4),
        0,
        "no Timer 1 interrupt while TENB is clear"
    );
}
