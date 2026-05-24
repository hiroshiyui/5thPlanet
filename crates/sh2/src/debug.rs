//! Disassembly and state-dump helpers used by tests and the ROM harness.
//! Filled out alongside the ISA table in task #2.

use crate::isa::Op;

/// Render a decoded [`Op`] in human-readable form.
pub fn disasm(op: Op) -> alloc::string::String {
    use core::fmt::Write as _;
    let mut s = alloc::string::String::new();
    match op {
        Op::Illegal(w) => {
            let _ = write!(s, ".word 0x{:04x}", w);
        }
        Op::Nop => s.push_str("nop"),
        // Full pretty-printer lands with the interpreter (task #3). Until then
        // we round-trip the Op's Debug repr so traces stay readable.
        other => {
            let _ = write!(s, "{:?}", other);
        }
    }
    s
}
