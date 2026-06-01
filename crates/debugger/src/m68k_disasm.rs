//! A compact MC68000 disassembler for `sdbg` — enough to read the SCSP sound
//! 68k (the M11 VF2 sound-driver investigation). It covers the common
//! instruction families (MOVE/MOVEA, the ALU groups, Bcc/BSR/BRA, MOVEQ,
//! ADDQ/SUBQ, Scc/DBcc, shifts/rotates, MOVEM, LEA/PEA, JMP/JSR, the immediate
//! group, and the line-4 system ops) and all 12 effective-address modes with
//! their extension words. Rare/68020+ encodings fall back to `dc.w $XXXX`.
//!
//! Decoding is driven by a `read16(addr)` closure over the 68k's address space
//! (the caller maps that to main-bus sound RAM at 0x05A0_0000); the disassembler
//! consumes extension words as it resolves operands and reports the byte length.

/// One disassembled instruction: rendered text + its length in bytes.
pub struct Insn {
    pub text: String,
    pub len: u32,
}

/// Reads consecutive 16-bit big-endian words starting at `pc`, tracking length.
struct Reader<'a> {
    read: &'a dyn Fn(u32) -> u16,
    at: u32,
}

impl Reader<'_> {
    fn w(&mut self) -> u16 {
        let v = (self.read)(self.at);
        self.at = self.at.wrapping_add(2);
        v
    }
    fn l(&mut self) -> u32 {
        ((self.w() as u32) << 16) | self.w() as u32
    }
}

const SZ_BWL: [&str; 3] = ["b", "w", "l"];
const CC: [&str; 16] = [
    "t", "f", "hi", "ls", "cc", "cs", "ne", "eq", "vc", "vs", "pl", "mi", "ge", "lt", "gt", "le",
];

/// Decode one instruction at `pc`. `read` returns the 16-bit word at a 68k addr.
pub fn disasm(read: &dyn Fn(u32) -> u16, pc: u32) -> Insn {
    let mut r = Reader { read, at: pc };
    let op = r.w();
    let text = decode(op, pc, &mut r);
    Insn {
        text,
        len: r.at.wrapping_sub(pc),
    }
}

/// Brief-extension index register (modes 6 and 7/3).
fn index(ext: u16) -> String {
    let an = ext & 0x8000 != 0;
    let reg = (ext >> 12) & 7;
    let sz = if ext & 0x0800 != 0 { "l" } else { "w" };
    format!("{}{reg}.{sz}", if an { "a" } else { "d" })
}

/// Render an effective address `(mode, reg)` of operand `size` (bytes), pulling
/// extension words from `r`. `size` only affects `#imm`.
fn ea(mode: u16, reg: u16, size: u32, r: &mut Reader) -> String {
    match mode {
        0 => format!("d{reg}"),
        1 => format!("a{reg}"),
        2 => format!("(a{reg})"),
        3 => format!("(a{reg})+"),
        4 => format!("-(a{reg})"),
        5 => format!("({:#x},a{reg})", r.w() as i16),
        6 => {
            let ext = r.w();
            format!("({:#x},a{reg},{})", (ext as i8), index(ext))
        }
        7 => match reg {
            0 => format!("({:#06x}).w", r.w()),
            1 => format!("({:#010x}).l", r.l()),
            2 => format!("({:#x},pc)", r.w() as i16),
            3 => {
                let ext = r.w();
                format!("({:#x},pc,{})", (ext as i8), index(ext))
            }
            4 => match size {
                1 => format!("#{:#x}", r.w() & 0xff),
                4 => format!("#{:#x}", r.l()),
                _ => format!("#{:#x}", r.w()),
            },
            _ => "?".into(),
        },
        _ => "?".into(),
    }
}

fn imm(size: u32, r: &mut Reader) -> u32 {
    match size {
        1 => (r.w() & 0xff) as u32,
        4 => r.l(),
        _ => r.w() as u32,
    }
}

fn branch_target(op: u16, pc: u32, r: &mut Reader) -> u32 {
    let d8 = op as i8;
    if d8 == 0 {
        pc.wrapping_add(2).wrapping_add(r.w() as i16 as u32) // 16-bit
    } else if d8 == -1 {
        pc.wrapping_add(2).wrapping_add(r.l()) // 32-bit (68020, but harmless)
    } else {
        pc.wrapping_add(2).wrapping_add(d8 as u32) // 8-bit
    }
}

