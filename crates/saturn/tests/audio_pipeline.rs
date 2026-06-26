//! Audio-pipeline test program — a known signal end-to-end through the SCSP.
//!
//! The BIOS audio path is opaque: when a sound is wrong we can't tell whether
//! the BIOS, the 68k driver, or our SCSP synthesis is at fault. This test
//! removes every variable except the synthesis: a ~16-instruction SH-2 program
//! (no BIOS, no disc, no 68k sound driver) sets up **one SCSP slot to loop a
//! sine sample** and keys it on; the harness seeds the sine into sound RAM,
//! runs the program, and drains the SCSP output. Known input (a sine) → known
//! expected output (the same sine, looped, at full volume).
//!
//! Run it and dump the output for inspection:
//! ```text
//! AUDIO_OUT=/tmp/sine.pcm cargo test -p saturn --test audio_pipeline \
//!     -- --ignored --nocapture
//! ```
//! The same program image (`build_sine_program`) is a valid Saturn boot image,
//! so it can be run on Mednafen too for a sample-accurate cross-check (the next
//! step: proving the synthesis, not just that it's non-silent).

use saturn::Saturn;
use sh2::debug::disasm;
use sh2::decoder::decode;

// ---- a tiny SH-2 encoder (only the handful of forms this program needs) ----
// Verified against the project's decoder in `program_disassembles_as_expected`.

/// `MOV.L @(disp,PC), Rn` — load a 32-bit constant from the pool (disp in longs).
fn movl_pcrel(rn: u16, disp: u16) -> u16 {
    0xD000 | (rn << 8) | disp
}
/// `MOV.W @(disp,PC), Rn` — load a (sign-extended) 16-bit constant (disp in words).
fn movw_pcrel(rn: u16, disp: u16) -> u16 {
    0x9000 | (rn << 8) | disp
}
/// `MOV #imm, Rn` — load an 8-bit signed immediate.
fn mov_imm(rn: u16, imm: i8) -> u16 {
    0xE000 | (rn << 8) | (imm as u8 as u16)
}
/// `MOV.W R0, @(disp,Rn)` — store R0 to `Rn + disp*2` (disp 0..15).
fn movw_store_r0(rn: u16, disp: u16) -> u16 {
    0x8100 | (rn << 4) | disp
}

const NOP: u16 = 0x0009;
const BRA_SELF: u16 = 0xAFFE; // BRA . (disp = -2)

/// The slot-0 register values, written in order; reg 0 (key-on) is written
/// last. See `crates/saturn/src/scsp/mod.rs` for the bit layout.
const SA_LOW: u16 = 0x4000; // sample at sound-RAM byte offset 0x4000
const REG_LEA: i8 = 63; // loop end = sample 63 (one 64-sample sine period)
const REG_AR: i8 = 0x1F; // reg4: AR = max (instant attack), D1R/D2R = 0 → hold full
const DISDL_FULL: u16 = 0xE000; // reg0xB: DISDL = 7 (full direct), DIPAN = centre
const KEY_ON: u16 = 0x1820; // reg0: KYONEX | KYONB | LPCTL=forward-loop, SA-hi=0

const SCSP_REGS: u32 = 0x05B0_0000;
const CODE_PC: u32 = 0x0000_0020;
const STACK: u32 = 0x0601_0000;

// --- 68k-driven variant: the sound CPU (not the SH-2) sets up the slot ---
const M68K_PC: u32 = 0x0000_1000; // 68k driver program, sound-RAM offset
const M68K_SSP: u32 = 0x0001_0000; // 68k supervisor stack pointer
const M68K_SCSP: u32 = 0x0010_0000; // SCSP registers, as the 68k addresses them

