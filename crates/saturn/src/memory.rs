//! Typed memory regions backing the Saturn bus.
//!
//! Each struct owns its bytes and exposes `read8/16/32` + `write8/16/32`
//! at *region-local* offsets — that is, an offset already reduced modulo
//! the region size by the caller. Endianness is big-endian throughout
//! (SH-2 / Saturn convention).
//!
//! Wait-state values are conservative best-case numbers from the SH7604
//! `BSC` defaults and the *ST-V Service Manual* memory-map appendix.
//! Real software can change them by writing the BSC's `WCR`; that
//! refinement is queued for a later milestone.

/// Read-only BIOS ROM. Mirrors to fill its addressable window: the real
/// Saturn 512 KiB BIOS appears twice across the 1 MiB region at
/// `0x0000_0000..0x0010_0000`. A bus-side caller passes in a local
/// offset that's already been folded into the region's range; the ROM
/// then folds *that* into `rom.len()` so any image size mirrors cleanly.
#[derive(Clone, Debug)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BiosRom {
    // The BIOS image is read-only external media (and copyrighted), so save
    // states reference it rather than embedding it: `#[serde(skip)]` omits
    // the bytes, and `Saturn::load_state` re-grafts the live image. The
    // placeholder is a single 0xFF byte so the mirror-fold modulo never
    // divides by zero before the graft.
    #[serde(skip, default = "placeholder_bios")]
    rom: Vec<u8>,
}

fn placeholder_bios() -> Vec<u8> {
    vec![0xFF]
}

impl BiosRom {
    pub fn new(image: Vec<u8>) -> Self {
        assert!(!image.is_empty(), "BIOS image must be non-empty");
        Self { rom: image }
    }

    pub fn len(&self) -> usize {
        self.rom.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rom.is_empty()
    }

    /// The raw BIOS bytes — used to fingerprint the image for save-state
    /// media validation (the image itself is never serialized).
    pub fn image(&self) -> &[u8] {
        &self.rom
    }

    pub fn read8(&self, offset: u32) -> u8 {
        self.rom[(offset as usize) % self.rom.len()]
    }
    pub fn read16(&self, offset: u32) -> u16 {
        u16::from_be_bytes([self.read8(offset), self.read8(offset.wrapping_add(1))])
    }
    pub fn read32(&self, offset: u32) -> u32 {
        u32::from_be_bytes([
            self.read8(offset),
            self.read8(offset.wrapping_add(1)),
            self.read8(offset.wrapping_add(2)),
            self.read8(offset.wrapping_add(3)),
        ])
    }
    // Writes to ROM are silently ignored. Software that writes to BIOS
    // address space on real hardware sees the write disappear; we model
    // the same so misbehaving code doesn't trap inside the bus.
    pub fn write_ignored(&self) {}
}

/// Generic byte-addressable RAM region. Used for both work RAM tiers and
/// backup RAM. `size` is the region's byte length; addresses are folded
/// modulo `size` so mirrored aliases work transparently.
#[derive(Clone, Debug)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Ram {
    bytes: Vec<u8>,
}

