//! VDP1 command table entry — one 32-byte draw command.
//!
//! VDP1 draws by walking a *command table* in VRAM. Each command is
//! 0x20 bytes (16 big-endian halfwords). The first word (`CMDCTRL`)
//! carries the end bit, the jump-mode select, the character-read
//! direction and the command type; the rest are command-specific data.
//! Layout is the VDP1 User's Manual §"Command Tables":
//!
//! ```text
//!   00 CMDCTRL  e jjj zzzz .. dd cccc   end / jump / zoom / dir / type
//!   02 CMDLINK  link>>2                 next command (jump targets)
//!   04 CMDPMOD  draw mode (colour mode, calc, gouraud, clip, SPD, ...)
//!   06 CMDCOLR  colour bank / lookup base
//!   08 CMDSRCA  character address >>3
//!   0a CMDSIZE  char size: x in 8-px units (bits 13-8), y in lines (7-0)
//!   0c CMDXA/YA … 1a CMDXD/YD            up to four vertices
//!   1c CMDGRDA  gouraud shading table address >>3
//!   1e (unused)
//! ```
//!
//! Fields are extracted up front so the plotter never re-parses VRAM.

use super::vram::Vram;

/// One decoded VDP1 command. Field names mirror the hardware register
/// mnemonics so the plotter reads like the manual.
#[derive(Clone, Copy, Debug, Default)]
pub struct Command {
    pub ctrl: u16,
    pub link: u16,
    pub pmod: u16,
    pub colr: u16,
    pub srca: u16,
    pub size: u16,
    pub xa: u16,
    pub ya: u16,
    pub xb: u16,
    pub yb: u16,
    pub xc: u16,
    pub yc: u16,
    pub xd: u16,
    pub yd: u16,
    pub grda: u16,
    /// Set by the dispatcher when this command draws an untextured
    /// primitive (polygon / line / polyline) so the pixel path uses
    /// `CMDCOLR` directly instead of a character fetch.
    pub ispoly: bool,
}

impl Command {
    /// Read the 32-byte command at byte offset `addr` in VRAM.
    pub fn read(vram: &Vram, addr: u32) -> Self {
        let w = |off: u32| vram.read16(addr.wrapping_add(off));
        Self {
            ctrl: w(0x00),
            link: w(0x02),
            pmod: w(0x04),
            colr: w(0x06),
            srca: w(0x08),
            size: w(0x0A),
            xa: w(0x0C),
            ya: w(0x0E),
            xb: w(0x10),
            yb: w(0x12),
            xc: w(0x14),
            yc: w(0x16),
            xd: w(0x18),
            yd: w(0x1A),
            grda: w(0x1C),
            ispoly: false,
        }
    }

    /// CMDCTRL bit 15 — end of command list. The hardware terminates
    /// before processing whenever this bit is set, regardless of the
    /// other CMDCTRL fields (VDP1 manual §"End Bit").
    #[inline]
    pub fn is_end(&self) -> bool {
        self.ctrl & 0x8000 != 0
    }

    /// CMDCTRL bits 14-12 — jump-mode select (NEXT/ASSIGN/CALL/RETURN,
    /// each optionally with the SKIP flag from bit 14).
    #[inline]
    pub fn jump_mode(&self) -> u16 {
        self.ctrl & 0x7000
    }

    /// CMDCTRL bits 3-0 — command type (sprite / polygon / line / clip).
    #[inline]
    pub fn command_type(&self) -> u16 {
        self.ctrl & 0x000F
    }

    /// CMDCTRL bits 5-4 — character read direction (bit 0 = h-flip,
    /// bit 1 = v-flip).
    #[inline]
    pub fn direction(&self) -> u16 {
        (self.ctrl & 0x0030) >> 4
    }

    /// Character size in pixels: width in 8-pixel units (bits 13-8),
    /// height in lines (bits 7-0).
    #[inline]
    pub fn char_size(&self) -> (i32, i32) {
        let xsize = (((self.size & 0x3F00) >> 8) * 8) as i32;
        let ysize = (self.size & 0x00FF) as i32;
        (xsize, ysize)
    }

    /// Character data byte address: CMDSRCA << 3.
    #[inline]
    pub fn pattern_addr(&self) -> u32 {
        (self.srca as u32) * 8
    }
}
