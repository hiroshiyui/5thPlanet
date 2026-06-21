//! CHD disc-image round-trip validation (roadmap G1).
//!
//! The pure decode logic is unit-tested inside `chd_image.rs`. This is the
//! end-to-end check that needs a *real* `.chd`: it decodes a CHD and the same
//! disc in its original CUE/ISO/CCD form and asserts the two [`Disc`]s are
//! byte-identical (image + TOC + per-track geometry). Because a real CHD isn't
//! committed, the test is `#[ignore]` and env-driven — point it at a pair:
//!
//! ```text
//! CHD_TEST="roms/SS - Boot Disc.chd" CHD_TEST_REF="roms/SS - Boot Disc.ccd" \
//!   cargo test -p saturn --features chd --test chd -- --ignored --nocapture
//! ```
//!
//! Generate a `.chd` from a CUE with `chdman createcd -i in.cue -o out.chd`
//! (chdman 0.287 does not ingest `.ccd` directly — feed it a `.cue`/`.toc`).
//! The kept fixtures in `roms/` (gitignored) are ready to point at.
#![cfg(feature = "chd")]

use saturn::chd_image::from_chd;
use saturn::disc::Disc;
use std::fs::File;
use std::path::Path;

/// Load the reference image by extension, mirroring the jupiter frontend.
fn load_ref(path: &str) -> Disc {
    let p = Path::new(path);
    let dir = p.parent().unwrap_or_else(|| Path::new("."));
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "iso" => Disc::from_iso(std::fs::read(p).unwrap()),
        "cue" => {
            let cue = std::fs::read_to_string(p).unwrap();
            Disc::from_cue(&cue, |name| std::fs::read(dir.join(name)).ok()).unwrap()
        }
        "ccd" => {
            let ccd = std::fs::read_to_string(p).unwrap();
            let img = std::fs::read(p.with_extension("img")).unwrap();
            Disc::from_ccd(&ccd, img).unwrap()
        }
        other => panic!("unsupported reference format .{other}"),
    }
}

#[test]
#[ignore = "needs a real .chd: CHD_TEST=<file.chd> CHD_TEST_REF=<file.cue|.iso|.ccd>"]
fn chd_matches_reference_image() {
    let chd_path = std::env::var("CHD_TEST").expect("set CHD_TEST=<file.chd>");
    let ref_path = std::env::var("CHD_TEST_REF").expect("set CHD_TEST_REF=<file.cue|.iso|.ccd>");

    let chd = from_chd(File::open(&chd_path).unwrap()).expect("decode CHD");
    let reference = load_ref(&ref_path);

    // TOC and per-track geometry must match exactly.
    assert_eq!(chd.first_track(), reference.first_track(), "first track");
    assert_eq!(chd.last_track(), reference.last_track(), "last track");
    assert_eq!(chd.lead_out_fad(), reference.lead_out_fad(), "lead-out FAD");
    assert_eq!(chd.toc(), reference.toc(), "408-byte TOC");

    // The decoded sector image must be byte-for-byte identical.
    assert_eq!(chd.image().len(), reference.image().len(), "image length");
    assert!(chd.image() == reference.image(), "image bytes differ");

    println!(
        "OK: {} tracks, {} bytes, lead-out FAD {} — CHD matches {}",
        chd.last_track() - chd.first_track() + 1,
        chd.image().len(),
        chd.lead_out_fad(),
        ref_path,
    );
}
