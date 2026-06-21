//! A tiny SH-2 encoder + program-image builder for the diagnostics suite.
//!
//! Only the handful of instruction forms the checks need — each `fn` returns
//! the 16-bit big-endian opcode word, mirroring the decoder-verified encodings
//! in [`sh2::decoder`] (and the test-local encoder in
//! `crates/saturn/tests/audio_pipeline.rs`). `assemble` lays a program out the
//! way [`crate::system::Saturn::reset`] expects: the 32-bit reset **PC** at byte
//! 0 and **SP** at byte 4, then code from [`CODE_BASE`]. SH-2 is big-endian, so
//! every word/longword is stored `to_be_bytes`.
//!
//! These programs run from reset with **no BIOS** (the image *is* the boot ROM
//! at `0x0000_0000`), so they must be self-contained and end in a spin
//! ([`bra_self`] + [`NOP`] delay slot). A diagnostics check reads the result the
//! program wrote to Work RAM.

/// Where code starts (4-byte aligned, leaving room for the reset vectors).
pub const CODE_BASE: u32 = 0x20;
/// Stack pointer the programs run with (top of High Work RAM, as in
/// `audio_pipeline.rs`). The checks don't push, but `reset` loads it into R15.
pub const STACK: u32 = 0x0601_0000;

pub const NOP: u16 = 0x0009;

/// `MOV #imm, Rn` — load an 8-bit signed immediate (`0xExii`).
pub fn mov_imm(rn: u16, imm: i8) -> u16 {
    0xE000 | (rn << 8) | (imm as u8 as u16)
}
/// `ADD #imm, Rn` — add an 8-bit signed immediate (`0x7nii`).
pub fn add_imm(rn: u16, imm: i8) -> u16 {
    0x7000 | (rn << 8) | (imm as u8 as u16)
}
/// `ADD Rm, Rn` (`0x3nmC`).
pub fn add(rn: u16, rm: u16) -> u16 {
    0x300C | (rn << 8) | (rm << 4)
}
/// `SUB Rm, Rn` (`0x3nm8`).
pub fn sub(rn: u16, rm: u16) -> u16 {
    0x3008 | (rn << 8) | (rm << 4)
}
/// `MUL.L Rm, Rn` — 32×32→MACL (`0x0nm7`).
pub fn mul_l(rn: u16, rm: u16) -> u16 {
    0x0007 | (rn << 8) | (rm << 4)
}
/// `STS MACL, Rn` (`0x0n1A`).
pub fn sts_macl(rn: u16) -> u16 {
    0x001A | (rn << 8)
}
/// `MOV.L Rm, @Rn` — store Rm to the address in Rn (`0x2nm2`, decodes `MovLS`).
pub fn movl_store(addr_rn: u16, src_rm: u16) -> u16 {
    0x2002 | (addr_rn << 8) | (src_rm << 4)
}
/// `MOV.L @Rm, Rn` — load from the address in Rm into Rn (`0x6nm2`, `MovLL`).
pub fn movl_load(dst_rn: u16, addr_rm: u16) -> u16 {
    0x6002 | (dst_rn << 8) | (addr_rm << 4)
}
/// `MOV.W @Rm, Rn` — load a sign-extended 16-bit word from @Rm (`0x6nm1`, `MovWL`).
pub fn movw_load(dst_rn: u16, addr_rm: u16) -> u16 {
    0x6001 | (dst_rn << 8) | (addr_rm << 4)
}
/// `AND Rm, Rn` (`0x2nm9`).
pub fn and_(rn: u16, rm: u16) -> u16 {
    0x2009 | (rn << 8) | (rm << 4)
}
/// `OR Rm, Rn` (`0x2nmB`).
pub fn or_(rn: u16, rm: u16) -> u16 {
    0x200B | (rn << 8) | (rm << 4)
}
/// `XOR Rm, Rn` (`0x2nmA`).
pub fn xor_(rn: u16, rm: u16) -> u16 {
    0x200A | (rn << 8) | (rm << 4)
}
/// `SHLL Rn` — logical shift left 1 (`0x4n00`).
pub fn shll(rn: u16) -> u16 {
    0x4000 | (rn << 8)
}
/// `SHLR Rn` — logical shift right 1 (`0x4n01`).
pub fn shlr(rn: u16) -> u16 {
    0x4001 | (rn << 8)
}
/// `SHLL8 Rn` — logical shift left 8 (`0x4n18`).
pub fn shll8(rn: u16) -> u16 {
    0x4018 | (rn << 8)
}
/// `SHLL16 Rn` — logical shift left 16 (`0x4n28`).
pub fn shll16(rn: u16) -> u16 {
    0x4028 | (rn << 8)
}
/// `BRA .` — branch to self (`disp = -2`); pair with a [`NOP`] delay slot to
/// park the CPU after the program's work is done.
pub const fn bra_self() -> u16 {
    0xAFFE
}

/// Build a runnable program image: reset PC at byte 0, SP at byte 4, then the
/// code words from [`CODE_BASE`]. Append `bra_self()` + `NOP` yourself as the
/// last two words so the CPU parks. Entry PC is always [`CODE_BASE`].
pub fn assemble(code: &[u16]) -> Vec<u8> {
    let mut out = vec![0u8; CODE_BASE as usize + code.len() * 2];
    out[0..4].copy_from_slice(&CODE_BASE.to_be_bytes());
    out[4..8].copy_from_slice(&STACK.to_be_bytes());
    let mut off = CODE_BASE as usize;
    for &w in code {
        out[off..off + 2].copy_from_slice(&w.to_be_bytes());
        off += 2;
    }
    out
}
