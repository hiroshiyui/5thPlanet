//! SCSP host wiring through the Saturn aggregate (M5 task #3).
//!
//! Exercises the full path the BIOS uses to bring up sound: the main SH-2
//! stages a 68k program into sound RAM (at 0x05A0_0000), then SMPC `SNDON`
//! releases the sound 68k, which the scheduler runs from that RAM.

use saturn::Saturn;
use sh2::bus::{AccessKind, Bus};

const COMREG: u32 = 0x0010_001F;
const SOUND_RAM: u32 = 0x05A0_0000;
const SCSP_REGS: u32 = 0x05B0_0000;

#[test]
fn sndon_releases_the_sound_cpu_which_runs_from_sound_ram() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();

    // 68k vector table + program staged via the SH-2's view of sound RAM.
    sat.bus.write32(SOUND_RAM, 0x0001_0000, AccessKind::Data); // initial SSP
    sat.bus
        .write32(SOUND_RAM + 4, 0x0000_2000, AccessKind::Data); // initial PC
    sat.bus
        .write16(SOUND_RAM + 0x2000, 0x7642, AccessKind::Data); // MOVEQ #0x42, D3
    sat.bus
        .write16(SOUND_RAM + 0x2002, 0x60FE, AccessKind::Data); // BRA self

    assert!(!sat.bus.scsp.running, "68k held in reset at power-on");

    sat.bus.write8(COMREG, 0x06, AccessKind::Data); // SMPC SNDON
    sat.run_for(5_000);

    assert!(sat.bus.scsp.running, "SNDON released the 68k");
    assert_eq!(
        sat.bus.scsp.cpu.regs.pc, 0x2002,
        "68k spinning in its BRA-self loop"
    );
    assert_eq!(
        sat.bus.scsp.cpu.regs.d[3], 0x42,
        "68k executed the staged program from sound RAM"
    );
}

#[test]
fn sndon_while_running_does_not_re_reset_the_68k() {
    // G2: a redundant SNDON must NOT restart a running sound driver — matching
    // Mednafen's `if(!SoundCPUOn) TurnSoundCPUOn()` guard. Only the first SNDON
    // after power-on / SNDOFF reloads the 68k reset vectors.
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    sat.bus.write32(SOUND_RAM, 0x0001_0000, AccessKind::Data); // SSP vector
    sat.bus
        .write32(SOUND_RAM + 4, 0x0000_2000, AccessKind::Data); // PC vector

    sat.bus.scsp.start(); // first SNDON → reset loads the PC vector
    assert!(sat.bus.scsp.running);
    assert_eq!(
        sat.bus.scsp.cpu.regs.pc, 0x2000,
        "reset loaded the PC vector"
    );

    // A running 68k has advanced its PC; a redundant SNDON must leave it.
    sat.bus.scsp.cpu.regs.pc = 0x3000;
    sat.bus.scsp.start(); // SNDON while running → no-op
    assert_eq!(
        sat.bus.scsp.cpu.regs.pc, 0x3000,
        "SNDON while running did not re-reset the 68k"
    );

    // After SNDOFF the next SNDON does reset (reloads the vector).
    sat.bus.scsp.stop();
    sat.bus.scsp.start();
    assert_eq!(
        sat.bus.scsp.cpu.regs.pc, 0x2000,
        "SNDON after SNDOFF reloaded the PC vector"
    );
}

#[test]
fn sndoff_re_holds_the_sound_cpu() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    sat.bus
        .write32(SOUND_RAM + 4, 0x0000_2000, AccessKind::Data);
    sat.bus
        .write16(SOUND_RAM + 0x2000, 0x60FE, AccessKind::Data); // BRA self
    sat.bus.write8(COMREG, 0x06, AccessKind::Data); // SNDON
    sat.run_for(2_000);
    assert!(sat.bus.scsp.running);
    sat.bus.write8(COMREG, 0x07, AccessKind::Data); // SNDOFF
    sat.run_for(2_000);
    assert!(!sat.bus.scsp.running, "SNDOFF re-held the 68k");
}

#[test]
fn scsp_registers_round_trip_through_the_sh2_bus() {
    let mut sat = Saturn::with_blank_bios();
    sat.bus.write16(SCSP_REGS + 0x400, 0xCAFE, AccessKind::Data);
    let (v, _) = sat.bus.read16(SCSP_REGS + 0x400, AccessKind::Data);
    assert_eq!(v, 0xCAFE);
}

#[test]
fn sound_ram_is_shared_between_sh2_and_the_68k() {
    let mut sat = Saturn::with_blank_bios();
    sat.reset();
    // Program: MOVE.W (0x20).w, D0 — load the word the SH-2 wrote at 0x20.
    // 0x3038 = MOVE.W (xxx).W, D0 ; extension word = 0x0020.
    sat.bus
        .write32(SOUND_RAM + 4, 0x0000_2000, AccessKind::Data);
    sat.bus
        .write16(SOUND_RAM + 0x2000, 0x3038, AccessKind::Data);
    sat.bus
        .write16(SOUND_RAM + 0x2002, 0x0020, AccessKind::Data);
    sat.bus
        .write16(SOUND_RAM + 0x2004, 0x60FE, AccessKind::Data); // BRA self
    // SH-2 plants a value at sound-RAM 0x20; the 68k reads the same byte.
    sat.bus.write16(SOUND_RAM + 0x20, 0x1234, AccessKind::Data);
    sat.bus.write8(COMREG, 0x06, AccessKind::Data); // SNDON
    sat.run_for(5_000);
    assert_eq!(
        sat.bus.scsp.cpu.regs.d[0] & 0xFFFF,
        0x1234,
        "68k read the SH-2's write to shared sound RAM"
    );
}
