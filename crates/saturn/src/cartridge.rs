//! Saturn cartridge slot (the rear expansion connector on the A-bus).
//!
//! Three families of cart plug into the same address window and are told
//! apart by software reading a single **cart-ID byte at `0x04FF_FFFF`**
//! (an empty slot floats high and reads `0xFF`). The BIOS backup manager
//! and several games probe this byte and then size the regions below it:
//!
//! ```text
//!   0x0200_0000..0x023F_FFFF   ROM cart (game ROM, e.g. KOF '95 / Ultraman)
//!   0x0240_0000..0x025F_FFFF   Extension DRAM bank 0
//!   0x0260_0000..0x027F_FFFF   Extension DRAM bank 1
//!   0x0400_0000..0x047F_FFFF   Battery (backup) RAM cart
//!   0x04FF_FFFF                cart-ID byte
//! ```
//!
//! Addresses here are the *masked physical* form the SH-2 produces after
//! `classify()` strips the cache/cache-through indicator, so the CPU's
//! `0x2200_0000` / `0x2400_0000` mirrors land on the same handlers.
//!
//! Cart-ID codes match the hardware (and MAME's `device_sat_cart_interface`):
//! `0x21`/`0x22`/`0x23`/`0x24` for 4/8/16/32-Mbit battery carts, `0x5A`/`0x5C`
//! for the 8-Mbit (1 MiB) / 32-Mbit (4 MiB) Extension DRAM carts, and `0xFF`
//! for a ROM cart or an empty slot.
//!
//! The two Extension DRAM banks are independent chips (1 MiB cart = two
//! 512 KiB chips; 4 MiB cart = two 2 MiB chips), each mirrored across its
//! 2 MiB window. The 4 MiB DRAM cart is what Street Fighter Zero 3 and
//! The King of Fighters '97 require. The battery cart stores its bytes in
//! the Saturn's odd-byte packing — each 32-bit access carries one data byte
//! in bits 23..16 and another in bits 7..0, the rest reading back as 0 — so
//! a future backup manager sees the same layout real hardware presents.

use crate::memory::Ram;

pub const CART_BASE: u32 = 0x0200_0000;
pub const CART_END: u32 = 0x04FF_FFFF;

pub const CART_ROM_BASE: u32 = 0x0200_0000;
pub const CART_ROM_END: u32 = 0x023F_FFFF;
pub const CART_DRAM0_BASE: u32 = 0x0240_0000;
pub const CART_DRAM0_END: u32 = 0x025F_FFFF;
pub const CART_DRAM1_BASE: u32 = 0x0260_0000;
pub const CART_DRAM1_END: u32 = 0x027F_FFFF;
pub const CART_BRAM_BASE: u32 = 0x0400_0000;
pub const CART_BRAM_END: u32 = 0x047F_FFFF;
pub const CART_ID_ADDR: u32 = 0x04FF_FFFF;

/// Cart-ID returned by an empty slot (and by a ROM cart): the A-bus floats
/// high. The BIOS reads this as "no Extension RAM / unknown cart".
const ID_NONE: u8 = 0xFF;
const ID_DRAM_1MB: u8 = 0x5A; // 8 Mbit
const ID_DRAM_4MB: u8 = 0x5C; // 32 Mbit
const ID_BRAM_4MBIT: u8 = 0x21; // 512 KiB
const ID_BRAM_8MBIT: u8 = 0x22; // 1 MiB
const ID_BRAM_16MBIT: u8 = 0x23; // 2 MiB
const ID_BRAM_32MBIT: u8 = 0x24; // 4 MiB

