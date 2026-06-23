//! End-to-end verification that the cache is wired into `Cpu::mem_*`
//! (M2 task #2). Each test enables the cache via CCR, drives one or two
//! `MOV.L` instructions through the public `Cpu::step` interface, and
//! asserts the cache state changed as expected.
//!
//! A custom `CountingBus` tracks how many external 32-bit reads landed
//! on the bus; that's the observable that distinguishes a cache hit
//! (no bus traffic) from a miss (line-fill of 4 reads).

use sh2::Cpu;
use sh2::bus::{AccessKind, Bus};
use sh2::harness::MemBus;

const PC0: u32 = 0x0000_1000;

/// Wraps a MemBus and counts the externally observable read32 calls.
/// We only need read32 for the cache miss-fill path; the others delegate
/// straight through.
struct CountingBus {
    inner: MemBus,
    reads32: u32,
}

impl CountingBus {
    fn new(inner: MemBus) -> Self {
        Self { inner, reads32: 0 }
    }
}

impl Bus for CountingBus {
    fn read8(&mut self, addr: u32, k: AccessKind) -> (u8, u32) {
        self.inner.read8(addr, k)
    }
    fn read16(&mut self, addr: u32, k: AccessKind) -> (u16, u32) {
        self.inner.read16(addr, k)
    }
    fn read32(&mut self, addr: u32, k: AccessKind) -> (u32, u32) {
        self.reads32 += 1;
        self.inner.read32(addr, k)
    }
    fn write8(&mut self, addr: u32, v: u8, k: AccessKind) -> u32 {
        self.inner.write8(addr, v, k)
    }
    fn write16(&mut self, addr: u32, v: u16, k: AccessKind) -> u32 {
        self.inner.write16(addr, v, k)
    }
    fn write32(&mut self, addr: u32, v: u32, k: AccessKind) -> u32 {
        self.inner.write32(addr, v, k)
    }
}

fn make_cpu(prog: &[u16]) -> (Cpu, CountingBus) {
    let mut mem = MemBus::new(64 * 1024);
    mem.load_program(PC0, prog);
    let mut cpu = Cpu::new();
    cpu.regs.pc = PC0;
    cpu.regs.r[15] = 0x0000_8000;
    (cpu, CountingBus::new(mem))
}

#[test]
fn cache_disabled_does_not_install_lines() {
    // Two MOV.L @R1, R2 to the same address. With CCR=0, both reads
    // hit the bus directly.
    let mut cpu;
    let mut bus;
    (cpu, bus) = make_cpu(&[0x6212, 0x6212]);
    cpu.regs.r[1] = 0x4000;
    bus.inner.write_u32(0x4000, 0xDEAD_BEEF);

    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[2], 0xDEAD_BEEF);
    assert_eq!(bus.reads32, 2, "both reads went straight to bus");
}

#[test]
fn cache_enabled_second_data_read_is_a_hit() {
    // CCR=0x01 (CE). Track data-side reads by sampling around each step:
    // the first MOV.L data load misses (+4 line-fill reads); the second
    // hits (+0).
    let mut cpu;
    let mut bus;
    (cpu, bus) = make_cpu(&[0x6212, 0x6212]);
    cpu.regs.r[1] = 0x4000;
    bus.inner.write_u32(0x4000, 0xCAFE_F00D);
    cpu.cache.set_ccr(0x01);

    let before_first = bus.reads32;
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[2], 0xCAFE_F00D);
    let after_first = bus.reads32;
    // The step covers an instruction fetch (line-fill, 4 reads) AND
    // a data load (line-fill, 4 reads) — 8 total since both miss.
    assert_eq!(after_first - before_first, 8);

    let before_second = bus.reads32;
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.r[2], 0xCAFE_F00D);
    assert_eq!(
        bus.reads32, before_second,
        "second step's fetch and data both hit cache"
    );
}

#[test]
fn write_through_keeps_cached_line_coherent_with_bus() {
    // MOV.L R3, @R1 ; MOV.L @R1, R2
    let mut cpu;
    let mut bus;
    (cpu, bus) = make_cpu(&[0x6212, 0x2132, 0x6212]);
    cpu.regs.r[1] = 0x4000;
    cpu.regs.r[3] = 0xAAAA_BBBB;
    bus.inner.write_u32(0x4000, 0x1111_1111);
    cpu.cache.set_ccr(0x01);

    cpu.step(&mut bus); // MOV.L @R1,R2 — miss-fill, R2 = 0x11111111
    assert_eq!(cpu.regs.r[2], 0x1111_1111);
    let after_fill = bus.reads32;

    cpu.step(&mut bus); // MOV.L R3,@R1 — write-through to bus + line
    // Bus saw the write directly.
    let mem = bus.inner.as_slice();
    assert_eq!(&mem[0x4000..0x4004], &[0xAA, 0xAA, 0xBB, 0xBB]);

    cpu.step(&mut bus); // MOV.L @R1,R2 — hit, should see updated value
    assert_eq!(cpu.regs.r[2], 0xAAAA_BBBB, "cache reflects the write");
    assert_eq!(bus.reads32, after_fill, "no extra bus read on the hit");
}

