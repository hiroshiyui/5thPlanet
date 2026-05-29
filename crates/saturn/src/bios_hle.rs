//! HLE of the SEGA Saturn BIOS **system-call library** (ADR-0011).
//!
//! Games reach BIOS services through a pointer table in low work RAM
//! (`0x06000200..0x06000360`): a call is `JSR @[table slot]`, which lands the
//! master SH-2 at a fixed BIOS entry address. For an HLE direct boot
//! ([`Saturn::cold_hle_boot`](crate::Saturn::cold_hle_boot)) we populate that
//! table ourselves and intercept execution at those entry addresses — running a
//! host implementation instead of BIOS code — so a game gets a working SYS
//! environment without depending on our (failing) BIOS boot.
//!
//! Modelled on Yabause `src/bios.c` (`BiosInit` / `BiosHandleFunc`). The entry
//! addresses are the BIOS-ROM addresses the table points at; the dispatcher
//! keys on `(pc - 0x200) >> 2` exactly as Yabause does. Functions read args in
//! `R4..R7`, mutate machine state through the bus, set the result in `R0`, and
//! return via [`Cpu::hle_return`](sh2::Cpu::hle_return).

use sh2::Cpu;
use sh2::bus::{AccessKind, Bus};

use crate::SaturnBus;
use crate::scu::SCU_BASE;

/// BIOS SYS-call entry addresses (the values written into the call table; the
/// dispatch hook intercepts the master here). Mirrors the "Setup Bios
/// Functions" block of Yabause `BiosInit`.
pub const SYS_ADDRS: &[u32] = &[
    0x0210, 0x026C, 0x0274, 0x0280, 0x029C, 0x02DC, 0x0300, 0x0304, 0x0310, 0x0314, 0x0320, 0x0330,
    0x0334, 0x0340, 0x0344, 0x0358, // + BUP block:
    0x0384, 0x0388, 0x038C, 0x0390, 0x0394, 0x0398, 0x039C, 0x03A0, 0x03A4, 0x03A8,
];

/// Whether `pc` is a BIOS SYS-call entry the dispatcher handles. A cheap range
/// gate fronts the slice scan so the per-instruction check is nearly free off
/// the SYS addresses.
#[inline]
pub fn is_sys_addr(pc: u32) -> bool {
    (0x0210..=0x03A8).contains(&pc) && SYS_ADDRS.contains(&pc)
}

/// Run the HLE SYS function the master has reached, then return to the caller.
/// `cpu.regs.pc` is the entry address (already checked by [`is_sys_addr`]).
pub fn dispatch(cpu: &mut Cpu, bus: &mut SaturnBus) {
    let idx = (cpu.regs.pc.wrapping_sub(0x200)) >> 2;
    let mut implemented = true;
    match idx {
        0x48 => change_system_clock(cpu, bus), // 0x0320
        0x4C => get_semaphore(cpu, bus),       // 0x0330
        0x4D => clear_semaphore(cpu, bus),     // 0x0334
        _ => {
            // Not yet implemented: return harmlessly (R0 = 0) rather than
            // letting the game fall into the BIOS fatal handler.
            implemented = false;
            cpu.regs.r[0] = 0;
        }
    }
    trace(idx as u8, implemented, cpu.regs.r[4]);
    cpu.hle_return();
    // The dispatch replaces a BIOS routine; charge a nominal cost so the
    // scheduler deadline advances (the entity must not stall).
    cpu.pipeline.advance(11);
}

