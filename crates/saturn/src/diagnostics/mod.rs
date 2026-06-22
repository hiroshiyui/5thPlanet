//! Built-in self-diagnostics: a battery of tiny hand-assembled SH-2 programs,
//! each run from reset on a throwaway [`Saturn`] (no BIOS, no disc, no external
//! toolchain — the pattern proven by `tests/audio_pipeline.rs`). Each program
//! exercises one behavior and writes a result to Work RAM; the check reads it
//! back and reports pass/fail.
//!
//! Surfaced via the `jupiter doctor` CLI subcommand, the OSD "Diagnostics…"
//! screen, and the `all_diagnostics_pass` test (a CI accuracy regression that
//! needs none of the gitignored commercial BIOS/disc media).
//!
//! [`run_all`] builds and runs its own short-lived `Saturn` instances; it never
//! touches a live machine, so it's safe to call from the running frontend.

mod asm;

use crate::system::Saturn;
use asm::{
    CLRMAC, NOP, add, add_imm, and_, assemble, bra_self, bt, cmp_eq, mac_l, mov_imm, movl_load,
    movl_store, movw_load, mul_l, or_, shll, shll8, shll16, shlr, sts_macl, sub, xor_,
};

/// The result of one diagnostic check.
#[derive(Clone, Debug)]
pub struct DiagOutcome {
    pub name: &'static str,
    pub category: &'static str,
    pub passed: bool,
    /// Human-readable detail, e.g. `got 0x0000002A, want 0x0000002A`.
    pub detail: String,
}

struct Diag {
    name: &'static str,
    category: &'static str,
    run: fn() -> (bool, String),
}

const REGISTRY: &[Diag] = &[
    Diag { name: "cpu_add_imm", category: "cpu", run: cpu_add_imm },
    Diag { name: "cpu_sub_reg", category: "cpu", run: cpu_sub_reg },
    Diag { name: "cpu_mull_macl", category: "cpu", run: cpu_mull_macl },
    Diag { name: "branch_delay_slot", category: "branch", run: branch_delay_slot },
    Diag { name: "mem_roundtrip_low", category: "memory", run: mem_roundtrip_low },
    Diag { name: "mem_roundtrip_high", category: "memory", run: mem_roundtrip_high },
    Diag { name: "divu_divide", category: "onchip", run: divu_divide },
    Diag { name: "timer_frc_advances", category: "onchip", run: timer_frc_advances },
    Diag { name: "cpu_logic_ops", category: "cpu", run: cpu_logic_ops },
    Diag { name: "cpu_shift_ops", category: "cpu", run: cpu_shift_ops },
    Diag { name: "scu_dma_copy", category: "scu", run: scu_dma_copy },
    Diag { name: "dmac_transfer", category: "onchip", run: dmac_transfer },
    Diag { name: "cpu_mac_l", category: "cpu", run: cpu_mac_l },
    Diag { name: "cpu_cmp_branch", category: "branch", run: cpu_cmp_branch },
    Diag { name: "vdp2_back_screen", category: "vdp2", run: vdp2_back_screen },
    Diag { name: "scsp_tone", category: "scsp", run: scsp_tone },
];

/// Run every built-in diagnostic and collect the outcomes (registry order).
pub fn run_all() -> Vec<DiagOutcome> {
    REGISTRY
        .iter()
        .map(|d| {
            let (passed, detail) = (d.run)();
            DiagOutcome { name: d.name, category: d.category, passed, detail }
        })
        .collect()
}

/// Cycle budget for one check — every program parks in `BRA .` after a few
/// dozen instructions, so over-running is free; this is ~100× margin.
const RUN_CYCLES: u64 = 5_000;

/// Low/High Work RAM scratch the programs write their results to (clear of the
/// tiny BIOS-mapped program at `0x0` and the SP at `0x0601_0000`).
const LOW_SCRATCH: u32 = 0x0020_0100;
const HIGH_SCRATCH: u32 = 0x0600_0104;
const SENTINEL: u32 = 0x2A2A_2A2A;

fn run_program(code: &[u16]) -> Saturn {
    let mut sat = Saturn::new(assemble(code));
    sat.reset();
    sat.run_for(RUN_CYCLES);
    sat
}

