//! Camera Raw's tone model as Photoshop applies it, measured as a black box on Photoshop 27.11's
//! Camera Raw Filter (a chart of grey ramps, hue ramps and colour patches went through the filter by
//! script, see the dev log of 2026-10-25; no Adobe code or data):
//!
//! - It works on linear ProPhoto RGB. The tone controls (Exposure, Contrast, Whites, Blacks, the
//!   parametric and point curves) each make a curve of the grey level (sRGB-encoded), which is
//!   applied hue-preservingly: the curve maps a pixel's largest and smallest channel and the
//!   middle one keeps its place between them (as DNG's hue-preserving RGB tone curve does). On the
//!   chart this reproduces Photoshop within about one level on average for every control; plain
//!   per-channel curves in sRGB were 4–14 levels off.
//! - The red, green and blue point curves act on each ProPhoto channel separately.
//! - Exposure's curves (a highlight shoulder: +1 maps 240 to 251, never clips) and the regions of
//!   the parametric curve are tables of the measured curves ([`super::acr_tables`]).
//! - Contrast is one S-curve about a pivot that follows the image: on the gamma-warped axis that
//!   puts the pivot at the middle, the measured curves of six differently bright charts coincide.
//!   The pivot in linear light is about 0.511 · G^0.34, G the geometric mean of the luminance
//!   (fitted on the six charts within a few levels).

use super::acr_tables::{CONTRAST, EXPOSURE, PARAMETRIC_DARKS, PARAMETRIC_HIGHLIGHTS, PARAMETRIC_LIGHTS, PARAMETRIC_SHADOWS};
use super::acr_grade_tables::{
    GRADE_GLOBAL, GRADE_HIGHLIGHTS, GRADE_HIGHLIGHTS_BALANCE, GRADE_LUM_GLOBAL, GRADE_LUM_HIGHLIGHTS, GRADE_LUM_MIDTONES, GRADE_LUM_SHADOWS, GRADE_MIDTONES, GRADE_MIDTONES_BALANCE,
    GRADE_SHADOWS, GRADE_SHADOWS_BALANCE, VIGNETTE,
};
use super::{CameraRaw, Encoding, Wheel, luma};
use crate::photo_util::par_rows;

