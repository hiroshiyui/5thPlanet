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
//!
//! Build with it: `cargo build -p physdisc --features libcdio` (needs
//! `libcdio-dev` / `pkgconf libcdio`), or via the frontend's `physical-disc`
//! feature.

// Several constants/helpers are used only on the `libcdio` path; in the default
// stub build they're intentionally unused.
#![cfg_attr(not(feature = "libcdio"), allow(dead_code))]

use saturn::disc::{SectorSource, TrackInfo};

/// FAD (Frame ADdress) = absolute LBA/LSN + 150 (the 2-second lead-in).
const FAD_OFFSET: u32 = 150;
const SECTOR_USER: usize = 2048;
const SECTOR_RAW: usize = 2352;

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
    /// Cached track table (number, ctrl/addr, audio?, start FAD, length).
    tracks: Vec<TrackInfo>,
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
        let (tracks, lead_out_fad) = inner.read_toc()?;
        let fingerprint = toc_fingerprint(&tracks, lead_out_fad);
        Ok(Self {
            inner,
            dev_file: open_block_device(device),
            tracks,
            lead_out_fad,
            fingerprint,
        })
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

    fn subheader(&self, _fad: u32) -> Option<(u8, u8, u8, u8)> {
        // Mode-2 subheader filtering isn't reconstructed from a live drive's
        // cooked reads; games that boot don't rely on it. (Image-based discs
        // still expose it via `saturn::disc::Disc`.)
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
        let dir = std::env::temp_dir();
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
mod ffi {
    use super::{TrackInfo, FAD_OFFSET, SECTOR_RAW, SECTOR_USER};
    use core::ffi::{c_char, c_int, c_void, CStr};
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
        fn cdio_get_disc_last_lsn(p: *const CdIoT) -> i32;
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
    }

    const TRACK_FORMAT_AUDIO: c_int = 0;
    const DRIVER_OP_SUCCESS: c_int = 0;
    const CDIO_INVALID_LSN: i32 = -1;

    /// A safe owner of a libcdio handle.
    pub struct Cdio {
        p: *mut CdIoT,
    }

    // The handle is used single-threaded behind `&self` reads; the bus is
    // `Send` but never shares the CD-block across threads concurrently.
    unsafe impl Send for Cdio {}

    impl Cdio {
        /// Open the named optical device (e.g. `/dev/sr0`); `Err` if libcdio
        /// can't probe it. `driver_id 0` lets libcdio auto-detect the backend.
        pub fn open(device: &str) -> Result<Self, String> {
            let c = CString::new(device).map_err(|_| "device path has a NUL byte".to_string())?;
            // driver_id 0 = DRIVER_UNKNOWN → libcdio auto-detects.
            let p = unsafe { cdio_open(c.as_ptr(), 0) };
            if p.is_null() {
                let _ = unsafe { CStr::from_ptr(c.as_ptr()) }; // keep `c` alive
                return Err(format!("libcdio could not open '{device}'"));
            }
            Ok(Self { p })
        }

        /// Read the TOC into our [`TrackInfo`] table + lead-out FAD.
        pub fn read_toc(&self) -> Result<(Vec<TrackInfo>, u32), String> {
            let first = unsafe { cdio_get_first_track_num(self.p) };
            let last = unsafe { cdio_get_last_track_num(self.p) };
            if first == 0 || first == 0xFF || last == 0xFF || last < first {
                return Err("libcdio returned no readable TOC".into());
            }
            let mut tracks = Vec::new();
            for n in first..=last {
                let lsn = unsafe { cdio_get_track_lsn(self.p, n) };
                let last_lsn = unsafe { cdio_get_track_last_lsn(self.p, n) };
                if lsn == CDIO_INVALID_LSN {
                    continue;
                }
                let is_audio = unsafe { cdio_get_track_format(self.p, n) } == TRACK_FORMAT_AUDIO;
                let length = (last_lsn - lsn + 1).max(0) as u32;
                tracks.push(TrackInfo {
                    number: n,
                    ctrl_addr: if is_audio { 0x01 } else { 0x41 },
                    is_audio,
                    start_fad: lsn as u32 + FAD_OFFSET,
                    length,
                });
            }
            if tracks.is_empty() {
                return Err("libcdio TOC had no usable tracks".into());
            }
            let lead_lsn = unsafe { cdio_get_disc_last_lsn(self.p) };
            let lead_out_fad = lead_lsn as u32 + FAD_OFFSET + 1;
            Ok((tracks, lead_out_fad))
        }

        /// Read one cooked 2048-byte Mode-1/2 data sector at `lsn`.
        pub fn read_mode1(&self, lsn: i32, out: &mut [u8]) -> bool {
            if out.len() < SECTOR_USER || lsn < 0 {
                return false;
            }
            let rc =
                unsafe { cdio_read_mode1_sectors(self.p, out.as_mut_ptr().cast(), lsn, false, 1) };
            rc == DRIVER_OP_SUCCESS
        }

        /// Read one raw 2352-byte Red Book audio sector at `lsn` (DAE).
        pub fn read_audio(&self, lsn: i32, out: &mut [u8]) -> bool {
            if out.len() < SECTOR_RAW || lsn < 0 {
                return false;
            }
            let rc = unsafe { cdio_read_audio_sectors(self.p, out.as_mut_ptr().cast(), lsn, 1) };
            rc == DRIVER_OP_SUCCESS
        }
    }

    impl Drop for Cdio {
        fn drop(&mut self) {
            if !self.p.is_null() {
                unsafe { cdio_destroy(self.p) };
            }
        }
    }
}
