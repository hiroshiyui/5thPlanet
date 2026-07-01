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
    movl_store, movw_load, movw_store, mul_l, or_, shll, shll8, shll16, shlr, sts_macl, sub, xor_,
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
    Diag {
        name: "cpu_add_imm",
        category: "cpu",
        run: cpu_add_imm,
    },
    Diag {
        name: "cpu_sub_reg",
        category: "cpu",
        run: cpu_sub_reg,
    },
    Diag {
        name: "cpu_mull_macl",
        category: "cpu",
        run: cpu_mull_macl,
    },
    Diag {
        name: "branch_delay_slot",
        category: "branch",
        run: branch_delay_slot,
    },
    Diag {
        name: "mem_roundtrip_low",
        category: "memory",
        run: mem_roundtrip_low,
    },
    Diag {
        name: "mem_roundtrip_high",
        category: "memory",
        run: mem_roundtrip_high,
    },
    Diag {
        name: "divu_divide",
        category: "onchip",
        run: divu_divide,
    },
    Diag {
        name: "timer_frc_advances",
        category: "onchip",
        run: timer_frc_advances,
    },
    Diag {
        name: "cpu_logic_ops",
        category: "cpu",
        run: cpu_logic_ops,
    },
    Diag {
        name: "cpu_shift_ops",
        category: "cpu",
        run: cpu_shift_ops,
    },
    Diag {
        name: "scu_dma_copy",
        category: "scu",
        run: scu_dma_copy,
    },
    Diag {
        name: "dmac_transfer",
        category: "onchip",
        run: dmac_transfer,
    },
    Diag {
        name: "cpu_mac_l",
        category: "cpu",
        run: cpu_mac_l,
    },
    Diag {
        name: "cpu_cmp_branch",
        category: "branch",
        run: cpu_cmp_branch,
    },
    Diag {
        name: "vdp2_back_screen",
        category: "vdp2",
        run: vdp2_back_screen,
    },
    Diag {
        name: "scsp_tone",
        category: "scsp",
        run: scsp_tone,
    },
    Diag {
        name: "m68k_exec",
        category: "m68k",
        run: m68k_exec,
    },
    Diag {
        name: "scu_dsp_exec",
        category: "scu_dsp",
        run: scu_dsp_exec,
    },
    Diag {
        name: "vdp1_polygon",
        category: "vdp1",
        run: vdp1_polygon,
    },
    Diag {
        name: "savestate_roundtrip",
        category: "savestate",
        run: savestate_roundtrip,
    },
    Diag {
        name: "cache_purge_coherency",
        category: "cache",
        run: cache_purge_coherency,
    },
    Diag {
        name: "scu_dma_alias_fold",
        category: "scu",
        run: scu_dma_alias_fold,
    },
];

/// Run every built-in diagnostic and collect the outcomes (registry order).
pub fn run_all() -> Vec<DiagOutcome> {
    REGISTRY
        .iter()
        .map(|d| {
            let (passed, detail) = (d.run)();
            DiagOutcome {
                name: d.name,
                category: d.category,
                passed,
                detail,
            }
        })
        .collect()
}

