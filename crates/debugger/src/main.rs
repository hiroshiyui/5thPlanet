//! `sdbg` — an interactive, headless Saturn debugger (Tier-1).
//!
//! A gdb-style REPL over the deterministic `saturn::Saturn` core, so the
//! emulator can be poked *live* — breakpoints, single-step, register/memory/
//! disassembly inspection, CD-block state + command history, and save-state
//! rewind — instead of the edit→rebuild→re-run→grep loop that ad-hoc trace
//! tests force. It wraps the debug hooks the core already exposes
//! (`debug_step_master`, `set_master_bp`/`take_master_bp_hit`, `run_for`,
//! `run_frame`, `save_state`/`load_state`, `CdBlock` accessors) and adds no new
//! core behaviour — it is purely an observer/driver.
//!
//! Usage:
//! ```sh
//! cargo run -p sdbg -- <bios.bin> [disc.cue] [--region=jp|us|eu] [--rtc=<unix>]
//! ```
//! Then type `help` at the `sdbg>` prompt.
//!
//! Stepping model: `si`/`c` drive only the **master** SH-2 (the slave is
//! frozen, peripherals are drained per instruction) — exact and ideal for
//! master-PC analysis. `fc`/`frame`/`run` advance the **whole system** (slave +
//! VDP + CD + SCSP via the scheduler). Use `fc` when the slave must run.

use std::io::{self, Write};
use std::path::Path;

use saturn::Saturn;
use saturn::disc::Disc;
use sh2::bus::{AccessKind, Bus};
use sh2::debug::disasm;
use sh2::decoder::decode;

/// One emulated NTSC frame in master cycles (matches `system.rs`).
const CYCLES_PER_FRAME: u64 = 479_151;

struct Dbg {
    sat: Saturn,
    fb: Vec<u8>,
    master_bp: Option<u32>,
    slave_bp: Option<u32>,
    /// Poll watchpoints checked during `c`: (addr, size-in-bytes).
    watches: Vec<(u32, u8)>,
}

fn parse_num(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u32::from_str_radix(s, 16).ok()
}

fn parse_dec(s: &str) -> Option<u64> {
    s.trim().parse().ok()
}

/// Parse a hex byte string (`"060B17D0"`, optional `0x`) into bytes for `find`.
fn parse_hex_bytes(s: &str) -> Vec<u8> {
    let t = s.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    if t.is_empty() || !t.len().is_multiple_of(2) || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Vec::new();
    }
    (0..t.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&t[i..i + 2], 16).ok())
        .collect()
}

/// Short mnemonic for a CD-block host command (subset; falls back to hex).
fn cd_name(cmd: u8) -> &'static str {
    match cmd {
        0x00 => "GetStatus",
        0x01 => "GetHwInfo",
        0x02 => "GetToc",
        0x03 => "GetSessionInfo",
        0x04 => "Init",
        0x06 => "EndDataXfer",
        0x10 => "Play",
        0x11 => "Seek",
        0x20 => "GetSubcode",
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
        0x62 => "DeleteSectorData",
        0x63 => "GetThenDelSector",
        0x67 => "GetCopyError",
        0x70 => "ChangeDir",
        0x71 => "ReadDir",
        0x72 => "GetFileScope",
        0x73 => "GetFileInfo",
        0x74 => "ReadFile",
        0x75 => "AbortFile",
        0xE0 => "AuthDisc",
        0xE1 => "IsAuth",
        _ => "?",
    }
}

impl Dbg {
    fn read_mem(&mut self, addr: u32, size: u8) -> u32 {
        match size {
            1 => self.sat.bus.read8(addr, AccessKind::Data).0 as u32,
            2 => self.sat.bus.read16(addr, AccessKind::Data).0 as u32,
            _ => self.sat.bus.read32(addr, AccessKind::Data).0,
        }
    }

