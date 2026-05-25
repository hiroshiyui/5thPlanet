//! VDP2 frame renderer — produces an RGBA8888 framebuffer from the
//! current VRAM / CRAM / register state.
//!
//! M3 scope (deliberate floor, will grow as games surface needs):
//!
//! - **One NBG layer** (NBG0). Other NBGs / RBGs / sprite layer
//!   render as fully transparent — composited on top of the
//!   backdrop they leave the backdrop visible.
//! - **Backdrop** = CRAM index 0 (real VDP2 has a BKTAU/BKTAL
//!   register that points at the backdrop colour separately; M3
//!   approximates it with palette entry 0 which most software
//!   programs to be the actual splash background).
//! - **Bitmap mode**: 8 bpp paletted (the splash format the BIOS
//!   uses). Bitmap base address is `MPOFN.N0MP × 0x20000`; horizontal
//!   stride is still the screen width (full `N0BMSZ` width decode is a
//!   later refinement). Bitmap-enable is read from `CHCTLA.N0BMEN`.
//! - **Tile mode**: 4 bpp paletted, 8×8 cells, 1-word PNC entries,
//!   64×64-cell plane (= 512×512 pixels). The pattern-name-table base
//!   comes from the map registers (`MPOFN`/`MPABN0`); character data
//!   stays at a fixed VRAM base (no character-base register exists —
//!   real hardware derives it from the character number, a later
//!   refinement). Wrap / flip flags / multi-plane composition come
//!   later.
//! - **Scrolling**: integer NBG0 scroll from `SCXIN0`/`SCYIN0` is
//!   applied; fractional scroll and zoom are ignored.
//! - **NTSC low-res** (320×224). PAL / hi-res come with the
//!   resolution decode in TVMD.
//!
//! The renderer is `&Vdp2 -> &mut [u8]` — pure, no side effects on
//! the VDP2 state, no allocation. The SDL2 frontend in task #7
//! calls it once per frame and uploads the buffer to a texture.

use super::Vdp2;

pub const FRAME_WIDTH: usize = 320;
pub const FRAME_HEIGHT: usize = 224;
pub const FRAMEBUFFER_BYTES: usize = FRAME_WIDTH * FRAME_HEIGHT * 4;

const BACKDROP_PALETTE_INDEX: usize = 0;

/// Bitmap stride in pixels. Real VDP2 picks from `N0BMSZ`; the minimal
/// renderer uses the screen width directly (see module docs).
const BITMAP_PITCH: u32 = FRAME_WIDTH as u32;

/// Fixed VRAM base for tile character data. There is no
/// character-base register on VDP2 — hardware addresses character
/// patterns by `character_number × cell_size`. The minimal renderer
/// keeps a fixed base so the pattern-name-table (whose base *is*
/// register-driven) and the character data don't overlap. Deriving
/// the per-character address is a later refinement.
const TILE_CHARACTER_DATA_BASE: u32 = 0x4000;
const TILE_PLANE_WIDTH_CELLS: u32 = 64;
/// Tile plane is 64×64 cells of 8×8 px = 512×512 px; scroll wraps here.
const TILE_PLANE_WIDTH_PX: u32 = TILE_PLANE_WIDTH_CELLS * 8;

/// Render one frame of NTSC low-res into `out`. Panics if `out`'s
/// length isn't exactly [`FRAMEBUFFER_BYTES`].
pub fn render_frame(vdp2: &Vdp2, out: &mut [u8]) {
    assert_eq!(out.len(), FRAMEBUFFER_BYTES, "framebuffer must be 320×224×4");

    if !vdp2.regs.display_enabled() {
        out.fill(0);
        // Keep alpha at 0xFF so downstream SDL doesn't render the
        // disabled frame as a transparent black hole.
        for px in out.chunks_exact_mut(4) {
            px[3] = 0xFF;
        }
        return;
    }

    fill_backdrop(vdp2, out);

    if !vdp2.regs.nbg0_enabled() {
        return;
    }

    if vdp2.regs.nbg0_bitmap_enabled() {
        render_nbg0_bitmap_8bpp(vdp2, out);
    } else {
        render_nbg0_tile_4bpp(vdp2, out);
    }
}

fn fill_backdrop(vdp2: &Vdp2, out: &mut [u8]) {
    let (r, g, b) = vdp2.cram.color_rgb888_mode0(BACKDROP_PALETTE_INDEX);
    for px in out.chunks_exact_mut(4) {
        px[0] = r;
        px[1] = g;
        px[2] = b;
        px[3] = 0xFF;
    }
}

#[inline]
fn put_pixel(out: &mut [u8], x: usize, y: usize, r: u8, g: u8, b: u8) {
    let dst = (y * FRAME_WIDTH + x) * 4;
    out[dst] = r;
    out[dst + 1] = g;
    out[dst + 2] = b;
    out[dst + 3] = 0xFF;
}