#[test]
fn cache_through_alias_bypasses_cache_entirely() {
    // Two MOV.L through the 0x2xxxxxxx alias to 0x4000. Even with CCR
    // enabled, each access goes to bus (8 reads, since reads are 1-each
    // for cache-through 32-bit).
    let mut cpu;
    let mut bus;
    (cpu, bus) = make_cpu(&[0x6212, 0x6212]);
    cpu.regs.r[1] = 0x2000_4000; // cache-through alias of 0x4000
    bus.inner.write_u32(0x4000, 0xFEED_FACE);
    cpu.cache.set_ccr(0x01);

    // Sample the data-side traffic only by snapshotting around the loads.
    // The data load is a single 32-bit bus read because cache-through
    // bypasses the line-fill path; do that twice to prove it.
    let before = bus.reads32;
    cpu.step(&mut bus);
    let after_first = bus.reads32;
    // First step fetches a fresh instruction line (4 reads) + 1 data read.
    assert_eq!(after_first - before, 5);
    let mid = bus.reads32;
    cpu.step(&mut bus);
    // Second step: instruction fetch hits cache (+0), data read still
    // goes to bus (+1) because cache-through doesn't cache it.
    assert_eq!(bus.reads32 - mid, 1, "cache-through data read every time");
    assert_eq!(cpu.regs.r[2], 0xFEED_FACE);
}

#[test]
fn associative_purge_drops_a_stale_line_for_cross_master_coherency() {
    // MOV.L @R1,R2 ; MOV.L R3,@R4 ; MOV.L @R1,R2
    //   R1 = 0x4000        (cacheable data address)
    //   R4 = 0x4000_4000   (the region-2 associative-purge alias of 0x4000)
    // Models the real use: this CPU caches 0x4000, another bus master (the
    // other SH-2 / SCU DMA) overwrites it directly, and software issues an
    // associative purge so the next read is coherent again.
    let mut cpu;
    let mut bus;
    (cpu, bus) = make_cpu(&[0x6212, 0x2432, 0x6212]);
    cpu.regs.r[1] = 0x4000;
    cpu.regs.r[4] = 0x4000_4000; // associative-purge alias of 0x4000
    cpu.regs.r[3] = 0; // the purge ignores the written value
    bus.inner.write_u32(0x4000, 0x1111_1111);
    cpu.cache.set_ccr(0x01);

    cpu.step(&mut bus); // load → miss-fill, line now resident with 0x11111111
    assert_eq!(cpu.regs.r[2], 0x1111_1111);

    // Another bus master overwrites memory directly, bypassing this cache.
    bus.inner.write_u32(0x4000, 0x2222_2222);

    let before_purge = bus.reads32;
    cpu.step(&mut bus); // MOV.L R3,@R4 → associative purge of 0x4000's line
    assert_eq!(
        bus.reads32, before_purge,
        "associative purge does not touch the external bus"
    );

    cpu.step(&mut bus); // re-load → now a MISS → fetches the fresh value
    assert_eq!(
        cpu.regs.r[2], 0x2222_2222,
        "purge forced a coherent re-read of external memory"
    );
}

#[test]
fn instruction_fetch_also_uses_cache() {
    // A tight 4-NOP loop runs entirely from cache after the first
    // line-fill. NOPs at PC0..PC0+6 all live on the same 16-byte line
    // (PC0 = 0x1000, line base 0x1000, offsets 0/2/4/6).
    let mut cpu;
    let mut bus;
    (cpu, bus) = make_cpu(&[0x0009, 0x0009, 0x0009, 0x0009]);
    cpu.cache.set_ccr(0x01);

    cpu.step(&mut bus); // first NOP — line fill on fetch
    let after_fill = bus.reads32;
    assert!(after_fill >= 4, "line fill must touch bus");
    for _ in 0..3 {
        cpu.step(&mut bus);
    }
    assert_eq!(bus.reads32, after_fill, "subsequent fetches all hit cache");
}

#[test]
fn reset_purges_and_disables_the_cache_so_the_core_refetches() {
    // A reset must drop pre-reset cache lines AND disable the cache (SH7604
    // CCR.CE=0). Load-bearing for the Saturn slave, which is re-reset via SMPC
    // SSHON to relocate it (SAN5's FMV->menu transition): without the purge the
    // rebooted core fetch-hits its stale pre-reset lines and never picks up the
    // new code. Disabling alone is NOT enough — the lines return when the boot
    // re-enables the cache, so the reset must invalidate them.
    let mut cpu;
    let mut bus;
    (cpu, bus) = make_cpu(&[0x6212]); // MOV.L @R1,R2
    cpu.regs.r[1] = 0x4000;
    bus.inner.write_u32(0x4000, 0x1111_1111);
    cpu.cache.set_ccr(0x01);
    cpu.step(&mut bus); // caches the data line = 0x11111111
    assert_eq!(cpu.regs.r[2], 0x1111_1111);

    // Another bus master overwrites memory directly (bypassing this cache).
    bus.inner.write_u32(0x4000, 0x2222_2222);

    cpu.reset(&mut bus);
    assert!(!cpu.cache.enabled(), "reset clears CCR → cache disabled");

    // Re-enable and re-run the load: it must MISS the purged line and read the
    // fresh value, not the stale 0x11111111 that a disable-only reset leaves.
    cpu.regs.pc = PC0;
    cpu.regs.r[1] = 0x4000;
    cpu.cache.set_ccr(0x01);
    cpu.step(&mut bus);
    assert_eq!(
        cpu.regs.r[2], 0x2222_2222,
        "reset purged the stale line; the re-fetch reads current memory"
    );
}
