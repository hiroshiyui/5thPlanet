//! CHD (Compressed Hunks of Data) CD-image decoding — roadmap **G1**.
//!
//! `.chd` is MAME's compressed, single-file, multi-track disc container — the
//! lossless equivalent of a CUE+BIN (data track *and* Red Book CD-DA tracks),
//! with the sectors stored as compressed "hunks". This module decodes a CD CHD
//! into a [`Disc`] by decompressing every hunk and concatenating each track's
//! raw 2352-byte sectors, then handing the assembled image to
//! [`Disc::from_pending_tracks`] — so the TOC, read paths, save-state
//! fingerprint, and the cdrdao byte-swap warning are all shared with the
//! CUE/CCD parsers, unchanged.
//!
//! **Feature-gated** (`chd`): unlike the libcdio FFI in `physdisc`, the `chd`
//! crate is pure Rust, so this needs no separate crate and no `unsafe` (the
//! workspace `unsafe_code = "forbid"` lint is per-crate; a dependency's
//! internal `unsafe` is irrelevant). `jupiter` enables the feature; headless
//! and core test builds stay lean without it.
//!
//! Layout reference: the chdman CD format stores each CD frame as
//! `CD_FRAME_SIZE` = 2352 sector bytes + 96 subcode bytes, packed into hunks;
//! tracks are concatenated with each track padded to a `CD_TRACK_PADDING`-frame
//! boundary. Per-track geometry comes from the `CHTR`/`CHT2` text metadata
//! entries. We keep the 2352-byte sector and drop the 96-byte subcode (the
//! CD-block synthesises subcode it needs).

use std::io::{Read, Seek};

use crate::disc::{Disc, TrackMode};

/// Sector bytes per CD frame (raw 2352).
const CD_SECTOR_SIZE: usize = 2352;
/// Subcode bytes per CD frame (we discard these).
const CD_SUBCODE_SIZE: usize = 96;
/// Total stored bytes per CD frame in a CHD.
const CD_FRAME_SIZE: usize = CD_SECTOR_SIZE + CD_SUBCODE_SIZE; // 2448
/// Tracks are padded so each starts on a multiple of this many frames.
const CD_TRACK_PADDING: u32 = 4;

/// A track as described by one CHD CD metadata entry (the fields we use).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChdTrack {
    number: u8,
    mode: TrackMode,
    /// Track length in frames/sectors (not counting any pregap padding).
    frames: u32,
    /// Leading pregap frames physically stored ahead of the track data, i.e.
    /// the chdman `PGTYPE` had a `V` ("in the file") prefix. Synthesised
    /// pregaps (no `V`) are not in the stream and contribute 0 here.
    pregap_in_file: u32,
}

/// Map a chdman track TYPE string to our [`TrackMode`]. chdman emits
/// `MODE1`/`MODE1_RAW`, `MODE2`/`MODE2_RAW`/`MODE2_FORM1`/`MODE2_FORM2`/
/// `MODE2_FORM_MIX`, `AUDIO`, and the rare `CDI/*`. We only need the user-data
/// layout, which is fixed by the family (Mode 1 / Mode 2 / Audio).
fn track_type_to_mode(ty: &str) -> Option<TrackMode> {
    if ty == "AUDIO" {
        Some(TrackMode::Audio)
    } else if ty.starts_with("MODE1") {
        Some(TrackMode::Mode1)
    } else if ty.starts_with("MODE2") || ty.starts_with("CDI") {
        Some(TrackMode::Mode2)
    } else {
        None
    }
}

/// Parse one `CHTR`/`CHT2` metadata value, e.g.
/// `"TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:12345"` (CHTR) or the longer
/// `"… FRAMES:n PREGAP:p PGTYPE:Vmode1 PGSUB:none POSTGAP:0"` (CHT2). Returns
/// `None` for a value we can't parse (caller treats that as an unsupported
/// CHD). Pure — unit-tested directly.
fn parse_track_metadata(value: &str) -> Option<ChdTrack> {
    // The value is space-separated `KEY:VALUE` tokens (NUL-terminated on disk;
    // trim any trailing NUL/whitespace first).
    let value = value.trim_end_matches('\0').trim();
    let mut number = None;
    let mut ty = None;
    let mut frames = None;
    let mut pregap = 0u32;
    let mut pgtype = "";
    for tok in value.split_whitespace() {
        let (k, v) = tok.split_once(':')?;
        match k {
            "TRACK" => number = v.parse().ok(),
            "TYPE" => ty = Some(v),
            "FRAMES" => frames = v.parse().ok(),
            "PREGAP" => pregap = v.parse().unwrap_or(0),
            "PGTYPE" => pgtype = v,
            _ => {} // SUBTYPE / PGSUB / POSTGAP — not needed for geometry
        }
    }
    // A `V` (any case) prefix on PGTYPE means the pregap frames are stored in
    // the CHD ahead of the track; otherwise the pregap is synthesised.
    let pregap_in_file = if pgtype.starts_with('V') || pgtype.starts_with('v') {
        pregap
    } else {
        0
    };
    Some(ChdTrack {
        number: number?,
        mode: track_type_to_mode(ty?)?,
        frames: frames?,
        pregap_in_file,
    })
}