fn read_low(sat: &Saturn, addr: u32) -> u32 {
    sat.bus.low_wram.read32(addr - crate::bus::LOW_WRAM_BASE)
}
fn read_high(sat: &Saturn, addr: u32) -> u32 {
    sat.bus.high_wram.read32(addr - crate::bus::HIGH_WRAM_BASE)
}

fn verdict(got: u32, want: u32) -> (bool, String) {
    (got == want, alloc_detail(got, want))
}
fn alloc_detail(got: u32, want: u32) -> String {
    format!("got 0x{got:08X}, want 0x{want:08X}")
}
/// Pass iff `second > first` (a monotonic-advance check, e.g. a free-running
/// counter). Detail shows both samples.
fn verdict_advanced(first: u32, second: u32) -> (bool, String) {
    (second > first, format!("advanced {first} -> {second}"))
}

/// Emit the five instructions that build `0x0020_0100` into R1 (clobbers R2).
fn build_low_scratch_addr() -> [u16; 5] {
    [mov_imm(1, 0x20), shll16(1), mov_imm(2, 1), shll8(2), add(1, 2)]
}

// --- the checks ----------------------------------------------------------

/// `ADD #imm` accumulates: 0 + 10 + 32 = 42.
fn cpu_add_imm() -> (bool, String) {
    let mut code = vec![mov_imm(0, 0), add_imm(0, 10), add_imm(0, 32)];
    code.extend(build_low_scratch_addr());
    code.extend([movl_store(1, 0), bra_self(), NOP]);
    verdict(read_low(&run_program(&code), LOW_SCRATCH), 42)
}

/// Register `ADD`/`SUB`: 0 + 100 − 58 = 42.
fn cpu_sub_reg() -> (bool, String) {
    let mut code = vec![
        mov_imm(4, 100),
        mov_imm(5, 58),
        mov_imm(0, 0),
        add(0, 4),
        sub(0, 5),
    ];
    code.extend(build_low_scratch_addr());
    code.extend([movl_store(1, 0), bra_self(), NOP]);
    verdict(read_low(&run_program(&code), LOW_SCRATCH), 42)
}

/// `MUL.L` → MACL → `STS`: 6 × 7 = 42.
fn cpu_mull_macl() -> (bool, String) {
    let mut code = vec![mov_imm(4, 6), mov_imm(5, 7), mul_l(4, 5), sts_macl(0)];
    code.extend(build_low_scratch_addr());
    code.extend([movl_store(1, 0), bra_self(), NOP]);
    verdict(read_low(&run_program(&code), LOW_SCRATCH), 42)
}

/// A taken `BRA` whose delay slot must execute and whose skipped instruction
/// must not: R0 = 1, slot `ADD #41` → 42, the skipped `MOV #99` never runs.
/// 42 distinguishes all three outcomes (slot-skipped = 1, branch-not-taken = 99).
fn branch_delay_slot() -> (bool, String) {
    // bra disp=1 skips exactly the one instruction after the delay slot:
    // target = (bra_pc + 4) + disp*2, with bra@0x22 → target 0x28 (the addr build).
    let mut code = vec![
        mov_imm(0, 1),      // 0x20  R0 = 1
        asm_bra(1),         // 0x22  BRA target (delay slot follows)
        add_imm(0, 41),     // 0x24  delay slot: R0 = 42 (must run)
        mov_imm(0, 99),     // 0x26  skipped (must NOT run)
    ];
    code.extend(build_low_scratch_addr()); // 0x28.. target
    code.extend([movl_store(1, 0), bra_self(), NOP]);
    verdict(read_low(&run_program(&code), LOW_SCRATCH), 42)
}

/// Store/load round-trip through Low WRAM: write the sentinel, clear, read it
/// back, store the read-back value to a second scratch slot.
fn mem_roundtrip_low() -> (bool, String) {
    let code = roundtrip_code(build_low_scratch_addr().to_vec(), 4);
    // result written at base+4 = 0x0020_0104
    verdict(read_low(&run_program(&code), LOW_SCRATCH + 4), SENTINEL)
}

/// Same round-trip through High WRAM (`0x0600_0100`).
fn mem_roundtrip_high() -> (bool, String) {
    // build 0x0600_0100 into R1 (clobbers R2): 6<<24 | 0x100.
    let base = vec![mov_imm(1, 6), shll16(1), shll8(1), mov_imm(2, 1), shll8(2), add(1, 2)];
    let code = roundtrip_code(base, 4);
    verdict(read_high(&run_program(&code), HIGH_SCRATCH), SENTINEL)
}

