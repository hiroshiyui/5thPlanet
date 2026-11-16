//! Host-side file conventions shared by the frontends (`jupiter`) and the
//! debugger (`sdbg`): where the battery-backed save files live, how a `--cart=`
//! spec maps to a [`Cartridge`], how a disc image is opened from disk, and a
//! crash-safe file write.
//!
//! None of this is emulation — it is the glue between the machine and the host
//! filesystem. It lives here (rather than in either binary) so the frontend and
//! the debugger can never drift apart: a debugger that looked for the battery at
//! a different path than the frontend wrote it would replay a recorded session
//! against a different machine state, silently breaking determinism.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::cartridge::Cartridge;
use crate::disc::Disc;

/// Largest single disc-image file we will read into memory. A full 80-minute
/// CD is 360 000 × 2352 B ≈ 847 MB; 1 GiB leaves headroom for over-burned
/// (90+ min) images while refusing to slurp an arbitrary huge file a CUE sheet
/// happens to name.
pub const MAX_DISC_FILE_BYTES: u64 = 1 << 30;

/// The internal backup RAM ("battery") file for a save base: the BIOS image
/// path with its extension replaced by `.bup` (e.g. `bios/US.bin` →
/// `bios/US.bup`). Keyed to the BIOS because the internal backup RAM is a
/// console-level resource shared by every game (ADR-0023).
pub fn battery_path(save_base: &Path) -> PathBuf {
    save_base.with_extension("bup")
}

/// The virtual memory-card file for backup-RAM cartridge card `card`:
/// `<save_base>.<card>.crtbup`, keyed to the BIOS like the internal `.bup`
/// (ADR-0028).
pub fn cart_card_path(save_base: &Path, card: u8) -> PathBuf {
    save_base.with_extension(format!("{card}.crtbup"))
}

/// Parse a `--cart=` spec into a [`Cartridge`]: `ram1m`/`ram4m` (Extension
/// DRAM), `bram[4|8|16|32]` (battery backup RAM, in Mbit; bare `bram` = 32),
/// or `rom:<path>` (a game ROM image read from disk).
pub fn parse_cart(spec: &str) -> Result<Cartridge, String> {
    if let Some(path) = spec.strip_prefix("rom:") {
        let bytes = read_capped(Path::new(path), MAX_DISC_FILE_BYTES)?;
        return Ok(Cartridge::rom(bytes));
    }
    match spec {
        "ram1m" => Ok(Cartridge::ext_ram_1mb()),
        "ram4m" => Ok(Cartridge::ext_ram_4mb()),
        "bram" | "bram32" => Ok(Cartridge::backup_ram(0x0040_0000)),
        "bram4" => Ok(Cartridge::backup_ram(0x0008_0000)),
        "bram8" => Ok(Cartridge::backup_ram(0x0010_0000)),
        "bram16" => Ok(Cartridge::backup_ram(0x0020_0000)),
        other => Err(format!(
            "unknown cart kind '{other}' (use ram1m / ram4m / bram[4|8|16|32] / rom:<path>)"
        )),
    }
}

/// Validate a `FILE "<name>"` entry of a CUE sheet before resolving it against
/// the sheet's directory. Only plain relative names (optionally in a
/// subdirectory) are accepted: an absolute path, a drive/UNC prefix, or any
/// `..` component would let a downloaded CUE sheet make us read an arbitrary
/// file elsewhere on the host. Both `/` and `\` count as separators so a
/// Windows-authored sheet is checked the same on every host.
pub fn validate_cue_file_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("empty FILE name".into());
    }
    let p = Path::new(name);
    let drive_prefix = name.len() >= 2 && name.as_bytes()[1] == b':';
    if p.is_absolute() || p.has_root() || name.starts_with(['/', '\\']) || drive_prefix {
        return Err(format!("FILE '{name}' is an absolute path"));
    }
    if name.split(['/', '\\']).any(|seg| seg == "..")
        || p.components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(format!("FILE '{name}' escapes the CUE sheet's directory"));
    }
    Ok(())
}

/// Read a whole file, refusing one larger than `cap` bytes (checked up front
/// from the metadata, and enforced while reading in case it grows).
pub fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let f = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = f
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if len > cap {
        return Err(format!(
            "{}: {len} bytes exceeds the {cap}-byte limit",
            path.display()
        ));
    }
    let mut buf = Vec::with_capacity(len as usize);
    f.take(cap + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if buf.len() as u64 > cap {
        return Err(format!("{}: exceeds the {cap}-byte limit", path.display()));
    }
    Ok(buf)
}