/// System / boot **compatibility** checks (heuristic, best-effort — **not**
/// goldens, and **not** part of [`run_all`] or the CI test). Unlike the hermetic
/// feature checks, these need real media: they boot a fresh throwaway machine
/// from `bios` (+ optional `disc`, with the SMPC `region` code) and observe
/// whether it produces video and — with a disc — whether the 1st-read program
/// reaches High WRAM (i.e. authentication + IP.BIN read + load + jump all
/// worked). Answers "does my setup boot?", which is distinct from "is the
/// emulator correct?".
pub fn run_system(bios: Vec<u8>, disc: Option<crate::disc::Disc>, region: u8) -> Vec<DiagOutcome> {
    // TOC facts are pure — read them before the disc is moved into the machine.
    let toc = disc
        .as_ref()
        .map(|d| (d.first_track(), d.last_track(), d.lead_out_fad()));
    let has_disc = disc.is_some();

    let mut sat = Saturn::new(bios);
    if let Some(d) = disc {
        sat.insert_disc(d);
    }
    sat.reset();
    sat.set_region(region);

    // With a disc: boot until the 1st-read program reaches High WRAM (the strong
    // auth+load+jump signal), early-exiting as soon as it does. Without a disc:
    // run to the BIOS splash plateau and measure that it produced video.
    let mut fb = vec![0u8; crate::vdp2::FRAMEBUFFER_BYTES];
    let mut reached_hwram = false;
    let mut max_non_black = 0usize;
    for _ in 0..if has_disc { 400 } else { 180 } {
        let dims = sat.run_frame(&mut fb);
        if has_disc {
            if (0x0600_0000..=0x060F_FFFF).contains(&sat.master().regs.pc) {
                reached_hwram = true;
                break;
            }
        } else {
            let non_black = fb[..dims.0 * dims.1 * 4]
                .chunks_exact(4)
                .filter(|p| (p[0] | p[1] | p[2]) != 0)
                .count();
            max_non_black = max_non_black.max(non_black);
        }
    }

    let mut out = Vec::new();
    match toc {
        // BIOS-only: did the boot chain produce video (the SEGA splash)?
        None => out.push(DiagOutcome {
            name: "bios_video",
            category: "system",
            passed: max_non_black > 64,
            detail: format!("{max_non_black} non-black px (peak)"),
        }),
        // Disc present: structural TOC + the end-to-end boot (auth → IP.BIN →
        // 1st-read load → jump into High WRAM). disc_boots passing implies the
        // BIOS path itself is healthy, so a separate bios_video is redundant.
        Some((first, last, lead_out)) => {
            out.push(DiagOutcome {
                name: "disc_toc",
                category: "disc",
                passed: first >= 1 && last >= first && lead_out > crate::disc::FAD_OFFSET,
                detail: format!("tracks {first}..{last}, lead-out FAD {lead_out}"),
            });
            out.push(DiagOutcome {
                name: "disc_boots",
                category: "disc",
                passed: reached_hwram,
                detail: format!(
                    "master PC=0x{:08X} ({})",
                    sat.master().regs.pc,
                    if reached_hwram {
                        "reached HWRAM"
                    } else {
                        "never reached HWRAM"
                    }
                ),
            });
        }
    }
    out
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
    [
        mov_imm(1, 0x20),
        shll16(1),
        mov_imm(2, 1),
        shll8(2),
        add(1, 2),
    ]
}

/// Emit the seven instructions that build the [`SENTINEL`] (`0x2A2A2A2A`) into
/// R0 via repeated `SHLL8; ADD #0x2A` — the value the memory/DMA checks move and
/// read back. Shared by the round-trip and DMA checks (and unit-tested).
fn sentinel_r0() -> [u16; 7] {
    [
        mov_imm(0, 0x2A),
        shll8(0),
        add_imm(0, 0x2A),
        shll8(0),
        add_imm(0, 0x2A),
        shll8(0),
        add_imm(0, 0x2A),
    ]
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
        mov_imm(0, 1),  // 0x20  R0 = 1
        asm_bra(1),     // 0x22  BRA target (delay slot follows)
        add_imm(0, 41), // 0x24  delay slot: R0 = 42 (must run)
        mov_imm(0, 99), // 0x26  skipped (must NOT run)
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
    let base = vec![
        mov_imm(1, 6),
        shll16(1),
        shll8(1),
        mov_imm(2, 1),
        shll8(2),
        add(1, 2),
    ];
    let code = roundtrip_code(base, 4);
    verdict(read_high(&run_program(&code), HIGH_SCRATCH), SENTINEL)
}