    fn dump_regs(&self) {
        let m = self.sat.master();
        println!(
            "MASTER pc={:08X} pr={:08X} sr=(T{} I{:X}) gbr={:08X} vbr={:08X} mach={:08X} macl={:08X}{}",
            m.regs.pc,
            m.regs.pr,
            m.regs.sr.t() as u8,
            m.regs.sr.imask(),
            m.regs.gbr,
            m.regs.vbr,
            m.regs.mach,
            m.regs.macl,
            if self.sat.master_is_halted() {
                " [HALTED]"
            } else {
                ""
            },
        );
        for row in 0..4 {
            print!("  ");
            for col in 0..4 {
                let i = row * 4 + col;
                print!("r{i:<2}={:08X} ", m.regs.r[i]);
            }
            println!();
        }
        let s = self.sat.slave();
        println!(
            "SLAVE  pc={:08X} pr={:08X} sr=(T{} I{:X}) gbr={:08X} vbr={:08X}{}",
            s.regs.pc,
            s.regs.pr,
            s.regs.sr.t() as u8,
            s.regs.sr.imask(),
            s.regs.gbr,
            s.regs.vbr,
            if self.sat.slave_is_halted() {
                " [HALTED]"
            } else {
                ""
            },
        );
        for row in 0..4 {
            print!("  ");
            for col in 0..4 {
                let i = row * 4 + col;
                print!("r{i:<2}={:08X} ", s.regs.r[i]);
            }
            println!();
        }
    }

    /// Step the slave one instruction (master frozen) and show its new PC.
    fn step_slave(&mut self, n: u64) {
        for _ in 0..n {
            self.sat.debug_step_slave();
        }
        let pc = self.sat.slave().regs.pc;
        let w = self.read_mem(pc, 2) as u16;
        println!("slave @ {pc:08X}: {w:04X}  {}", disasm(decode(w)));
    }

    /// Trace `n` master instructions, printing each PC + disasm (to `file` if
    /// given, else stdout). A windowed trace from a save state is the basis for
    /// a per-instruction diff against Mednafen's `SS_PCTRACE`.
    fn trace(&mut self, n: u64, file: Option<&str>) {
        let mut buf = String::new();
        for _ in 0..n {
            let pc = self.sat.master().regs.pc;
            let w = self.read_mem(pc, 2) as u16;
            if file.is_some() {
                buf.push_str(&format!("{pc:08X}: {w:04X}  {}\n", disasm(decode(w))));
            } else {
                println!("{pc:08X}: {w:04X}  {}", disasm(decode(w)));
            }
            self.sat.debug_step_master();
        }
        if let Some(f) = file {
            match std::fs::write(f, buf) {
                Ok(()) => println!("wrote {n} master PCs to {f}"),
                Err(e) => println!("write failed: {e}"),
            }
        }
    }

    /// Full-system run until a bus write to `addr` (optionally with value
    /// `val`); reports the storing instruction's PC. The watchpoint records the
    /// *first* matching write, so its PC is exact even though the run stops at
    /// the enclosing frame boundary.
    fn write_break(&mut self, addr: u32, val: Option<u32>, max_frames: u64) {
        self.sat.bus.watch = Some((addr, val));
        self.sat.bus.watch_hit = None;
        for f in 0..max_frames {
            self.sat.run_for(CYCLES_PER_FRAME);
            if let Some((a, v, pc)) = self.sat.bus.watch_hit.take() {
                self.sat.bus.watch = None;
                println!("write {a:08X} = {v:08X} by pc {pc:08X} (frame {f})");
                self.dump_regs();
                return;
            }
        }
        self.sat.bus.watch = None;
        println!(
            "no matching write to {addr:08X}{} in {max_frames} frames",
            val.map(|v| format!(" =={v:08X}")).unwrap_or_default()
        );
    }

    /// Scan memory for a 32-bit (big-endian) value over `[start, start+len)`.
    fn find32(&mut self, value: u32, start: u32, len: u32) {
        let end = start.saturating_add(len);
        let (mut a, mut hits) = (start & !3, 0u32);
        while a < end && hits < 64 {
            if self.read_mem(a, 4) == value {
                println!("  {a:08X}");
                hits += 1;
            }
            a += 4;
        }
        println!("{hits} match(es) for {value:08X} in [{start:08X}..{end:08X})");
    }

    /// Scan memory for a byte pattern over `[start, start+len)`.
    fn find_bytes(&mut self, pat: &[u8], start: u32, len: u32) {
        let end = start.saturating_add(len);
        let (mut a, mut hits) = (start, 0u32);
        while a < end && hits < 64 {
            if pat
                .iter()
                .enumerate()
                .all(|(i, &b)| self.read_mem(a + i as u32, 1) as u8 == b)
            {
                println!("  {a:08X}");
                hits += 1;
            }
            a += 1;
        }
        println!(
            "{hits} match(es) for {} bytes in [{start:08X}..{end:08X})",
            pat.len()
        );
    }

