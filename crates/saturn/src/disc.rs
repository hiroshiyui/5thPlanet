//! CD-ROM disc image — tracks, TOC, and FAD-addressed sector reads.
//!
//! This is the *media* abstraction the CD-block ([`crate::cd_block`]) reads
//! from; it knows nothing about the host command protocol. A [`Disc`] is a set
//! of [`Track`]s over a single concatenated image buffer, addressed by **FAD**
//! (Frame ADdress): `FAD = LBA + 150`, the CD's 2-second lead-in offset, so the
//! first user sector is FAD 150. The Saturn CD-block speaks FAD throughout.
//!
//! Three image formats are parsed, all producing the same [`Disc`]:
//!
//! - **raw ISO** ([`Disc::from_iso`]) — one Mode-1 data track of 2048-byte
//!   sectors starting at FAD 150. The common single-data-track case.
//! - **CUE/BIN** ([`Disc::from_cue`]) — the `.cue` sheet lists tracks
//!   (`MODE1/2352`, `MODE2/2352`, `AUDIO`) with `INDEX 01` start times; the
//!   `.bin`(s) hold the raw sector bytes.
//! - **CloneCD CCD/IMG** ([`Disc::from_ccd`]) — the `.ccd` carries a full TOC
//!   (`[Entry]` points `0xA0`/`0xA1`/`0xA2` + per-track `PLBA`); the `.img`
//!   is raw 2352-byte sectors. This is what `roms/` ships for testing.
//!
//! Sector data is normalised to the 2048-byte user payload regardless of the
//! on-disc sector size (2352 raw sectors carry sync/header/EDC around it).
//!
//! The Saturn TOC layout ([`Disc::toc`]) follows MAME's `saturn_cd_hle.cpp`:
//! 102 four-byte entries — 99 track slots (unused = `0xFFFFFFFF`) then three
//! metadata entries (first track, last track, lead-out).

extern crate alloc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// FAD = LBA + this. The CD lead-in is 150 sectors (2 s at 75 Hz).
pub const FAD_OFFSET: u32 = 150;

/// User-data bytes per sector, the unit the CD-block buffers and transfers.
pub const SECTOR_USER: usize = 2048;

/// Track data type, which fixes where the 2048-byte user payload sits inside a
/// raw 2352-byte sector (and the `ctrl/adr` reported in status/TOC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackMode {
    /// Red Book audio (2352 bytes, all sample data).
    Audio,
    /// Mode 1 data: 12 sync + 4 header + 2048 user + 288 EDC/ECC.
    Mode1,
    /// Mode 2 (Form 1) data: 12 sync + 4 header + 8 subheader + 2048 user + …
    Mode2,
}

impl TrackMode {
    /// Byte offset of the 2048-byte user payload within a *raw 2352-byte*
    /// sector of this mode (irrelevant for 2048-byte cooked sectors).
    fn user_offset_2352(self) -> usize {
        match self {
            TrackMode::Audio => 0,
            TrackMode::Mode1 => 16, // sync(12) + header(4)
            TrackMode::Mode2 => 24, // sync(12) + header(4) + subheader(8)
        }
    }

    /// `ctrl/adr` nibble byte: control in the high nibble, ADR = 1 in the low.
    /// Data tracks report `0x41`, audio `0x01` — the value the BIOS keys on.
    fn ctrl_addr(self) -> u8 {
        match self {
            TrackMode::Audio => 0x01,
            _ => 0x41,
        }
    }
}

/// One track: where it lives in the image and its disc geometry.
#[derive(Clone, Debug)]
pub struct Track {
    /// 1-based track number.
    pub number: u8,
    pub mode: TrackMode,
    /// FAD of this track's first sector (`LBA + 150`).
    pub start_fad: u32,
    /// Length in sectors.
    pub length: u32,
    /// Byte offset of this track's first sector in [`Disc::image`].
    image_offset: usize,
    /// On-disc bytes per sector (2048 cooked, or 2352 raw).
    sector_size: usize,
}

impl Track {
    /// `(control << 4) | adr` for status reports and the TOC.
    pub fn ctrl_addr(&self) -> u8 {
        self.mode.ctrl_addr()
    }
}

/// A parsed disc image: the concatenated sector bytes plus the track table.
#[derive(Clone, Debug)]
pub struct Disc {
    image: Vec<u8>,
    tracks: Vec<Track>,
    /// FAD of the lead-out (one past the last user sector) = total LBA + 150.
    lead_out_fad: u32,
}