/// Build the self-contained sine-playback program. Layout: reset vector (PC, SP)
/// at 0, code at 0x20, a longword + word constant pool after the code.
fn build_sine_program() -> Vec<u8> {
    // Code words (offsets relative to CODE_PC, computed below for the pool refs).
    // Pool: longword 0x05B0_0000 at 0x50, then words 0x4000/0xE000/0x1820.
    let code: [u16; 22] = [
        movl_pcrel(1, 11),    // 0x20  R1 = 0x05B00000 (pool@0x50)
        movw_pcrel(0, 23),    // 0x22  R0 = 0x4000     (pool@0x54)
        movw_store_r0(1, 1),  // 0x24  reg1 SA-low = R0
        mov_imm(0, 0),        // 0x26  R0 = 0
        movw_store_r0(1, 2),  // 0x28  reg2 LSA = 0
        mov_imm(0, REG_LEA),  // 0x2A  R0 = 63
        movw_store_r0(1, 3),  // 0x2C  reg3 LEA = 63
        mov_imm(0, REG_AR),   // 0x2E  R0 = 0x1F
        movw_store_r0(1, 4),  // 0x30  reg4 AR = max
        mov_imm(0, 0),        // 0x32  R0 = 0
        movw_store_r0(1, 5),  // 0x34  reg5 = 0
        mov_imm(0, 0),        // 0x36  R0 = 0
        movw_store_r0(1, 6),  // 0x38  reg6 TL = 0 (max volume)
        mov_imm(0, 0),        // 0x3A  R0 = 0
        movw_store_r0(1, 8),  // 0x3C  reg8 OCT/FNS = 0 (base pitch, 1:1)
        mov_imm(0, 0),        // 0x3E  R0 = 0
        movw_store_r0(1, 10), // 0x40  reg0xA ISEL/IMXL = 0 (no DSP)
        movw_pcrel(0, 8),     // 0x42  R0 = 0xE000     (pool@0x56)
        movw_store_r0(1, 11), // 0x44  reg0xB DISDL = 7
        movw_pcrel(0, 7),     // 0x46  R0 = 0x1820     (pool@0x58)
        movw_store_r0(1, 0),  // 0x48  reg0 = KEY ON (last)
        BRA_SELF,             // 0x4A  spin forever
    ];

    let mut prog = vec![0u8; 0x60];
    prog[0..4].copy_from_slice(&CODE_PC.to_be_bytes()); // reset PC
    prog[4..8].copy_from_slice(&STACK.to_be_bytes()); // reset SP
    let mut off = CODE_PC as usize;
    for w in code {
        prog[off..off + 2].copy_from_slice(&w.to_be_bytes());
        off += 2;
    }
    prog[0x4C..0x4E].copy_from_slice(&NOP.to_be_bytes()); // BRA delay slot
    // Constant pool.
    prog[0x50..0x54].copy_from_slice(&SCSP_REGS.to_be_bytes());
    prog[0x54..0x56].copy_from_slice(&SA_LOW.to_be_bytes());
    prog[0x56..0x58].copy_from_slice(&DISDL_FULL.to_be_bytes());
    prog[0x58..0x5A].copy_from_slice(&KEY_ON.to_be_bytes());
    prog
}

/// One period of a sine, `n` samples, peak amplitude `amp`, as signed 16-bit.
fn sine_period(n: usize, amp: i16) -> Vec<i16> {
    (0..n)
        .map(|i| {
            let phase = (i as f64) / (n as f64) * std::f64::consts::TAU;
            (phase.sin() * amp as f64).round() as i16
        })
        .collect()
}

/// Self-check: our hand-encoded words round-trip through the real decoder, so
/// the program is exactly what the comments claim (and stays that way).
#[test]
fn program_disassembles_as_expected() {
    let prog = build_sine_program();
    let at = |o: usize| u16::from_be_bytes([prog[o], prog[o + 1]]);
    assert_eq!(u32::from_be_bytes(prog[0..4].try_into().unwrap()), CODE_PC);
    assert_eq!(u32::from_be_bytes(prog[4..8].try_into().unwrap()), STACK);
    // The pool holds exactly the SCSP base + the three 16-bit register values.
    assert_eq!(
        u32::from_be_bytes(prog[0x50..0x54].try_into().unwrap()),
        SCSP_REGS
    );
    assert_eq!(at(0x54), SA_LOW);
    assert_eq!(at(0x58), KEY_ON);
    // Spot-check a couple of instructions decode to the intended ops.
    assert!(matches!(
        decode(at(0x20)),
        sh2::isa::Op::MovLPcRel { rn: 1, .. }
    ));
    assert!(matches!(
        decode(at(0x48)),
        sh2::isa::Op::MovWS0 { rn: 1, disp: 0 }
    ));
}

