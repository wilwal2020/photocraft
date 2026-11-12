//! Shading, PhotoCraft's own adjustment (see [`photocraft_doc::adjust::Shading`]): the mask is
//! the shadow amount, and each colour picks its own shadow colour in OKLab.
//!
//! Per pixel, at shadow amount `a` (the layer's opacity × mask, 1 without a mask):
//! - lightness drops by `depth · a` (relative, so dark colours don't fall to black);
//! - chroma keeps part of itself through the darkening (`C · √(L'/L)`: constant chroma would
//!   over-saturate vivid colours, constant saturation goes muddy), then moves toward the gamut
//!   edge at the new lightness and hue by `saturation · a`: muted colours (skin, khaki) gain the
//!   most, colours already near the edge hardly change;
//! - Chroma Retention sets how much chroma survives (0: physical C·L′/L, 0.5: the √ above,
//!   1: all of it); Self-bounce lifts each channel's shadow by its own reflectance (yellows
//!   ochre, oranges red, at the same lightness); Physical takes the shadow colour from the local
//!   colour times the ambient colour instead of the hue turn;
//! - the Terminator band turns hue toward its colour and raises chroma in a bump on the shadow
//!   amount (skin's warm edge), for colours near its hue;
//! - Saturation Curve swings the colour picker's saturation out and back on the way (toward the
//!   vivid edge above 0, toward grey below), no and full shadow unchanged;
//! - hue turns away from `split_hue` toward its opposite, the shorter way round, by up to
//!   `hue_shift · a` degrees, easing off near the split (no tear between neighbours turning
//!   opposite ways) and near the opposite (hues there are already as dark as hues get);
//! - greys (chroma below the `protect_neutrals` threshold) get neither hue nor chroma changes,
//!   but take on the ambient colour, which every colour leans toward by the same small OKLab
//!   offset (greys and pastels turn the most); near-black colours are left mostly alone
//!   (`protect_darks`).
//!
//! Tuned on a swatch sheet and the user's colouring (2026-11-09): skin shades to a warm
//! red-brown, blonde to gold ochre, cyan cloth to teal-blue, greys stay grey; Multiply with one
//! colour turned all of them greyish pink.

use photocraft_doc::adjust::{CurvePoint, Shading};

use super::Transfer;
use crate::Buffer;

#[inline]
pub(crate) fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Linear sRGB → OKLab (Björn Ottosson's published matrices).
pub fn lin_to_oklab([r, g, b]: [f32; 3]) -> [f32; 3] {
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab → linear sRGB (unclamped).
pub fn oklab_to_lin([l, a, b]: [f32; 3]) -> [f32; 3] {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    ]
}

fn in_gamut(c: [f32; 3]) -> bool {
    c.iter().all(|v| (-1e-5..=1.0 + 1e-5).contains(v))
}

/// The largest OKLab chroma inside sRGB at lightness `l` and hue `h` (degrees), by bisection.
fn max_chroma_exact(l: f32, h: f32) -> f32 {
    let (s, c) = h.to_radians().sin_cos();
    let (mut lo, mut hi) = (0.0f32, 0.4f32);
    for _ in 0..20 {
        let mid = (lo + hi) / 2.0;
        if in_gamut(oklab_to_lin([l, mid * c, mid * s])) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Lightness steps (0..=1) and hue steps (one per degree) of the gamut table, which the GPU
/// compositor uploads as LUT rows (`lightness · TABLE_H + hue`).
pub const TABLE_L: usize = 257;
pub const TABLE_H: usize = 360;

/// [`max_chroma_exact`] sampled every 1/256 of lightness and every degree of hue.
pub fn chroma_table() -> &'static [f32] {
    static TABLE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = Vec::with_capacity(TABLE_L * TABLE_H);
        for li in 0..TABLE_L {
            for hi in 0..TABLE_H {
                t.push(max_chroma_exact(li as f32 / (TABLE_L - 1) as f32, hi as f32 * 360.0 / TABLE_H as f32));
            }
        }
        t
    })
}

/// The largest chroma inside sRGB at lightness `l` (0..=1) and hue `h` (degrees), interpolated
/// from a table (a lower bound up to the interpolation's error; results are clipped anyway).
pub fn max_chroma(l: f32, h: f32) -> f32 {
    let t = chroma_table();
    let lf = if l.is_finite() { l.clamp(0.0, 1.0) } else { 0.0 } * (TABLE_L - 1) as f32;
    let hf = if h.is_finite() { h.rem_euclid(360.0) } else { 0.0 } * TABLE_H as f32 / 360.0;
    let (l0, h0) = (lf.floor() as usize, hf.floor() as usize);
    let (l1, h1) = ((l0 + 1).min(TABLE_L - 1), (h0 + 1) % TABLE_H);
    let (fl, fh) = (lf - l0 as f32, hf - h0 as f32);
    let at = |li: usize, hi: usize| t.get(li * TABLE_H + hi % TABLE_H).copied().unwrap_or(0.0);
    let a = at(l0, h0) + (at(l0, h1) - at(l0, h0)) * fh;
    let b = at(l1, h0) + (at(l1, h1) - at(l1, h0)) * fh;
    a + (b - a) * fl
}

