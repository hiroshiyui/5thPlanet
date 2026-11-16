//! Crash-tolerant persistence of the battery-backed save memories: the internal
//! backup RAM (`<bios>.bup`) and the active backup-cartridge card
//! (`<bios>.<N>.crtbup`).
//!
//! Real hardware keeps these alive on a battery, so a game's save exists the
//! moment the game writes it. Persisting only at a clean exit (the old model)
//! lost every save made in a session that ended in a crash, a panic, or a
//! killed process. [`BatteryWriter`] instead remembers what it last wrote and
//! re-writes a file only when its *contents* (or its target path — a BIOS swap
//! or card switch re-keys it) changed; the frontend calls it every few seconds
//! and once more at shutdown. Writes are atomic
//! ([`saturn::host_files::write_atomic`]), so an interrupted write never
//! truncates the previous save.
//!
//! `sdl`-free and core-free (paths + byte slices only), so it is unit-tested
//! without a machine or a window.

// Compiled for tests in headless builds too; only the SDL frontend calls it.
#![cfg_attr(not(feature = "sdl-frontend"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use saturn::host_files::write_atomic;

/// How often the frontend checks the battery memories for changes. A check is
/// a 32 KiB (+ up to 4 MiB with a backup cart) compare, far below a frame.
pub const CHECK_PERIOD: Duration = Duration::from_secs(5);

/// The last image successfully persisted for one battery file.
#[derive(Default)]
struct Persisted {
    path: PathBuf,
    bytes: Vec<u8>,
}

/// Change-tracking writer for the two battery files. See the module docs.
pub struct BatteryWriter {
    bup: Option<Persisted>,
    card: Option<Persisted>,
    last_check: Instant,
}

impl Default for BatteryWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BatteryWriter {
    pub fn new() -> Self {
        Self {
            bup: None,
            card: None,
            last_check: Instant::now(),
        }
    }

    /// Whether [`CHECK_PERIOD`] has elapsed since the last check; resets the
    /// timer when it has.
    pub fn due(&mut self) -> bool {
        if self.last_check.elapsed() >= CHECK_PERIOD {
            self.last_check = Instant::now();
            true
        } else {
            false
        }
    }

    /// Record that `path` already holds `bytes` (just loaded from it), so an
    /// unchanged memory isn't immediately re-written.
    pub fn note_bup(&mut self, path: &Path, bytes: &[u8]) {
        self.bup = Some(Persisted {
            path: path.to_path_buf(),
            bytes: bytes.to_vec(),
        });
    }

    /// [`note_bup`](Self::note_bup) for the backup-cartridge card.
    pub fn note_card(&mut self, path: &Path, bytes: &[u8]) {
        self.card = Some(Persisted {
            path: path.to_path_buf(),
            bytes: bytes.to_vec(),
        });
    }

    /// Persist the internal backup RAM to `path` if it differs from what was
    /// last written there (`force` writes regardless). Returns whether a write
    /// happened.
    pub fn persist_bup(&mut self, path: &Path, bytes: &[u8], force: bool) -> bool {
        write_if_changed(&mut self.bup, path, bytes, force, "backup RAM")
    }

    /// Persist the backup-cartridge card to `path` if it changed (or `force`).
    pub fn persist_card(&mut self, path: &Path, bytes: &[u8], force: bool) -> bool {
        write_if_changed(&mut self.card, path, bytes, force, "backup cartridge")
    }
}

/// Write `bytes` to `path` unless `last` records that exact image at that exact
/// path. On success `last` is updated; on failure the error is logged and
/// `last` is left alone, so the next check retries.
fn write_if_changed(
    last: &mut Option<Persisted>,
    path: &Path,
    bytes: &[u8],
    force: bool,
    what: &str,
) -> bool {
    let unchanged = last
        .as_ref()
        .is_some_and(|p| p.path == path && p.bytes == bytes);
    if unchanged && !force {
        return false;
    }
    match write_atomic(path, bytes) {
        Ok(()) => {
            match last {
                // Reuse the allocation (the card image can be 4 MiB).
                Some(p) => {
                    p.path.clear();
                    p.path.push(path);
                    p.bytes.clear();
                    p.bytes.extend_from_slice(bytes);
                }
                None => {
                    *last = Some(Persisted {
                        path: path.to_path_buf(),
                        bytes: bytes.to_vec(),
                    })
                }
            }
            true
        }
        Err(e) => {
            eprintln!("failed to persist {what} to {}: {e}", path.display());
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tmp/battery_tests")
            .join(format!("{tag}-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_only_when_contents_or_path_change() {
        let d = scratch("change");
        let a = d.join("a.bup");
        let b = d.join("b.bup");
        let mut w = BatteryWriter::new();
        assert!(w.persist_bup(&a, b"one", false), "first write");
        assert!(!w.persist_bup(&a, b"one", false), "unchanged: skipped");
        assert!(w.persist_bup(&a, b"two", false), "changed contents");
        assert_eq!(fs::read(&a).unwrap(), b"two");
        assert!(w.persist_bup(&b, b"two", false), "re-keyed path");
        assert_eq!(fs::read(&b).unwrap(), b"two");
        assert!(w.persist_bup(&b, b"two", true), "force writes anyway");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_noted_load_is_not_rewritten_and_card_is_tracked_separately() {
        let d = scratch("note");
        let bup = d.join("x.bup");
        let card = d.join("x.0.crtbup");
        let mut w = BatteryWriter::new();
        w.note_bup(&bup, b"loaded");
        assert!(!w.persist_bup(&bup, b"loaded", false));
        assert!(
            !bup.exists(),
            "noted image is trusted to be on disk already"
        );
        assert!(w.persist_card(&card, b"card", false));
        assert!(!w.persist_card(&card, b"card", false));
        assert!(w.persist_bup(&bup, b"game saved", false));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_write_is_retried_next_time() {
        let d = scratch("fail");
        // A directory where the file should be: the rename onto it fails.
        let blocked = d.join("blocked.bup");
        fs::create_dir_all(&blocked).unwrap();
        let mut w = BatteryWriter::new();
        assert!(!w.persist_bup(&blocked, b"data", false));
        fs::remove_dir(&blocked).unwrap();
        assert!(
            w.persist_bup(&blocked, b"data", false),
            "retried after failure"
        );
        let _ = fs::remove_dir_all(&d);
    }
}