type M3 = [[f32; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
}

fn apply(m: &M3, v: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

fn inverse(m: &M3) -> Option<M3> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let c = |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    Some([
        [c(1, 2, 1, 2) / det, -c(0, 2, 1, 2) / det, c(0, 1, 1, 2) / det],
        [-c(1, 2, 0, 2) / det, c(0, 2, 0, 2) / det, -c(0, 1, 0, 2) / det],
        [c(1, 2, 0, 1) / det, -c(0, 2, 0, 1) / det, c(0, 1, 0, 1) / det],
    ])
}

const SRGB_XYZ: M3 = [[0.412_456_4, 0.357_576_1, 0.180_437_5], [0.212_672_9, 0.715_152_2, 0.072_175], [0.019_333_9, 0.119_192, 0.950_304_1]];
const ADOBE_XYZ: M3 = [[0.576_730_9, 0.185_554, 0.188_185_2], [0.297_376_9, 0.627_349_1, 0.075_274_1], [0.027_034_3, 0.070_687_2, 0.991_108_5]];
const PROPHOTO_XYZ: M3 = [[0.797_674_9, 0.135_191_7, 0.031_353_4], [0.288_040_2, 0.711_874_1, 0.000_085_7], [0.0, 0.0, 0.825_21]];
/// Bradford adaptation from D65 to D50.
const D65_TO_D50: M3 = [[1.047_811_2, 0.022_886_6, -0.050_127], [0.029_542_4, 0.990_484_4, -0.017_049_1], [-0.009_234_5, 0.015_043_6, 0.752_131_6]];

/// The document's linear RGB to linear ProPhoto and back, by its transfer curve: ProPhoto's 1.8
/// is ProPhoto itself, Adobe RGB's 2.2 Adobe RGB, anything else sRGB.
#[derive(Clone, Copy, Debug)]
pub(super) struct Space {
    to: M3,
    from: M3,
}

impl Space {
    pub(super) fn of(enc: Encoding) -> Self {
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let primaries = if (enc.0 - 1.8).abs() < 0.05 {
            None
        } else if (enc.0 - 2.2).abs() < 0.05 {
            Some(ADOBE_XYZ)
        } else {
            Some(SRGB_XYZ)
        };
        let Some(p) = primaries else { return Space { to: identity, from: identity } };
        let to = inverse(&PROPHOTO_XYZ).map(|pi| mul(&pi, &mul(&D65_TO_D50, &p))).unwrap_or(identity);
        let from = inverse(&to).unwrap_or(identity);
        Space { to, from }
    }
}

/// Table entries of a [`Tone`] curve over 0 … 1.
const STEPS: usize = 4096;

/// A curve of the grey level (sRGB-encoded, 0 … 1) as a table read with linear interpolation.
#[derive(Clone, Debug)]
pub(super) struct Tone {
    lut: Vec<f32>,
    /// The slope past 1 in linear light (float documents' values above white).
    past: f32,
}

impl Tone {
    pub(super) fn from_fn(f: impl Fn(f32) -> f32) -> Self {
        let mut lut: Vec<f32> = (0..=STEPS)
            .map(|i| {
                let x = i as f32 / STEPS as f32;
                let y = f(x);
                if y.is_finite() { y.clamp(0.0, 1.0) } else { x }
            })
            .collect();
        // Never decreasing.
        for i in 1..lut.len() {
            lut[i] = lut[i].max(lut[i - 1]);
        }
        Tone { lut, past: 1.0 }
    }
    pub(super) fn at(&self, x: f32) -> f32 {
        if x.is_nan() || x <= 0.0 {
            return self.lut[0] + x.min(0.0);
        }
        if x >= 1.0 {
            return self.lut[STEPS] + (x - 1.0);
        }
        let p = x * STEPS as f32;
        let i = (p as usize).min(STEPS - 1);
        self.lut[i] + (self.lut[i + 1] - self.lut[i]) * (p - i as f32)
    }
    /// This curve after `first`.
    pub(super) fn after(&self, first: &Tone) -> Tone {
        Tone { past: self.past * first.past, ..Tone::from_fn(|x| self.at(first.at(x))) }
    }
}

/// Reads row `k` of a table of 65-point curves at `x` (0 … 1).
fn sample65(c: &[f32; 65], x: f32) -> f32 {
    let p = x.clamp(0.0, 1.0) * 64.0;
    let i = (p as usize).min(63);
    c[i] + (c[i + 1] - c[i]) * (p - i as f32)
}

/// The curve of `table` (rows of (parameter, curve)) at parameter `v`, at `x`, interpolated
/// between the two rows around `v` (clamped to the table's ends).
fn table_at(table: &[(f32, [f32; 65])], v: f32, x: f32) -> f32 {
    let v = if v.is_finite() { v } else { 0.0 };
    let first = table.first().map_or(0.0, |r| r.0);
    let last = table.last().map_or(0.0, |r| r.0);
    let v = v.clamp(first, last);
    let i = table.windows(2).position(|w| v <= w[1].0).unwrap_or(0);
    let (Some(a), Some(b)) = (table.get(i), table.get(i + 1)) else { return x };
    let t = if b.0 > a.0 { (v - a.0) / (b.0 - a.0) } else { 0.0 };
    sample65(&a.1, x) * (1.0 - t) + sample65(&b.1, x) * t
}

/// Exposure (stops) as Camera Raw's grey curve.
/// Past white (float documents) it is a plain gain.
pub(super) fn exposure(e: f32) -> Tone {
    let e = if e.is_finite() { e.clamp(-10.0, 10.0) } else { 0.0 };
    Tone { past: 2f32.powf(e), ..Tone::from_fn(|x| table_at(&EXPOSURE, e, x)) }
}

/// The geometric mean of the luminance of straight RGBA pixels (doc encoding `enc`), on at most
/// about 64K of them: what Contrast's pivot follows.
pub(super) fn contrast_pivot(px: &[[f32; 4]], enc: Encoding) -> f32 {
    let curves = enc.curves();
    let step = (px.len() / 65_536).max(1);
    let (mut sum, mut n) = (0.0f64, 0u32);
    for q in px.iter().step_by(step) {
        if q[3].is_nan() || q[3] <= 0.0 || q.iter().any(|v| !v.is_finite()) {
            continue;
        }
        let l = luma([0, 1, 2].map(|c| curves.decode(q[c]).clamp(0.0, 1.0)));
        sum += f64::from((l + 1e-3).ln());
        n += 1;
    }
    let g = if n == 0 { 0.18 } else { (sum / f64::from(n)).exp() as f32 };
    (0.511 * g.max(1e-4).powf(0.34)).clamp(0.02, 0.8)
}

/// Contrast (−100 … 100) about `pivot` (linear light) as Camera Raw's grey curve.
pub(super) fn contrast(amount: f32, pivot: f32) -> Tone {
    let srgb = Encoding(0.0);
    let p = if pivot.is_finite() { srgb.encode(pivot).clamp(0.05, 0.95) } else { 0.5 };
    let g = 0.5f32.ln() / p.ln();
    Tone::from_fn(|x| table_at(&CONTRAST, amount, x.max(0.0).powf(g)).max(0.0).powf(1.0 / g))
}

/// The parametric curve (its four regions and splits) as Camera Raw's grey curve. The measured
/// regions are at the default splits (25/50/75 %); other splits move them by warping the input
/// axis so the default splits fall on them.
pub(super) fn parametric(p: &CameraRaw) -> Tone {
    let mut s = p.curve_splits.map(|v| if v.is_finite() { v.clamp(1.0, 99.0) / 100.0 } else { 0.5 });
    s[1] = s[1].max(s[0] + 0.01);
    s[2] = s[2].max(s[1] + 0.01).min(0.99);
    let knots = [(0.0, 0.0), (s[0], 0.25), (s[1], 0.5), (s[2], 0.75), (1.0, 1.0)];
    let warp = |x: f32| {
        let i = knots.windows(2).position(|w| x <= w[1].0).unwrap_or(3);
        let (a, b) = (knots[i], knots[i + 1]);
        a.1 + (b.1 - a.1) * ((x - a.0) / (b.0 - a.0).max(1e-6)).clamp(0.0, 1.0)
    };
    Tone::from_fn(|x| {
        let u = warp(x);
        x + table_at(&PARAMETRIC_SHADOWS, p.curve_shadows, u)
            + table_at(&PARAMETRIC_DARKS, p.curve_darks, u)
            + table_at(&PARAMETRIC_LIGHTS, p.curve_lights, u)
            + table_at(&PARAMETRIC_HIGHLIGHTS, p.curve_highlights, u)
    })
}

/// A point curve (`[input, output]` in 0..=255) as a grey curve.
pub(super) fn point(points: &[[f32; 2]]) -> Tone {
    let lut = super::curve_lut(points, STEPS + 1);
    Tone { lut, past: 1.0 }
}

/// `tone` on linear light, `decode(tone(encode(v)))` (sRGB curve), as a table indexed by the
/// square root of the light so the dark end gets its share of entries; a slope of 1 below 0 and
/// the tone's slope past 1.
struct Light {
    lut: Vec<f32>,
    past: f32,
}

impl Light {
    fn of(tone: &Tone) -> Self {
        let srgb = Encoding(0.0).curves();
        let lut = (0..=STEPS)
            .map(|i| {
                let u = i as f32 / STEPS as f32;
                srgb.decode(tone.at(srgb.encode(u * u)))
            })
            .collect();
        Light { lut, past: tone.past }
    }
    #[inline]
    fn at(&self, v: f32) -> f32 {
        if v.is_nan() || v <= 0.0 {
            return self.lut[0] + v.min(0.0);
        }
        if v >= 1.0 {
            return self.lut[STEPS] + (v - 1.0) * self.past;
        }
        let p = v.sqrt() * STEPS as f32;
        let i = (p as usize).min(STEPS - 1);
        self.lut[i] + (self.lut[i + 1] - self.lut[i]) * (p - i as f32)
    }
}

/// Applies `tone` hue-preservingly in linear ProPhoto to straight RGBA pixels encoded with `enc`.
pub(super) fn hue_preserving(px: &mut [[f32; 4]], w: usize, tone: &Tone, enc: Encoding) {
    let curves = enc.curves();
    let space = Space::of(enc);
    let light = Light::of(tone);
    let f = |v: f32| light.at(v);
    par_rows(px, w, 1, |_, row| {
        for q in row.iter_mut() {
            let lin = [0, 1, 2].map(|c| curves.decode(q[c]));
            let pro = apply(&space.to, lin);
            let (mx, mn) = (pro[0].max(pro[1]).max(pro[2]), pro[0].min(pro[1]).min(pro[2]));
            let (fmx, fmn) = (f(mx), f(mn));
            let out: [f32; 3] = if mx - mn > 1e-9 { pro.map(|v| fmn + (fmx - fmn) * (v - mn) / (mx - mn)) } else { [fmx; 3] };
            let back = apply(&space.from, out);
            for c in 0..3 {
                q[c] = curves.encode(back[c]);
            }
        }
    });
}

/// Vibrance's strength by hue (of the encoded ProPhoto pixel, 12 bins centred at 15°, 45°, …),
/// fitted on Photoshop's +50: weakest on reds and oranges (skin), strongest on yellows to cyans.
const VIBRANCE_HUE: [f32; 12] = [0.12, 0.41, 0.57, 0.63, 0.65, 0.61, 0.58, 0.66, 0.57, 0.56, 0.40, 0.20];

fn vibrance_by_hue(c: [f32; 3]) -> f32 {
    let h = (3f32.sqrt() * (c[1] - c[2])).atan2(2.0 * c[0] - c[1] - c[2]).to_degrees().rem_euclid(360.0);
    let x = ((h - 15.0) / 30.0).rem_euclid(12.0);
    let i = (x as usize).min(11);
    let t = x - i as f32;
    VIBRANCE_HUE[i] * (1.0 - t) + VIBRANCE_HUE[(i + 1) % 12] * t
}

/// Vibrance and then Saturation (−100 … 100) as measured on Photoshop: Saturation scales a pixel's
/// distance from its luminance in linear ProPhoto (within 2–3 levels on the chart). Raising
/// Vibrance scales the distance below its brightest channel, in linear ProPhoto, by an amount that
/// falls with the saturation and depends on the hue (2.8 levels); lowering it pulls the encoded
/// ProPhoto channels toward their mean, about evenly (6.6 levels; Photoshop's is uneven in a way
/// this doesn't follow).
pub(super) fn vibrance_saturation(px: &mut [[f32; 4]], w: usize, vibrance: f32, saturation: f32, enc: Encoding) {
    let v = if vibrance.is_finite() { vibrance.clamp(-100.0, 100.0) / 100.0 } else { 0.0 };
    let s = if saturation.is_finite() { saturation.clamp(-100.0, 100.0) / 100.0 } else { 0.0 };
    let curves = enc.curves();
    let srgb = Encoding(0.0).curves();
    let space = Space::of(enc);
    let y = PROPHOTO_XYZ[1];
    par_rows(px, w, 1, |_, row| {
        for q in row.iter_mut() {
            let lin = [0, 1, 2].map(|c| curves.decode(q[c]));
            let mut pro = apply(&space.to, lin);
            if v != 0.0 {
                let e = pro.map(|c| srgb.encode(c.clamp(0.0, 1.0)));
                let (mx, mn) = (e[0].max(e[1]).max(e[2]), e[0].min(e[1]).min(e[2]));
                if mx > 1e-6 && mx - mn > 1e-6 {
                    if v > 0.0 {
                        let k = 1.0 + v * vibrance_by_hue(e) * (1.0 - (mx - mn) / mx);
                        let top = pro[0].max(pro[1]).max(pro[2]);
                        pro = pro.map(|c| (top - (top - c) * k).max(0.0));
                    } else {
                        let k = 1.0 + v * 0.92;
                        let mean = (e[0] + e[1] + e[2]) / 3.0;
                        // Only the in-range part moves; the excess (float documents) stays.
                        pro = std::array::from_fn(|c| srgb.decode(mean + (e[c] - mean) * k) + (pro[c] - srgb.decode(e[c])));
                    }
                }
            }
            if s != 0.0 {
                let l = y[0] * pro[0] + y[1] * pro[1] + y[2] * pro[2];
                pro = pro.map(|c| (l + (c - l) * (1.0 + s)).max(0.0));
            }
            let back = apply(&space.from, pro);
            for c in 0..3 {
                q[c] = curves.encode(back[c]);
            }
        }
    });
}

/// A grading wheel's mask (log gain per unit of saturation by grey level) at `blending` and
/// `balance`: the Blending row, moved by how the Balance rows (measured at Blending 50) differ
/// from Balance 0.
fn grade_mask(by_blending: &[[f32; 65]; 5], by_balance: &[[f32; 65]; 5], blending: f32, balance: f32) -> [f32; 65] {
    let rows = |t: &[[f32; 65]; 5], x: f32| -> [f32; 65] {
        let x = if x.is_finite() { x.clamp(0.0, 4.0) } else { 2.0 };
        let i = (x as usize).min(3);
        let f = x - i as f32;
        std::array::from_fn(|k| t[i][k] * (1.0 - f) + t[i + 1][k] * f)
    };
    let bl = rows(by_blending, blending / 25.0);
    let ba = rows(by_balance, balance / 50.0 + 2.0);
    std::array::from_fn(|k| (bl[k] + ba[k] - by_balance[2][k]).max(0.0))
}

/// A hue's direction of log gains: 2 · hsv(hue, 1, 1) − 1, so red is (1, −1, −1) and orange
/// (1, 0, −1).
fn grade_direction(hue: f32) -> [f32; 3] {
    let h = if hue.is_finite() { hue.rem_euclid(360.0) / 60.0 } else { 0.0 };
    let f = h.fract();
    let c = match h as u32 {
        0 => [1.0, f, 0.0],
        1 => [1.0 - f, 1.0, 0.0],
        2 => [0.0, 1.0, f],
        3 => [0.0, 1.0 - f, 1.0],
        4 => [f, 0.0, 1.0],
        _ => [1.0, 0.0, 1.0 - f],
    };
    c.map(|v| 2.0 * v - 1.0)
}

/// Colour Grading as measured on Photoshop. The Luminance sliders are grey curves (their measured
/// tables), applied hue-preservingly. Each wheel's colour multiplies the pixel's linear ProPhoto
/// channels by exp(sat · mask(level) · direction(hue)), the level being the encoded ProPhoto
/// luminance before grading; Shadows and Highlights keep the luminance. Like lift and gain, the
/// Shadows colour adds its change (as it would to a grey of the pixel's luminance) and Highlights
/// multiplies; Midtones and Global are between (fitted: half and 0.3 added). On the chart this
/// is within 2–5 levels of Photoshop.
pub(super) fn grade(px: &mut [[f32; 4]], w: usize, p: &CameraRaw, enc: Encoding) {
    let wheels = [p.grade_shadows, p.grade_midtones, p.grade_highlights, p.grade_global];
    let lum = |t: &[(f32, [f32; 65])], wh: &Wheel| (wh.lum != 0.0).then(|| Tone::from_fn(|x| table_at(t, wh.lum, x)));
    let curves = [lum(&GRADE_LUM_SHADOWS, &wheels[0]), lum(&GRADE_LUM_MIDTONES, &wheels[1]), lum(&GRADE_LUM_HIGHLIGHTS, &wheels[2]), lum(&GRADE_LUM_GLOBAL, &wheels[3])];
    let mut tone: Option<Tone> = None;
    for c in curves.into_iter().flatten() {
        tone = Some(match tone {
            Some(t) => c.after(&t),
            None => c,
        });
    }
    let masks = [
        grade_mask(&GRADE_SHADOWS, &GRADE_SHADOWS_BALANCE, p.grade_blending, p.grade_balance),
        grade_mask(&GRADE_MIDTONES, &GRADE_MIDTONES_BALANCE, p.grade_blending, p.grade_balance),
        grade_mask(&GRADE_HIGHLIGHTS, &GRADE_HIGHLIGHTS_BALANCE, p.grade_blending, p.grade_balance),
        GRADE_GLOBAL,
    ];
    /// A coloured wheel: its strength, direction, mask, the share of the change added and whether
    /// it keeps the luminance.
    struct Colour<'a> {
        strength: f32,
        dir: [f32; 3],
        mask: &'a [f32; 65],
        add: f32,
        keep: bool,
    }
    let added = [1.0, 0.5, 0.0, 0.3];
    let colours: Vec<Colour> = (0..4)
        .filter(|&k| wheels[k].sat.is_finite() && wheels[k].sat > 0.0)
        .map(|k| Colour { strength: wheels[k].sat.min(100.0) / 100.0, dir: grade_direction(wheels[k].hue), mask: &masks[k], add: added[k], keep: k == 0 || k == 2 })
        .collect();
    let curves_enc = enc.curves();
    let srgb = Encoding(0.0).curves();
    let space = Space::of(enc);
    let y = PROPHOTO_XYZ[1];
    let light = tone.as_ref().map(Light::of);
    par_rows(px, w, 1, |_, row| {
        for q in row.iter_mut() {
            let lin = [0, 1, 2].map(|c| curves_enc.decode(q[c]));
            let mut pro = apply(&space.to, lin);
            let level = srgb.encode((y[0] * pro[0] + y[1] * pro[1] + y[2] * pro[2]).clamp(0.0, 1.0));
            if let Some(f) = &light {
                let (mx, mn) = (pro[0].max(pro[1]).max(pro[2]), pro[0].min(pro[1]).min(pro[2]));
                let (fmx, fmn) = (f.at(mx), f.at(mn));
                pro = if mx - mn > 1e-9 { pro.map(|v| fmn + (fmx - fmn) * (v - mn) / (mx - mn)) } else { [fmx; 3] };
            }
            for Colour { strength, dir, mask, add, keep } in &colours {
                let a = strength * sample65(mask, level);
                let mut g = dir.map(|d| (a * d).exp());
                if *keep {
                    let n = y[0] * g[0] + y[1] * g[1] + y[2] * g[2];
                    if n > 1e-6 {
                        g = g.map(|v| v / n);
                    }
                }
                let base = (y[0] * pro[0] + y[1] * pro[1] + y[2] * pro[2]).max(0.0);
                pro = std::array::from_fn(|c| (pro[c] * g[c] * (1.0 - add) + (pro[c] + base * (g[c] - 1.0)) * add).max(0.0));
            }
            let back = apply(&space.from, pro);
            for c in 0..3 {
                q[c] = curves_enc.encode(back[c]);
            }
        }
    });
}