/// RGB (0..=1) → a colour picker's axes: hue (degrees), saturation, brightness (HSB).
pub fn rgb_to_hsb(c: [f32; 3]) -> [f32; 3] {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = mx - mn;
    let h = if d <= 0.0 {
        0.0
    } else if mx == c[0] {
        60.0 * ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if mx == c[1] {
        60.0 * ((c[2] - c[0]) / d + 2.0)
    } else {
        60.0 * ((c[0] - c[1]) / d + 4.0)
    };
    [h, if mx > 0.0 { d / mx } else { 0.0 }, mx]
}

/// HSB (hue in degrees) → RGB.
pub fn hsb_to_rgb([h, s, v]: [f32; 3]) -> [f32; 3] {
    let h = if h.is_finite() { h.rem_euclid(360.0) / 60.0 } else { 0.0 };
    // rem_euclid of a hair below 0 rounds to 360: that is red, not the magenta sector's end.
    let h = if h >= 6.0 { 0.0 } else { h };
    let f = h - h.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match h.floor() as i32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// How far the Advanced Curve's graph reaches past the base → shadow box on each side, in box
/// widths. The band out there runs on to the picker's edges: right to full saturation, left to
/// grey, above the base to bright, below full shadow to black (Lighting: below full light to
/// bright, above the base to black).
pub const PATH_MARGIN: f32 = 0.5;
/// Samples in the Advanced Curve's table (the GPU reads the same table), equal steps along it.
pub const PATH_LEN: usize = 256;

fn point(input: f32, output: f32) -> CurvePoint {
    CurvePoint { input, output }
}

/// The curve the Saturation Curve slider (`curve`, -1..1) draws on the Advanced Curve's graph
/// (base colour at (0, 1), full shadow or light at (1, 0)): one bulge out to the side, toward
/// the vivid edge above 0 (halfway out the band at 1) and toward grey below (all the way at -1).
/// Empty at 0. The user's design, 2026-11-10: the slider draws the curve, a drawn curve locks it.
pub fn slider_points(curve: f32) -> Vec<CurvePoint> {
    if !curve.is_finite() || curve.abs() <= 1e-6 {
        return Vec::new();
    }
    let c = curve.clamp(-1.0, 1.0);
    let x = 0.5 + if c > 0.0 { c * (0.5 + 0.5 * PATH_MARGIN) } else { c * (0.5 + PATH_MARGIN) };
    vec![point(0.0, 1.0), point(x, 0.5), point(1.0, 0.0)]
}

/// The smooth curve through `points` in their order (centripetal Catmull-Rom, the ends carried
/// on straight), as a polyline in the graph's units.
pub fn path_polyline(points: &[CurvePoint]) -> Vec<[f32; 2]> {
    let p: Vec<[f32; 2]> = points.iter().map(|q| [q.input, q.output]).collect();
    let n = p.len();
    if n < 2 {
        return p;
    }
    let get = |i: isize| -> [f32; 2] {
        if i < 0 {
            [2.0 * p[0][0] - p[1][0], 2.0 * p[0][1] - p[1][1]]
        } else if i as usize >= n {
            [2.0 * p[n - 1][0] - p[n - 2][0], 2.0 * p[n - 1][1] - p[n - 2][1]]
        } else {
            p[i as usize]
        }
    };
    const SEG: usize = 48;
    let knot = |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]).hypot(b[1] - a[1]).sqrt().max(1e-4);
    let mut out = vec![p[0]];
    for i in 0..n - 1 {
        let (p0, p1, p2, p3) = (get(i as isize - 1), p[i], p[i + 1], get(i as isize + 2));
        let t1 = knot(p0, p1);
        let t2 = t1 + knot(p1, p2);
        let t3 = t2 + knot(p2, p3);
        for k in 1..=SEG {
            let t = t1 + (t2 - t1) * k as f32 / SEG as f32;
            let lerp = |a: [f32; 2], b: [f32; 2], ta: f32, tb: f32| {
                let w = (t - ta) / (tb - ta);
                [a[0] + (b[0] - a[0]) * w, a[1] + (b[1] - a[1]) * w]
            };
            let (a1, a2, a3) = (lerp(p0, p1, 0.0, t1), lerp(p1, p2, t1, t2), lerp(p2, p3, t2, t3));
            let (b1, b2) = (lerp(a1, a2, 0.0, t2), lerp(a2, a3, t1, t3));
            out.push(if k == SEG { p2 } else { lerp(b1, b2, t1, t2) });
        }
    }
    out
}

/// The Advanced Curve (`saturation_points`; the slider's curve when nothing is drawn between the
/// ends) as a table: for `PATH_LEN` equal steps along the curve (the amount, 0..1), how far the
/// colour has gone toward full shadow (or light) in the picker's saturation and in its
/// brightness: 0 the base colour, 1 full shadow, past either end the band beyond the box. None
/// for the straight line (the path itself).
pub fn path_table(points: &[CurvePoint], curve: f32) -> Option<Vec<[f32; 2]>> {
    let drawn = photocraft_doc::adjust::sanitized_points(points);
    let pts = if drawn.len() > 2 { drawn } else { slider_points(curve) };
    if pts.len() < 3 {
        return None;
    }
    let line = path_polyline(&pts);
    let mut acc = vec![0.0f32; line.len()];
    for i in 1..line.len() {
        acc[i] = acc[i - 1] + (line[i][0] - line[i - 1][0]).hypot(line[i][1] - line[i - 1][1]);
    }
    let total = *acc.last()?;
    if total.is_nan() || total <= 1e-6 {
        return None;
    }
    let last = (PATH_LEN - 1) as f32;
    let mut j = 0;
    let mut out = Vec::with_capacity(PATH_LEN);
    for k in 0..PATH_LEN {
        let at = total * k as f32 / last;
        while j + 2 < line.len() && acc[j + 1] < at {
            j += 1;
        }
        let w = ((at - acc[j]) / (acc[j + 1] - acc[j]).max(1e-9)).clamp(0.0, 1.0);
        let q = [line[j][0] + (line[j + 1][0] - line[j][0]) * w, line[j][1] + (line[j + 1][1] - line[j][1]) * w];
        out.push([q[0], 1.0 - q[1]]);
    }
    // The ends are the box's corners exactly: no shadow and full shadow stay put.
    out[0] = [0.0, 0.0];
    out[PATH_LEN - 1] = [1.0, 1.0];
    if out.iter().enumerate().all(|(i, q)| (q[0] - i as f32 / last).abs() < 1e-4 && (q[1] - i as f32 / last).abs() < 1e-4) {
        return None;
    }
    Some(out)
}

/// The point of a [`path_table`] at amount `a`.
pub fn path_at(table: &[[f32; 2]], a: f32) -> [f32; 2] {
    let last = table.len().saturating_sub(1);
    let x = if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 } * last as f32;
    let i = (x.floor() as usize).min(last);
    let j = (i + 1).min(last);
    let (p, q, w) = (table[i], table[j], x - i as f32);
    [p[0] + (q[0] - p[0]) * w, p[1] + (q[1] - p[1]) * w]
}

/// The Hue Curve (`hue_points`; x the amount, y how far the hue has turned: 1 the base colour's,
/// 0 full shadow's, below the box further on, above it back the other way) as a table: for
/// `PATH_LEN` amounts, how far the hue has turned (0..1 inside the box). None for the straight
/// line (the path's own timing).
pub fn hue_table(points: &[CurvePoint]) -> Option<Vec<f32>> {
    let pts = photocraft_doc::adjust::sanitized_hue_points(points);
    if pts.len() < 3 {
        return None;
    }
    // The spline keeps to 0..1, so fit it over the whole graph (-0.5..1.5) and map back.
    let span = 1.0 + 2.0 * PATH_MARGIN;
    let fitted: Vec<CurvePoint> = pts.iter().map(|q| point(q.input, (q.output + PATH_MARGIN) / span)).collect();
    let mut t: Vec<f32> = super::curve_samples::<PATH_LEN>(&fitted).into_iter().map(|y| 1.0 - (y * span - PATH_MARGIN)).collect();
    t[0] = 0.0;
    t[PATH_LEN - 1] = 1.0;
    let last = (PATH_LEN - 1) as f32;
    if t.iter().enumerate().all(|(i, v)| (v - i as f32 / last).abs() < 1e-4) {
        return None;
    }
    Some(t)
}

/// The value of a [`hue_table`] at amount `a`.
pub fn hue_at(table: &[f32], a: f32) -> f32 {
    let last = table.len().saturating_sub(1);
    let x = if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 } * last as f32;
    let i = (x.floor() as usize).min(last);
    let j = (i + 1).min(last);
    table[i] + (table[j] - table[i]) * (x - i as f32)
}

