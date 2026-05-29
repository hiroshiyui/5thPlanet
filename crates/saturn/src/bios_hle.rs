//! HLE of the SEGA Saturn BIOS **system-call library** (ADR-0011).
//!
//! Games reach BIOS services through a pointer table in low work RAM
//! (`0x06000200..0x06000360`): a call is `JSR @[table slot]`, which lands an
//! SH-2 at a fixed BIOS entry address. For an HLE direct boot
//! ([`Saturn::cold_hle_boot`](crate::Saturn::cold_hle_boot)) we populate that
//! table ourselves and intercept execution at those entry addresses — running a
//! host implementation instead of BIOS code — so a game gets a working SYS
//! environment without depending on our (failing) BIOS boot. The dispatch hook
//! is enabled on **both** SH-2s (the table is shared work RAM and the slave
//! `JSR`s it during its own init); [`dispatch`] takes `is_slave` because a few
//! functions (the SCU interrupt-mask calls) are master-only.
//!
//! Modelled on Yabause `src/bios.c` (`BiosInit` / `BiosHandleFunc`). The entry
//! addresses are the BIOS-ROM addresses the table points at; the dispatcher
//! keys on `(pc - 0x200) >> 2` exactly as Yabause does. Functions read args in
//! `R4..R7`, mutate machine state through the bus, set the result in `R0`, and
//! return via [`Cpu::hle_return`](sh2::Cpu::hle_return). The slave is *started*
//! (on `SSHON`) at the game-written entry `[0x06000250]` — see
//! [`Saturn::release_slave`](crate::Saturn::release_slave).

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
pub fn dispatch(cpu: &mut Cpu, bus: &mut SaturnBus, is_slave: bool) {
    let idx = (cpu.regs.pc.wrapping_sub(0x200)) >> 2;
    let mut implemented = true;
    match idx {
        0x40 => set_scu_interrupt(cpu, bus),                  // 0x0300
        0x41 => get_scu_interrupt(cpu, bus),                  // 0x0304
        0x44 => set_sh2_interrupt(cpu, bus),                  // 0x0310
        0x45 => get_sh2_interrupt(cpu, bus),                  // 0x0314
        0x48 => change_system_clock(cpu, bus),                // 0x0320
        0x4C => get_semaphore(cpu, bus),                      // 0x0330
        0x4D => clear_semaphore(cpu, bus),                    // 0x0334
        0x50 => set_scu_interrupt_mask(cpu, bus, is_slave),   // 0x0340
        0x51 => change_scu_interrupt_mask(cpu, bus, is_slave), // 0x0344
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

/// The SCU interrupt-handler table the BIOS dispatch indexes by `vector << 2`
/// (base `0x06000900`), and the BIOS default/"no handler" routine. Matches the
/// real BIOS layout (verified against the running ROM) and Yabause `BiosInit`.
const SCU_INT_TABLE: u32 = 0x0600_0900;
const BIOS_DEFAULT_HANDLER: u32 = 0x0600_0610;
/// The stored SCU interrupt mask the BIOS keeps in low work RAM.
const SCU_MASK_VAR: u32 = 0x0600_0348;

/// `SetScuInterrupt` (slot 0x300): install handler `R5` for SCU vector `R4` in
/// the BIOS dispatch table (`R5 == 0` → the BIOS default). Yabause
/// `BiosSetScuInterrupt`.
fn set_scu_interrupt(cpu: &mut Cpu, bus: &mut SaturnBus) {
    let slot = SCU_INT_TABLE + (cpu.regs.r[4] << 2);
    let handler = if cpu.regs.r[5] == 0 {
        BIOS_DEFAULT_HANDLER
    } else {
        cpu.regs.r[5]
    };
    bus.write32(slot, handler, AccessKind::Data);
}

/// `GetScuInterrupt` (slot 0x304): R0 = the installed SCU-vector-`R4` handler.
fn get_scu_interrupt(cpu: &mut Cpu, bus: &mut SaturnBus) {
    cpu.regs.r[0] = bus.read32(SCU_INT_TABLE + (cpu.regs.r[4] << 2), AccessKind::Data).0;
}

/// `SetSh2Interrupt` (slot 0x310): install handler `R5` directly in the SH-2
/// exception vector table at `VBR + (R4 << 2)`. With `R5 == 0` Yabause restores
/// a precomputed default; we leave the existing vector in place (games that use
/// this pass a real handler).
fn set_sh2_interrupt(cpu: &mut Cpu, bus: &mut SaturnBus) {
    if cpu.regs.r[5] != 0 {
        let slot = cpu.regs.vbr.wrapping_add(cpu.regs.r[4] << 2);
        bus.write32(slot, cpu.regs.r[5], AccessKind::Data);
    }
}

/// `GetSh2Interrupt` (slot 0x314): R0 = the SH-2 vector at `VBR + (R4 << 2)`.
fn get_sh2_interrupt(cpu: &mut Cpu, bus: &mut SaturnBus) {
    let slot = cpu.regs.vbr.wrapping_add(cpu.regs.r[4] << 2);
    cpu.regs.r[0] = bus.read32(slot, AccessKind::Data).0;
}

/// `SetScuInterruptMask` (slot 0x340): set the SCU interrupt mask to `R4` (both
/// the stored copy and the live IMS register), then A-bus-ack unless masked.
/// Yabause `BiosSetScuInterruptMask`.
fn set_scu_interrupt_mask(cpu: &mut Cpu, bus: &mut SaturnBus, is_slave: bool) {
    let m = cpu.regs.r[4];
    // The stored mask + the live IMS are master-only (Yabause `!isslave`); a
    // slave call must not clobber them, or it overwrites the master's setup
    // (e.g. re-masking VBlank the game just unmasked).
    if !is_slave {
        bus.write32(SCU_MASK_VAR, m, AccessKind::Data);
        bus.write32(SCU_BASE + 0x00A0, m, AccessKind::Data); // IMS
    }
    if m & 0x8000 == 0 {
        bus.write32(SCU_BASE + 0x00A8, 1, AccessKind::Data); // A-bus interrupt ack
    }
}

/// `ChangeScuInterruptMask` (slot 0x344): `mask = (stored & R4) | R5`, applied to
/// the stored copy + IMS, and `R4` written to IST; A-bus-ack unless masked.
/// Yabause `BiosChangeScuInterruptMask`.
fn change_scu_interrupt_mask(cpu: &mut Cpu, bus: &mut SaturnBus, is_slave: bool) {
    let (r4, r5) = (cpu.regs.r[4], cpu.regs.r[5]);
    // Stored mask + IMS + IST are master-only (Yabause `!isslave`).
    if !is_slave {
        let newmask = (bus.read32(SCU_MASK_VAR, AccessKind::Data).0 & r4) | r5;
        bus.write32(SCU_MASK_VAR, newmask, AccessKind::Data);
        bus.write32(SCU_BASE + 0x00A0, newmask, AccessKind::Data); // IMS
        bus.write32(SCU_BASE + 0x00A4, r4 as i16 as u32, AccessKind::Data); // IST
    }
    if r4 & 0x8000 == 0 {
        bus.write32(SCU_BASE + 0x00A8, 1, AccessKind::Data); // A-bus interrupt ack
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::SaturnBus;
    use sh2::Cpu;

    /// Land the master on SYS entry `addr` with the given args and dispatch it,
    /// as the step hook does. Returns the post-call (cpu, bus).
    fn run_sys(addr: u32, r4: u32, r5: u32) -> (Cpu, SaturnBus) {
        let mut cpu = Cpu::new();
        let mut bus = SaturnBus::with_blank_bios();
        cpu.regs.pc = addr;
        cpu.regs.pr = 0x0600_1000;
        cpu.regs.r[4] = r4;
        cpu.regs.r[5] = r5;
        assert!(is_sys_addr(addr), "{addr:#06X} is a SYS entry");
        dispatch(&mut cpu, &mut bus, false);
        (cpu, bus)
    }

    #[test]
    fn set_scu_interrupt_installs_handler_and_returns() {
        let (cpu, mut bus) = run_sys(0x0300, 0x40, 0x0601_2345);
        // vector 0x40 → table slot 0x06000900 + 0x40*4 = 0x06000A00.
        assert_eq!(bus.read32(SCU_INT_TABLE + 0x100, AccessKind::Data).0, 0x0601_2345);
        assert_eq!(cpu.regs.pc, cpu.regs.pr, "SYS call returns via PR");
    }

    #[test]
    fn set_scu_interrupt_zero_handler_uses_bios_default() {
        let (_cpu, mut bus) = run_sys(0x0304 - 4, 0x41, 0); // 0x0300 = SetScuInterrupt
        assert_eq!(
            bus.read32(SCU_INT_TABLE + 0x41 * 4, AccessKind::Data).0,
            BIOS_DEFAULT_HANDLER,
        );
    }

    #[test]
    fn get_scu_interrupt_reads_back_installed_handler() {
        let mut cpu = Cpu::new();
        let mut bus = SaturnBus::with_blank_bios();
        bus.write32(SCU_INT_TABLE + 0x47 * 4, 0x0609_ABCD, AccessKind::Data);
        cpu.regs.pc = 0x0304; // GetScuInterrupt
        cpu.regs.pr = 0x0600_1000;
        cpu.regs.r[4] = 0x47;
        dispatch(&mut cpu, &mut bus, false);
        assert_eq!(cpu.regs.r[0], 0x0609_ABCD);
    }

    #[test]
    fn change_scu_interrupt_mask_applies_and_then_or() {
        let mut cpu = Cpu::new();
        let mut bus = SaturnBus::with_blank_bios();
        bus.write32(SCU_MASK_VAR, 0xFFFF_FFFF, AccessKind::Data);
        cpu.regs.pc = 0x0344; // ChangeScuInterruptMask
        cpu.regs.pr = 0x0600_1000;
        cpu.regs.r[4] = 0xFFFF_FFFE; // AND: clear VBlankIN mask bit
        cpu.regs.r[5] = 0x0000_0004; // OR: set bit 2
        dispatch(&mut cpu, &mut bus, false);
        // (0xFFFFFFFF & 0xFFFFFFFE) | 0x4 = 0xFFFFFFFE
        assert_eq!(bus.read32(SCU_MASK_VAR, AccessKind::Data).0, 0xFFFF_FFFE);
    }

    #[test]
    fn set_sh2_interrupt_writes_vbr_vector() {
        let mut cpu = Cpu::new();
        let mut bus = SaturnBus::with_blank_bios();
        cpu.regs.vbr = 0x0600_0000;
        cpu.regs.pc = 0x0310; // SetSh2Interrupt
        cpu.regs.pr = 0x0600_1000;
        cpu.regs.r[4] = 0x4A;
        cpu.regs.r[5] = 0x0604_2222;
        dispatch(&mut cpu, &mut bus, false);
        assert_eq!(bus.read32(0x0600_0000 + 0x4A * 4, AccessKind::Data).0, 0x0604_2222);
    }
}