/// Reads a piecewise-linear table of (x, y) at `x` (clamped to its ends).
fn piecewise(t: &[(f32, f32)], x: f32) -> f32 {
    let x = if x.is_finite() { x } else { 0.0 };
    let (Some(first), Some(last)) = (t.first(), t.last()) else { return 0.0 };
    if x <= first.0 {
        return first.1;
    }
    if x >= last.0 {
        return last.1;
    }
    let i = t.windows(2).position(|w| x <= w[1].0).unwrap_or(0);
    let (a, b) = (t[i], t[i + 1]);
    a.1 + (b.1 - a.1) * (x - a.0) / (b.0 - a.0).max(1e-6)
}

/// The post-crop vignette's mask (0 … 1) over an image, as measured on Photoshop: a logistic in
/// log ρ², ρ the distance from the centre on an ellipse through the edge midpoints (Roundness 0),
/// moved toward a circle through the corners as Roundness rises (below 0, toward a rounded
/// rectangle, not measured). Midpoint sets the centre of the logistic, Feather (and a low
/// Midpoint) its width. Within 1–3 % of Photoshop's masks at the measured settings.
pub(super) struct VignetteMask {
    ax: f32,
    ay: f32,
    circle: f32,
    round: f32,
    power: f32,
    centre: f32,
    width: f32,
}