/// Where progress `p` across the box puts a picker axis that runs from `v0` (the base colour) to
/// `v1` (full shadow or light): in between inside the box, on toward `before` past the base end
/// and toward `beyond` past the far end (all the way at the graph's edge).
pub fn band(p: f32, v0: f32, v1: f32, before: f32, beyond: f32) -> f32 {
    if p < 0.0 {
        v0 + (before - v0) * (-p / PATH_MARGIN).min(1.0)
    } else if p > 1.0 {
        v1 + (beyond - v1) * ((p - 1.0) / PATH_MARGIN).min(1.0)
    } else {
        v0 + (v1 - v0) * p
    }
}

/// The Advanced Curve on one colour. `path` (document-encoded) is the colour on its path at
/// amount `a`; it moves by how far the curve's point `at` ([`path_at`]) sits from the straight
/// line, in the picker's saturation and brightness, with the box stretched over this colour's own
/// `base` and `end` (its full shadow or light), so one curve fits every colour. `k` is how much of
/// the saturation move the colour takes (greys and linework are spared, as by the path's other
/// curves), `kv` how much of the brightness move (linework); `lighting` turns the brightness
/// bands round (past full light is brighter). `hue` is the Hue Curve's turn at this amount
/// ([`hue_at`]; `a` itself when it is straight): the path's hue moves by how far that is from
/// `a`, as a share of this colour's own turn from base to end.
///
/// `literal` (Literal Curve, user request 2026-11-11): the curve keeps its drawn shape instead of
/// being stretched to each colour's box: its distance from the straight line is applied the same
/// in both directions, scaled to this colour's own path length (the box's diagonal is the path
/// from base to end), right always more saturated and up always brighter (Lighting: down
/// brighter); past the box it carries on in proportion rather than toward the picker's edges.
#[allow(clippy::too_many_arguments)]
pub fn bend(path: [f32; 3], base: [f32; 3], end: [f32; 3], at: [f32; 2], hue: f32, a: f32, k: f32, kv: f32, lighting: bool, literal: bool) -> [f32; 3] {
    // On the straight lines (the ends, always): the path itself, bit for bit.
    if (at[0] - a).abs() <= 1e-6 && (at[1] - a).abs() <= 1e-6 && (hue - a).abs() <= 1e-6 {
        return path;
    }
    let clean = |c: [f32; 3]| c.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
    let weight = |k: f32| if k.is_finite() { k.clamp(0.0, 1.0) } else { 0.0 };
    let a = if a.is_finite() { a } else { 0.0 };
    let at = at.map(|v| if v.is_finite() { v } else { a });
    let turn = if hue.is_finite() { hue } else { a };
    let ([h, s, v], [h0, s0, v0], [h1, s1, v1]) = (rgb_to_hsb(clean(path)), rgb_to_hsb(clean(base)), rgb_to_hsb(clean(end)));
    let (before, beyond) = if lighting { (0.0, 1.0) } else { (1.0, 0.0) };
    let (ds, dv) = if literal {
        let scale = (s1 - s0).hypot(v1 - v0) / std::f32::consts::SQRT_2;
        ((at[0] - a) * scale, (at[1] - a) * scale * (beyond - before))
    } else {
        (band(at[0], s0, s1, 0.0, 1.0) - band(a, s0, s1, 0.0, 1.0), band(at[1], v0, v1, before, beyond) - band(a, v0, v1, before, beyond))
    };
    let mut ds = ds * weight(k);
    // A colour with almost no saturation has no hue to go vivid in.
    if ds > 0.0 {
        ds *= smoothstep(0.0, 0.02, s.max(s0).max(s1));
    }
    let dv = dv * weight(kv);
    let mut hue = if s > 1e-3 {
        h
    } else if s1 > 1e-3 {
        h1
    } else {
        h0
    };
    // (A grey end or base has no hue to turn between.)
    if (turn - a).abs() > 1e-6 && s0 > 1e-3 && s1 > 1e-3 {
        hue += (turn - a) * hue_diff(h0, h1);
    }
    hsb_to_rgb([hue, (s + ds).clamp(0.0, 1.0), (v + dv).clamp(0.0, 1.0)])
}

/// Shading settings checked once per render, with the ambient colour's direction in OKLab.
pub struct Prepared {
    s: Shading,
    ambient_dir: [f32; 2],
    ambient_light: [f32; 3],
    terminator_hue: f32,
    path: Option<Vec<[f32; 2]>>,
    hue: Option<Vec<f32>>,
}

/// The signed turn (degrees, -180..180) from hue `from` to hue `to`, the shorter way round.
pub fn hue_diff(from: f32, to: f32) -> f32 {
    (to - from + 180.0).rem_euclid(360.0) - 180.0
}

/// Linear-light `c` as a light: scaled to luminance 1 (only its colour is kept), white if black.
pub fn as_light(c: [f32; 3]) -> [f32; 3] {
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    if y > 1e-4 { c.map(|v| (v / y).min(8.0)) } else { [1.0; 3] }
}

impl Prepared {
    pub fn new(s: &Shading) -> Self {
        let s = s.sanitized();
        let lin = s.ambient.map(photocraft_color::convert::srgb_to_linear);
        let [_, a, b] = lin_to_oklab(lin);
        let n = a.hypot(b);
        let ambient_dir = if n > 1e-5 { [a / n, b / n] } else { [0.0, 0.0] };
        let [_, ta, tb] = lin_to_oklab(s.terminator.map(photocraft_color::convert::srgb_to_linear));
        let terminator_hue = tb.atan2(ta).to_degrees().rem_euclid(360.0);
        let path = path_table(&s.saturation_points, s.saturation_curve);
        let hue = hue_table(&s.hue_points);
        Self { s, ambient_dir, ambient_light: as_light(lin), terminator_hue, path, hue }
    }

    /// The settings, sanitized.
    pub fn settings(&self) -> &Shading {
        &self.s
    }

    /// The ambient colour as a light (linear, luminance 1), for the physical shadow colour.
    pub fn ambient_light(&self) -> [f32; 3] {
        self.ambient_light
    }

    /// The Advanced Curve's table ([`path_table`]), None when straight.
    pub fn path(&self) -> Option<&[[f32; 2]]> {
        self.path.as_deref()
    }

    /// The Advanced Curve's point at amount `a` ([`path_at`]), None when straight.
    pub fn bent(&self, a: f32) -> Option<[f32; 2]> {
        Some(path_at(self.path.as_deref()?, a))
    }

    /// The Hue Curve's table ([`hue_table`]), None when straight.
    pub fn hue_curve(&self) -> Option<&[f32]> {
        self.hue.as_deref()
    }

    /// The Hue Curve's turn at amount `a` ([`hue_at`]), None when straight.
    pub fn hue_bent(&self, a: f32) -> Option<f32> {
        Some(hue_at(self.hue.as_deref()?, a))
    }

