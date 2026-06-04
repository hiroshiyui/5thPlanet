//! Temporary BIOS-boot tracer for the M4 iterate-to-splash loop.
//!
//! NOT a regression test — `#[ignore]`d so it never runs in CI. Run it
//! manually to see where the master SH-2 parks during BIOS init:
//!
//! ```sh
//! cargo test -p saturn --test trace_boot -- --ignored --nocapture
//! ```
//!
//! Delete this file once the splash renders (M4 task #6).

use std::path::PathBuf;

use saturn::Saturn;
use saturn::vdp2::FRAMEBUFFER_BYTES;
use sh2::bus::Bus;

const BIOS_CANDIDATES: &[&str] = &[
    "bios/Sega Saturn BIOS (USA).bin",
    "bios/Sega Saturn BIOS (EUR).bin",
    "bios/Sega Saturn BIOS v1.01 (JAP).bin",
    "bios/Sega Saturn BIOS v1.00 (JAP).bin",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn load_bios() -> Option<(Vec<u8>, PathBuf)> {
    let root = workspace_root();
    for rel in BIOS_CANDIDATES {
        let p = root.join(rel);
        if let Ok(bytes) = std::fs::read(&p) {
            return Some((bytes, p));
        }
    }
    None
}

#[test]
#[ignore = "manual: generate per-instruction master PC trace for reference diff"]
fn gen_master_pc_trace() {
    use std::io::Write;
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let n: u64 = std::env::var("PCTRACE_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000_000);
    let vblank = std::env::var("PCTRACE_VBLANK").is_ok();
    // Drain granularity: 1 = drain after every instruction (matches the
    // earlier debug runs); 256 = drain every ~256 cycles, matching
    // run_for/run_frame's batched draining.
    let drain_interval: u64 = std::env::var("PCTRACE_DRAIN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let f = std::fs::File::create("/tmp/our_pc.log").expect("create trace");
    let mut w = std::io::BufWriter::new(f);
    let mut frame_cyc: u64 = 0;
    let mut vblank_raised = false;
    let mut drain_cyc: u64 = 0;
    for _ in 0..n {
        // Skip delay-slot PCs to match reference emulators (Yabause)
        // that execute the slot inside the branch handler and don't
        // log it as a separate instruction.
        if !sat.master().next_is_delay_slot() {
            let pc = sat.master().regs.pc;
            writeln!(w, "{pc:08X}").unwrap();
        }
        let c = sat.debug_step_master_nodrain() as u64;
        drain_cyc += c;
        if drain_cyc >= drain_interval {
            sat.debug_drain();
            drain_cyc = 0;
        }
        if vblank {
            // Mirror run_frame's VBlank cadence at instruction granularity.
            frame_cyc += c;
            if !vblank_raised && frame_cyc >= 453_085 {
                let t = sat.bus.vdp2.regs.read16(0x004);
                sat.bus.vdp2.regs.write16(0x004, t | 0x0008);
                sat.bus.scu.raise(saturn::ScuSource::VBlankIn);
                vblank_raised = true;
            }
            if frame_cyc >= 476_932 {
                let t = sat.bus.vdp2.regs.read16(0x004);
                sat.bus.vdp2.regs.write16(0x004, t & !0x0008);
                frame_cyc -= 476_932;
                vblank_raised = false;
            }
        }
    }
    w.flush().unwrap();
    println!("wrote {n} master PCs to /tmp/our_pc.log");
}

/// M11: per-instruction master PC trace of the **VF2 boot**, for a reference
/// diff against MAME's `maincpu` trace (to pinpoint the give-up divergence —
/// the BIOS recognizes the disc, shows the license screen, then falls to the
/// CD shell instead of loading the game). Replicates the frontend's boot
/// conditions: JP v1.01 BIOS + its `.bup` clock state + Japan region + the full
/// VF2 disc image. Logs EVERY PC (no delay-slot skip — MAME logs delay slots),
/// raising VBlank at run_frame's cadence so the interrupt-driven boot matches.
///
/// ```sh
/// PCTRACE_N=40000000 PCTRACE_OUT=/tmp/our_vf2_pc.log \
///   cargo test -p saturn --test trace_boot -- --ignored --nocapture gen_vf2_pc_trace
/// ```
/// Then capture MAME's side (bounded to ~30M instrs to catch the first
/// divergence): `mameref/saturn saturnjp -bios 101 -rompath mameref/roms \
///   -cdrom roms/vf2_full.cue -debug`, console `trace vf2.tr,maincpu` then `go`,
/// stop after ~10 s, and diff the PC columns to the first sustained divergence.
/// M11 perf probe: time each `run_frame` of the VF2 boot to find frames that
/// overrun the 16.6 ms vsync budget (which the SDL frontend shows as an
/// unstable framerate), and report the master PC on the slowest frames so the
/// heavy work can be identified. `run_frame` here renders the VDP2 frame just
/// like the frontend, so the timing reflects what the user sees.
///
/// ```sh
/// FRAMES=280 cargo test -p saturn --test trace_boot -- \
///   --ignored --nocapture --exact frame_timing
/// ```
#[test]
#[ignore = "manual: per-frame run_frame timing for the VF2 boot (M11 perf)"]
fn frame_timing() {
    use saturn::vdp2::FRAMEBUFFER_BYTES;
    use std::time::Instant;
    let root = workspace_root();
    let bios_path = root.join("bios/Sega Saturn BIOS v1.01 (JAP).bin");
    let Ok(bios) = std::fs::read(&bios_path) else {
        println!("no JP BIOS; skipped");
        return;
    };
    // CUE=<name> overrides the disc image (default VF2), so the same boot probe
    // can be pointed at other commercial fixtures.
    let cue_name = std::env::var("CUE").unwrap_or_else(|_| "vf2_full.cue".into());
    let cue_path = root.join("roms").join(&cue_name);
    let Ok(cue) = std::fs::read_to_string(&cue_path) else {
        println!("no roms/{cue_name}; skipped");
        return;
    };
    let disc = match saturn::disc::Disc::from_cue(&cue, |name| {
        std::fs::read(root.join("roms").join(name)).ok()
    }) {
        Ok(d) => d,
        Err(e) => {
            println!("cue parse failed: {e}");
            return;
        }
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    sat.set_region(saturn::smpc::region::JAPAN);
    if let Ok(bup) = std::fs::read(root.join("bios/Sega Saturn BIOS v1.01 (JAP).bup")) {
        sat.load_internal_backup(&bup);
    }
    sat.set_rtc_unix(1_700_000_000);
    sat.insert_disc(disc);

    let frames: u32 = std::env::var("FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(280);
    // RENDER=0 times emulation only (run_for, no VDP2 composite) to isolate the
    // render cost from the emulation cost.
    let render = std::env::var("RENDER").map(|s| s != "0").unwrap_or(true);
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    let budget_ms = 1000.0 / 60.0; // 16.67 ms
    let mut total_ms = 0.0f64;
    let mut max_ms = 0.0f64;
    let mut overruns = 0u32;
    // Slowest frames: (ms, frame, master pc).
    let mut slow: Vec<(f64, u32, u32)> = Vec::new();
    for f in 0..frames {
        let t = Instant::now();
        if render {
            sat.run_frame(&mut fb);
        } else {
            sat.run_for(479_151);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let pc = sat.master().regs.pc;
        total_ms += ms;
        if ms > max_ms {
            max_ms = ms;
        }
        if ms > budget_ms {
            overruns += 1;
        }
        slow.push((ms, f, pc));
    }
    slow.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!(
        "frames={frames} avg={:.2}ms ({:.0} fps) max={max_ms:.2}ms overruns(>{budget_ms:.1}ms)={overruns}",
        total_ms / frames as f64,
        1000.0 / (total_ms / frames as f64),
    );
    println!("slowest 15 frames (ms, frame#, master PC):");
    for (ms, f, pc) in slow.iter().take(15) {
        println!("  {ms:7.2}ms  frame {f:>4}  pc=0x{pc:08X}");
    }
}

#[test]
#[ignore = "manual: VF2-boot master PC trace for the MAME reference diff (M11)"]
fn gen_vf2_pc_trace() {
    use std::io::Write;
    let root = workspace_root();
    let bios_path = root.join("bios/Sega Saturn BIOS v1.01 (JAP).bin");
    let Ok(bios) = std::fs::read(&bios_path) else {
        println!("no JP BIOS at {}; skipped", bios_path.display());
        return;
    };
    let cue_path = root.join("roms/vf2_full.cue");
    let Ok(cue) = std::fs::read_to_string(&cue_path) else {
        println!("no {}; skipped (copyrighted fixture)", cue_path.display());
        return;
    };
    let disc = match saturn::disc::Disc::from_cue(&cue, |name| {
        std::fs::read(root.join("roms").join(name)).ok()
    }) {
        Ok(d) => d,
        Err(e) => {
            println!("cue parse failed: {e}");
            return;
        }
    };

    let mut sat = Saturn::new(bios);
    sat.reset();
    sat.set_region(saturn::smpc::region::JAPAN);
    // Match the frontend: a charged battery (clock set, so the JP BIOS skips
    // the clock-set screen and boots the disc) + a fixed RTC.
    if let Ok(bup) = std::fs::read(root.join("bios/Sega Saturn BIOS v1.01 (JAP).bup")) {
        sat.load_internal_backup(&bup);
    }
    sat.set_rtc_unix(1_700_000_000);
    sat.insert_disc(disc);

    let n: u64 = std::env::var("PCTRACE_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40_000_000);
    let frames: u64 = std::env::var("PCTRACE_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let out = std::env::var("PCTRACE_OUT").unwrap_or_else(|_| "/tmp/our_vf2_pc.log".into());
    let f = std::fs::File::create(&out).expect("create trace");
    let mut w = std::io::BufWriter::new(f);
    // Trace through the REAL run path — `run_for_traced` drives the full
    // scheduler (master + slave + CD-block) through the aligned `batch_size`
    // (event-edge-clamped batching) and the complete peripheral drain set
    // (incl. VBlank-OUT) — so the trace reflects the Mednafen-aligned interrupt
    // timing, not the old hand-rolled single-step cadence. Per-frame chunked so
    // the PC buffer stays bounded over a long boot. Loop-collapse to mirror
    // Mednafen's SS_LogMasterPC: suppress a PC already seen in the last 64
    // logged (so idle/delay/poll spins log one pass, not their 10k+ iterations).
    // Set PCTRACE_DELAYSLOTS=1 for parity with Mednafen (it logs every Step,
    // delay slots included). NB: Mednafen logs the fetch-PC = our exec-PC + 4.
    let mut recent: std::collections::VecDeque<u32> = std::collections::VecDeque::with_capacity(64);
    let mut logged: u64 = 0;
    // PCTRACE_LO: only log PCs >= this, applied BEFORE the loop-collapse window
    // (matching Mednafen's SS_PCTRACE_LO, which returns before its recent-64
    // check). Set PCTRACE_LO=06000000 to keep only work-RAM execution and skip
    // the BIOS-ROM init noise, so the give-up divergence in the CD-boot loader
    // stands out.
    let lo: u32 = std::env::var("PCTRACE_LO")
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(0);
    // PCTRACE_HI: only log PCs <= this (matching Mednafen's SS_PCTRACE_HI). With
    // LO this is a *range* filter — set LO=06020000 HI=0602FFFF to keep only the
    // work-RAM CD-boot loader (0x0602xxxx) and drop the recognition-poll bulk
    // (0x0601xxxx) and the cache-through BIOS (0x20xxxxxx Mednafen logs but we
    // don't), so the post-Play give-up divergence stands out and the entry cap
    // isn't spent on noise.
    let hi: u32 = std::env::var("PCTRACE_HI")
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(u32::MAX);
    let mut pcs: Vec<u32> = Vec::with_capacity(8_000_000);
    'outer: for _ in 0..frames {
        pcs.clear();
        sat.run_for_traced(479_151, &mut pcs);
        for &pc in &pcs {
            if pc < lo || pc > hi {
                continue;
            }
            if !recent.contains(&pc) {
                writeln!(w, "{pc:08X}").unwrap();
                if recent.len() == 64 {
                    recent.pop_front();
                }
                recent.push_back(pc);
                logged += 1;
                if logged >= n {
                    break 'outer;
                }
            }
        }
    }
    w.flush().unwrap();
    println!("wrote {logged} VF2-boot master PCs (run_for_traced path) to {out}");
}

/// M11: stop AT the VF2 CD-boot give-up branch and dump the live loader code +
/// the work-RAM state words it branches on, so we can compare the *decision*
/// against Mednafen (whose full PC trace crashes headless here, but whose
/// SS_WWATCH on the same work-RAM word works). Uses the no-render `run_for` +
/// a master breakpoint (fast — fits the harness runtime budget), unlike SAT_FBP
/// which renders every frame.
///
/// ```sh
/// GIVEUP_PC=0x06028106 cargo test -p saturn --test trace_boot -- \
///   --ignored --nocapture dump_giveup_state
/// ```
#[test]
#[ignore = "manual: dump the VF2 CD-boot give-up branch + loader state (M11)"]
fn dump_giveup_state() {
    use sh2::bus::{AccessKind, Bus};
    let root = workspace_root();
    let bios_path = root.join("bios/Sega Saturn BIOS v1.01 (JAP).bin");
    let Ok(bios) = std::fs::read(&bios_path) else {
        println!("no JP BIOS; skipped");
        return;
    };
    // CUE=<name> overrides the disc image (default VF2), so the same boot probe
    // can be pointed at other commercial fixtures.
    let cue_name = std::env::var("CUE").unwrap_or_else(|_| "vf2_full.cue".into());
    let cue_path = root.join("roms").join(&cue_name);
    let Ok(cue) = std::fs::read_to_string(&cue_path) else {
        println!("no roms/{cue_name}; skipped");
        return;
    };
    let disc = match saturn::disc::Disc::from_cue(&cue, |name| {
        std::fs::read(root.join("roms").join(name)).ok()
    }) {
        Ok(d) => d,
        Err(e) => {
            println!("cue parse failed: {e}");
            return;
        }
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    sat.set_region(saturn::smpc::region::JAPAN);
    if let Ok(bup) = std::fs::read(root.join("bios/Sega Saturn BIOS v1.01 (JAP).bup")) {
        sat.load_internal_backup(&bup);
    }
    sat.set_rtc_unix(1_700_000_000);
    sat.insert_disc(disc);
    // Record the CD command stream (off in normal runs) so we can dump the
    // post-Play sequence the loader's drive-status check depends on.
    sat.bus.cd_block.cmd_log_on = true;

    let giveup: u32 = std::env::var("GIVEUP_PC")
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x0602_8106);
    sat.set_master_bp(giveup);

    // Run headless (no render) in 1-frame chunks until the give-up fires.
    // FRAMES caps the run (default 400; keep ≤~700 so a single test run fits the
    // harness ~8s budget).
    let frames: u32 = std::env::var("FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(400);
    let mut hit = None;
    for f in 0..frames {
        sat.run_for(479_151);
        if let Some(h) = sat.take_master_bp_hit() {
            println!("give-up 0x{giveup:08X} hit at frame {f}");
            hit = Some(h);
            break;
        }
    }
    // CD command stream up to here (post-Play window is the tail). Decode the
    // command name, the input CR1 (carries the command + top FAD byte) and the
    // reported status (CR1-out high byte) so we can see PLAY→…→ the drive state
    // the loader reads. CMD_LOG_TAIL limits how many trailing entries to show.
    let tail: usize = std::env::var("CMD_LOG_TAIL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let name = |c: u8| -> &'static str {
        match c {
            0x00 => "GetStatus",
            0x01 => "GetHwInfo",
            0x02 => "GetToc",
            0x03 => "GetSession",
            0x04 => "Init",
            0x06 => "EndDataXfer",
            0x10 => "Play",
            0x11 => "Seek",
            0x12 => "Scan",
            0x20 => "GetSubcodeQ",
            0x30 => "SetDevConn",
            0x31 => "GetDevConn",
            0x40 => "SetFilterRange",
            0x42 => "SetFilterSubhdr",
            0x44 => "SetFilterMode",
            0x46 => "SetFilterConn",
            0x48 => "ResetSelector",
            0x50 => "GetBufSize",
            0x51 => "GetBufStat",
            0x52 => "CalcActualSize",
            0x60 => "SetSectorLen",
            0x61 => "GetSectorData",
            0x62 => "GetThenDelSector",
            0x63 => "GetSectorInfo",
            0x67 => "GetCopyError",
            0x70 => "ChangeDir",
            0x71 => "ReadDir",
            0x72 => "GetFileScope",
            0x73 => "GetFileInfo",
            0x74 => "ReadFile",
            0x75 => "AbortFile",
            0xE0 => "Auth",
            0xE1 => "GetDiscRegion",
            _ => "?",
        }
    };
    let log = std::mem::take(&mut sat.bus.cd_block.cmd_log);
    println!(
        "\n=== CD command stream (last {tail} of {}) — [idx] caller cmd  CR_in -> CR_out  HIRQ_in->out st ===",
        log.len()
    );
    let start = log.len().saturating_sub(tail);
    for (i, e) in log.iter().enumerate().skip(start) {
        println!(
            "  [{i:>3}] @{:08X} {:02X} {:<15} in={:04X},{:04X},{:04X},{:04X} -> out={:04X},{:04X},{:04X},{:04X}  HIRQ {:04X}->{:04X} st={:02X}",
            e.caller_pc,
            e.cmd,
            name(e.cmd),
            e.cr_in[0],
            e.cr_in[1],
            e.cr_in[2],
            e.cr_in[3],
            e.cr_out[0],
            e.cr_out[1],
            e.cr_out[2],
            e.cr_out[3],
            e.hirq_in,
            e.hirq_out,
            e.status,
        );
    }

    // Scan low + high WRAM for a byte string — MEMSCAN="SEGA SEGASATURN"
    // confirms the transferred IP.BIN landed in work RAM intact. IP.BIN reaches
    // WRAM via GetThenDelete → the 32-bit data port (0x05818000) → SCU-DMA, a
    // path distinct from the auth command's direct CD-block read, so a correct
    // auth doesn't prove a correct transfer. Runs regardless of the BP, so a
    // never-hit GIVEUP_PC + a chosen FRAMES scans at an arbitrary frame.
    if let Ok(needle) = std::env::var("MEMSCAN") {
        let pat = needle.as_bytes();
        println!("\n=== MEMSCAN {needle:?} (low + high WRAM) ===");
        let mut found = 0;
        for (lo, hi) in [(0x0020_0000u32, 0x0030_0000u32), (0x0600_0000, 0x0610_0000)] {
            let mut a = lo;
            while a < hi && found < 24 {
                if sat.bus.read8(a, AccessKind::Data).0 == pat[0]
                    && pat
                        .iter()
                        .enumerate()
                        .all(|(k, &pb)| sat.bus.read8(a + k as u32, AccessKind::Data).0 == pb)
                {
                    let ctx: String = (0..48)
                        .map(|k| {
                            let bb = sat.bus.read8(a + k, AccessKind::Data).0;
                            if (0x20..0x7F).contains(&bb) {
                                bb as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    println!("  match @0x{a:08X}: {ctx}");
                    found += 1;
                }
                a += 1;
            }
        }
        if found == 0 {
            println!("  (no match in low/high WRAM)");
        }
    }

    let Some((r, pr, gbr, code, _probe)) = hit else {
        println!(
            "give-up 0x{giveup:08X} NOT hit in {frames} frames (pc=0x{:08X})",
            sat.master().regs.pc
        );
        return;
    };
    println!("PR=0x{pr:08X} GBR=0x{gbr:08X}");
    for row in 0..4 {
        let b = row * 4;
        println!(
            "  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}",
            b,
            r[b],
            b + 1,
            r[b + 1],
            b + 2,
            r[b + 2],
            b + 3,
            r[b + 3],
        );
    }
    println!("\n=== live give-up branch code @0x{giveup:08X} ===");
    for (i, &w) in code.iter().enumerate() {
        let op = sh2::decoder::decode(w);
        println!(
            "  0x{:08X}: {w:04X}  {}",
            giveup + (i as u32) * 2,
            sh2::debug::disasm(op)
        );
    }
    // The loader state words around GBR (0x06020000) the branch reads + the
    // error-code cell at 0x0601FFF0 (set on the failing path).
    println!("\n=== loader state words ===");
    for a in [
        0x0601_FFF0u32,
        0x0601_FFF4,
        0x0602_0000,
        0x0602_0004,
        0x0602_0008,
        0x0602_000C,
    ] {
        let (v, _) = sat.bus.read32(a, AccessKind::Data);
        println!("  [0x{a:08X}] = 0x{v:08X}");
    }
    // The low-WRAM BIOS CD-status block the error handler reads ([0x0600022C]
    // the loader maps: 0x22/2 -> error 8; else -> error 1). Dump a window so we
    // can see which CD field the BIOS mirrors there.
    println!("\n=== low-WRAM CD status block (0x06000220..0x06000260) ===");
    for a in (0x0600_0220u32..0x0600_0260).step_by(4) {
        let (v, _) = sat.bus.read32(a, AccessKind::Data);
        println!("  [0x{a:08X}] = 0x{v:08X}");
    }
    // The live CD-block host registers (HIRQ/CR1..CR4) right now.
    let (hirq, _) = sat.bus.read16(0x0589_0008, AccessKind::Data);
    let (cr1, _) = sat.bus.read16(0x0589_0018, AccessKind::Data);
    let (cr2, _) = sat.bus.read16(0x0589_001C, AccessKind::Data);
    let (cr3, _) = sat.bus.read16(0x0589_0020, AccessKind::Data);
    let (cr4, _) = sat.bus.read16(0x0589_0024, AccessKind::Data);
    println!("\n  CD now: HIRQ={hirq:04X} CR1={cr1:04X} CR2={cr2:04X} CR3={cr3:04X} CR4={cr4:04X}");
    // Any register pointing into low/high WRAM — deref it (the branch variable
    // is usually [Rn] for some Rn).
    println!("\n=== register-pointed WRAM ===");
    for (i, &a) in r.iter().enumerate() {
        if (0x0600_0000..0x0608_0000).contains(&(a & 0x07FF_FFFF)) {
            let (v, _) = sat.bus.read32(a & !3, AccessKind::Data);
            let (h, _) = sat.bus.read16(a & !1, AccessKind::Data);
            println!("  R{i:<2}=0x{a:08X}  [.&!3]=0x{v:08X}  [.&!1](16)=0x{h:04X}");
        }
    }
    // Optional: disassemble an arbitrary LIVE work-RAM range while stopped here
    // (DISASM_FROM / DISASM_LEN), to read the relocated loader's decision code.
    if let Ok(s) = std::env::var("DISASM_FROM") {
        let from = u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap_or(0);
        let len: u32 = std::env::var("DISASM_LEN")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x80);
        println!("\n=== live disasm @0x{from:08X}..0x{:08X} ===", from + len);
        for off in (0..len).step_by(2) {
            let a = from + off;
            let (w, _) = sat.bus.read16(a, AccessKind::Fetch);
            let op = sh2::decoder::decode(w);
            println!("  0x{a:08X}: {w:04X}  {}", sh2::debug::disasm(op));
        }
    }
    // Optional: hex-dump a LIVE work-RAM range as u32 words (DUMP_FROM/DUMP_LEN)
    // — e.g. the stack (R15) to read the call-chain return addresses, or a data
    // table the loader walks.
    if let Ok(s) = std::env::var("DUMP_FROM") {
        let from = u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap_or(0);
        let len: u32 = std::env::var("DUMP_LEN")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x40);
        println!(
            "\n=== live dump @0x{from:08X}..0x{:08X} (u32) ===",
            from + len
        );
        for off in (0..len).step_by(16) {
            let a = from + off;
            let mut row = format!("  0x{a:08X}:");
            for w in 0..4u32 {
                if off + w * 4 < len {
                    let (v, _) = sat.bus.read32(a + w * 4, AccessKind::Data);
                    row.push_str(&format!(" {v:08X}"));
                }
            }
            println!("{row}");
        }
    }
}

#[test]
#[ignore = "manual: disassemble the post-INTBACK-poll caller in high WRAM"]
fn disasm_post_poll_caller() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    // Step until the master reaches the 0x952 idle trap (or a cap), by
    // which point the post-poll caller code is copied into high WRAM.
    let mut steps = 0u64;
    while sat.master().regs.pc != 0x0600_0952 && steps < 12_000_000 {
        sat.debug_step_master();
        steps += 1;
    }
    println!(
        "stopped after {steps} steps at pc=0x{:08X}",
        sat.master().regs.pc
    );
    // Disassemble the caller around the SF-poll return point 0x0600112E.
    disasm_range(&mut sat, "post-poll caller", 0x0600_1100, 0x80, 0x0600_112E);
    // Dump the literal pool the MOV.L @(disp,PC) loads point into, so we
    // can see what [R1] (read at 0x1132 after the poll) actually is.
    println!("=== high-WRAM literal pool 0x06001330..0x060013C0 ===");
    for a in (0x0600_1330u32..0x0600_13C0).step_by(4) {
        let (v, _) = sat.bus.read32(a, sh2::bus::AccessKind::Data);
        println!("  0x{a:08X}: 0x{v:08X}");
    }
}

#[test]
#[ignore = "manual: capture run_frame's actual master PC trace"]
fn gen_runframe_trace() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    // debug_trace_pc reads PCTRACE_RUNFRAME (set it on the command line,
    // e.g. PCTRACE_RUNFRAME=/tmp/runframe_pc.log).
    if std::env::var("PCTRACE_RUNFRAME").is_err() {
        println!("set PCTRACE_RUNFRAME to capture the trace; running anyway");
    }
    let frames: u32 = std::env::var("PCTRACE_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..frames {
        sat.run_frame(&mut fb);
    }
    println!("ran {frames} run_frame frames; trace in PCTRACE_RUNFRAME");
}

#[test]
#[ignore = "manual BIOS trace; needs a real image in bios/"]
fn trace_master_pc_during_boot() {
    let Some((bios, path)) = load_bios() else {
        println!("no BIOS in bios/; trace skipped");
        return;
    };
    println!("BIOS: {}", path.display());

    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];

    // ---- Phase 0: entry-path trace into the 0x952 dead-wait ----
    // Coarse-find the chunk where the master first reaches the loop,
    // then re-boot and fine-step only the final couple of chunks so we
    // capture (at instruction granularity) the exact branch that lands
    // in the imask=15 loop.
    const TARGET: u32 = 0x0600_0952;
    println!("\n=== entry-path trace into 0x{TARGET:08X} ===");
    let mut c_first = None;
    for chunk in 0..120_000u32 {
        sat.run_for(256);
        if sat.master().regs.pc == TARGET {
            c_first = Some(chunk);
            break;
        }
    }
    println!("  first reached at coarse chunk {c_first:?}");

    if let Some(c) = c_first {
        let Some((bios2, _)) = load_bios() else {
            return;
        };
        let mut sat2 = Saturn::new(bios2);
        sat2.reset();
        for _ in 0..c.saturating_sub(2) {
            sat2.run_for(256);
        }
        // Fine-step the final window, recording deduped PCs.
        let mut ring: Vec<u32> = Vec::new();
        let mut steps = 0u32;
        loop {
            sat2.run_for(1);
            steps += 1;
            let pc = sat2.master().regs.pc;
            if ring.last() != Some(&pc) {
                ring.push(pc);
            }
            if pc == TARGET || steps > 20_000 {
                break;
            }
        }
        println!(
            "  fine window: {steps} steps, {} distinct PCs into the loop:",
            ring.len()
        );
        for pc in ring.iter().rev().take(60).rev() {
            print!(" {pc:06X}");
        }
        println!();
    }

    // ---- Phase 1: gross trajectory, one sample per frame ----
    println!("\n=== per-frame master PC (180 frames) ===");
    let mut last_pc = u32::MAX;
    let mut run_start = 0u32;
    for frame in 0..180u32 {
        sat.run_frame(&mut fb);
        let pc = sat.master().regs.pc;
        // Only print when the PC region changes, to keep output short.
        if pc != last_pc {
            if last_pc != u32::MAX {
                println!("  frames {run_start:>3}..{frame:<3} parked near 0x{last_pc:08X}");
            }
            run_start = frame;
            last_pc = pc;
        }
    }
    println!("  frames {run_start:>3}..180 parked near 0x{last_pc:08X}");

    let m = sat.master();
    println!(
        "\nfinal master: pc=0x{:08X} pr=0x{:08X} sr.imask={} gbr=0x{:08X}",
        m.regs.pc,
        m.regs.pr,
        m.regs.sr.imask(),
        m.regs.gbr,
    );
    println!(
        "cache: ccr=0x{:02X} enabled={} inst_disabled={}",
        m.cache.ccr(),
        m.cache.enabled(),
        m.cache.inst_disabled(),
    );
    for row in 0..4 {
        let b = row * 4;
        println!(
            "  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}",
            b,
            m.regs.r[b],
            b + 1,
            m.regs.r[b + 1],
            b + 2,
            m.regs.r[b + 2],
            b + 3,
            m.regs.r[b + 3],
        );
    }

    // ---- Phase 2: what is the master waiting on? ----
    println!("\n=== wait-state diagnostics at the park ===");
    {
        let m = sat.master();
        let nmi_pending = m.onchip.intc.next_pending(15).is_some();
        println!(
            "  vbr=0x{:08X}  nmi_pending={}  sr.imask={}",
            m.regs.vbr,
            nmi_pending,
            m.regs.sr.imask()
        );
        // NMI vector = VBR + 11*4 (Source::Nmi vector slot 11).
        let nmi_vec_addr = m.regs.vbr.wrapping_add(11 * 4);
        let (handler, _) = sat.bus.read32(nmi_vec_addr, sh2::bus::AccessKind::Data);
        println!("  NMI vector @0x{nmi_vec_addr:08X} -> handler 0x{handler:08X}");
    }
    println!(
        "  SMPC: comreg=0x{:02X} sf={} last_unknown={:?}",
        sat.bus.smpc.comreg, sat.bus.smpc.sf, sat.bus.smpc.last_unknown_command,
    );
    println!(
        "  VDP2: display_enabled={} tvmd=0x{:04X}",
        sat.bus.vdp2.regs.display_enabled(),
        sat.bus.vdp2.regs.tvmd(),
    );

    // Render one more frame and count non-black pixels.
    sat.run_frame(&mut fb);
    let nonblack = fb
        .chunks_exact(4)
        .filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0)
        .count();
    println!(
        "  framebuffer: {nonblack} / {} pixels non-black",
        fb.len() / 4
    );

    println!("\n  slave halted: {}", sat.slave_is_halted());

    // ---- Phase 3: disassemble around the park PC ----
    let park = sat.master().regs.pc;
    disasm_range(&mut sat, "park", park.saturating_sub(4), 0x78, park);

    // Predecessors of the loop (how is it entered / what check precedes it?).
    disasm_range(&mut sat, "loop predecessors", 0x0600_0900, 0x54, park);
    // The SMPC-event routine that branches into the dead-wait.
    disasm_range(&mut sat, "0x60007B0 routine", 0x0600_07A0, 0x60, park);
    // Caller context around PR.
    let pr = sat.master().regs.pr;
    disasm_range(&mut sat, "caller (PR)", pr.saturating_sub(0x18), 0x30, pr);

    // ---- Phase 4: disassemble the NMI handler ----
    let vbr = sat.master().regs.vbr;
    let (handler, _) = sat
        .bus
        .read32(vbr.wrapping_add(11 * 4), sh2::bus::AccessKind::Data);
    disasm_range(&mut sat, "NMI handler", handler, 0x60, u32::MAX);
}

#[test]
#[ignore = "manual: inspect the high-WRAM poll loop at 0x06001150"]
fn disasm_wram_poll_loop() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    // Step until the master reaches the new park loop (or a cap).
    const TARGET: u32 = 0x0601_08C0;
    let mut steps = 0u64;
    while sat.master().regs.pc != TARGET && steps < 120_000_000 {
        sat.debug_step_master();
        steps += 1;
    }
    let m = sat.master();
    println!(
        "stopped after {steps} steps at pc=0x{:08X} pr=0x{:08X} sr.imask={}",
        m.regs.pc,
        m.regs.pr,
        m.regs.sr.imask()
    );
    for row in 0..4 {
        let b = row * 4;
        println!(
            "  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}",
            b,
            m.regs.r[b],
            b + 1,
            m.regs.r[b + 1],
            b + 2,
            m.regs.r[b + 2],
            b + 3,
            m.regs.r[b + 3],
        );
    }
    disasm_range(&mut sat, "park loop", 0x0601_08B0, 0x20, TARGET);
    // Dereference the polled pointer: R3 := [0x06010970], then poll [R3].
    let (ptr, _) = sat.bus.read32(0x0601_0970, sh2::bus::AccessKind::Data);
    let (val16, _) = sat.bus.read16(ptr, sh2::bus::AccessKind::Data);
    println!("\n  polled ptr [0x06010970]=0x{ptr:08X}  [ptr](16)=0x{val16:04X}");
    let vbr = sat.master().regs.vbr;
    println!("  vbr=0x{vbr:08X}  SCU interrupt vectors (0x40..0x50):");
    for vec in 0x40u32..0x50 {
        let (h, _) = sat.bus.read32(vbr + vec * 4, sh2::bus::AccessKind::Data);
        println!("    vec 0x{vec:02X} @0x{:08X} -> 0x{h:08X}", vbr + vec * 4);
    }
    println!("  literal pool 0x06010960..0x060109A0:");
    for a in (0x0601_0960u32..0x0601_09A0).step_by(4) {
        let (v, _) = sat.bus.read32(a, sh2::bus::AccessKind::Data);
        println!("    0x{a:08X}: 0x{v:08X}");
    }
    // SCU interrupt state — this loop may be waiting on a handler.
    println!(
        "\n  SCU: ist=0x{:08X} ims=0x{:08X}",
        sat.bus.scu.ist, sat.bus.scu.ims
    );
    // Step past the RTS and watch where we actually land (interrupt?).
    println!("\n  === stepping past the RTS ===");
    for _ in 0..16 {
        let m = sat.master();
        let pend = m.onchip.intc.next_pending(m.regs.sr.imask());
        println!(
            "    pc=0x{:08X} imask={} pending={:?}",
            m.regs.pc,
            m.regs.sr.imask(),
            pend
        );
        sat.debug_step_master();
    }
    println!(
        "  VDP2 TVSTAT=0x{:04X} VCNT=0x{:04X}",
        sat.bus.vdp2.regs.read16(0x004),
        sat.bus.vdp2.regs.read16(0x00A),
    );
}

#[test]
#[ignore = "manual: watch display-enable + frame counter over many frames"]
fn watch_display() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..160u32 {
        sat.run_frame(&mut fb);
    }
    let ctr0 = sat.bus.read32(0x0604_08A4, sh2::bus::AccessKind::Data).0;
    let (mpc, mimask) = (sat.master().regs.pc, sat.master().regs.sr.imask());
    println!("after 160 frames: pc=0x{mpc:08X} imask={mimask} [0x060408A4]=0x{ctr0:08X}");
    // Single-step ~2 frames worth, counting VBlank-handler (0x06000840)
    // entries and whether the counter the wait-loop polls ever moves.
    const VBLANK_HANDLER: u32 = 0x0600_0840;
    let mut handler_entries = 0u64;
    let mut vblank_raises = 0u64;
    let mut prev_pc = 0u32;
    let mut last_ctr = sat.bus.read32(0x0604_08A4, sh2::bus::AccessKind::Data).0;
    for _ in 0..1_000_000u64 {
        let pc = sat.master().regs.pc;
        if pc == VBLANK_HANDLER && prev_pc != VBLANK_HANDLER {
            handler_entries += 1;
        }
        prev_pc = pc;
        let ist_before = sat.bus.scu.ist;
        sat.debug_step_master();
        if sat.bus.scu.ist & 1 != 0 && ist_before & 1 == 0 {
            vblank_raises += 1;
        }
        let ctr = sat.bus.read32(0x0604_08A4, sh2::bus::AccessKind::Data).0;
        if ctr != last_ctr {
            println!("  [0x060408A4] changed 0x{last_ctr:08X} -> 0x{ctr:08X}");
            last_ctr = ctr;
        }
    }
    println!(
        "over ~1M steps: vblank IST raises={vblank_raises}, VBlank-handler entries={handler_entries}, \
         final pc=0x{:08X}, SCU ist=0x{:08X}",
        sat.master().regs.pc,
        sat.bus.scu.ist,
    );
    disasm_range(
        &mut sat,
        "VBlank-IN handler",
        VBLANK_HANDLER,
        0x10,
        u32::MAX,
    );
    // Common interrupt dispatcher the per-vector stubs branch to.
    disasm_range(&mut sat, "common dispatch", 0x0600_08F2, 0x60, u32::MAX);
    // The dispatcher indexes a callback table; dump a chunk of low WRAM
    // around where user handlers are typically registered.
    let (tbl_base, _) = sat.bus.read32(0x0600_0960, sh2::bus::AccessKind::Data);
    println!("\n  callback table base [0x06000960]=0x{tbl_base:08X}");
    for (name, vec) in [("VBlankIn", 0x40u32), ("VBlankOut", 0x41), ("Sound", 0x46)] {
        let (cb, _) = sat
            .bus
            .read32(tbl_base.wrapping_add(vec * 4), sh2::bus::AccessKind::Data);
        println!("    {name} (vec 0x{vec:02X}) callback = 0x{cb:08X}");
    }
    // Experiment: is the still-halted slave the writer of [0x060408A4]?
    println!("\n  slave halted = {}", sat.slave_is_halted());
    sat.release_slave();
    for _ in 0..40u32 {
        sat.run_frame(&mut fb);
    }
    let (ctr_after, _) = sat.bus.read32(0x0604_08A4, sh2::bus::AccessKind::Data);
    println!(
        "  after releasing slave + 40 frames: [0x060408A4]=0x{ctr_after:08X} master pc=0x{:08X} slave pc=0x{:08X}",
        sat.master().regs.pc,
        sat.slave().regs.pc,
    );
}

#[test]
#[ignore = "manual: analyze the 0x060108BE WRAM park — what it polls + VBlank delivery"]
fn analyze_park() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..150u32 {
        sat.run_frame(&mut fb);
    }
    let pc = sat.master().regs.pc & 0x07FF_FFFF;
    println!(
        "after 150 frames: pc=0x{pc:07X} imask={}",
        sat.master().regs.sr.imask()
    );
    disasm_range(
        &mut sat,
        "park loop (live WRAM)",
        0x0601_08A0,
        0x40,
        pc | 0x0600_0000,
    );

    // Count VBlank-IN interrupt deliveries over the next 5 frames by watching
    // the master enter the VBlank handler vector, and watch the polled counter.
    let (ctr_before, _) = sat.bus.read32(0x0604_08A4, sh2::bus::AccessKind::Data);
    let mut handler_hits = 0u64;
    let mut steps = 0u64;
    let budget = 5 * (CYCLES_PER_FRAME_HINT);
    let mut prev_pc = sat.master().regs.pc;
    while steps < budget {
        sat.debug_step_master();
        sat.debug_drain();
        let p = sat.master().regs.pc & 0x07FF_FFFF;
        // VBlank-IN handler entry observed earlier at 0x06000840.
        if p == 0x0600_0840 && (prev_pc & 0x07FF_FFFF) != 0x0600_0840 {
            handler_hits += 1;
        }
        prev_pc = sat.master().regs.pc;
        steps += 1;
    }
    let (ctr_after, _) = sat.bus.read32(0x0604_08A4, sh2::bus::AccessKind::Data);
    println!(
        "over ~5 frames of single-step: VBlank handler(0x06000840) entered {handler_hits}x; \
         counter[60408A4] {ctr_before:08X} -> {ctr_after:08X}"
    );

    // Dump the VBlank-IN callback-table slot the handler dispatches through.
    // (Earlier probe: handler JSRs through R6 loaded from a table.)
    for a in [0x0600_0840u32, 0x0600_0924, 0x0600_083C] {
        let (w, _) = sat.bus.read32(a, sh2::bus::AccessKind::Data);
        println!("  [{a:08X}] = {w:08X}");
    }
}

const CYCLES_PER_FRAME_HINT: u64 = 479_151;

#[test]
#[ignore = "manual: disassemble the VBlank-IN handler to see how 0x060408A4 should update"]
fn disasm_vblank_handler() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..150u32 {
        sat.run_frame(&mut fb);
    }
    disasm_range(
        &mut sat,
        "VBlank-IN handler (live WRAM)",
        0x0600_0800,
        0x140,
        0x0600_0840,
    );
    // The interrupt vector table: which handler does the master's INTC vector
    // to for VBlank-IN? VBR + vector*4. Read VBR + a few likely SCU vectors.
    let vbr = sat.master().regs.vbr;
    println!("\nVBR=0x{vbr:08X}");
    for vec in 0x40u32..0x48 {
        let (h, _) = sat
            .bus
            .read32(vbr.wrapping_add(vec * 4), sh2::bus::AccessKind::Data);
        println!("  vector 0x{vec:02X} -> 0x{h:08X}");
    }
}

#[test]
#[ignore = "manual: is the VBlank-IN callback ever installed, or is install gated earlier?"]
fn watch_callback_install() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..150u32 {
        sat.run_frame(&mut fb);
    }
    // 1) Capture the REAL callback-table base by single-stepping into the
    //    dispatcher's table load at 0x0600091E (MOV.L @(R0,R3),R6 — R3=base,
    //    R0=vector<<2) during a live VBlank-IN.
    let mut base = 0u32;
    let mut vec_off = 0u32;
    let budget = 3 * CYCLES_PER_FRAME_HINT;
    for _ in 0..budget {
        sat.debug_step_master();
        sat.debug_drain();
        if (sat.master().regs.pc & 0x07FF_FFFF) == 0x0600_091E {
            base = sat.master().regs.r[3];
            vec_off = sat.master().regs.r[0];
            break;
        }
    }
    println!(
        "callback-table base=0x{base:08X}; this interrupt's slot offset=0x{vec_off:08X} \
         (vector 0x{:02X})",
        vec_off >> 2
    );
    let slot = base.wrapping_add(0x40 << 2); // VBlank-IN = vector 0x40
    let rd = |sat: &mut Saturn, a: u32| {
        sat.bus
            .read32(a & 0x07FF_FFFF, sh2::bus::AccessKind::Data)
            .0
    };
    println!(
        "VBlank-IN slot = 0x{slot:08X}, current = 0x{:08X}",
        rd(&mut sat, slot)
    );

    // 2) Run frame-by-frame for a long time; report the first frame the slot
    //    leaves the do-nothing stub (= install reached) or that it never does.
    let mut changed_at: Option<u32> = None;
    let start = rd(&mut sat, slot);
    for f in 0..900u32 {
        sat.run_frame(&mut fb);
        let v = rd(&mut sat, slot);
        if v != start {
            changed_at = Some(f);
            println!("  frame +{f}: VBlank slot changed 0x{start:08X} -> 0x{v:08X}");
            break;
        }
    }
    if changed_at.is_none() {
        println!(
            "  VBlank-IN slot NEVER changed over 900 frames (stays 0x{start:08X}) \
             => the BIOS never reaches callback-install; it is gated earlier."
        );
        let pc = sat.master().regs.pc & 0x07FF_FFFF;
        println!("  final pc=0x{pc:07X}");
    }
}