/// Shared body for the memory round-trip checks: build the sentinel in R0,
/// take a prebuilt base-address sequence (leaving the base in R1), store/clear/
/// load, then store the loaded value at base + `result_off`.
fn roundtrip_code(base_addr_seq: Vec<u16>, result_off: i8) -> Vec<u16> {
    // R0 = 0x2A2A2A2A via repeated (SHLL8; ADD #0x2A).
    let mut code = vec![
        mov_imm(0, 0x2A),
        shll8(0),
        add_imm(0, 0x2A),
        shll8(0),
        add_imm(0, 0x2A),
        shll8(0),
        add_imm(0, 0x2A),
    ];
    code.extend(base_addr_seq); // R1 = base
    code.extend([
        movl_store(1, 0), // mem[base] = sentinel
        mov_imm(0, 0),    // clear R0
        movl_load(0, 1),  // R0 = mem[base]
        mov_imm(3, result_off),
        add(3, 1),        // R3 = base + result_off
        movl_store(3, 0), // mem[base + off] = R0
        bra_self(),
        NOP,
    ]);
    code
}

/// SH-2 on-chip **DIVU**: 126 ÷ 3 = 42. Builds the DVSR address `0xFFFFFF00`
/// (`MOV #-1; SHLL8`), writes divisor then dividend (which triggers the 32÷32
/// divide), and reads the quotient back from DVDNT — the read auto-stalls until
/// the divider retires (~39 cycles), so no manual wait is needed.
fn divu_divide() -> (bool, String) {
    let code = [
        mov_imm(1, -1),
        shll8(1),         // R1 = 0xFFFFFF00 (DVSR)
        mov_imm(2, 4),
        add(2, 1),        // R2 = 0xFFFFFF04 (DVDNT)
        mov_imm(3, 3),
        movl_store(1, 3), // DVSR = 3
        mov_imm(4, 126),
        movl_store(2, 4), // DVDNT = 126 → trigger divide
        movl_load(0, 2),  // R0 = quotient (stalls until ready)
        // store the quotient to Low scratch
        mov_imm(1, 0x20),
        shll16(1),
        mov_imm(5, 1),
        shll8(5),
        add(1, 5),        // R1 = 0x0020_0100
        movl_store(1, 0),
        bra_self(),
        NOP,
    ];
    verdict(read_low(&run_program(&code), LOW_SCRATCH), 42)
}

/// SH-2 on-chip **FRT**: the free-running counter advances. Builds the FRC
/// address `0xFFFFFE12` (`MOV #-2; SHLL8` → `0xFFFFFE00`, + `0x12`), reads FRC,
/// burns cycles, reads it again, and verifies the second sample is larger. The
/// lazy FRT materializes on each register read, so the delta reflects the
/// cycles between them (≥ the φ/8 8-cycle tick at reset).
fn timer_frc_advances() -> (bool, String) {
    let mut code = vec![
        mov_imm(1, -2),
        shll8(1),    // R1 = 0xFFFFFE00
        mov_imm(2, 0x12),
        add(1, 2),   // R1 = 0xFFFFFE12 (FRC, 16-bit)
        // Low scratch base in R5 (= 0x0020_0100).
        mov_imm(5, 0x20),
        shll16(5),
        mov_imm(6, 1),
        shll8(6),
        add(5, 6),
        movw_load(3, 1), // R3 = FRC (first)
        movl_store(5, 3), // scratch[0x100] = first
    ];
    code.extend([NOP; 16]); // burn cycles so the counter ticks
    code.extend([
        movw_load(4, 1),  // R4 = FRC (second)
        mov_imm(6, 4),
        add(6, 5),        // R6 = 0x0020_0104
        movl_store(6, 4), // scratch[0x104] = second
        bra_self(),
        NOP,
    ]);
    let sat = run_program(&code);
    verdict_advanced(read_low(&sat, LOW_SCRATCH), read_low(&sat, LOW_SCRATCH + 4))
}