impl Ram {
    pub fn new(size: usize) -> Self {
        Self {
            bytes: vec![0u8; size],
        }
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    fn idx(&self, offset: u32) -> usize {
        (offset as usize) % self.bytes.len()
    }

    pub fn read8(&self, offset: u32) -> u8 {
        self.bytes[self.idx(offset)]
    }
    pub fn read16(&self, offset: u32) -> u16 {
        u16::from_be_bytes([self.read8(offset), self.read8(offset.wrapping_add(1))])
    }
    pub fn read32(&self, offset: u32) -> u32 {
        u32::from_be_bytes([
            self.read8(offset),
            self.read8(offset.wrapping_add(1)),
            self.read8(offset.wrapping_add(2)),
            self.read8(offset.wrapping_add(3)),
        ])
    }
    pub fn write8(&mut self, offset: u32, val: u8) {
        let i = self.idx(offset);
        self.bytes[i] = val;
    }
    pub fn write16(&mut self, offset: u32, val: u16) {
        let i = self.idx(offset);
        let n = self.bytes.len();
        let b = val.to_be_bytes();
        self.bytes[i] = b[0];
        self.bytes[(i + 1) % n] = b[1];
    }
    pub fn write32(&mut self, offset: u32, val: u32) {
        let i = self.idx(offset);
        let b = val.to_be_bytes();
        let n = self.bytes.len();
        self.bytes[i] = b[0];
        self.bytes[(i + 1) % n] = b[1];
        self.bytes[(i + 2) % n] = b[2];
        self.bytes[(i + 3) % n] = b[3];
    }
}

/// Capacity of the Saturn's internal battery-backed backup RAM, in *data*
/// bytes (32 KiB). The address window is twice this because of the odd-byte
/// packing below.
pub const INTERNAL_BACKUP_BYTES: usize = 32 * 1024;

/// The Saturn's internal battery-backed backup RAM at `0x0018_0000` — the
/// built-in "memory card" games write saves to.
///
/// Hardware exposes the 32 KiB across a 64 KiB window with **odd-byte
/// packing**: each 16-bit word carries one data byte in its low half and
/// reads 0 in its high half (data byte `n` lives at byte address `2n+1`).
/// This matches MAME `backupram_r/w` (`saturn_m.cpp`) and the backup-RAM
/// *cartridge* packing in [`crate::cartridge`]. Out-of-window offsets fold
/// modulo the data size, so the 512 KiB bus region mirrors transparently.
///
/// On power-on a charged-battery console shows the BIOS "BackUpRam Format"
/// signature, so a fresh instance is pre-formatted the same way
/// (MAME `nvram_init`); the frontend overwrites it with the persisted file
/// when one exists.
#[derive(Clone, Debug)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct BackupRam {
    data: Vec<u8>,
}

impl Default for BackupRam {
    fn default() -> Self {
        Self::new()
    }
}

impl BackupRam {
    pub fn new() -> Self {
        let mut data = vec![0u8; INTERNAL_BACKUP_BYTES];
        // nvram_init: the 16-byte "BackUpRam Format" tag, four times.
        const TAG: &[u8; 16] = b"BackUpRam Format";
        for chunk in data.chunks_mut(16).take(4) {
            chunk.copy_from_slice(TAG);
        }
        Self { data }
    }

    /// The raw 32 KiB of *data* bytes (unpacked) — for battery persistence.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Replace the contents from a persisted image (length-clamped to the
    /// 32 KiB capacity; a shorter file leaves the tail untouched).
    pub fn load(&mut self, src: &[u8]) {
        let n = src.len().min(self.data.len());
        self.data[..n].copy_from_slice(&src[..n]);
    }

    pub fn read8(&self, offset: u32) -> u8 {
        if offset & 1 == 0 {
            0 // even byte lanes are wired to 0
        } else {
            self.data[(offset as usize >> 1) % self.data.len()]
        }
    }
    pub fn read16(&self, offset: u32) -> u16 {
        u16::from_be_bytes([self.read8(offset), self.read8(offset.wrapping_add(1))])
    }
    pub fn read32(&self, offset: u32) -> u32 {
        u32::from_be_bytes([
            self.read8(offset),
            self.read8(offset.wrapping_add(1)),
            self.read8(offset.wrapping_add(2)),
            self.read8(offset.wrapping_add(3)),
        ])
    }
    pub fn write8(&mut self, offset: u32, val: u8) {
        if offset & 1 == 1 {
            let i = (offset as usize >> 1) % self.data.len();
            self.data[i] = val;
        }
    }
    pub fn write16(&mut self, offset: u32, val: u16) {
        let b = val.to_be_bytes();
        self.write8(offset, b[0]);
        self.write8(offset.wrapping_add(1), b[1]);
    }
    pub fn write32(&mut self, offset: u32, val: u32) {
        let b = val.to_be_bytes();
        self.write8(offset, b[0]);
        self.write8(offset.wrapping_add(1), b[1]);
        self.write8(offset.wrapping_add(2), b[2]);
        self.write8(offset.wrapping_add(3), b[3]);
    }
}

/// Stand-in for a region of registers that hasn't been modeled yet
/// (SMPC, SCU, VDP1/2, SCSP, A-bus, etc.). Reads return 0; writes are
/// dropped. Holds a name for traceable debug output.
#[derive(Clone, Debug)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct StubRegisterBank {
    // Debug-only label, and a borrowed `&'static str` at that, so it's not
    // serialized; a reloaded stub carries no state to lose (reads return 0,
    // writes are dropped) and keeps its compile-time name via the field's
    // default on the deserialize path being irrelevant to behaviour.
    #[serde(skip, default = "stub_name")]
    name: &'static str,
}

fn stub_name() -> &'static str {
    "STUB"
}