#[test]
#[ignore = "manual: read the BIOS interrupt callback table — what's installed per vector"]
fn dump_callback_table() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..150u32 {
        sat.run_frame(&mut fb);
    }
    let rd = |sat: &mut Saturn, a: u32| {
        sat.bus
            .read32(a & 0x07FF_FFFF, sh2::bus::AccessKind::Data)
            .0
    };
    let srmask_base = rd(&mut sat, 0x0600_0960);
    let cb_base = rd(&mut sat, 0x0600_0998);
    println!("SR-mask table base = 0x{srmask_base:08X}; callback table base = 0x{cb_base:08X}");
    // Vectors 0x40..0x48 — the SCU interrupt block (VBlank-IN..). Dump the
    // installed callback + whether it's the do-nothing stub 0x0600083C.
    for vec in 0x40u32..0x50 {
        let cb = rd(&mut sat, cb_base.wrapping_add(vec * 4));
        let stub = if (cb & 0x07FF_FFFF) == 0x0600_083C {
            " (do-nothing stub)"
        } else {
            ""
        };
        println!("  vec 0x{vec:02X}: callback=0x{cb:08X}{stub}");
    }
}

#[test]
#[ignore = "manual: analyze the post-splash park (0x06028F9E) — what it polls"]
fn analyze_park2() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..780u32 {
        sat.run_frame(&mut fb);
    }
    let pc = sat.master().regs.pc & 0x07FF_FFFF;
    let m = sat.master();
    println!(
        "after 780 frames: pc=0x{pc:07X} imask={}",
        m.regs.sr.imask()
    );
    for row in 0..4 {
        let b = row * 4;
        println!(
            "  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}",
            b,
            m.regs.r[b],
            b + 1,
            m.regs.r[b + 1],
            b + 2,
            m.regs.r[b + 2],
            b + 3,
            m.regs.r[b + 3],
        );
    }
    disasm_range(
        &mut sat,
        "post-splash park (live WRAM)",
        0x0602_8F60,
        0x60,
        pc | 0x0600_0000,
    );
    println!("\n  WRAM at register-pointed addresses:");
    for r in 0..16 {
        let a = sat.master().regs.r[r];
        if (0x0600_0000..0x0608_0000).contains(&(a & 0x07FF_FFFF)) {
            let (v, _) = sat.bus.read32(a & !3, sh2::bus::AccessKind::Data);
            println!("    R{r}=0x{a:08X}  [{:08X}]=0x{v:08X}", a & !3);
        }
    }
}