/// ALU logic: `0x0F OR 0x30 → 0x3F`, `XOR 0x15 → 0x2A`, `AND 0x2F → 0x2A`.
fn cpu_logic_ops() -> (bool, String) {
    let mut code = vec![
        mov_imm(0, 0x0F),
        mov_imm(2, 0x30),
        or_(0, 2), // 0x3F
        mov_imm(2, 0x15),
        xor_(0, 2), // 0x2A
        mov_imm(2, 0x2F),
        and_(0, 2), // 0x2A
    ];
    code.extend(build_low_scratch_addr());
    code.extend([movl_store(1, 0), bra_self(), NOP]);
    verdict(read_low(&run_program(&code), LOW_SCRATCH), 0x2A)
}

/// ALU shifts: `0x2A SHLR → 0x15 SHLL → 0x2A` (round-trips both shift ops).
fn cpu_shift_ops() -> (bool, String) {
    let mut code = vec![mov_imm(0, 0x2A), shlr(0), shll(0)];
    code.extend(build_low_scratch_addr());
    code.extend([movl_store(1, 0), bra_self(), NOP]);
    verdict(read_low(&run_program(&code), LOW_SCRATCH), 0x2A)
}

/// SCU **DMA** (the Saturn system peripheral, not an SH-2 on-chip block):
/// program channel 0 to copy the sentinel one word from one Low-WRAM address to
/// another and verify it moved. Mirrors `tests/scu.rs`'s proven manual-trigger
/// sequence: D0R/D0W/D0C/D0AD/D0MD, then a 32-bit write to D0EN with the DGO bit
/// fires it (the engine drains within `run_for`). Source `0x0020_0200` →
/// dest `0x0020_0300` (both Low WRAM — a legal DMA source, not the BIOS A-bus).
fn scu_dma_copy() -> (bool, String) {
    const DEST: u32 = 0x0020_0300;
    let mut code = vec![
        // R0 = sentinel 0x2A2A2A2A
        mov_imm(0, 0x2A), shll8(0), add_imm(0, 0x2A), shll8(0), add_imm(0, 0x2A), shll8(0), add_imm(0, 0x2A),
        // R2 = source 0x0020_0200, plant the sentinel there
        mov_imm(2, 0x20), shll16(2), mov_imm(4, 2), shll8(4), add(2, 4),
        movl_store(2, 0),
        // R3 = dest 0x0020_0300
        mov_imm(3, 0x20), shll16(3), mov_imm(4, 3), shll8(4), add(3, 4),
        // R1 = SCU base 0x05FE_0000 (6<<8 = 0x600, -2 = 0x5FE, <<16)
        mov_imm(1, 6), shll8(1), add_imm(1, -2), shll16(1),
        movl_store(1, 2), // D0R (base+0x00) = source
    ];
    // D0W (base+0x04) = dest
    code.extend([mov_imm(4, 0x04), add(4, 1), movl_store(4, 3)]);
    // D0C (base+0x08) = 4 bytes
    code.extend([mov_imm(4, 0x08), add(4, 1), mov_imm(5, 4), movl_store(4, 5)]);
    // D0AD (base+0x0C) = 0x101 (read +4, write +2 — the contiguous-copy form)
    code.extend([mov_imm(5, 1), shll8(5), add_imm(5, 1), mov_imm(4, 0x0C), add(4, 1), movl_store(4, 5)]);
    // D0MD (base+0x14) = (1<<16)|(1<<8)|7 = 0x10107 (RUP | WUP | manual factor)
    code.extend([
        mov_imm(5, 1), shll16(5),
        mov_imm(6, 1), shll8(6), add(5, 6),
        add_imm(5, 7),
        mov_imm(4, 0x14), add(4, 1), movl_store(4, 5),
    ]);
    // D0EN (base+0x10) = DGO (0x100) — triggers the transfer
    code.extend([mov_imm(5, 1), shll8(5), mov_imm(4, 0x10), add(4, 1), movl_store(4, 5)]);
    code.extend([bra_self(), NOP]);
    verdict(read_low(&run_program(&code), DEST), SENTINEL)
}