    fn dump_mem(&mut self, addr: u32, len: u32) {
        let mut a = addr & !0xF;
        let end = addr + len;
        while a < end {
            print!("{a:08X}: ");
            let mut ascii = String::new();
            for i in 0..16 {
                let b = self.read_mem(a + i, 1) as u8;
                print!("{b:02X} ");
                ascii.push(if (0x20..0x7F).contains(&b) {
                    b as char
                } else {
                    '.'
                });
            }
            println!(" {ascii}");
            a += 16;
        }
    }

    fn dump_disasm(&mut self, addr: u32, n: u32) {
        let cur = self.sat.master().regs.pc;
        for i in 0..n {
            let pc = addr + i * 2;
            let w = self.read_mem(pc, 2) as u16;
            let op = decode(w);
            let marker = if pc == cur { "=>" } else { "  " };
            println!("{marker} {pc:08X}: {w:04X}  {}", disasm(op));
        }
    }

    fn cd_state(&self) {
        let cb = &self.sat.bus.cd_block;
        let (status, fad, fadtoplay, free, parts) = cb.debug_state();
        let nonempty: Vec<String> = parts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(i, &c)| format!("p{i}={c}"))
            .collect();
        println!(
            "CD: status={status:02X} curfad={fad} fadToPlay={fadtoplay} free_blocks={free} hirq={:04X} mask={:04X}",
            cb.hirq, cb.hirq_mask,
        );
        println!(
            "    partitions: [{}]   has_disc={}",
            if nonempty.is_empty() {
                "all empty".into()
            } else {
                nonempty.join(" ")
            },
            cb.has_disc(),
        );
    }

    fn cdlog(&self, n: usize) {
        let log = &self.sat.bus.cd_block.cmd_log;
        if log.is_empty() {
            println!("(cmd log empty — it is enabled at startup; run something first)");
            return;
        }
        let start = log.len().saturating_sub(n);
        for (i, e) in log.iter().enumerate().skip(start) {
            println!(
                "  [{i:>4}] @{:08X} {:02X} {:<16} in={:04X},{:04X},{:04X},{:04X} -> {:04X},{:04X},{:04X},{:04X}  HIRQ {:04X}->{:04X} st={:02X}",
                e.caller_pc,
                e.cmd,
                cd_name(e.cmd),
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
    }

    /// Single-step the master `n` instructions (slave frozen; peripherals drained).
    fn step(&mut self, n: u64) {
        for _ in 0..n {
            self.sat.debug_step_master();
        }
        let pc = self.sat.master().regs.pc;
        let w = self.read_mem(pc, 2) as u16;
        println!("master @ {pc:08X}: {w:04X}  {}", disasm(decode(w)));
    }

    /// Continue by master single-step until a breakpoint PC, a watchpoint
    /// change, or `max` instructions elapse. Exact; slave is frozen.
    fn cont(&mut self, max: u64) {
        let watches = self.watches.clone();
        let prev: Vec<u32> = watches
            .iter()
            .map(|&(a, sz)| self.read_mem(a, sz))
            .collect();
        for i in 0..max {
            let pc = self.sat.master().regs.pc;
            if i > 0 && Some(pc) == self.master_bp {
                println!("breakpoint @ {pc:08X} (after {i} insns)");
                self.dump_regs();
                return;
            }
            self.sat.debug_step_master();
            for (j, &(a, sz)) in watches.iter().enumerate() {
                let v = self.read_mem(a, sz);
                if v != prev[j] {
                    println!(
                        "watch {a:08X} ({sz}B): {:0w$X} -> {:0w$X}  (master now @ {:08X}, after {} insns)",
                        prev[j],
                        v,
                        self.sat.master().regs.pc,
                        i + 1,
                        w = (sz as usize) * 2,
                    );
                    return;
                }
            }
        }
        println!(
            "ran {max} insns; master @ {:08X}",
            self.sat.master().regs.pc
        );
    }

    /// Full-system continue: run whole frames (slave + peripherals advance) up
    /// to `max_frames`, stopping when the armed master breakpoint snapshots.
    fn frame_cont(&mut self, max_frames: u64) {
        if let Some(pc) = self.master_bp {
            self.sat.set_master_bp(pc);
        }
        if let Some(pc) = self.slave_bp {
            self.sat.set_slave_bp(pc);
        }
        for f in 0..max_frames {
            self.sat.run_for(CYCLES_PER_FRAME);
            if let Some((r, pr, gbr, code)) = self.sat.take_master_bp_hit() {
                println!(
                    "master breakpoint hit at frame {f}: pc={:08X}",
                    self.master_bp.unwrap_or(0)
                );
                println!(
                    "  r0-7:  {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X}",
                    r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]
                );
                println!(
                    "  r8-15: {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X}",
                    r[8], r[9], r[10], r[11], r[12], r[13], r[14], r[15]
                );
                println!(
                    "  pr={pr:08X} gbr={gbr:08X} code={:04X?}",
                    &code[..code.len().min(8)]
                );
                return;
            }
            if let Some((r, ..)) = self.sat.take_slave_bp_hit() {
                println!(
                    "slave breakpoint hit at frame {f}; r0={:08X} r15={:08X}",
                    r[0], r[15]
                );
                return;
            }
        }
        println!(
            "ran {max_frames} frames; master @ {:08X} slave @ {:08X} disp={}",
            self.sat.master().regs.pc,
            self.sat.slave().regs.pc,
            self.sat.bus.vdp2.regs.display_enabled(),
        );
    }

    /// Full-system run until the CD-block first logs a host command matching
    /// `cmd` (and `cr4` in CR_in[3], if given). Stops on the first occurrence,
    /// preserving the preceding setup commands in the (1024-entry) `cmd_log`.
    fn cd_break(&mut self, cmd: u8, cr4: Option<u16>, max_frames: u64) {
        for f in 0..max_frames {
            self.sat.run_for(CYCLES_PER_FRAME);
            let log = &self.sat.bus.cd_block.cmd_log;
            let hit = log
                .iter()
                .rev()
                .take(256)
                .any(|e| e.cmd == cmd && cr4.is_none_or(|v| e.cr_in[3] == v));
            if hit {
                println!(
                    "CD command {cmd:02X}{} first seen at frame {f}; master @ {:08X}",
                    cr4.map(|v| format!(" cr4={v:04X}")).unwrap_or_default(),
                    self.sat.master().regs.pc,
                );
                return;
            }
        }
        println!("CD command {cmd:02X} not seen in {max_frames} frames");
    }

    fn exec(&mut self, line: &str) -> bool {
        let mut it = line.split_whitespace();
        let Some(cmd) = it.next() else { return true };
        let a1 = it.next();
        let a2 = it.next();
        let a3 = it.next();
        match cmd {
            "q" | "quit" | "exit" => return false,
            "h" | "help" | "?" => print_help(),
            "r" | "regs" => self.dump_regs(),
            "si" | "s" => self.step(a1.and_then(parse_dec).unwrap_or(1)),
            "ssi" => self.step_slave(a1.and_then(parse_dec).unwrap_or(1)),
            "t" => self.trace(a1.and_then(parse_dec).unwrap_or(32), a2),
            "c" | "cont" => self.cont(a1.and_then(parse_dec).unwrap_or(5_000_000)),
            "fc" => self.frame_cont(a1.and_then(parse_dec).unwrap_or(600)),
            "bw" => match a1.and_then(parse_num) {
                Some(addr) => self.write_break(
                    addr,
                    a2.and_then(parse_num),
                    a3.and_then(parse_dec).unwrap_or(6000),
                ),
                None => println!("usage: bw <addr-hex> [val-hex] [max-frames]"),
            },
            "find32" => match a1.and_then(parse_num) {
                Some(v) => self.find32(
                    v,
                    a2.and_then(parse_num).unwrap_or(0x0600_0000),
                    a3.and_then(parse_num).unwrap_or(0x0010_0000),
                ),
                None => println!("usage: find32 <value-hex> [start-hex] [len-hex]"),
            },
            "find" => match a1.map(parse_hex_bytes) {
                Some(pat) if !pat.is_empty() => self.find_bytes(
                    &pat,
                    a2.and_then(parse_num).unwrap_or(0x0600_0000),
                    a3.and_then(parse_num).unwrap_or(0x0010_0000),
                ),
                _ => {
                    println!("usage: find <hex-bytes> [start-hex] [len-hex]  (e.g. find 060B17D0)")
                }
            },
            "cb" => match a1.and_then(parse_num) {
                Some(cmdbyte) => self.cd_break(
                    cmdbyte as u8,
                    a2.and_then(parse_num).map(|v| v as u16),
                    a3.and_then(parse_dec).unwrap_or(6000),
                ),
                None => println!("usage: cb <cmd-hex> [cr4-hex] [max-frames]"),
            },
            "frame" | "f" => {
                let n = a1.and_then(parse_dec).unwrap_or(1);
                for _ in 0..n {
                    let mut fb = std::mem::take(&mut self.fb);
                    self.sat.run_frame(&mut fb);
                    self.fb = fb;
                }
                println!(
                    "master @ {:08X} disp={}",
                    self.sat.master().regs.pc,
                    self.sat.bus.vdp2.regs.display_enabled()
                );
            }
            "run" => {
                if let Some(c) = a1.and_then(parse_dec) {
                    self.sat.run_for(c);
                    println!("master @ {:08X}", self.sat.master().regs.pc);
                } else {
                    println!("usage: run <cycles>");
                }
            }
            "b" => match a1.and_then(parse_num) {
                Some(pc) => {
                    self.master_bp = Some(pc);
                    println!("master bp @ {pc:08X}");
                }
                None => {
                    self.master_bp = None;
                    println!("master bp cleared");
                }
            },
            "bs" => match a1.and_then(parse_num) {
                Some(pc) => {
                    self.slave_bp = Some(pc);
                    println!("slave bp @ {pc:08X}");
                }
                None => {
                    self.slave_bp = None;
                    println!("slave bp cleared");
                }
            },
            "m" | "x" => match a1.and_then(parse_num) {
                Some(addr) => self.dump_mem(addr, a2.and_then(parse_num).unwrap_or(64)),
                None => println!("usage: m <addr> [len]"),
            },
            "d" | "dis" => {
                let addr = a1.and_then(parse_num).unwrap_or(self.sat.master().regs.pc);
                self.dump_disasm(addr, a2.and_then(parse_num).unwrap_or(16));
            }
            "cd" => self.cd_state(),
            "cdlog" => self.cdlog(a1.and_then(parse_dec).map(|n| n as usize).unwrap_or(20)),
            "vdp" => println!(
                "VDP2 display_enabled={}  VDP1 drawing={}",
                self.sat.bus.vdp2.regs.display_enabled(),
                self.sat.bus.vdp1.is_drawing(),
            ),
            "w" => match a1.and_then(parse_num) {
                Some(addr) => {
                    let sz = a2.and_then(parse_num).unwrap_or(2) as u8;
                    self.watches.push((addr, sz));
                    println!("watch {addr:08X} ({sz}B); {} total", self.watches.len());
                }
                None => println!("usage: w <addr> [size=1|2|4]"),
            },
            "dw" => {
                self.watches.clear();
                println!("watchpoints cleared");
            }
            "save" => match a1 {
                Some(path) => match std::fs::write(path, self.sat.save_state()) {
                    Ok(()) => println!("saved {path}"),
                    Err(e) => println!("save failed: {e}"),
                },
                None => println!("usage: save <file>"),
            },
            "load" => match a1 {
                Some(path) => match std::fs::read(path) {
                    Ok(bytes) => match self.sat.load_state(&bytes) {
                        Ok(()) => {
                            // The cmd_log isn't serialized (kept out of save
                            // states to stay lean); re-enable logging so `cdlog`
                            // captures commands issued after the load.
                            self.sat.bus.cd_block.cmd_log_on = true;
                            println!("loaded {path}; master @ {:08X}", self.sat.master().regs.pc)
                        }
                        Err(e) => println!("load failed: {e:?}"),
                    },
                    Err(e) => println!("read failed: {e}"),
                },
                None => println!("usage: load <file>"),
            },
            other => println!("unknown command {other:?}; try `help`"),
        }
        true
    }
}

