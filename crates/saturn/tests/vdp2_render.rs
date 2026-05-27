//! End-to-end rendering through `Saturn::run_frame` (M3 task #6).
//!
//! Pre-loads VDP2 state (VRAM bitmap + CRAM palette + the right
//! registers to enable bitmap NBG0), runs one frame, and inspects
//! the resulting framebuffer at specific pixels. Proves the chain
//! Saturn → run_for → render_frame → output buffer is wired.

use saturn::Saturn;
use saturn::vdp2::{FRAME_HEIGHT, FRAME_WIDTH, FRAMEBUFFER_BYTES};
use sh2::bus::{AccessKind, Bus};

const REG_TVMD: u32 = 0x05F8_0000;
const REG_BGON: u32 = 0x05F8_0020;
const REG_CHCTLA: u32 = 0x05F8_0028;

#[test]
fn run_frame_returns_buffer_of_expected_size() {
    let mut sat = Saturn::with_blank_bios();
    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);
    assert_eq!(out.len(), FRAME_WIDTH * FRAME_HEIGHT * 4);
}

#[test]
fn display_off_yields_opaque_black_frame_even_after_running() {
    let mut sat = Saturn::with_blank_bios();
    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);
    for px in out.chunks_exact(4) {
        assert_eq!(px, &[0, 0, 0, 0xFF]);
    }
}

#[test]
fn bitmap_nbg0_through_run_frame_picks_up_synthetic_scene() {
    let mut sat = Saturn::with_blank_bios();
    // Halt the slave so its arbitrary state doesn't write anywhere
    // unexpected during the run.
    sat.halt_slave();

    // Program VDP2: DISP on, NBG0 on, bitmap mode.
    sat.bus.write16(REG_TVMD, 0x8000, AccessKind::Data);
    sat.bus.write16(REG_BGON, 0x0001, AccessKind::Data);
    sat.bus.write16(REG_CHCTLA, 0x0012, AccessKind::Data); // N0BMEN + N0CHCN=1 (8bpp)
    sat.bus.write16(0x05F8_00F8, 0x0001, AccessKind::Data); // PRINA.N0PRIN = 1
    // CRAM: index 0 = black, index 7 = pure red.
    sat.bus.vdp2.cram.write16(0, 0x0000);
    sat.bus.vdp2.cram.write16(7 * 2, 0x001F);
    // Bitmap: paint pixel (50, 60) with palette index 7. The hardware
    // bitmap is 512 px wide (N0BMSZ=0), independent of the 320-px screen.
    let off = 60u32 * 512 + 50;
    sat.bus.vdp2.vram.write8(off, 7);

    let mut out = vec![0u8; FRAMEBUFFER_BYTES];
    sat.run_frame(&mut out);

    let px = (60 * FRAME_WIDTH + 50) * 4;
    assert_eq!(&out[px..px + 4], &[0xFF, 0, 0, 0xFF], "red at (50,60)");
    // Pixel right next door is the backdrop (CRAM[0] = black).
    let px_next = (60 * FRAME_WIDTH + 51) * 4;
    assert_eq!(&out[px_next..px_next + 4], &[0, 0, 0, 0xFF]);
}