impl Disc {
    /// A raw ISO: a single Mode-1 data track of 2048-byte sectors at FAD 150.
    /// Trailing bytes that don't fill a sector are ignored.
    pub fn from_iso(image: Vec<u8>) -> Disc {
        let sectors = (image.len() / SECTOR_USER) as u32;
        let track = Track {
            number: 1,
            mode: TrackMode::Mode1,
            start_fad: FAD_OFFSET,
            length: sectors,
            image_offset: 0,
            sector_size: SECTOR_USER,
        };
        Disc {
            image,
            tracks: vec![track],
            lead_out_fad: FAD_OFFSET + sectors,
        }
    }

    /// Parse a CUE sheet. `cue` is the `.cue` text; `load` resolves each
    /// `FILE "name" BINARY` reference to its raw bytes. Tracks across multiple
    /// `FILE`s are concatenated into one image.
    pub fn from_cue(
        cue: &str,
        mut load: impl FnMut(&str) -> Option<Vec<u8>>,
    ) -> Result<Disc, String> {
        struct Pending {
            number: u8,
            mode: TrackMode,
            sector_size: usize,
            // (file_base in `image`, file length) of the FILE this track is in,
            // plus the INDEX-01 offset in *frames* relative to that file.
            file_base: usize,
            index01_frame: u32,
        }

        let mut image = Vec::new();
        let mut file_base = 0usize; // image offset where the current FILE starts
        let mut have_file = false;
        let mut pending: Vec<Pending> = Vec::new();
        let mut cur: Option<Pending> = None;

        for line in cue.lines() {
            let t = line.trim();
            let mut it = t.split_whitespace();
            match it.next().map(|s| s.to_ascii_uppercase()) {
                Some(kw) if kw == "FILE" => {
                    let name = cue_quoted(t).ok_or("FILE without a quoted name")?;
                    let bytes = load(&name).ok_or_else(|| alloc::format!("missing {name}"))?;
                    file_base = image.len();
                    image.extend_from_slice(&bytes);
                    have_file = true;
                }
                Some(kw) if kw == "TRACK" => {
                    if let Some(p) = cur.take() {
                        pending.push(p);
                    }
                    if !have_file {
                        return Err(String::from("TRACK before FILE"));
                    }
                    let number: u8 = it.next().and_then(|s| s.parse().ok()).ok_or("bad TRACK #")?;
                    let (mode, sector_size) = parse_track_type(it.next().unwrap_or(""))?;
                    cur = Some(Pending {
                        number,
                        mode,
                        sector_size,
                        file_base,
                        index01_frame: 0,
                    });
                }
                Some(kw) if kw == "INDEX" => {
                    let idx: u8 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0xFF);
                    if idx == 1 {
                        let frame = mmss_ff_to_frames(it.next().unwrap_or("")).ok_or("bad INDEX")?;
                        if let Some(p) = cur.as_mut() {
                            p.index01_frame = frame;
                        }
                    }
                }
                _ => {} // ignore PREGAP / REM / PERFORMER / etc.
            }
        }
        if let Some(p) = cur.take() {
            pending.push(p);
        }
        if pending.is_empty() {
            return Err(String::from("no tracks in CUE"));
        }
        Disc::from_pending_tracks(image, pending.into_iter().map(|p| {
            (p.number, p.mode, p.sector_size, p.file_base + p.index01_frame as usize * p.sector_size)
        }))
    }

    /// Parse a CloneCD `.ccd` + raw `.img`. The CCD's `[Entry]` blocks give a
    /// full TOC: points `0xA1` (last track) and `0xA2` (lead-out LBA), and per
    /// track the `PLBA` start. The `.img` is raw 2352-byte sectors.
    pub fn from_ccd(ccd: &str, img: Vec<u8>) -> Result<Disc, String> {
        // Collect [Entry] blocks. Each TOC point carries `Control`, `PLBA`
        // (position LBA), and `PMin`. For per-track points PLBA is the track
        // start; for the metadata points PMin holds the track *number* (0xA0 =
        // first, 0xA1 = last) and only 0xA2's PLBA (the lead-out) is a real LBA.
        struct Entry {
            point: u32,
            control: u32,
            plba: i64,
            pmin: i64,
        }
        let mut entries: Vec<Entry> = Vec::new();
        let (mut point, mut control, mut plba, mut pmin) = (None, 0u32, None, 0i64);
        let mut in_entry = false;
        let mut commit = |point: &mut Option<u32>, control, plba: &mut Option<i64>, pmin| {
            if let (Some(p), Some(l)) = (*point, *plba) {
                entries.push(Entry { point: p, control, plba: l, pmin });
            }
            *point = None;
            *plba = None;
        };
        for line in ccd.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                if in_entry {
                    commit(&mut point, control, &mut plba, pmin);
                }
                // Section headers are numbered: `[Entry 0]`, `[Entry 1]`, …
                in_entry = t.len() >= 6 && t[..6].eq_ignore_ascii_case("[Entry");
                control = 0;
                pmin = 0;
                continue;
            }
            if let Some((k, v)) = t.split_once('=') {
                let v = v.trim();
                match k.trim().to_ascii_uppercase().as_str() {
                    "POINT" => point = parse_int(v).map(|x| x as u32),
                    "CONTROL" => control = parse_int(v).unwrap_or(0) as u32,
                    "PLBA" => plba = parse_int(v),
                    "PMIN" => pmin = parse_int(v).unwrap_or(0),
                    _ => {}
                }
            }
        }
        if in_entry {
            commit(&mut point, control, &mut plba, pmin);
        }

        let n = entries
            .iter()
            .find(|e| e.point == 0xA1)
            .map(|e| e.pmin as u8)
            .ok_or("CCD: no point 0xA1 (last track)")?;
        let lead_out_lba = entries.iter().find(|e| e.point == 0xA2).map(|e| e.plba);

        let mut tracks: Vec<(u8, TrackMode, usize, usize)> = Vec::new();
        for tno in 1..=n {
            let (control, lba) = entries
                .iter()
                .find(|e| e.point == tno as u32)
                .map(|e| (e.control, e.plba))
                .ok_or_else(|| alloc::format!("CCD: missing track {tno}"))?;
            // CCD Control bit 2 (0x4) = data track.
            let mode = if control & 0x04 != 0 { TrackMode::Mode1 } else { TrackMode::Audio };
            tracks.push((tno, mode, 2352, lba.max(0) as usize * 2352));
        }
        let mut disc = Disc::from_pending_tracks(img, tracks.into_iter())?;
        if let Some(lo) = lead_out_lba {
            disc.lead_out_fad = (lo.max(0) as u32) + FAD_OFFSET;
        }
        Ok(disc)
    }

    /// Build a [`Disc`] from `(number, mode, sector_size, image_byte_offset)`
    /// tuples in track order, deriving each track's FAD/length from the next
    /// track's start (and the image end for the last).
    fn from_pending_tracks(
        image: Vec<u8>,
        tracks: impl Iterator<Item = (u8, TrackMode, usize, usize)>,
    ) -> Result<Disc, String> {
        let raw: Vec<_> = tracks.collect();
        if raw.is_empty() {
            return Err(String::from("no tracks"));
        }
        let mut out = Vec::with_capacity(raw.len());
        for (i, &(number, mode, sector_size, offset)) in raw.iter().enumerate() {
            // The track runs until the next track's image offset (or image end).
            let end = raw.get(i + 1).map(|n| n.3).unwrap_or(image.len());
            if offset > image.len() || end < offset {
                return Err(alloc::format!("track {number} offset {offset} past image"));
            }
            let length = ((end - offset) / sector_size) as u32;
            // First track starts at FAD 150; later tracks follow contiguously.
            let start_fad = FAD_OFFSET + (offset / sector_size) as u32;
            out.push(Track {
                number,
                mode,
                start_fad,
                length,
                image_offset: offset,
                sector_size,
            });
        }
        let last = out.last().unwrap();
        let lead_out_fad = last.start_fad + last.length;
        Ok(Disc {
            image,
            tracks: out,
            lead_out_fad,
        })
    }

    /// The 2048-byte user payload of the sector at `fad`, or `None` if `fad`
    /// is outside any track (e.g. the lead-out) or the image is short.
    pub fn read_sector(&self, fad: u32) -> Option<&[u8]> {
        let t = self.track_at_fad(fad)?;
        let rel = (fad - t.start_fad) as usize;
        let base = t.image_offset + rel * t.sector_size + t.mode.user_offset_2352() * (t.sector_size == 2352) as usize;
        self.image.get(base..base + SECTOR_USER)
    }

    /// The track containing `fad`, if any.
    pub fn track_at_fad(&self, fad: u32) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|t| fad >= t.start_fad && fad < t.start_fad + t.length)
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }
    pub fn first_track(&self) -> u8 {
        self.tracks.first().map_or(1, |t| t.number)
    }
    pub fn last_track(&self) -> u8 {
        self.tracks.last().map_or(1, |t| t.number)
    }
    pub fn lead_out_fad(&self) -> u32 {
        self.lead_out_fad
    }

    /// The Saturn TOC: 102 × 4-byte big-endian entries (408 bytes). 99 track
    /// slots (unused = `0xFFFFFFFF`) then first-track / last-track / lead-out
    /// metadata. Mirrors MAME `saturn_cd_hle.cpp::cd_readTOC`.
    pub fn toc(&self) -> [u8; 408] {
        let mut toc = [0xFFu8; 408];
        let mut first_ctrl = 0xFFu8;
        let mut last_ctrl = 0xFFu8;
        for t in &self.tracks {
            if t.number == 0 || t.number > 99 {
                continue;
            }
            let p = (t.number as usize - 1) * 4;
            let ca = t.ctrl_addr();
            toc[p] = ca;
            toc[p + 1] = (t.start_fad >> 16) as u8;
            toc[p + 2] = (t.start_fad >> 8) as u8;
            toc[p + 3] = t.start_fad as u8;
            if t.number == self.first_track() {
                first_ctrl = ca;
            }
            if t.number == self.last_track() {
                last_ctrl = ca;
            }
        }
        // Entry 99 — first track.
        toc[99 * 4] = first_ctrl;
        toc[99 * 4 + 1] = self.first_track();
        toc[99 * 4 + 2] = 0;
        toc[99 * 4 + 3] = 0;
        // Entry 100 — last track.
        toc[100 * 4] = last_ctrl;
        toc[100 * 4 + 1] = self.last_track();
        toc[100 * 4 + 2] = 0;
        toc[100 * 4 + 3] = 0;
        // Entry 101 — lead-out.
        toc[101 * 4] = first_ctrl;
        toc[101 * 4 + 1] = (self.lead_out_fad >> 16) as u8;
        toc[101 * 4 + 2] = (self.lead_out_fad >> 8) as u8;
        toc[101 * 4 + 3] = self.lead_out_fad as u8;
        toc
    }
}