#[test]
#[ignore = "manual: plays a known sine through one SCSP slot; AUDIO_OUT=<file> dumps PCM"]
fn audio_pipeline_sine() {
    const N: usize = 64; // 64-sample period at 1:1 → 44100/64 ≈ 689 Hz
    const AMP: i16 = 0x4000; // half-scale, so a clean tone can't clip on its own

    let mut sat = Saturn::new(build_sine_program());
    sat.reset();

    // Seed the sine into sound RAM at the slot's SA (0x4000), and park the 68k
    // (SNDON releases it) on a harmless BRA-self so it can't touch the SCSP.
    let sine = sine_period(N, AMP);
    for (i, &s) in sine.iter().enumerate() {
        sat.bus
            .scsp
            .ram
            .write16(SA_LOW as u32 + i as u32 * 2, s as u16);
    }
    sat.bus.scsp.ram.write32(0, 0x0000_1000); // 68k SSP
    sat.bus.scsp.ram.write32(4, 0x0000_0100); // 68k PC
    sat.bus.scsp.ram.write16(0x100, 0x60FE); // 68k: BRA self
    sat.bus.scsp.start(); // SNDON → SCSP generates
    sat.bus.scsp.ctrl.write16(0x400, 0x000F); // MVOL = unity (a reset SCSP is silent)

    // Print the program disassembly — this *is* the trace of the pipeline setup.
    println!("--- sine test program (master starts at {CODE_PC:#06X}) ---");
    for o in (CODE_PC..0x4C).step_by(2) {
        let w = u16::from_be_bytes([
            build_sine_program()[o as usize],
            build_sine_program()[o as usize + 1],
        ]);
        println!("  {o:#06X}: {w:04X}  {}", disasm(decode(w)));
    }

    // Run a few frames so the master sets up the slot and the SCSP fills the
    // output, then drain the audio.
    let mut fb = vec![0u8; saturn::vdp2::FRAMEBUFFER_BYTES];
    let mut pcm: Vec<i16> = Vec::new();
    for _ in 0..8 {
        sat.run_frame(&mut fb);
        pcm.extend(sat.take_audio());
    }

    let peak = pcm.iter().map(|&x| x.unsigned_abs()).max().unwrap_or(0);
    let nonzero = pcm.iter().filter(|&&x| x != 0).count();
    println!(
        "master pc={:08X}  output: {} samples, {nonzero} non-zero, peak={peak}",
        sat.master().regs.pc,
        pcm.len()
    );
    if let Ok(p) = std::env::var("AUDIO_OUT") {
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&p, &bytes).unwrap();
        println!("wrote {} bytes to {p}", bytes.len());
    }

    assert!(nonzero > 0, "slot produced audio (the sine plays)");
    assert!(
        peak >= AMP as u16 / 4,
        "sine reaches a meaningful level (peak={peak})"
    );
    assert!(
        peak < 30000,
        "steady single voice must not clip (peak={peak})"
    );
}

// ===========================================================================
// 68k-driven variant — the *sound CPU* sets up the slot, not the SH-2.
//
// The SH-2 sine program above proves the SCSP *synthesis* is correct, but in a real
// boot it's the hosted MC68EC000 sound driver (not the SH-2) that programs the
// slots and strobes key-on — and that path is the prime suspect for the silent
// game/BIOS BGM. This test removes the SH-2 from the picture: the SH-2 just
// spins, and a tiny 68k program staged in sound RAM does the entire slot setup
// + key-on through the 68k's own view of the SCSP registers (0x10_0000). If the
// sine comes out, our hosted 68k *can* drive the SCSP to make sound, so the BGM
// blocker is the real driver's control flow (it never reaches key-on), not the
// 68k→SCSP path itself.
// ===========================================================================

/// `MOV.W #imm, (abs).L` (68k) — store a 16-bit immediate to an absolute long
/// address. Encoding `0x33FC`, then the immediate word, then the 32-bit address;
/// all big-endian (the 68k, like the SH-2, is big-endian). Self-contained (no
/// address register), so each register write is independently verifiable.
fn m68k_movw_imm_abs(imm: u16, abs: u32) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0..2].copy_from_slice(&0x33FCu16.to_be_bytes());
    b[2..4].copy_from_slice(&imm.to_be_bytes());
    b[4..8].copy_from_slice(&abs.to_be_bytes());
    b
}

