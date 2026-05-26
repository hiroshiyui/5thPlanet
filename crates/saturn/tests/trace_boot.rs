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
    let n = (r6.min(24)) as u32;
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
    println!("  (Yabause reference: HIRQ=0xFFF7 CR1=0x2100 CR2=0x4101 CR3=0x0100 CR4=0x0096)");
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