/// Round `frames` up to the next multiple of [`CD_TRACK_PADDING`].
fn pad_to_track_boundary(frames: u32) -> u32 {
    frames.div_ceil(CD_TRACK_PADDING) * CD_TRACK_PADDING
}

/// Given the parsed tracks and the fully-decompressed CHD frame stream
/// (`raw`, a sequence of [`CD_FRAME_SIZE`] frames), build the concatenated
/// 2352-byte-sector image and the `(number, mode, sector_size, offset)` list
/// for [`Disc::from_pending_tracks`]. Pure over its inputs — unit-tested with a
/// synthetic frame stream. Each track's stored span (its `pregap_in_file` +
/// `frames`) is read from the CHD at a 4-frame-padded running offset; we keep
/// only the `frames` sectors after the in-file pregap.
fn assemble(tracks: &[ChdTrack], raw: &[u8]) -> Result<Disc, String> {
    let mut image = Vec::new();
    let mut pending: Vec<(u8, TrackMode, usize, usize)> = Vec::with_capacity(tracks.len());
    let mut chd_frame: usize = 0; // running frame offset into `raw`

    for t in tracks {
        // Skip any pregap frames physically stored ahead of the track data. The
        // frame counts come from (untrusted) CHD metadata, so do the offset math
        // in `usize` — a crafted `FRAMES`/`PREGAP` must yield a clean "runs past
        // the data" error, not a u32 wrap that bypasses the bounds check below.
        let data_start = chd_frame + t.pregap_in_file as usize;
        let need = (data_start + t.frames as usize) * CD_FRAME_SIZE;
        if need > raw.len() {
            return Err(format!(
                "CHD track {} runs past the decompressed data ({} > {} bytes)",
                t.number, need, raw.len()
            ));
        }
        let offset = image.len();
        // Copy this track's sectors: the first 2352 of each 2448-byte frame.
        for f in 0..t.frames as usize {
            let fbase = (data_start + f) * CD_FRAME_SIZE;
            image.extend_from_slice(&raw[fbase..fbase + CD_SECTOR_SIZE]);
        }
        pending.push((t.number, t.mode, CD_SECTOR_SIZE, offset));
        // Advance past this track's stored span, padded to a 4-frame boundary.
        // Safe in u32: this track passed the bounds check, so its frame span fits.
        chd_frame += pad_to_track_boundary(t.pregap_in_file + t.frames) as usize;
    }
    Disc::from_pending_tracks(image, pending.into_iter())
}

