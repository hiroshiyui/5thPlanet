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
#[derive(Debug)]
#[derive(serde::Serialize, serde::Deserialize)]
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
}

impl SaturnBus {
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
        }
    }

    /// Construct with a placeholder all-zero BIOS image — useful for
    /// bus-routing unit tests that don't need real boot code.
    pub fn with_blank_bios() -> Self {
        Self::new(vec![0u8; 512 * 1024])
    }
}

#[inline]
fn waits_for(addr: u32) -> u32 {
    match addr {
        BIOS_BASE..=BIOS_END => BIOS_WAITS,
        BACKUP_BASE..=BACKUP_END => BACKUP_WAITS,
        LOW_WRAM_BASE..=LOW_WRAM_END => LOW_WRAM_WAITS,
        HIGH_WRAM_BASE..=HIGH_WRAM_END => HIGH_WRAM_WAITS,
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
        (v, waits_for(addr))
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
        (v, waits_for(addr))
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
        (v, waits_for(addr))
    }

    fn write8(&mut self, addr: u32, val: u8, _k: AccessKind) -> u32 {
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
        waits_for(addr)
    }

    fn write16(&mut self, addr: u32, val: u16, _k: AccessKind) -> u32 {
        match addr {
            BIOS_BASE..=BIOS_END => self.bios.write_ignored(),
            SMPC_BASE..=SMPC_END => self.smpc.write16(addr - SMPC_BASE, val),
            BACKUP_BASE..=BACKUP_END => self.backup.write16(addr - BACKUP_BASE, val),
            LOW_WRAM_BASE..=LOW_WRAM_END => self.low_wram.write16(addr - LOW_WRAM_BASE, val),
            SOUND_BASE..=SOUND_END => self.sound.write16(addr - SOUND_BASE, val),
            a if Cartridge::owns(a) => self.cartridge.write16(a, val),
            CD_BLOCK_BASE..=CD_BLOCK_END => self.cd_block.write16(addr - CD_BLOCK_BASE, val),
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
            _ => {}
        }
        waits_for(addr)
    }

    fn write32(&mut self, addr: u32, val: u32, _k: AccessKind) -> u32 {
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
        waits_for(addr)
    }
}