#[test]
#[ignore = "manual: framebuffer hash + non-black count over time — find a stable splash frame"]
fn splash_timeline() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    let fnv = |b: &[u8]| {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &x in b {
            h ^= x as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    };
    let mut prev = 0u64;
    for f in 1..=1000u32 {
        sat.run_frame(&mut fb);
        if f % 20 == 0 || f <= 5 {
            let h = fnv(&fb);
            let nb = fb
                .chunks_exact(4)
                .filter(|p| p[0] | p[1] | p[2] != 0)
                .count();
            let mark = if h == prev { " (stable)" } else { "" };
            println!("frame {f:>3}: hash=0x{h:016X} nonblack={nb}{mark}");
            prev = h;
        }
    }
}

#[test]
#[ignore = "manual: dump the framebuffer at several frames to PPM for visual splash check"]
fn dump_framebuffer() {
    use std::io::Write;
    let root = workspace_root();
    let bios_path = match std::env::var("BIOS") {
        Ok(p) if std::path::Path::new(&p).is_absolute() => PathBuf::from(p),
        Ok(p) => root.join(p),
        Err(_) => root.join("bios/Sega Saturn BIOS v1.01 (JAP).bin"),
    };
    let Ok(bios) = std::fs::read(&bios_path) else {
        println!("no BIOS at {}; skipped", bios_path.display());
        return;
    };
    let region = match std::env::var("REGION").as_deref() {
        Ok("us") | Ok("usa") => saturn::smpc::region::NORTH_AMERICA,
        Ok("eu") | Ok("europe") => saturn::smpc::region::EUROPE_PAL,
        _ => saturn::smpc::region::JAPAN,
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    sat.set_region(region);
    sat.set_rtc_unix(1_700_000_000);
    if let Ok(cue_name) = std::env::var("CUE") {
        if let Ok(cue) = std::fs::read_to_string(root.join("roms").join(&cue_name)) {
            if let Ok(d) = saturn::disc::Disc::from_cue(&cue, |name| {
                std::fs::read(root.join("roms").join(name)).ok()
            }) {
                sat.insert_disc(d);
                println!("inserted disc roms/{cue_name}");
            }
        }
    }
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    let mut dims = (320usize, 224usize);
    let mut next = 0u32;
    let frames: Vec<u32> = std::env::var("SNAP_FRAMES")
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![240, 300, 360, 420, 480, 540]);
    for snap in frames {
        while next < snap {
            dims = sat.run_frame(&mut fb);
            next += 1;
        }
        let (w, h) = dims;
        // Write a binary PPM (P6, RGB) — drop the RGBA alpha byte.
        let path = format!("/tmp/fb_{snap:03}.ppm");
        let mut out = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        write!(out, "P6\n{w} {h}\n255\n").unwrap();
        // The renderer packs the active frame tightly at row stride = `w`, so
        // the image is the first `w*h` RGBA pixels (not the whole MAX buffer).
        for px in fb.chunks_exact(4).take(w * h) {
            out.write_all(&px[0..3]).unwrap();
        }
        out.flush().unwrap();
        let nonblack = fb
            .chunks_exact(4)
            .take(w * h)
            .filter(|p| p[0] | p[1] | p[2] != 0)
            .count();
        println!("frame {snap}: wrote {path} ({w}x{h}, {nonblack} non-black px)");
    }
}