fn decode(op: u16, pc: u32, r: &mut Reader) -> String {
    let mode = (op >> 3) & 7;
    let reg = op & 7;
    let dn = (op >> 9) & 7;
    match op >> 12 {
        0x0 => decode_immediate(op, r),
        0x1..=0x3 => decode_move(op, r),
        0x4 => decode_line4(op, pc, r),
        0x5 => {
            // ADDQ/SUBQ, or Scc/DBcc/TRAPcc when size field == 3.
            if (op >> 6) & 3 == 3 {
                let cc = CC[((op >> 8) & 0xf) as usize];
                if mode == 1 {
                    let base = pc.wrapping_add(2);
                    let disp = r.w() as i16 as u32;
                    return format!("db{cc} d{reg},{:#x}", base.wrapping_add(disp));
                }
                return format!("s{cc} {}", ea(mode, reg, 1, r));
            }
            let data = if dn == 0 { 8 } else { dn };
            let sz = ((op >> 6) & 3) as usize;
            let m = if op & 0x0100 != 0 { "subq" } else { "addq" };
            format!("{m}.{} #{data},{}", SZ_BWL[sz], ea(mode, reg, 1 << sz, r))
        }
        0x6 => {
            let cc = (op >> 8) & 0xf;
            let tgt = branch_target(op, pc, r);
            match cc {
                0 => format!("bra {tgt:#x}"),
                1 => format!("bsr {tgt:#x}"),
                _ => format!("b{} {tgt:#x}", CC[cc as usize]),
            }
        }
        0x7 => format!("moveq #{:#x},d{dn}", op as i8),
        0x8 => decode_or_div(op, r),
        0x9 => decode_addsub(op, "sub", r),
        0xb => decode_cmp_eor(op, r),
        0xc => decode_and_mul(op, r),
        0xd => decode_addsub(op, "add", r),
        0xe => decode_shift(op, r),
        _ => format!("dc.w {op:#06x}"),
    }
}

fn decode_immediate(op: u16, r: &mut Reader) -> String {
    let mode = (op >> 3) & 7;
    let reg = op & 7;
    let bitops = ["btst", "bchg", "bclr", "bset"];
    // Static bit ops: 0000 1000 tt mmmrrr (immediate bit number in next word).
    if op & 0xff00 == 0x0800 {
        let tt = ((op >> 6) & 3) as usize;
        let bit = r.w() & 0xff;
        return format!("{} #{bit},{}", bitops[tt], ea(mode, reg, 1, r));
    }
    // Dynamic bit ops: 0000 ddd 1 tt mmmrrr (Dn holds the bit number).
    if op & 0xf100 == 0x0100 {
        let tt = ((op >> 6) & 3) as usize;
        return format!("{} d{},{}", bitops[tt], (op >> 9) & 7, ea(mode, reg, 1, r));
    }
    let names = ["ori", "andi", "subi", "addi", "?", "eori", "cmpi", "?"];
    let kind = ((op >> 9) & 7) as usize;
    // ORI/ANDI/EORI to CCR (#imm, byte, ea=0x3C) or SR (#imm, word, ea=0x7C).
    if matches!(kind, 0 | 1 | 5) {
        if op & 0x00ff == 0x003c {
            return format!("{}i #{:#x},ccr", names[kind], r.w() & 0xff);
        }
        if op & 0x00ff == 0x007c {
            return format!("{}i #{:#x},sr", names[kind], r.w());
        }
    }
    let sz = ((op >> 6) & 3) as usize;
    if sz == 3 || kind >= 7 {
        return format!("dc.w {op:#06x}");
    }
    let size = 1u32 << sz;
    let i = imm(size, r);
    format!("{}.{} #{i:#x},{}", names[kind], SZ_BWL[sz], ea(mode, reg, size, r))
}

fn decode_move(op: u16, r: &mut Reader) -> String {
    let size = match op >> 12 {
        1 => 1,
        3 => 2,
        _ => 4,
    };
    let szc = match size {
        1 => "b",
        2 => "w",
        _ => "l",
    };
    let src_mode = (op >> 3) & 7;
    let src_reg = op & 7;
    let src = ea(src_mode, src_reg, size, r);
    let dst_mode = (op >> 6) & 7;
    let dst_reg = (op >> 9) & 7;
    let dst = ea(dst_mode, dst_reg, size, r);
    let m = if dst_mode == 1 { "movea" } else { "move" };
    format!("{m}.{szc} {src},{dst}")
}

fn reg_list(mask: u16, predecrement: bool) -> String {
    // bit order: A7..A0 D7..D0 for -(An); D0..D7 A0..A7 otherwise.
    let bit = |i: usize| {
        if predecrement {
            (mask >> (15 - i)) & 1
        } else {
            (mask >> i) & 1
        }
    };
    let mut parts = Vec::new();
    for (base, pfx) in [(0usize, 'd'), (8usize, 'a')] {
        let mut i = 0;
        while i < 8 {
            if bit(base + i) == 1 {
                let start = i;
                while i < 8 && bit(base + i) == 1 {
                    i += 1;
                }
                if i - 1 == start {
                    parts.push(format!("{pfx}{start}"));
                } else {
                    parts.push(format!("{pfx}{start}-{pfx}{}", i - 1));
                }
            } else {
                i += 1;
            }
        }
    }
    parts.join("/")
}

