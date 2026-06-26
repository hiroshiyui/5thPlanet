//! VDP1 register bank — 24 bytes at `0x05D0_0000..=0x05D0_0017`.
//!
//! Eleven 16-bit registers. The low seven (0x00..0x0C) are writeable
//! control registers; the high four (0x10..0x16) are read-only status.
//!
//! ```text
//!   0x00  TVMR   TV Mode                       (W)
//!   0x02  FBCR   Frame Buffer Change Mode      (W)
//!   0x04  PTMR   Plot Trigger                  (W)
//!   0x06  EWDR   Erase/Write Data              (W)
//!   0x08  EWLR   Erase/Write Upper-Left coord  (W)
//!   0x0A  EWRR   Erase/Write Lower-Right coord (W)
//!   0x0C  ENDR   Draw Forced Termination       (W)
//!   0x10  EDSR   Transfer End Status           (R: bit1 CEF, bit0 BEF)
//!   0x12  LOPR   Last Operation Cmd Address    (R)
//!   0x14  COPR   Current Operation Cmd Address (R)
//!   0x16  MODR   Mode Status                   (R: version/mode)
//! ```
//!
//! The plotter (see [`super::plotter`]) drives the status registers:
//! `EDSR.CEF` (current-frame draw end), `COPR` (current command-table
//! address) and `LOPR` (last command-table address). BIOS/game code
//! writes `PTMR` to kick a plot then polls `EDSR` for the end flag;
//! the [`super::Vdp1`] aggregate runs the command list on that write
//! and calls the mutators here. This module is now pure register
//! state — the draw logic lives in the plotter.

const REG_BYTES: usize = 0x18;

/// EDSR.CEF — current-frame draw end. Set when a plot finishes.
pub const EDSR_CEF: u16 = 0x0002;

/// VDP1's 24-byte register bank (TVMR/FBCR/PTMR/… control + EDSR/LOPR/COPR/MODR
/// status). Plain register storage; the plotter and frame-change logic read it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Vdp1Regs {
    /// Writeable control registers, flat big-endian (offsets 0x00..0x0E).
    raw: [u8; REG_BYTES],
    /// EDSR draw-end status (read at 0x10): bit1 CEF, bit0 BEF.
    edsr: u16,
    /// LOPR (0x12) — last-operation command-table address (>>3).
    lopr: u16,
    /// COPR (0x14) — current-operation command-table address (>>3).
    copr: u16,
}

impl Default for Vdp1Regs {
    fn default() -> Self {
        Self::new()
    }
}

impl Vdp1Regs {
    pub fn new() -> Self {
        Self {
            raw: [0; REG_BYTES],
            // Power-on: no draw has run, so no end flag yet.
            edsr: 0,
            lopr: 0,
            copr: 0,
        }
    }

    fn idx(offset: u32) -> usize {
        (offset as usize) % REG_BYTES
    }

    /// Clear EDSR.CEF — the plotter calls this at the start of a list.
    pub fn cef_clear(&mut self) {
        self.edsr &= !EDSR_CEF;
    }
    /// Set EDSR.CEF — the plotter calls this when the list finishes.
    pub fn cef_set(&mut self) {
        self.edsr |= EDSR_CEF;
    }
    /// Latch the last/current command-table addresses (already >>3).
    pub fn set_command_addrs(&mut self, lopr: u16, copr: u16) {
        self.lopr = lopr;
        self.copr = copr;
    }
    /// Current PTMR value (plot-trigger mode bits live in 1:0).
    pub fn ptmr(&self) -> u16 {
        self.read16(0x04)
    }

    /// Read-only status registers are synthesized; everything else is
    /// flat storage.
    pub fn read16(&self, offset: u32) -> u16 {
        match offset & 0xFF {
            0x10 => self.edsr,
            0x12 => self.lopr,
            0x14 => self.copr,
            // MODR — mode/version. Bits 15..12 are a fixed version (1).
            0x16 => 0x1000,
            other => {
                let i = Self::idx(other);
                u16::from_be_bytes([self.raw[i], self.raw[(i + 1) % REG_BYTES]])
            }
        }
    }

    pub fn write16(&mut self, offset: u32, val: u16) {
        match offset & 0xFF {
            // Read-only status window: writes ignored.
            0x10 | 0x12 | 0x14 | 0x16 => {}
            // PTMR (0x04) and ENDR (0x0C) are stored here; the Vdp1
            // aggregate inspects them after the write to drive the
            // plotter, since the command list lives in VRAM.
            other => self.store16(other, val),
        }
    }

    fn store16(&mut self, offset: u32, val: u16) {
        let i = Self::idx(offset);
        let b = val.to_be_bytes();
        self.raw[i] = b[0];
        self.raw[(i + 1) % REG_BYTES] = b[1];
    }

