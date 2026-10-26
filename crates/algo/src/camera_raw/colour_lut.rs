//! A run of Camera Raw's per-pixel colour stages sampled once on a grid and applied to the image
//! by tetrahedral interpolation. While a slider is dragged the preview moves through these
//! instead of running every stage on every pixel (see `develop_cached`): building the grid runs
//! the stages on `LUT_SIZE³` colours, applying it costs one lookup per pixel whatever the stages
//! do. Greys stay on the grid's diagonal, so they come out exactly as the stages make them; colours
//! in between are on average within a tenth of a level (more where a stage clips a colour to the
//! gamut, which the grid rounds off; the exact frame follows the drag). Pixels outside 0…1 (float
//! documents, or pushed there by an earlier stage) and non-finite ones go through the stages
//! themselves.

use crate::photo_util::par_rows;

/// Grid points per channel.
pub(super) const LUT_SIZE: usize = 33;
/// How far outside 0…1 a value is still read from the grid's edge (a tenth of an 8-bit level):
/// rounding in the stages before leaves values a hair outside.
const EDGE: f32 = 1.0 / 2550.0;
/// The row width the pixels outside the grid go through the stages in.
const ODD_ROW: usize = 1024;
/// Rows of the image per band (see [`ColourLut::apply`]).
const BAND_ROWS: usize = 16;
/// Below this many pixels the stages themselves are as fast as building the grid.
pub(super) const LUT_MIN_PIXELS: usize = 4 * LUT_SIZE * LUT_SIZE * LUT_SIZE;

/// Whether a table pays on `px`: at least [`LUT_MIN_PIXELS`] of them, and few outside 0…1 (those
/// go through the stages anyway; a fifth of the image or more and the table only adds work).
/// Judged on every 97th pixel.
pub(super) fn worth(px: &[[f32; 4]]) -> bool {
    if px.len() < LUT_MIN_PIXELS {
        return false;
    }
    let (mut seen, mut odd) = (0usize, 0usize);
    for q in px.iter().step_by(97) {
        seen += 1;
        odd += usize::from(!inside(q));
    }
    odd * 5 < seen
}

/// Whether the grid holds `q` (see [`EDGE`]).
fn inside(q: &[f32; 4]) -> bool {
    q[..3].iter().all(|v| (-EDGE..=1.0 + EDGE).contains(v))
}

pub(super) struct ColourLut {
    n: usize,
    v: Vec<[f32; 3]>,
}

impl ColourLut {
    /// Samples `stages` (which change straight RGBA pixels in place, given a row width) on an
    /// `n³` grid of encoded values from 0 to 1.
    pub(super) fn build(n: usize, stages: &(impl Fn(&mut [[f32; 4]], usize) + Sync)) -> Self {
        let n = n.clamp(2, 129);
        let step = 1.0 / (n - 1) as f32;
        let mut px: Vec<[f32; 4]> = (0..n * n * n).map(|i| [(i % n) as f32 * step, ((i / n) % n) as f32 * step, (i / (n * n)) as f32 * step, 1.0]).collect();
        stages(&mut px, n);
        ColourLut { n, v: px.into_iter().map(|q| [q[0], q[1], q[2]]).collect() }
    }

    fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.v.get(r + g * self.n + b * self.n * self.n).copied().unwrap_or([0.0; 3])
    }

    /// The table at an encoded colour in 0…1 (tetrahedral interpolation).
    fn lookup(&self, c: [f32; 3]) -> [f32; 3] {
        let top = (self.n - 1) as f32;
        let s = c.map(|v| v * top);
        let i = s.map(|v| (v as usize).min(self.n - 2));
        let f: [f32; 3] = std::array::from_fn(|k| s[k] - i[k] as f32);
        let corner = |dr: usize, dg: usize, db: usize| self.at(i[0] + dr, i[1] + dg, i[2] + db);
        let c000 = corner(0, 0, 0);
        let c111 = corner(1, 1, 1);
        // The tetrahedron of the unit cube holding the point, by the order of its fractions.
        let (w1, a, w2, b, w3) = if f[0] >= f[1] {
            if f[1] >= f[2] {
                (f[0], corner(1, 0, 0), f[1], corner(1, 1, 0), f[2])
            } else if f[0] >= f[2] {
                (f[0], corner(1, 0, 0), f[2], corner(1, 0, 1), f[1])
            } else {
                (f[2], corner(0, 0, 1), f[0], corner(1, 0, 1), f[1])
            }
        } else if f[2] >= f[1] {
            (f[2], corner(0, 0, 1), f[1], corner(0, 1, 1), f[0])
        } else if f[2] >= f[0] {
            (f[1], corner(0, 1, 0), f[2], corner(0, 1, 1), f[0])
        } else {
            (f[1], corner(0, 1, 0), f[0], corner(1, 1, 0), f[2])
        };
        std::array::from_fn(|k| c000[k] + w1 * (a[k] - c000[k]) + w2 * (b[k] - a[k]) + w3 * (c111[k] - b[k]))
    }

    /// Applies the table to `px` (rows `w` wide); pixels it can't hold go through `stages`, a
    /// band of rows at a time (each band's on its own thread, the stages set up once per band).
    pub(super) fn apply(&self, px: &mut [[f32; 4]], w: usize, stages: &(impl Fn(&mut [[f32; 4]], usize) + Sync)) {
        let band = w.max(1).saturating_mul(BAND_ROWS);
        par_rows(px, band, 1, |_, rows| {
            let mut odd = Vec::new();
            for (i, q) in rows.iter_mut().enumerate() {
                if inside(q) {
                    let c = self.lookup([q[0], q[1], q[2]].map(|v| v.clamp(0.0, 1.0)));
                    q[..3].copy_from_slice(&c);
                } else {
                    odd.push(i);
                }
            }
            if odd.is_empty() {
                return;
            }
            let mut rest: Vec<[f32; 4]> = odd.iter().filter_map(|&i| rows.get(i).copied()).collect();
            let len = rest.len();
            stages(&mut rest, len.min(ODD_ROW));
            for (&i, q) in odd.iter().zip(rest) {
                if let Some(o) = rows.get_mut(i) {
                    *o = q;
                }
            }
        });
    }

    /// [`ColourLut::apply`] reading `src` and writing `dst` (the same length), which saves copying
    /// the source first.
    pub(super) fn apply_from(&self, src: &[[f32; 4]], dst: &mut [[f32; 4]], w: usize, stages: &(impl Fn(&mut [[f32; 4]], usize) + Sync)) {
        if src.len() != dst.len() {
            dst.iter_mut().zip(src).for_each(|(d, s)| *d = *s);
            self.apply(dst, w, stages);
            return;
        }
        let band = w.max(1).saturating_mul(BAND_ROWS);
        par_rows(dst, band, 1, |y, rows| {
            let start = y.saturating_mul(band);
            let Some(from) = src.get(start..start.saturating_add(rows.len())) else { return };
            let mut odd = Vec::new();
            for (i, (q, s)) in rows.iter_mut().zip(from).enumerate() {
                *q = *s;
                if inside(s) {
                    let c = self.lookup([s[0], s[1], s[2]].map(|v| v.clamp(0.0, 1.0)));
                    q[..3].copy_from_slice(&c);
                } else {
                    odd.push(i);
                }
            }
            if odd.is_empty() {
                return;
            }
            let mut rest: Vec<[f32; 4]> = odd.iter().filter_map(|&i| rows.get(i).copied()).collect();
            let len = rest.len();
            stages(&mut rest, len.min(ODD_ROW));
            for (&i, q) in odd.iter().zip(rest) {
                if let Some(o) = rows.get_mut(i) {
                    *o = q;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linear_map_is_reproduced_and_odd_pixels_take_the_stages() {
        let stages = |px: &mut [[f32; 4]], _w: usize| {
            for q in px.iter_mut() {
                let (r, g, b) = (q[0], q[1], q[2]);
                *q = [0.8 * r + 0.1 * g + 0.1 * b, 0.2 * r + 0.7 * g + 0.1 * b, 0.05 * r + 0.15 * g + 0.8 * b, q[3]];
            }
        };
        let lut = ColourLut::build(9, &stages);
        let mut px = vec![[0.3, 0.6, 0.9, 1.0], [0.0, 1.0, 0.5, 0.5], [1.5, -0.2, 0.4, 1.0], [f32::NAN, 0.1, 0.1, 1.0]];
        let mut want = px.clone();
        stages(&mut want, 4);
        let mut from = vec![[0.0f32; 4]; px.len()];
        lut.apply_from(&px, &mut from, 2, &stages);
        lut.apply(&mut px, 2, &stages);
        assert!(px.iter().zip(&from).all(|(a, b)| a.iter().zip(b).all(|(x, y)| x == y || (x.is_nan() && y.is_nan()))));
        for (a, b) in px.iter().zip(&want) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 1e-5 || (a[c].is_nan() && b[c].is_nan()), "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn greys_stay_on_the_diagonal() {
        // A curve on each channel: a grey in between grid points interpolates along the diagonal only.
        let stages = |px: &mut [[f32; 4]], _w: usize| {
            for q in px.iter_mut() {
                for c in 0..3 {
                    q[c] = q[c] * q[c];
                }
            }
        };
        let lut = ColourLut::build(5, &stages);
        let mut px = vec![[0.33, 0.33, 0.33, 1.0]];
        lut.apply(&mut px, 1, &stages);
        assert!(px[0][0] == px[0][1] && px[0][1] == px[0][2], "{:?}", px[0]);
    }
}