/// SH-2 on-chip **DMAC** (the CPU's own 2-channel DMA controller, distinct from
/// the SCU engine): program channel 0 for an auto-request longword copy and
/// verify it moved the sentinel. The transfer runs in `Cpu::step` once DE
/// (CHCR0) and DME (DMAOR) are both set, reading/writing the external bus.
/// SAR0 base `0xFFFFFF80` builds cleanly as `mov_imm(-128)` (sign-extended).
/// Mirrors `crates/sh2/tests/dmac.rs` (CHCR0 `0x5C01` = DM/SM increment, TS
/// longword, DE set). Source `0x0020_0200` → dest `0x0020_0300`.
fn dmac_transfer() -> (bool, String) {
    const DEST: u32 = 0x0020_0300;
    let mut code = vec![
        // R0 = sentinel 0x2A2A2A2A
        mov_imm(0, 0x2A), shll8(0), add_imm(0, 0x2A), shll8(0), add_imm(0, 0x2A), shll8(0), add_imm(0, 0x2A),
        // R2 = source 0x0020_0200, plant the sentinel
        mov_imm(2, 0x20), shll16(2), mov_imm(4, 2), shll8(4), add(2, 4),
        movl_store(2, 0),
        // R3 = dest 0x0020_0300
        mov_imm(3, 0x20), shll16(3), mov_imm(4, 3), shll8(4), add(3, 4),
        // R1 = SAR0 base 0xFFFFFF80
        mov_imm(1, -128),
        movl_store(1, 2), // SAR0 (base+0x00) = source
    ];
    // DAR0 (base+0x04) = dest
    code.extend([mov_imm(4, 0x04), add(4, 1), movl_store(4, 3)]);
    // TCR0 (base+0x08) = 1 longword
    code.extend([mov_imm(4, 0x08), add(4, 1), mov_imm(5, 1), movl_store(4, 5)]);
    // CHCR0 (base+0x0C) = 0x5C01 (DM=inc, SM=inc, TS=long, DE=1)
    code.extend([mov_imm(5, 0x5C), shll8(5), add_imm(5, 1), mov_imm(4, 0x0C), add(4, 1), movl_store(4, 5)]);
    // DMAOR (base+0x30) = 1 (DME) — arms the master enable, runs the transfer
    code.extend([mov_imm(5, 1), mov_imm(4, 0x30), add(4, 1), movl_store(4, 5)]);
    code.extend([bra_self(), NOP]);
    verdict(read_low(&run_program(&code), DEST), SENTINEL)
}

/// `MAC.L` multiply-accumulate over two 2-element longword arrays (exercises the
/// MAC unit, the accumulator, and `@Rn+` post-increment addressing):
/// `[3,4]·[10,3] = 3*10 + 4*3 = 42`. Plants both arrays in Low WRAM, CLRMACs,
/// runs two `MAC.L @R2+,@R3+`, then `STS MACL`.
fn cpu_mac_l() -> (bool, String) {
    let mut code = vec![
        // R1 = 0x0020_0200 (array base)
        mov_imm(1, 0x20), shll16(1), mov_imm(4, 2), shll8(4), add(1, 4),
        // A = [3, 4] at +0x00 / +0x04
        mov_imm(5, 3), movl_store(1, 5),
        mov_imm(4, 4), add(4, 1), mov_imm(5, 4), movl_store(4, 5),
        // B = [10, 3] at +0x10 / +0x14
        mov_imm(4, 0x10), add(4, 1), mov_imm(5, 10), movl_store(4, 5),
        mov_imm(4, 0x14), add(4, 1), mov_imm(5, 3), movl_store(4, 5),
        // R2 = &A (0x200200), R3 = &B (0x200210)
        mov_imm(2, 0x20), shll16(2), mov_imm(4, 2), shll8(4), add(2, 4),
        mov_imm(3, 0x20), shll16(3), mov_imm(4, 2), shll8(4), add(3, 4), mov_imm(4, 0x10), add(3, 4),
        CLRMAC,
        mac_l(2, 3), // 3*10 = 30
        mac_l(2, 3), // + 4*3 = 42
        sts_macl(0),
    ];
    code.extend(build_low_scratch_addr());
    code.extend([movl_store(1, 0), bra_self(), NOP]);
    verdict(read_low(&run_program(&code), LOW_SCRATCH), 42)
}

/// `CMP/EQ` + conditional `BT`: equal operands set T, the taken branch skips the
/// "wrong" store. R0 = 42 only if both CMP/EQ and BT behave (a broken either
/// way leaves the wrong value).
fn cpu_cmp_branch() -> (bool, String) {
    let mut code = vec![
        mov_imm(1, 5),   // 0x20
        mov_imm(2, 5),   // 0x22
        cmp_eq(2, 1),    // 0x24  T = (R2 == R1) = 1
        bt(0),           // 0x26  if T: target = PC+4 = 0x2A (skips the next insn)
        mov_imm(0, 7),   // 0x28  wrong value (must be skipped)
        mov_imm(0, 42),  // 0x2A  target: correct value
    ];
    code.extend(build_low_scratch_addr());
    code.extend([movl_store(1, 0), bra_self(), NOP]);
    verdict(read_low(&run_program(&code), LOW_SCRATCH), 42)
}