    pub fn read8(&self, offset: u32) -> u8 {
        let w = self.read16(offset & !1);
        if offset & 1 == 0 {
            (w >> 8) as u8
        } else {
            w as u8
        }
    }
    pub fn write8(&mut self, offset: u32, val: u8) {
        let aligned = offset & !1;
        let cur = self.read16(aligned);
        let new = if offset & 1 == 0 {
            (cur & 0x00FF) | ((val as u16) << 8)
        } else {
            (cur & 0xFF00) | val as u16
        };
        self.write16(aligned, new);
    }
    pub fn read32(&self, offset: u32) -> u32 {
        ((self.read16(offset) as u32) << 16) | self.read16(offset + 2) as u32
    }
    pub fn write32(&mut self, offset: u32, val: u32) {
        self.write16(offset, (val >> 16) as u16);
        self.write16(offset + 2, val as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_registers_round_trip() {
        let mut r = Vdp1Regs::new();
        r.write16(0x00, 0x0001); // TVMR
        r.write16(0x06, 0xCAFE); // EWDR
        assert_eq!(r.read16(0x00), 0x0001);
        assert_eq!(r.read16(0x06), 0xCAFE);
    }

    #[test]
    fn cef_set_and_clear_drive_edsr() {
        let mut r = Vdp1Regs::new();
        assert_eq!(r.read16(0x10) & EDSR_CEF, 0, "no draw yet");
        r.cef_set();
        assert_eq!(r.read16(0x10) & EDSR_CEF, EDSR_CEF, "draw acked");
        r.cef_clear();
        assert_eq!(r.read16(0x10) & EDSR_CEF, 0, "cleared at list start");
    }

    #[test]
    fn ptmr_round_trips_for_the_aggregate_to_inspect() {
        let mut r = Vdp1Regs::new();
        r.write16(0x04, 0x0002); // PTMR — PTM = automatic draw
        assert_eq!(r.ptmr() & 0x03, 0x02);
        // Writing PTMR no longer auto-acks; the plotter owns CEF now.
        assert_eq!(r.read16(0x10) & EDSR_CEF, 0);
    }

    #[test]
    fn command_addr_registers_report_plotter_progress() {
        let mut r = Vdp1Regs::new();
        r.set_command_addrs(0x0040, 0x0080);
        assert_eq!(r.read16(0x12), 0x0040, "LOPR");
        assert_eq!(r.read16(0x14), 0x0080, "COPR");
    }

    #[test]
    fn status_registers_ignore_writes() {
        let mut r = Vdp1Regs::new();
        r.write16(0x12, 0xFFFF); // LOPR is read-only
        assert_eq!(r.read16(0x12), 0);
        r.write16(0x16, 0xFFFF); // MODR is read-only
        assert_eq!(r.read16(0x16), 0x1000);
    }

    #[test]
    fn modr_reports_fixed_version() {
        let r = Vdp1Regs::new();
        assert_eq!(r.read16(0x16), 0x1000);
    }

    #[test]
    fn byte_writes_compose_into_a_halfword() {
        let mut r = Vdp1Regs::new();
        // EWDR (0x06) built one byte at a time: high byte then low byte.
        r.write8(0x06, 0xAB);
        r.write8(0x07, 0xCD);
        assert_eq!(r.read16(0x06), 0xABCD);
        assert_eq!(r.read8(0x06), 0xAB, "high byte read-back");
        assert_eq!(r.read8(0x07), 0xCD, "low byte read-back");
    }

    #[test]
    fn word_access_spans_two_registers() {
        let mut r = Vdp1Regs::new();
        // A 32-bit write to EWLR (0x08) lands EWLR (hi) and EWRR (lo).
        r.write32(0x08, 0x0123_4567);
        assert_eq!(r.read16(0x08), 0x0123, "EWLR = high halfword");
        assert_eq!(r.read16(0x0A), 0x4567, "EWRR = low halfword");
        assert_eq!(r.read32(0x08), 0x0123_4567, "32-bit read-back");
    }

    #[test]
    fn read32_synthesizes_the_status_window() {
        let mut r = Vdp1Regs::new();
        r.cef_set();
        r.set_command_addrs(0x0040, 0x0080);
        // EDSR (0x10) | LOPR (0x12) as one 32-bit read.
        assert_eq!(r.read32(0x10), (EDSR_CEF as u32) << 16 | 0x0040);
        // COPR (0x14) | MODR (0x16).
        assert_eq!(r.read32(0x14), 0x0080 << 16 | 0x1000);
    }

    #[test]
    fn status_window_byte_writes_are_ignored() {
        let mut r = Vdp1Regs::new();
        r.cef_set(); // EDSR.CEF set
        r.write8(0x10, 0xFF); // EDSR is read-only — write must not stick
        r.write8(0x11, 0xFF);
        assert_eq!(r.read16(0x10), EDSR_CEF, "EDSR unchanged by byte writes");
    }
}