/// What is plugged into the slot. Defaults to [`Cartridge::None`].
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Cartridge {
    /// Empty slot — ID byte reads `0xFF`, all cart space floats high.
    #[default]
    None,
    /// Extension DRAM cart: two independent banks, each mirrored across
    /// its 2 MiB window. `id` is `0x5A` (1 MiB) or `0x5C` (4 MiB).
    Dram { bank0: Ram, bank1: Ram, id: u8 },
    /// Battery (backup) RAM cart. Bytes are stored linearly; the bus
    /// presents them in the Saturn odd-byte packing (see module docs).
    Bram { bytes: Vec<u8>, id: u8 },
    /// Game ROM cart, mirrored across its 4 MiB window. ID reads `0xFF`.
    /// The image is read-only external media, so it's `#[serde(skip)]`'d out
    /// of save states (like the BIOS and disc) and re-grafted by
    /// `Saturn::load_state`; only `Dram`/`Bram` carry volatile state worth
    /// snapshotting.
    Rom {
        #[serde(skip)]
        bytes: Vec<u8>,
    },
}

impl Cartridge {
    /// 1 MiB (8 Mbit) Extension DRAM cart — two 512 KiB banks.
    pub fn ext_ram_1mb() -> Self {
        Cartridge::Dram {
            bank0: Ram::new(512 * 1024),
            bank1: Ram::new(512 * 1024),
            id: ID_DRAM_1MB,
        }
    }

    /// 4 MiB (32 Mbit) Extension DRAM cart — two 2 MiB banks. Required by
    /// Street Fighter Zero 3 and The King of Fighters '97.
    pub fn ext_ram_4mb() -> Self {
        Cartridge::Dram {
            bank0: Ram::new(2 * 1024 * 1024),
            bank1: Ram::new(2 * 1024 * 1024),
            id: ID_DRAM_4MB,
        }
    }

    /// Battery backup-RAM cart of `bytes` total capacity (must be one of
    /// 512 KiB / 1 / 2 / 4 MiB to map to a real cart-ID). The cart is
    /// pre-formatted with the BIOS "BackUpRam Format" signature so the
    /// backup manager treats it as initialised.
    pub fn backup_ram(size: usize) -> Self {
        let id = match size {
            0x0008_0000 => ID_BRAM_4MBIT,
            0x0010_0000 => ID_BRAM_8MBIT,
            0x0020_0000 => ID_BRAM_16MBIT,
            0x0040_0000 => ID_BRAM_32MBIT,
            _ => ID_NONE,
        };
        let mut bytes = vec![0u8; size];
        // nvram_default: the 16-byte "BackUpRam Format" tag repeated 32×.
        const TAG: &[u8; 16] = b"BackUpRam Format";
        for chunk in bytes.chunks_mut(16).take(32) {
            chunk.copy_from_slice(&TAG[..chunk.len()]);
        }
        Cartridge::Bram { bytes, id }
    }

    /// Game ROM cart from a raw image (mirrored to fill the 4 MiB window).
    pub fn rom(bytes: Vec<u8>) -> Self {
        Cartridge::Rom { bytes }
    }

    /// True for any address in the cartridge window (`0x0200_0000..=0x04FF_FFFF`).
    #[inline]
    pub fn owns(addr: u32) -> bool {
        (CART_BASE..=CART_END).contains(&addr)
    }

    /// The cart-ID byte exposed at `0x04FF_FFFF`.
    pub fn cart_id(&self) -> u8 {
        match self {
            Cartridge::None | Cartridge::Rom { .. } => ID_NONE,
            Cartridge::Dram { id, .. } | Cartridge::Bram { id, .. } => *id,
        }
    }

    // --- byte primitives: every wider access is composed from these so the
    //     backup packing and ID-byte placement stay consistent across widths.

