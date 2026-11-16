//! Live physical-disc [`SectorSource`] for the 5thPlanet CD-block, backed by
//! the cross-platform **libcdio** C library (Linux/macOS/Windows/BSD).
//!
//! This lets the emulator read an *original* Saturn disc from a host optical
//! drive instead of a ripped image. The Saturn copy-protection "security ring"
//! is irrelevant: our authentication is HLE and header-only (it checks the
//! `"SEGA SEGASATURN"` string in the normal data area), and a PC drive reads
//! the standard data + audio tracks + TOC fine.
//!
//! # Feature gating & unsafe
//!
//! The actual drive access lives behind the **`libcdio`** cargo feature. With
//! the feature off — the default — [`PhysicalDisc::open`] returns an error and
//! the crate links nothing, so `cargo build --workspace` / CI need no libcdio.
//! With it on, the FFI to libcdio is `unsafe`; this crate is the **only** place
//! in the workspace that opts out of `unsafe_code = "forbid"` (see ADR-0009).
//! Even here it is `deny`'d crate-wide and allowed only on the private `ffi`
//! module, where every `unsafe` block carries a `// SAFETY:` justification.
//!
//! Build with it: `cargo build -p physdisc --features libcdio` (needs
//! `libcdio-dev` / `pkgconf libcdio`), or via the frontend's `physical-disc`
//! feature.

// Several constants/helpers are used only on the `libcdio` path; in the default
// stub build they're intentionally unused.
#![cfg_attr(not(feature = "libcdio"), allow(dead_code))]
// The workspace `forbid` doesn't reach this crate (see Cargo.toml); keep unsafe
// out of everything but the `ffi` module anyway.
#![deny(unsafe_code)]

use saturn::disc::{SectorSource, TrackInfo};

/// FAD (Frame ADdress) = absolute LBA/LSN + 150 (the 2-second lead-in).
const FAD_OFFSET: u32 = 150;
const SECTOR_USER: usize = 2048;
const SECTOR_RAW: usize = 2352;
/// A Mode-2 sector minus its 16-byte sync + header: the 8-byte subheader
/// (written twice) + 2328 bytes of user data / EDC (libcdio `M2RAW_SECTOR_SIZE`).
const M2RAW: usize = 2336;
/// Sync + header bytes that precede the Mode-2 payload in a raw sector.
const RAW_HEADER: usize = SECTOR_RAW - M2RAW;

/// Rebuild the full 2352-byte raw image of a Mode-2 sector at `fad` from its
/// 2336-byte post-header payload: the 12-byte sync pattern, the BCD MSF address
/// and mode byte 2, then the payload (subheader at raw offset 16, Form-2 user
/// data at 24 — where `saturn::disc::Disc` images keep them).
fn mode2_raw_sector(fad: u32, m2: &[u8], out: &mut [u8]) {
    let bcd = |v: u32| (((v / 10) << 4) | (v % 10)) as u8;
    out[0] = 0x00;
    out[1..11].fill(0xFF);
    out[11] = 0x00;
    out[12] = bcd(fad / (75 * 60));
    out[13] = bcd((fad / 75) % 60);
    out[14] = bcd(fad % 75);
    out[15] = 2; // mode
    out[RAW_HEADER..SECTOR_RAW].copy_from_slice(&m2[..M2RAW]);
}