// --- frame-based chip checks ---------------------------------------------
//
// VDP2/SCSP need VRAM/sound-RAM data and a rendered frame / drained audio,
// which is awkward to hand-assemble — so unlike the SH-2-program checks above
// these drive the chip directly through the bus (a chip-in-isolation test) on a
// machine whose CPUs are parked (master spins on `BRA .`, slave halted by
// reset), then run a frame and inspect the output. Setups mirror the known-good
// `tests/vdp2_render.rs` and `tests/audio_pipeline.rs`.

/// VDP2 renders a solid back-screen colour: DISP on, no NBG/RBG layers, a single
/// RGB555 green in the back-screen table → the whole frame is green. Verifies
/// the register decode + compositor + the run_frame → framebuffer chain.
fn vdp2_back_screen() -> (bool, String) {
    use sh2::bus::{AccessKind, Bus};
    let mut sat = Saturn::new(assemble(&[bra_self(), NOP]));
    sat.reset();
    sat.bus.write16(0x05F8_0000, 0x8000, AccessKind::Data); // TVMD: DISP on
    sat.bus.write16(0x05F8_0020, 0x0000, AccessKind::Data); // BGON: all layers off
    sat.bus.vdp2.vram.write16(0x200, 0x03E0); // RGB555 green at back-screen word 0x100
    sat.bus.write16(0x05F8_00AC, 0x0000, AccessKind::Data); // BKTAU: hi=0, single-colour
    sat.bus.write16(0x05F8_00AE, 0x0100, AccessKind::Data); // BKTAL: word 0x100
    let mut fb = vec![0u8; crate::vdp2::FRAMEBUFFER_BYTES];
    let (w, h) = sat.run_frame(&mut fb);
    let px = ((h / 2) * w + w / 2) * 4;
    let got = [fb[px], fb[px + 1], fb[px + 2], fb[px + 3]];
    let want = [0x00, 0xFF, 0x00, 0xFF];
    (got == want, format!("centre pixel {got:02X?}, want {want:02X?}"))
}

/// SCSP synthesis: program slot 0 to loop a full-scale 64-sample sine at full
/// volume, key it on, run a few frames, and verify the drained audio reaches a
/// meaningful peak (mirrors `tests/audio_pipeline.rs`). The 68k is parked in
/// sound RAM so it can't disturb the slot after SNDON.
fn scsp_tone() -> (bool, String) {
    use sh2::bus::{AccessKind, Bus};
    const SCSP: u32 = 0x05B0_0000;
    const SA_LOW: u32 = 0x4000;
    const AMP: i16 = 0x4000;
    let mut sat = Saturn::new(assemble(&[bra_self(), NOP]));
    sat.reset();
    // Seed one sine period at the slot's sample start.
    for i in 0..64u32 {
        let phase = i as f64 / 64.0 * std::f64::consts::TAU;
        let s = (phase.sin() * AMP as f64).round() as i16;
        sat.bus.scsp.ram.write16(SA_LOW + i * 2, s as u16);
    }
    // Park the hosted 68k (SSP/PC at sound RAM [0]/[4], BRA-self at the PC) so
    // SNDON doesn't run garbage over the SCSP registers, then release it.
    sat.bus.scsp.ram.write32(0, 0x0000_1000);
    sat.bus.scsp.ram.write32(4, 0x0000_0100);
    sat.bus.scsp.ram.write16(0x100, 0x60FE);
    sat.bus.scsp.start(); // SNDON (resets the 68k) — slot setup + key-on follow
    // Master volume to unity: a fresh-reset SCSP has MVOL=0 and is silent (the
    // BIOS/driver normally programs this — see scsp `master_volume`).
    sat.bus.write16(SCSP + 0x400, 0x000F, AccessKind::Data); // MVOL = 0xF
    // Slot 0: SA / loop window / instant attack / full direct level, key on last
    // (the KYONEX strobe in reg0 is processed while the SCSP is running).
    sat.bus.write16(SCSP + 0x02, SA_LOW as u16, AccessKind::Data); // reg1 SA-low
    sat.bus.write16(SCSP + 0x04, 0, AccessKind::Data); // reg2 LSA
    sat.bus.write16(SCSP + 0x06, 63, AccessKind::Data); // reg3 LEA (loop end)
    sat.bus.write16(SCSP + 0x08, 0x001F, AccessKind::Data); // reg4 AR = max
    sat.bus.write16(SCSP + 0x0C, 0, AccessKind::Data); // reg6 TL = 0 (max volume)
    sat.bus.write16(SCSP + 0x10, 0, AccessKind::Data); // reg8 OCT/FNS = 0 (1:1 pitch)
    sat.bus.write16(SCSP + 0x14, 0, AccessKind::Data); // reg0xA ISEL/IMXL = 0
    sat.bus.write16(SCSP + 0x16, 0xE000, AccessKind::Data); // reg0xB DISDL = 7 (full)
    sat.bus.write16(SCSP, 0x1820, AccessKind::Data); // reg0 KEY ON (last)
    let mut fb = vec![0u8; crate::vdp2::FRAMEBUFFER_BYTES];
    let mut peak = 0u16;
    for _ in 0..8 {
        sat.run_frame(&mut fb);
        for s in sat.take_audio() {
            peak = peak.max(s.unsigned_abs());
        }
    }
    let want = (AMP as u16) / 4; // 0x1000 — the audio_pipeline acceptance bar
    (peak >= want, format!("peak {peak}, want >= {want}"))
}

