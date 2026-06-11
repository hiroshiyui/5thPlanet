//! VDP2 rotation-parameter table + the affine screen→plane transform.
//!
//! A rotation background (RBG0/RBG1) maps each screen dot through a 2×3 affine
//! matrix sourced from a *rotation parameter table* in VRAM. This module reads
//! that table (parameter set A or B) and evaluates the transform; the renderer
//! then samples the rotation plane at the resulting coordinate.
//!
//! All coefficients are 16.16 fixed point (the table packs them with varying
//! field widths, sign-extended on read here). The transform mirrors MAME's
//! `saturn_v.cpp` rotation math, which follows the VDP2 manual's formula; the
//! line-coefficient table (per-line scaling) and the dual-parameter window
//! selection are later refinements — this is the single-parameter, constant-kx
//! case that covers basic rotation/scaling.

use super::vram::Vram;

/// 16.16 fixed-point multiply: `(a · b) >> 16` with a 64-bit intermediate.
#[inline]
fn mfx(a: i32, b: i32) -> i32 {
    (((a as i64) * (b as i64)) >> 16) as i32
}

/// Read `word & mask`, sign-extending with `fill` when `sign_bit` is set.
#[inline]
fn field(word: u32, mask: u32, sign_bit: u32, fill: u32) -> i32 {
    let v = word & mask;
    (if word & sign_bit != 0 { v | fill } else { v }) as i32
}

/// The rotation parameter set used for one background, all 16.16 fixed point.
#[derive(Clone, Copy, Debug, Default)]
pub struct RotationParams {
    pub xst: i32,
    pub yst: i32,
    pub zst: i32,
    pub dxst: i32,
    pub dyst: i32,
    pub dx: i32,
    pub dy: i32,
    pub a: i32,
    pub b: i32,
    pub c: i32,
    pub d: i32,
    pub e: i32,
    pub f: i32,
    pub px: i32,
    pub py: i32,
    pub pz: i32,
    pub cx: i32,
    pub cy: i32,
    pub cz: i32,
    pub mx: i32,
    pub my: i32,
    pub kx: i32,
    pub ky: i32,
    /// Coefficient-table start address (16.16; integer part = entry index),
    /// its per-line delta, and its per-dot delta (DKAx — drives the per-dot
    /// coefficient mode when a VRAM bank is RDBS-granted / CRKTE is set).
    pub kast: i32,
    pub dkast: i32,
    pub dkax: i32,
}