    /// How much of the path linear-light `c` takes (Protect Darks: near-black colours barely move).
    pub fn keep(&self, c: [f32; 3]) -> f32 {
        let l = lin_to_oklab(c.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }))[0];
        1.0 - self.s.protect_darks * (1.0 - smoothstep(0.2, 0.5, l))
    }

    /// The terminator band's OKLCH hue (degrees).
    pub fn terminator_hue(&self) -> f32 {
        self.terminator_hue
    }

    /// The ambient colour's unit direction in OKLab's a/b plane.
    pub fn ambient_direction(&self) -> [f32; 2] {
        self.ambient_dir
    }

    /// Whether this shading changes nothing at any amount.
    pub fn is_identity(&self) -> bool {
        let s = &self.s;
        s.depth == 0.0
            && s.saturation == 0.0
            && self.path.is_none()
            && self.hue.is_none()
            && s.hue_shift == 0.0
            && s.ambient_amount == 0.0
            && s.mood_amount == 0.0
            && s.terminator_amount == 0.0
            && !s.physical
    }

    /// The amount after Steps: the mask snapped to `steps` tones (none below half a step).
    pub fn stepped(&self, a: f32) -> f32 {
        let a = if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 };
        match self.s.steps {
            n @ 2.. => (a * n as f32 + 0.5).floor() / n as f32,
            _ => a,
        }
    }

    /// Shade linear-light `c` at amount `a` (already stepped).
    pub fn shade_lin(&self, c: [f32; 3], a: f32) -> [f32; 3] {
        self.shade_lin_weighted(c, a).0
    }

    /// [`Self::shade_lin`], with how much of the Advanced Curve's saturation move the colour
    /// takes ([`bend`]).
    pub fn shade_lin_weighted(&self, c: [f32; 3], a: f32) -> ([f32; 3], f32) {
        let s = &self.s;
        if a <= 0.0 || c.iter().any(|v| !v.is_finite()) {
            return (c, 0.0);
        }
        let c = c.map(|v| v.clamp(0.0, 1.0));
        let [l, la, lb] = lin_to_oklab(c);
        let chroma = la.hypot(lb);
        let hue = lb.atan2(la).to_degrees().rem_euclid(360.0);
        // Near-black colours barely move; greys get no hue or chroma changes.
        let keep = 1.0 - s.protect_darks * (1.0 - smoothstep(0.2, 0.5, l));
        let ae = a * keep;
        let c0 = 0.05 * s.protect_neutrals;
        let w = if c0 > 0.0 { smoothstep(0.3 * c0, 1.3 * c0, chroma) } else { 1.0 };
        let d = (hue - s.split_hue + 180.0).rem_euclid(360.0) - 180.0;
        let to_pole = 180.0 - d.abs();
        let taper = smoothstep(0.0, 30.0, d.abs()) * smoothstep(0.0, 70.0, to_pole);
        let l2 = l * (1.0 - s.depth * ae);
        let ratio = (l2 / l.max(1e-6)).clamp(0.0, 1.0);
        let (mut h2, base) = if s.physical {
            // Physical: the local colour lit by the ambient colour (linear RGB; half of the
            // shadow's light, the rest bounce), at its own lightness; the darkening below takes
            // it to L′.
            let lit: [f32; 3] = std::array::from_fn(|i| c[i] * (1.0 + (self.ambient_light[i] - 1.0) * ae * 0.5));
            let [lp, ap, bp] = lin_to_oklab(lit);
            let k = l / lp.max(1e-6);
            (bp.atan2(ap).to_degrees().rem_euclid(360.0), (ap * k).hypot(bp * k))
        } else {
            // Hue: away from the split, toward its opposite, the shorter way round.
            let turned = (s.hue_shift * ae * w * taper).min(to_pole);
            (hue + d.signum() * turned, chroma)
        };
        // Chroma Retention: C·(L′/L)^(1 − retention); 0.5 is the √ of palette practice.
        // (Shading 2: straight down the picker, chroma dims with the light.)
        let retention = if s.picker_axes { 0.0 } else { s.chroma_retention };
        let mut kept = base * ratio.max(1e-6).powf(1.0 - retention);
        if s.self_bounce > 0.0 && chroma > 1e-5 {
            // Self-bounce: each channel's shadow lifted by its own reflectance (multi-bounce
            // occlusion, V / (1 − (1 − V)·albedo)), taken as a hue turn and chroma ratio against
            // plain dimming at the same lightness.
            let v = ratio * ratio * ratio;
            let lit: [f32; 3] = std::array::from_fn(|i| c[i] * v / (1.0 - (1.0 - v) * c[i]).max(1e-6));
            let [lb2, ab2, bb2] = lin_to_oklab(lit);
            let cb = ab2.hypot(bb2) * l2 / lb2.max(1e-6);
            let sb = s.self_bounce * w;
            h2 += hue_diff(hue, bb2.atan2(ab2).to_degrees()) * sb;
            kept *= 1.0 + ((cb / (chroma * ratio).max(1e-6)).clamp(0.0, 4.0) - 1.0) * sb;
        }
        // Chroma: then toward the gamut edge.
        let cmax = max_chroma(l2, h2).max(0.0);
        // (Shading 2: sideways at this lightness, toward the vivid edge or toward grey.)
        let push = if s.picker_axes { (s.saturation_shift * ae * w).clamp(-1.0, 1.0) } else { (s.saturation * ae * w).clamp(0.0, 1.0) };
        let mut c2 = if push >= 0.0 { kept + (cmax - kept).max(0.0) * push } else { kept * (1.0 + push) }.min(cmax);
        // Terminator band: a bump on the shadow amount, turning hue toward the band colour and
        // raising chroma, for colours near its hue (blues barely move).
        let x = (a - s.terminator_position) / s.terminator_width;
        let bump = if x.abs() < 1.0 { (1.0 - x * x) * (1.0 - x * x) } else { 0.0 };
        let k = s.terminator_amount * bump * smoothstep(-0.2, 1.0, hue_diff(h2, self.terminator_hue).to_radians().cos()) * keep * w;
        if k > 0.0 {
            h2 += hue_diff(h2, self.terminator_hue) * k * 0.6;
            let cmax = max_chroma(l2, h2).max(0.0);
            c2 = (c2 + (cmax - c2).max(0.0) * k * 0.6).min(cmax.max(c2));
        }
        let (sin, cos) = h2.to_radians().sin_cos();
        // One OKLab offset for every colour: it turns greys and pastels a long way and vivid
        // colours hardly at all, as sky-lit shade does.
        let tint = 0.06 * s.ambient_amount * ae;
        let out = [l2, c2 * cos + self.ambient_dir[0] * tint, c2 * sin + self.ambient_dir[1] * tint];
        (oklab_to_lin(out).map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }), keep * w)
    }

    /// The Multiply-style mood tint at amount `a`, on document-encoded `c`.
    fn mood(&self, c: [f32; 3], a: f32) -> [f32; 3] {
        let k = self.s.mood_amount * a;
        if k <= 0.0 {
            return c;
        }
        std::array::from_fn(|i| c[i] * (1.0 - k * (1.0 - self.s.mood[i])))
    }
}