/// `ChangeSystemClock` (slot 0x320): switch the 320/352-dot clock and reset the
/// SCU DMA/timer/A-bus state. Faithful to Yabause `BiosChangeSystemClock` for
/// the parts reachable through the bus; the SH-2 on-chip WDT/standby pokes and
/// the SMPC clock-change command are elided (not load-bearing for boot).
fn change_system_clock(cpu: &mut Cpu, bus: &mut SaturnBus) {
    let speed = cpu.regs.r[4];
    let w = |bus: &mut SaturnBus, addr: u32, val: u32| {
        bus.write32(addr, val, AccessKind::Data);
    };
    // Stash the selected clock speed where games read it back.
    w(bus, 0x0600_0324, speed);

    // Clear, then reset, the SCU DMA / timer / A-bus registers (Yabause maps
    // these at 0x25FE_0000; the bus uses the cache-stripped physical base).
    w(bus, SCU_BASE + 0x00A8, 0); // A-bus interrupt ack
    w(bus, SCU_BASE + 0x00B8, 0); // A-bus refresh
    for j in 0..3u32 {
        for i in 0..7u32 {
            w(bus, SCU_BASE + j * 0xC + i * 4, 0);
        }
    }
    w(bus, SCU_BASE + 0x0060, 0); // DMA force stop
    w(bus, SCU_BASE + 0x0080, 0); // DSP control port
    w(bus, SCU_BASE + 0x00B0, 0x1FF0_1FF0); // A-bus set
    w(bus, SCU_BASE + 0x00B4, 0x1FF0_1FF0);
    w(bus, SCU_BASE + 0x00B8, 0x1F); // A-bus refresh
    w(bus, SCU_BASE + 0x00A8, 0x1); // A-bus interrupt ack
    w(bus, SCU_BASE + 0x0090, 0x3FF); // Timer 0 compare
    w(bus, SCU_BASE + 0x0094, 0x1FF); // Timer 1 set data
    w(bus, SCU_BASE + 0x0098, 0); // Timer 1 mode

    let mask = bus.read32(0x0600_0348, AccessKind::Data).0;
    w(bus, SCU_BASE + 0x00A0, mask); // SCU interrupt mask
    if mask & 0x8000 == 0 {
        w(bus, SCU_BASE + 0x00A8, 1); // A-bus interrupt acknowledge
    }
    // ChangeSystemClock leaves R0 untouched.
}

/// `GetSemaphore` (slot 0x330): test-and-set the semaphore byte at
/// `0x06000B00 + R4`; R0 = 1 if it was free, else 0 (Yabause `BiosGetSemaphore`).
fn get_semaphore(cpu: &mut Cpu, bus: &mut SaturnBus) {
    let addr = 0x0600_0B00 + cpu.regs.r[4];
    let cur = bus.read8(addr, AccessKind::Data).0;
    cpu.regs.r[0] = u32::from(cur == 0);
    bus.write8(addr, cur | 0x80, AccessKind::Data);
}

/// `ClearSemaphore` (slot 0x334): clear the semaphore byte at `0x06000B00 + R4`.
fn clear_semaphore(cpu: &mut Cpu, bus: &mut SaturnBus) {
    bus.write8(0x0600_0B00 + cpu.regs.r[4], 0, AccessKind::Data);
}

/// Write the BIOS SYS-call pointer table into low work RAM: each slot at
/// `0x06000200 + i*4` gets the matching entry address (which the dispatch hook
/// intercepts). Mirrors Yabause `BiosInit`'s "Setup Bios Functions" block.
/// Called by [`Saturn::cold_hle_boot`](crate::Saturn::cold_hle_boot).
pub fn install_call_table(bus: &mut SaturnBus) {
    for &addr in SYS_ADDRS {
        bus.write32(0x0600_0200 + addr - 0x0200, addr, AccessKind::Data);
    }
}

/// Trace each dispatched SYS call (name, whether implemented, R4) under the
/// `SAT_SYS_TRACE` env — the iterate-to-boot signal for which functions a game
/// needs. No-op in tests / when unset.
#[cfg(not(test))]
fn trace(idx: u8, implemented: bool, r4: u32) {
    if std::env::var_os("SAT_SYS_TRACE").is_some() {
        let name = sys_name(idx);
        let tag = if implemented { "" } else { " UNIMPL" };
        eprintln!("SYS {idx:#04x} {name}{tag} (R4={r4:08X})");
    }
}
#[cfg(test)]
fn trace(_idx: u8, _implemented: bool, _r4: u32) {}

#[cfg(not(test))]
fn sys_name(idx: u8) -> &'static str {
    match idx {
        0x04 => "PowerOnMemoryClear",
        0x1B => "ExecuteCDPlayer",
        0x1D => "CheckMPEGCard",
        0x20 => "ChangeScuInterruptPriority",
        0x27 => "CDINIT2",
        0x37 => "CDINIT1",
        0x40 => "SetScuInterrupt",
        0x41 => "GetScuInterrupt",
        0x44 => "SetSh2Interrupt",
        0x45 => "GetSh2Interrupt",
        0x48 => "ChangeSystemClock",
        0x4C => "GetSemaphore",
        0x4D => "ClearSemaphore",
        0x50 => "SetScuInterruptMask",
        0x51 => "ChangeScuInterruptMask",
        0x56 => "BUPInit",
        0x60..=0x6B => "BUP*",
        _ => "?",
    }
}
