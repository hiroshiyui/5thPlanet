//! Saturn-wide memory bus seen by both SH-2 cores.
//!
//! [`SaturnBus`] implements `sh2::Bus`. It dispatches every access by
//! the top 8 bits of the address, after the CPU has already stripped
//! the cached/cache-through indicator. Anything outside the modeled
//! region map reads as 0 and ignores writes (open-bus behaviour).
//!
//! Memory map (physical, after CPU `classify()`):
//!
//! ```text
//!   0x0000_0000..0x000F_FFFF   BIOS ROM (mirrored)
//!   0x0010_0000..0x0017_FFFF   SMPC / system registers (stub)
//!   0x0018_0000..0x001F_FFFF   Backup RAM (32 KiB, mirrored)
//!   0x0020_0000..0x002F_FFFF   Low work RAM (1 MiB)
//!   0x0040_0000..0x004F_FFFF   Sound area (stub)
//!   0x0500_0000..0x05FF_FFFF   A-Bus + B-Bus (stub for M2; VDP1/2/SCSP
//!                              get subdivided in M3+)
//!   0x0600_0000..0x06FF_FFFF   High work RAM (1 MiB)
//!   everything else            open bus (0 on read, drop writes)
//! ```
//!
//! Wait-state numbers are conservative defaults; software can later
//! override them via the SH-2 `BSC` registers (out of M2 scope).

use sh2::bus::{AccessKind, Bus};

use crate::cartridge::Cartridge;
use crate::cd_block::{CD_BLOCK_BASE, CD_BLOCK_END, CdBlock};

/// CD-block data-transfer port — the 32-bit alias the SCU DMA reads sector
/// data from (`src == 0x0581_8000`, special-cased in the SCU). Distinct from
/// the register/FIFO window at [`CD_BLOCK_BASE`].
const CD_DATA_PORT: u32 = 0x0581_8000;
const CD_DATA_PORT_END: u32 = 0x0581_8003;
use crate::memory::{BackupRam, BiosRom, Ram, StubRegisterBank};
use crate::scsp::Scsp;
use crate::scu::{SCU_BASE, SCU_END, Scu};
use crate::smpc::Smpc;
use crate::vdp1::Vdp1;
use crate::vdp2::Vdp2;

pub const BIOS_BASE: u32 = 0x0000_0000;
pub const BIOS_END: u32 = 0x000F_FFFF;
pub const SMPC_BASE: u32 = 0x0010_0000;
pub const SMPC_END: u32 = 0x0017_FFFF;
pub const BACKUP_BASE: u32 = 0x0018_0000;
pub const BACKUP_END: u32 = 0x001F_FFFF;
pub const LOW_WRAM_BASE: u32 = 0x0020_0000;
pub const LOW_WRAM_END: u32 = 0x002F_FFFF;
pub const SOUND_BASE: u32 = 0x0040_0000;
pub const SOUND_END: u32 = 0x004F_FFFF;
/// SCSP sound RAM: 512 KiB at 0x05A0_0000, mirrored through the 1 MiB window,
/// shared between the SH-2 and the hosted sound 68k (which sees it at 0).
pub const SCSP_RAM_BASE: u32 = 0x05A0_0000;
pub const SCSP_RAM_END: u32 = 0x05AF_FFFF;
/// SCSP control + slot + DSP registers at 0x05B0_0000 (mirrored).
pub const SCSP_REGS_BASE: u32 = 0x05B0_0000;
pub const SCSP_REGS_END: u32 = 0x05BF_FFFF;
pub const ABUS_BBUS_BASE: u32 = 0x0500_0000;
pub const ABUS_BBUS_END: u32 = 0x05FF_FFFF;
/// Inter-CPU FRT input-capture (FTI) trigger regions: a 16-bit write to the
/// first pulses the slave SH-2's FTI, the second the master's (Yabause
/// `SSH2/MSH2InputCaptureWriteWord`; Saturn hardware wires the cores' FTI here).
pub const SLAVE_FTI_BASE: u32 = 0x0100_0000;
pub const SLAVE_FTI_END: u32 = 0x017F_FFFF;
pub const MASTER_FTI_BASE: u32 = 0x0180_0000;
pub const MASTER_FTI_END: u32 = 0x01FF_FFFF;
pub const HIGH_WRAM_BASE: u32 = 0x0600_0000;
pub const HIGH_WRAM_END: u32 = 0x06FF_FFFF;

