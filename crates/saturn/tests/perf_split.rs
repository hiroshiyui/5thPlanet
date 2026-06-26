//! Coarse perf split (ignored, manual): attribute gameplay wall-time to
//! CPU+SCSP (`run_for`) vs VDP rendering (`render_frame`). perf-sampling is
//! blocked in CI sandboxes; this is the portable fallback. Run with the game
//! disc:  CUE=<cue> cargo test --release -p saturn --test perf_split -- --ignored --nocapture
use saturn::Saturn;
use std::path::PathBuf;
use std::time::Instant;

const CYCLES_PER_FRAME: u64 = 479_151;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
#[ignore]
fn gameplay_cpu_vs_render_split() {
    let root = root();
    let bios_path = std::env::var("BIOS")
        .map(|p| root.join(p))
        .unwrap_or_else(|_| root.join("bios/Sega Saturn BIOS v1.01 (JAP).bin"));
    let Ok(bios) = std::fs::read(&bios_path) else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    sat.set_region(saturn::smpc::region::JAPAN);
    sat.set_rtc_unix(1_700_000_000);
    if let Ok(cue) = std::env::var("CUE") {
        let s = std::fs::read_to_string(root.join("roms").join(&cue)).unwrap();
        let d = saturn::disc::Disc::from_cue(&s, |n| std::fs::read(root.join("roms").join(n)).ok())
            .unwrap();
        sat.insert_disc(d);
    }
    let warm: u32 = std::env::var("WARM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900);
    let n: u32 = std::env::var("N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let mut fb = vec![0u8; saturn::vdp2::FRAMEBUFFER_BYTES];

    for _ in 0..warm {
        sat.run_frame(&mut fb);
    }

    let (mut cpu_ns, mut render_ns) = (0u128, 0u128);
    for _ in 0..n {
        let t0 = Instant::now();
        sat.run_for(CYCLES_PER_FRAME);
        cpu_ns += t0.elapsed().as_nanos();
        let t1 = Instant::now();
        let _ = saturn::vdp2::render_frame(&sat.bus.vdp2, Some(sat.bus.vdp1.display_fb()), &mut fb);
        render_ns += t1.elapsed().as_nanos();
    }
    let total = cpu_ns + render_ns;
    let frame_ms = total as f64 / n as f64 / 1e6;
    println!(
        "SPLIT over {n} gameplay frames (after {warm} warm):\n  \
         CPU+SCSP (run_for): {:.1}% ({:.2} ms/frame)\n  \
         VDP render:         {:.1}% ({:.2} ms/frame)\n  \
         total {:.2} ms/frame  |  real-time budget = 16.69 ms  |  speed = {:.0}%",
        cpu_ns as f64 / total as f64 * 100.0,
        cpu_ns as f64 / n as f64 / 1e6,
        render_ns as f64 / total as f64 * 100.0,
        render_ns as f64 / n as f64 / 1e6,
        frame_ms,
        16.69 / frame_ms * 100.0,
    );
}
