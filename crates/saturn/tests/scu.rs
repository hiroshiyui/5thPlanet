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
const D0EN: u32 = SCU_BASE + 0x10;
const DGO: u32 = 1 << 8;

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
    let (v, _) = sat.bus.read32(SCU_BASE + 0xF8, AccessKind::Data);
    assert_eq!(v, 0x0000_0004);
}

#[test]
fn dma_channel0_copies_bios_bytes_into_low_wram() {
    let mut sat = build();
    // Set up channel 0: copy 64 bytes from BIOS+0x10 to low WRAM 0x1000.
    sat.bus.write32(D0R, 0x0000_0010, AccessKind::Data); // source: BIOS+0x10
    sat.bus.write32(D0W, 0x0020_1000, AccessKind::Data); // dest: low WRAM
    sat.bus.write32(D0C, 0x40, AccessKind::Data); // count: 64 bytes
    sat.bus.write32(D0EN, DGO, AccessKind::Data); // trigger

    // Drain runs inside the next run_for batch.
    sat.run_for(512);

    // Verify 64 bytes landed at the destination and match the source.
    for i in 0..0x40u32 {
        let (b, _) = sat.bus.read8(0x0020_1000 + i, AccessKind::Data);
        assert_eq!(b, (0x10 + i) as u8, "byte {i:#x} mismatch");
    }
    // SCU should report the channel done (count zeroed, addresses past the block).
    let (cnt, _) = sat.bus.read32(D0C, AccessKind::Data);
    assert_eq!(cnt, 0, "transfer_count zeroed after DMA");
    let (read_after, _) = sat.bus.read32(D0R, AccessKind::Data);
    let (write_after, _) = sat.bus.read32(D0W, AccessKind::Data);
    assert_eq!(read_after, 0x0010 + 0x40);
    assert_eq!(write_after, 0x0020_1000 + 0x40);
}

#[test]
fn dma_with_non_multiple_of_four_bytes_handles_tail_correctly() {
    let mut sat = build();
    // 7 bytes — exercises the tail-loop after the 4-byte fast path.
    sat.bus.write32(D0R, 0x0000_0010, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_2000, AccessKind::Data);
    sat.bus.write32(D0C, 7, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    sat.run_for(512);
    for i in 0..7u32 {
        let (b, _) = sat.bus.read8(0x0020_2000 + i, AccessKind::Data);
        assert_eq!(b, (0x10 + i) as u8);
    }
    // Untouched byte beyond the count remains 0.
    let (b8, _) = sat.bus.read8(0x0020_2000 + 7, AccessKind::Data);
    assert_eq!(b8, 0);
}

#[test]
fn dma_with_zero_count_does_not_trigger() {
    let mut sat = build();
    sat.bus.write32(D0R, 0x0000_0010, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_3000, AccessKind::Data);
    sat.bus.write32(D0C, 0, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    sat.run_for(512);
    // Destination still zero.
    let (b, _) = sat.bus.read8(0x0020_3000, AccessKind::Data);
    assert_eq!(b, 0);
}

#[test]
fn channels_1_and_2_have_independent_state() {
    let mut sat = build();
    // Channel 1 — base 0x20.
    sat.bus
        .write32(SCU_BASE + 0x20, 0x0000_0020, AccessKind::Data);
    sat.bus
        .write32(SCU_BASE + 0x24, 0x0020_4000, AccessKind::Data);
    sat.bus.write32(SCU_BASE + 0x28, 0x10, AccessKind::Data);
    sat.bus.write32(SCU_BASE + 0x30, DGO, AccessKind::Data);
    // Channel 2 — base 0x40.
    sat.bus
        .write32(SCU_BASE + 0x40, 0x0000_0030, AccessKind::Data);
    sat.bus
        .write32(SCU_BASE + 0x44, 0x0020_5000, AccessKind::Data);
    sat.bus.write32(SCU_BASE + 0x48, 0x10, AccessKind::Data);
    sat.bus.write32(SCU_BASE + 0x50, DGO, AccessKind::Data);

    sat.run_for(512);

    for i in 0..0x10u32 {
        let (b1, _) = sat.bus.read8(0x0020_4000 + i, AccessKind::Data);
        assert_eq!(b1, (0x20 + i) as u8, "ch1 byte {i:#x}");
        let (b2, _) = sat.bus.read8(0x0020_5000 + i, AccessKind::Data);
        assert_eq!(b2, (0x30 + i) as u8, "ch2 byte {i:#x}");
    }
}

#[test]
fn dma_completion_raises_level0_dma_end_through_the_drainer() {
    let mut sat = build();
    sat.bus.write32(D0R, 0x0000_0010, AccessKind::Data);
    sat.bus.write32(D0W, 0x0020_3000, AccessKind::Data);
    sat.bus.write32(D0C, 0x40, AccessKind::Data);
    sat.bus.write32(D0EN, DGO, AccessKind::Data);
    sat.run_for(512);
    // IST bit for Level0DmaEnd should be set; software hasn't W1C'd it yet.
    let (ist, _) = sat.bus.read32(SCU_BASE + 0xB4, AccessKind::Data);
    assert_ne!(ist & (1 << ScuSource::Level0DmaEnd.bit()), 0);
}

#[test]
fn ist_is_w1c_via_bus_write() {
    let mut sat = build();
    sat.bus.scu.raise(ScuSource::Timer0);
    sat.bus.scu.raise(ScuSource::HBlankIn);
    // Acknowledge only Timer0 via W1C.
    sat.bus.write32(
        SCU_BASE + 0xB4,
        1 << ScuSource::Timer0.bit(),
        AccessKind::Data,
    );
    let (ist, _) = sat.bus.read32(SCU_BASE + 0xB4, AccessKind::Data);
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
    let (ist, _) = sat.bus.read32(SCU_BASE + 0xB4, AccessKind::Data);
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
