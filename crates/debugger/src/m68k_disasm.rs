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
            return format!("{} #{:#x},ccr", names[kind], r.w() & 0xff);
        }
        if op & 0x00ff == 0x007c {
            return format!("{} #{:#x},sr", names[kind], r.w());
        }
    }
    let sz = ((op >> 6) & 3) as usize;
    if sz == 3 || kind >= 7 {
        return format!("dc.w {op:#06x}");
    }
    let size = 1u32 << sz;
    let i = imm(size, r);
    format!(
        "{}.{} #{i:#x},{}",
        names[kind],
        SZ_BWL[sz],
        ea(mode, reg, size, r)
    )
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
        let x = if mode == 0 {
            format!("d{reg}")
        } else {
            format!("-(a{reg})")
        };
        let xd = if mode == 0 {
            format!("d{dn}")
        } else {
            format!("-(a{dn})")
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Disassemble a word stream starting at pc 0; missing words read as 0.
    fn dis(words: &[u16]) -> (String, u32) {
        let read = |a: u32| -> u16 { words.get((a / 2) as usize).copied().unwrap_or(0) };
        let insn = disasm(&read, 0);
        (insn.text, insn.len)
    }

    /// Assert the rendered text (length checked separately where it matters).
    fn t(words: &[u16]) -> String {
        dis(words).0
    }

    #[test]
    fn immediate_group_ori_andi_subi_addi_eori_cmpi() {
        assert_eq!(dis(&[0x0001, 0x0012]), ("ori.b #0x12,d1".into(), 4));
        assert_eq!(t(&[0x0440, 0x1234]), "subi.w #0x1234,d0");
        assert_eq!(
            dis(&[0x0680, 0x1234, 0x5678]),
            ("addi.l #0x12345678,d0".into(), 6)
        );
        assert_eq!(t(&[0x0c00, 0x0042]), "cmpi.b #0x42,d0");
        assert_eq!(t(&[0x0a40, 0xbeef]), "eori.w #0xbeef,d0");
    }

    #[test]
    fn immediate_to_ccr_and_sr() {
        assert_eq!(t(&[0x023c, 0x00ff]), "andi #0xff,ccr");
        assert_eq!(t(&[0x007c, 0x2700]), "ori #0x2700,sr");
        assert_eq!(t(&[0x0a3c, 0x0010]), "eori #0x10,ccr");
    }

    #[test]
    fn static_and_dynamic_bit_ops() {
        assert_eq!(t(&[0x0800, 0x0005]), "btst #5,d0"); // bit number is decimal
        assert_eq!(t(&[0x08c0, 0x0007]), "bset #7,d0");
        assert_eq!(t(&[0x0100]), "btst d0,d0");
        assert_eq!(t(&[0x0342]), "bchg d1,d2");
    }

    #[test]
    fn immediate_dc_w_fallbacks() {
        // size field == 3 → unsupported.
        assert_eq!(t(&[0x00c0]), "dc.w 0x00c0");
    }

    #[test]
    fn move_and_movea_all_sizes() {
        assert_eq!(t(&[0x1200]), "move.b d0,d1");
        assert_eq!(t(&[0x2210]), "move.l (a0),d1");
        assert_eq!(t(&[0x3248]), "movea.w a0,a1");
        assert_eq!(dis(&[0x303c, 0x1234]), ("move.w #0x1234,d0".into(), 4));
        assert_eq!(t(&[0x22e0]), "move.l -(a0),(a1)+");
    }

    #[test]
    fn effective_address_modes() {
        assert_eq!(t(&[0x3028, 0x0010]), "move.w (0x10,a0),d0"); // (d16,An)
        // Negative disp renders as the two's-complement hex (Rust LowerHex of i16).
        assert_eq!(t(&[0x3028, 0xfff0]), "move.w (0xfff0,a0),d0");
        assert_eq!(t(&[0x3030, 0x1004]), "move.w (0x4,a0,d1.w),d0"); // (d8,An,Xn)
        assert_eq!(t(&[0x3030, 0x8804]), "move.w (0x4,a0,a0.l),d0"); // An index, .l
        assert_eq!(t(&[0x3038, 0x1234]), "move.w (0x1234).w,d0"); // abs.w
        assert_eq!(t(&[0x3039, 0x0012, 0x3456]), "move.w (0x00123456).l,d0"); // abs.l
        assert_eq!(t(&[0x303a, 0x0010]), "move.w (0x10,pc),d0"); // (d16,PC)
        assert_eq!(t(&[0x303b, 0x1004]), "move.w (0x4,pc,d1.w),d0"); // (d8,PC,Xn)
    }

    #[test]
    fn branches_quick_and_conditional() {
        assert_eq!(t(&[0x6004]), "bra 0x6"); // 8-bit
        assert_eq!(t(&[0x6106]), "bsr 0x8");
        assert_eq!(dis(&[0x6600, 0x0010]), ("bne 0x12".into(), 4)); // 16-bit
        assert_eq!(dis(&[0x60ff, 0x0001, 0x0000]), ("bra 0x10002".into(), 6)); // 32-bit
        assert_eq!(t(&[0x6f08]), "ble 0xa");
    }

    #[test]
    fn moveq_addq_subq_scc_dbcc() {
        assert_eq!(dis(&[0x7042]), ("moveq #0x42,d0".into(), 2));
        assert_eq!(t(&[0x5200]), "addq.b #1,d0");
        assert_eq!(t(&[0x5108]), "subq.b #8,a0"); // data 0 → 8
        assert_eq!(t(&[0x54c0]), "scc d0"); // size==3, mode!=1 → Scc
        assert_eq!(t(&[0x57c8, 0x0010]), "dbeq d0,0x12"); // Scc size + mode 1 → DBcc
    }

    #[test]
    fn or_and_div_groups() {
        assert_eq!(t(&[0x8250]), "or.w (a0),d1");
        assert_eq!(t(&[0x8350]), "or.w d1,(a0)");
        assert_eq!(t(&[0x82d0]), "divu (a0),d1");
        assert_eq!(t(&[0x83d0]), "divs (a0),d1");
    }

    #[test]
    fn and_mul_exg_groups() {
        assert_eq!(t(&[0xc250]), "and.w (a0),d1");
        assert_eq!(t(&[0xc350]), "and.w d1,(a0)");
        assert_eq!(t(&[0xc2d0]), "mulu (a0),d1");
        assert_eq!(t(&[0xc3d0]), "muls (a0),d1");
        assert_eq!(t(&[0xc342]), "exg d1,d2");
        assert_eq!(t(&[0xc34a]), "exg a1,a2");
        assert_eq!(t(&[0xc38a]), "exg d1,a2");
    }

    #[test]
    fn add_sub_adda_suba_addx_subx() {
        assert_eq!(t(&[0x9250]), "sub.w (a0),d1");
        assert_eq!(t(&[0x92c8]), "suba.w a0,a1");
        assert_eq!(t(&[0x93c8]), "suba.l a0,a1");
        assert_eq!(t(&[0x9300]), "subx.b d0,d1");
        assert_eq!(t(&[0x9509]), "subx.b -(a1),-(a2)");
        assert_eq!(t(&[0xd390]), "add.l d1,(a0)");
        assert_eq!(t(&[0xd2c8]), "adda.w a0,a1");
    }

    #[test]
    fn cmp_cmpa_cmpm_eor() {
        assert_eq!(t(&[0xb250]), "cmp.w (a0),d1");
        assert_eq!(t(&[0xb2c8]), "cmpa.w a0,a1");
        assert_eq!(t(&[0xb3c8]), "cmpa.l a0,a1");
        assert_eq!(t(&[0xb308]), "cmpm.b (a0)+,(a1)+");
        assert_eq!(t(&[0xb342]), "eor.w d1,d2");
    }

    #[test]
    fn shifts_and_rotates() {
        assert_eq!(t(&[0xe242]), "asr.w #1,d2"); // immediate count
        assert_eq!(t(&[0xe042]), "asr.w #8,d2"); // count 0 → 8
        assert_eq!(t(&[0xe3aa]), "lsl.l d1,d2"); // register count
        assert_eq!(t(&[0xe4d0]), "roxr (a0)"); // memory shift by one
        assert_eq!(t(&[0xe7d0]), "rol (a0)");
    }

    #[test]
    fn line4_fixed_single_word_ops() {
        assert_eq!(t(&[0x4e70]), "reset");
        assert_eq!(t(&[0x4e71]), "nop");
        assert_eq!(dis(&[0x4e72, 0x2700]), ("stop #0x2700".into(), 4));
        assert_eq!(t(&[0x4e73]), "rte");
        assert_eq!(t(&[0x4e75]), "rts");
        assert_eq!(t(&[0x4e76]), "trapv");
        assert_eq!(t(&[0x4e77]), "rtr");
        assert_eq!(t(&[0x4afc]), "illegal");
    }

    #[test]
    fn line4_trap_link_unlk_usp() {
        assert_eq!(t(&[0x4e45]), "trap #0x5");
        assert_eq!(dis(&[0x4e50, 0x0010]), ("link a0,#0x10".into(), 4));
        assert_eq!(t(&[0x4e59]), "unlk a1");
        assert_eq!(t(&[0x4e62]), "move a2,usp");
        assert_eq!(t(&[0x4e6b]), "move usp,a3");
    }

    #[test]
    fn line4_jmp_jsr_lea_pea_move_ccr_sr() {
        assert_eq!(t(&[0x4ed0]), "jmp (a0)");
        assert_eq!(t(&[0x4e90]), "jsr (a0)");
        assert_eq!(t(&[0x43d0]), "lea (a0),a1");
        assert_eq!(t(&[0x4850]), "pea (a0)");
        assert_eq!(t(&[0x44c0]), "move d0,ccr");
        assert_eq!(t(&[0x46c0]), "move d0,sr");
        assert_eq!(t(&[0x40c0]), "move sr,d0");
    }

    #[test]
    fn line4_ext_tas_clr_neg_not_tst() {
        assert_eq!(t(&[0x4880]), "ext.w d0");
        assert_eq!(t(&[0x48c0]), "ext.l d0");
        assert_eq!(t(&[0x4ad0]), "tas (a0)");
        assert_eq!(t(&[0x4200]), "clr.b d0");
        assert_eq!(t(&[0x4041]), "negx.w d1");
        assert_eq!(t(&[0x4490]), "neg.l (a0)");
        assert_eq!(t(&[0x4642]), "not.w d2");
        assert_eq!(t(&[0x4a83]), "tst.l d3");
        assert_eq!(t(&[0x4100]), "dc.w 0x4100"); // line-4 fallback
    }

    #[test]
    fn movem_to_and_from_memory_with_reg_lists() {
        // store d0-d1 to -(a2): predecrement bit order (d0 = bit 15).
        assert_eq!(dis(&[0x48a2, 0xc000]), ("movem.w d0-d1,-(a2)".into(), 4));
        // load (a0)+ into d0-d1: ascending bit order (d0 = bit 0).
        assert_eq!(t(&[0x4cd8, 0x0003]), "movem.l (a0)+,d0-d1");
        // mixed singles, ranges, and an address register.
        assert_eq!(t(&[0x4cd0, 0x801d]), "movem.l (a0),d0/d2-d4/a7");
    }

    #[test]
    fn top_level_dc_w_fallback() {
        assert_eq!(t(&[0xa000]), "dc.w 0xa000");
        assert_eq!(t(&[0xf000]), "dc.w 0xf000");
    }
}