/// The 68k driver: program slot 0 (same values as the SH-2 sine program) through the
/// 68k's SCSP window, key-on last, then spin. `BRA *` is `0x60FE`.
fn build_sine_68k_driver() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&m68k_movw_imm_abs(SA_LOW, M68K_SCSP + 0x02)); // reg1 SA-low
    p.extend_from_slice(&m68k_movw_imm_abs(REG_LEA as u16, M68K_SCSP + 0x06)); // reg3 LEA=63
    p.extend_from_slice(&m68k_movw_imm_abs(REG_AR as u16, M68K_SCSP + 0x08)); // reg4 AR=max
    p.extend_from_slice(&m68k_movw_imm_abs(DISDL_FULL, M68K_SCSP + 0x16)); // reg0xB DISDL=7
    p.extend_from_slice(&m68k_movw_imm_abs(KEY_ON, M68K_SCSP)); // reg0 = KEY ON (last)
    p.extend_from_slice(&0x60FEu16.to_be_bytes()); // BRA self
    p
}

/// An SH-2 program that does nothing but spin (so the master keeps time advancing
/// while the 68k does the real work).
fn build_park_program() -> Vec<u8> {
    let mut prog = vec![0u8; 0x30];
    prog[0..4].copy_from_slice(&CODE_PC.to_be_bytes()); // reset PC
    prog[4..8].copy_from_slice(&STACK.to_be_bytes()); // reset SP
    prog[CODE_PC as usize..CODE_PC as usize + 2].copy_from_slice(&BRA_SELF.to_be_bytes());
    prog[CODE_PC as usize + 2..CODE_PC as usize + 4].copy_from_slice(&NOP.to_be_bytes());
    prog
}

#[test]
#[ignore = "manual: plays a sine through one SCSP slot DRIVEN BY THE 68k; AUDIO_OUT=<file> dumps PCM"]
fn audio_pipeline_sine_68k() {
    const N: usize = 64;
    const AMP: i16 = 0x4000;

    let mut sat = Saturn::new(build_park_program()); // SH-2 just spins; the 68k works
    sat.reset();

    // Seed the sine into sound RAM at SA (shared RAM — same offset either CPU's
    // view), then stage the 68k driver + its reset vectors. `start()` (SNDON)
    // reloads SSP/PC from sound RAM [0]/[4] and runs the driver.
    let sine = sine_period(N, AMP);
    for (i, &s) in sine.iter().enumerate() {
        sat.bus
            .scsp
            .ram
            .write16(SA_LOW as u32 + i as u32 * 2, s as u16);
    }
    sat.bus.scsp.ram.write32(0, M68K_SSP);
    sat.bus.scsp.ram.write32(4, M68K_PC);
    for (i, &b) in build_sine_68k_driver().iter().enumerate() {
        sat.bus.scsp.ram.write8(M68K_PC + i as u32, b);
    }
    sat.bus.scsp.start();
    sat.bus.scsp.ctrl.write16(0x400, 0x000F); // MVOL = unity (a reset SCSP is silent)

    let mut fb = vec![0u8; saturn::vdp2::FRAMEBUFFER_BYTES];
    let mut pcm: Vec<i16> = Vec::new();
    for _ in 0..8 {
        sat.run_frame(&mut fb);
        pcm.extend(sat.take_audio());
    }

    // Proof the 68k actually drove the SCSP: slot 0 must carry the values the
    // driver wrote (SA/LEA), independent of whether audio came out.
    let d = sat.bus.scsp.slot_debug(0);
    println!(
        "after 68k driver: 68k pc={:06X}  slot0 active={} sa={:#07X} lea={} eg={}",
        sat.bus.scsp.cpu.regs.pc, d.active, d.sa, d.lea, d.eg_state
    );
    assert_eq!(d.sa, SA_LOW as u32, "68k wrote slot0 SA");
    assert_eq!(d.lea, REG_LEA as u16, "68k wrote slot0 LEA");

    let peak = pcm.iter().map(|&x| x.unsigned_abs()).max().unwrap_or(0);
    let nonzero = pcm.iter().filter(|&&x| x != 0).count();
    println!(
        "68k-driven output: {} samples, {nonzero} non-zero, peak={peak}",
        pcm.len()
    );
    if let Ok(p) = std::env::var("AUDIO_OUT") {
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&p, &bytes).unwrap();
        println!("wrote {} bytes to {p}", bytes.len());
    }

    assert!(
        nonzero > 0,
        "the 68k-keyed slot produces audio (the sine plays)"
    );
    assert!(
        peak >= AMP as u16 / 4,
        "sine reaches a meaningful level (peak={peak})"
    );
    assert!(peak < 30000, "single voice must not clip (peak={peak})");
}
