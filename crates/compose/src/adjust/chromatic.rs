//! Chromatic Aberration ([`ChromaticAberration`]): the red and blue channels of the composite
//! beneath resampled from shifted positions, green and alpha kept.
//!
//! It is the one adjustment that reads neighbouring pixels. The compositor therefore renders every
//! tile (and every other rectangle it is asked for) over the rectangle grown by [`stack_reach`],
//! applies the adjustment there and keeps the middle (`render::with_apron`), so tiles agree at
//! their seams. Samples are taken inside the canvas, clamped to its edge, and within the buffer
//! (a buffer without the apron degrades at its edge rather than reading out of bounds). Colours
//! are mixed premultiplied, so a transparent neighbour lends no colour.

use photocraft_doc::adjust::ChromaticAberration;
use photocraft_doc::{Adjustment, Layer, LayerContent};
use photocraft_geom::Rect;

use crate::Buffer;

/// The largest apron any document asks for, in pixels: the sum of a few dozen layers at their
/// largest reach, and a bound on the extra work one tile can cost.
pub const MAX_STACK_REACH: i32 = 1024;

/// How far the composite at a pixel can read from it through the visible Chromatic Aberration
/// layers of `layers` (nested in groups too), in pixels: their reaches added up, since one above
/// another reads what the lower one read. 0 without any.
pub fn stack_reach(layers: &[Layer]) -> i32 {
    crate::visible_layers(layers)
        .iter()
        .filter_map(|l| match &l.content {
            LayerContent::Adjustment(Adjustment::ChromaticAberration(c)) => Some(c.reach()),
            _ => None,
        })
        .fold(0i32, i32::saturating_add)
        .min(MAX_STACK_REACH)
}

/// Whether `layers` hold a visible Chromatic Aberration that shifts anything.
pub fn has_chromatic(layers: &[Layer]) -> bool {
    stack_reach(layers) > 0
}

/// Applies `ca` to `buf`, whose pixels outside `canvas` are left alone (and never sampled).
pub fn apply_chromatic(ca: &ChromaticAberration, buf: &mut Buffer, canvas: Rect) {
    let ca = ca.sanitized();
    let rect = buf.rect;
    let area = rect.intersect(&canvas);
    if ca.is_identity() || area.is_empty() || buf.px.len() != rect.width() as usize * rect.height() as usize {
        return;
    }
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let src = Planes::new(&buf.px);
    let radius = ca.blur_radius();
    // The blur's taps are clamped to the canvas too (buffer indices), so pixels past it never count.
    let (ilo, ihi) = ([(area.x0 - rect.x0) as usize, (area.y0 - rect.y0) as usize], [(area.x1 - 1 - rect.x0) as usize, (area.y1 - 1 - rect.y0) as usize]);
    let soft = (radius > 0).then(|| src.blurred(w, h, blur_sigma(&ca), radius, ilo, ihi));
    let Geometry { cx, cy, corner } = geometry(&ca, canvas);
    // Samples stay inside the canvas and the buffer. Positions are in document coordinates, not
    // the buffer's, so every tile rounds them alike.
    let lo = [area.x0 as f32, area.y0 as f32];
    let hi = [(area.x1 - 1) as f32, (area.y1 - 1) as f32];
    let sampler = Sampler { w, origin: [rect.x0, rect.y0], lo, hi };
    let fade = |rho: f32| {
        let f = ca.falloff;
        if f <= 0.0 { 1.0 } else { smoothstep(f * 0.5, f, rho) }
    };

    for y in area.y0..area.y1 {
        let row = (y - rect.y0) as usize * w;
        let fy = y as f32 - cy;
        for x in area.x0..area.x1 {
            let i = row + (x - rect.x0) as usize;
            let Some(p) = buf.px.get(i).copied() else { continue };
            if p[3] <= 0.0 {
                continue;
            }
            let fx = x as f32 - cx;
            // `sqrt` of the sum, not `hypot`: the GPU kernel's arithmetic (compose.wgsl `ca_px`).
            let dist = (fx * fx + fy * fy).sqrt();
            let rho = dist / corner;
            let m = fade(rho);
            if m <= 0.0 {
                continue;
            }
            let (ux, uy) = if dist > 0.0 { (fx / dist, fy / dist) } else { (0.0, 0.0) };
            let shift = |radial: f32, sign: f32| {
                let r = radial * rho * m;
                (ux * r + sign * ca.offset_x * m, uy * r + sign * ca.offset_y * m)
            };
            let mut out = p;
            for (c, (dx, dy)) in [(0usize, shift(ca.red_shift, 1.0)), (2, shift(ca.blue_shift, -1.0))] {
                // The channel moved by (dx, dy): its value here comes from where it moved from.
                let (sx, sy) = (x as f32 - dx, y as f32 - dy);
                let sharp = sampler.sample(&src, c, sx, sy);
                let v = match &soft {
                    Some(blurred) => {
                        let k = (dx * dx + dy * dy).sqrt().min(1.0);
                        let smooth = sampler.sample(blurred, c, sx, sy);
                        match (sharp, smooth) {
                            (Some(a), Some(b)) => Some(a + (b - a) * k),
                            (a, b) => a.or(b),
                        }
                    }
                    None => sharp,
                };
                if let Some(v) = v {
                    out[c] = v;
                }
            }
            if let Some(q) = buf.px.get_mut(i) {
                *q = out;
            }
        }
    }
}

