# Test fixtures

- **`audiocd.chd`** (294 bytes) — a CHD of a single 750-sector Red Book audio
  track of **pure silence** (all-zero PCM, so it compresses to almost nothing
  and carries no copyrightable content). Generated with
  `chdman createcd` from a silent CUE/BIN. Committed (unlike `roms/`, which is
  gitignored) so `tests/chd.rs` can cover `chd_image::from_chd` in CI without
  any commercial media. Regenerate from a silent CUE if ever needed.