impl VignetteMask {
    pub(super) fn new(p: &CameraRaw, full_w: usize, full_h: usize) -> Self {
        let fin = |v: f32, d: f32| if v.is_finite() { v } else { d };
        let mid = fin(p.vignette_midpoint, 50.0).clamp(0.0, 100.0);
        let feather = fin(p.vignette_feather, 50.0).clamp(0.0, 100.0);
        let round = fin(p.vignette_roundness, 0.0).clamp(-100.0, 100.0) / 100.0;
        let (ax, ay) = (full_w.max(1) as f32 / 2.0, full_h.max(1) as f32 / 2.0);
        let wide = ((feather - 50.0) / 50.0).max(0.0);
        let centre = piecewise(&[(0.0, -1.07), (25.0, -0.52), (50.0, -0.05), (75.0, 0.40), (100.0, 1.30)], mid) + wide * piecewise(&[(0.0, -1.03), (30.0, 0.0)], mid);
        let width = piecewise(&[(0.0, 0.01), (25.0, 0.17), (50.0, 0.42), (75.0, 0.62), (100.0, 0.60)], feather)
            * piecewise(&[(0.0, 1.55), (25.0, 1.24), (50.0, 1.0), (75.0, 0.76), (100.0, 0.6)], mid)
            * (1.0 + wide * piecewise(&[(0.0, 1.1), (30.0, 0.2), (50.0, 0.0)], mid));
        VignetteMask {
            ax,
            ay,
            circle: ax.hypot(ay) / std::f32::consts::SQRT_2,
            round: round.max(0.0),
            power: 2.0 + (-round).max(0.0) * 6.0,
            centre,
            width: width.max(1e-3),
        }
    }
    /// The mask at pixel offset `(dx, dy)` from the image's centre.
    pub(super) fn at(&self, dx: f32, dy: f32) -> f32 {
        let (nx, ny) = ((dx / self.ax).abs(), (dy / self.ay).abs());
        let ellipse = if self.power == 2.0 { nx.hypot(ny) } else { (nx.powf(self.power) + ny.powf(self.power)).powf(1.0 / self.power) };
        let r = ellipse + (dx.hypot(dy) / self.circle - ellipse) * self.round;
        let l = (r * r).max(1e-6).log2();
        let m = 1.0 / (1.0 + (-(l - self.centre) / self.width).exp());
        if m.is_finite() { m } else { 0.0 }
    }
}