    fn read8_impl(&self, addr: u32) -> u8 {
        match addr {
            CART_ROM_BASE..=CART_ROM_END => match self {
                Cartridge::Rom { bytes } if !bytes.is_empty() => {
                    bytes[(addr - CART_ROM_BASE) as usize % bytes.len()]
                }
                _ => 0xFF,
            },
            CART_DRAM0_BASE..=CART_DRAM0_END => match self {
                Cartridge::Dram { bank0, .. } => bank0.read8(addr - CART_DRAM0_BASE),
                _ => 0xFF,
            },
            CART_DRAM1_BASE..=CART_DRAM1_END => match self {
                Cartridge::Dram { bank1, .. } => bank1.read8(addr - CART_DRAM1_BASE),
                _ => 0xFF,
            },
            CART_BRAM_BASE..=CART_BRAM_END => self.bram_read8(addr - CART_BRAM_BASE),
            CART_ID_ADDR => self.cart_id(),
            _ => 0xFF,
        }
    }

    fn write8_impl(&mut self, addr: u32, val: u8) {
        match addr {
            CART_DRAM0_BASE..=CART_DRAM0_END => {
                if let Cartridge::Dram { bank0, .. } = self {
                    bank0.write8(addr - CART_DRAM0_BASE, val);
                }
            }
            CART_DRAM1_BASE..=CART_DRAM1_END => {
                if let Cartridge::Dram { bank1, .. } = self {
                    bank1.write8(addr - CART_DRAM1_BASE, val);
                }
            }
            CART_BRAM_BASE..=CART_BRAM_END => self.bram_write8(addr - CART_BRAM_BASE, val),
            // ROM and the ID byte are read-only; empty/unmapped drops writes.
            _ => {}
        }
    }

    /// Backup RAM packing: each 32-bit word holds two data bytes, in bits
    /// 23..16 (even index) and 7..0 (odd index); the other two byte lanes
    /// are wired to 0. So big-endian byte offset 1 and 3 within a word carry
    /// data, offsets 0 and 2 always read back 0.
    fn bram_read8(&self, off: u32) -> u8 {
        let Cartridge::Bram { bytes, .. } = self else {
            return 0xFF;
        };
        if bytes.is_empty() {
            return 0xFF;
        }
        let nwords = (bytes.len() / 2) as u32;
        let w = (off / 4 % nwords) as usize;
        match off & 3 {
            1 => bytes[w * 2],
            3 => bytes[w * 2 + 1],
            _ => 0x00,
        }
    }

    fn bram_write8(&mut self, off: u32, val: u8) {
        let Cartridge::Bram { bytes, .. } = self else {
            return;
        };
        if bytes.is_empty() {
            return;
        }
        let nwords = (bytes.len() / 2) as u32;
        let w = (off / 4 % nwords) as usize;
        match off & 3 {
            1 => bytes[w * 2] = val,
            3 => bytes[w * 2 + 1] = val,
            _ => {} // wired-to-0 lanes ignore writes
        }
    }

    pub fn read8(&self, addr: u32) -> u8 {
        self.read8_impl(addr)
    }
    pub fn read16(&self, addr: u32) -> u16 {
        u16::from_be_bytes([self.read8_impl(addr), self.read8_impl(addr.wrapping_add(1))])
    }
    pub fn read32(&self, addr: u32) -> u32 {
        u32::from_be_bytes([
            self.read8_impl(addr),
            self.read8_impl(addr.wrapping_add(1)),
            self.read8_impl(addr.wrapping_add(2)),
            self.read8_impl(addr.wrapping_add(3)),
        ])
    }
    pub fn write8(&mut self, addr: u32, val: u8) {
        self.write8_impl(addr, val);
    }
    pub fn write16(&mut self, addr: u32, val: u16) {
        let b = val.to_be_bytes();
        self.write8_impl(addr, b[0]);
        self.write8_impl(addr.wrapping_add(1), b[1]);
    }
    pub fn write32(&mut self, addr: u32, val: u32) {
        let b = val.to_be_bytes();
        self.write8_impl(addr, b[0]);
        self.write8_impl(addr.wrapping_add(1), b[1]);
        self.write8_impl(addr.wrapping_add(2), b[2]);
        self.write8_impl(addr.wrapping_add(3), b[3]);
    }
}