/// Apply Shading to `buf` (document-encoded through `transfer`). `amounts` holds each pixel's
/// shadow amount (the layer's opacity × mask), or None for full shadow everywhere.
pub fn apply_shading(s: &Shading, buf: &mut Buffer, amounts: Option<&[f32]>, transfer: Transfer) {
    let p = Prepared::new(s);
    if p.is_identity() {
        return;
    }
    for (i, px) in buf.px.iter_mut().enumerate() {
        if px[3] <= 0.0 {
            continue;
        }
        let a = p.stepped(amounts.map_or(1.0, |m| m.get(i).copied().unwrap_or(0.0)));
        if a <= 0.0 {
            continue;
        }
        let src = [px[0], px[1], px[2]].map(|v| transfer.decode(v));
        let (lin, k) = p.shade_lin_weighted(src, a);
        let mut shaded = lin.map(|v| transfer.encode(v));
        let (at, turn) = (p.bent(a), p.hue_bent(a));
        if at.is_some() || turn.is_some() {
            let end = p.shade_lin(src, 1.0).map(|v| transfer.encode(v));
            shaded = bend(shaded, [px[0], px[1], px[2]], end, at.unwrap_or([a, a]), turn.unwrap_or(a), a, k, p.keep(src), false, p.s.literal_curve);
        }
        let out = p.mood(shaded, a);
        px[0] = out[0];
        px[1] = out[1];
        px[2] = out[2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_geom::Rect;

    fn lch(srgb: [f32; 3]) -> [f32; 3] {
        let [l, a, b] = lin_to_oklab(srgb.map(photocraft_color::convert::srgb_to_linear));
        [l, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0)]
    }

    fn shade(srgb: [f32; 3], a: f32, s: &Shading) -> [f32; 3] {
        let mut b = Buffer::filled(Rect::new(0, 0, 1, 1), [srgb[0], srgb[1], srgb[2], 1.0]);
        apply_shading(s, &mut b, Some(&[a]), Transfer::Srgb);
        [b.px[0][0], b.px[0][1], b.px[0][2]]
    }

    fn hue_turn(from: f32, to: f32) -> f32 {
        (to - from + 180.0).rem_euclid(360.0) - 180.0
    }

    #[test]
    fn oklab_round_trips() {
        for c in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.9, 0.2, 0.1], [0.1, 0.5, 0.9], [0.3, 0.3, 0.3]] {
            let back = oklab_to_lin(lin_to_oklab(c));
            for k in 0..3 {
                assert!((back[k] - c[k]).abs() < 1e-4, "{c:?} → {back:?}");
            }
        }
        // White is L 1 with no chroma.
        let w = lin_to_oklab([1.0; 3]);
        assert!((w[0] - 1.0).abs() < 1e-3 && w[1].hypot(w[2]) < 1e-3);
    }

    #[test]
    fn table_chroma_follows_the_gamut_edge() {
        for (l, h) in [(0.5, 30.0), (0.7, 140.0), (0.45, 265.0), (0.9, 100.0), (0.3, 200.0), (0.63, 331.0)] {
            let (t, e) = (max_chroma(l, h), max_chroma_exact(l, h));
            assert!((t - e).abs() < 0.012, "L {l} h {h}: table {t} vs {e}");
        }
        // White has no chroma to spare (at L 0 any chroma is black within rounding).
        assert!(max_chroma(1.0, 50.0) < 0.01, "{}", max_chroma(1.0, 50.0));
        assert!(max_chroma(f32::NAN, f32::NAN).is_finite());
    }

    /// The colours the user shades (skin, blonde hair, cyan cloth) each turn their own way,
    /// darker and richer; greys stay grey (2026-11-09).
    #[test]
    fn each_colour_picks_its_own_shadow() {
        let s = Shading::default();
        let skin = [0.93, 0.70, 0.52];
        let (before, after) = (lch(skin), lch(shade(skin, 1.0, &s)));
        assert!(after[0] < before[0] * 0.75, "skin darkens: {before:?} → {after:?}");
        assert!(hue_turn(before[2], after[2]) < -8.0, "skin turns toward red: {before:?} → {after:?}");
        assert!(after[1] / after[0] > 1.3 * before[1] / before[0], "skin gets richer: {before:?} → {after:?}");
        let blonde = [0.96, 0.88, 0.55];
        let (before, after) = (lch(blonde), lch(shade(blonde, 1.0, &s)));
        assert!(hue_turn(before[2], after[2]) < -4.0, "blonde turns toward orange, not green: {before:?} → {after:?}");
        let cyan = [0.45, 0.80, 0.85];
        let (before, after) = (lch(cyan), lch(shade(cyan, 1.0, &s)));
        assert!(hue_turn(before[2], after[2]) > 8.0, "cyan turns toward blue: {before:?} → {after:?}");
        let grey = [0.6, 0.6, 0.6];
        let (before, after) = (lch(grey), lch(shade(grey, 1.0, &Shading { ambient_amount: 0.0, ..s.clone() })));
        assert!(after[0] < before[0] * 0.75 && after[1] < 0.005, "grey darkens and stays grey: {after:?}");
        // With the ambient colour, a grey picks up its hue.
        let tinted = lch(shade(grey, 1.0, &Shading { ambient_amount: 1.0, ..s.clone() }));
        let ambient = lch(s.ambient);
        assert!(tinted[1] > 0.02 && hue_turn(ambient[2], tinted[2]).abs() < 10.0, "{tinted:?} vs {ambient:?}");
    }

    #[test]
    fn vivid_colours_stay_in_gamut_and_blacks_stay_put() {
        let s = Shading { saturation: 1.0, hue_shift: 90.0, depth: 1.0, ..Shading::default() };
        for c in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0, 0.0], [1.0, 1.0, 1.0], [0.85, 0.15, 0.15]] {
            for a in [0.1, 0.5, 1.0] {
                let o = shade(c, a, &s);
                assert!(o.iter().all(|v| (0.0..=1.0).contains(v)), "{c:?} at {a}: {o:?}");
            }
        }
        // A vivid red keeps its chroma through light shadow rather than jumping to the edge.
        let red = [0.85, 0.15, 0.15];
        let (r0, r1) = (lch(red), lch(shade(red, 0.25, &Shading::default())));
        assert!(r1[1] < r0[1] * 1.05, "{r0:?} → {r1:?}");
        let black = [0.04, 0.04, 0.05];
        let o = shade(black, 1.0, &Shading::default());
        assert!((0..3).all(|k| (o[k] - black[k]).abs() < 0.02), "{o:?}");
    }

    #[test]
    fn amount_and_steps() {
        let s = Shading::default();
        let skin = [0.93, 0.70, 0.52];
        assert_eq!(shade(skin, 0.0, &s), skin);
        let ls: Vec<f32> = [0.2, 0.4, 0.6, 0.8, 1.0].iter().map(|&a| lch(shade(skin, a, &s))[0]).collect();
        assert!(ls.windows(2).all(|w| w[1] < w[0]), "darker with more shadow: {ls:?}");
        // Steps: the mask snaps to tones; nothing below half a step.
        let cel = Shading { steps: 2, ..s.clone() };
        assert_eq!(shade(skin, 0.2, &cel), skin);
        assert_eq!(shade(skin, 0.4, &cel), shade(skin, 0.6, &cel));
        assert_eq!(shade(skin, 0.8, &cel), shade(skin, 1.0, &s));
        // Mood: a Multiply-style tint toward its colour.
        let mood = shade([1.0; 3], 1.0, &Shading { depth: 0.0, ambient_amount: 0.0, mood: [0.5, 1.0, 1.0], mood_amount: 1.0, ..s.clone() });
        assert!((mood[0] - 0.5).abs() < 1e-3 && (mood[1] - 1.0).abs() < 1e-3, "{mood:?}");
    }

    /// Saturation Curve swings the picker saturation out and back on the way to full shadow:
    /// brightness and hue stay on the path, 0 is the path itself, +100 reaches the picker's vivid
    /// edge halfway and -100 grey, no and full shadow are unchanged, and it is one arc (the
    /// saturation turns once at most). Greys stay grey.
    #[test]
    fn saturation_curve_swings_the_saturation() {
        // The slider draws one bulge on the Advanced Curve: out toward the vivid edge and back
        // (above 0), or toward grey and back (below), with no shadow and full shadow unchanged.
        // (Without the terminator band, whose bump on the path is its own.)
        let s = Shading { terminator_amount: 0.0, ..Shading::default() };
        let curved = |k: f32| Shading { saturation_curve: k, ..s.clone() };
        for start in [[0.98, 0.84, 0.735], [0.93, 0.70, 0.52], [0.96, 0.88, 0.55], [0.45, 0.80, 0.85]] {
            let end = rgb_to_hsb(shade(start, 1.0, &s));
            for k in [-1.0, -0.5, 0.0, 0.5, 1.0] {
                assert_eq!(shade(start, 1.0, &curved(k)), shade(start, 1.0, &s), "{start:?} {k}: full shadow unchanged");
                let mut sat = vec![rgb_to_hsb(start)[1]];
                for i in 1..40 {
                    let t = i as f32 / 40.0;
                    let (p0, p) = (rgb_to_hsb(shade(start, t, &s)), rgb_to_hsb(shade(start, t, &curved(k))));
                    if k == 0.0 {
                        assert_eq!(shade(start, t, &curved(k)), shade(start, t, &s));
                    }
                    assert!((p[2] - p0[2]).abs() < 0.15 && (p[1] < 0.01 || hue_turn(p0[0], p[0]).abs() < 0.5), "{start:?} {k} at {t}: {p0:?} → {p:?}");
                    assert!((p[1] - p0[1]) * k >= -1e-3, "{start:?} {k} at {t}: the curve's way {p0:?} → {p:?}");
                    sat.push(p[1]);
                }
                sat.push(end[1]);
                let d: Vec<f32> = sat.windows(2).map(|w| w[1] - w[0]).filter(|d| d.abs() > 0.002).collect();
                let turns = d.windows(2).filter(|w| w[0].signum() != w[1].signum()).count();
                assert!(turns <= 1, "{start:?} {k}: one arc {sat:?}");
            }
            let along = |k: f32| (1..40).map(|i| rgb_to_hsb(shade(start, i as f32 / 40.0, &curved(k)))[1]).collect::<Vec<_>>();
            let (most, least) = (along(1.0).into_iter().fold(0.0f32, f32::max), along(-1.0).into_iter().fold(1.0f32, f32::min));
            assert!(most > end[1] + (1.0 - end[1]) * 0.5 - 0.03 && least < 0.03, "{start:?}: {most} {least}");
        }
        // Greys stay grey, whichever way it swings.
        let grey = lch(shade([0.6; 3], 0.5, &Shading { ambient_amount: 0.0, ..curved(1.0) }));
        assert!(grey[1] < 0.005, "{grey:?}");
    }

    /// Chroma Retention orders shadow chroma (physical < palette < graphic) at the same lightness;
    /// Self-bounce turns yellows toward orange at the same lightness and leaves greys alone.
    #[test]
    fn chroma_retention_and_self_bounce() {
        let s = Shading { self_bounce: 0.0, terminator_amount: 0.0, saturation: 0.0, ..Shading::default() };
        let skin = [0.93, 0.70, 0.52];
        let at = |r: f32| lch(shade(skin, 1.0, &Shading { chroma_retention: r, ..s.clone() }));
        let (p, m, g) = (at(0.0), at(0.5), at(1.0));
        assert!(p[1] < m[1] && m[1] < g[1], "{p:?} {m:?} {g:?}");
        assert!((p[0] - g[0]).abs() < 0.003, "same lightness: {p:?} {g:?}");
        let yellow = [0.96, 0.88, 0.30];
        let (no, yes) = (lch(shade(yellow, 1.0, &s)), lch(shade(yellow, 1.0, &Shading { self_bounce: 1.0, ..s.clone() })));
        assert!(hue_turn(no[2], yes[2]) < -3.0 && (no[0] - yes[0]).abs() < 0.003, "{no:?} → {yes:?}");
        let grey = [0.6; 3];
        let (a, b) = (shade(grey, 1.0, &s), shade(grey, 1.0, &Shading { self_bounce: 1.0, ..s.clone() }));
        assert!((0..3).all(|k| (a[k] - b[k]).abs() < 1e-4), "{a:?} {b:?}");
    }

    /// The terminator band warms skin where light turns to shadow, nowhere else, and barely
    /// touches blue.
    #[test]
    fn terminator_band_warms_the_edge() {
        let s = Shading { self_bounce: 0.0, terminator_amount: 0.0, ..Shading::default() };
        let t = Shading { terminator_amount: 1.0, ..s.clone() };
        let band = lch(s.terminator)[2];
        let skin = [0.93, 0.70, 0.52];
        let (no, yes) = (lch(shade(skin, 0.4, &s)), lch(shade(skin, 0.4, &t)));
        assert!(hue_turn(no[2], band).abs() > hue_turn(yes[2], band).abs() + 3.0 && yes[1] > no[1], "{no:?} → {yes:?}");
        assert!((no[0] - yes[0]).abs() < 0.003, "same lightness");
        for a in [0.05, 0.75, 1.0] {
            assert_eq!(shade(skin, a, &s), shade(skin, a, &t), "outside the band at {a}");
        }
        let blue = [0.25, 0.40, 0.85];
        let (no, yes) = (lch(shade(blue, 0.4, &s)), lch(shade(blue, 0.4, &t)));
        assert!(hue_turn(no[2], yes[2]).abs() < 1.0 && (no[1] - yes[1]).abs() < 0.005, "{no:?} → {yes:?}");
    }

    /// Physical: the shadow takes the ambient colour as its light (greys turn toward it, skin
    /// toward it too), at the depth rule's lightness.
    #[test]
    fn physical_shadow_takes_the_ambient_light() {
        let s = Shading { self_bounce: 0.0, terminator_amount: 0.0, ambient_amount: 0.0, ..Shading::default() };
        let p = Shading { physical: true, ..s.clone() };
        let ambient = lch(s.ambient)[2];
        let grey = lch(shade([0.6; 3], 1.0, &p));
        assert!(grey[1] > 0.02 && hue_turn(ambient, grey[2]).abs() < 15.0, "{grey:?} vs {ambient}");
        let skin = [0.93, 0.70, 0.52];
        let (art, phy) = (lch(shade(skin, 1.0, &s)), lch(shade(skin, 1.0, &p)));
        assert!((art[0] - phy[0]).abs() < 0.003, "same lightness: {art:?} {phy:?}");
        assert!(hue_turn(phy[2], ambient).abs() < hue_turn(lch(skin)[2], ambient).abs(), "toward the ambient: {phy:?}");
    }

    /// Shading 2: Brightness goes straight down the colour picker (picker saturation kept within
    /// sRGB's curve), Saturation sideways at the same lightness: +100 to the vivid edge, -100 to
    /// grey, in order between.
    #[test]
    fn shading_2_moves_along_the_picker_axes() {
        let s = Shading { hue_shift: 0.0, self_bounce: 0.0, terminator_amount: 0.0, ambient_amount: 0.0, saturation_shift: 0.0, ..Shading::picker() };
        let at = |k: f32| Shading { saturation_shift: k, ..s.clone() };
        for c in [[0.93, 0.70, 0.52], [0.45, 0.80, 0.85], [0.82, 0.22, 0.24], [0.96, 0.88, 0.55]] {
            let (st, dn) = (rgb_to_hsb(c), rgb_to_hsb(shade(c, 1.0, &s)));
            assert!(dn[2] < st[2] - 0.05 && (dn[1] - st[1]).abs() < 0.04, "{c:?}: straight down {st:?} → {dn:?}");
            let ch: Vec<[f32; 3]> = [-1.0, -0.5, 0.0, 0.5, 1.0].iter().map(|&k| lch(shade(c, 1.0, &at(k)))).collect();
            assert!(ch.windows(2).all(|w| w[0][1] < w[1][1] + 1e-4 && (w[0][0] - w[1][0]).abs() < 0.003), "{c:?}: {ch:?}");
            assert!(ch[0][1] < 0.005, "{c:?}: grey at -100 {:?}", ch[0]);
            let edge = max_chroma(ch[4][0], ch[4][2]);
            assert!(ch[4][1] > edge * 0.9, "{c:?}: vivid edge at +100 {:?} vs {edge}", ch[4]);
        }
        assert_eq!(photocraft_doc::Adjustment::Shading(Shading::picker()).label(), "Shading 2");
    }

    #[test]
    fn picker_helpers_and_the_curve_table() {
        for c in [[0.9, 0.2, 0.1], [0.1, 0.8, 0.3], [0.2, 0.3, 0.9], [0.5, 0.5, 0.5], [0.0; 3], [1.0, 0.0, 1.0]] {
            let back = hsb_to_rgb(rgb_to_hsb(c));
            assert!((0..3).all(|k| (back[k] - c[k]).abs() < 1e-5), "{c:?} → {back:?}");
        }
        // A hue a hair below 0 is red, not magenta.
        assert_eq!(hsb_to_rgb([-1e-9, 1.0, 1.0]), [1.0, 0.0, 0.0]);
        let pt = |input: f32, output: f32| CurvePoint { input, output };
        // The slider draws one bulge: out the vivid side above 0, toward grey below, nothing at 0.
        assert!(slider_points(0.0).is_empty() && slider_points(f32::NAN).is_empty());
        assert!((slider_points(1.0)[1].input - 1.25).abs() < 1e-6 && (slider_points(-1.0)[1].input + 0.5).abs() < 1e-6);
        // No curve, the straight line and a point on it are the path itself.
        assert!(path_table(&[], 0.0).is_none());
        assert!(path_table(&[pt(0.0, 1.0), pt(1.0, 0.0)], 0.0).is_none());
        assert!(path_table(&[pt(0.0, 1.0), pt(0.4, 0.6), pt(1.0, 0.0)], 0.0).is_none());
        let t = path_table(&[], 1.0).unwrap();
        assert_eq!(t.len(), PATH_LEN);
        let (first, last) = (t[0], t[PATH_LEN - 1]);
        assert!(first[0].abs() < 1e-4 && first[1].abs() < 1e-4 && (last[0] - 1.0).abs() < 1e-4 && (last[1] - 1.0).abs() < 1e-4, "{first:?} {last:?}");
        assert!((0..PATH_LEN).any(|i| t[i][0] > 1.2), "out to the vivid side");
        // A drawn curve wins over the slider.
        let drawn = [pt(0.0, 1.0), pt(0.6, 0.95), pt(1.0, 0.0)];
        assert_eq!(path_table(&drawn, 1.0), path_table(&drawn, 0.0));
        // On the straight line, bend changes nothing; anything non-finite stays finite.
        let c = [0.8, 0.5, 0.4];
        let same = bend(c, [0.9, 0.7, 0.6], [0.4, 0.2, 0.15], [0.5, 0.5], 0.5, 0.5, 1.0, 1.0, false, false);
        assert!((0..3).all(|k| (same[k] - c[k]).abs() < 1e-5), "{same:?}");
        assert!(bend([f32::NAN, 2.0, -1.0], c, c, [f32::NAN, 9.0], f32::NAN, 0.5, f32::NAN, 1.0, true, true).iter().all(|v| v.is_finite()));
    }

    #[test]
    fn hostile_input_stays_finite() {
        let s = Shading { depth: f32::NAN, saturation: f32::INFINITY, saturation_curve: f32::NAN, hue_shift: -5.0, split_hue: f32::NAN, ..Shading::default() };
        for c in [[f32::NAN, 0.5, 0.5], [2.0, -1.0, 0.5], [0.5; 3]] {
            for a in [f32::NAN, -1.0, 0.5, 9.0] {
                let mut b = Buffer::filled(Rect::new(-3, -3, -2, -2), [c[0], c[1], c[2], 1.0]);
                apply_shading(&s, &mut b, Some(&[a]), Transfer::Srgb);
                assert!(b.px[0].iter().all(|v| v.is_finite() || c.iter().any(|x| x.is_nan())), "{c:?} at {a}: {:?}", b.px[0]);
            }
        }
        // A short amounts slice shades nothing past its end.
        let mut b = Buffer::filled(Rect::new(0, 0, 2, 1), [0.9, 0.7, 0.5, 1.0]);
        apply_shading(&Shading::default(), &mut b, Some(&[1.0]), Transfer::Srgb);
        assert_eq!(b.px[1], [0.9, 0.7, 0.5, 1.0]);
    }

    /// The advanced Saturation Curve (user request, 2026-11-10): the straight line is the path
    /// itself; drawn above it the colour keeps the base colour's saturation longer, below it takes
    /// full shadow's sooner; brightness stays on the path and full shadow is unchanged.
    #[test]
    fn a_literal_curve_keeps_its_shape_on_the_picker() {
        let pt = |input: f32, output: f32| CurvePoint { input, output };
        // A bulge straight out to the right: literal, every colour moves sideways by the same
        // share of its own path length, and only sideways; stretched, it depends on the box.
        let bulge = vec![pt(0.0, 1.0), pt(0.8, 0.5), pt(1.0, 0.0)];
        let base = Shading { terminator_amount: 0.0, protect_neutrals: 0.0, ..Shading::picker() };
        let literal = Shading { saturation_points: bulge.clone(), literal_curve: true, ..base.clone() };
        let stretched = Shading { saturation_points: bulge, ..base.clone() };
        let hsb = |c: [f32; 3]| rgb_to_hsb(c);
        let mut shares = Vec::new();
        for c in [[0.89, 0.23, 0.27], [0.93, 0.70, 0.52], [0.39, 0.78, 0.30]] {
            let (b, e) = (hsb(c), hsb(shade(c, 1.0, &base)));
            let len = (e[1] - b[1]).hypot(e[2] - b[2]);
            let (p, l) = (hsb(shade(c, 0.5, &base)), hsb(shade(c, 0.5, &literal)));
            shares.push((l[1] - p[1]) / len);
            assert!(l[1] > p[1], "{c:?}: further right {p:?} → {l:?}");
            let st = hsb(shade(c, 0.5, &stretched));
            assert!((st[1] - p[1]).abs() > 1e-3, "{c:?}: the stretched curve moves too");
        }
        let (lo, hi) = shares.iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
        assert!(hi - lo < 0.06, "the same share for every colour: {shares:?}");
        // The straight line is the path itself, literal or not.
        let straight = Shading { literal_curve: true, ..base.clone() };
        assert_eq!(shade([0.89, 0.23, 0.27], 0.5, &straight), shade([0.89, 0.23, 0.27], 0.5, &base));
    }

    #[test]
    fn the_hue_curve_times_the_hue_turn() {
        let pt = |input: f32, output: f32| CurvePoint { input, output };
        assert!(hue_table(&[]).is_none() && hue_table(&[pt(0.0, 1.0), pt(0.5, 0.5), pt(1.0, 0.0)]).is_none(), "straight is no curve");
        let t = hue_table(&[pt(0.0, 1.0), pt(0.4, -0.3), pt(1.0, 0.0)]).unwrap();
        assert!(t[0] == 0.0 && t[PATH_LEN - 1] == 1.0 && hue_at(&t, 0.4) > 1.2, "{}", hue_at(&t, 0.4));
        let base = Shading { terminator_amount: 0.0, hue_shift: 40.0, ..Default::default() };
        let with = |points: Vec<CurvePoint>| Shading { hue_points: points, ..base.clone() };
        let skin = [0.93, 0.70, 0.52];
        let hsb = |c: [f32; 3]| rgb_to_hsb(c);
        let (h0, h1) = (hsb(skin)[0], hsb(shade(skin, 1.0, &base))[0]);
        let turn = |c: [f32; 3]| hue_diff(h0, hsb(c)[0]);
        let full = hue_diff(h0, h1);
        let plain = shade(skin, 0.3, &base);
        // Early: most of the turn already a third of the way in; late: hardly any.
        let early = shade(skin, 0.3, &with(vec![pt(0.0, 1.0), pt(0.2, 0.1), pt(1.0, 0.0)]));
        let late = shade(skin, 0.3, &with(vec![pt(0.0, 1.0), pt(0.6, 0.97), pt(1.0, 0.0)]));
        let along = |c: [f32; 3]| turn(c) * full.signum();
        assert!(along(early) > along(plain) + 0.2 * full.abs() && along(late) < along(plain), "{} {} {} of {full}", turn(early), turn(plain), turn(late));
        // Past the box: further than full shadow's turn.
        let past = shade(skin, 0.5, &with(vec![pt(0.0, 1.0), pt(0.5, -0.5), pt(1.0, 0.0)]));
        assert!(turn(past).abs() > full.abs() * 1.2, "{} vs {full}", turn(past));
        // Saturation and brightness stay the path's; full shadow stays put.
        let (a, b) = (hsb(plain), hsb(early));
        assert!((a[1] - b[1]).abs() < 1e-4 && (a[2] - b[2]).abs() < 1e-4, "{a:?} {b:?}");
        assert_eq!(shade(skin, 1.0, &with(vec![pt(0.0, 1.0), pt(0.2, 0.1), pt(1.0, 0.0)])), shade(skin, 1.0, &base));
    }

    #[test]
    fn the_advanced_curve_bends_the_path_in_the_picker() {
        let pt = |input: f32, output: f32| CurvePoint { input, output };
        let base = Shading { terminator_amount: 0.0, ..Default::default() };
        let with = |points: Vec<CurvePoint>, curve: f32| Shading { saturation_points: points, saturation_curve: curve, ..base.clone() };
        let hsb = |c: [f32; 3]| rgb_to_hsb(c);
        let red = [0.89, 0.23, 0.27];
        let plain = hsb(shade(red, 0.5, &base));
        // The user's drawing: saturation first, brightness held, then down late.
        let held = hsb(shade(red, 0.5, &with(vec![pt(0.0, 1.0), pt(0.5, 0.98), pt(0.85, 0.75), pt(1.0, 0.0)], 0.0)));
        assert!(held[2] > plain[2] + 0.05, "{held:?} {plain:?}");
        // The slider's bulge: out toward the vivid edge, or toward grey, the brightness near the path's.
        let (up, down) = (hsb(shade(red, 0.5, &with(vec![], 1.0))), hsb(shade(red, 0.5, &with(vec![], -1.0))));
        assert!(up[1] > plain[1] + 0.05 && down[1] < plain[1] - 0.2, "{up:?} {down:?} {plain:?}");
        // A drawn curve locks the slider out.
        let drawn = vec![pt(0.0, 1.0), pt(0.2, 0.3), pt(1.0, 0.0)];
        assert_eq!(shade(red, 0.5, &with(drawn.clone(), 1.0)), shade(red, 0.5, &with(drawn.clone(), 0.0)));
        // No shadow and full shadow stay put.
        let full = (shade(red, 1.0, &base), shade(red, 1.0, &with(drawn, 0.0)));
        assert!((0..3).all(|k| (full.0[k] - full.1[k]).abs() < 1e-4), "{full:?}");
        // Greys stay grey.
        let grey = hsb(shade([0.6; 3], 0.5, &Shading { ambient_amount: 0.0, ..with(vec![], 1.0) }));
        assert!(grey[1] < 0.01, "{grey:?}");
    }
}
