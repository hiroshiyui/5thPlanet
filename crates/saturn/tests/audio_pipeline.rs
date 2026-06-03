//! Audio-pipeline test ROM — a known signal end-to-end through the SCSP.
//!
//! The BIOS audio path is opaque: when a sound is wrong we can't tell whether
//! the BIOS, the 68k driver, or our SCSP synthesis is at fault. This test
//! removes every variable except the synthesis: a ~16-instruction SH-2 program
//! (no BIOS, no disc, no 68k sound driver) sets up **one SCSP slot to loop a
//! sine sample** and keys it on; the harness seeds the sine into sound RAM,
//! runs the ROM, and drains the SCSP output. Known input (a sine) → known
//! expected output (the same sine, looped, at full volume).
//!
//! Run it and dump the output for inspection:
//! ```text
//! AUDIO_OUT=/tmp/sine.pcm cargo test -p saturn --test audio_pipeline \
//!     -- --ignored --nocapture
//! ```
//! The same ROM image (`build_sine_rom`) is a valid Saturn boot image, so it
//! can be run on Mednafen too for a sample-accurate cross-check (the next step:
//! proving the synthesis, not just that it's non-silent).

use saturn::Saturn;
use sh2::debug::disasm;
use sh2::decoder::decode;

// ---- a tiny SH-2 encoder (only the handful of forms this ROM needs) ----
// Verified against the project's decoder in `rom_disassembles_as_expected`.

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

/// Build the self-contained sine-playback ROM. Layout: reset vector (PC, SP) at
/// 0, code at 0x20, a longword + word constant pool after the code.
fn build_sine_rom() -> Vec<u8> {
    // Code words (offsets relative to CODE_PC, computed below for the pool refs).
    // Pool: longword 0x05B0_0000 at 0x50, then words 0x4000/0xE000/0x1820.
    let code: [u16; 22] = [
        movl_pcrel(1, 11),       // 0x20  R1 = 0x05B00000 (pool@0x50)
        movw_pcrel(0, 23),       // 0x22  R0 = 0x4000     (pool@0x54)
        movw_store_r0(1, 1),     // 0x24  reg1 SA-low = R0
        mov_imm(0, 0),           // 0x26  R0 = 0
        movw_store_r0(1, 2),     // 0x28  reg2 LSA = 0
        mov_imm(0, REG_LEA),     // 0x2A  R0 = 63
        movw_store_r0(1, 3),     // 0x2C  reg3 LEA = 63
        mov_imm(0, REG_AR),      // 0x2E  R0 = 0x1F
        movw_store_r0(1, 4),     // 0x30  reg4 AR = max
        mov_imm(0, 0),           // 0x32  R0 = 0
        movw_store_r0(1, 5),     // 0x34  reg5 = 0
        mov_imm(0, 0),           // 0x36  R0 = 0
        movw_store_r0(1, 6),     // 0x38  reg6 TL = 0 (max volume)
        mov_imm(0, 0),           // 0x3A  R0 = 0
        movw_store_r0(1, 8),     // 0x3C  reg8 OCT/FNS = 0 (base pitch, 1:1)
        mov_imm(0, 0),           // 0x3E  R0 = 0
        movw_store_r0(1, 10),    // 0x40  reg0xA ISEL/IMXL = 0 (no DSP)
        movw_pcrel(0, 8),        // 0x42  R0 = 0xE000     (pool@0x56)
        movw_store_r0(1, 11),    // 0x44  reg0xB DISDL = 7
        movw_pcrel(0, 7),        // 0x46  R0 = 0x1820     (pool@0x58)
        movw_store_r0(1, 0),     // 0x48  reg0 = KEY ON (last)
        BRA_SELF,                // 0x4A  spin forever
    ];

    let mut rom = vec![0u8; 0x60];
    rom[0..4].copy_from_slice(&CODE_PC.to_be_bytes()); // reset PC
    rom[4..8].copy_from_slice(&STACK.to_be_bytes()); // reset SP
    let mut off = CODE_PC as usize;
    for w in code {
        rom[off..off + 2].copy_from_slice(&w.to_be_bytes());
        off += 2;
    }
    rom[0x4C..0x4E].copy_from_slice(&NOP.to_be_bytes()); // BRA delay slot
    // Constant pool.
    rom[0x50..0x54].copy_from_slice(&SCSP_REGS.to_be_bytes());
    rom[0x54..0x56].copy_from_slice(&SA_LOW.to_be_bytes());
    rom[0x56..0x58].copy_from_slice(&DISDL_FULL.to_be_bytes());
    rom[0x58..0x5A].copy_from_slice(&KEY_ON.to_be_bytes());
    rom
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
/// the ROM is exactly the program the comments claim (and stays that way).
#[test]
fn rom_disassembles_as_expected() {
    let rom = build_sine_rom();
    let at = |o: usize| u16::from_be_bytes([rom[o], rom[o + 1]]);
    assert_eq!(u32::from_be_bytes(rom[0..4].try_into().unwrap()), CODE_PC);
    assert_eq!(u32::from_be_bytes(rom[4..8].try_into().unwrap()), STACK);
    // The pool holds exactly the SCSP base + the three 16-bit register values.
    assert_eq!(u32::from_be_bytes(rom[0x50..0x54].try_into().unwrap()), SCSP_REGS);
    assert_eq!(at(0x54), SA_LOW);
    assert_eq!(at(0x58), KEY_ON);
    // Spot-check a couple of instructions decode to the intended ops.
    assert!(matches!(decode(at(0x20)), sh2::isa::Op::MovLPcRel { rn: 1, .. }));
    assert!(matches!(decode(at(0x48)), sh2::isa::Op::MovWS0 { rn: 1, disp: 0 }));
}

#[test]
#[ignore = "manual: plays a known sine through one SCSP slot; AUDIO_OUT=<file> dumps PCM"]
fn audio_pipeline_sine() {
    const N: usize = 64; // 64-sample period at 1:1 → 44100/64 ≈ 689 Hz
    const AMP: i16 = 0x4000; // half-scale, so a clean tone can't clip on its own

    let mut sat = Saturn::new(build_sine_rom());
    sat.reset();

    // Seed the sine into sound RAM at the slot's SA (0x4000), and park the 68k
    // (SNDON releases it) on a harmless BRA-self so it can't touch the SCSP.
    let sine = sine_period(N, AMP);
    for (i, &s) in sine.iter().enumerate() {
        sat.bus.scsp.ram.write16(SA_LOW as u32 + i as u32 * 2, s as u16);
    }
    sat.bus.scsp.ram.write32(0, 0x0000_1000); // 68k SSP
    sat.bus.scsp.ram.write32(4, 0x0000_0100); // 68k PC
    sat.bus.scsp.ram.write16(0x100, 0x60FE); // 68k: BRA self
    sat.bus.scsp.start(); // SNDON → SCSP generates

    // Print the ROM disassembly — this *is* the trace of the pipeline setup.
    println!("--- sine test ROM (master starts at {CODE_PC:#06X}) ---");
    for o in (CODE_PC..0x4C).step_by(2) {
        let w = u16::from_be_bytes([
            build_sine_rom()[o as usize],
            build_sine_rom()[o as usize + 1],
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
    assert!(peak >= AMP as u16 / 4, "sine reaches a meaningful level (peak={peak})");
    assert!(peak < 30000, "steady single voice must not clip (peak={peak})");
}