/// Load a disc image, picking the parser by file extension: `.iso` (raw
/// 2048-byte data track), `.cue` (CUE sheet + its track files, each validated
/// by [`validate_cue_file_name`]), or `.ccd` (CloneCD control file + sibling
/// `.img`). Every image file is size-capped at [`MAX_DISC_FILE_BYTES`].
pub fn load_image_disc(path: &Path) -> Result<Disc, String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "iso" => Ok(Disc::from_iso(read_capped(path, MAX_DISC_FILE_BYTES)?)),
        "cue" => {
            let cue = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            // `from_cue`'s loader reports only "missing"; keep the first real
            // reason so the user sees why a track was refused.
            let mut first_err: Option<String> = None;
            let res = Disc::from_cue(&cue, |name| {
                let r = validate_cue_file_name(name)
                    .and_then(|()| read_capped(&dir.join(name), MAX_DISC_FILE_BYTES));
                match r {
                    Ok(bytes) => Some(bytes),
                    Err(e) => {
                        first_err.get_or_insert(e);
                        None
                    }
                }
            });
            res.map_err(|e| match first_err {
                Some(why) => format!("{e} ({why})"),
                None => e,
            })
        }
        "ccd" => {
            let ccd = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let img = path.with_extension("img");
            Disc::from_ccd(&ccd, read_capped(&img, MAX_DISC_FILE_BYTES)?)
        }
        other => Err(format!(
            "unknown disc format '.{other}' (use .cue / .iso / .ccd)"
        )),
    }
}

/// Write `bytes` to `path` crash-safely: write a temporary sibling in the same
/// directory, flush it to disk, then atomically rename it over `path`. A crash
/// or power loss mid-write leaves either the old file or the new one — never a
/// truncated save (which a plain truncating `fs::write` can).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".tmp{}", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        // The workspace `tmp/` (gitignored; the project's temp-file convention).
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tmp/host_files_tests")
            .join(format!("{tag}-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_paths_key_to_the_bios_stem() {
        let base = Path::new("bios/Sega Saturn BIOS (USA).bin");
        assert_eq!(
            battery_path(base),
            Path::new("bios/Sega Saturn BIOS (USA).bup")
        );
        assert_eq!(
            cart_card_path(base, 2),
            Path::new("bios/Sega Saturn BIOS (USA).2.crtbup")
        );
    }

    #[test]
    fn parse_cart_accepts_every_documented_kind() {
        for spec in [
            "ram1m", "ram4m", "bram", "bram4", "bram8", "bram16", "bram32",
        ] {
            assert!(parse_cart(spec).is_ok(), "{spec}");
        }
        assert!(parse_cart("bram64").is_err());
        assert!(parse_cart("rom:/nonexistent/cart.bin").is_err());
    }

    #[test]
    fn cue_file_names_must_stay_inside_the_sheet_dir() {
        for ok in ["game.bin", "Track 01.bin", "sub/track02.bin", "a..b.bin"] {
            assert!(validate_cue_file_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "\\\\server\\share\\x.bin",
            "C:\\x.bin",
            "c:x.bin",
            "../x.bin",
            "sub/../../x.bin",
            "..\\x.bin",
            "..",
        ] {
            assert!(validate_cue_file_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn read_capped_refuses_oversized_files() {
        let d = scratch("cap");
        let p = d.join("f.bin");
        fs::write(&p, [0u8; 100]).unwrap();
        assert_eq!(read_capped(&p, 100).unwrap().len(), 100);
        assert!(read_capped(&p, 99).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cue_naming_an_escaping_file_is_rejected_with_the_reason() {
        let d = scratch("cue");
        let cue = d.join("g.cue");
        fs::write(
            &cue,
            "FILE \"../outside.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let err = load_image_disc(&cue).unwrap_err();
        assert!(err.contains("escapes"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn write_atomic_replaces_contents_and_leaves_no_temp() {
        let d = scratch("atomic");
        let p = d.join("save.bup");
        fs::write(&p, b"old-and-longer").unwrap();
        write_atomic(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
        let names: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names.len(), 1, "temp file left behind: {names:?}");
        let _ = fs::remove_dir_all(&d);
    }
}