fn print_help() {
    println!(
        "\
sdbg commands:
  si [n]          step master n instructions (slave frozen)        [alias s]
  ssi [n]         step SLAVE n instructions (master frozen)
  t [n] [file]    trace n master insns (PC+disasm) to stdout or file
  c [n]           continue master single-step until bp/watch/max-n insns
  fc [n]          full-system continue up to n frames, stop at master bp
  bw <addr> [val] [maxf]  full-system run until a bus write to <addr>
                  (optional ==val), report the storing instruction's PC
  cb <cmd> [cr4] [maxf]  full-system run until the CD logs host command <cmd>
                  (hex; optional CR_in[3] match), preserving setup in cdlog
  frame [n]       run n full frames (slave+VDP+CD advance)         [alias f]
  run <cyc>       run_for <cyc> master cycles (full system)
  b [pc]          set/clear master breakpoint (hex)
  bs [pc]         set/clear slave breakpoint
  w <addr> [sz]   add a poll watchpoint (size 1/2/4, default 2), checked in `c`
  dw              clear watchpoints
  find32 <v> [start] [len]  scan memory for a 32-bit value (default: high WRAM)
  find <bytes> [start] [len]  scan memory for a hex byte pattern
  regs            dump master + slave registers (r0-r15 each)       [alias r]
  m <addr> [len]  hex-dump memory                                  [alias x]
  d [addr] [n]    disassemble n insns from addr (default: master pc) [alias dis]
  cd              CD-block state (status/hirq/curfad/partitions)
  cdlog [n]       last n CD host commands
  vdp             VDP display state
  save <file>     write a save state (snapshot)
  load <file>     restore a save state (rewind; re-enables cdlog)
  help            this help                                        [alias h ?]
  quit            exit                                             [alias q]"
    );
}