/// The radial shift's centre in pixel-index coordinates (pixel x's centre is at x) and the
/// distance from it to the farthest canvas corner (where the shift is at full strength).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub cx: f32,
    pub cy: f32,
    pub corner: f32,
}

/// [`Geometry`] of `ca` (sanitized) on `canvas`; shared with the GPU kernel's parameters.
pub fn geometry(ca: &ChromaticAberration, canvas: Rect) -> Geometry {
    let cx = canvas.x0 as f32 + ca.center[0] * canvas.width() as f32 - 0.5;
    let cy = canvas.y0 as f32 + ca.center[1] * canvas.height() as f32 - 0.5;
    let corner = [(canvas.x0, canvas.y0), (canvas.x1, canvas.y0), (canvas.x0, canvas.y1), (canvas.x1, canvas.y1)]
        .iter()
        .map(|&(x, y)| {
            let (dx, dy) = (x as f32 - 0.5 - cx, y as f32 - 0.5 - cy);
            (dx * dx + dy * dy).sqrt()
        })
        .fold(1.0f32, f32::max);
    Geometry { cx, cy, corner }
}

/// The softness blur's Gaussian sigma (`softness / 2`, at least 0.1).
pub fn blur_sigma(ca: &ChromaticAberration) -> f32 {
    (ca.softness / 2.0).max(0.1)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x >= e1 { 1.0 } else { 0.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Premultiplied red and blue, and alpha, of a buffer: the planes the shifted channels are read from.
struct Planes {
    red: Vec<f32>,
    blue: Vec<f32>,
    alpha: Vec<f32>,
}

impl Planes {
    fn new(px: &[[f32; 4]]) -> Self {
        Self { red: px.iter().map(|p| p[0] * p[3]).collect(), blue: px.iter().map(|p| p[2] * p[3]).collect(), alpha: px.iter().map(|p| p[3]).collect() }
    }

    fn plane(&self, c: usize) -> &[f32] {
        if c == 0 { &self.red } else { &self.blue }
    }

    /// The planes blurred by a Gaussian of `sigma`, cut at `radius`, taps clamped to `lo..=hi`
    /// (buffer indices): across, then down, as the GPU's two passes.
    fn blurred(&self, w: usize, h: usize, sigma: f32, radius: i32, lo: [usize; 2], hi: [usize; 2]) -> Self {
        let kernel: Vec<f32> = (-radius..=radius).map(|d| (-(d * d) as f32 / (2.0 * sigma * sigma)).exp()).collect();
        let total: f32 = kernel.iter().sum();
        let kernel: Vec<f32> = kernel.iter().map(|k| k / total.max(f32::MIN_POSITIVE)).collect();
        let blur = |plane: &[f32]| blur2d(plane, w, h, &kernel, radius, lo, hi);
        Self { red: blur(&self.red), blue: blur(&self.blue), alpha: blur(&self.alpha) }
    }
}

/// A separable blur of a `w` × `h` plane by `kernel` (`2 · radius + 1` taps), taps clamped to
/// `lo..=hi`.
fn blur2d(plane: &[f32], w: usize, h: usize, kernel: &[f32], radius: i32, lo: [usize; 2], hi: [usize; 2]) -> Vec<f32> {
    if w == 0 || h == 0 || plane.len() != w * h {
        return plane.to_vec();
    }
    let at = |v: i64, axis: usize| v.clamp(lo[axis] as i64, hi[axis] as i64) as usize;
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, d) in kernel.iter().zip(-radius..=radius) {
                acc += k * plane.get(y * w + at(x as i64 + i64::from(d), 0)).copied().unwrap_or(0.0);
            }
            if let Some(t) = tmp.get_mut(y * w + x) {
                *t = acc;
            }
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, d) in kernel.iter().zip(-radius..=radius) {
                acc += k * tmp.get(at(y as i64 + i64::from(d), 1) * w + x).copied().unwrap_or(0.0);
            }
            if let Some(o) = out.get_mut(y * w + x) {
                *o = acc;
            }
        }
    }
    out
}