/// Map a CUE `TRACK` type token to `(mode, on-disc sector size)`.
fn parse_track_type(tok: &str) -> Result<(TrackMode, usize), String> {
    match tok.to_ascii_uppercase().as_str() {
        "AUDIO" => Ok((TrackMode::Audio, 2352)),
        "MODE1/2048" => Ok((TrackMode::Mode1, 2048)),
        "MODE1/2352" => Ok((TrackMode::Mode1, 2352)),
        "MODE2/2336" => Ok((TrackMode::Mode2, 2336)),
        "MODE2/2352" => Ok((TrackMode::Mode2, 2352)),
        other => Err(alloc::format!("unsupported TRACK type {other}")),
    }
}

/// Extract the first double-quoted substring from a line (CUE `FILE "x" …`).
fn cue_quoted(line: &str) -> Option<String> {
    let a = line.find('"')?;
    let b = line[a + 1..].find('"')? + a + 1;
    Some(String::from(&line[a + 1..b]))
}

/// Parse `MM:SS:FF` (minutes:seconds:frames, 75 frames/s) to a frame count.
fn mmss_ff_to_frames(s: &str) -> Option<u32> {
    let mut p = s.split(':');
    let m: u32 = p.next()?.parse().ok()?;
    let sec: u32 = p.next()?.parse().ok()?;
    let f: u32 = p.next()?.parse().ok()?;
    Some((m * 60 + sec) * 75 + f)
}

