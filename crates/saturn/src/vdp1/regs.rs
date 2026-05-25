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
//! **NOT a VDP1 emulation.** There is no plotter (M5). The one piece
//! of behaviour modeled here is the draw-end handshake: BIOS init
//! writes `PTMR` to kick a plot, then polls `EDSR` for the end flag.
//! Since we don't actually plot, we set `EDSR.CEF` (current-end-flag)
//! synchronously on a `PTMR` write so the poll completes instead of
//! spinning forever.

const REG_BYTES: usize = 0x18;

/// EDSR.CEF — current-frame draw end. Set when a plot "finishes".
const EDSR_CEF: u16 = 0x0002;

#[derive(Clone, Debug)]
pub struct Vdp1Regs {
    /// Writeable control registers, flat big-endian (offsets 0x00..0x0E).
    raw: [u8; REG_BYTES],
    /// Synthesized EDSR draw-end status (read at 0x10).
    edsr: u16,
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
        }
    }

    fn idx(offset: u32) -> usize {
        (offset as usize) % REG_BYTES
    }

    /// Read-only status registers are synthesized; everything else is
    /// flat storage.
    pub fn read16(&self, offset: u32) -> u16 {
        match offset & 0xFF {
            0x10 => self.edsr,
            // LOPR / COPR — last & current command-table addresses. No
            // plotter, so both read as 0 (table start).
            0x12 | 0x14 => 0,
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
            0x04 => {
                // PTMR — plot trigger. Latch storage and ack the draw
                // immediately (no real plotter; see module docs).
                self.store16(0x04, val);
                self.edsr |= EDSR_CEF;
            }
            0x0C => {
                // ENDR — forced draw termination. Also raises the end
                // flag so a waiting poll completes.
                self.store16(0x0C, val);
                self.edsr |= EDSR_CEF;
            }
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
    fn plot_trigger_sets_edsr_end_flag() {
        let mut r = Vdp1Regs::new();
        assert_eq!(r.read16(0x10) & EDSR_CEF, 0, "no draw yet");
        r.write16(0x04, 0x0001); // PTMR — start plot
        assert_eq!(r.read16(0x10) & EDSR_CEF, EDSR_CEF, "draw must ack");
    }

    #[test]
    fn forced_termination_also_acks() {
        let mut r = Vdp1Regs::new();
        r.write16(0x0C, 0x0000); // ENDR
        assert_eq!(r.read16(0x10) & EDSR_CEF, EDSR_CEF);
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
}