impl StubRegisterBank {
    pub const fn new(name: &'static str) -> Self {
        Self { name }
    }
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn read8(&self, _offset: u32) -> u8 {
        0
    }
    pub fn read16(&self, _offset: u32) -> u16 {
        0
    }
    pub fn read32(&self, _offset: u32) -> u32 {
        0
    }
    pub fn write8(&mut self, _offset: u32, _val: u8) {}
    pub fn write16(&mut self, _offset: u32, _val: u16) {}
    pub fn write32(&mut self, _offset: u32, _val: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- BiosRom ---------------------------------------------------------

    #[test]
    fn bios_reads_big_endian_and_mirrors_modulo_len() {
        // A 4-byte image. Bytes are returned MSB-first (SH-2 big-endian).
        let rom = BiosRom::new(vec![0x11, 0x22, 0x33, 0x44]);
        assert_eq!(rom.len(), 4);
        assert!(!rom.is_empty());
        assert_eq!(rom.read8(0), 0x11);
        assert_eq!(rom.read16(0), 0x1122);
        assert_eq!(rom.read32(0), 0x1122_3344);
        // Offset past the image folds modulo len(): offset 4 == offset 0.
        assert_eq!(rom.read8(4), 0x11, "4 % 4 == 0 mirrors to the start");
        assert_eq!(rom.read8(5), 0x22);
        // A 32-bit read straddling the wrap reads byte 3 then wraps to 0,1,2.
        assert_eq!(rom.read32(3), 0x4411_2233);
    }

    #[test]
    fn bios_writes_are_ignored() {
        // ROM is read-only; write_ignored is a no-op (compiles + does nothing).
        let rom = BiosRom::new(vec![0xAB, 0xCD]);
        rom.write_ignored();
        assert_eq!(rom.read16(0), 0xABCD);
        assert_eq!(rom.image(), &[0xAB, 0xCD]);
    }

    #[test]
    #[should_panic(expected = "BIOS image must be non-empty")]
    fn bios_rejects_empty_image() {
        let _ = BiosRom::new(Vec::new());
    }

    // ---- Ram -------------------------------------------------------------

    #[test]
    fn ram_round_trip_big_endian_all_widths() {
        let mut ram = Ram::new(16);
        assert_eq!(ram.len(), 16);
        assert!(!ram.is_empty());
        ram.write32(0, 0xDE_AD_BE_EF);
        // Big-endian byte layout: MSB at the lowest address.
        assert_eq!(ram.read8(0), 0xDE);
        assert_eq!(ram.read8(1), 0xAD);
        assert_eq!(ram.read8(2), 0xBE);
        assert_eq!(ram.read8(3), 0xEF);
        assert_eq!(ram.read16(0), 0xDEAD);
        assert_eq!(ram.read16(2), 0xBEEF);
        assert_eq!(ram.read32(0), 0xDEAD_BEEF);
        // write16 round-trips too.
        ram.write16(8, 0x1234);
        assert_eq!(ram.read16(8), 0x1234);
        ram.write8(8, 0x99);
        assert_eq!(ram.read8(8), 0x99);
    }

    #[test]
    fn ram_folds_offsets_modulo_size() {
        // A 4-byte RAM mirrors across the address space.
        let mut ram = Ram::new(4);
        ram.write8(1, 0x5A);
        assert_eq!(ram.read8(1 + 4), 0x5A, "(1+4) % 4 == 1");
        assert_eq!(ram.read8(1 + 8), 0x5A);
        // write32 at the very top wraps its trailing bytes back to index 0.
        ram.write32(3, 0xAA_BB_CC_DD); // index 3, then 0, 1, 2
        assert_eq!(ram.read8(3), 0xAA);
        assert_eq!(ram.read8(0), 0xBB);
        assert_eq!(ram.read8(1), 0xCC);
        assert_eq!(ram.read8(2), 0xDD);
    }

    #[test]
    fn ram_as_slice_exposes_backing_bytes() {
        let mut ram = Ram::new(4);
        ram.as_mut_slice()[2] = 0x42;
        assert_eq!(ram.as_slice(), &[0x00, 0x00, 0x42, 0x00]);
    }

    // ---- BackupRam (odd-byte packing) ------------------------------------

    #[test]
    fn backup_ram_is_preformatted_with_signature() {
        // nvram_init writes the 16-byte "BackUpRam Format" tag four times.
        let bram = BackupRam::new();
        assert_eq!(bram.bytes().len(), INTERNAL_BACKUP_BYTES);
        let tag = b"BackUpRam Format";
        for n in 0..4 {
            assert_eq!(&bram.bytes()[n * 16..n * 16 + 16], tag);
        }
        // The tag is *data* bytes, surfaced on odd byte addresses: data byte 0
        // ('B') lives at byte address 1.
        assert_eq!(bram.read8(1), b'B');
        assert_eq!(bram.read8(3), b'a');
        assert_eq!(bram.read8(5), b'c');
    }

    #[test]
    fn backup_ram_odd_byte_packing() {
        // Data byte n lives at byte address 2n+1; even addresses read 0 and
        // drop writes (the high half of each 16-bit word is wired to 0).
        let mut bram = BackupRam::new();
        bram.write8(1, 0x7E); // odd → data[0]
        bram.write8(0, 0xFF); // even → dropped
        assert_eq!(bram.read8(1), 0x7E);
        assert_eq!(bram.read8(0), 0x00, "even lane is wired to 0");
        // data[0] is the underlying storage for byte address 1.
        assert_eq!(bram.bytes()[0], 0x7E);
        // A different odd address maps to a different data index.
        bram.write8(3, 0x5C); // → data[1]
        assert_eq!(bram.bytes()[1], 0x5C);
        assert_eq!(bram.read8(3), 0x5C);
    }

    #[test]
    fn backup_ram_wide_access_packs_into_odd_lanes() {
        let mut bram = BackupRam::new();
        // write16 splits MSB→addr, LSB→addr+1; only the odd lane survives.
        bram.write16(0, 0x1234); // addr0 (even, dropped), addr1 (odd) <- 0x34
        assert_eq!(bram.read8(0), 0x00);
        assert_eq!(bram.read8(1), 0x34);
        assert_eq!(bram.read16(0), 0x0034, "even byte reads 0, odd reads data");
        // write32 over addresses 4..7: only the two odd lanes (5, 7) store.
        bram.write32(4, 0xAA_BB_CC_DD);
        assert_eq!(bram.read8(4), 0x00);
        assert_eq!(bram.read8(5), 0xBB);
        assert_eq!(bram.read8(6), 0x00);
        assert_eq!(bram.read8(7), 0xDD);
        assert_eq!(bram.read32(4), 0x00BB_00DD);
    }

    #[test]
    fn backup_ram_folds_modulo_data_size() {
        // The data index is (off >> 1) % 32 KiB, so the window mirrors.
        let mut bram = BackupRam::new();
        bram.write8(1, 0x42); // data[0]
        // off = (INTERNAL_BACKUP_BYTES * 2) + 1 → index (>>1) % len == 0.
        let mirror_off = (INTERNAL_BACKUP_BYTES as u32 * 2) + 1;
        assert_eq!(bram.read8(mirror_off), 0x42, "data mirrors past 64 KiB");
    }

    #[test]
    fn backup_ram_load_clamps_to_capacity() {
        let mut bram = BackupRam::new();
        // A short image only overwrites its prefix data bytes.
        bram.load(&[0xC0, 0xC1, 0xC2]);
        assert_eq!(bram.bytes()[0], 0xC0);
        assert_eq!(bram.bytes()[1], 0xC1);
        assert_eq!(bram.bytes()[2], 0xC2);
        // Data byte 0 surfaces on odd byte address 1.
        assert_eq!(bram.read8(1), 0xC0);
        // An over-long image is clamped to the 32 KiB capacity (no panic).
        let big = vec![0x9Au8; INTERNAL_BACKUP_BYTES + 100];
        bram.load(&big);
        assert_eq!(bram.bytes().len(), INTERNAL_BACKUP_BYTES);
        assert!(bram.bytes().iter().all(|&b| b == 0x9A));
    }

    #[test]
    fn backup_ram_default_matches_new() {
        assert_eq!(BackupRam::default().bytes(), BackupRam::new().bytes());
    }

    // ---- StubRegisterBank ------------------------------------------------

    #[test]
    fn stub_reads_zero_and_drops_writes() {
        let mut stub = StubRegisterBank::new("TEST");
        assert_eq!(stub.name(), "TEST");
        stub.write8(0x10, 0xFF);
        stub.write16(0x10, 0xFFFF);
        stub.write32(0x10, 0xFFFF_FFFF);
        assert_eq!(stub.read8(0x10), 0);
        assert_eq!(stub.read16(0x10), 0);
        assert_eq!(stub.read32(0x10), 0);
    }
}