/// A Saturn disc read live from a host optical drive.
pub struct PhysicalDisc {
    #[cfg(feature = "libcdio")]
    inner: ffi::Cdio,
    /// The raw block device, when the source is an actual drive (not an image).
    /// Data sectors are read through this with a plain *cooked* positioned read
    /// (`read_at`/`seek_read`) — the kernel hands back 2048-byte Mode-1/2 user
    /// data and, unlike libcdio's SG_IO `READ CD`, it needs no `CAP_SYS_RAWIO`,
    /// so it works for an ordinary `cdrom`-group user. libcdio is still used for
    /// the TOC and for CD-DA (audio) extraction.
    #[cfg(feature = "libcdio")]
    dev_file: Option<std::fs::File>,
    /// The last Mode-2 sector read raw (`(lsn, 2336-byte payload)`), so the
    /// CD-block's per-sector `subheader` probe and the following data read
    /// cost one drive read, not two.
    #[cfg(feature = "libcdio")]
    m2_cache: std::sync::Mutex<Option<(i32, Box<[u8; M2RAW]>)>>,
    /// Cached track table (number, ctrl/addr, audio?, start FAD, length).
    tracks: Vec<TrackInfo>,
    /// Per track (index-matched with `tracks`): a Mode-2 (CD-ROM XA) data
    /// track, whose sectors carry a subheader (and may be Form-2).
    xa: Vec<bool>,
    lead_out_fad: u32,
    /// Stable identity for save-state media validation (FNV-1a of the TOC).
    fingerprint: u64,
}

impl core::fmt::Debug for PhysicalDisc {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PhysicalDisc")
            .field("tracks", &self.tracks.len())
            .field("lead_out_fad", &self.lead_out_fad)
            .finish()
    }
}

impl PhysicalDisc {
    /// Open the optical device at `device` (e.g. `/dev/sr0`, `D:`), read its
    /// TOC, and present it as a [`SectorSource`].
    #[cfg(feature = "libcdio")]
    pub fn open(device: &str) -> Result<Self, String> {
        let inner = ffi::Cdio::open(device)?;
        let ffi::Toc {
            tracks,
            xa,
            lead_out_fad,
        } = inner.read_toc()?;
        let fingerprint = toc_fingerprint(&tracks, lead_out_fad);
        Ok(Self {
            inner,
            dev_file: open_block_device(device),
            m2_cache: std::sync::Mutex::new(None),
            tracks,
            xa,
            lead_out_fad,
            fingerprint,
        })
    }

    /// Whether `fad` lies in a Mode-2 (XA) data track.
    fn is_xa(&self, fad: u32) -> bool {
        self.tracks
            .iter()
            .zip(&self.xa)
            .any(|(t, &xa)| xa && fad >= t.start_fad && fad < t.start_fad + t.length)
    }

    /// Run `f` over the 2336-byte post-header payload of the Mode-2 sector at
    /// `lsn` (served from a one-sector cache). `None` if libcdio can't read it
    /// raw — e.g. the drive needs `CAP_SYS_RAWIO` for `READ CD` — in which case
    /// callers fall back to the cooked 2048-byte path.
    #[cfg(feature = "libcdio")]
    fn with_mode2<R>(&self, lsn: i32, f: impl FnOnce(&[u8; M2RAW]) -> R) -> Option<R> {
        let mut cache = self.m2_cache.lock().ok()?;
        if cache.as_ref().is_none_or(|(l, _)| *l != lsn) {
            let mut buf = Box::new([0u8; M2RAW]);
            if !self.inner.read_mode2_raw(lsn, &mut buf[..]) {
                return None;
            }
            *cache = Some((lsn, buf));
        }
        cache.as_ref().map(|(_, b)| f(b))
    }

    /// Cooked positioned read of one 2048-byte data sector via the block device
    /// (no `&mut self`, no `CAP_SYS_RAWIO`). `false` if there's no block device
    /// (e.g. an image source) or the read fails.
    #[cfg(feature = "libcdio")]
    fn read_block(&self, lsn: i32, out: &mut [u8]) -> bool {
        let Some(file) = &self.dev_file else {
            return false;
        };
        if lsn < 0 || out.len() < SECTOR_USER {
            return false;
        }
        let offset = lsn as u64 * SECTOR_USER as u64;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            file.read_exact_at(&mut out[..SECTOR_USER], offset).is_ok()
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            file.seek_read(&mut out[..SECTOR_USER], offset)
                .is_ok_and(|n| n == SECTOR_USER)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = offset;
            false
        }
    }

    /// Stub when built without the `libcdio` feature.
    #[cfg(not(feature = "libcdio"))]
    pub fn open(_device: &str) -> Result<Self, String> {
        Err("physdisc was built without the `libcdio` feature; \
             rebuild with --features physical-disc (needs libcdio)"
            .into())
    }

    fn track_at(&self, fad: u32) -> Option<&TrackInfo> {
        self.tracks
            .iter()
            .find(|t| fad >= t.start_fad && fad < t.start_fad + t.length)
    }
}