/// Wait states added on top of the inherent access cycles. Numbers are
/// the per-region defaults documented in the SH7604 / Saturn manuals.
const BIOS_WAITS: u32 = 10;
const BACKUP_WAITS: u32 = 6;
const LOW_WRAM_WAITS: u32 = 3;
const HIGH_WRAM_WAITS: u32 = 1;
const STUB_WAITS: u32 = 0;

// Not `Clone`: the CD-block holds a `Box<dyn SectorSource>` (an image or a
// live drive) that isn't cloneable, and nothing clones the bus anyway.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SaturnBus {
    pub bios: BiosRom,
    pub smpc: Smpc,
    /// Internal battery-backed backup RAM (32 KiB, odd-byte packed).
    pub backup: BackupRam,
    pub low_wram: Ram,
    pub sound: StubRegisterBank,
    pub scu: Scu,
    pub vdp1: Vdp1,
    pub vdp2: Vdp2,
    pub cd_block: CdBlock,
    pub scsp: Scsp,
    /// Rear expansion connector: Extension RAM / backup / ROM cart, or an
    /// empty slot (the default). Mapped at `0x0200_0000..0x04FF_FFFF`.
    pub cartridge: Cartridge,
    pub abus_bbus: StubRegisterBank,
    pub high_wram: Ram,
    /// Current global cycle, refreshed by the scheduler before each CPU
    /// step (see `Sh2Entity::step`). Lets time-varying peripheral reads —
    /// notably the SMPC `SF` flag's INTBACK completion — resolve at the
    /// exact instruction that reads them, rather than at a coarse drain
    /// boundary.
    pub cycle: u64,
    /// PC of the CPU currently stepping, refreshed alongside `cycle`. Debug-only
    /// (used by the `SAT_WWATCH` write-watchpoint to name the writing instruction);
    /// `#[serde(skip)]` so it never affects save-state determinism.
    #[serde(skip)]
    pub step_pc: u32,
    /// Pending FRT input-capture (FTI) triggers from inter-CPU signalling: a
    /// 16-bit write to `0x0100_0000..0x017F_FFFF` pulses the *slave*'s FTI,
    /// `0x0180_0000..0x01FF_FFFF` the *master*'s. The bus can't reach the cores,
    /// so it flags here and `Saturn::drain_input_capture` applies it (the
    /// SMPC/SCU drain-at-aggregate pattern). `#[serde(skip)]` — transient signal.
    #[serde(skip)]
    pub slave_input_capture: bool,
    #[serde(skip)]
    pub master_input_capture: bool,
    /// Debug-only programmatic write-watchpoint (the `sdbg` debugger's `bw`):
    /// `(addr, optional value)`. When a bus write matches, [`watch_hit`] records
    /// `(addr, written value, writing-instruction PC)` — the *first* match only,
    /// so the run can stop at the originating store. `#[serde(skip)]` — debug
    /// state, never part of a save state.
    #[serde(skip)]
    pub watch: Option<(u32, Option<u32>)>,
    #[serde(skip)]
    pub watch_hit: Option<(u32, u32, u32)>,
}

impl SaturnBus {
    /// Extra SH-2 stall for an access to VDP1 VRAM/FB while the plotter is
    /// drawing — the SH-2↔VDP1 VRAM bus contention (M12 #6). 0 elsewhere. Added
    /// on top of [`waits_for`] so graphics-drawing code can't outrun the
    /// reference; see [`crate::vdp1::Vdp1::draw_slowdown`].
    #[inline]
    fn vdp1_draw_stall(&mut self, addr: u32, write: bool) -> u32 {
        if Vdp1::owns(addr) {
            self.vdp1.draw_slowdown(addr, self.cycle, write)
        } else {
            0
        }
    }

