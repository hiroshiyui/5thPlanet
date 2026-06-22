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

mod m68k_disasm;

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
    /// Master breakpoints, each `(pc, optional (reg-index, value) guard)` — the
    /// guard fires only when `R[idx] == val`. Several may be set (`b <addr>`
    /// adds, `bd <id>` deletes); both `c` and `fc` honour the whole list.
    mbps: Vec<(u32, Option<(usize, u32)>)>,
    /// Slave breakpoints (same shape); honoured by `fc`.
    sbps: Vec<(u32, Option<(usize, u32)>)>,
    /// Symbol table `(name, addr)`: resolves names where an address is expected
    /// (`b main`, `m loader_state`) and annotates output (`name+0xNN`). Loaded
    /// via `syms <file>` / `--syms=<file>` or defined with `sym <name> <addr>`.
    syms: Vec<(String, u32)>,
    /// Optional memory probe address captured (read through the bus = raw WRAM,
    /// no CPU cache) on any breakpoint hit. Set via `probe <addr>`. Compares
    /// what a CPU loaded (via its cache) against true memory at the bp cycle.
    bp_probe: Option<u32>,
    /// Poll watchpoints checked during `c`: (addr, size-in-bytes).
    watches: Vec<(u32, u8)>,
    /// Optional SCSP 68k breakpoint: `(pc, optional (reg, val) guard)` — reg 0-7 =
    /// D0-D7, 8-15 = A0-A7. Set via `b68 <addr> [reg val]`; armed/checked in `fc`.
    bp68: Option<(u32, Option<(u8, u32)>)>,
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

/// Parse a reference master-PC trace (one hex PC per line — e.g. a Mednafen
/// `SS_PCTRACE` dump) into a PC vector. Tolerant: strips a `0x` prefix and
/// surrounding whitespace, takes the leading hex token (so trailing
/// comments/fields are ignored), and skips blank / non-hex lines (headers).
fn parse_trace_pcs(text: &str) -> Vec<u32> {
    text.lines()
        .filter_map(|l| {
            let t = l.trim();
            let t = t
                .strip_prefix("0x")
                .or_else(|| t.strip_prefix("0X"))
                .unwrap_or(t);
            let tok: String = t.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            (!tok.is_empty())
                .then(|| u32::from_str_radix(&tok, 16).ok())
                .flatten()
        })
        .collect()
}

/// The loop-collapse both ours (`gen_vf2_pc_trace`) and Mednafen's
/// `SS_LogMasterPC` apply: suppress a PC already seen in the last 64 logged PCs,
/// so an idle/poll spin logs one pass instead of its thousands of iterations.
/// Returns `true` if `pc` should be logged; updates `recent` in place.
fn collapse_should_log(recent: &mut std::collections::VecDeque<u32>, pc: u32) -> bool {
    if recent.contains(&pc) {
        return false;
    }
    if recent.len() == 64 {
        recent.pop_front();
    }
    recent.push_back(pc);
    true
}