/// Bilinear, premultiplied sampling within `lo..=hi` (document coordinates) of a buffer `w`
/// pixels wide whose first pixel is at `origin`.
struct Sampler {
    w: usize,
    origin: [i32; 2],
    lo: [f32; 2],
    hi: [f32; 2],
}

impl Sampler {
    /// Channel `c` (0 red, 2 blue) at `(x, y)`, straight; `None` where every tap is transparent.
    fn sample(&self, planes: &Planes, c: usize, x: f32, y: f32) -> Option<f32> {
        let x = if x.is_finite() { x.clamp(self.lo[0], self.hi[0]) } else { self.lo[0] };
        let y = if y.is_finite() { y.clamp(self.lo[1], self.hi[1]) } else { self.lo[1] };
        let (x0, y0) = (x.floor(), y.floor());
        let (tx, ty) = (x - x0, y - y0);
        let x1 = (x0 + 1.0).min(self.hi[0]);
        let y1 = (y0 + 1.0).min(self.hi[1]);
        let plane = planes.plane(c);
        let (mut num, mut den) = (0.0f32, 0.0f32);
        for (xx, yy, k) in [(x0, y0, (1.0 - tx) * (1.0 - ty)), (x1, y0, tx * (1.0 - ty)), (x0, y1, (1.0 - tx) * ty), (x1, y1, tx * ty)] {
            if k <= 0.0 {
                continue;
            }
            let (bx, by) = (xx as i64 - i64::from(self.origin[0]), yy as i64 - i64::from(self.origin[1]));
            let Some(i) = usize::try_from(by).ok().zip(usize::try_from(bx).ok()).map(|(by, bx)| by * self.w + bx) else { continue };
            num += k * plane.get(i).copied().unwrap_or(0.0);
            den += k * planes.alpha.get(i).copied().unwrap_or(0.0);
        }
        (den > 1e-6).then(|| num / den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(rect: Rect, f: impl Fn(i32, i32) -> [f32; 4]) -> Buffer {
        let mut b = Buffer::transparent(rect);
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                b.px[((y - rect.y0) * rect.width() as i32 + (x - rect.x0)) as usize] = f(x, y);
            }
        }
        b
    }

    /// A fixed offset moves red one way and blue the other by whole pixels; green and alpha stay.
    #[test]
    fn offset_moves_red_and_blue_apart() {
        let canvas = Rect::new(0, 0, 32, 8);
        // A white stripe at x = 10 on black.
        let mut b = buf(canvas, |x, _| if x == 10 { [1.0; 4] } else { [0.0, 0.0, 0.0, 1.0] });
        let ca = ChromaticAberration { red_shift: 0.0, blue_shift: 0.0, offset_x: 2.0, ..Default::default() };
        apply_chromatic(&ca, &mut b, canvas);
        let at = |x: i32| b.px[(4 * 32 + x) as usize];
        assert_eq!(at(12), [1.0, 0.0, 0.0, 1.0], "red moved right");
        assert_eq!(at(8), [0.0, 0.0, 1.0, 1.0], "blue moved left");
        assert_eq!(at(10), [0.0, 1.0, 0.0, 1.0], "green stayed");
        assert!(b.px.iter().all(|p| p[3] == 1.0));
    }

