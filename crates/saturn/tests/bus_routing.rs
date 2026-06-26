//! Saturn bus routing (M2 task #3). One test per region — verify reads
//! and writes land in the right backing store, that BIOS mirrors,
//! and that unmapped addresses behave as open bus.

use saturn::SaturnBus;
use saturn::bus::{
    ABUS_BBUS_BASE, BACKUP_BASE, BIOS_BASE, HIGH_WRAM_BASE, LOW_WRAM_BASE, SCSP_RAM_BASE,
    SMPC_BASE, SOUND_BASE,
};
use sh2::bus::{AccessKind, Bus};

fn bios_with_pattern() -> Vec<u8> {
    // 512 KiB image with a recognizable pattern: byte i = i & 0xFF.
    (0..512 * 1024).map(|i| (i & 0xFF) as u8).collect()
}

fn fresh() -> SaturnBus {
    SaturnBus::new(bios_with_pattern())
}

#[test]
fn bios_reads_from_image_and_mirrors_within_window() {
    let mut bus = fresh();
    // First quarter — direct read.
    let (a, _) = bus.read32(BIOS_BASE + 0x100, AccessKind::Fetch);
    assert_eq!(a, 0x0001_0203, "matches byte pattern at offset 0x100");
    // Past the image size, mirror back to the start.
    let (b, _) = bus.read32(BIOS_BASE + 0x8_0000 + 0x100, AccessKind::Fetch);
    assert_eq!(b, a, "BIOS mirrors across the 1 MiB window");
}

#[test]
fn bios_writes_are_silently_dropped() {
    let mut bus = fresh();
    let (before, _) = bus.read8(BIOS_BASE + 0x10, AccessKind::Data);
    bus.write8(BIOS_BASE + 0x10, 0xFF, AccessKind::Data);
    let (after, _) = bus.read8(BIOS_BASE + 0x10, AccessKind::Data);
    assert_eq!(before, after, "BIOS is read-only");
}

#[test]
fn low_wram_round_trip() {
    let mut bus = fresh();
    bus.write32(LOW_WRAM_BASE + 0x1234, 0xCAFE_F00D, AccessKind::Data);
    let (v, _) = bus.read32(LOW_WRAM_BASE + 0x1234, AccessKind::Data);
    assert_eq!(v, 0xCAFE_F00D);
}

#[test]
fn high_wram_round_trip_and_independent_of_low_wram() {
    let mut bus = fresh();
    bus.write32(HIGH_WRAM_BASE + 0x4000, 0xDEAD_BEEF, AccessKind::Data);
    let (v, _) = bus.read32(HIGH_WRAM_BASE + 0x4000, AccessKind::Data);
    assert_eq!(v, 0xDEAD_BEEF);
    // Same offset into low WRAM is independent — different region.
    let (z, _) = bus.read32(LOW_WRAM_BASE + 0x4000, AccessKind::Data);
    assert_eq!(z, 0);
}

#[test]
fn backup_ram_odd_byte_packing_and_mirrors() {
    let mut bus = fresh();
    // Internal backup RAM is odd-byte packed: data lives only on odd byte
    // addresses; even bytes read 0 and ignore writes (hardware / MAME).
    bus.write8(BACKUP_BASE + 0x11, 0x77, AccessKind::Data); // odd → stored
    bus.write8(BACKUP_BASE + 0x10, 0x55, AccessKind::Data); // even → dropped
    let (odd, _) = bus.read8(BACKUP_BASE + 0x11, AccessKind::Data);
    let (even, _) = bus.read8(BACKUP_BASE + 0x10, AccessKind::Data);
    assert_eq!(odd, 0x77);
    assert_eq!(even, 0x00, "even byte lanes are wired to 0");
    // 32 KiB of data spans a 64 KiB window, then mirrors within 512 KiB.
    let (mirror, _) = bus.read8(BACKUP_BASE + 0x1_0000 + 0x11, AccessKind::Data);
    assert_eq!(mirror, 0x77);
}

#[test]
fn scsp_ram_round_trip_and_mirrors() {
    let mut bus = fresh();
    // The BIOS sound-RAM init write-verifies this region; it must hold
    // writes (unlike the open-bus A/B-bus stub).
    bus.write32(SCSP_RAM_BASE, 0x0000_A000, AccessKind::Data);
    let (v, _) = bus.read32(SCSP_RAM_BASE, AccessKind::Data);
    assert_eq!(v, 0x0000_A000);
    // 512 KiB RAM mirrored within the 1 MiB window.
    let (mirror, _) = bus.read32(SCSP_RAM_BASE + 0x8_0000, AccessKind::Data);
    assert_eq!(mirror, 0x0000_A000);
}