/// `BRA disp` (`0xA000 | disp12`). Kept here (not in `asm`) because only the
/// delay-slot check needs a non-self branch displacement.
fn asm_bra(disp: i16) -> u16 {
    0xA000 | (disp as u16 & 0x0FFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CI accuracy regression: every built-in check passes on our core.
    #[test]
    fn all_diagnostics_pass() {
        for o in run_all() {
            assert!(o.passed, "diagnostic {}/{} failed: {}", o.category, o.name, o.detail);
        }
    }

    /// Self-check: the hand-encoded opcodes decode to the intended ops (catches
    /// a mistyped bit pattern independently of the functional run).
    #[test]
    fn encoder_opcodes_decode_as_expected() {
        use sh2::decoder::decode;
        use sh2::isa::Op;
        assert!(matches!(decode(mul_l(4, 5)), Op::MulL { rn: 4, rm: 5 }));
        assert!(matches!(decode(sts_macl(0)), Op::StsMacl { rn: 0 }));
        assert!(matches!(decode(movl_store(1, 0)), Op::MovLS { rn: 1, rm: 0 }));
        assert!(matches!(decode(movl_load(0, 1)), Op::MovLL { rn: 0, rm: 1 }));
        assert!(matches!(decode(shll16(1)), Op::Shll16 { rn: 1 }));
        assert!(matches!(decode(shll8(2)), Op::Shll8 { rn: 2 }));
        assert!(matches!(decode(add(1, 2)), Op::Add { rn: 1, rm: 2 }));
        assert!(matches!(decode(sub(0, 5)), Op::Sub { rn: 0, rm: 5 }));
        assert!(matches!(decode(asm_bra(1)), Op::Bra { disp: 1 }));
        assert!(matches!(decode(movw_load(3, 1)), Op::MovWL { rn: 3, rm: 1 }));
        assert!(matches!(decode(and_(0, 2)), Op::And { rn: 0, rm: 2 }));
        assert!(matches!(decode(or_(0, 2)), Op::Or { rn: 0, rm: 2 }));
        assert!(matches!(decode(xor_(0, 2)), Op::Xor { rn: 0, rm: 2 }));
        assert!(matches!(decode(shll(0)), Op::Shll { rn: 0 }));
        assert!(matches!(decode(shlr(0)), Op::Shlr { rn: 0 }));
        assert!(matches!(decode(mac_l(2, 3)), Op::MacL { rn: 2, rm: 3 }));
        assert!(matches!(decode(cmp_eq(2, 1)), Op::CmpEq { rn: 2, rm: 1 }));
        assert!(matches!(decode(bt(0)), Op::Bt { disp: 0 }));
        assert!(matches!(decode(CLRMAC), Op::Clrmac));
        // No program word decodes to Illegal.
        for d in REGISTRY {
            let _ = (d.run)(); // also exercises every program end-to-end
        }
    }
}