fn render_nbg0_bitmap_8bpp(vdp2: &Vdp2, out: &mut [u8]) {
    let base = vdp2.regs.nbg0_bitmap_base();
    let scroll_x = vdp2.regs.nbg0_scroll_x();
    let scroll_y = vdp2.regs.nbg0_scroll_y();
    for y in 0..FRAME_HEIGHT {
        let sy = y as u32 + scroll_y;
        for x in 0..FRAME_WIDTH {
            let sx = x as u32 + scroll_x;
            let src = base + sy * BITMAP_PITCH + sx;
            let idx = vdp2.vram.read8(src) as usize;
            let (r, g, b) = vdp2.cram.color_rgb888_mode0(idx);
            put_pixel(out, x, y, r, g, b);
        }
    }
}

fn render_nbg0_tile_4bpp(vdp2: &Vdp2, out: &mut [u8]) {
    let pn_base = vdp2.regs.nbg0_pattern_table_base();
    let scroll_x = vdp2.regs.nbg0_scroll_x();
    let scroll_y = vdp2.regs.nbg0_scroll_y();
    for y in 0..FRAME_HEIGHT {
        // Source coordinate after integer scroll, wrapped into the plane.
        let src_y = (y as u32 + scroll_y) % TILE_PLANE_WIDTH_PX;
        let tile_y = src_y / 8;
        let in_y = src_y % 8;
        for x in 0..FRAME_WIDTH {
            let src_x = (x as u32 + scroll_x) % TILE_PLANE_WIDTH_PX;
            let tile_x = src_x / 8;
            let in_x = src_x % 8;

            // Pattern name table entry: 16 bits per cell.
            //   bits 9..0  character number (10 bits)
            //   bits 15..12 palette bank (4 bits, selects high nibble of
            //               the 8-bit CRAM index)
            // Flip flags / multi-bank PNC formats deferred.
            let pn_off = pn_base + (tile_y * TILE_PLANE_WIDTH_CELLS + tile_x) * 2;
            let pn = vdp2.vram.read16(pn_off);
            let char_num = (pn & 0x03FF) as u32;
            let palette_bank = ((pn >> 12) & 0xF) as usize;

            // 4 bpp, 8×8 cell = 32 bytes per cell; 4 bytes per row;
            // two pixels packed per byte (high nibble = even column).
            let char_off = TILE_CHARACTER_DATA_BASE + char_num * 32;
            let byte = vdp2.vram.read8(char_off + in_y * 4 + in_x / 2);
            let nibble = if in_x % 2 == 0 { byte >> 4 } else { byte & 0xF } as usize;
            let palette_idx = (palette_bank << 4) | nibble;
            let (r, g, b) = vdp2.cram.color_rgb888_mode0(palette_idx);
            put_pixel(out, x, y, r, g, b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_buf() -> Vec<u8> {
        vec![0xCD; FRAMEBUFFER_BYTES] // sentinel so we can see what got written
    }

    #[test]
    fn display_disabled_emits_opaque_black() {
        let v = Vdp2::new();
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        for chunk in buf.chunks_exact(4) {
            assert_eq!(chunk, &[0, 0, 0, 0xFF]);
        }
    }

    #[test]
    fn display_enabled_no_layer_fills_with_backdrop() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // TVMD.DISP
        // Backdrop = CRAM index 0 = pure red.
        v.cram.write16(0, 0x001F);
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        for chunk in buf.chunks_exact(4) {
            assert_eq!(chunk, &[0xFF, 0, 0, 0xFF]);
        }
    }

    #[test]
    fn bitmap_mode_renders_palette_indices_from_vram() {
        let mut v = Vdp2::new();
        // DISP on, NBG0 on, NBG0 bitmap-enable.
        v.regs.write16(0x000, 0x8000);
        v.regs.write16(0x020, 0x0001);
        v.regs.write16(0x028, 0x0002); // CHCTLA.N0BMEN (bit 1)
        // Palette: index 0 black, index 1 green, index 2 blue.
        v.cram.write16(0, 0x0000);
        v.cram.write16(2, 0x1F << 5); // green: R=0 G=31 B=0
        v.cram.write16(4, 0x7C00); // blue: B=31
        // Put index 1 at (10, 5) and index 2 at (200, 100).
        let off1 = 5u32 * BITMAP_PITCH + 10;
        let off2 = 100u32 * BITMAP_PITCH + 200;
        v.vram.write8(off1, 1);
        v.vram.write8(off2, 2);

        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        let px1 = (5 * FRAME_WIDTH + 10) * 4;
        let px2 = (100 * FRAME_WIDTH + 200) * 4;
        assert_eq!(&buf[px1..px1 + 4], &[0, 0xFF, 0, 0xFF], "green at (10,5)");
        assert_eq!(&buf[px2..px2 + 4], &[0, 0, 0xFF, 0xFF], "blue at (200,100)");
    }

    #[test]
    fn bitmap_base_follows_map_offset_register() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0001); // NBG0 on
        v.regs.write16(0x028, 0x0002); // N0BMEN
        v.regs.write16(0x03E, 0x0001); // MPOFN.N0MP = 1 → base 0x20000
        // Palette index 1 = pure red.
        v.cram.write16(2, 0x001F);
        // Pixel (0,0) reads from base 0x20000, not VRAM 0.
        v.vram.write8(0x2_0000, 1);
        v.vram.write8(0, 1); // would also be red if base were wrong=0…
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(&buf[0..4], &[0xFF, 0, 0, 0xFF], "pixel 0 reads from 0x20000");
    }

    #[test]
    fn bitmap_integer_scroll_shifts_source() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0001); // NBG0 on
        v.regs.write16(0x028, 0x0002); // N0BMEN
        v.regs.write16(0x070, 0x0002); // SCXIN0 = 2
        v.regs.write16(0x074, 0x0003); // SCYIN0 = 3
        v.cram.write16(2, 0x001F); // index 1 = red
        // With scroll (2,3), screen (0,0) samples VRAM (3*pitch + 2).
        v.vram.write8(3 * BITMAP_PITCH + 2, 1);
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(&buf[0..4], &[0xFF, 0, 0, 0xFF], "scroll offsets source");
    }

    #[test]
    fn tile_pattern_table_base_follows_map_registers() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0001); // NBG0 on, tile mode
        // N0MP=0, N0MPA=1 → plane number 1 → PN base 0x2000.
        v.regs.write16(0x040, 0x0001); // MPABN0.N0MPA = 1
        let pn_base = 0x2000u32;
        // PN at tile (0,0): char 3, palette bank 1.
        let pn: u16 = (1 << 12) | 3;
        v.vram.write16(pn_base, pn);
        // Character 3 pixel (0,0) = nibble 5.
        let char_off = TILE_CHARACTER_DATA_BASE + 3 * 32;
        v.vram.write8(char_off, 0x50); // high nibble (even col) = 5
        // Palette index = (1<<4)|5 = 0x15 → red.
        v.cram.write16(0x15 * 2, 0x001F);
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        assert_eq!(&buf[0..4], &[0xFF, 0, 0, 0xFF], "PN read from register base");
    }

    #[test]
    fn tile_mode_resolves_pattern_to_character_to_palette() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000);
        v.regs.write16(0x020, 0x0001); // NBG0 on
        v.regs.write16(0x028, 0x0000); // CHCTLA all zero → tile mode

        // PN at tile (0, 0): char_num=5, palette_bank=2. Default map
        // registers → pattern-name-table base 0.
        let pn: u16 = (2 << 12) | 5;
        v.vram.write16(0, pn);
        // Character 5 — 8 rows × 4 bytes; fill pixel (3, 4) with nibble 7.
        // Pixel (3, 4) is in row 4, column 3 → byte offset 1 (column 3/2),
        // low nibble (column 3 is odd).
        let char_off = TILE_CHARACTER_DATA_BASE + 5 * 32;
        let row_off = char_off + 4 * 4 + 1;
        v.vram.write8(row_off, 0x07);
        // Palette index = (2 << 4) | 7 = 0x27 = 39 → set CRAM[39] to pure red.
        v.cram.write16(39 * 2, 0x001F);

        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        let px = (4 * FRAME_WIDTH + 3) * 4;
        assert_eq!(&buf[px..px + 4], &[0xFF, 0, 0, 0xFF]);
    }

    #[test]
    fn nbg0_disabled_leaves_backdrop_intact() {
        let mut v = Vdp2::new();
        v.regs.write16(0x000, 0x8000); // DISP
        v.regs.write16(0x020, 0x0000); // NBG0 off
        v.cram.write16(0, 0x1F << 5); // backdrop green
        // Even with bitmap data in VRAM, layer-off must keep backdrop.
        v.vram.write8(0, 1);
        v.cram.write16(2, 0x7C00); // would render blue if enabled
        let mut buf = fresh_buf();
        render_frame(&v, &mut buf);
        let px = 0;
        assert_eq!(&buf[px..px + 4], &[0, 0xFF, 0, 0xFF]);
    }

    #[test]
    #[should_panic(expected = "framebuffer must be 320×224×4")]
    fn wrong_buffer_size_panics_loudly() {
        let v = Vdp2::new();
        let mut tiny = [0u8; 64];
        render_frame(&v, &mut tiny);
    }
}