/// Open `path` as a raw block device for cooked data reads — `Some` only for
/// an actual device, not an image file (a `.cue` is a regular file, read via
/// libcdio instead). Best-effort: `None` disables the block-read fast path.
#[cfg(all(feature = "libcdio", unix))]
fn open_block_device(path: &str) -> Option<std::fs::File> {
    use std::os::unix::fs::FileTypeExt;
    let file = std::fs::File::open(path).ok()?;
    let is_device = file.metadata().ok()?.file_type().is_block_device();
    is_device.then_some(file)
}
#[cfg(all(feature = "libcdio", windows))]
fn open_block_device(path: &str) -> Option<std::fs::File> {
    // A Windows raw device path is like `\\.\D:`; treat a non-regular open as
    // the device. Best-effort — falls back to libcdio reads if this is None.
    let file = std::fs::File::open(path).ok()?;
    let regular = file.metadata().ok()?.is_file();
    (!regular).then_some(file)
}
#[cfg(all(feature = "libcdio", not(any(unix, windows))))]
fn open_block_device(_path: &str) -> Option<std::fs::File> {
    None
}

/// FNV-1a over the track table — a cheap, stable disc identity (no full image
/// to hash for a live drive).
fn toc_fingerprint(tracks: &[TrackInfo], lead_out_fad: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: u32| {
        for b in v.to_be_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
    };
    for t in tracks {
        eat(t.number as u32);
        eat(t.ctrl_addr as u32);
        eat(t.start_fad);
        eat(t.length);
    }
    eat(lead_out_fad);
    h
}

impl SectorSource for PhysicalDisc {
    fn toc(&self) -> [u8; 408] {
        // Same 102×4-byte layout as `saturn::disc::Disc::toc`.
        fn put(toc: &mut [u8; 408], slot: usize, ca: u8, fad: u32) {
            let p = slot * 4;
            toc[p] = ca;
            toc[p + 1] = (fad >> 16) as u8;
            toc[p + 2] = (fad >> 8) as u8;
            toc[p + 3] = fad as u8;
        }
        let mut toc = [0xFFu8; 408];
        let (mut first_ca, mut last_ca) = (0xFF, 0xFF);
        for t in &self.tracks {
            if t.number == 0 || t.number > 99 {
                continue;
            }
            put(&mut toc, (t.number - 1) as usize, t.ctrl_addr, t.start_fad);
            if t.number == self.first_track() {
                first_ca = t.ctrl_addr;
            }
            if t.number == self.last_track() {
                last_ca = t.ctrl_addr;
            }
        }
        // Entry 99 first-track / 100 last-track carry the track number in byte
        // 1 (not a FAD); entry 101 is the lead-out.
        put(&mut toc, 99, first_ca, 0);
        toc[99 * 4 + 1] = self.first_track();
        put(&mut toc, 100, last_ca, 0);
        toc[100 * 4 + 1] = self.last_track();
        put(&mut toc, 101, first_ca, self.lead_out_fad);
        toc
    }

    fn first_track(&self) -> u8 {
        self.tracks.first().map_or(1, |t| t.number)
    }
    fn last_track(&self) -> u8 {
        self.tracks.last().map_or(1, |t| t.number)
    }
    fn lead_out_fad(&self) -> u32 {
        self.lead_out_fad
    }

    fn track_at_fad(&self, fad: u32) -> Option<TrackInfo> {
        self.track_at(fad).copied()
    }

    fn read_sector(&self, fad: u32, out: &mut [u8]) -> bool {
        let _ = (fad, &out);
        #[cfg(feature = "libcdio")]
        {
            // Only data tracks have a 2048-byte user payload.
            match self.track_at(fad) {
                Some(t) if !t.is_audio => {}
                _ => return false,
            }
            let lsn = fad as i32 - FAD_OFFSET as i32;
            // Prefer the cooked block read (works without rawio); fall back to
            // libcdio's SG_IO read (images, or drives with rawio).
            self.read_block(lsn, out) || self.inner.read_mode1(lsn, &mut out[..SECTOR_USER])
        }
        #[cfg(not(feature = "libcdio"))]
        false
    }