/// Decode set HIRQ bits to their names (for the `hirqlog` CD-timing trace).
fn hirq_bits(v: u16) -> String {
    const NAMES: [(u16, &str); 12] = [
        (0x001, "CMOK"),
        (0x002, "DRDY"),
        (0x004, "CSCT"),
        (0x008, "BFUL"),
        (0x010, "PEND"),
        (0x020, "DCHG"),
        (0x040, "ESEL"),
        (0x080, "EHST"),
        (0x100, "ECPY"),
        (0x200, "EFLS"),
        (0x400, "SCDQ"),
        (0x800, "MPED"),
    ];
    let s: Vec<&str> = NAMES
        .iter()
        .filter(|(b, _)| v & b != 0)
        .map(|(_, n)| *n)
        .collect();
    if s.is_empty() {
        "-".into()
    } else {
        s.join("|")
    }
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
    // ---- Symbols ---------------------------------------------------------

    /// Resolve an address token: a defined symbol name, else a hex literal.
    fn resolve(&self, tok: &str) -> Option<u32> {
        self.syms
            .iter()
            .find(|(n, _)| n == tok)
            .map(|(_, a)| *a)
            .or_else(|| parse_num(tok))
    }

    /// The nearest symbol at or below `addr` (within 0x1000), as `name` or
    /// `name+0xNN` — for annotating disassembly, breakpoint hits, and the call
    /// chain. `None` when no symbol covers it.
    fn label(&self, addr: u32) -> Option<String> {
        self.syms
            .iter()
            .filter(|(_, a)| *a <= addr && addr - *a < 0x1000)
            .max_by_key(|(_, a)| *a)
            .map(|(n, a)| {
                if *a == addr {
                    n.clone()
                } else {
                    format!("{n}+0x{:X}", addr - a)
                }
            })
    }

    /// `08X` address with a trailing `  <label>` when a symbol covers it.
    fn fmt_addr(&self, addr: u32) -> String {
        match self.label(addr) {
            Some(l) => format!("{addr:08X} <{l}>"),
            None => format!("{addr:08X}"),
        }
    }

    /// Load `name addr` (or `addr name`) pairs from a file — one per line, `#`
    /// comments and blank lines skipped. A later definition of a name replaces
    /// an earlier one.
    fn load_syms(&mut self, path: &str) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                println!("read {path} failed: {e}");
                return;
            }
        };
        let mut n = 0;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut it = line.split_whitespace();
            let (Some(a), Some(b)) = (it.next(), it.next()) else {
                continue;
            };
            // Accept either column order; the hex token is the address.
            let (name, addr) = match (parse_num(a), parse_num(b)) {
                (None, Some(x)) => (a, x), // name addr
                (Some(x), _) => (b, x),    // addr name
                (None, None) => continue,
            };
            self.define_sym(name, addr);
            n += 1;
        }
        println!("loaded {n} symbols from {path}; {} total", self.syms.len());
    }

    /// Define (or redefine) a symbol.
    fn define_sym(&mut self, name: &str, addr: u32) {
        self.syms.retain(|(n, _)| n != name);
        self.syms.push((name.to_string(), addr));
    }

    // ---- Breakpoint management ------------------------------------------

    /// List all breakpoints (master, slave, 68k) with a global id (the index
    /// `bd <id>` deletes).
    fn list_bps(&self) {
        if self.mbps.is_empty() && self.sbps.is_empty() && self.bp68.is_none() {
            println!("no breakpoints (`b <addr>` to add)");
            return;
        }
        let cond = |g: &Option<(usize, u32)>| match g {
            Some((i, v)) => format!("  if R{i}=={v:08X}"),
            None => String::new(),
        };
        let mut id = 0;
        for (pc, g) in &self.mbps {
            println!("  {id:>2}  [M]   {}{}", self.fmt_addr(*pc), cond(g));
            id += 1;
        }
        for (pc, g) in &self.sbps {
            println!("  {id:>2}  [S]   {}{}", self.fmt_addr(*pc), cond(g));
            id += 1;
        }
        if let Some((pc, g)) = &self.bp68 {
            let g = g
                .map(|(r, v)| {
                    let n = if r < 8 { format!("D{r}") } else { format!("A{}", r - 8) };
                    format!("  if {n}=={v:08X}")
                })
                .unwrap_or_default();
            println!("  {id:>2}  [68k] {pc:06X}{g}");
        }
    }

    /// Delete a breakpoint by global id (the index shown by `list_bps`).
    fn delete_bp(&mut self, id: usize) {
        let (m, s) = (self.mbps.len(), self.sbps.len());
        if id < m {
            let (pc, _) = self.mbps.remove(id);
            println!("deleted master bp {pc:08X}");
        } else if id < m + s {
            let (pc, _) = self.sbps.remove(id - m);
            println!("deleted slave bp {pc:08X}");
        } else if id == m + s && self.bp68.is_some() {
            let (pc, _) = self.bp68.take().unwrap();
            println!("deleted 68k bp {pc:06X}");
        } else {
            println!("no breakpoint #{id}");
        }
    }

    /// Parse the optional `<regidx> <val>` guard following a breakpoint address.
    fn parse_guard(idx: Option<&str>, val: Option<&str>) -> Option<(usize, u32)> {
        match (idx.and_then(parse_dec), val.and_then(parse_num)) {
            (Some(i), Some(v)) if i < 16 => Some((i as usize, v)),
            _ => None,
        }
    }

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

    /// Dump both SH-2s' free-running-timer (FRT) state — the inter-CPU FTI
    /// input-capture handshake lives here (FTCSR.ICF bit 7 = input captured,
    /// TIER.ICIE bit 7 = capture-interrupt enabled). Lets a deadlock between
    /// a master/slave dispatch loop be inspected: a slave parked polling
    /// FTCSR.ICF with ICF clear is waiting for the master to pulse its FTI.
    fn dump_frt(&self) {
        for (who, f) in [
            ("MASTER", &self.sat.master().onchip.frt),
            ("SLAVE ", &self.sat.slave().onchip.frt),
        ] {
            println!(
                "{who} FRT: TIER={:02X}(ICIE={}) FTCSR={:02X}(ICF={} OCFA={} OCFB={} OVF={}) FRC={:04X} FICR={:04X}",
                f.tier, (f.tier >> 7) & 1,
                f.ftcsr, (f.ftcsr >> 7) & 1, (f.ftcsr >> 3) & 1, (f.ftcsr >> 2) & 1, (f.ftcsr >> 1) & 1,
                f.frc, f.ficr,
            );
        }
    }

    /// Dump both SH-2s' cache state — CCR (enable / instruction- & data-
    /// replacement-disable / two-way) and the fetch/data hit·miss tallies.
    /// A stale instruction-cache (a fetch hit returning code the game has
    /// since overwritten via DMA) is a known game-hang class; this surfaces
    /// how heavily each core caches instructions.
    fn dump_cache(&self) {
        for (who, c) in [
            ("MASTER", &self.sat.master().cache),
            ("SLAVE ", &self.sat.slave().cache),
        ] {
            let [fh, fm, dh, dm] = c.dbg_stats();
            println!(
                "{who} CACHE: CCR={:02X} enabled={} Idis={} Ddis={} 2way={} | fetch {fh}h/{fm}m  data {dh}h/{dm}m | purges full={} assoc={}",
                c.ccr(), c.enabled() as u8, c.inst_disabled() as u8, c.data_disabled() as u8, c.two_way() as u8,
                c.dbg_purges(), c.dbg_assoc_purges(),
            );
        }
    }

    /// Cache coherency audit: compare every valid master cache line against the
    /// raw backing memory (read through the bus, which bypasses the SH-2 cache).
    /// A mismatch is a **stale line** — memory a DMA / the other CPU wrote after
    /// the line was filled, that a purge didn't drop. Reports the first stale
    /// lines (address + cached-vs-memory first bytes) — the smoking gun for the
    /// instruction-cache-staleness hang class.
    fn cache_audit(&mut self) {
        let lines = self.sat.master().cache.dbg_lines();
        let total = lines.len();
        let mut stale = 0u32;
        for (addr, data) in lines {
            let mut mem = [0u8; 16];
            for i in 0..4 {
                let w = self.sat.bus.read32(addr.wrapping_add(i * 4), AccessKind::Data).0;
                mem[i as usize * 4..i as usize * 4 + 4].copy_from_slice(&w.to_be_bytes());
            }
            if mem != data {
                stale += 1;
                if stale <= 24 {
                    println!(
                        "STALE @{addr:08X}: cache {:02X?} vs mem {:02X?}",
                        &data[..8], &mem[..8]
                    );
                }
            }
        }
        println!("cache audit: {stale} stale of {total} valid lines");
    }

    /// Live stale-instruction-fetch detector: enable [`Cpu::dbg_detect_stale`]
    /// on both SH-2s and advance frame-by-frame (up to `max_frames`) until one
    /// fetches a cache-hit that disagrees with backing memory — the exact
    /// instruction-cache-coherency divergence. Reports `(addr, cached, memory,
    /// cycle, frame)` and stops there so the context can be inspected.
    fn detect_stale(&mut self, max_frames: u64) {
        self.sat.master_mut().dbg_detect_stale = true;
        self.sat.master_mut().dbg_stale_fetch = None;
        self.sat.slave_mut().dbg_detect_stale = true;
        self.sat.slave_mut().dbg_stale_fetch = None;
        let mut caught = false;
        for f in 0..max_frames {
            self.frame_cont(1);
            let m = self.sat.master().dbg_stale_fetch;
            let s = self.sat.slave().dbg_stale_fetch;
            if m.is_some() || s.is_some() {
                if let Some((a, c, mem, cyc)) = m {
                    println!("MASTER STALE FETCH @{a:08X}: cache {c:04X} vs mem {mem:04X} (cycle {cyc}, frame {f})");
                }
                if let Some((a, c, mem, cyc)) = s {
                    println!("SLAVE STALE FETCH @{a:08X}: cache {c:04X} vs mem {mem:04X} (cycle {cyc}, frame {f})");
                }
                caught = true;
                break;
            }
        }
        if !caught {
            println!(
                "no stale read in {max_frames} frames ({} master + {} slave comparisons made)",
                self.sat.master().dbg_stale_checks,
                self.sat.slave().dbg_stale_checks,
            );
        }
        self.sat.master_mut().dbg_detect_stale = false;
        self.sat.slave_mut().dbg_detect_stale = false;
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

    /// Read `words` 16-bit words of the SCSP 68k's address space (sound RAM at
    /// main-bus 0x05A0_0000) starting at 68k address `base68`, into a buffer.
    fn read_sound_words(&mut self, base68: u32, words: u32) -> Vec<u16> {
        (0..words)
            .map(|i| self.read_mem(0x05A0_0000 + ((base68 + i * 2) & 0x7_FFFF), 2) as u16)
            .collect()
    }

    /// Disassemble `n` 68k instructions at 68k address `addr` (the SCSP sound
    /// driver). Reads sound RAM via the main bus; targets shown in 68k space.
    fn disasm68(&mut self, addr: u32, n: usize) {
        let base68 = addr & 0x7_FFFF;
        let buf = self.read_sound_words(base68, n as u32 * 6 + 8);
        let read = |a: u32| -> u16 {
            buf.get(((a.wrapping_sub(base68)) / 2) as usize)
                .copied()
                .unwrap_or(0)
        };
        let mut pc = base68;
        for _ in 0..n {
            let insn = m68k_disasm::disasm(&read, pc);
            let raw: String = (0..insn.len / 2)
                .map(|j| format!("{:04X} ", read(pc + j * 2)))
                .collect();
            println!("  {pc:06X}: {:<22}{}", raw.trim_end(), insn.text);
            pc = pc.wrapping_add(insn.len.max(2));
        }
    }

    /// Dump the last `n` entries of the 68k PC ring (consecutive-collapsed),
    /// each disassembled. The ring is enabled at startup; advance with `fc`.
    fn trace68(&mut self, n: usize) {
        let trace = self.sat.bus.scsp.take_68k_trace();
        if trace.is_empty() {
            println!("(68k trace empty — run the system with `fc` first; SNDON must have fired)");
            return;
        }
        let start = trace.len().saturating_sub(n);
        for &pc in &trace[start..] {
            let base68 = pc & 0x7_FFFF;
            let buf = self.read_sound_words(base68, 6);
            let read = |a: u32| -> u16 {
                buf.get(((a.wrapping_sub(base68)) / 2) as usize)
                    .copied()
                    .unwrap_or(0)
            };
            let insn = m68k_disasm::disasm(&read, base68);
            println!("  {pc:06X}: {}", insn.text);
        }
        println!(
            "({} PCs in ring; showed last {})",
            trace.len(),
            n.min(trace.len())
        );
    }

    fn dump_disasm(&mut self, addr: u32, n: u32) {
        let cur = self.sat.master().regs.pc;
        for i in 0..n {
            let pc = addr + i * 2;
            let w = self.read_mem(pc, 2) as u16;
            let op = decode(w);
            let marker = if pc == cur { "=>" } else { "  " };
            // A symbol that lands exactly on this PC labels the line.
            let lbl = match self.label(pc) {
                Some(l) if l.find('+').is_none() => format!("  <{l}>"),
                _ => String::new(),
            };
            println!("{marker} {pc:08X}: {w:04X}  {}{lbl}", disasm(op));
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

    /// Dump the last `n` HIRQ-edge entries (M11 CD-timing alignment): each
    /// `old -> new (cause)` with the bits set (+) and cleared (-). The cause is
    /// the command mnemonic, or `readpump`/`w1c`. Diff this timeline against
    /// Mednafen's `cdb.cpp` HIRQ to pin the exact bit/edge the GFS server reads.
    fn hirqlog(&self, n: usize) {
        let log = &self.sat.bus.cd_block.hirq_log;
        if log.is_empty() {
            println!("(HIRQ-edge log empty — enabled at startup; run something first)");
            return;
        }
        let start = log.len().saturating_sub(n);
        for &(old, new, cause) in &log[start..] {
            let cause_s = match cause {
                0x100 => "readpump".to_string(),
                0x101 => "w1c".to_string(),
                c => format!("cmd:{}", cd_name(c as u8)),
            };
            println!(
                "  {old:04X} -> {new:04X}  {cause_s:<18} +{}  -{}",
                hirq_bits(new & !old),
                hirq_bits(old & !new),
            );
        }
        println!("({} entries; showed last {})", log.len(), n.min(log.len()));
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
            let m = &self.sat.master().regs;
            let (pc, regs) = (m.pc, m.r);
            if i > 0
                && self
                    .mbps
                    .iter()
                    .any(|(bp, g)| *bp == pc && g.is_none_or(|(idx, v)| regs[idx] == v))
            {
                println!("breakpoint @ {} (after {i} insns)", self.fmt_addr(pc));
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
    /// to `max_frames`, stopping when an armed breakpoint (master, slave, or
    /// 68k) snapshots. Honours the whole master/slave breakpoint *lists*; the
    /// hit's PC says which one fired.
    fn frame_cont(&mut self, max_frames: u64) {
        self.sat.set_master_bps(self.mbps.clone());
        self.sat.set_slave_bps(self.sbps.clone());
        // Arm the memory probe on whichever CPU breakpoint fires.
        self.sat.set_master_bp_probe(self.bp_probe);
        self.sat.set_slave_bp_probe(self.bp_probe);
        // Arm the SCSP 68k breakpoint (sound-driver debugging).
        self.sat.set_scsp_bp68(self.bp68);
        for f in 0..max_frames {
            self.sat.run_for(CYCLES_PER_FRAME);
            if let Some(h) = self.sat.take_scsp_bp68_hit() {
                println!("68k breakpoint hit at frame {f}: pc={:06X}", h.pc);
                println!(
                    "  d0-7: {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X}",
                    h.d[0], h.d[1], h.d[2], h.d[3], h.d[4], h.d[5], h.d[6], h.d[7]
                );
                println!(
                    "  a0-7: {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X}",
                    h.a[0], h.a[1], h.a[2], h.a[3], h.a[4], h.a[5], h.a[6], h.a[7]
                );
                println!("  sr: imask={} super={}", h.sr_imask, h.sr_super);
                return;
            }
            if let Some(h) = self.sat.take_master_bp_hit() {
                self.report_bp_hit("master", f, &h, true);
                return;
            }
            if let Some(h) = self.sat.take_slave_bp_hit() {
                self.report_bp_hit("slave", f, &h, false);
                return;
            }
        }
        println!(
            "ran {max_frames} frames; master @ {} slave @ {} disp={}",
            self.fmt_addr(self.sat.master().regs.pc),
            self.fmt_addr(self.sat.slave().regs.pc),
            self.sat.bus.vdp2.regs.display_enabled(),
        );
    }

    /// Print a captured SH-2 breakpoint hit: the PC (with symbol label), the
    /// probe value, R0..R15, a code preview, and — for the master — the
    /// best-effort stack call-chain (return-address-shaped stack words). No
    /// frame pointers on SH-2, so the chain is heuristic but reliably surfaces
    /// the callers; symbols annotate each.
    fn report_bp_hit(&mut self, which: &str, frame: u64, h: &saturn::scheduler::BpHit, chain: bool) {
        println!("{which} breakpoint hit at frame {frame}: pc={}", self.fmt_addr(h.pc));
        if let Some(a) = self.bp_probe {
            println!("  probe [{a:08X}] = {:08X} (raw WRAM via bus)", h.probe);
        }
        let r = h.regs;
        println!(
            "  r0-7:  {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X}",
            r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]
        );
        println!(
            "  r8-15: {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X} {:08X}",
            r[8], r[9], r[10], r[11], r[12], r[13], r[14], r[15]
        );
        println!("  pr={:08X} gbr={:08X} code={:04X?}", h.pr, h.gbr, &h.code[..h.code.len().min(8)]);
        if chain {
            let sp = r[15];
            println!("  stack call-chain (sp={sp:08X}, likely return addrs):");
            let mut shown = 0;
            for off in (0..0x100u32).step_by(4) {
                let w = self.read_mem(sp.wrapping_add(off), 4);
                let in_code = (0x0600_0000..0x0610_0000).contains(&w)
                    || (0x0000_0000..0x0008_0000).contains(&w);
                if in_code && w & 1 == 0 {
                    println!("    [{:08X}] = {}", sp.wrapping_add(off), self.fmt_addr(w));
                    shown += 1;
                    if shown == 16 {
                        break;
                    }
                }
            }
        }
    }

    /// Reference master-PC **trace-diff**: run ours through the real
    /// full-system path (`run_for_traced` — slave + peripherals advancing, the
    /// Mednafen-aligned interrupt timing) and compare the loop-collapsed master
    /// PC stream against a reference dump, stopping at the **first divergent PC**
    /// with a both-sides context window. This hosts the project's primary
    /// debugging methodology (the LLE↔Mednafen PC-trace-diff) inside the REPL,
    /// instead of generating two trace files and grep-diffing them by hand.
    ///
    /// Conventions, to line ours up with a Mednafen `SS_PCTRACE` log:
    /// - `TDIFF_ADD` (default 4) is added to our exec-PC before the compare —
    ///   Mednafen logs the *fetch*-PC (= our exec-PC + 4). Set `TDIFF_ADD=0` for
    ///   an exec-PC reference (e.g. one produced by `gen_vf2_pc_trace`).
    /// - `PCTRACE_LO` / `PCTRACE_HI` range-filter both sides *before* the
    ///   collapse, exactly like `gen_vf2_pc_trace`. Pick a window where BOTH
    ///   emulators log identically (e.g. `PCTRACE_LO=06000000` to keep work-RAM
    ///   and drop the cache-through `0x20xxxxxx` PCs Mednafen logs but we don't),
    ///   or the first "divergence" is a logging artifact, not a bug.
    /// - `PCTRACE_DELAYSLOTS=1` includes delay-slot PCs (honored by
    ///   `run_for_traced`).
    ///
    /// Advances the machine like `fc`/`run`; run it on a fresh load (or after
    /// `load`) so ours starts where the reference does.
    fn tdiff(&mut self, ref_path: &str, max_frames: u64) {
        let text = match std::fs::read_to_string(ref_path) {
            Ok(t) => t,
            Err(e) => {
                println!("read {ref_path} failed: {e}");
                return;
            }
        };
        let reference = parse_trace_pcs(&text);
        if reference.is_empty() {
            println!("no PCs parsed from {ref_path} (expected one hex PC per line)");
            return;
        }
        let env_hex = |k: &str, d: u32| -> u32 {
            std::env::var(k)
                .ok()
                .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
                .unwrap_or(d)
        };
        let add = env_hex("TDIFF_ADD", 4);
        let lo = env_hex("PCTRACE_LO", 0);
        let hi = env_hex("PCTRACE_HI", u32::MAX);
        println!(
            "tdiff: {} ref PCs · add=+{add} · range {lo:08X}..={hi:08X} · up to {max_frames} frames",
            reference.len()
        );

        // Snapshot the pre-trace state so the divergence can be re-run to a
        // breakpoint for exact register state (the frame-batched trace pass runs
        // past it). Cheap — external media is referenced, not embedded.
        let snap = self.sat.save_state();
        let mut recent: std::collections::VecDeque<u32> = std::collections::VecDeque::with_capacity(64);
        let mut cursor = 0usize; // index into `reference` matched so far
        let mut pcs: Vec<u32> = Vec::with_capacity(8_000_000);
        for f in 0..max_frames {
            pcs.clear();
            self.sat.run_for_traced(CYCLES_PER_FRAME, &mut pcs);
            for &raw in &pcs {
                let pc = raw.wrapping_add(add);
                if pc < lo || pc > hi {
                    continue;
                }
                if !collapse_should_log(&mut recent, pc) {
                    continue;
                }
                if cursor >= reference.len() {
                    println!(
                        "\n✓ matched all {} reference PCs (frame {f}); ours continues past the reference. No divergence.",
                        reference.len()
                    );
                    return;
                }
                if pc != reference[cursor] {
                    self.report_divergence(&reference, cursor, pc, add, f);
                    self.pin_divergence(&snap, pc.wrapping_sub(add), f + 2);
                    return;
                }
                cursor += 1;
            }
        }
        println!(
            "\nran {max_frames} frames; matched {cursor}/{} reference PCs, no divergence \
             (ours stopped at the frame budget — raise the frame count if the reference is longer).",
            reference.len()
        );
    }

    /// Print the first-divergence context: the matched-prefix tail (both sides
    /// agree), the diverging PC on each side, the reference's continuation, and
    /// a disassembly of what *ours* executed there.
    fn report_divergence(&mut self, reference: &[u32], i: usize, our_pc: u32, add: u32, frame: u64) {
        const TAIL: usize = 6; // matched context before the split
        const AHEAD: usize = 8; // reference continuation after the split
        let exec = our_pc.wrapping_sub(add);
        println!("\n*** DIVERGENCE at logged PC #{i} (frame {frame}) ***");
        println!("  common prefix tail (both sides agree):");
        let from = i.saturating_sub(TAIL);
        for (k, &pc) in reference[from..i].iter().enumerate() {
            println!("    #{:<7} {pc:08X}", from + k);
        }
        println!("  ----- diverge -----");
        println!("    ref  #{i} -> {:08X}", reference[i]);
        println!("    ours #{i} -> {our_pc:08X}   (exec-PC {exec:08X})");
        println!("  reference would continue:");
        for &pc in &reference[i..(i + AHEAD).min(reference.len())] {
            println!("    {pc:08X}");
        }
        println!("  ours executed @ {exec:08X}:");
        self.dump_disasm(exec, 4);
    }

    /// After a divergence, rewind to the pre-trace snapshot and re-run to a
    /// breakpoint at the divergent PC, so its full register state + call-chain
    /// are captured (the frame-batched trace pass runs past it). The machine is
    /// left parked there for `r`/`m`/`d` inspection.
    ///
    /// This lands on the **first execution** of the divergent PC. For a
    /// control-flow fork (ours branches somewhere the matched prefix never went)
    /// that *is* the divergence; for a divergence inside a loop that matched
    /// several iterations first, it lands on the loop's first pass — re-run with
    /// a register-guarded breakpoint (`b {pc} <ri> <v>`) to pin the right one.
    fn pin_divergence(&mut self, snap: &[u8], exec: u32, max_frames: u64) {
        if self.sat.load_state(snap).is_err() {
            println!("  (could not rewind to capture registers at the divergence)");
            return;
        }
        self.sat.bus.cd_block.cmd_log_on = true;
        self.sat.set_master_bps(vec![(exec, None)]);
        self.sat.set_master_bp_probe(self.bp_probe);
        for f in 0..max_frames {
            self.sat.run_for(CYCLES_PER_FRAME);
            if let Some(h) = self.sat.take_master_bp_hit() {
                println!("\n  registers at the divergence (rewound + re-run to {exec:08X}):");
                self.report_bp_hit("  divergence", f, &h, true);
                println!("  machine parked here — inspect with `r` / `m` / `d`.");
                return;
            }
        }
        println!("  (re-run did not reach {exec:08X} within {max_frames} frames)");
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
            "tdiff" => match a1 {
                Some(path) => self.tdiff(path, a2.and_then(parse_dec).unwrap_or(600)),
                None => println!("usage: tdiff <ref-pc-trace> [max_frames]"),
            },
            "bw" => match a1.and_then(|t| self.resolve(t)) {
                Some(addr) => self.write_break(
                    addr,
                    a2.and_then(parse_num),
                    a3.and_then(parse_dec).unwrap_or(6000),
                ),
                None => println!("usage: bw <addr|sym> [val-hex] [max-frames]"),
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
            // `b` (no arg) lists; `b <addr> [ri v]` adds a master breakpoint.
            "b" => match a1 {
                None => self.list_bps(),
                Some(tok) => match self.resolve(tok) {
                    Some(pc) => {
                        let g = Self::parse_guard(a2, a3);
                        self.mbps.push((pc, g));
                        println!("master bp #{} @ {}", self.mbps.len() - 1, self.fmt_addr(pc));
                    }
                    None => println!("unknown symbol/address {tok:?}"),
                },
            },
            "bs" => match a1 {
                None => self.list_bps(),
                Some(tok) => match self.resolve(tok) {
                    Some(pc) => {
                        let g = Self::parse_guard(a2, a3);
                        self.sbps.push((pc, g));
                        println!("slave bp #{} @ {}", self.mbps.len() + self.sbps.len() - 1, self.fmt_addr(pc));
                    }
                    None => println!("unknown symbol/address {tok:?}"),
                },
            },
            "bd" => match a1 {
                Some("*") => {
                    self.mbps.clear();
                    self.sbps.clear();
                    self.bp68 = None;
                    println!("all breakpoints cleared");
                }
                Some(s) => match parse_dec(s) {
                    Some(id) => self.delete_bp(id as usize),
                    None => println!("usage: bd <id|*>"),
                },
                None => println!("usage: bd <id|*>  (ids from `b`)"),
            },
            "sym" => match (a1, a2.and_then(|t| self.resolve(t))) {
                (Some(name), Some(addr)) => {
                    self.define_sym(name, addr);
                    println!("sym {name} = {addr:08X}");
                }
                _ => println!("usage: sym <name> <addr>"),
            },
            "syms" => match a1 {
                Some(path) => self.load_syms(path),
                None => {
                    if self.syms.is_empty() {
                        println!("(no symbols; `syms <file>` or `sym <name> <addr>`)");
                    } else {
                        let mut s = self.syms.clone();
                        s.sort_by_key(|(_, a)| *a);
                        for (n, a) in &s {
                            println!("  {a:08X}  {n}");
                        }
                    }
                }
            },
            "b68" => match a1.and_then(|t| self.resolve(t)) {
                Some(pc) => {
                    // Optional register guard: `b68 <addr> <reg> <val>` where reg
                    // 0-7 = D0-D7, 8-15 = A0-A7.
                    let guard = match (a2.and_then(parse_dec), a3.and_then(parse_num)) {
                        (Some(idx), Some(val)) if idx < 16 => Some((idx as u8, val)),
                        _ => None,
                    };
                    self.bp68 = Some((pc, guard));
                    match guard {
                        Some((ri, v)) => {
                            let name = if ri < 8 {
                                format!("D{ri}")
                            } else {
                                format!("A{}", ri - 8)
                            };
                            println!("68k bp @ {pc:06X} when {name}=={v:08X} (arm with `fc`)");
                        }
                        None => println!("68k bp @ {pc:06X} (arm with `fc`)"),
                    }
                }
                None => {
                    self.bp68 = None;
                    println!("68k bp cleared");
                }
            },
            "probe" => match a1.map(|t| self.resolve(t)) {
                Some(Some(addr)) => {
                    self.bp_probe = Some(addr);
                    println!("bp probe @ {addr:08X} (raw WRAM captured on next bp hit)");
                }
                Some(None) => println!("unknown symbol/address"),
                None => {
                    self.bp_probe = None;
                    println!("bp probe cleared");
                }
            },
            "m" | "x" => match a1.and_then(|t| self.resolve(t)) {
                Some(addr) => self.dump_mem(addr, a2.and_then(parse_num).unwrap_or(64)),
                None => println!("usage: m <addr|sym> [len]"),
            },
            "d" | "dis" => {
                let addr = a1
                    .and_then(|t| self.resolve(t))
                    .unwrap_or(self.sat.master().regs.pc);
                self.dump_disasm(addr, a2.and_then(parse_num).unwrap_or(16));
            }
            "d68" => match a1.and_then(parse_num) {
                Some(addr) => self.disasm68(
                    addr,
                    a2.and_then(parse_dec).map(|n| n as usize).unwrap_or(16),
                ),
                None => println!("usage: d68 <68k-addr> [n]  (SCSP sound 68k)"),
            },
            "t68" => self.trace68(a1.and_then(parse_dec).map(|n| n as usize).unwrap_or(64)),
            "cd" => self.cd_state(),
            "frt" => self.dump_frt(),
            "cache" => self.dump_cache(),
            "caudit" => self.cache_audit(),
            "stale" => self.detect_stale(a1.and_then(parse_dec).unwrap_or(2000)),
            "cdlog" => self.cdlog(a1.and_then(parse_dec).map(|n| n as usize).unwrap_or(20)),
            "hirqlog" => self.hirqlog(a1.and_then(parse_dec).map(|n| n as usize).unwrap_or(40)),
            "vdp" => println!(
                "VDP2 display_enabled={}  VDP1 drawing={}",
                self.sat.bus.vdp2.regs.display_enabled(),
                self.sat.bus.vdp1.is_drawing(),
            ),
            "scsp" => {
                // a6 = sound-driver work-area base. The driver's command ring
                // lives at a6+0x1840 (write ptr) / a6+0x1842 (read ptr): the main
                // loop (0x1066) keys a voice only when they differ. Capture a6 and
                // the bytes around 0x1840 first (needs &mut self) so we can watch
                // whether the BIOS master ever queues a sound command.
                let a6 = self.sat.bus.scsp.cpu.regs.a[6];
                let ring = self.read_sound_words(a6.wrapping_add(0x1840), 4);
                let s = &self.sat.bus.scsp;
                let active = (0..32).filter(|&i| s.slot_active(i)).count();
                println!(
                    "SCSP running(SNDON)={}  active slots={}/32",
                    s.running, active
                );
                let c = &s.cpu;
                println!(
                    "  68k pc={:06X} stopped={} a7={:08X}  d0={:08X} d1={:08X} a0={:08X} a1={:08X}",
                    c.regs.pc,
                    c.stopped,
                    c.regs.a[7],
                    c.regs.d[0],
                    c.regs.d[1],
                    c.regs.a[0],
                    c.regs.a[1]
                );
                println!(
                    "  68k a6={a6:06X}  cmd-ring @a6+0x1840: {:04X} {:04X} {:04X} {:04X}  (write/read ptrs)",
                    ring[0], ring[1], ring[2], ring[3]
                );
                let (keyon_execs, slot_starts) = s.ctrl.dbg_keyon_counts();
                println!(
                    "  key-on activity (lifetime): KYONEX strobes={keyon_execs}  slot starts={slot_starts}"
                );
                let (tof, ttc, tts) = s.ctrl.dbg_timer_counts();
                println!(
                    "  timers (lifetime): overflow A/B/C={}/{}/{}  tick_timers calls={ttc} samples={tts}",
                    tof[0], tof[1], tof[2]
                );
                let (lvl, scieb, scipd) = s.ctrl.irq_state();
                println!(
                    "  68k IRQ: asserted_level={lvl}  SCIEB={scieb:04X} SCIPD={scipd:04X}  imask={} super={}",
                    c.regs.sr.imask, c.regs.sr.supervisor
                );
                let (dsp_run, efreg, efreg_hw, mixs_hw) = s.dsp_state();
                let efmax = efreg.iter().map(|&e| (e as i32).abs()).max().unwrap_or(0);
                println!(
                    "  DSP running={dsp_run} EFREG[0..4]={:?} max|EFREG|={efmax}",
                    &efreg[..4]
                );
                println!("  EFREG high-water: {efreg_hw:?}");
                println!("  MIXS  high-water: {mixs_hw:?}");
                println!(
                    "  DSP program writes EFREG indices (EWT targets): {:?}",
                    s.dsp_ewt_targets()
                );
                // Per-active-slot playback parameters — to tell a mis-programmed
                // slot (bad SA/pitch/loop) from a render bug (sane params).
                for i in 0..32 {
                    if !s.slot_active(i) {
                        continue;
                    }
                    let d = s.slot_debug(i);
                    println!(
                        "  slot{i:02} SA={:05X} loop={}[{:04X}..{:04X}] {} oct={:+} fns={:03X} step={:05X} eg={}/{:03X} | direct(disdl={} pan={:02X}) tl={:02X} | dsp-send(imxl={} isel={}) dsp-ret(efsdl={} efpan={:02X})",
                        d.sa,
                        d.lpctl,
                        d.lsa,
                        d.lea,
                        if d.pcm8 { "8b" } else { "16b" },
                        d.oct,
                        d.fns,
                        d.step,
                        d.eg_state,
                        d.eg_volume >> 16,
                        d.disdl,
                        d.dipan,
                        d.tl,
                        d.imxl,
                        d.isel,
                        d.efsdl,
                        d.efpan,
                    );
                }
            }
            "w" => match a1.and_then(|t| self.resolve(t)) {
                Some(addr) => {
                    let sz = a2.and_then(parse_num).unwrap_or(2) as u8;
                    self.watches.push((addr, sz));
                    println!("watch {addr:08X} ({sz}B); {} total", self.watches.len());
                }
                None => println!("usage: w <addr|sym> [size=1|2|4]"),
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
  tdiff <ref> [n]  master-PC trace-diff vs a reference dump (Mednafen SS_PCTRACE):
                  full-system run, stop at the FIRST divergent PC + context.
                  Envs: TDIFF_ADD (default 4 = Mednafen fetch-PC), PCTRACE_LO/HI
                  (filter to a window both log identically, e.g. LO=06000000)
  bw <addr> [val] [maxf]  full-system run until a bus write to <addr>
                  (optional ==val), report the storing instruction's PC
  cb <cmd> [cr4] [maxf]  full-system run until the CD logs host command <cmd>
                  (hex; optional CR_in[3] match), preserving setup in cdlog
  frame [n]       run n full frames (slave+VDP+CD advance)         [alias f]
  run <cyc>       run_for <cyc> master cycles (full system)
  b [addr] [ri v]  add a master breakpoint (addr = hex or symbol); guard: fire only when R[ri]==v.
                  `b` with no addr LISTS all breakpoints (with ids); honoured by both `c` and `fc`
  bs <addr> [ri v]  add a slave breakpoint (honoured by `fc`)
  bd <id|*>       delete breakpoint by id (from `b`), or `*` for all
  b68 <addr> [r v]  set/clear SCSP 68k breakpoint (guard: r 0-7=D, 8-15=A); use with fc
  sym <name> <addr>  define a symbol; syms <file> loads `name addr` pairs (`syms` lists them)
  probe [addr]    set/clear bp memory probe — capture raw-WRAM [addr] (via bus, no cache) on a bp hit
  w <addr> [sz]   add a poll watchpoint (size 1/2/4, default 2), checked in `c`
  dw              clear watchpoints
  find32 <v> [start] [len]  scan memory for a 32-bit value (default: high WRAM)
  find <bytes> [start] [len]  scan memory for a hex byte pattern
  regs            dump master + slave registers (r0-r15 each)       [alias r]
  m <addr> [len]  hex-dump memory (addr = hex or symbol)           [alias x]
  d [addr] [n]    disassemble n insns from addr/symbol (default: master pc) [alias dis]
  d68 <addr> [n]  disassemble n SCSP 68k insns at 68k addr (sound RAM)
  t68 [n]         dump last n 68k PCs (disassembled) from the trace ring
  cd              CD-block state (status/hirq/curfad/partitions)
  cdlog [n]       last n CD host commands
  hirqlog [n]     last n HIRQ-edge changes (old->new +set -clr, cause) [CD-timing]
  vdp             VDP display state
  scsp            SCSP sound state (SNDON running + active slots)
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
    let mut syms_path: Option<String> = None;
    for a in &args {
        if let Some(r) = a.strip_prefix("--region=") {
            region = match r {
                "us" | "usa" => saturn::smpc::region::NORTH_AMERICA,
                "eu" | "europe" => saturn::smpc::region::EUROPE_PAL,
                _ => saturn::smpc::region::JAPAN,
            };
        } else if let Some(t) = a.strip_prefix("--rtc=") {
            rtc = t.parse().unwrap_or(rtc);
        } else if let Some(t) = a.strip_prefix("--syms=") {
            syms_path = Some(t.to_string());
        } else if bios_path.is_none() {
            bios_path = Some(a.clone());
        } else if disc_path.is_none() {
            disc_path = Some(a.clone());
        }
    }
    let Some(bios_path) = bios_path else {
        eprintln!(
            "usage: sdbg <bios.bin> [disc.cue] [--region=jp|us|eu] [--rtc=<unix>] [--syms=<file>]"
        );
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
    // HIRQ-edge log on by default (M11 CD-timing alignment; `hirqlog`).
    sat.bus.cd_block.hirq_log_on = true;
    // SCSP 68k PC ring on by default (for `t68` sound-driver tracing).
    sat.bus.scsp.enable_68k_trace();

    let mut dbg = Dbg {
        sat,
        // Full-size so `frame`/`f` can't panic when VDP2 switches to hi-res
        // (run_frame asserts the buffer fits the active resolution).
        fb: vec![0u8; saturn::vdp2::FRAMEBUFFER_BYTES],
        mbps: Vec::new(),
        sbps: Vec::new(),
        syms: Vec::new(),
        bp68: None,
        bp_probe: None,
        watches: Vec::new(),
    };
    if let Some(path) = syms_path {
        dbg.load_syms(&path);
    }
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

#[cfg(test)]
mod tests {
    use super::{collapse_should_log, parse_trace_pcs};
    use std::collections::VecDeque;

    #[test]
    fn parse_trace_pcs_is_tolerant() {
        let t = "\
06004000
0x06004004
  06004008  ; trailing comment
0600400C\textra-field

not-a-pc
== header ==
0600 4010";
        // Bare hex, 0x-prefixed, leading whitespace, and trailing junk all parse
        // to the leading hex token; blank/non-hex/`==`-header lines are skipped;
        // `0600 4010` stops at the space → 0x0600.
        assert_eq!(
            parse_trace_pcs(t),
            vec![0x0600_4000, 0x0600_4004, 0x0600_4008, 0x0600_400C, 0x0600],
        );
    }

    #[test]
    fn collapse_suppresses_repeats_within_the_64_window() {
        let mut r = VecDeque::new();
        assert!(collapse_should_log(&mut r, 0x100), "first sight logs");
        assert!(collapse_should_log(&mut r, 0x200));
        assert!(!collapse_should_log(&mut r, 0x100), "repeat within window suppressed");
        assert!(!collapse_should_log(&mut r, 0x200));
        // Fill the window past 64 distinct PCs so 0x100 is evicted, then it logs
        // again — a spin that idles longer than the window re-logs one pass.
        for i in 0..64u32 {
            collapse_should_log(&mut r, 0x1000 + i);
        }
        assert!(collapse_should_log(&mut r, 0x100), "evicted PC logs again");
    }
}