/// The Highlight Priority vignette's tone: its measured grey curves by amount (−100 … 100), each
/// on linear light, applied hue-preservingly in linear ProPhoto at a pixel's own amount (the
/// slider times the mask); within a level of Photoshop on the chart.
pub(super) struct VignetteTone {
    rows: Vec<(f32, Light)>,
    space: Space,
}

impl VignetteTone {
    pub(super) fn new(enc: Encoding) -> Self {
        let rows = VIGNETTE.iter().map(|(a, c)| (*a, Light::of(&Tone::from_fn(|x| sample65(c, x))))).collect();
        VignetteTone { rows, space: Space::of(enc) }
    }
    /// The curve at `amount` of linear light `v`.
    fn at(&self, amount: f32, v: f32) -> f32 {
        let i = self.rows.windows(2).position(|w| amount <= w[1].0).unwrap_or(self.rows.len().saturating_sub(2));
        let (Some(a), Some(b)) = (self.rows.get(i), self.rows.get(i + 1)) else { return v };
        let t = ((amount - a.0) / (b.0 - a.0).max(1e-6)).clamp(0.0, 1.0);
        a.1.at(v) * (1.0 - t) + b.1.at(v) * t
    }
    /// Applies the vignette at `amount` to linear RGB `lin` (the document's primaries).
    pub(super) fn apply(&self, lin: [f32; 3], amount: f32) -> [f32; 3] {
        let pro = apply(&self.space.to, lin);
        let (mx, mn) = (pro[0].max(pro[1]).max(pro[2]), pro[0].min(pro[1]).min(pro[2]));
        let (fmx, fmn) = (self.at(amount, mx), self.at(amount, mn));
        let out = if mx - mn > 1e-9 { pro.map(|v| fmn + (fmx - fmn) * (v - mn) / (mx - mn)) } else { [fmx; 3] };
        apply(&self.space.from, out)
    }
}