#[test]
#[ignore = "manual: at the park, is VDP2 display on + is the splash in the framebuffer?"]
fn check_splash_state() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..150u32 {
        sat.run_frame(&mut fb);
    }
    // VDP2 TVMD (0x05F8_0000): bit15 DISP = display enable; bits[2:0] HRESO.
    let (tvmd, _) = sat.bus.read16(0x05F8_0000, sh2::bus::AccessKind::Data);
    let (bgon, _) = sat.bus.read16(0x05F8_0020, sh2::bus::AccessKind::Data); // BGON
    println!(
        "TVMD=0x{tvmd:04X} (DISP={}), BGON=0x{bgon:04X}",
        (tvmd >> 15) & 1
    );
    // Framebuffer non-black pixel count.
    let nonblack = fb
        .chunks_exact(4)
        .filter(|p| p[0] | p[1] | p[2] != 0)
        .count();
    println!("framebuffer: {nonblack}/{} pixels non-black", fb.len() / 4);
    // Histogram of the top distinct RGBA colors.
    use std::collections::HashMap;
    let mut hist: HashMap<[u8; 4], u32> = HashMap::new();
    for p in fb.chunks_exact(4) {
        *hist.entry([p[0], p[1], p[2], p[3]]).or_default() += 1;
    }
    let mut v: Vec<_> = hist.into_iter().collect();
    v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    for (c, n) in v.into_iter().take(6) {
        println!("  color {c:02X?} x{n}");
    }
    // The watched address the park polls: R3 = *(0x06010970) (PC-rel @0x060108BC).
    for a in [0x0601_0970u32, 0x0601_0974, 0x0601_0978] {
        let (ptr, _) = sat.bus.read32(a, sh2::bus::AccessKind::Data);
        let (val, _) = sat
            .bus
            .read32(ptr & 0x07FF_FFFF, sh2::bus::AccessKind::Data);
        println!("  [{a:08X}]=0x{ptr:08X}  *that=0x{val:08X}");
    }
}

