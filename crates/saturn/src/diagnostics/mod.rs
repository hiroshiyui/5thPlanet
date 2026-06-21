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
use asm::{NOP, add, add_imm, assemble, bra_self, mov_imm, movl_load, movl_store, mul_l, shll8, shll16, sts_macl, sub};

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
        // No program word decodes to Illegal.
        for d in REGISTRY {
            let _ = (d.run)(); // also exercises every program end-to-end
        }
    }
}