impl RotationParams {
    /// Read parameter set `which` (0 = A, 1 = B) from the table at byte
    /// `base` in VRAM. Field masks/sign bits match the VDP2 table format.
    pub fn read(vram: &Vram, base: u32, which: usize) -> Self {
        // Parameter A clears bit 7 of the address; B sets it.
        let addr = if which == 0 {
            base & !0x80
        } else {
            base | 0x80
        };
        let w = |k: u32| vram.read32(addr.wrapping_add(k * 4));

        // Screen start (1.13.10-ish, kept as 16.16).
        let st = |word: u32| field(word, 0x1FFF_FFC0, 0x1000_0000, 0xE000_0000);
        // Per-line / per-dot start deltas.
        let dd = |word: u32| field(word, 0x0007_FFC0, 0x0004_0000, 0xFFF8_0000);
        // Matrix coefficients.
        let mc = |word: u32| field(word, 0x000F_FFC0, 0x0008_0000, 0xFFF0_0000);

        let w13 = w(13);
        let w15 = w(15);
        Self {
            xst: st(w(0)),
            yst: st(w(1)),
            zst: st(w(2)),
            dxst: dd(w(3)),
            dyst: dd(w(4)),
            dx: dd(w(5)),
            dy: dd(w(6)),
            a: mc(w(7)),
            b: mc(w(8)),
            c: mc(w(9)),
            d: mc(w(10)),
            e: mc(w(11)),
            f: mc(w(12)),
            // s14 at bits 29..16 — the sign is bit 29 ONLY (Mednafen
            // `sign_x_to_s32(14, ...)`). A 0x3000_0000 sign mask here once
            // corrupted every Px in [4096, 8191] into a large negative,
            // displacing VF2's ring floor by a per-line kx-graded offset
            // (fighters looked "ring out" while inside).
            px: field(w13, 0x3FFF_0000, 0x2000_0000, 0xC000_0000),
            py: {
                let v = (w13 & 0x0000_3FFF) << 16;
                if v & 0x2000_0000 != 0 {
                    (v | 0xC000_0000) as i32
                } else {
                    v as i32
                }
            },
            pz: field(w(14), 0x3FFF_0000, 0x2000_0000, 0xC000_0000),
            cx: field(w15, 0x3FFF_0000, 0x2000_0000, 0xC000_0000),
            cy: {
                let v = (w15 & 0x0000_3FFF) << 16;
                if v & 0x2000_0000 != 0 {
                    (v | 0xC000_0000) as i32
                } else {
                    v as i32
                }
            },
            cz: field(w(16), 0x3FFF_0000, 0x2000_0000, 0xC000_0000),
            mx: field(w(17), 0x3FFF_FFC0, 0x2000_0000, 0xC000_0000),
            my: field(w(18), 0x3FFF_FFC0, 0x2000_0000, 0xC000_0000),
            kx: field(w(19), 0x00FF_FFFF, 0x0080_0000, 0xFF00_0000),
            ky: field(w(20), 0x00FF_FFFF, 0x0080_0000, 0xFF00_0000),
            // Coefficient table start (word 21, positive) + per-line delta
            // (word 22, signed 26-bit).
            kast: (w(21) & 0xFFFF_FFC0) as i32,
            dkast: field(w(22), 0x03FF_FFC0, 0x0200_0000, 0xFC00_0000),
            dkax: field(w(23), 0x03FF_FFC0, 0x0200_0000, 0xFC00_0000),
        }
    }

    /// Map screen dot `(sx, sy)` to a rotation-plane coordinate (integer
    /// pixels). Constant-`kx`/`ky` scaling (no coefficient table).
    pub fn transform(&self, sx: i32, sy: i32) -> (i32, i32) {
        self.transform_k(sx, sy, self.kx, self.ky, None)
    }