fn decode_line4(op: u16, pc: u32, r: &mut Reader) -> String {
    let mode = (op >> 3) & 7;
    let reg = op & 7;
    // Fixed single-word ops.
    match op {
        0x4e70 => return "reset".into(),
        0x4e71 => return "nop".into(),
        0x4e72 => return format!("stop #{:#x}", r.w()),
        0x4e73 => return "rte".into(),
        0x4e75 => return "rts".into(),
        0x4e76 => return "trapv".into(),
        0x4e77 => return "rtr".into(),
        0x4afc => return "illegal".into(),
        _ => {}
    }
    if op & 0xfff0 == 0x4e40 {
        return format!("trap #{:#x}", op & 0xf);
    }
    if op & 0xfff8 == 0x4e50 {
        return format!("link a{reg},#{:#x}", r.w() as i16);
    }
    if op & 0xfff8 == 0x4e58 {
        return format!("unlk a{reg}");
    }
    if op & 0xfff8 == 0x4e60 {
        return format!("move a{reg},usp");
    }
    if op & 0xfff8 == 0x4e68 {
        return format!("move usp,a{reg}");
    }
    if op & 0xffc0 == 0x4ec0 {
        return format!("jmp {}", ea(mode, reg, 4, r));
    }
    if op & 0xffc0 == 0x4e80 {
        return format!("jsr {}", ea(mode, reg, 4, r));
    }
    if op & 0xf1c0 == 0x41c0 {
        return format!("lea {},a{}", ea(mode, reg, 4, r), (op >> 9) & 7);
    }
    if op & 0xffc0 == 0x4840 {
        return format!("pea {}", ea(mode, reg, 4, r));
    }
    if op & 0xffc0 == 0x44c0 {
        return format!("move {},ccr", ea(mode, reg, 2, r));
    }
    if op & 0xffc0 == 0x46c0 {
        return format!("move {},sr", ea(mode, reg, 2, r));
    }
    if op & 0xffc0 == 0x40c0 {
        return format!("move sr,{}", ea(mode, reg, 2, r));
    }
    if op & 0xffb8 == 0x4880 {
        // EXT.w / EXT.l
        let sz = if op & 0x40 != 0 { "l" } else { "w" };
        return format!("ext.{sz} d{reg}");
    }
    if op & 0xfff8 == 0x4840 {
        return format!("swap d{reg}");
    }
    if op & 0xffc0 == 0x4ac0 {
        return format!("tas {}", ea(mode, reg, 1, r));
    }
    // MOVEM (reg list <-> memory): 0100 1d00 1ss mmmrrr, d=direction.
    if op & 0xfb80 == 0x4880 {
        let to_mem = op & 0x0400 == 0;
        let sz = if op & 0x40 != 0 { "l" } else { "w" };
        let mask = r.w();
        let size = if op & 0x40 != 0 { 4 } else { 2 };
        let mem = ea(mode, reg, size, r);
        let list = reg_list(mask, to_mem && mode == 4);
        return if to_mem {
            format!("movem.{sz} {list},{mem}")
        } else {
            format!("movem.{sz} {mem},{list}")
        };
    }
    // CLR/NEG/NEGX/NOT/TST (single-operand line-4).
    let sz = ((op >> 6) & 3) as usize;
    if sz < 3 {
        let name = match (op >> 8) & 0xf {
            0 => Some("negx"),
            2 => Some("clr"),
            4 => Some("neg"),
            6 => Some("not"),
            0xa => Some("tst"),
            _ => None,
        };
        if let Some(n) = name {
            return format!("{n}.{} {}", SZ_BWL[sz], ea(mode, reg, 1 << sz, r));
        }
    }
    let _ = pc;
    format!("dc.w {op:#06x}")
}

fn decode_addsub(op: u16, base: &str, r: &mut Reader) -> String {
    let mode = (op >> 3) & 7;
    let reg = op & 7;
    let dn = (op >> 9) & 7;
    let opmode = (op >> 6) & 7;
    if opmode == 3 || opmode == 7 {
        // ADDA/SUBA: word (3) or long (7).
        let size = if opmode == 7 { 4 } else { 2 };
        let szc = if opmode == 7 { "l" } else { "w" };
        return format!("{base}a.{szc} {},a{dn}", ea(mode, reg, size, r));
    }
    let sz = (opmode & 3) as usize;
    let to_ea = opmode & 4 != 0;
    // ADDX/SUBX share the to-ea column with mode 0/1 + bit pattern.
    if to_ea && (mode == 0 || mode == 1) {
        let x = if mode == 0 { format!("d{reg}") } else { format!("-(a{reg})") };
        let xd = if mode == 0 { format!("d{dn}") } else { format!("-(a{dn})") };
        return format!("{base}x.{} {x},{xd}", SZ_BWL[sz]);
    }
    let operand = ea(mode, reg, 1 << sz, r);
    if to_ea {
        format!("{base}.{} d{dn},{operand}", SZ_BWL[sz])
    } else {
        format!("{base}.{} {operand},d{dn}", SZ_BWL[sz])
    }
}