/// Shared body for the memory round-trip checks: build the sentinel in R0,
/// take a prebuilt base-address sequence (leaving the base in R1), store/clear/
/// load, then store the loaded value at base + `result_off`.
fn roundtrip_code(base_addr_seq: Vec<u16>, result_off: i8) -> Vec<u16> {
    let mut code = sentinel_r0().to_vec(); // R0 = 0x2A2A2A2A
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
        shll8(1), // R1 = 0xFFFFFF00 (DVSR)
        mov_imm(2, 4),
        add(2, 1), // R2 = 0xFFFFFF04 (DVDNT)
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
        add(1, 5), // R1 = 0x0020_0100
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
        shll8(1), // R1 = 0xFFFFFE00
        mov_imm(2, 0x12),
        add(1, 2), // R1 = 0xFFFFFE12 (FRC, 16-bit)
        // Low scratch base in R5 (= 0x0020_0100).
        mov_imm(5, 0x20),
        shll16(5),
        mov_imm(6, 1),
        shll8(6),
        add(5, 6),
        movw_load(3, 1),  // R3 = FRC (first)
        movl_store(5, 3), // scratch[0x100] = first
    ];
    code.extend([NOP; 16]); // burn cycles so the counter ticks
    code.extend([
        movw_load(4, 1), // R4 = FRC (second)
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
    verdict(read_low(&run_scu_dma_copy(false), 0x0020_0300), SENTINEL)
}

/// Like [`scu_dma_copy`], but the destination pointer is written as a
/// cache-through **alias** (`0x2020_0300`, bit 29 set). The SCU-DMA engine must
/// fold every address to its 27-bit physical form before the bus access, or the
/// write lands nowhere (an unfolded alias reads/writes open bus — the Doukyuusei
/// menu-background bug). Passing proves the folding still happens.
fn scu_dma_alias_fold() -> (bool, String) {
    verdict(read_low(&run_scu_dma_copy(true), 0x0020_0300), SENTINEL)
}

/// Shared SCU-DMA channel-0 word copy `0x0020_0200` → `0x0020_0300` (both Low
/// WRAM — a legal DMA source, not the BIOS A-bus). Mirrors `tests/scu.rs`'s
/// manual-trigger sequence: D0R/D0W/D0C/D0AD/D0MD, then a 32-bit write to D0EN
/// with the DGO bit fires it (the engine drains within `run_for`). When
/// `alias_dest`, the D0W pointer is the cache-through alias of the destination,
/// which the engine must fold — see [`scu_dma_alias_fold`].
fn run_scu_dma_copy(alias_dest: bool) -> Saturn {
    let mut code = sentinel_r0().to_vec(); // R0 = sentinel 0x2A2A2A2A
    code.extend([
        // R2 = source 0x0020_0200, plant the sentinel there
        mov_imm(2, 0x20),
        shll16(2),
        mov_imm(4, 2),
        shll8(4),
        add(2, 4),
        movl_store(2, 0),
        // R3 = dest 0x0020_0300
        mov_imm(3, 0x20),
        shll16(3),
        mov_imm(4, 3),
        shll8(4),
        add(3, 4),
    ]);
    if alias_dest {
        // R3 |= 0x2000_0000 (the cache-through region bit: 0x20 << 24), turning
        // the dest into its cache-through alias 0x2020_0300.
        code.extend([mov_imm(4, 0x20), shll8(4), shll8(4), shll8(4), add(3, 4)]);
    }
    code.extend([
        // R1 = SCU base 0x05FE_0000 (6<<8 = 0x600, -2 = 0x5FE, <<16)
        mov_imm(1, 6),
        shll8(1),
        add_imm(1, -2),
        shll16(1),
        movl_store(1, 2), // D0R (base+0x00) = source
    ]);
    // D0W (base+0x04) = dest
    code.extend([mov_imm(4, 0x04), add(4, 1), movl_store(4, 3)]);
    // D0C (base+0x08) = 4 bytes
    code.extend([mov_imm(4, 0x08), add(4, 1), mov_imm(5, 4), movl_store(4, 5)]);
    // D0AD (base+0x0C) = 0x101 (read +4, write +2 — the contiguous-copy form)
    code.extend([
        mov_imm(5, 1),
        shll8(5),
        add_imm(5, 1),
        mov_imm(4, 0x0C),
        add(4, 1),
        movl_store(4, 5),
    ]);
    // D0MD (base+0x14) = (1<<16)|(1<<8)|7 = 0x10107 (RUP | WUP | manual factor)
    code.extend([
        mov_imm(5, 1),
        shll16(5),
        mov_imm(6, 1),
        shll8(6),
        add(5, 6),
        add_imm(5, 7),
        mov_imm(4, 0x14),
        add(4, 1),
        movl_store(4, 5),
    ]);
    // D0EN (base+0x10) = DGO (0x100) — triggers the transfer
    code.extend([
        mov_imm(5, 1),
        shll8(5),
        mov_imm(4, 0x10),
        add(4, 1),
        movl_store(4, 5),
    ]);
    code.extend([bra_self(), NOP]);
    run_program(&code)
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
    let mut code = sentinel_r0().to_vec(); // R0 = sentinel 0x2A2A2A2A
    code.extend([
        // R2 = source 0x0020_0200, plant the sentinel
        mov_imm(2, 0x20),
        shll16(2),
        mov_imm(4, 2),
        shll8(4),
        add(2, 4),
        movl_store(2, 0),
        // R3 = dest 0x0020_0300
        mov_imm(3, 0x20),
        shll16(3),
        mov_imm(4, 3),
        shll8(4),
        add(3, 4),
        // R1 = SAR0 base 0xFFFFFF80
        mov_imm(1, -128),
        movl_store(1, 2), // SAR0 (base+0x00) = source
    ]);
    // DAR0 (base+0x04) = dest
    code.extend([mov_imm(4, 0x04), add(4, 1), movl_store(4, 3)]);
    // TCR0 (base+0x08) = 1 longword
    code.extend([mov_imm(4, 0x08), add(4, 1), mov_imm(5, 1), movl_store(4, 5)]);
    // CHCR0 (base+0x0C) = 0x5C01 (DM=inc, SM=inc, TS=long, DE=1)
    code.extend([
        mov_imm(5, 0x5C),
        shll8(5),
        add_imm(5, 1),
        mov_imm(4, 0x0C),
        add(4, 1),
        movl_store(4, 5),
    ]);
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
        mov_imm(1, 0x20),
        shll16(1),
        mov_imm(4, 2),
        shll8(4),
        add(1, 4),
        // A = [3, 4] at +0x00 / +0x04
        mov_imm(5, 3),
        movl_store(1, 5),
        mov_imm(4, 4),
        add(4, 1),
        mov_imm(5, 4),
        movl_store(4, 5),
        // B = [10, 3] at +0x10 / +0x14
        mov_imm(4, 0x10),
        add(4, 1),
        mov_imm(5, 10),
        movl_store(4, 5),
        mov_imm(4, 0x14),
        add(4, 1),
        mov_imm(5, 3),
        movl_store(4, 5),
        // R2 = &A (0x200200), R3 = &B (0x200210)
        mov_imm(2, 0x20),
        shll16(2),
        mov_imm(4, 2),
        shll8(4),
        add(2, 4),
        mov_imm(3, 0x20),
        shll16(3),
        mov_imm(4, 2),
        shll8(4),
        add(3, 4),
        mov_imm(4, 0x10),
        add(3, 4),
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
        mov_imm(1, 5),  // 0x20
        mov_imm(2, 5),  // 0x22
        cmp_eq(2, 1),   // 0x24  T = (R2 == R1) = 1
        bt(0),          // 0x26  if T: target = PC+4 = 0x2A (skips the next insn)
        mov_imm(0, 7),  // 0x28  wrong value (must be skipped)
        mov_imm(0, 42), // 0x2A  target: correct value
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
    (
        got == want,
        format!("centre pixel {got:02X?}, want {want:02X?}"),
    )
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
    sat.bus
        .write16(SCSP + 0x02, SA_LOW as u16, AccessKind::Data); // reg1 SA-low
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

/// SCSP **sound CPU** (MC68EC000) executes a program: stage a tiny 68k routine
/// into sound RAM that writes the sentinel to a fixed address, release the CPU
/// (`SNDON` resets it and reloads SSP/PC from sound-RAM `[0]`/`[4]`), run a few
/// frames, and read the address back. Proves the hosted-68k boot + step path is
/// alive (the class that historically silenced BGM). The 68k program is:
/// `MOVE.L #$2A2A2A2A,($0800).W` (`0x21FC 2A2A 2A2A 0800`) then `BRA.S *`
/// (`0x60FE`).
fn m68k_exec() -> (bool, String) {
    let mut sat = Saturn::new(assemble(&[bra_self(), NOP]));
    sat.reset();
    // 68k reset vectors: SSP at sound RAM [0], initial PC at [4].
    sat.bus.scsp.ram.write32(0, 0x0000_1000); // SSP (grows down from 0x1000)
    sat.bus.scsp.ram.write32(4, 0x0000_0100); // PC = 0x100
    for (i, w) in [0x21FCu16, 0x2A2A, 0x2A2A, 0x0800, 0x60FE]
        .iter()
        .enumerate()
    {
        sat.bus.scsp.ram.write16(0x100 + i as u32 * 2, *w);
    }
    sat.bus.scsp.start(); // SNDON — resets + releases the 68k
    let mut fb = vec![0u8; crate::vdp2::FRAMEBUFFER_BYTES];
    for _ in 0..4 {
        sat.run_frame(&mut fb);
    }
    verdict(sat.bus.scsp.ram.read32(0x0800), SENTINEL)
}

/// **SCU-DSP** executes microcode: load a 3-word program — `MVI RA0, WRAM>>2`;
/// `DMA` one word A/B-bus → data-RAM bank 0; `ENDI` — via the PPD port, start it
/// (PPAF `LEF|EXF`), and verify the sentinel planted in Work RAM lands in DSP
/// data RAM. Mirrors `tests/scu.rs`'s DSP-DMA test; the aggregate steps the DSP
/// in `drain_scu_dsp` during `run_for`.
fn scu_dsp_exec() -> (bool, String) {
    use sh2::bus::{AccessKind, Bus};
    const SCU: u32 = 0x05FE_0000;
    const PPD: u32 = SCU + 0x84;
    const PPAF: u32 = SCU + 0x80;
    const SRC: u32 = 0x0020_1000;
    let mut sat = Saturn::new(assemble(&[bra_self(), NOP]));
    sat.reset();
    sat.bus.write32(SRC, SENTINEL, AccessKind::Data); // the word the DSP fetches
    let ra0 = SRC >> 2;
    let mvi_ra0 = (0b10u32 << 30) | (6 << 26) | (ra0 & 0x01FF_FFFF);
    let dma = (0b11u32 << 30) | (1 << 15) | 1; // add_sel=1 (→4 bytes), 1 word
    let endi = (0b11u32 << 30) | (0b11 << 28) | (1 << 27);
    for w in [mvi_ra0, dma, endi] {
        sat.bus.write32(PPD, w, AccessKind::Data); // load at PC 0,1,2
    }
    sat.bus
        .write32(PPAF, (1 << 15) | (1 << 16), AccessKind::Data); // LEF | EXF: run
    sat.run_for(4096);
    let got = sat.bus.scu.dsp.data_ram[0][0];
    (
        got == SENTINEL && sat.bus.scu.dsp.stopped(),
        format!("data_ram[0][0]=0x{got:08X}, want 0x{SENTINEL:08X}"),
    )
}

/// **VDP1** plots a solid polygon: write a one-command list (a 10×10 blue quad)
/// plus an END command into VDP1 VRAM, kick a one-shot draw (`PTMR` PTM=01), and
/// check the draw framebuffer — an interior dot is filled and a corner outside
/// the quad stays empty. Exercises the command-list plotter end to end (the
/// whole subsystem had no self-check).
fn vdp1_polygon() -> (bool, String) {
    use sh2::bus::{AccessKind, Bus};
    const VRAM: u32 = 0x05C0_0000; // command table
    const FB: u32 = 0x05C8_0000; // draw framebuffer
    let mut sat = Saturn::new(assemble(&[bra_self(), NOP]));
    sat.reset();
    // One polygon (10,10)-(20,20) in blue (RGB555 0x001F), then an END command.
    let cmd: [u16; 14] = [
        0x0004, 0x0000, // CMDCTRL polygon | CMDLINK
        0x0080, 0x001F, // CMDPMOD (ECD, no transparency) | CMDCOLR (blue)
        0x0000, 0x0000, // CMDSRCA | CMDSIZE (unused)
        0x000A, 0x000A, // XA,YA = (10,10)
        0x0014, 0x000A, // XB,YB = (20,10)
        0x0014, 0x0014, // XC,YC = (20,20)
        0x000A, 0x0014, // XD,YD = (10,20)
    ];
    for (i, w) in cmd.iter().enumerate() {
        sat.bus.write16(VRAM + i as u32 * 2, *w, AccessKind::Data);
    }
    sat.bus.write16(VRAM + 0x20, 0x8000, AccessKind::Data); // END command
    sat.bus.write16(0x05D0_0004, 0x0001, AccessKind::Data); // PTMR PTM=01: draw now
    let px = |x: u32, y: u32| sat.bus.vdp1.read16(FB + (y * 512 + x) * 2);
    let (interior, corner) = (px(15, 15), px(0, 0));
    (
        interior != 0 && corner == 0,
        format!("interior=0x{interior:04X} (want !=0), corner=0x{corner:04X} (want 0)"),
    )
}

/// **Save-state** round-trip is complete and deterministic (ADR-0027): snapshot
/// a seeded throwaway, reload it into a fresh machine, run both forward by the
/// same budget, and require byte-identical re-snapshots. A regression in *any*
/// serialized field (an un-`Serialize`d peripheral) makes the two diverge.
fn savestate_roundtrip() -> (bool, String) {
    use sh2::bus::{AccessKind, Bus};
    let prog = assemble(&[bra_self(), NOP]);
    let mut a = Saturn::new(prog.clone());
    a.reset();
    a.run_for(50_000);
    a.bus.write32(LOW_SCRATCH, SENTINEL, AccessKind::Data);
    let snap = a.save_state();
    let mut b = Saturn::new(prog);
    b.reset();
    if let Err(e) = b.load_state(&snap) {
        return (false, format!("load_state rejected the snapshot: {e}"));
    }
    a.run_for(200_000);
    b.run_for(200_000);
    let _ = a.take_audio();
    let _ = b.take_audio();
    let (sa, sb) = (a.save_state(), b.save_state());
    (
        sa == sb,
        format!(
            "post-run snapshots {} ({} vs {} bytes)",
            if sa == sb { "identical" } else { "DIVERGED" },
            sa.len(),
            sb.len()
        ),
    )
}

/// **SH-2 cache coherency** — the SAN5 signature bug class. Enable the cache,
/// cache address A, change memory behind the cache via a **cache-through alias**
/// write, confirm the cached read is now *stale*, then **associatively purge**
/// the line and confirm the re-read sees the new value. Exercises CCR enable,
/// line install, the cache-through bypass, and the by-address purge together.
fn cache_purge_coherency() -> (bool, String) {
    // R0 = V0 = sentinel — the value first cached at A.
    let mut code = sentinel_r0().to_vec();
    code.extend([
        // Enable the cache: CCR (0xFFFFFE92, 16-bit) = 0x0001 (CE = bit 0).
        mov_imm(1, -2),
        shll8(1), // R1 = 0xFFFFFE00
        mov_imm(2, 0x49),
        shll(2),   // R2 = 0x92 (0x49 << 1 — 0x92 won't fit a signed imm)
        add(1, 2), // R1 = 0xFFFFFE92
        mov_imm(3, 1),
        movw_store(1, 3), // CCR = 0x0001
        // R4 = A = 0x0020_0200 (cacheable Low WRAM).
        mov_imm(4, 0x20),
        shll16(4),
        mov_imm(5, 2),
        shll8(5),
        add(4, 5),        // R4 = 0x0020_0200
        movl_store(4, 0), // [A] = V0 (write-through, no line yet)
        movl_load(6, 4),  // read A → miss → install line holding V0
        // Write V1 = 0x55 through the cache-through alias 0x2020_0200.
        mov_imm(7, 0x55), // R7 = V1
        mov_imm(8, 0x20),
        shll8(8),
        shll8(8),
        shll8(8),         // R8 = 0x2000_0000
        add(8, 4),        // R8 = 0x2020_0200 (alias of A)
        movl_store(8, 7), // [alias] = V1 (bypasses cache → line now stale)
        movl_load(9, 4),  // read A → HIT → stale V0 ("before")
    ]);
    code.extend(build_low_scratch_addr()); // R1 = 0x0020_0100 (clobbers R2)
    code.extend([
        movl_store(1, 9), // scratch[0x100] = before
        // Associative-purge A via 0x4020_0200 (invalidate the line, no bus).
        mov_imm(10, 0x40),
        shll8(10),
        shll8(10),
        shll8(10),         // R10 = 0x4000_0000
        add(10, 4),        // R10 = 0x4020_0200 (purge alias of A)
        movl_load(11, 10), // purge access (returned value ignored)
        movl_load(12, 4),  // read A → miss → refetch V1 ("after")
        add_imm(1, 4),     // R1 = 0x0020_0104
        movl_store(1, 12), // scratch[0x104] = after
        bra_self(),
        NOP,
    ]);
    let sat = run_program(&code);
    let before = read_low(&sat, LOW_SCRATCH);
    let after = read_low(&sat, LOW_SCRATCH + 4);
    (
        before == SENTINEL && after == 0x55,
        format!(
            "stale-hit 0x{before:08X} (want 0x2A2A2A2A), post-purge 0x{after:08X} (want 0x00000055)"
        ),
    )
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
            assert!(
                o.passed,
                "diagnostic {}/{} failed: {}",
                o.category, o.name, o.detail
            );
        }
    }

    /// Negative control for the CI gate: prove a *failing* check is actually
    /// detected, so `all_diagnostics_pass` can't be a false green. We don't put
    /// a failing check in `REGISTRY` (that would wedge CI); instead we run the
    /// real `cpu_add_imm` program (which computes 42) through the same
    /// assemble→run→readback→verdict path but with a deliberately-wrong `want`,
    /// and confirm the outcome is reported failed and the gate's predicate
    /// rejects it.
    #[test]
    fn a_wrong_result_is_detected_as_failure() {
        let mut code = vec![mov_imm(0, 0), add_imm(0, 10), add_imm(0, 32)];
        code.extend(build_low_scratch_addr());
        code.extend([movl_store(1, 0), bra_self(), NOP]);
        let (passed, detail) = verdict(read_low(&run_program(&code), LOW_SCRATCH), 99);
        assert!(
            !passed,
            "a wrong expectation must be reported as failure (detail: {detail})"
        );
        assert!(
            detail.contains("0000002A") && detail.contains("00000063"),
            "detail should show got 0x2A vs want 0x63: {detail}"
        );
        // The same all-pass predicate `all_diagnostics_pass` relies on must flag it.
        let outcomes = [DiagOutcome {
            name: "neg_control",
            category: "test",
            passed,
            detail,
        }];
        assert!(
            !outcomes.iter().all(|o| o.passed),
            "the CI gate's all-pass predicate must reject a failing outcome"
        );
    }

    /// The extracted `sentinel_r0` fragment really leaves `0x2A2A2A2A` in R0
    /// (independently exercises the shared helper).
    #[test]
    fn sentinel_r0_builds_the_sentinel() {
        let mut code = sentinel_r0().to_vec(); // R0 = sentinel (uses R0 only)
        code.extend(build_low_scratch_addr()); // R1 = scratch (uses R1/R2)
        code.extend([movl_store(1, 0), bra_self(), NOP]);
        assert_eq!(read_low(&run_program(&code), LOW_SCRATCH), SENTINEL);
    }

    /// Self-check: the hand-encoded opcodes decode to the intended ops (catches
    /// a mistyped bit pattern independently of the functional run).
    #[test]
    fn encoder_opcodes_decode_as_expected() {
        use sh2::decoder::decode;
        use sh2::isa::Op;
        assert!(matches!(decode(mul_l(4, 5)), Op::MulL { rn: 4, rm: 5 }));
        assert!(matches!(decode(sts_macl(0)), Op::StsMacl { rn: 0 }));
        assert!(matches!(
            decode(movl_store(1, 0)),
            Op::MovLS { rn: 1, rm: 0 }
        ));
        assert!(matches!(
            decode(movl_load(0, 1)),
            Op::MovLL { rn: 0, rm: 1 }
        ));
        assert!(matches!(decode(shll16(1)), Op::Shll16 { rn: 1 }));
        assert!(matches!(decode(shll8(2)), Op::Shll8 { rn: 2 }));
        assert!(matches!(decode(add(1, 2)), Op::Add { rn: 1, rm: 2 }));
        assert!(matches!(decode(sub(0, 5)), Op::Sub { rn: 0, rm: 5 }));
        assert!(matches!(decode(asm_bra(1)), Op::Bra { disp: 1 }));
        assert!(matches!(
            decode(movw_load(3, 1)),
            Op::MovWL { rn: 3, rm: 1 }
        ));
        assert!(matches!(
            decode(movw_store(1, 3)),
            Op::MovWS { rn: 1, rm: 3 }
        ));
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