    /// Debug-only: record the first bus write matching the programmatic
    /// write-watchpoint (`bw`). `val` is the size-appropriate written value;
    /// `step_pc` is the PC of the storing instruction (refreshed per step).
    fn note_write(&mut self, addr: u32, val: u32) {
        if self.watch_hit.is_none()
            && let Some((waddr, wval)) = self.watch
            && addr == waddr
            && wval.is_none_or(|v| v == val)
        {
            self.watch_hit = Some((addr, val, self.step_pc));
        }
    }
    /// Construct a bus with the supplied BIOS image. RAM regions are
    /// freshly allocated and zeroed.
    pub fn new(bios: Vec<u8>) -> Self {
        Self {
            bios: BiosRom::new(bios),
            smpc: Smpc::new(),
            backup: BackupRam::new(),
            low_wram: Ram::new(1024 * 1024),
            sound: StubRegisterBank::new("SOUND"),
            scu: Scu::new(),
            vdp1: Vdp1::new(),
            vdp2: Vdp2::new(),
            cd_block: CdBlock::new(),
            scsp: Scsp::new(),
            cartridge: Cartridge::None,
            abus_bbus: StubRegisterBank::new("A/B-BUS"),
            high_wram: Ram::new(1024 * 1024),
            cycle: 0,
            step_pc: 0,
            slave_input_capture: false,
            master_input_capture: false,
            watch: None,
            watch_hit: None,
        }
    }

    /// Construct with a placeholder all-zero BIOS image — useful for
    /// bus-routing unit tests that don't need real boot code.
    pub fn with_blank_bios() -> Self {
        Self::new(vec![0u8; 512 * 1024])
    }
}

/// Debug write-watchpoint (boot-divergence investigation): when `SAT_WWATCH=0xADDR`
/// is set, log any write whose byte span covers `ADDR`, with width, value, access
/// kind and cycle. No-op (one cheap env check, cached) when unset.
#[inline]
fn write_watch(addr: u32, size: u32, val: u32, k: AccessKind, cycle: u64, pc: u32) {
    use std::sync::OnceLock;
    static W: OnceLock<Option<u32>> = OnceLock::new();
    let w = *W.get_or_init(|| {
        std::env::var("SAT_WWATCH")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
    });
    // Value-match mode (SAT_WVAL=0xVALUE): log any write of that 32-bit value,
    // regardless of address — finds *where* a known datum lands (e.g. the
    // IP.BIN "SEGA" word 0x53454741 to locate the IP.BIN's WRAM destination).
    static WV: OnceLock<Option<u32>> = OnceLock::new();
    let wv = *WV.get_or_init(|| {
        std::env::var("SAT_WVAL")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
    });
    if let Some(tv) = wv
        && val == tv
    {
        eprintln!(
            "WVAL {tv:08X}: w{}@{addr:08X} {k:?} cyc={cycle} pc={pc:08X}",
            size * 8
        );
    }
    if let Some(t) = w {
        // High work RAM (1 MiB) mirrors across its 16 MiB window, so a write to
        // any mirror of `t` hits the same byte. Compare folded offsets there.
        let fold = |a: u32| {
            if (HIGH_WRAM_BASE..=HIGH_WRAM_END).contains(&a) {
                HIGH_WRAM_BASE + ((a - HIGH_WRAM_BASE) % 0x10_0000)
            } else {
                a
            }
        };
        let (fa, ft) = (fold(addr), fold(t));
        // Optional window (SAT_WWATCH_WIN bytes) so a memset-style clear loop
        // near the target is visible, not just the exact word.
        static WIN: OnceLock<u32> = OnceLock::new();
        let win = *WIN.get_or_init(|| {
            std::env::var("SAT_WWATCH_WIN")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        });
        if fa.wrapping_add(size) > ft.saturating_sub(win) && fa < ft.wrapping_add(win.max(1)) {
            eprintln!(
                "WWATCH {t:08X}: w{}@{addr:08X} val={val:08X} {k:?} cyc={cycle} pc={pc:08X}",
                size * 8
            );
        }
    }
}

