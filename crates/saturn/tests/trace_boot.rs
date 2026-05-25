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
    println!("stopped after {steps} steps at pc=0x{:08X}", sat.master().regs.pc);
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
        let Some((bios2, _)) = load_bios() else { return };
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
        println!("  fine window: {steps} steps, {} distinct PCs into the loop:", ring.len());
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
    println!("  literal pool 0x06010960..0x060109A0:");
    for a in (0x0601_0960u32..0x0601_09A0).step_by(4) {
        let (v, _) = sat.bus.read32(a, sh2::bus::AccessKind::Data);
        println!("    0x{a:08X}: 0x{v:08X}");
    }
    // SCU interrupt state — this loop may be waiting on a handler.
    println!(
        "\n  SCU: ist=0x{:08X} ims=0x{:08X}",
        sat.bus.scu.ist,
        sat.bus.scu.ims
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

fn disasm_range(sat: &mut Saturn, label: &str, start: u32, len: u32, mark: u32) {
    println!("\n=== disassembly: {label} @0x{start:08X} ===");
    for off in (0..len).step_by(2) {
        let addr = start + off;
        let (w, _) = sat.bus.read16(addr, sh2::bus::AccessKind::Fetch);
        let op = sh2::decoder::decode(w);
        let marker = if addr == mark { " <== park" } else { "" };
        println!("  0x{addr:08X}: {w:04X}  {}{marker}", sh2::debug::disasm(op));
    }
}