/// Decode a CD CHD from `reader` into a [`Disc`].
///
/// Errors (with a human-readable message) on: a non-CD CHD, an unsupported
/// metadata form (legacy `CHCD` binary or GD-ROM), a track TYPE we don't map,
/// or any decompression failure.
pub fn from_chd<R: Read + Seek>(reader: R) -> Result<Disc, String> {
    use chd::metadata::{KnownMetadata, Metadata, MetadataTag};

    let mut chd = chd::Chd::open(reader, None).map_err(|e| format!("CHD open failed: {e}"))?;

    // Collect metadata entries (each reads its value bytes).
    let metas: Vec<Metadata> = chd
        .metadata_refs()
        .try_into()
        .map_err(|e| format!("CHD metadata read failed: {e}"))?;

    // Parse CD track entries (`CHTR`/`CHT2`). Reject the legacy binary `CHCD`
    // and GD-ROM forms explicitly rather than silently mis-reading them.
    let cht2 = KnownMetadata::CdRomTrack2.metatag();
    let chtr = KnownMetadata::CdRomTrack.metatag();
    let mut tracks: Vec<ChdTrack> = Vec::new();
    let mut saw_unsupported_cd = false;
    for m in &metas {
        let tag = m.metatag();
        if tag == cht2 || tag == chtr {
            let text = core::str::from_utf8(&m.value)
                .map_err(|_| "CHD track metadata is not UTF-8".to_string())?;
            let t = parse_track_metadata(text)
                .ok_or_else(|| format!("unrecognised CHD track metadata: {text:?}"))?;
            tracks.push(t);
        } else if KnownMetadata::is_cdrom(tag) {
            saw_unsupported_cd = true; // CHCD (old) or GD-ROM
        }
    }

    if tracks.is_empty() {
        return Err(if saw_unsupported_cd {
            "CHD uses a legacy/GD-ROM CD format this build doesn't decode \
             (re-create it with a current chdman `createcd`)"
                .to_string()
        } else {
            "CHD has no CD tracks (is it a hard-disk or A/V CHD?)".to_string()
        });
    }
    tracks.sort_by_key(|t| t.number);

    // Decompress every hunk into one contiguous frame stream.
    let hunk_size = chd.header().hunk_size() as usize;
    let hunk_count = chd.header().hunk_count();
    let mut raw = Vec::with_capacity(hunk_count as usize * hunk_size);
    let mut out = chd.get_hunksized_buffer();
    let mut cmp = Vec::new();
    for i in 0..hunk_count {
        let mut hunk = chd.hunk(i).map_err(|e| format!("CHD hunk {i}: {e}"))?;
        hunk
            .read_hunk_in(&mut cmp, &mut out)
            .map_err(|e| format!("CHD hunk {i} decompress: {e}"))?;
        raw.extend_from_slice(&out);
    }

    assemble(&tracks, &raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chtr_minimal() {
        let t = parse_track_metadata("TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:12345").unwrap();
        assert_eq!(t.number, 1);
        assert_eq!(t.mode, TrackMode::Mode1);
        assert_eq!(t.frames, 12345);
        assert_eq!(t.pregap_in_file, 0);
    }

    #[test]
    fn parses_cht2_with_synthesised_pregap() {
        // No `V` prefix on PGTYPE → pregap is generated, not stored.
        let t = parse_track_metadata(
            "TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:1000 PREGAP:150 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0",
        )
        .unwrap();
        assert_eq!(t.mode, TrackMode::Audio);
        assert_eq!(t.frames, 1000);
        assert_eq!(t.pregap_in_file, 0);
    }

    #[test]
    fn parses_cht2_with_in_file_pregap() {
        // `V` prefix → pregap frames are stored in the CHD.
        let t = parse_track_metadata(
            "TRACK:3 TYPE:MODE2_RAW SUBTYPE:RW FRAMES:500 PREGAP:150 PGTYPE:VMODE1 PGSUB:NONE POSTGAP:0",
        )
        .unwrap();
        assert_eq!(t.mode, TrackMode::Mode2);
        assert_eq!(t.pregap_in_file, 150);
    }

    #[test]
    fn rejects_unknown_type() {
        assert!(parse_track_metadata("TRACK:1 TYPE:WEIRD FRAMES:10").is_none());
        assert!(parse_track_metadata("TYPE:AUDIO FRAMES:10").is_none()); // no TRACK
    }

    #[test]
    fn padding_rounds_up_to_four() {
        assert_eq!(pad_to_track_boundary(0), 0);
        assert_eq!(pad_to_track_boundary(1), 4);
        assert_eq!(pad_to_track_boundary(4), 4);
        assert_eq!(pad_to_track_boundary(5), 8);
        assert_eq!(pad_to_track_boundary(752), 752);
    }

    /// Assemble a synthetic two-track stream and confirm sectors land where the
    /// track table says, with the 4-frame padding between tracks skipped.
    #[test]
    fn assemble_two_tracks_skips_padding_and_subcode() {
        // Track 1: 2 data frames; padded to 4 frames in the stream.
        // Track 2: 3 audio frames.
        let tracks = [
            ChdTrack { number: 1, mode: TrackMode::Mode1, frames: 2, pregap_in_file: 0 },
            ChdTrack { number: 2, mode: TrackMode::Audio, frames: 3, pregap_in_file: 0 },
        ];
        // Build the raw frame stream: 4 (padded t1) + 3 (t2) = 7 frames.
        let total = 7;
        let mut raw = vec![0u8; total * CD_FRAME_SIZE];
        // Stamp each frame's first sector byte with a recognisable id, and the
        // subcode region with a sentinel that must NOT appear in the image.
        for f in 0..total {
            raw[f * CD_FRAME_SIZE] = (0xF0 | f) as u8; // sector marker
            raw[f * CD_FRAME_SIZE + CD_SECTOR_SIZE] = 0xAB; // subcode sentinel
        }
        let d = assemble(&tracks, &raw).unwrap();
        // Image holds 5 sectors (2 + 3), no padding frame, no subcode bytes.
        assert_eq!(d.image().len(), 5 * CD_SECTOR_SIZE);
        assert_eq!(d.image()[0], 0xF0); // track1 frame0
        assert_eq!(d.image()[CD_SECTOR_SIZE], 0xF1); // track1 frame1
        // Track2 starts after the 4-frame-padded track1 → stream frame 4.
        assert_eq!(d.image()[2 * CD_SECTOR_SIZE], 0xF4);
        assert_eq!(d.image()[4 * CD_SECTOR_SIZE], 0xF6);
        // No subcode sentinel leaked into the image.
        assert!(!d.image().contains(&0xAB));
        // Track geometry: 2 tracks, FAD-150 start, correct modes.
        assert_eq!(d.tracks().len(), 2);
        assert_eq!(d.first_track(), 1);
        assert_eq!(d.last_track(), 2);
    }

    /// A track that claims more frames than the decompressed data holds must be
    /// a clean error (not a panic / out-of-bounds) — guards the corrupt-CHD path
    /// and the `usize` bounds arithmetic in `assemble`.
    #[test]
    fn assemble_rejects_a_track_past_the_data() {
        // Claims 4 frames; only 1 frame of data is present.
        let tracks = [ChdTrack { number: 1, mode: TrackMode::Mode1, frames: 4, pregap_in_file: 0 }];
        let raw = vec![0u8; CD_FRAME_SIZE];
        let err = assemble(&tracks, &raw).unwrap_err();
        assert!(err.contains("runs past"), "expected a bounds error, got: {err}");
    }
}