/// Debug read-watchpoint (handshake/poll investigation): when `SAT_RWATCH=0xADDR`
/// is set, log any read whose byte span covers `[ADDR, ADDR+SAT_RWATCH_WIN)`,
/// with width, value, access kind, cycle and PC. Mirrors [`write_watch`] but for
/// reads — finds where the master polls a status word a peripheral/68k writes
/// (e.g. a sound-driver ready signature in sound RAM). No-op when unset. Note the
/// SCSP 68k reads via its own bus, so this isolates *main-CPU* (+ SCU-DMA) reads.
#[inline]
fn read_watch(addr: u32, size: u32, val: u32, k: AccessKind, cycle: u64, pc: u32) {
    use std::sync::OnceLock;
    static R: OnceLock<Option<u32>> = OnceLock::new();
    let r = *R.get_or_init(|| {
        std::env::var("SAT_RWATCH")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
    });
    let Some(t) = r else { return };
    static WIN: OnceLock<u32> = OnceLock::new();
    let win = *WIN.get_or_init(|| {
        std::env::var("SAT_RWATCH_WIN")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4)
    });
    if addr.wrapping_add(size) > t && addr < t.wrapping_add(win.max(1)) {
        eprintln!(
            "RWATCH {t:08X}: r{}@{addr:08X} val={val:08X} {k:?} cyc={cycle} pc={pc:08X}",
            size * 8
        );
    }
}

#[inline]
fn waits_for(addr: u32, write: bool) -> u32 {
    match addr {
        BIOS_BASE..=BIOS_END => BIOS_WAITS,
        BACKUP_BASE..=BACKUP_END => BACKUP_WAITS,
        LOW_WRAM_BASE..=LOW_WRAM_END => LOW_WRAM_WAITS,
        HIGH_WRAM_BASE..=HIGH_WRAM_END => HIGH_WRAM_WAITS,
        // VDP1 VRAM / framebuffer / registers — the B-bus charges the SH-2 a base
        // access cost (separate from, and on top of, the draw-slowdown). Mednafen
        // `scu.inc` BBusRW: a read costs +14; a write +2 immediate + 9 deferred
        // write-finish (≈11 for the back-to-back writes the BIOS animation does).
        0x05C0_0000..=0x05D7_FFFF => {
            if write {
                11
            } else {
                14
            }
        }
        // VDP2 VRAM / CRAM / registers — read +20, write +2 immediate + 3 deferred
        // (≈5). The asymmetry (slow reads) matches Mednafen `scu.inc` BBusRW.
        0x05E0_0000..=0x05FB_FFFF => {
            if write {
                5
            } else {
                20
            }
        }
        // A-bus CS2 (`0x05800000..=0x058FFFFF`): the CD-block host registers + its
        // SCU-DMA data port. Mednafen `scu.inc` ABusRW_DB charges the SH-2 **+8**
        // per access (read and write alike). The BIOS audio-CD player polls the
        // CD HIRQ / CR1–4 status here every panel loop, so a 0-wait here let the
        // master outrun the LLE reference (the BGM-trigger phase lead — the master
        // reaches its BGM command ~43 Timer-B ticks early).
        0x0580_0000..=0x058F_FFFF => 8,
        _ => STUB_WAITS,
    }
}