    fn read_full_sector(&self, fad: u32, out: &mut [u8]) -> usize {
        let _ = (fad, &out);
        #[cfg(feature = "libcdio")]
        {
            let t = match self.track_at(fad) {
                Some(t) => t,
                None => return 0,
            };
            let lsn = fad as i32 - FAD_OFFSET as i32;
            // A Mode-2 (XA) sector: rebuild the full raw sector so the CD-block
            // finds the subheader + a Form-2 sector's 2324-byte payload where
            // an image keeps them.
            if self.is_xa(fad)
                && out.len() >= SECTOR_RAW
                && self
                    .with_mode2(lsn, |m2| mode2_raw_sector(fad, m2, out))
                    .is_some()
            {
                return SECTOR_RAW;
            }
            if t.is_audio {
                // Red Book: a full 2352-byte audio sector via DAE.
                if self.inner.read_audio(lsn, &mut out[..SECTOR_RAW]) {
                    SECTOR_RAW
                } else {
                    0
                }
            } else if self.read_block(lsn, &mut out[..SECTOR_USER])
                || self.inner.read_mode1(lsn, &mut out[..SECTOR_USER])
            {
                // Cooked 2048 user payload; the read pump's common path only
                // needs that (the full 2352 raw isn't reconstructed for data).
                SECTOR_USER
            } else {
                0
            }
        }
        #[cfg(not(feature = "libcdio"))]
        0
    }

    fn subheader(&self, fad: u32) -> Option<(u8, u8, u8, u8)> {
        // Only Mode-2 (XA) sectors have one; it is the first 4 bytes of the
        // raw post-header payload — `(file, channel, submode, coding)` on
        // disc, returned as `(chan, fnum, subm, cinf)` like `Disc::subheader`.
        // Without raw-read access it stays `None` (cooked-only behaviour).
        #[cfg(feature = "libcdio")]
        if self.is_xa(fad) {
            let lsn = fad as i32 - FAD_OFFSET as i32;
            return self.with_mode2(lsn, |m2| (m2[1], m2[0], m2[2], m2[3]));
        }
        let _ = fad;
        None
    }

    fn fingerprint(&self) -> u64 {
        self.fingerprint
    }
}

#[cfg(all(test, feature = "libcdio"))]
mod libcdio_tests {
    use super::*;
    use std::io::Write;