    /// [`transform`] with per-dot coefficient overrides: `kx`/`ky` replace the
    /// parameter-table scales, and `xp` (when set, 16.16) replaces the
    /// viewpoint X term — the coefficient table's mode-3 payload (VDP2 manual
    /// "Coefficient Table"; Mednafen `case 3: Xp = sext << 2`).
    pub fn transform_k(&self, sx: i32, sy: i32, kx: i32, ky: i32, xp_ovr: Option<i32>) -> (i32, i32) {
        let dx = mfx(self.a, self.dx) + mfx(self.b, self.dy);
        let dy = mfx(self.d, self.dx) + mfx(self.e, self.dy);

        let xp = xp_ovr.unwrap_or_else(|| {
            mfx(self.a, self.px - self.cx)
                + mfx(self.b, self.py - self.cy)
                + mfx(self.c, self.pz - self.cz)
                + self.cx
                + self.mx
        });
        let yp = mfx(self.d, self.px - self.cx)
            + mfx(self.e, self.py - self.cy)
            + mfx(self.f, self.pz - self.cz)
            + self.cy
            + self.my;

        let vy = sy << 16;
        let xsp = mfx(self.a, self.xst + mfx(self.dxst, vy) - self.px)
            + mfx(self.b, self.yst + mfx(self.dyst, vy) - self.py)
            + mfx(self.c, self.zst - self.pz);
        let ysp = mfx(self.d, self.xst + mfx(self.dxst, vy) - self.px)
            + mfx(self.e, self.yst + mfx(self.dyst, vy) - self.py)
            + mfx(self.f, self.zst - self.pz);

        // Per-dot step is `mfx(k, d)`, accumulated `sx` times across the line.
        let xs = mfx(kx, xsp) + xp + sx.wrapping_mul(mfx(kx, dx));
        let ys = mfx(ky, ysp) + yp + sx.wrapping_mul(mfx(ky, dy));
        (xs >> 16, ys >> 16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: u32 = 1 << 16; // 1.0 in 16.16
    const NEG_ONE: u32 = 0xFFFF_0000; // -1.0 in 16.16

    fn table(words: &[(u32, u32)]) -> Vram {
        let mut v = Vram::new();
        for &(k, val) in words {
            v.write32(k * 4, val);
        }
        v
    }

    /// Identity: the per-dot horizontal step (dx) and per-line vertical step
    /// (dyst) are 1.0 and the matrix passes them through (A=E=1), so a screen
    /// dot maps to the same plane coordinate.
    #[test]
    fn identity_maps_one_to_one() {
        let rp = RotationParams::read(
            &table(&[
                (4, ONE),  // dyst = 1.0 (plane y advances 1 per scanline)
                (5, ONE),  // dx   = 1.0 (plane x advances 1 per dot)
                (7, ONE),  // A = 1.0
                (11, ONE), // E = 1.0
                (19, ONE), // kx = 1.0
                (20, ONE), // ky = 1.0
            ]),
            0,
            0,
        );
        assert_eq!(rp.transform(0, 0), (0, 0));
        assert_eq!(rp.transform(100, 50), (100, 50));
    }

    /// kx = ky = 0.5 halves the sampled source coordinate (2× zoom-in).
    #[test]
    fn half_scale_compresses_source_coordinates() {
        let half = 1 << 15; // 0.5
        let rp = RotationParams::read(
            &table(&[
                (4, ONE),
                (5, ONE),
                (7, ONE),
                (11, ONE),
                (19, half),
                (20, half),
            ]),
            0,
            0,
        );
        assert_eq!(rp.transform(0, 0), (0, 0));
        assert_eq!(rp.transform(100, 80), (50, 40));
    }

    /// 90° rotation: A=0, B=-1, D=1, E=0 maps screen (x, y) → plane (-y, x).
    #[test]
    fn ninety_degree_rotation() {
        let rp = RotationParams::read(
            &table(&[
                (4, ONE),     // dyst = 1.0
                (5, ONE),     // dx   = 1.0
                (8, NEG_ONE), // B = -1.0
                (10, ONE),    // D = 1.0
                (19, ONE),    // kx
                (20, ONE),    // ky
            ]),
            0,
            0,
        );
        assert_eq!(rp.transform(10, 4), (-4, 10));
    }

    /// Px is a signed 14-bit integer at bits 29..16 whose sign is bit 29
    /// ONLY (Mednafen `sign_x_to_s32(14, ...)`). The regression: values in
    /// [4096, 8191] — bit 28 set, bit 29 clear — are POSITIVE; a sign mask
    /// that also tested bit 28 corrupted them to large negatives, displacing
    /// the rotation plane by a per-line kx-graded offset (VF2's ring floor —
    /// fighters appeared "ring out" while inside).
    #[test]
    fn px_in_the_4096_8191_range_stays_positive() {
        // Word 13 packs Px (high 16 bits) and Py (low 14): Px = 4096.
        let rp = RotationParams::read(&table(&[(13, 4096 << 16)]), 0, 0);
        assert_eq!(rp.px, 4096 << 16, "Px=+4096 must read positive 16.16");
        // The genuine sign bit still works: Px = -4096 (raw 14-bit 0x3000).
        let rp = RotationParams::read(&table(&[(13, 0x3000 << 16)]), 0, 0);
        assert_eq!(rp.px, -4096 << 16);
        // And +8191 / the most-negative -8192.
        let rp = RotationParams::read(&table(&[(13, 8191 << 16)]), 0, 0);
        assert_eq!(rp.px, 8191 << 16);
        let rp = RotationParams::read(&table(&[(13, 0x2000 << 16)]), 0, 0);
        assert_eq!(rp.px, -8192 << 16);
    }

    #[test]
    fn parameter_b_reads_from_the_high_half_of_the_table() {
        let mut v = Vram::new();
        for &(k, val) in &[
            (4u32, ONE),
            (5, ONE),
            (7, ONE),
            (11, ONE),
            (19, ONE),
            (20, ONE),
        ] {
            v.write32(0x80 + k * 4, val);
        }
        let rp = RotationParams::read(&v, 0, 1);
        assert_eq!(rp.transform(7, 9), (7, 9));
    }
}