    /// The radial shift is nothing at the centre and its full amount at the farthest corner;
    /// a clean centre (falloff) leaves the middle untouched.
    #[test]
    fn radial_shift_grows_from_the_centre() {
        let canvas = Rect::new(0, 0, 64, 64);
        let pattern = |x: i32, y: i32| [x as f32 / 63.0, 0.5, y as f32 / 63.0, 1.0];
        let before = buf(canvas, pattern);
        let mut b = buf(canvas, pattern);
        apply_chromatic(&ChromaticAberration { red_shift: 8.0, blue_shift: -8.0, ..Default::default() }, &mut b, canvas);
        let px = |b: &Buffer, x: i32, y: i32| b.px[(y * 64 + x) as usize];
        // Near the centre almost nothing moves; at the corner the red comes from 8 px inward
        // (along the diagonal: 8 / √2 px in x).
        assert!((px(&b, 32, 32)[0] - px(&before, 32, 32)[0]).abs() < 0.01);
        let moved = (px(&before, 63, 63)[0] - px(&b, 63, 63)[0]) * 63.0;
        assert!((moved - 8.0 / 2f32.sqrt()).abs() < 0.3, "{moved}");
        // Blue moves inward: 21.5 px above the centre it comes from 8 · 21.5 / 44.5 px farther out.
        let moved = (px(&before, 32, 10)[2] - px(&b, 32, 10)[2]) * 63.0;
        assert!((moved - 8.0 * 21.506 / 44.548).abs() < 0.1, "{moved}");
        assert_eq!(px(&b, 63, 63)[1], px(&before, 63, 63)[1]);
        let mut clean = buf(canvas, pattern);
        apply_chromatic(&ChromaticAberration { red_shift: 8.0, blue_shift: -8.0, falloff: 0.8, ..Default::default() }, &mut clean, canvas);
        for (x, y) in [(32, 32), (40, 28), (20, 40)] {
            assert_eq!(px(&clean, x, y), px(&before, x, y), "({x}, {y}) is in the clean centre");
        }
        assert_ne!(px(&clean, 63, 63), px(&before, 63, 63));
    }

    /// Transparent neighbours lend no colour, transparent pixels stay as they are, and pixels
    /// outside the canvas are left alone.
    #[test]
    fn transparency_and_the_canvas_edge() {
        let canvas = Rect::new(0, 0, 16, 4);
        let rect = Rect::new(-4, 0, 20, 4);
        let mut b = buf(rect, |x, _| if (0..8).contains(&x) { [0.8, 0.4, 0.2, 1.0] } else { [0.0; 4] });
        let before = b.clone();
        apply_chromatic(&ChromaticAberration { red_shift: 0.0, blue_shift: 0.0, offset_x: 3.0, softness: 2.0, ..Default::default() }, &mut b, canvas);
        for (i, (p, q)) in b.px.iter().zip(&before.px).enumerate() {
            let x = rect.x0 + (i % 24) as i32;
            if !(0..16).contains(&x) || q[3] <= 0.0 {
                assert_eq!(p, q, "x {x} unchanged");
            } else {
                assert!(p.iter().all(|v| v.is_finite()) && p[3] == q[3]);
                // Every sample that found colour found the layer's own, never black.
                assert!(p[0] == 0.8 || (p[0] - 0.8).abs() < 1e-4, "x {x}: {p:?}");
            }
        }
    }

    /// Hostile settings and odd buffers never panic.
    #[test]
    fn hostile_settings_do_not_panic() {
        let canvas = Rect::new(0, 0, 3, 2);
        for ca in [
            ChromaticAberration {
                red_shift: f32::NAN,
                blue_shift: f32::INFINITY,
                offset_x: -1e30,
                offset_y: 1e30,
                center: [f32::NAN, -9.0],
                falloff: f32::NAN,
                softness: 1e9,
            },
            ChromaticAberration { softness: 10.0, offset_x: 50.0, ..Default::default() },
            ChromaticAberration { falloff: 1.0, ..Default::default() },
        ] {
            for rect in [canvas, Rect::new(0, 0, 1, 1), Rect::new(50, 50, 52, 51), Rect::EMPTY] {
                let mut b = buf(rect, |x, y| [x as f32 * 0.1, 0.2, y as f32 * 0.3, 1.0]);
                apply_chromatic(&ca, &mut b, canvas);
                assert!(b.px.iter().flatten().all(|v| v.is_finite()));
            }
        }
    }
}