fn decode_or_div(op: u16, r: &mut Reader) -> String {
    let mode = (op >> 3) & 7;
    let reg = op & 7;
    let dn = (op >> 9) & 7;
    let opmode = (op >> 6) & 7;
    if opmode == 3 {
        return format!("divu {},d{dn}", ea(mode, reg, 2, r));
    }
    if opmode == 7 {
        return format!("divs {},d{dn}", ea(mode, reg, 2, r));
    }
    let sz = (opmode & 3) as usize;
    let to_ea = opmode & 4 != 0;
    let operand = ea(mode, reg, 1 << sz, r);
    if to_ea {
        format!("or.{} d{dn},{operand}", SZ_BWL[sz])
    } else {
        format!("or.{} {operand},d{dn}", SZ_BWL[sz])
    }
}

fn decode_and_mul(op: u16, r: &mut Reader) -> String {
    let mode = (op >> 3) & 7;
    let reg = op & 7;
    let dn = (op >> 9) & 7;
    let opmode = (op >> 6) & 7;
    if opmode == 3 {
        return format!("mulu {},d{dn}", ea(mode, reg, 2, r));
    }
    if opmode == 7 {
        return format!("muls {},d{dn}", ea(mode, reg, 2, r));
    }
    // EXG: 1100 ddd 1 xxxxx rrr.
    if op & 0xf130 == 0xc100 {
        let m = (op >> 3) & 0x1f;
        return match m {
            0x08 => format!("exg d{dn},d{reg}"),
            0x09 => format!("exg a{dn},a{reg}"),
            0x11 => format!("exg d{dn},a{reg}"),
            _ => format!("and? {op:#06x}"),
        };
    }
    let sz = (opmode & 3) as usize;
    let to_ea = opmode & 4 != 0;
    let operand = ea(mode, reg, 1 << sz, r);
    if to_ea {
        format!("and.{} d{dn},{operand}", SZ_BWL[sz])
    } else {
        format!("and.{} {operand},d{dn}", SZ_BWL[sz])
    }
}

fn decode_cmp_eor(op: u16, r: &mut Reader) -> String {
    let mode = (op >> 3) & 7;
    let reg = op & 7;
    let dn = (op >> 9) & 7;
    let opmode = (op >> 6) & 7;
    if opmode == 3 || opmode == 7 {
        let size = if opmode == 7 { 4 } else { 2 };
        let szc = if opmode == 7 { "l" } else { "w" };
        return format!("cmpa.{szc} {},a{dn}", ea(mode, reg, size, r));
    }
    let sz = (opmode & 3) as usize;
    if opmode & 4 != 0 {
        // CMPM (mode 1) or EOR (other modes).
        if mode == 1 {
            return format!("cmpm.{} (a{reg})+,(a{dn})+", SZ_BWL[sz]);
        }
        return format!("eor.{} d{dn},{}", SZ_BWL[sz], ea(mode, reg, 1 << sz, r));
    }
    format!("cmp.{} {},d{dn}", SZ_BWL[sz], ea(mode, reg, 1 << sz, r))
}

fn decode_shift(op: u16, r: &mut Reader) -> String {
    let kinds = ["as", "ls", "rox", "ro"];
    if (op >> 6) & 3 == 3 {
        // Memory shift by one: 1110 0kk1 1 mmmrrr.
        let kind = kinds[((op >> 9) & 3) as usize];
        let dir = if op & 0x0100 != 0 { "l" } else { "r" };
        return format!("{kind}{dir} {}", ea((op >> 3) & 7, op & 7, 2, r));
    }
    let sz = ((op >> 6) & 3) as usize;
    let kind = kinds[((op >> 3) & 3) as usize];
    let dir = if op & 0x0100 != 0 { "l" } else { "r" };
    let reg = op & 7;
    let cr = (op >> 9) & 7;
    if op & 0x20 != 0 {
        format!("{kind}{dir}.{} d{cr},d{reg}", SZ_BWL[sz])
    } else {
        let cnt = if cr == 0 { 8 } else { cr };
        format!("{kind}{dir}.{} #{cnt},d{reg}", SZ_BWL[sz])
    }
}