#[test]
fn smpc_sound_abus_bbus_stubs_return_zero() {
    let mut bus = fresh();
    for &base in &[SMPC_BASE, SOUND_BASE, ABUS_BBUS_BASE] {
        bus.write32(base + 0x100, 0xAAAA_BBBB, AccessKind::Data);
        let (v, _) = bus.read32(base + 0x100, AccessKind::Data);
        assert_eq!(v, 0, "stub region returns 0 even after a write");
    }
}

#[test]
fn unmapped_address_reads_zero() {
    let mut bus = fresh();
    // Between low WRAM end (0x002F_FFFF) and sound base (0x0040_0000)
    // is unmodeled space.
    let (v, _) = bus.read32(0x0035_0000, AccessKind::Data);
    assert_eq!(v, 0);
    bus.write32(0x0035_0000, 0xFFFF_FFFF, AccessKind::Data);
    let (v2, _) = bus.read32(0x0035_0000, AccessKind::Data);
    assert_eq!(v2, 0, "writes to unmapped space are dropped");
}

#[test]
fn wait_states_per_region_are_sane() {
    let mut bus = fresh();
    // Just exercise that each region returns *some* wait count without
    // panicking; concrete values are an implementation detail we don't
    // want to over-specify here, but BIOS should outlast work RAM.
    let (_, bios_w) = bus.read32(BIOS_BASE, AccessKind::Fetch);
    let (_, low_w) = bus.read32(LOW_WRAM_BASE, AccessKind::Data);
    let (_, high_w) = bus.read32(HIGH_WRAM_BASE, AccessKind::Data);
    assert!(bios_w >= low_w);
    assert!(low_w >= high_w);
}