#[test]
#[ignore = "manual: is the boot stuck or just slow? sample PC over many frames"]
fn boot_progress() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    // Region override: the only BIOS present may be EUR (PAL, area 0x0C); the
    // default region is North America (0x04). Set SATURN_REGION=0x0C to match.
    if let Ok(r) = std::env::var("SATURN_REGION") {
        let r = u8::from_str_radix(r.trim_start_matches("0x"), 16).unwrap_or(0x04);
        sat.set_region(r);
        println!("region set to 0x{r:02X}");
    }
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    let frames: u32 = std::env::var("PROGRESS_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let chunk = 20u32;
    for c in 0..(frames / chunk) {
        for _ in 0..chunk {
            sat.run_frame(&mut fb);
        }
        let m = sat.master();
        let pc = m.regs.pc & 0x07FF_FFFF;
        // Show the master PC + a couple of WRAM cells the BIOS boot watches.
        let (vbl, _) = sat.bus.read32(0x0604_08A4, sh2::bus::AccessKind::Data);
        println!(
            "frame {:>4}: pc=0x{pc:07X} imask={} vblank_ctr[60408A4]=0x{vbl:08X}",
            (c + 1) * chunk,
            sat.master().regs.sr.imask(),
        );
    }
}

#[test]
#[ignore = "manual: disassemble the two BIOS steady-state loops (our hang vs MAME)"]
fn disasm_bios_park_loops() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    // BIOS is ROM — no boot needed to read the code.
    disasm_range(
        &mut sat,
        "OUR hang (0x00001D3C tight spin)",
        0x0000_1D10,
        0x60,
        0x0000_1D3C,
    );
    disasm_range(
        &mut sat,
        "MAME loop (0x00003200 region)",
        0x0000_31E0,
        0x80,
        0x0000_3200,
    );
}

#[test]
#[ignore = "manual: dump the 0x06001168 boot divergence (loop condition + polled memory)"]
fn catch_divergence_1168() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    // The divergence is at ~9.3M instructions ≈ frame 37; run up to it.
    for _ in 0..37u32 {
        sat.run_frame(&mut fb);
    }
    // Single-step to the divergent branch and dump state.
    const TARGET: u32 = 0x0600_1168;
    let mut steps = 0u64;
    while sat.master().regs.pc != TARGET && steps < 6_000_000 {
        sat.debug_step_master();
        steps += 1;
    }
    if sat.master().regs.pc != TARGET {
        println!(
            "never reached 0x{TARGET:08X} (pc=0x{:08X})",
            sat.master().regs.pc
        );
        return;
    }
    let m = sat.master();
    println!(
        "at 0x{TARGET:08X} (after {steps} steps): sr.t={} imask={}",
        m.regs.sr.t(),
        m.regs.sr.imask()
    );
    for row in 0..4 {
        let b = row * 4;
        println!(
            "  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}",
            b,
            m.regs.r[b],
            b + 1,
            m.regs.r[b + 1],
            b + 2,
            m.regs.r[b + 2],
            b + 3,
            m.regs.r[b + 3],
        );
    }
    disasm_range(&mut sat, "divergent loop", 0x0600_1140, 0x50, TARGET);
    // The loop likely polls a memory location; dump the candidates the
    // registers point at (low WRAM around the values in R0..R7).
    println!("\n  memory at register-pointed WRAM:");
    for r in 0..8 {
        let a = sat.master().regs.r[r];
        if (0x0600_0000..0x0608_0000).contains(&a) {
            let (v, _) = sat.bus.read32(a & !3, sh2::bus::AccessKind::Data);
            println!("    R{r}=0x{a:08X}  [{:08X}]=0x{v:08X}", a & !3);
        }
    }
}

#[test]
#[ignore = "manual: trace one VBlank-IN handler invocation + its user callback"]
fn trace_vblank_handler() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    // Run past the relocation + into the park loop (~frame 40+).
    for _ in 0..60u32 {
        sat.run_frame(&mut fb);
    }
    const HANDLER: u32 = 0x0600_0840;
    const RTE: u32 = 0x0600_094A;
    const JSR: u32 = 0x0600_0924; // JSR @R6 — calls the user callback
    let ctr_addr = 0x0604_08A4u32;
    let read32 = |s: &mut Saturn, a: u32| s.bus.read32(a, sh2::bus::AccessKind::Data).0;

    // Single-step until the next VBlank-IN handler entry, then trace it.
    let mut steps = 0u64;
    while sat.master().regs.pc != HANDLER && steps < 4_000_000 {
        sat.debug_step_master();
        steps += 1;
    }
    if sat.master().regs.pc != HANDLER {
        println!("VBlank handler never entered in {steps} steps");
        return;
    }
    println!("VBlank-IN handler entered after {steps} steps");
    let ctr_before = read32(&mut sat, ctr_addr);
    let mut callback = 0u32;
    let mut trace_steps = 0u64;
    let mut in_callback = false;
    let mut callback_pcs: Vec<u32> = Vec::new();
    loop {
        let pc = sat.master().regs.pc;
        if pc == JSR {
            callback = sat.master().regs.r[6];
            println!("  JSR @R6: callback = 0x{callback:08X}");
        }
        // After the JSR, record where the callback actually executes.
        if callback != 0 && pc == callback {
            in_callback = true;
        }
        if in_callback && callback_pcs.len() < 40 {
            callback_pcs.push(pc);
        }
        if pc == RTE {
            break;
        }
        sat.debug_step_master();
        trace_steps += 1;
        if trace_steps > 2000 {
            println!("  handler didn't reach RTE in 2000 steps (pc=0x{pc:08X})");
            break;
        }
    }
    let ctr_after = read32(&mut sat, ctr_addr);
    println!("  counter [0x{ctr_addr:08X}]: before=0x{ctr_before:08X} after=0x{ctr_after:08X}");
    if !callback_pcs.is_empty() {
        println!("  callback PC path (first {}):", callback_pcs.len());
        for pc in &callback_pcs {
            print!(" {:06X}", pc & 0xFFFFFF);
        }
        println!();
        disasm_range(&mut sat, "callback", callback, 0x40, u32::MAX);
    } else if callback != 0 {
        println!("  callback 0x{callback:08X} was never entered (JSR didn't reach it)");
        disasm_range(&mut sat, "callback target", callback, 0x20, u32::MAX);
    }
    // Dump the dispatcher's callback-table region around the VBlank-IN slot.
    println!("\n  callback table (R3+vec*4) candidates:");
    for base in [0x0600_0980u32, 0x0600_0A80] {
        let cb = read32(&mut sat, base + 0x40 * 4);
        println!("    [0x{base:08X} + 0x40*4] = 0x{cb:08X}");
    }
}

#[test]
#[ignore = "manual: full-system master PC trace (scheduler order, with slave) for ref diff"]
fn gen_fullsystem_pc_trace() {
    use std::io::Write;
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    // Trace the master PC as the *full* scheduler steps it (master + slave +
    // CD-block interleaved) — the real run_frame path, where the slave
    // perturbs the master's interrupt phase. Default ~60 NTSC frames.
    let frames: u64 = std::env::var("PCTRACE_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let cycles = frames * 479_151;
    let mut pcs: Vec<u32> = Vec::with_capacity(20_000_000);
    sat.run_for_traced(cycles, &mut pcs);
    let f = std::fs::File::create("/tmp/our_pc.log").expect("create trace");
    let mut w = std::io::BufWriter::new(f);
    for pc in &pcs {
        writeln!(w, "{pc:08X}").unwrap();
    }
    w.flush().unwrap();
    println!(
        "wrote {} full-system master PCs ({frames} frames) to /tmp/our_pc.log",
        pcs.len()
    );
}

#[test]
#[ignore = "manual: dump master state at the 0x4216 CMOK-handshake divergence"]
fn catch_4216() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    // Step with drain every 256 cycles (matches run_for/gen_master_pc_trace,
    // the path the resync diff used) and stop at the first 0x4216 whose R14
    // points at the WRAM var MAME reads as 0 (0x060003A4).
    let mut drain_cyc: u64 = 0;
    let mut steps = 0u64;
    loop {
        let m = sat.master();
        if m.regs.pc == 0x0000_4216 {
            break;
        }
        if steps > 40_000_000 {
            println!("never reached 0x4216 in {steps} steps");
            return;
        }
        let c = sat.debug_step_master_nodrain() as u64;
        drain_cyc += c;
        if drain_cyc >= 256 {
            sat.debug_drain();
            drain_cyc = 0;
        }
        steps += 1;
    }
    let (r2, r12, r14, pr) = {
        let m = sat.master();
        (m.regs.r[2], m.regs.r[12], m.regs.r[14], m.regs.pr)
    };
    let (val, _) = sat.bus.read16(r14, sh2::bus::AccessKind::Data);
    println!(
        "at 0x4216 (step {steps}): R14=0x{r14:08X} *(R14)=0x{val:04X} R2=0x{r2:08X} R12=0x{r12:08X} PR=0x{pr:08X}"
    );
    println!("  MAME reference: R14=0x060003A4 *(R14)=0x0000 R2=0x0000 R12=0x0000 PR=0x000041C2");
}

#[test]
#[ignore = "manual: catch the JSR @R12 probe that returns non-zero (divergence)"]
fn catch_divergent_probe() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    // The diverging branch is `TST R4,R4; BF` at 0x337C: it falls through
    // (success) while R4==0 and branches (error) when R4!=0. Catch the
    // first invocation where R4 != 0 — the call to @R12 that we get wrong.
    let mut steps = 0u64;
    let mut last_r12 = 0u32;
    let mut last_r4_at_jsr = 0u32;
    loop {
        let m = sat.master();
        if m.regs.pc == 0x0000_3374 {
            last_r12 = m.regs.r[12];
            last_r4_at_jsr = m.regs.r[4];
        }
        if m.regs.pc == 0x0000_337C && m.regs.r[4] != 0 {
            break;
        }
        if steps > 40_000_000 {
            println!("never caught a non-zero R4 at 0x337C in {steps} steps");
            return;
        }
        sat.debug_step_master();
        steps += 1;
    }
    let m = sat.master();
    println!(
        "caught at step {steps}: pc=0x337C result R4=0x{:08X}; at JSR R12=0x{last_r12:08X} R4=0x{last_r4_at_jsr:08X}",
        m.regs.r[4]
    );
    for row in 0..4 {
        let b = row * 4;
        println!(
            "  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}  r{:<2}=0x{:08X}",
            b,
            m.regs.r[b],
            b + 1,
            m.regs.r[b + 1],
            b + 2,
            m.regs.r[b + 2],
            b + 3,
            m.regs.r[b + 3],
        );
    }
    disasm_range(&mut sat, "probe subroutine @R12", last_r12, 0x50, u32::MAX);
}