/// Applies `tones` (red, green, blue; `None` leaves a channel) to each linear ProPhoto channel.
pub(super) fn per_channel(px: &mut [[f32; 4]], w: usize, tones: &[Option<Tone>; 3], enc: Encoding) {
    let curves = enc.curves();
    let space = Space::of(enc);
    let lights = tones.each_ref().map(|t| t.as_ref().map(Light::of));
    par_rows(px, w, 1, |_, row| {
        for q in row.iter_mut() {
            let lin = [0, 1, 2].map(|c| curves.decode(q[c]));
            let mut pro = apply(&space.to, lin);
            for (v, l) in pro.iter_mut().zip(&lights) {
                if let Some(l) = l {
                    *v = l.at(*v);
                }
            }
            let back = apply(&space.from, pro);
            for c in 0..3 {
                q[c] = curves.encode(back[c]);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spaces_round_trip_and_keep_grey() {
        for gamma in [0.0, 1.8, 2.2] {
            let s = Space::of(Encoding::of(gamma));
            let v = [0.3, 0.6, 0.1];
            let back = apply(&s.from, apply(&s.to, v));
            assert!(v.iter().zip(back).all(|(a, b)| (a - b).abs() < 1e-5), "{gamma}");
            let g = apply(&s.to, [0.5; 3]);
            assert!(g.iter().all(|c| (c - 0.5).abs() < 2e-3), "{gamma}: {g:?}");
        }
    }

    #[test]
    fn measured_curves_are_photoshops() {
        // Exposure +1: Photoshop maps grey 128 to 181 and 240 to 251.
        let e = exposure(1.0);
        assert!((e.at(128.0 / 255.0) * 255.0 - 181.0).abs() < 1.5);
        assert!((e.at(240.0 / 255.0) * 255.0 - 251.0).abs() < 1.5);
        assert!((exposure(0.0).at(0.4) - 0.4).abs() < 1e-3);
        // Contrast on the chart (pivot ~148): neutral at the pivot, an S around it.
        let piv = Encoding(0.0).decode(148.5 / 255.0);
        let c = contrast(100.0, piv);
        assert!((c.at(148.5 / 255.0) - 148.5 / 255.0).abs() < 0.01);
        assert!(c.at(0.25) < 0.2 && c.at(0.8) > 0.85);
        assert!((contrast(0.0, piv).at(0.3) - 0.3).abs() < 1e-3);
        // The parametric curve at neutral is the identity; Darks +100 lifts the darks a lot.
        let flat = parametric(&CameraRaw::default());
        assert!((flat.at(0.37) - 0.37).abs() < 1e-3);
        let d = parametric(&CameraRaw { curve_darks: 100.0, ..Default::default() });
        assert!(d.at(96.0 / 255.0) * 255.0 > 140.0);
        // Odd values stay finite.
        for t in [exposure(f32::NAN), contrast(f32::INFINITY, f32::NAN), parametric(&CameraRaw { curve_splits: [f32::NAN, 0.0, 200.0], ..Default::default() })] {
            assert!(t.lut.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn hue_preserving_curves_keep_hue_and_grey() {
        let mut px = vec![[0.8, 0.4, 0.2, 1.0], [0.5, 0.5, 0.5, 1.0]];
        let before = px.clone();
        hue_preserving(&mut px, 2, &exposure(1.0), Encoding(0.0));
        assert!((px[1][0] - px[1][1]).abs() < 1e-3 && (px[1][1] - px[1][2]).abs() < 1e-3, "grey stays grey");
        assert!(px[0][0] > before[0][0] && px[0][0] > px[0][1] && px[0][1] > px[0][2], "order kept: {:?}", px[0]);
        let mut same = before.clone();
        hue_preserving(&mut same, 2, &Tone::from_fn(|x| x), Encoding(0.0));
        assert!(same.iter().flatten().zip(before.iter().flatten()).all(|(a, b)| (a - b).abs() < 1e-3));
        let mut grey = vec![[0.5f32, 0.5, 0.5, 1.0]];
        per_channel(&mut grey, 1, &[Some(point(&[[0.0, 0.0], [128.0, 160.0], [255.0, 255.0]])), None, None], Encoding(0.0));
        assert!(grey[0][0] > 0.6 && grey[0][1] < 0.5, "a red curve in ProPhoto also lowers sRGB green: {:?}", grey[0]);
    }

    #[test]
    fn vibrance_and_saturation_spare_grey_and_follow_their_sign() {
        let spread = |q: &[f32; 4]| q[0].max(q[1]).max(q[2]) - q[0].min(q[1]).min(q[2]);
        for (v, s) in [(50.0, 0.0), (-50.0, 0.0), (0.0, 50.0), (0.0, -50.0), (f32::NAN, f32::INFINITY)] {
            let mut px = vec![[0.7, 0.5, 0.3, 1.0], [0.4, 0.4, 0.4, 1.0], [0.2, 0.6, 0.5, 1.0]];
            let before = px.clone();
            vibrance_saturation(&mut px, 3, v, s, Encoding(0.0));
            assert!(px.iter().flatten().all(|c| c.is_finite()));
            assert!((px[1][0] - 0.4).abs() < 1e-3 && (px[1][2] - 0.4).abs() < 1e-3, "grey stays: {:?}", px[1]);
            let more = v + s;
            if more.is_finite() {
                assert_eq!(spread(&px[0]) > spread(&before[0]), more > 0.0, "{v} {s}: {:?}", px[0]);
            }
        }
        // Vibrance spares orange (skin) more than cyan-green.
        let mut px = vec![[0.8, 0.55, 0.4, 1.0], [0.4, 0.75, 0.7, 1.0]];
        let before = px.clone();
        vibrance_saturation(&mut px, 2, 100.0, 0.0, Encoding(0.0));
        assert!(spread(&px[0]) / spread(&before[0]) < spread(&px[1]) / spread(&before[1]));
    }

    #[test]
    fn grading_tints_its_tones_like_photoshop() {
        let wheel = |hue: f32, sat: f32, lum: f32| Wheel { hue, sat, lum };
        let ramp = || (0..=8).map(|i| [i as f32 / 8.0, i as f32 / 8.0, i as f32 / 8.0, 1.0]).collect::<Vec<_>>();
        // Shadows blue (220°, 50) at Blending 100: Photoshop turns grey 64 into (28, 69, 88).
        let mut px = vec![[64.0 / 255.0, 64.0 / 255.0, 64.0 / 255.0, 1.0]];
        grade(&mut px, 1, &CameraRaw { grade_shadows: wheel(220.0, 50.0, 0.0), grade_blending: 100.0, ..Default::default() }, Encoding(0.0));
        let got = px[0].map(|v| v * 255.0);
        assert!((got[0] - 28.0).abs() < 6.0 && (got[1] - 69.0).abs() < 6.0 && (got[2] - 88.0).abs() < 7.0, "{got:?}");
        // Highlights leave black, Shadows white.
        let mut px = ramp();
        grade(&mut px, 9, &CameraRaw { grade_highlights: wheel(40.0, 80.0, 0.0), grade_shadows: wheel(200.0, 80.0, 0.0), ..Default::default() }, Encoding(0.0));
        assert!(px[0][..3].iter().all(|v| v.abs() < 0.01) && px[8][..3].iter().all(|v| (v - 1.0).abs() < 0.02), "{px:?}");
        assert!(px[2][2] > px[2][0] && px[6][0] > px[6][2], "cool shadows, warm highlights: {px:?}");
        // Shadows luminance +50 lifts black to about 14.
        let mut px = ramp();
        grade(&mut px, 9, &CameraRaw { grade_shadows: wheel(0.0, 0.0, 50.0), ..Default::default() }, Encoding(0.0));
        assert!((px[0][0] * 255.0 - 14.0).abs() < 2.0, "{:?}", px[0]);
        // Odd values stay finite.
        let mut px = ramp();
        let odd = Wheel { hue: f32::NAN, sat: f32::INFINITY, lum: f32::NAN };
        grade(&mut px, 9, &CameraRaw { grade_global: odd, grade_blending: f32::NAN, grade_balance: f32::INFINITY, ..Default::default() }, Encoding(0.0));
        assert!(px.iter().flatten().all(|v| v.is_finite()));
    }

    #[test]
    fn the_vignette_follows_photoshops_mask_and_tone() {
        // Defaults (Midpoint 50, Feather 50, Roundness 0): Photoshop's mask is 0 at the centre,
        // about 0.42 at the middle of a long edge and 0.96 in the corner.
        let m = VignetteMask::new(&CameraRaw::default(), 512, 320);
        assert!(m.at(0.0, 0.0) < 0.01);
        assert!((m.at(250.0, 0.0) - 0.42).abs() < 0.08, "{}", m.at(250.0, 0.0));
        assert!(m.at(255.0, 159.0) > 0.85);
        // Roundness +100 is a circle through the corners: the long edge darkens more, the short less.
        let round = VignetteMask::new(&CameraRaw { vignette_roundness: 100.0, ..Default::default() }, 512, 320);
        assert!(round.at(250.0, 0.0) > m.at(250.0, 0.0) && round.at(0.0, 155.0) < m.at(0.0, 155.0));
        // −50 at full strength takes grey 128 to about 45; 0 leaves it.
        let t = VignetteTone::new(Encoding(0.0));
        let srgb = Encoding(0.0);
        let g = srgb.decode(128.0 / 255.0);
        let out = t.apply([g; 3], -50.0).map(|v| srgb.encode(v) * 255.0);
        assert!((out[0] - 45.0).abs() < 3.0, "{out:?}");
        assert!((srgb.encode(t.apply([g; 3], 0.0)[1]) * 255.0 - 128.0).abs() < 0.5);
        let odd = VignetteMask::new(&CameraRaw { vignette_midpoint: f32::NAN, vignette_feather: f32::INFINITY, ..Default::default() }, 0, 0);
        assert!(odd.at(f32::NAN, 3.0).is_finite() && t.apply([f32::NAN, 0.2, 0.1], f32::NAN).iter().all(|v| !v.is_infinite()));
    }
}