#[test]
fn endianness_is_big_for_word_writes() {
    let mut bus = fresh();
    bus.write32(LOW_WRAM_BASE, 0x1122_3344, AccessKind::Data);
    let (b0, _) = bus.read8(LOW_WRAM_BASE, AccessKind::Data);
    let (b1, _) = bus.read8(LOW_WRAM_BASE + 1, AccessKind::Data);
    let (b2, _) = bus.read8(LOW_WRAM_BASE + 2, AccessKind::Data);
    let (b3, _) = bus.read8(LOW_WRAM_BASE + 3, AccessKind::Data);
    assert_eq!([b0, b1, b2, b3], [0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn high_wram_round_trip_8_and_16_bit() {
    // Exercise the 8/16-bit dispatch arms for high WRAM (the 32-bit arm is
    // covered above); high WRAM is the 1 MiB region at 0x0600_0000.
    let mut bus = fresh();
    bus.write8(HIGH_WRAM_BASE + 0x10, 0xA5, AccessKind::Data);
    bus.write16(HIGH_WRAM_BASE + 0x20, 0xBEEF, AccessKind::Data);
    assert_eq!(bus.read8(HIGH_WRAM_BASE + 0x10, AccessKind::Data).0, 0xA5);
    assert_eq!(
        bus.read16(HIGH_WRAM_BASE + 0x20, AccessKind::Data).0,
        0xBEEF
    );
}

#[test]
fn scsp_regs_region_routes_distinctly_from_scsp_ram() {
    // SCSP control/slot/DSP registers live at 0x05B0_0000 — a different bus
    // arm from the sound RAM at 0x05A0_0000. A write to one must not appear
    // in the other.
    use saturn::bus::SCSP_REGS_BASE;
    let mut bus = fresh();
    bus.write16(SCSP_RAM_BASE, 0x1234, AccessKind::Data);
    // The regs read path returns whatever the SCSP control bank decodes; we
    // only assert the regions are independent: sound RAM keeps its value.
    let _ = bus.read16(SCSP_REGS_BASE, AccessKind::Data);
    assert_eq!(bus.read16(SCSP_RAM_BASE, AccessKind::Data).0, 0x1234);
}

#[test]
fn empty_cartridge_slot_floats_high() {
    // The default slot is empty: the whole cart window floats high (0xFF) and
    // the cart-ID byte at 0x04FF_FFFF reads 0xFF (ID_NONE).
    use saturn::bus::SaturnBus;
    let mut bus = SaturnBus::with_blank_bios();
    assert_eq!(bus.read8(0x0200_0000, AccessKind::Data).0, 0xFF);
    assert_eq!(bus.read8(0x04FF_FFFF, AccessKind::Data).0, 0xFF, "cart-ID");
    // Writes to an empty slot are dropped (no backing store) — still 0xFF.
    bus.write8(0x0200_0000, 0x00, AccessKind::Data);
    assert_eq!(bus.read8(0x0200_0000, AccessKind::Data).0, 0xFF);
}

#[test]
fn cd_block_register_window_routes_to_cd_block() {
    // The CD-block host-register window is at 0x0589_0000. Reads route to the
    // CD-block (not open bus / not the A/B-bus stub); a fresh CD-block with no
    // disc returns defined HIRQ/CR state rather than panicking.
    let mut bus = fresh();
    // HIRQ register reads are well-defined; just exercise the routing arm at
    // all three widths without asserting a specific value (CD state is owned
    // by cd_block tests).
    let _ = bus.read8(0x0589_0000, AccessKind::Data);
    let _ = bus.read16(0x0589_0008, AccessKind::Data);
    let _ = bus.read32(0x0589_0008, AccessKind::Data);
}

#[test]
fn vdp1_and_vdp2_vram_route_to_their_owners() {
    // VDP1 VRAM (0x05C0_0000) and VDP2 VRAM (0x05E0_0000) are distinct owned
    // regions, separate from the A/B-bus stub. Each holds its own write.
    let mut bus = fresh();
    bus.write16(0x05C0_0000, 0x1111, AccessKind::Data); // VDP1 VRAM
    bus.write16(0x05E0_0000, 0x2222, AccessKind::Data); // VDP2 VRAM
    assert_eq!(bus.read16(0x05C0_0000, AccessKind::Data).0, 0x1111);
    assert_eq!(bus.read16(0x05E0_0000, AccessKind::Data).0, 0x2222);
}

#[test]
fn slave_fti_write16_sets_slave_capture_flag() {
    // A 16-bit write to 0x0100_0000..0x017F_FFFF pulses the *slave* SH-2's FRT
    // input capture. The bus can't reach the cores, so it latches a flag the
    // aggregate drains. The flag starts clear; the master's must stay clear.
    use saturn::bus::SLAVE_FTI_BASE;
    let mut bus = fresh();
    assert!(!bus.slave_input_capture);
    assert!(!bus.master_input_capture);
    bus.write16(SLAVE_FTI_BASE, 0x0000, AccessKind::Data);
    assert!(bus.slave_input_capture, "slave FTI latched");
    assert!(!bus.master_input_capture, "master FTI untouched");
}

#[test]
fn master_fti_write16_sets_master_capture_flag() {
    // The companion region 0x0180_0000..0x01FF_FFFF pulses the *master*'s FTI.
    use saturn::bus::MASTER_FTI_BASE;
    let mut bus = fresh();
    bus.write16(MASTER_FTI_BASE + 0x4000, 0xFFFF, AccessKind::Data);
    assert!(bus.master_input_capture, "master FTI latched");
    assert!(!bus.slave_input_capture, "slave FTI untouched");
}

#[test]
fn fti_regions_are_open_bus_on_read() {
    // The FTI trigger regions are write-only; reads fall through to open bus
    // (0). Writes of any width pulse the target input-capture line.
    use saturn::bus::{MASTER_FTI_BASE, SLAVE_FTI_BASE};
    let mut bus = fresh();
    assert_eq!(bus.read32(SLAVE_FTI_BASE, AccessKind::Data).0, 0);
    assert_eq!(bus.read32(MASTER_FTI_BASE, AccessKind::Data).0, 0);
    bus.write8(SLAVE_FTI_BASE, 0xFF, AccessKind::Data);
    assert!(bus.slave_input_capture, "8-bit writes also pulse FTI");
}

#[test]
fn backup_ram_round_trip_via_16_and_32_bit_bus() {
    // Cover the 16/32-bit bus arms for backup RAM (the 8-bit path is covered
    // above). Odd-byte packing means only odd lanes carry data.
    let mut bus = fresh();
    bus.write16(BACKUP_BASE + 0x20, 0x00CD, AccessKind::Data); // odd lane <- 0xCD
    assert_eq!(bus.read16(BACKUP_BASE + 0x20, AccessKind::Data).0, 0x00CD);
    bus.write32(BACKUP_BASE + 0x40, 0x00AB_00CD, AccessKind::Data);
    assert_eq!(
        bus.read32(BACKUP_BASE + 0x40, AccessKind::Data).0,
        0x00AB_00CD
    );
}

#[test]
fn unmapped_space_is_open_bus_at_all_widths() {
    // The gap between low WRAM and the sound area is unmodeled: reads 0, drops
    // writes, at every access width.
    let mut bus = fresh();
    let gap = 0x0035_0000;
    bus.write8(gap, 0xFF, AccessKind::Data);
    bus.write16(gap, 0xFFFF, AccessKind::Data);
    bus.write32(gap, 0xFFFF_FFFF, AccessKind::Data);
    assert_eq!(bus.read8(gap, AccessKind::Data).0, 0);
    assert_eq!(bus.read16(gap, AccessKind::Data).0, 0);
    assert_eq!(bus.read32(gap, AccessKind::Data).0, 0);
}

#[test]
fn scsp_region_charges_bbus_wait_states() {
    // Mednafen `scu.inc` BBusRW: an SH-2 read from the SCSP region is always
    // two 16-bit B-bus accesses at +24 each (= +48, any width); a write costs
    // a +17 write-finish. Regression for the VF2 SFX wedge: with 0 waits the
    // game's sound-request spin-timeout (0x10000 mailbox reads) expired faster
    // than the 68k driver's IRQ-masked wake-from-sleep re-init, latching its
    // "sound driver wedged" flag and silently dropping all later SFX.
    use saturn::bus::SCSP_REGS_BASE;
    let mut bus = fresh();
    assert_eq!(bus.read8(SCSP_RAM_BASE, AccessKind::Data).1, 48);
    assert_eq!(bus.read16(SCSP_RAM_BASE, AccessKind::Data).1, 48);
    assert_eq!(bus.read32(SCSP_RAM_BASE, AccessKind::Data).1, 48);
    assert_eq!(bus.read16(SCSP_REGS_BASE, AccessKind::Data).1, 48);
    // Writes (the exact B-bus deferred-write model, Mednafen scu.inc
    // BBusRW_DB): a store hands off to the B-bus in +2 CPU cycles and the
    // +17 (+13 for the second 16-bit half) device-side completion lands on
    // `bbus_write_finish`. A lone store returns 0 (write buffer); the next
    // store stalls only until the previous one's +2 handoff (2002 − 2001);
    // the +30 completion serializes the *next B-bus access* instead.
    bus.cycle = 2000;
    assert_eq!(bus.write16(SCSP_RAM_BASE, 0, AccessKind::Data), 0);
    bus.cycle = 2001;
    assert_eq!(bus.write32(SCSP_RAM_BASE, 0, AccessKind::Data), 1);
    // The 32-bit store handed off at 2021 and completes at 2021 + 17 + 13 =
    // 2051; a read right after waits that out, then pays its own two halves.
    bus.cycle = 2002;
    assert_eq!(
        bus.read16(SCSP_RAM_BASE, AccessKind::Data).1,
        (2051 - 2002) + 48
    );
}

#[test]
fn bbus_write_completion_does_not_block_cs3_traffic() {
    // The deferred B-bus write completion is B-bus-local: WRAM work between
    // two SCSP stores pays only its own cost (the old flat model charged the
    // whole bus for the +17, overcharging mixed-region sequences).
    let mut bus = fresh();
    bus.cycle = 3000;
    assert_eq!(bus.write16(SCSP_RAM_BASE, 0, AccessKind::Data), 0);
    // High-WRAM read one cycle later: waits the +2 handoff (to 3002), +1
    // turnaround (back-to-back region change), then its own SDRAM +7 — but
    // NOT the +17 SCSP completion.
    bus.cycle = 3001;
    assert_eq!(bus.read32(HIGH_WRAM_BASE, AccessKind::Data).1, 9);
    // The SCSP completion (3019) still gates the next B-bus access.
    bus.cycle = 3011;
    assert_eq!(
        bus.read16(SCSP_RAM_BASE, AccessKind::Data).1,
        (3019 - 3011) + 48
    );
}

// ---- M12 task #8: the per-access BSC bus-timing model ----------------------
// Costs and semantics are a faithful port of Mednafen `sh7095.inc`
// `BSC_BusRead/Write` + `ss.cpp` `BusRW_DB_CS0`; each test pins one mechanism.

#[test]
fn bsc_cs0_is_a_16bit_bus_so_32bit_accesses_pay_twice() {
    let mut bus = fresh();
    bus.cycle = 1000;
    assert_eq!(bus.read16(BIOS_BASE, AccessKind::Data).1, 8);
    bus.cycle = 1100;
    assert_eq!(bus.read32(BIOS_BASE, AccessKind::Data).1, 16);
    bus.cycle = 1200;
    assert_eq!(bus.read16(LOW_WRAM_BASE, AccessKind::Data).1, 7);
    bus.cycle = 1300;
    assert_eq!(bus.read32(LOW_WRAM_BASE, AccessKind::Data).1, 14);
    bus.cycle = 1400;
    assert_eq!(bus.read32(BACKUP_BASE, AccessKind::Data).1, 16);
}

#[test]
fn bsc_high_wram_sdram_read7_and_write_buffered_with_array_busy_window() {
    let mut bus = fresh();
    bus.cycle = 1000;
    assert_eq!(bus.read32(HIGH_WRAM_BASE, AccessKind::Data).1, 7);
    // A lone store is free to the CPU (the write buffer)...
    bus.cycle = 1100;
    assert_eq!(bus.write32(HIGH_WRAM_BASE, 0, AccessKind::Data), 0);
    // ...but the SDRAM array stays busy 2 cycles past the bus handoff: a
    // read one cycle later waits out the store (1 bus + 2 array) + its own 7.
    bus.cycle = 1101;
    assert_eq!(bus.read32(HIGH_WRAM_BASE, AccessKind::Data).1, 10);
}

#[test]
fn bsc_line_fill_burst_beats_are_free_on_sdram_full_price_on_cs0() {
    let mut bus = fresh();
    // CS3: the fill is one SDRAM burst — only the first beat pays.
    bus.cycle = 1000;
    assert_eq!(bus.read32(HIGH_WRAM_BASE, AccessKind::Data).1, 7);
    assert_eq!(bus.read32(HIGH_WRAM_BASE + 4, AccessKind::LineFill).1, 0);
    // CS0 (BIOS ROM): no burst mode — every beat pays both 16-bit halves.
    bus.cycle = 2000;
    assert_eq!(bus.read32(BIOS_BASE, AccessKind::Data).1, 16);
    assert_eq!(bus.read32(BIOS_BASE + 4, AccessKind::LineFill).1, 16);
}

#[test]
fn bsc_bus_turnaround_charges_one_cycle_on_back_to_back_region_change() {
    let mut bus = fresh();
    bus.cycle = 1000;
    assert_eq!(bus.read32(HIGH_WRAM_BASE, AccessKind::Data).1, 7); // CS3
    // A CS0 read right behind it (same instruction): +1 turnaround before
    // the BIOS transaction.
    assert_eq!(bus.read16(BIOS_BASE, AccessKind::Data).1, 9); // 1 + 8
}

#[test]
fn bsc_shared_timestamp_makes_a_cpu_wait_for_the_siblings_access() {
    let mut bus = fresh();
    // One CPU reads at cycle 1000 — the bus is busy until 1007.
    bus.cycle = 1000;
    assert_eq!(bus.read32(HIGH_WRAM_BASE, AccessKind::Data).1, 7);
    // The sibling issues its own read at 1002: it queues behind the first
    // access (5 cycles) and then pays its own 7 — the CPU↔CPU arbitration
    // that emerges from the shared timestamp.
    bus.cycle = 1002;
    assert_eq!(bus.read32(HIGH_WRAM_BASE + 8, AccessKind::Data).1, 12);
}

#[test]
fn bsc_dma_accesses_are_cost_only_and_dont_disturb_cpu_bus_state() {
    let mut bus = fresh();
    bus.cycle = 1000;
    // SH-2 DMAC / SCU-DMA timeline costs (Mednafen `dma_time_thing`): a
    // C-bus SDRAM read is 6 per 32-bit word, a C-bus write is free
    // (`DMA_Write` WriteBus==2 charges nothing), B-bus VDP1/VDP2 cost 1 per
    // 16-bit access and the SCSP 13.
    assert_eq!(bus.read32(HIGH_WRAM_BASE, AccessKind::Dma).1, 6);
    assert_eq!(bus.write32(HIGH_WRAM_BASE, 0, AccessKind::Dma), 0);
    assert_eq!(bus.read32(LOW_WRAM_BASE, AccessKind::Dma).1, 14);
    assert_eq!(bus.write16(0x05C0_0000, 0, AccessKind::Dma), 1); // VDP1
    assert_eq!(bus.write16(0x05E0_0000, 0, AccessKind::Dma), 1); // VDP2
    assert_eq!(bus.write16(SCSP_RAM_BASE, 0, AccessKind::Dma), 13); // SCSP
    assert_eq!(bus.read32(0x05C0_0000, AccessKind::Dma).1, 2); // two 16-bit halves
    // A CPU access right after pays only its own cost — the DMA charges
    // never touched the shared bus state.
    assert_eq!(bus.read32(HIGH_WRAM_BASE, AccessKind::Data).1, 7);
}