#[test]
#[ignore = "manual: dump the diverging memcmp (0x22F0) buffers + caller"]
fn catch_divergent_memcmp() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    // The divergence is at line ~8.78M ≈ step ~9.7M. Skip to just before
    // it, then dump the next memcmp(R7, R5, R6) entry (pc == 0x22F0).
    let skip: u64 = std::env::var("SKIP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8_600_000);
    for _ in 0..skip {
        sat.debug_step_master();
    }
    let mut steps = skip;
    while !(sat.master().regs.pc == 0x0000_22F0 && sat.master().regs.r[6] != 0)
        && steps < skip + 2_000_000
    {
        sat.debug_step_master();
        steps += 1;
    }
    let m = sat.master();
    let (r5, r6, r7, pr) = (m.regs.r[5], m.regs.r[6], m.regs.r[7], m.regs.pr);
    println!(
        "memcmp entry at step {steps}: R7=0x{r7:08X} R5=0x{r5:08X} len R6=0x{r6:08X} PR=0x{pr:08X}"
    );
    let n = r6.min(24);
    let dump = |sat: &mut Saturn, base: u32, label: &str| {
        let mut s = String::new();
        for i in 0..n {
            let (b, _) = sat.bus.read8(base + i, sh2::bus::AccessKind::Data);
            s.push_str(&format!("{b:02X} "));
        }
        println!("  {label} @0x{base:08X}: {s}");
    };
    dump(&mut sat, r7, "buf A (R7)");
    dump(&mut sat, r5, "buf B (R5)");
}

#[test]
#[ignore = "manual: catch the HIRQ-bit poll at 0x32EC and dump arg + HIRQ"]
fn catch_hirq_poll() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let skip: u64 = std::env::var("SKIP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20_000_000);
    for _ in 0..skip {
        sat.debug_step_master();
    }
    let mut steps = skip;
    // 0x32EC: TST R0,R0 where R0 = HIRQ & arg, R2 = arg.
    while sat.master().regs.pc != 0x0000_32EC && steps < skip + 2_000_000 {
        sat.debug_step_master();
        steps += 1;
    }
    let m = sat.master();
    println!(
        "step {steps}: pc=0x{:08X}  R0 (HIRQ & arg)=0x{:08X}  R2 (arg)=0x{:08X}",
        m.regs.pc, m.regs.r[0], m.regs.r[2]
    );
    let (hirq, _) = sat.bus.read16(0x0589_0008, sh2::bus::AccessKind::Data);
    println!("  CD HIRQ now = 0x{hirq:04X}");
}

#[test]
#[ignore = "manual: dump CD-block register state after boot (vs Yabause)"]
fn dump_cd_state() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..100u32 {
        sat.run_frame(&mut fb);
    }
    let rd = |sat: &mut Saturn, a: u32| sat.bus.read16(a, sh2::bus::AccessKind::Data).0;
    println!(
        "CD after 100 frames: HIRQ=0x{:04X} CR1=0x{:04X} CR2=0x{:04X} CR3=0x{:04X} CR4=0x{:04X}",
        rd(&mut sat, 0x0589_0008),
        rd(&mut sat, 0x0589_0018),
        rd(&mut sat, 0x0589_001C),
        rd(&mut sat, 0x0589_0020),
        rd(&mut sat, 0x0589_0024),
    );
    // MAME (no CD image): if no command was issued, the "CDBLOCK" signature
    // is held (CR1=0x0043 CR2=0x4442 CR3=0x4C4F CR4=0x434B, HIRQ=0); after a
    // command + periodic it's PERI|PAUSE status with zero geometry
    // (CR1=0x2100 CR2=CR3=CR4=0).
    println!("  (MAME, no disc: signature held until a command, then 0x2100/0/0/0)");
}

#[test]
#[ignore = "manual: disassemble a BIOS ROM range (no boot needed)"]
fn disasm_bios() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS; skipped");
        return;
    };
    let mut sat = Saturn::new(bios);
    let start: u32 = std::env::var("DA_START")
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x0000_3360);
    let len: u32 = std::env::var("DA_LEN")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x60);
    disasm_range(&mut sat, "bios", start, len, u32::MAX);
}

fn disasm_range(sat: &mut Saturn, label: &str, start: u32, len: u32, mark: u32) {
    println!("\n=== disassembly: {label} @0x{start:08X} ===");
    for off in (0..len).step_by(2) {
        let addr = start + off;
        let (w, _) = sat.bus.read16(addr, sh2::bus::AccessKind::Fetch);
        let op = sh2::decoder::decode(w);
        let marker = if addr == mark { " <== park" } else { "" };
        println!(
            "  0x{addr:08X}: {w:04X}  {}{marker}",
            sh2::debug::disasm(op)
        );
    }
}

#[test]
#[ignore = "manual: dump SCSP timer/interrupt regs + SCU sound-request state at the park"]
fn scsp_state_at_park() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..780u32 {
        sat.run_frame(&mut fb);
    }
    let rd16 = |sat: &mut Saturn, o: u32| {
        sat.bus
            .read16(0x05B0_0000 + o, sh2::bus::AccessKind::Data)
            .0
    };
    for (name, o) in [
        ("TIMA", 0x418u32),
        ("TIMB", 0x41A),
        ("TIMC", 0x41C),
        ("SCIEB", 0x41E),
        ("SCIPD", 0x420),
        ("MCIEB", 0x42A),
        ("MCIPD", 0x42C),
    ] {
        println!("  {name}(0x{o:03X}) = 0x{:04X}", rd16(&mut sat, o));
    }
}

#[test]
#[ignore = "manual: SCU IMS/IST + SMPC state at the post-splash park (is vec 0x47 masked?)"]
fn scu_state_at_park2() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..780u32 {
        sat.run_frame(&mut fb);
    }
    let base = 0x25FE_0000u32;
    let rd = |sat: &mut Saturn, o: u32| sat.bus.read32(base + o, sh2::bus::AccessKind::Data).0;
    let ims = rd(&mut sat, 0xB0);
    let ist = rd(&mut sat, 0xB4);
    println!("at park: IMS=0x{ims:08X} IST=0x{ist:08X}",);
    println!(
        "  Smpc(bit7) masked={} pending={}",
        (ims >> 7) & 1,
        (ist >> 7) & 1
    );
    println!("  imask(master)={}", sat.master().regs.sr.imask());
    // Step ~3 frames; count Smpc raises (IST bit7 transitions) + INTBACK COMREG writes.
    let mut smpc_seen = 0u64;
    let mut prev_ist7 = (ist >> 7) & 1;
    let mut steps = 0u64;
    while steps < 3 * 479_151 {
        sat.debug_step_master();
        sat.debug_drain();
        let i = (sat.bus.read32(base + 0xB4, sh2::bus::AccessKind::Data).0 >> 7) & 1;
        if i == 1 && prev_ist7 == 0 {
            smpc_seen += 1;
        }
        prev_ist7 = i;
        steps += 1;
    }
    println!("over ~3 frames: SCU Smpc(IST bit7) rising edges = {smpc_seen}");
}