    /// libcdio's driver/image initialisation is not thread-safe: opening two
    /// images concurrently (parallel tests) intermittently fails, so each test
    /// holds this lock for its duration.
    static LIBCDIO_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The workspace `tmp/` (the project's temp-file convention, gitignored),
    /// plus the serialization guard.
    fn scratch_dir() -> (std::sync::MutexGuard<'static, ()>, std::path::PathBuf) {
        let guard = LIBCDIO_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/physdisc_tests");
        std::fs::create_dir_all(&d).unwrap();
        (guard, d)
    }

    /// A Mode-2 (XA) track through libcdio: the subheader is exposed and a
    /// Form-2 sector comes back as a full raw sector with its 2324-byte payload
    /// at offset 24. `cargo test -p physdisc --features libcdio -- --ignored`.
    #[test]
    #[ignore = "needs libcdio; reads a synthetic Mode-2 CUE/BIN image"]
    fn exposes_mode2_subheaders_from_a_cue_image() {
        let (_serial, dir) = scratch_dir();
        let bin = dir.join("physdisc_m2.bin");
        let cue = dir.join("physdisc_m2.cue");
        let mut data = vec![0u8; SECTOR_RAW * 4];
        for s in 0..4usize {
            let raw = &mut data[s * SECTOR_RAW..(s + 1) * SECTOR_RAW];
            mode2_raw_sector(FAD_OFFSET + s as u32, &[0u8; M2RAW], raw);
            // Subheader (twice): file 1, channel 3, submode (Form-2 on odd
            // sectors), coding 0.
            let subm = if s % 2 == 1 { 0x20 } else { 0x08 };
            for rep in [16, 20] {
                raw[rep..rep + 4].copy_from_slice(&[1, 3, subm, 0]);
            }
            raw[24..30].copy_from_slice(b"M2DATA");
            raw[30] = s as u8;
        }
        std::fs::write(&bin, &data).unwrap();
        std::fs::write(
            &cue,
            "FILE \"physdisc_m2.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let disc = PhysicalDisc::open(cue.to_str().unwrap()).expect("open cue via libcdio");
        assert_eq!(disc.subheader(FAD_OFFSET), Some((3, 1, 0x08, 0)));
        assert_eq!(disc.subheader(FAD_OFFSET + 1), Some((3, 1, 0x20, 0)));
        let mut raw = [0u8; SECTOR_RAW];
        assert_eq!(disc.read_full_sector(FAD_OFFSET + 1, &mut raw), SECTOR_RAW);
        assert_eq!(&raw[24..30], b"M2DATA");
        assert_eq!(raw[30], 1);
        assert_eq!(raw[15], 2, "mode byte");
        assert_eq!(&raw[12..15], &[0x00, 0x02, 0x01], "BCD MSF of FAD 151");
    }

    /// TEMP manual probe of a real drive (`SAT_CDROM`, default `/dev/sr0`):
    /// dumps the TOC and checks the Saturn IP header at FAD 150.
    #[test]
    #[ignore = "manual: probe a real optical drive"]
    fn probe_physical_drive() {
        let dev = std::env::var("SAT_CDROM").unwrap_or_else(|_| "/dev/sr0".into());
        let disc = match PhysicalDisc::open(&dev) {
            Ok(d) => d,
            Err(e) => {
                println!("open {dev} failed: {e}");
                return;
            }
        };
        println!(
            "drive {dev}: first={} last={} lead_out_fad={}",
            disc.first_track(),
            disc.last_track(),
            disc.lead_out_fad()
        );
        for t in &disc.tracks {
            println!(
                "  track {:2}  {}  start_fad={:>7}  len={:>7}  ctrl=0x{:02X}",
                t.number,
                if t.is_audio { "AUDIO" } else { "DATA " },
                t.start_fad,
                t.length,
                t.ctrl_addr
            );
        }
        let mut buf = [0u8; SECTOR_RAW];
        if disc.read_sector(FAD_OFFSET, &mut buf[..SECTOR_USER]) {
            println!(
                "Saturn disc (SEGA SEGASATURN header): {}",
                &buf[..15] == b"SEGA SEGASATURN"
            );
            println!("maker = {:?}", String::from_utf8_lossy(&buf[16..32]).trim());
            println!(
                "product = {:?}",
                String::from_utf8_lossy(&buf[32..42]).trim()
            );
            println!(
                "title = {:?}",
                String::from_utf8_lossy(&buf[0x60..0x70]).trim()
            );
        } else {
            println!("read_sector(FAD 150) failed");
        }
        // CD-DA (audio) read of the first audio track via libcdio DAE.
        if let Some(a) = disc.tracks.iter().find(|t| t.is_audio) {
            let lsn = a.start_fad as i32 - FAD_OFFSET as i32;
            let ok = disc.inner.read_audio(lsn, &mut buf);
            println!("CD-DA read of track {} (lsn {lsn}): {ok}", a.number);
        }
    }

    /// Open a synthetic CUE/BIN image through libcdio (it reads images as well
    /// as devices) and verify the TOC + a data-sector read end to end. Needs
    /// libcdio at link time, so it's gated + `#[ignore]`d:
    /// `cargo test -p physdisc --features libcdio -- --ignored`.
    #[test]
    #[ignore = "needs libcdio; reads a synthetic CUE/BIN image"]
    fn reads_toc_and_sector_from_a_cue_image() {
        let (_serial, dir) = scratch_dir();
        let bin = dir.join("physdisc_t.bin");
        let cue = dir.join("physdisc_t.cue");
        // Two tracks in one BIN: 4 Mode-1/2352 data sectors (user marker at the
        // 2048-payload offset 16), then 4 Red Book audio sectors (marker at the
        // raw start).
        let mut data = vec![0u8; SECTOR_RAW * 8];
        for s in 0..4 {
            let base = s * SECTOR_RAW + 16;
            data[base..base + 8].copy_from_slice(b"SATTEST\0");
            data[base + 7] = s as u8;
        }
        for s in 0..4 {
            let base = (4 + s) * SECTOR_RAW;
            data[base..base + 6].copy_from_slice(b"AUDIO!");
            data[base + 6] = s as u8;
        }
        std::fs::File::create(&bin)
            .unwrap()
            .write_all(&data)
            .unwrap();
        std::fs::write(
            &cue,
            "FILE \"physdisc_t.bin\" BINARY\n\
             \x20 TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n\
             \x20 TRACK 02 AUDIO\n    INDEX 01 00:00:04\n",
        )
        .unwrap();

        let disc = PhysicalDisc::open(cue.to_str().unwrap()).expect("open cue via libcdio");
        assert_eq!(disc.first_track(), 1);
        assert_eq!(disc.last_track(), 2);
        // 8 sectors from LSN 0: the lead-out starts at LSN 8 = FAD 158, right
        // after the last track's last sector.
        assert_eq!(disc.lead_out_fad(), FAD_OFFSET + 8);
        let t2 = disc.track_at_fad(FAD_OFFSET + 4).unwrap();
        assert_eq!(t2.start_fad + t2.length, disc.lead_out_fad());
        assert!(
            !disc.track_at_fad(FAD_OFFSET).unwrap().is_audio,
            "track 1 is data"
        );
        assert!(
            disc.track_at_fad(FAD_OFFSET + 4).unwrap().is_audio,
            "track 2 is audio"
        );

        // FAD 150 == LSN 0: the first user data sector.
        let mut buf = [0u8; SECTOR_RAW];
        assert!(
            disc.read_sector(FAD_OFFSET, &mut buf[..SECTOR_USER]),
            "read data sector 0"
        );
        assert_eq!(&buf[..7], b"SATTEST");
        assert_eq!(buf[7], 0);
        assert!(disc.read_sector(FAD_OFFSET + 2, &mut buf[..SECTOR_USER]));
        assert_eq!(buf[7], 2);

        // FAD 154 == LSN 4: the first audio sector, read raw (2352) via DAE.
        let mut audio = [0u8; SECTOR_RAW];
        assert_eq!(
            disc.read_full_sector(FAD_OFFSET + 4, &mut audio),
            SECTOR_RAW,
            "read audio sector"
        );
        assert_eq!(&audio[..6], b"AUDIO!");
        assert_eq!(audio[6], 0);
    }
}

/// libcdio FFI — the only `unsafe` in the workspace (ADR-0009). Confined here,
/// behind the `libcdio` feature, and wrapped in a safe [`Cdio`] handle.
#[cfg(feature = "libcdio")]
#[allow(unsafe_code)]
mod ffi {
    use super::{FAD_OFFSET, M2RAW, SECTOR_RAW, SECTOR_USER, TrackInfo};
    use core::ffi::{c_char, c_int, c_void};
    use std::ffi::CString;

    // libcdio opaque handle + the small slice of its C API we use. Signatures
    // follow <cdio/cdio.h> (libcdio ≥ 0.9): `lsn_t` = i32, `track_t` = u8,
    // `driver_return_code_t`/`track_format_t` are C ints. `TRACK_FORMAT_AUDIO`
    // is 0. Reads return DRIVER_OP_SUCCESS (0) on success.
    #[repr(C)]
    struct CdIoT {
        _opaque: [u8; 0],
    }

    #[link(name = "cdio")]
    unsafe extern "C" {
        fn cdio_open(psz_source: *const c_char, driver_id: c_int) -> *mut CdIoT;
        fn cdio_destroy(p: *mut CdIoT);
        fn cdio_get_first_track_num(p: *const CdIoT) -> u8;
        fn cdio_get_last_track_num(p: *const CdIoT) -> u8;
        fn cdio_get_track_lsn(p: *const CdIoT, track: u8) -> i32;
        fn cdio_get_track_last_lsn(p: *const CdIoT, track: u8) -> i32;
        fn cdio_get_track_format(p: *const CdIoT, track: u8) -> c_int;
        fn cdio_read_audio_sectors(
            p: *const CdIoT,
            buf: *mut c_void,
            lsn: i32,
            blocks: u32,
        ) -> c_int;
        fn cdio_read_mode1_sectors(
            p: *const CdIoT,
            buf: *mut c_void,
            lsn: i32,
            form2: bool,
            blocks: u32,
        ) -> c_int;
        fn cdio_read_mode2_sector(
            p: *const CdIoT,
            buf: *mut c_void,
            lsn: i32,
            form2: bool,
        ) -> c_int;
    }

    // <cdio/track.h> `track_format_t`: AUDIO, CDI, XA (Mode 2), DATA (Mode 1), …
    const TRACK_FORMAT_AUDIO: c_int = 0;
    const TRACK_FORMAT_XA: c_int = 2;
    const DRIVER_OP_SUCCESS: c_int = 0;
    const CDIO_INVALID_LSN: i32 = -1;
    /// `CDIO_CDROM_LEADOUT_TRACK`: `cdio_get_track_lsn` on it returns the
    /// lead-out's starting LSN (<cdio/track.h>).
    const CDIO_CDROM_LEADOUT_TRACK: u8 = 0xAA;

    /// The disc's TOC as libcdio reports it.
    pub struct Toc {
        pub tracks: Vec<TrackInfo>,
        /// Index-matched with `tracks`: a Mode-2 (XA) data track.
        pub xa: Vec<bool>,
        pub lead_out_fad: u32,
    }

    /// A safe owner of a libcdio handle.
    pub struct Cdio {
        p: *mut CdIoT,
    }

    // SAFETY: the handle is exclusively owned (never cloned or aliased) and is
    // only used through `&self` reads from the one thread that owns the CD-block
    // at a time; moving it to another thread is fine because libcdio handles
    // carry no thread affinity. It is deliberately NOT `Sync`.
    unsafe impl Send for Cdio {}

    impl Cdio {
        /// Open the named optical device (e.g. `/dev/sr0`); `Err` if libcdio
        /// can't probe it. `driver_id 0` lets libcdio auto-detect the backend.
        pub fn open(device: &str) -> Result<Self, String> {
            let c = CString::new(device).map_err(|_| "device path has a NUL byte".to_string())?;
            // driver_id 0 = DRIVER_UNKNOWN → libcdio auto-detects.
            // SAFETY: `c` is a valid NUL-terminated string that outlives the
            // call (libcdio copies the source name); a NULL return is handled.
            let p = unsafe { cdio_open(c.as_ptr(), 0) };
            if p.is_null() {
                return Err(format!("libcdio could not open '{device}'"));
            }
            Ok(Self { p })
        }

        /// Read the TOC into our [`TrackInfo`] table + lead-out FAD.
        pub fn read_toc(&self) -> Result<Toc, String> {
            // SAFETY (every call in this fn): `self.p` is a live, non-NULL
            // handle from `cdio_open` (checked there, freed only in `Drop`);
            // these getters only read the cached TOC.
            let first = unsafe { cdio_get_first_track_num(self.p) };
            // SAFETY: as above.
            let last = unsafe { cdio_get_last_track_num(self.p) };
            if first == 0 || first == 0xFF || last == 0xFF || last < first {
                return Err("libcdio returned no readable TOC".into());
            }
            let mut tracks = Vec::new();
            let mut xa = Vec::new();
            for n in first..=last {
                // SAFETY: as above; `n` is within the reported track range.
                let lsn = unsafe { cdio_get_track_lsn(self.p, n) };
                // SAFETY: as above.
                let last_lsn = unsafe { cdio_get_track_last_lsn(self.p, n) };
                if lsn == CDIO_INVALID_LSN || lsn < 0 {
                    continue;
                }
                // SAFETY: as above.
                let format = unsafe { cdio_get_track_format(self.p, n) };
                let is_audio = format == TRACK_FORMAT_AUDIO;
                let length = if last_lsn == CDIO_INVALID_LSN {
                    0
                } else {
                    (last_lsn - lsn + 1).max(0) as u32
                };
                tracks.push(TrackInfo {
                    number: n,
                    ctrl_addr: if is_audio { 0x01 } else { 0x41 },
                    is_audio,
                    start_fad: lsn as u32 + FAD_OFFSET,
                    length,
                });
                xa.push(format == TRACK_FORMAT_XA);
            }
            if tracks.is_empty() {
                return Err("libcdio TOC had no usable tracks".into());
            }
            // The lead-out's *starting* LSN — i.e. one past the last sector —
            // which is exactly the TOC lead-out address (FAD = LSN + 150). The
            // old `cdio_get_disc_last_lsn() + 1` relied on that function's
            // loosely documented "lsn of the end of the CD" (its drivers return
            // the lead-out LSN itself, which made the `+ 1` one sector late).
            // SAFETY: as above; 0xAA is libcdio's documented lead-out track.
            let lead_lsn = unsafe { cdio_get_track_lsn(self.p, CDIO_CDROM_LEADOUT_TRACK) };
            if lead_lsn == CDIO_INVALID_LSN || lead_lsn < 0 {
                return Err("libcdio returned no lead-out address".into());
            }
            Ok(Toc {
                tracks,
                xa,
                lead_out_fad: lead_lsn as u32 + FAD_OFFSET,
            })
        }

        /// Read one cooked 2048-byte Mode-1/2 data sector at `lsn`.
        pub fn read_mode1(&self, lsn: i32, out: &mut [u8]) -> bool {
            if out.len() < SECTOR_USER || lsn < 0 {
                return false;
            }
            // SAFETY: `self.p` is live (see `read_toc`); `out` is checked above to
            // hold ≥ 2048 bytes, exactly one cooked Mode-1 block (form2 = false).
            let rc =
                unsafe { cdio_read_mode1_sectors(self.p, out.as_mut_ptr().cast(), lsn, false, 1) };
            rc == DRIVER_OP_SUCCESS
        }

        /// Read the 2336-byte post-header payload of one Mode-2 sector at
        /// `lsn` (subheader ×2 + user data/EDC; `form2 = true` = no Form-1
        /// cooking). Needs raw `READ CD` access on a live drive.
        pub fn read_mode2_raw(&self, lsn: i32, out: &mut [u8]) -> bool {
            if out.len() < M2RAW || lsn < 0 {
                return false;
            }
            // SAFETY: `self.p` is live (see `read_toc`); `out` is checked above
            // to hold ≥ M2RAW (2336) bytes, the size libcdio writes for one
            // sector with `form2 = true`.
            let rc = unsafe { cdio_read_mode2_sector(self.p, out.as_mut_ptr().cast(), lsn, true) };
            rc == DRIVER_OP_SUCCESS
        }

        /// Read one raw 2352-byte Red Book audio sector at `lsn` (DAE).
        pub fn read_audio(&self, lsn: i32, out: &mut [u8]) -> bool {
            if out.len() < SECTOR_RAW || lsn < 0 {
                return false;
            }
            // SAFETY: `self.p` is live (see `read_toc`); `out` is checked above to
            // hold ≥ 2352 bytes, one raw Red Book frame.
            let rc = unsafe { cdio_read_audio_sectors(self.p, out.as_mut_ptr().cast(), lsn, 1) };
            rc == DRIVER_OP_SUCCESS
        }
    }

    impl Drop for Cdio {
        fn drop(&mut self) {
            if !self.p.is_null() {
                // SAFETY: `self.p` came from `cdio_open`, is non-NULL, and is
                // destroyed exactly once (here, as the sole owner).
                unsafe { cdio_destroy(self.p) };
            }
        }
    }
}