fn load_disc(cue_path: &Path) -> Option<Disc> {
    let cue = std::fs::read_to_string(cue_path).ok()?;
    let dir = cue_path.parent().unwrap_or_else(|| Path::new("."));
    match Disc::from_cue(&cue, |n| std::fs::read(dir.join(n)).ok()) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("cue parse failed: {e}");
            None
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut bios_path = None;
    let mut disc_path = None;
    let mut region = saturn::smpc::region::JAPAN;
    let mut rtc: u64 = 1_700_000_000;
    for a in &args {
        if let Some(r) = a.strip_prefix("--region=") {
            region = match r {
                "us" | "usa" => saturn::smpc::region::NORTH_AMERICA,
                "eu" | "europe" => saturn::smpc::region::EUROPE_PAL,
                _ => saturn::smpc::region::JAPAN,
            };
        } else if let Some(t) = a.strip_prefix("--rtc=") {
            rtc = t.parse().unwrap_or(rtc);
        } else if bios_path.is_none() {
            bios_path = Some(a.clone());
        } else if disc_path.is_none() {
            disc_path = Some(a.clone());
        }
    }
    let Some(bios_path) = bios_path else {
        eprintln!("usage: sdbg <bios.bin> [disc.cue] [--region=jp|us|eu] [--rtc=<unix>]");
        std::process::exit(2);
    };
    let bios = match std::fs::read(&bios_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read BIOS {bios_path}: {e}");
            std::process::exit(2);
        }
    };
    let mut sat = Saturn::new(bios);
    sat.reset();
    sat.set_region(region);
    sat.set_rtc_unix(rtc);
    // Battery file next to the BIOS, if present (matches the frontend/tests).
    if let Ok(bup) = std::fs::read(format!("{bios_path}.bup")) {
        sat.load_internal_backup(&bup);
    }
    if let Some(dp) = &disc_path {
        match load_disc(Path::new(dp)) {
            Some(d) => {
                sat.insert_disc(d);
                println!("disc inserted: {dp}");
            }
            None => eprintln!("no disc loaded"),
        }
    }
    // CD command history on by default — the debugger's most-used view.
    sat.bus.cd_block.cmd_log_on = true;

    let mut dbg = Dbg {
        sat,
        fb: vec![0u8; 320 * 224 * 4],
        master_bp: None,
        slave_bp: None,
        watches: Vec::new(),
    };
    println!(
        "sdbg ready. BIOS={bios_path}. type `help`. master @ {:08X}",
        dbg.sat.master().regs.pc
    );

    let stdin = io::stdin();
    loop {
        print!("sdbg> ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                if !dbg.exec(line.trim()) {
                    break;
                }
            }
            Err(e) => {
                eprintln!("input error: {e}");
                break;
            }
        }
    }
}