#[test]
#[ignore = "manual: is the BIOS issuing SMPC commands (INTBACK) post-splash?"]
fn smpc_activity_at_park() {
    let Some((bios, _)) = load_bios() else {
        println!("no BIOS");
        return;
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for _ in 0..780u32 {
        sat.run_frame(&mut fb);
    }
    // SMPC SF at 0x0010_0063 (bit0 busy); COMREG at 0x0010_001F.
    let sf = |sat: &mut Saturn| sat.bus.read8(0x0010_0063, sh2::bus::AccessKind::Data).0;
    let comreg = |sat: &mut Saturn| sat.bus.read8(0x0010_001F, sh2::bus::AccessKind::Data).0;
    println!(
        "at park: SF=0x{:02X} COMREG=0x{:02X}",
        sf(&mut sat),
        comreg(&mut sat)
    );
    let mut sf_busy_edges = 0u64;
    let mut prev = sf(&mut sat) & 1;
    let mut steps = 0u64;
    while steps < 5 * 479_151 {
        sat.debug_step_master();
        sat.debug_drain();
        let b = sf(&mut sat) & 1;
        if b == 1 && prev == 0 {
            sf_busy_edges += 1;
        }
        prev = b;
        steps += 1;
    }
    println!(
        "over ~5 frames: SMPC SF busy rising edges = {sf_busy_edges} (each = a command issued)"
    );
}

/// M11 render check: boot a game and, every 300 frames, report whether it is
/// actually *running and drawing* — master/slave PC (is the game executing? is
/// the slave released?), VDP2 display-enable, VDP1 draw state, and the
/// framebuffer's non-black pixel count (anything on screen?). Diagnoses the
/// "boots to game code but the screen stays black" symptom.
///
/// ```sh
/// FRAMES=1800 CUE=vf2_full.cue cargo test -p saturn --test trace_boot -- \
///   --ignored --nocapture vf2_render_state
/// ```
#[test]
#[ignore = "manual: post-boot run/render state for a game disc"]
fn vf2_render_state() {
    let root = workspace_root();
    let bios_path = root.join("bios/Sega Saturn BIOS v1.01 (JAP).bin");
    let Ok(bios) = std::fs::read(&bios_path) else {
        println!("no JP BIOS; skipped");
        return;
    };
    let cue_name = std::env::var("CUE").unwrap_or_else(|_| "vf2_full.cue".into());
    let Ok(cue) = std::fs::read_to_string(root.join("roms").join(&cue_name)) else {
        println!("no roms/{cue_name}; skipped");
        return;
    };
    let disc =
        match saturn::disc::Disc::from_cue(&cue, |n| std::fs::read(root.join("roms").join(n)).ok())
        {
            Ok(d) => d,
            Err(e) => {
                println!("cue parse failed: {e}");
                return;
            }
        };
    let mut sat = Saturn::new(bios);
    sat.reset();
    sat.set_region(saturn::smpc::region::JAPAN);
    if let Ok(bup) = std::fs::read(root.join("bios/Sega Saturn BIOS v1.01 (JAP).bup")) {
        sat.load_internal_backup(&bup);
    }
    sat.set_rtc_unix(1_700_000_000);
    sat.insert_disc(disc);
    let frames: u32 = std::env::var("FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1800);
    // Full-size buffer: VDP2 may switch to hi-res mid-boot (up to 704×512), and
    // run_frame asserts the buffer fits the active resolution. Count non-black
    // pixels over just the active w×h that run_frame reports.
    let mut fb = vec![0u8; FRAMEBUFFER_BYTES];
    for f in 0..frames {
        let (w, h) = sat.run_frame(&mut fb);
        if (f + 1) % 300 == 0 || f + 1 == frames {
            let nonblack = fb[..w * h * 4]
                .chunks_exact(4)
                .filter(|p| (p[0] | p[1] | p[2]) != 0)
                .count();
            println!(
                "frame {:>4}: master=0x{:08X} slave=0x{:08X} {w}x{h} VDP2.disp={} VDP1.drawing={} fb_nonblack={}/{}",
                f + 1,
                sat.master().regs.pc,
                sat.slave().regs.pc,
                sat.bus.vdp2.regs.display_enabled(),
                sat.bus.vdp1.is_drawing(),
                nonblack,
                w * h,
            );
        }
    }
    // Classify the post-boot hang: imask 15 = all interrupts masked (a
    // fatal/masked park — nothing can break the `bf $`); imask < the VBlank
    // level = an interrupt-driven event-wait (a handler is meant to set the
    // stacked T). T==0 means the spin is still waiting.
    use sh2::bus::{AccessKind, Bus};
    let (mpc, mimask, mt, mpr, mr15) = {
        let m = sat.master();
        (
            m.regs.pc,
            m.regs.sr.imask(),
            m.regs.sr.t(),
            m.regs.pr,
            m.regs.r[15],
        )
    };
    let (spc, simask, st, spr) = {
        let s = sat.slave();
        (s.regs.pc, s.regs.sr.imask(), s.regs.sr.t(), s.regs.pr)
    };
    println!(
        "\nMASTER pc=0x{mpc:08X} imask={mimask} T={} PR=0x{mpr:08X} R15=0x{mr15:08X}",
        mt as u8
    );
    println!(
        "SLAVE  pc=0x{spc:08X} imask={simask} T={} PR=0x{spr:08X}",
        st as u8
    );
    // The SH-2 records the last CPU *fault* (vector, faulting PC) — illegal(4) /
    // slot-illegal(6) / address-error(9/10) / TRAPA, not interrupts — and the
    // raw word for a general-illegal. This is the reliable crash site (the stack
    // frame is clobbered by the BIOS reset that follows the fault).
    println!(
        "MASTER last_fault={:08X?} illegal_word={:04X?}",
        sat.master().last_fault,
        sat.master().last_illegal_word
    );
    println!(
        "SLAVE  last_fault={:08X?} illegal_word={:04X?}",
        sat.slave().last_fault,
        sat.slave().last_illegal_word
    );
    let base = (mpc & !1).wrapping_sub(0x12);
    println!("=== spin disasm @0x{base:08X} ===");
    for off in (0..0x18u32).step_by(2) {
        let a = base + off;
        let (w, _) = sat.bus.read16(a, AccessKind::Fetch);
        println!(
            "  0x{a:08X}: {w:04X}  {}",
            sh2::debug::disasm(sh2::decoder::decode(w))
        );
    }
    // Verify the 1st-read (AAAVF2.BIN) loaded into WRAM byte-for-byte against the
    // disc file — a gap (zeros where the file has data) means the ReadFile
    // streaming dropped sectors, which would crash the game into uninitialized
    // memory. FAD/len/load-addr from the vf2_iso9660 dump + the IP.BIN entry.
    if let Ok(d2) =
        saturn::disc::Disc::from_cue(&cue, |n| std::fs::read(root.join("roms").join(n)).ok())
    {
        const FILE_FAD: u32 = 0xAE;
        const FILE_LEN: usize = 670640;
        const LOAD: u32 = 0x0600_4000;
        let sectors = FILE_LEN.div_ceil(2048);
        let mut mism = 0usize;
        let mut first = None;
        for s in 0..sectors {
            let Some(data) = d2.read_sector(FILE_FAD + s as u32) else {
                break;
            };
            let n = (FILE_LEN - s * 2048).min(data.len());
            #[allow(clippy::needless_range_loop)]
            for b in 0..n {
                let mem = sat
                    .bus
                    .read8(LOAD + (s * 2048 + b) as u32, AccessKind::Data)
                    .0;
                if mem != data[b] {
                    mism += 1;
                    if first.is_none() {
                        first = Some((LOAD + (s * 2048 + b) as u32, data[b], mem));
                    }
                }
            }
        }
        match first {
            None => println!("\nAAAVF2.BIN load: MATCHES disc across {sectors} sectors — load OK"),
            Some((a, disc, mem)) => println!(
                "\nAAAVF2.BIN load: {mism} mismatches; first @0x{a:08X} disc=0x{disc:02X} mem=0x{mem:02X}"
            ),
        }
    }
}

/// SCSP audio-output probe (M11): run a disc for FRAMES frames, draining
/// `take_audio` each frame exactly as the SDL frontend does, and report the
/// per-window + total absolute sample level — confirms the SCSP produces
/// non-silent audio end-to-end (active slots → output samples). Defaults to
/// *Doukyuusei ~if~* (it actually boots to a title with BGM, so audio is
/// meaningful — VF2 never reaches gameplay); override with `CUE=`.
#[test]
#[ignore = "manual: SCSP audio-output probe (CUE=/FRAMES=)"]
fn audio_probe() {
    let root = workspace_root();
    let bios_path = root.join("bios/Sega Saturn BIOS v1.01 (JAP).bin");
    let Ok(bios) = std::fs::read(&bios_path) else {
        println!("no JP BIOS; skipped");
        return;
    };
    let cue_name =
        std::env::var("CUE").unwrap_or_else(|_| "Doukyuusei - if (Japan) (1M, 2M).cue".into());
    let Ok(cue) = std::fs::read_to_string(root.join("roms").join(&cue_name)) else {
        println!("no roms/{cue_name}; skipped");
        return;
    };
    let disc = match saturn::disc::Disc::from_cue(&cue, |name| {
        std::fs::read(root.join("roms").join(name)).ok()
    }) {
        Ok(d) => d,
        Err(e) => {
            println!("cue parse failed: {e}");
            return;
        }
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    sat.set_region(saturn::smpc::region::JAPAN);
    sat.set_rtc_unix(1_700_000_000);
    sat.insert_disc(disc);
    let frames: u32 = std::env::var("FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2500);
    let mut fb = vec![0u8; saturn::vdp2::FRAMEBUFFER_BYTES];
    let (mut win, mut total, mut n): (i64, i64, u64) = (0, 0, 0);
    // AUDIO_OUT=<path>: also dump the raw interleaved i16-LE stereo (44100 Hz) —
    // exactly what take_audio (hence the SDL queue) receives — so the SCSP output
    // can be analysed/played (`aplay -f S16_LE -r 44100 -c 2 <path>`) independent
    // of the SDL path: separates "SCSP renders garbage" from "SDL mangles it".
    let dump = std::env::var("AUDIO_OUT").ok();
    let mut pcm: Vec<u8> = Vec::new();
    for f in 0..frames {
        sat.run_frame(&mut fb);
        let a = sat.take_audio();
        if dump.is_some() {
            for &x in &a {
                pcm.extend_from_slice(&x.to_le_bytes());
            }
        }
        let s: i64 = a.iter().map(|&x| (x as i64).abs()).sum();
        win += s;
        total += s;
        n += a.len() as u64;
        if (f + 1) % 300 == 0 {
            println!(
                "frame {:4}: |audio| sum over last 300 frames = {win}",
                f + 1
            );
            win = 0;
        }
    }
    let avg = if n > 0 { total / n as i64 } else { 0 };
    println!("AUDIO total |sum|={total} over {n} samples; avg |amplitude|={avg} (0 = silent)");
    if let Some(p) = dump {
        std::fs::write(&p, &pcm).expect("write AUDIO_OUT");
        println!(
            "wrote {} bytes to {p} — play: aplay -f S16_LE -r 44100 -c 2 {p}",
            pcm.len()
        );
    }
}

/// BIOS-only SCSP audio probe (M11 sound target): boot the JAP BIOS with **no
/// disc**, exactly as the frontend does (region JAPAN + host RTC set, so the
/// machine reaches the multiplayer menu rather than sitting on the clock-setup
/// screen), and report whether the BIOS ever keys an SCSP slot / produces
/// non-silent output. This is the faithful repro for "I want to hear the BIOS
/// boot BGM + menu nav SFX": sdbg (no region/RTC) is a less-faithful repro.
///
/// `SAT_PAD=0xBITS` (saturn pad mask) taps the port-1 pad for ~6 frames once a
/// second after `PAD_FROM` frames, to exercise the direction-key nav SFX.
/// `AUDIO_OUT=<path>` dumps the raw i16-LE stereo (play: aplay -f S16_LE -r
/// 44100 -c 2 <path>). `FRAMES=` overrides the run length.
#[test]
#[ignore = "manual: BIOS-only SCSP audio probe (no disc; SAT_PAD=/AUDIO_OUT=/FRAMES=)"]
fn bios_audio_probe() {
    let root = workspace_root();
    // BIOS=<path-or-name> overrides the default JP BIOS; REGION=us|jp|eu picks
    // the SMPC region byte (default JP). Use BIOS="bios/Sega Saturn BIOS (USA).bin"
    // REGION=us to mirror MAME's `saturn` (USA) driver.
    let bios_path = match std::env::var("BIOS") {
        Ok(p) if std::path::Path::new(&p).is_absolute() => PathBuf::from(p),
        Ok(p) => root.join(p),
        Err(_) => root.join("bios/Sega Saturn BIOS v1.01 (JAP).bin"),
    };
    let Ok(bios) = std::fs::read(&bios_path) else {
        println!("no BIOS at {}; skipped", bios_path.display());
        return;
    };
    let region = match std::env::var("REGION").as_deref() {
        Ok("us") | Ok("usa") => saturn::smpc::region::NORTH_AMERICA,
        Ok("eu") | Ok("europe") => saturn::smpc::region::EUROPE_PAL,
        _ => saturn::smpc::region::JAPAN,
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    sat.set_region(region);
    sat.set_rtc_unix(1_700_000_000);
    // No disc by default (the bare BIOS menu); CUE=<name> inserts a disc from
    // roms/ — e.g. CUE=audiocd.cue to reach the CD-player panel WITH an audio
    // disc (the Mednafen oracle path, since Mednafen can't boot no-disc).
    if let Ok(cue_name) = std::env::var("CUE") {
        match std::fs::read_to_string(root.join("roms").join(&cue_name)) {
            Ok(cue) => match saturn::disc::Disc::from_cue(&cue, |name| {
                std::fs::read(root.join("roms").join(name)).ok()
            }) {
                Ok(d) => {
                    sat.insert_disc(d);
                    println!("inserted disc roms/{cue_name}");
                }
                Err(e) => println!("cue parse failed: {e}"),
            },
            Err(_) => println!("no roms/{cue_name}; running no-disc"),
        }
    }
    let frames: u32 = std::env::var("FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1200);
    let pad: u16 = std::env::var("SAT_PAD")
        .ok()
        .and_then(|s| u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(0);
    let pad_from: u32 = std::env::var("PAD_FROM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);

    let mut fb = vec![0u8; saturn::vdp2::FRAMEBUFFER_BYTES];
    let dump = std::env::var("AUDIO_OUT").ok();
    // TRACE68=<n>: record the 68k PC ring over the last <n> frames, then print
    // the distinct execution path — shows the driver's steady-state loop (what
    // it polls instead of building the BGM voices).
    let trace68: Option<u32> = std::env::var("TRACE68").ok().and_then(|s| s.parse().ok());
    if trace68.is_some() {
        sat.bus.scsp.enable_68k_footprint(); // distinct PCs over the WHOLE run
    }
    // ENQLOG=<pc>: capture the 68k value regs at every hit of the BGM enqueue
    // PC (default 0x4B9A), to diff the event stream vs Mednafen's SS_SEQDUMP.
    let enqlog: Option<u32> = std::env::var("ENQLOG").ok().map(|s| {
        u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap_or(0x4B9A)
    });
    if let Some(pc) = enqlog {
        sat.bus.scsp.enable_enq_log(pc);
    }
    // ITRACE=<file>: aligned instruction-boundary 68k trace (vs mednaref SS_ITRACE).
    let itrace_out = std::env::var("ITRACE").ok();
    if itrace_out.is_some() {
        sat.bus.scsp.enable_68k_itrace();
    }
    // SCOPE: the cross-emulator signal "oscilloscope". SCOPE_PC=<68k PC> is the
    // timebase trigger (default 0x40F2 = the seq-tick, so one row per Timer-B
    // tick); SCOPE_CH="name:addr:width,..." lists sound-RAM channels (addr+width
    // hex); SCOPE_OUT=<file> dumps the CSV. The same SCOPE_PC/SCOPE_CH drive the
    // matching mednaref capture for an aligned overlay/diff.
    let scope_on = std::env::var("SCOPE_CH").is_ok();
    if scope_on {
        let pc = std::env::var("SCOPE_PC")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x40F2);
        let channels: Vec<(String, u32, u8)> = std::env::var("SCOPE_CH")
            .unwrap()
            .split(',')
            .filter_map(|spec| {
                let mut it = spec.split(':');
                let name = it.next()?.to_string();
                let addr = u32::from_str_radix(it.next()?.trim().trim_start_matches("0x"), 16).ok()?;
                let w = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(4u8);
                Some((name, addr, w))
            })
            .collect();
        let max = std::env::var("SCOPE_N")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8000);
        sat.bus.scsp.enable_scope(pc, channels, max);
    }
    // MASTERHIST: histogram the master SH-2 PC ring at the end — what loop the
    // master is in near the BGM trigger (the master-side trigger gate, M12 #5).
    // Run with FRAMES set to just before the trigger (~594).
    let masterhist = std::env::var("MASTERHIST").is_ok();
    if masterhist {
        sat.enable_master_pc_trace();
        sat.set_master_trace_freeze(0xFFFF_FFFE, 0xFFFF_FFFF); // never freeze (M11 default would)
    }
    // SRAMWATCH: tally the master's Data reads of SCSP sound RAM
    // (phys 0x05A0_0000..0x05B0_0000) by cache treatment, to test whether the
    // master↔68k mailbox is read through cacheable addresses (staleness-prone
    // `hit`) vs cache-through / cache-off (always fresh). The 68k writes sound
    // RAM with no cache; the master holds a private write-through cache with no
    // hardware coherency, so a cached `hit` could read a value stale of the
    // 68k's latest write. An optional `SRAMWATCH_LO`/`SRAMWATCH_HI` narrows the
    // range to the command mailbox.
    let sramwatch = std::env::var("SRAMWATCH").is_ok();
    if sramwatch {
        let lo = std::env::var("SRAMWATCH_LO")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x05A0_0000);
        let hi = std::env::var("SRAMWATCH_HI")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x05B0_0000);
        sat.master_mut().enable_read_watch(lo, hi);
    }
    // VDP1LOG: per-frame VDP1 command-count series (A6 — the command-list
    // divergence vs Mednafen, the upstream cause of the early BGM trigger).
    // Each entry is (frame, plots, max command_count, max pixels). VDP1_OUT=<f>
    // additionally dumps the full series for a frame-by-frame diff.
    let vdp1log = std::env::var("VDP1LOG").is_ok();
    let mut vdp1_series: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut pcm: Vec<u8> = Vec::new();
    let (mut total, mut n): (i64, u64) = (0, 0);
    let mut peak_slots = 0usize;
    let mut first_keyon: Option<u32> = None;
    // M12: stamp the frame the CD drive finishes recognition spin-up (leaves
    // DrivePhase::Startup) to split the BGM-trigger lead into recognition vs
    // post-recognition.
    let mut recog_done: Option<u32> = None;
    let started_in_startup = sat.bus.cd_block.dbg_in_startup();
    for f in 0..frames {
        if recog_done.is_none() && started_in_startup && !sat.bus.cd_block.dbg_in_startup() {
            recog_done = Some(f);
        }
        // Optional scripted nav: tap the pad for 6 frames at the top of each
        // second, after the menu is up — drives the direction-key SFX path.
        let held = if pad != 0 && f >= pad_from && (f % 60) < 6 {
            pad
        } else {
            0
        };
        sat.set_pad1(held);

        sat.run_frame(&mut fb);

        if vdp1log {
            let (plots, cmds, px) = sat.bus.vdp1.dbg_take_frame();
            vdp1_series.push((f, plots, cmds, px));
        }

        let active = (0..32).filter(|&i| sat.bus.scsp.slot_active(i)).count();
        if active > peak_slots {
            peak_slots = active;
        }
        if active > 0 && first_keyon.is_none() {
            first_keyon = Some(f);
        }

        let a = sat.take_audio();
        if dump.is_some() {
            for &x in &a {
                pcm.extend_from_slice(&x.to_le_bytes());
            }
        }
        let s: i64 = a.iter().map(|&x| (x as i64).abs()).sum();
        total += s;
        n += a.len() as u64;
    }
    let avg = if n > 0 { total / n as i64 } else { 0 };
    let (keyon_execs, slot_starts) = sat.bus.scsp.ctrl.dbg_keyon_counts();
    println!(
        "BIOS-only audio: peak active slots={peak_slots}/32, first key-on at frame {}, \
         total |sum|={total}, avg |amplitude|={avg} (0 = silent)",
        first_keyon
            .map(|f| f.to_string())
            .unwrap_or_else(|| "NEVER".into())
    );
    println!(
        "  key-on activity (lifetime): KYONEX strobes={keyon_execs}  slot starts={slot_starts}"
    );
    println!(
        "  CD recognition (Startup→settle) completed at frame {}",
        recog_done
            .map(|f| f.to_string())
            .unwrap_or_else(|| "NEVER/not-in-startup".into())
    );
    if vdp1log {
        let drawn: Vec<&(u32, u32, u32, u32)> =
            vdp1_series.iter().filter(|&&(_, p, _, _)| p > 0).collect();
        let max_cmds = drawn.iter().map(|&&(_, _, c, _)| c).max().unwrap_or(0);
        println!(
            "  VDP1 per-frame: {} of {} frames drew; peak command_count={max_cmds}",
            drawn.len(),
            vdp1_series.len()
        );
        for tier in [16u32, 64, 128, 256, 371] {
            match drawn.iter().find(|&&&(_, _, c, _)| c >= tier) {
                Some(&&(f, _, c, _)) => {
                    println!("    first frame with >= {tier} cmds: f{f} (cmds={c})")
                }
                None => println!("    never reached {tier} cmds"),
            }
        }
        if let Ok(p) = std::env::var("VDP1_OUT") {
            let s: String = vdp1_series
                .iter()
                .map(|(f, p, c, x)| format!("{f} {p} {c} {x}\n"))
                .collect();
            std::fs::write(&p, s).unwrap();
            println!("    wrote {} per-frame VDP1 entries to {p}", vdp1_series.len());
        }
    }
    // Buzz diagnosis: every active slot at the end of the run — why didn't it free?
    for i in 0..32 {
        if sat.bus.scsp.slot_active(i) {
            let d = sat.bus.scsp.slot_debug(i);
            println!(
                "  slot{i:02} eg={}/{:#X} disdl={} tl={:#04X} loop={} sa={:#07X}",
                d.eg_state, d.eg_volume, d.disdl, d.tl, d.lpctl, d.sa
            );
        }
    }
    // 68k work-area census: the BGM driver builds its per-channel/voice
    // structures in sound RAM (the prior trace put them near 0x7000-0x7FFF). If
    // the driver never sets these up, the per-channel processor finds nothing to
    // key. Scan the whole 512 KiB in 256-byte blocks and report non-zero spans
    // so the driver/program/work-area map is visible without assuming addresses.
    {
        let ram = &sat.bus.scsp.ram;
        let mut spans: Vec<(u32, u32)> = Vec::new();
        let mut cur: Option<(u32, u32)> = None;
        for blk in (0..0x8_0000u32).step_by(0x100) {
            let nz = (0..0x100u32).step_by(4).any(|o| ram.read32(blk + o) != 0);
            match (&mut cur, nz) {
                (None, true) => cur = Some((blk, blk + 0x100)),
                (Some((_, end)), true) => *end = blk + 0x100,
                (Some(s), false) => {
                    spans.push(*s);
                    cur = None;
                }
                (None, false) => {}
            }
        }
        if let Some(s) = cur {
            spans.push(s);
        }
        println!("  sound-RAM non-zero spans (256B blocks):");
        for (a, b) in &spans {
            println!("    {a:#08X}..{b:#08X}  ({} KiB)", (b - a) / 1024);
        }
        // The master->68k command channel (per prior trace: 0x500 / 0x700).
        let w = |o: u32| ram.read16(o);
        println!(
            "  cmd 0x500: {:04X} {:04X} {:04X} {:04X} | 0x700: {:04X} {:04X} {:04X} {:04X}",
            w(0x500),
            w(0x504),
            w(0x508),
            w(0x50C),
            w(0x700),
            w(0x704),
            w(0x708),
            w(0x70C)
        );
    }
    if trace68.is_some() {
        // Whole-run footprint: every distinct 68k PC executed, bucketed by 0x100
        // so it lines up with the Mednafen pcring histogram. The question is
        // which regions Mednafen's driver runs (the 0x4000-0x5000 BGM sequence
        // engine) that ours never reaches.
        let seen = sat.bus.scsp.take_68k_footprint();
        let mut buckets: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
        for pc in &seen {
            *buckets.entry(pc & !0xFF).or_default() += 1;
        }
        println!("  68k whole-run footprint: {} distinct PCs", seen.len());
        for (b, c) in &buckets {
            println!("    {b:#06X}: {c} PCs");
        }
        let reached = |lo: u32, hi: u32| seen.iter().any(|&p| p >= lo && p < hi);
        println!(
            "    reaches: 0x13A2 dispatch={}  0x2C00-0x3070 voice-key={}  0x4000-0x5100 seq-engine={}",
            seen.contains(&0x13A2),
            reached(0x2C00, 0x3070),
            reached(0x4000, 0x5100),
        );
        if let Ok(p) = std::env::var("FOOT_OUT") {
            let s: String = seen.iter().map(|pc| format!("{pc:06X}\n")).collect();
            std::fs::write(&p, s).unwrap();
            println!("    wrote {} footprint PCs to {p}", seen.len());
        }
    }
    if enqlog.is_some() {
        let log = sat.bus.scsp.take_enq_log();
        let mut hist = [0u32; 8];
        for r in &log {
            hist[((r[0] >> 4) & 7) as usize] += 1;
        }
        println!(
            "  68k enqueue stream: {} events, cmd histogram (cmd0..7)={hist:?}",
            log.len()
        );
        let evs: Vec<String> = log.iter().take(48).map(|r| format!("{:02X}", r[0] & 0xFF)).collect();
        println!("    first events (d0 low byte): {}", evs.join(" "));
        if let Ok(p) = std::env::var("ENQ_OUT") {
            let s: String = log
                .iter()
                .map(|r| format!("a6={:06X} d0={:08X} d1={:08X} d2={:08X} d3={:08X}\n", r[4], r[0], r[1], r[2], r[3]))
                .collect();
            std::fs::write(&p, s).unwrap();
            println!("    wrote {} enqueue events to {p}", log.len());
        }
    }
    if scope_on && let Some(sc) = sat.bus.scsp.take_scope() {
        let names: Vec<&str> = sc.channels.iter().map(|(n, _, _)| n.as_str()).collect();
        let mut out = format!("# pc={:04X} timebase-hits={}\nrow {}\n", sc.trigger_pc, sc.rows.len(), names.join(" "));
        for (i, row) in sc.rows.iter().enumerate() {
            let vals: Vec<String> = row.iter().map(|v| format!("{v:X}")).collect();
            out.push_str(&format!("{i} {}\n", vals.join(" ")));
        }
        match std::env::var("SCOPE_OUT") {
            Ok(p) => {
                std::fs::write(&p, &out).unwrap();
                println!("  SCOPE: wrote {} rows × {} channels to {p}", sc.rows.len(), sc.channels.len());
            }
            Err(_) => print!("{out}"),
        }
    }
    if let Some(p) = itrace_out {
        let t = sat.bus.scsp.take_68k_itrace();
        // `pc cycle d4 d7` — cycle is the 68k accumulated clock for the
        // tail-aligned cycle-exact lockstep vs mednaref (BGM-trigger root).
        let s: String = t
            .iter()
            .map(|(pc, cyc, d4, d7)| format!("{pc:04X} {cyc} {d4:08X} {d7:08X}\n"))
            .collect();
        std::fs::write(&p, s).unwrap();
        let (n, s_first, s_trig) = sat.bus.scsp.take_68k_trigger_timing();
        let period = if n > 1 {
            (s_trig.saturating_sub(s_first)) as f64 / (n - 1) as f64
        } else {
            0.0
        };
        println!(
            "  wrote {} itrace entries to {p}; seq-ticks={n}, sample@first-tick={s_first}, \
             sample@trigger={s_trig}, Timer-B period={period:.4} samples/tick",
            t.len()
        );
        let (calls, drawing, hits, cycles) = sat.bus.vdp1.dbg_slowdown();
        println!(
            "  VDP1 draw-slowdown: {calls} total accesses, {drawing} while-drawing, \
             {hits} stall hits, {cycles} stall cycles charged"
        );
        let (plots, dur_sum, dur_max, max_cmds, max_px) = sat.bus.vdp1.dbg_plots();
        let dur_avg = if plots > 0 { dur_sum / plots as u64 } else { 0 };
        println!(
            "  VDP1 plots: {plots} begin_plot calls, avg duration={dur_avg} cy, max={dur_max} cy \
             (frame budget ~479151 cy); max command_count={max_cmds}, max pixels={max_px}"
        );
        println!(
            "  SH-2 associative purges: master={}, slave={}",
            sat.master().cache.dbg_assoc_purges(),
            sat.slave().cache.dbg_assoc_purges()
        );
    }
    if sramwatch && let Some(w) = sat.master().read_watch {
        let cacheable = w.hit + w.miss + w.bypass;
        let total = cacheable + w.through;
        println!(
            "  master sound-RAM[{:08X}..{:08X}] Data reads: {total} total | \
             cache-through={} (fresh) | cacheable={cacheable} [hit={} STALE-PRONE, miss={}, bypass={}]",
            w.lo, w.hi, w.through, w.hit, w.miss, w.bypass
        );
        if w.hit + w.miss == 0 {
            println!(
                "  ⇒ master never reads this region through the cache ⇒ cache staleness is NOT in play here"
            );
        } else {
            println!(
                "  ⇒ master DOES read this region cacheable ({} hit/miss) ⇒ staleness possible; \
                 check whether a purge/invalidate is expected",
                w.hit + w.miss
            );
        }
    }
    if masterhist {
        let pcs = sat.take_master_pc_trace();
        let mut hist: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for pc in &pcs {
            *hist.entry(*pc).or_default() += 1;
        }
        let mut v: Vec<(u32, u32)> = hist.into_iter().collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        println!(
            "  master PC histogram (ring={}, top 24) at frame {frames}, master @ {:08X}:",
            pcs.len(),
            sat.master().regs.pc
        );
        for (pc, c) in v.iter().take(24) {
            println!("    {pc:08X}: {c}");
        }
        print!("  ordered tail (last 40):");
        for pc in pcs.iter().rev().take(40).rev() {
            print!(" {:06X}", pc & 0xFFFFFF);
        }
        println!();
    }
    if let Some(p) = dump {
        std::fs::write(&p, &pcm).expect("write AUDIO_OUT");
        println!("wrote {} bytes to {p}", pcm.len());
    }
}