/// Parse a decimal or `0x`-prefixed hex integer (CCD values are mixed).
fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_is_one_mode1_track_from_fad_150() {
        let img = vec![0u8; SECTOR_USER * 4]; // 4 sectors
        let d = Disc::from_iso(img);
        assert_eq!(d.tracks().len(), 1);
        assert_eq!(d.first_track(), 1);
        assert_eq!(d.last_track(), 1);
        assert_eq!(d.tracks()[0].start_fad, 150);
        assert_eq!(d.tracks()[0].length, 4);
        assert_eq!(d.lead_out_fad(), 154);
    }

    #[test]
    fn iso_read_sector_returns_2048_user_bytes_by_fad() {
        let mut img = vec![0u8; SECTOR_USER * 3];
        img[SECTOR_USER] = 0xAB; // first byte of sector 1 (FAD 151)
        let d = Disc::from_iso(img);
        assert_eq!(d.read_sector(150).unwrap().len(), 2048);
        assert_eq!(d.read_sector(151).unwrap()[0], 0xAB);
        assert!(d.read_sector(149).is_none(), "lead-in is not a sector");
        assert!(d.read_sector(153).is_none(), "lead-out is not a sector");
    }

    #[test]
    fn mode1_2352_user_payload_skips_the_16_byte_header() {
        // One MODE1/2352 track via CUE; user byte 0 sits at sector offset 16.
        let mut bin = vec![0u8; 2352 * 2];
        bin[16] = 0x5A; // sector 0 user[0]
        bin[2352 + 16] = 0x6B; // sector 1 user[0]
        let cue = "FILE \"game.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n";
        let d = Disc::from_cue(cue, |n| (n == "game.bin").then(|| bin.clone())).unwrap();
        assert_eq!(d.read_sector(150).unwrap()[0], 0x5A);
        assert_eq!(d.read_sector(151).unwrap()[0], 0x6B);
    }

    #[test]
    fn cue_multitrack_data_plus_audio_geometry() {
        // 2 data sectors then 3 audio sectors, single BIN, INDEX 01 at frame 2.
        let bin = vec![0u8; 2352 * 5];
        let cue = "\
FILE \"g.bin\" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 00:00:02
    INDEX 01 00:00:02
";
        let d = Disc::from_cue(cue, |_| Some(bin.clone())).unwrap();
        assert_eq!(d.tracks().len(), 2);
        assert_eq!(d.tracks()[0].mode, TrackMode::Mode1);
        assert_eq!(d.tracks()[0].start_fad, 150);
        assert_eq!(d.tracks()[0].length, 2);
        assert_eq!(d.tracks()[1].mode, TrackMode::Audio);
        assert_eq!(d.tracks()[1].start_fad, 152);
        assert_eq!(d.tracks()[1].length, 3);
        assert_eq!(d.tracks()[1].ctrl_addr(), 0x01);
        assert_eq!(d.lead_out_fad(), 155);
    }

    #[test]
    fn toc_matches_the_saturn_layout() {
        let bin = vec![0u8; 2352 * 5];
        let cue = "\
FILE \"g.bin\" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 00:00:02
";
        let d = Disc::from_cue(cue, |_| Some(bin.clone())).unwrap();
        let toc = d.toc();
        // Track 1: data (0x41), FAD 150 = 0x000096.
        assert_eq!(&toc[0..4], &[0x41, 0x00, 0x00, 0x96]);
        // Track 2: audio (0x01), FAD 152 = 0x000098.
        assert_eq!(&toc[4..8], &[0x01, 0x00, 0x00, 0x98]);
        // Track 3 slot is unused.
        assert_eq!(&toc[8..12], &[0xFF, 0xFF, 0xFF, 0xFF]);
        // Entry 99: first track (ctrl/adr of track 1, number 1).
        assert_eq!(&toc[396..400], &[0x41, 0x01, 0x00, 0x00]);
        // Entry 100: last track (ctrl/adr of track 2, number 2).
        assert_eq!(&toc[400..404], &[0x01, 0x02, 0x00, 0x00]);
        // Entry 101: lead-out FAD 155 = 0x00009B, ctrl/adr of first track.
        assert_eq!(&toc[404..408], &[0x41, 0x00, 0x00, 0x9B]);
    }

    #[test]
    fn ccd_img_parses_toc_and_lead_out() {
        // Minimal CCD: 1 data track at LBA 0, lead-out at LBA 4.
        let ccd = "\
[Entry 0]
Point=0xa1
Control=0x04
PMin=1
PLBA=44850
[Entry 1]
Point=0xa2
Control=0x04
PLBA=4
[Entry 2]
Point=0x01
Control=0x04
PLBA=0
";
        let img = vec![0u8; 2352 * 4];
        let d = Disc::from_ccd(ccd, img).unwrap();
        assert_eq!(d.first_track(), 1);
        assert_eq!(d.last_track(), 1);
        assert_eq!(d.tracks()[0].start_fad, 150);
        assert_eq!(d.lead_out_fad(), 154); // LBA 4 + 150
        assert_eq!(d.tracks()[0].ctrl_addr(), 0x41);
    }

    #[test]
    fn time_and_int_parsers() {
        assert_eq!(mmss_ff_to_frames("00:02:00"), Some(150));
        assert_eq!(mmss_ff_to_frames("01:00:00"), Some(4500));
        assert_eq!(parse_int("0xa1"), Some(0xA1));
        assert_eq!(parse_int("4350"), Some(4350));
        assert_eq!(parse_int("-150"), Some(-150));
    }
}