impl Bus for SaturnBus {
    fn read8(&mut self, addr: u32, _k: AccessKind) -> (u8, u32) {
        let v = match addr {
            BIOS_BASE..=BIOS_END => self.bios.read8(addr - BIOS_BASE),
            SMPC_BASE..=SMPC_END => {
                self.smpc.settle_intback(self.cycle);
                self.smpc.read8(addr - SMPC_BASE)
            }
            BACKUP_BASE..=BACKUP_END => self.backup.read8(addr - BACKUP_BASE),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.read8(addr - LOW_WRAM_BASE),
            SOUND_BASE..=SOUND_END => self.sound.read8(addr - SOUND_BASE),
            a if Cartridge::owns(a) => self.cartridge.read8(a),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.read8(addr - CD_BLOCK_BASE),
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.read8(a)
            }
            a if Vdp2::owns(a) => self.vdp2.read8(a),
            SCU_BASE..=SCU_END => self.scu.read8(addr - SCU_BASE),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.read8(addr - SCSP_RAM_BASE),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.read8(addr - SCSP_REGS_BASE),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.read8(addr - ABUS_BBUS_BASE),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.read8(addr - HIGH_WRAM_BASE),
            _ => 0,
        };
        read_watch(addr, 1, v as u32, _k, self.cycle, self.step_pc);
        (v, waits_for(addr, false) + self.vdp1_draw_stall(addr, false))
    }

    fn read16(&mut self, addr: u32, _k: AccessKind) -> (u16, u32) {
        let v = match addr {
            BIOS_BASE..=BIOS_END => self.bios.read16(addr - BIOS_BASE),
            SMPC_BASE..=SMPC_END => {
                self.smpc.settle_intback(self.cycle);
                self.smpc.read16(addr - SMPC_BASE)
            }
            BACKUP_BASE..=BACKUP_END => self.backup.read16(addr - BACKUP_BASE),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.read16(addr - LOW_WRAM_BASE),
            SOUND_BASE..=SOUND_END => self.sound.read16(addr - SOUND_BASE),
            a if Cartridge::owns(a) => self.cartridge.read16(a),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.read16(addr - CD_BLOCK_BASE),
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.read16(a)
            }
            a if Vdp2::owns(a) => self.vdp2.read16(a),
            SCU_BASE..=SCU_END => self.scu.read16(addr - SCU_BASE),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.read16(addr - SCSP_RAM_BASE),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.read16(addr - SCSP_REGS_BASE),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.read16(addr - ABUS_BBUS_BASE),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.read16(addr - HIGH_WRAM_BASE),
            _ => 0,
        };
        read_watch(addr, 2, v as u32, _k, self.cycle, self.step_pc);
        (v, waits_for(addr, false) + self.vdp1_draw_stall(addr, false))
    }

    fn read32(&mut self, addr: u32, _k: AccessKind) -> (u32, u32) {
        let v = match addr {
            BIOS_BASE..=BIOS_END => self.bios.read32(addr - BIOS_BASE),
            SMPC_BASE..=SMPC_END => {
                self.smpc.settle_intback(self.cycle);
                self.smpc.read32(addr - SMPC_BASE)
            }
            BACKUP_BASE..=BACKUP_END => self.backup.read32(addr - BACKUP_BASE),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.read32(addr - LOW_WRAM_BASE),
            SOUND_BASE..=SOUND_END => self.sound.read32(addr - SOUND_BASE),
            a if Cartridge::owns(a) => self.cartridge.read32(a),
            // CD data-transfer port alias (the SCU-DMA path; see saturn_scu).
            CD_DATA_PORT..=CD_DATA_PORT_END => self.cd_block.read_data_port(),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.read32(addr - CD_BLOCK_BASE),
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.read32(a)
            }
            a if Vdp2::owns(a) => self.vdp2.read32(a),
            SCU_BASE..=SCU_END => self.scu.read32(addr - SCU_BASE),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.read32(addr - SCSP_RAM_BASE),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.read32(addr - SCSP_REGS_BASE),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.read32(addr - ABUS_BBUS_BASE),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.read32(addr - HIGH_WRAM_BASE),
            _ => 0,
        };
        read_watch(addr, 4, v, _k, self.cycle, self.step_pc);
        (v, waits_for(addr, false) + self.vdp1_draw_stall(addr, false))
    }

    fn write8(&mut self, addr: u32, val: u8, _k: AccessKind) -> u32 {
        write_watch(addr, 1, val as u32, _k, self.cycle, self.step_pc);
        self.note_write(addr, val as u32);
        match addr {
            BIOS_BASE..=BIOS_END => self.bios.write_ignored(),
            SMPC_BASE..=SMPC_END => self.smpc.write8(addr - SMPC_BASE, val),
            BACKUP_BASE..=BACKUP_END => self.backup.write8(addr - BACKUP_BASE, val),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.write8(addr - LOW_WRAM_BASE, val),
            SOUND_BASE..=SOUND_END => self.sound.write8(addr - SOUND_BASE, val),
            a if Cartridge::owns(a) => self.cartridge.write8(a, val),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.write8(addr - CD_BLOCK_BASE, val),
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.write8(a, val)
            }
            a if Vdp2::owns(a) => self.vdp2.write8(a, val),
            SCU_BASE..=SCU_END => self.scu.write8(addr - SCU_BASE, val),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.write8(addr - SCSP_RAM_BASE, val),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.write8(addr - SCSP_REGS_BASE, val),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.write8(addr - ABUS_BBUS_BASE, val),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.write8(addr - HIGH_WRAM_BASE, val),
            _ => {}
        }
        waits_for(addr, true) + self.vdp1_draw_stall(addr, true)
    }

    fn write16(&mut self, addr: u32, val: u16, _k: AccessKind) -> u32 {
        write_watch(addr, 2, val as u32, _k, self.cycle, self.step_pc);
        self.note_write(addr, val as u32);
        match addr {
            BIOS_BASE..=BIOS_END => self.bios.write_ignored(),
            SMPC_BASE..=SMPC_END => self.smpc.write16(addr - SMPC_BASE, val),
            BACKUP_BASE..=BACKUP_END => self.backup.write16(addr - BACKUP_BASE, val),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.write16(addr - LOW_WRAM_BASE, val),
            SOUND_BASE..=SOUND_END => self.sound.write16(addr - SOUND_BASE, val),
            a if Cartridge::owns(a) => self.cartridge.write16(a, val),
            CD_BLOCK_BASE..=CD_BLOCK_END => {
                // Record the issuing master PC so the CD command trace can name
                // the loader code that drove each command (debug-only).
                self.cd_block.caller_pc = self.step_pc;
                self.cd_block.write16(addr - CD_BLOCK_BASE, val)
            }
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.write16(a, val)
            }
            a if Vdp2::owns(a) => self.vdp2.write16(a, val),
            SCU_BASE..=SCU_END => self.scu.write16(addr - SCU_BASE, val),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.write16(addr - SCSP_RAM_BASE, val),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.write16(addr - SCSP_REGS_BASE, val),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.write16(addr - ABUS_BBUS_BASE, val),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.write16(addr - HIGH_WRAM_BASE, val),
            // Inter-CPU FRT input-capture (FTI) trigger: a 16-bit write to this
            // region pulses the *other* SH-2's free-running-timer input capture
            // (the Saturn slave/master "wake" signal). Drained at the aggregate.
            SLAVE_FTI_BASE..=SLAVE_FTI_END => self.slave_input_capture = true,
            MASTER_FTI_BASE..=MASTER_FTI_END => self.master_input_capture = true,
            _ => {}
        }
        waits_for(addr, true) + self.vdp1_draw_stall(addr, true)
    }

    fn write32(&mut self, addr: u32, val: u32, _k: AccessKind) -> u32 {
        write_watch(addr, 4, val, _k, self.cycle, self.step_pc);
        self.note_write(addr, val);
        match addr {
            BIOS_BASE..=BIOS_END => self.bios.write_ignored(),
            SMPC_BASE..=SMPC_END => self.smpc.write32(addr - SMPC_BASE, val),
            BACKUP_BASE..=BACKUP_END => self.backup.write32(addr - BACKUP_BASE, val),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.write32(addr - LOW_WRAM_BASE, val),
            SOUND_BASE..=SOUND_END => self.sound.write32(addr - SOUND_BASE, val),
            a if Cartridge::owns(a) => self.cartridge.write32(a, val),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.write32(addr - CD_BLOCK_BASE, val),
            a if Vdp1::owns(a) => {
                self.vdp1.tick(self.cycle);
                self.vdp1.write32(a, val)
            }
            a if Vdp2::owns(a) => self.vdp2.write32(a, val),
            SCU_BASE..=SCU_END => self.scu.write32(addr - SCU_BASE, val),
            SCSP_RAM_BASE..=SCSP_RAM_END => self.scsp.ram.write32(addr - SCSP_RAM_BASE, val),
            SCSP_REGS_BASE..=SCSP_REGS_END => self.scsp.ctrl.write32(addr - SCSP_REGS_BASE, val),
            ABUS_BBUS_BASE..=ABUS_BBUS_END => self.abus_bbus.write32(addr - ABUS_BBUS_BASE, val),
            HIGH_WRAM_BASE..=HIGH_WRAM_END => self.high_wram.write32(addr - HIGH_WRAM_BASE, val),
            _ => {}
        }
        waits_for(addr, true) + self.vdp1_draw_stall(addr, true)
    }
}
