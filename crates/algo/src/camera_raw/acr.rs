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

use super::acr_vibrance_tables::{VIB_HUE, VIB_K, VIB_LEVELS, VIB_T};
use super::acr_hsl_lum_tables::{HSL_LUM_C, HSL_LUM_L};
use super::acr_hsl_tables::{HSL_SAT_GAIN, HSL_SHIFT, HSL_WEIGHTS};
use super::acr_tables::{CONTRAST, EXPOSURE, PARAMETRIC_DARKS, PARAMETRIC_HIGHLIGHTS, PARAMETRIC_LIGHTS, PARAMETRIC_SHADOWS};
use super::acr_grade_tables::{
    GRADE_GLOBAL, GRADE_HIGHLIGHTS, GRADE_HIGHLIGHTS_BALANCE, GRADE_LUM_GLOBAL, GRADE_LUM_HIGHLIGHTS, GRADE_LUM_MIDTONES, GRADE_LUM_SHADOWS, GRADE_MIDTONES, GRADE_MIDTONES_BALANCE,
    GRADE_SHADOWS, GRADE_SHADOWS_BALANCE, VIGNETTE,
};
use super::{CameraRaw, Curves, Encoding, Wheel, luma, rgb_to_hsv};
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

/// White balance as measured on Photoshop (black-box, on a rendered image): ln of the gain on each
/// linear ProPhoto channel at Temperature and at Tint −100, −50, −25, −10, 10, 25, 50, 100.
/// Temperature −100 is extrapolated (Photoshop's result there leaves sRGB).
#[allow(clippy::approx_constant)] // measured levels; some lie near mathematical constants
const WB_TEMPERATURE: [(f32, [f32; 3]); 8] = [
    (-100.0, [0.4343, 0.7433, 2.9871]),
    (-50.0, [0.1646, 0.3402, 1.4377]),
    (-25.0, [0.0298, 0.1386, 0.6630]),
    (-10.0, [0.0050, 0.0465, 0.2486]),
    (10.0, [0.2313, 0.1920, -0.0001]),
    (25.0, [0.5529, 0.4625, 0.0033]),
    (50.0, [0.9965, 0.8399, 0.0096]),
    (100.0, [1.5431, 1.3240, 0.0222]),
];
#[allow(clippy::approx_constant)] // measured levels; some lie near mathematical constants
const WB_TINT: [(f32, [f32; 3]); 8] = [
    (-100.0, [0.3225, 1.1378, 0.0310]),
    (-50.0, [0.1748, 0.4335, 0.0045]),
    (-25.0, [0.0762, 0.1808, 0.0012]),
    (-10.0, [0.0268, 0.0634, -0.0001]),
    (10.0, [0.0350, -0.0001, 0.0651]),
    (25.0, [0.0997, 0.0001, 0.1895]),
    (50.0, [0.2433, -0.0005, 0.4790]),
    (100.0, [0.7072, -0.0373, 1.5145]),
];

/// ln of the white-balance gains (see [`WB_TEMPERATURE`]) before they are scaled so the smallest
/// is 1: the two sliders' logs add.
pub(super) fn white_balance_log(temperature: f32, tint: f32) -> [f32; 3] {
    let at = |table: &[(f32, [f32; 3]); 8], v: f32| -> [f32; 3] {
        let v = if v.is_finite() { v.clamp(-100.0, 100.0) } else { 0.0 };
        let mut prev = (0.0f32, [0.0f32; 3]);
        // The knots in order, with 0 (no change) between −10 and 10.
        let knots = table.iter().take(4).copied().chain(std::iter::once((0.0, [0.0; 3]))).chain(table.iter().skip(4).copied());
        for (i, (x, y)) in knots.enumerate() {
            if v <= x {
                if i == 0 {
                    return y;
                }
                let t = (v - prev.0) / (x - prev.0).max(1e-6);
                return std::array::from_fn(|c| prev.1[c] + (y[c] - prev.1[c]) * t);
            }
            prev = (x, y);
        }
        prev.1
    };
    let (a, b) = (at(&WB_TEMPERATURE, temperature), at(&WB_TINT, tint));
    std::array::from_fn(|c| a[c] + b[c])
}

/// White balance on the document's pixels as measured on Photoshop: a gain per linear ProPhoto
/// channel (the smallest 1, so nothing darkens), each channel rolled off toward white as Photoshop
/// does, `g·x / (1 + (g − 1)·x^1.164)^0.99` (a darker channel takes the full gain; within 0.006 of
/// Photoshop on its grey ramp).
pub(super) fn white_balance(px: &mut [[f32; 4]], w: usize, temperature: f32, tint: f32, enc: Encoding) {
    let l = white_balance_log(temperature, tint);
    let low = l[0].min(l[1]).min(l[2]);
    let g = l.map(|v| (v - low).exp());
    let curves = enc.curves();
    let space = Space::of(enc);
    par_rows(px, w, 1, |_, row| {
        for q in row.iter_mut() {
            let lin = [0, 1, 2].map(|c| curves.decode(q[c]));
            let pro = apply(&space.to, lin);
            let out: [f32; 3] = std::array::from_fn(|c| {
                let x = pro[c].max(0.0);
                g[c] * x / (1.0 + (g[c] - 1.0) * x.powf(1.164)).powf(0.99)
            });
            let back = apply(&space.from, out);
            for c in 0..3 {
                q[c] = curves.encode(back[c].max(0.0));
            }
        }
    });
}

/// The linear sRGB colour `lin` to linear ProPhoto.
pub(super) fn srgb_to_prophoto(lin: [f32; 3]) -> [f32; 3] {
    apply(&Space::of(Encoding(0.0)).to, lin)
}

/// Calibration as measured on Photoshop (fitted on the chart, black-box): in linear ProPhoto each
/// primary's Hue and Saturation move the other two channels' entries of that primary's column of a
/// matrix, taking them off those rows' diagonal so white stays white. Per primary (red, green,
/// blue), slider (hue, saturation) and level (−100, −50, 50, 100): the two entries, in channel order.
#[allow(clippy::approx_constant)] // measured levels; some lie near mathematical constants
const CALIBRATION: [[[[f32; 2]; 4]; 2]; 3] = [
    [[[-0.265, 0.332], [-0.134, 0.160], [0.158, -0.132], [0.327, -0.258]], [[0.375, 0.380], [0.182, 0.185], [-0.175, -0.169], [-0.366, -0.341]]],
    [[[0.342, -0.262], [0.169, -0.137], [-0.149, 0.164], [-0.290, 0.329]], [[0.422, 0.399], [0.215, 0.202], [-0.187, -0.158], [-0.360, -0.304]]],
    [[[-0.318, 0.312], [-0.162, 0.155], [0.172, -0.140], [0.343, -0.260]], [[0.422, 0.403], [0.214, 0.202], [-0.182, -0.166], [-0.331, -0.297]]],
];

/// The Calibration matrix for the primaries' `(hue, saturation)` sliders (−100 … 100), acting on
/// the document's linear RGB (see [`CALIBRATION`]); within 1.4–3.7 levels of Photoshop on the
/// chart per slider. `None` when every slider is at 0.
pub(super) fn calibration(primaries: [(f32, f32); 3], enc: Encoding) -> Option<M3> {
    let fin = |v: f32| if v.is_finite() { v.clamp(-100.0, 100.0) / 100.0 } else { 0.0 };
    let mut m = [[1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let mut any = false;
    for (k, (hue, sat)) in primaries.into_iter().enumerate() {
        for (kind, v) in [hue, sat].into_iter().map(fin).enumerate() {
            if v == 0.0 {
                continue;
            }
            any = true;
            let t = &CALIBRATION[k][kind];
            let (half, full) = if v < 0.0 { (t[1], t[0]) } else { (t[2], t[3]) };
            let a = v.abs();
            let e: [f32; 2] = if a <= 0.5 {
                half.map(|x| x * a / 0.5)
            } else {
                std::array::from_fn(|i| half[i] + (full[i] - half[i]) * (a - 0.5) / 0.5)
            };
            let others = [0, 1, 2].into_iter().filter(|r| *r != k);
            for (r, d) in others.zip(e) {
                m[r][k] += d;
                m[r][r] -= d;
            }
        }
    }
    if !any {
        return None;
    }
    let space = Space::of(enc);
    Some(mul(&space.from, &mul(&m, &space.to)))
}

/// One level's Vibrance table at (saturation, encoded brightest channel, hue): ln k and ln tr.
fn vibrance_at(level: usize, sv: f32, te: f32, hue: f32) -> (f32, f32) {
    let x = (sv * 10.0 - 0.5).clamp(0.0, 9.0);
    let y = (te * 8.0 - 0.5).clamp(0.0, 7.0);
    let (x0, y0) = ((x as usize).min(8), (y as usize).min(6));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let bilinear = |t: &[[f32; 8]; 10]| {
        t[x0][y0] * (1.0 - fx) * (1.0 - fy) + t[x0 + 1][y0] * fx * (1.0 - fy) + t[x0][y0 + 1] * (1.0 - fx) * fy + t[x0 + 1][y0 + 1] * fx * fy
    };
    let hx = (hue / 30.0 - 0.5).rem_euclid(12.0);
    let h0 = (hx as usize).min(11);
    let ht = hx - h0 as f32;
    let hf = VIB_HUE[level][h0] * (1.0 - ht) + VIB_HUE[level][(h0 + 1) % 12] * ht;
    (bilinear(&VIB_K[level]) * hf, bilinear(&VIB_T[level]) * hf)
}

/// Vibrance (−1 … 1) on a linear ProPhoto pixel, as measured on Photoshop: the pixel is scaled
/// uniformly toward a grey (exactly so on the chart), its distance below the brightest channel by
/// `k` and that channel by `tr`, both read from tables over the pixel's saturation and brightness
/// (and, raising, its hue: reds and skin move least). Lowering takes nearly all the colour from
/// pale colours and keeps much of it in strong ones, lightening them toward their brightest channel.
fn vibrance_of(pro: [f32; 3], v: f32, srgb: &Curves) -> [f32; 3] {
    let c = pro.map(|x| x.clamp(0.0, 1.0));
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    if mx <= 1e-6 || mx - mn <= 1e-6 {
        return pro;
    }
    let sv = (mx - mn) / mx;
    let te = srgb.encode(mx);
    let hue = rgb_to_hsv(c)[0];
    let a = v.abs() * 100.0;
    // The levels of this direction, weakest first: 25, 50, 75, 100.
    let side: [usize; 4] = if v < 0.0 { [3, 2, 1, 0] } else { [4, 5, 6, 7] };
    let (mut lk, mut lt) = (0.0, 0.0);
    let mut prev = (0.0f32, 0.0f32, 0.0f32);
    for level in side {
        let at = VIB_LEVELS[level].abs();
        let (k1, t1) = vibrance_at(level, sv, te, hue);
        if at >= a || level == side[3] {
            let f = ((a - prev.0) / (at - prev.0).max(1e-6)).clamp(0.0, 1.0);
            lk = prev.1 + (k1 - prev.1) * f;
            lt = prev.2 + (t1 - prev.2) * f;
            break;
        }
        prev = (at, k1, t1);
    }
    // The first column is centred at 5% saturation; toward grey the change fades out.
    let (k, tr) = (lk.exp(), (lt * (sv / 0.05).min(1.0)).exp());
    let top = pro[0].max(pro[1]).max(pro[2]);
    pro.map(|x| (top * tr - (top - x) * k).max(0.0))
}

/// Vibrance and then Saturation (−100 … 100) as measured on Photoshop: Saturation scales a pixel's
/// distance from its luminance in linear ProPhoto (within 1–3 levels on the chart); Vibrance is
/// [`vibrance_of`] (within 0.6–1.5 levels lowering, 0.5–3.2 raising; the old models were 4–14).
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
                pro = vibrance_of(pro, v, &srgb);
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

/// RGB with hue `hue` (degrees), lightness `l` and chroma `c` (HSL's construction).
fn from_hlc(hue: f32, l: f32, c: f32) -> [f32; 3] {
    let hp = hue.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r + m, g + m, b + m]
}

/// The eight bands' weights for a hue of linear ProPhoto (the measured table, interpolated).
fn band_weights(hue: f32) -> [f32; 8] {
    let x = if hue.is_finite() { hue.rem_euclid(360.0) / 5.0 } else { 0.0 };
    let k0 = (x as usize).min(71);
    let k1 = (k0 + 1) % 72;
    let t = x - k0 as f32;
    std::array::from_fn(|b| HSL_WEIGHTS[b][k0] * (1.0 - t) + HSL_WEIGHTS[b][k1] * t)
}

/// One band's Luminance at slider `v` (−1 … 1) for a pixel of lightness `l` and relative chroma
/// `s`: ln of the lightness ratio and of the chroma ratio (the measured tables, interpolated
/// between ±50 and ±100 and from 0).
fn hsl_lum_at(band: usize, v: f32, l: f32, s: f32) -> (f32, f32) {
    let x = (l * 12.0 - 0.5).clamp(0.0, 11.0);
    let x0 = (x as usize).min(10);
    let fx = x - x0 as f32;
    // Columns: s = 0, then the centres of ten bins.
    let s = s.clamp(0.0, 0.95);
    let (y0, fy) = if s < 0.05 { (0, s / 0.05) } else { let y = s * 10.0 + 0.5; ((y as usize).min(9), y - (y as usize).min(9) as f32) };
    let at = |t: &[[f32; 11]; 12]| {
        t[x0][y0] * (1.0 - fx) * (1.0 - fy) + t[x0 + 1][y0] * fx * (1.0 - fy) + t[x0][y0 + 1] * (1.0 - fx) * fy + t[x0 + 1][y0 + 1] * fx * fy
    };
    let (half, full) = if v < 0.0 { (1, 0) } else { (2, 3) };
    let a = v.abs();
    let (l50, c50) = (at(&HSL_LUM_L[band][half]), at(&HSL_LUM_C[band][half]));
    if a <= 0.5 {
        let f = a / 0.5;
        return (l50 * f, c50 * f);
    }
    let f = (a - 0.5) / 0.5;
    (l50 + (at(&HSL_LUM_L[band][full]) - l50) * f, c50 + (at(&HSL_LUM_C[band][full]) - c50) * f)
}

/// The Color Mixer (HSL) as measured on Photoshop: it works on linear ProPhoto, its bands
/// weigh a pixel by the hue there (the measured, overlapping weights), and it changes the pixel
/// about its HSL lightness L = (max + min) / 2. Hue turns the hue by the band's measured span,
/// keeping L and the chroma. Lowering Saturation scales the chroma about L (−100 is grey at L,
/// which is why a desaturated colour keeps its brightness); raising it scales the chroma by the
/// band's measured gain, keeping the brightest channel. Luminance changes L and the chroma by
/// measured ratios over the pixel's lightness and relative chroma (greys don't move; darkening a
/// pale colour deepens it, as a sky does on Photoshop). Within about 0.7 levels of Photoshop on the
/// chart (all 144 single-band settings); the old HSV model was three.
pub(super) fn hsl(px: &mut [[f32; 4]], w: usize, p: &CameraRaw, enc: Encoding) {
    let fin = |v: f32| if v.is_finite() { v.clamp(-100.0, 100.0) / 100.0 } else { 0.0 };
    let (hue, sat, lum) = (p.hsl_hue.map(fin), p.hsl_sat.map(fin), p.hsl_lum.map(fin));
    let curves = enc.curves();
    let space = Space::of(enc);
    par_rows(px, w, 1, |_, row| {
        for q in row.iter_mut() {
            let lin = [0, 1, 2].map(|c| curves.decode(q[c]));
            let pro = apply(&space.to, lin).map(|v| v.clamp(0.0, 1.0));
            let (mx, mn) = (pro[0].max(pro[1]).max(pro[2]), pro[0].min(pro[1]).min(pro[2]));
            let (l, c) = ((mx + mn) / 2.0, mx - mn);
            if c <= 1e-6 {
                continue;
            }
            let h = rgb_to_hsv(pro)[0];
            let wt = band_weights(h);
            let (mut dh, mut ks, mut dl, mut dc) = (0.0f32, 1.0f32, 0.0f32, 0.0f32);
            let rel = c / (2.0 * l.min(1.0 - l)).max(1e-9);
            for b in 0..8 {
                if wt[b] <= 0.0 {
                    continue;
                }
                if hue[b] != 0.0 {
                    dh += wt[b] * HSL_SHIFT[b][usize::from(hue[b] > 0.0)] * hue[b].abs();
                }
                if sat[b] != 0.0 {
                    ks += wt[b] * if sat[b] < 0.0 { sat[b] } else { HSL_SAT_GAIN[b] * sat[b] };
                }
                if lum[b] != 0.0 {
                    let (a, c) = hsl_lum_at(b, lum[b], l, rel);
                    dl += wt[b] * a;
                    dc += wt[b] * c;
                }
            }
            let ks = ks.max(0.0);
            let (lr, cr) = (dl.exp(), dc.exp());
            let (mut l2, mut c2) = (l * lr, c * ks * cr);
            if ks > 1.0 {
                // A raise keeps the brightest channel; the others deepen.
                let top = l2 + c * cr / 2.0;
                c2 = c2.min(top);
                l2 = top - c2 / 2.0;
            }
            // Within ProPhoto's gamut, so the hue holds.
            c2 = c2.min(2.0 * l2.clamp(0.0, 1.0).min(1.0 - l2.clamp(0.0, 1.0)));
            let out = from_hlc(h + dh, l2, c2).map(|v| v.max(0.0));
            // Back in the document's space a colour pushed out of its gamut clips, as on Photoshop.
            let top = lin.iter().fold(1.0f32, |m, &v| m.max(v));
            let back = apply(&space.from, out);
            for ch in 0..3 {
                q[ch] = curves.encode(back[ch].clamp(0.0, top));
            }
        }
    });
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

    fn hsl_on(c: [u8; 3], kind: usize, band: usize, v: f32) -> [f32; 3] {
        let mut p = CameraRaw::default();
        [&mut p.hsl_hue, &mut p.hsl_sat, &mut p.hsl_lum][kind][band] = v;
        let mut px = [[f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0, 1.0]];
        hsl(&mut px, 1, &p, Encoding::of(0.0));
        [px[0][0] * 255.0, px[0][1] * 255.0, px[0][2] * 255.0]
    }

    #[test]
    fn hsl_matches_photoshop_on_the_chart() {
        // (chart pixel, kind 0 hue / 1 sat / 2 lum, band, slider, Photoshop's result)
        // The pixels Photoshop moved most; the mean over the chart is about one level.
        type Case = ([u8; 3], usize, usize, f32, [f32; 3]);
        let cases: [Case; 5] = [
            // A sky darkened by Blue Luminance deepens (it doesn't grey).
            ([93, 142, 241], 2, 5, -44.0, [0.0, 86.0, 216.0]),
            ([144, 181, 217], 2, 5, -44.0, [73.0, 137.0, 185.0]),
            ([204, 132, 102], 1, 1, 100.0, [219.0, 106.0, 0.0]),
            ([180, 248, 4], 0, 3, 100.0, [0.0, 255.0, 125.0]),
            ([253, 18, 93], 1, 0, -100.0, [160.0, 160.0, 160.0]),
        ];
        for (c, kind, band, v, ps) in cases {
            let got = hsl_on(c, kind, band, v);
            assert!(got.iter().zip(ps).all(|(g, p)| (g - p).abs() < 18.0), "{c:?} {kind} {band} {v}: {got:?} vs {ps:?}");
        }
    }

    #[test]
    fn white_balance_and_calibration_match_photoshop() {
        let run = |c: f32, f: &dyn Fn(&mut [[f32; 4]])| {
            let mut px = [[c / 255.0, c / 255.0, c / 255.0, 1.0]];
            f(&mut px);
            [px[0][0] * 255.0, px[0][1] * 255.0, px[0][2] * 255.0]
        };
        // Greys 50 and 128 under Temperature +10, Tint −25 and Temperature −25 on Photoshop.
        for (t, n, c, ps) in [
            (10.0, 0.0, 50.0, [59.0, 55.0, 49.0]),
            (10.0, 0.0, 128.0, [146.0, 138.0, 127.0]),
            (0.0, -25.0, 128.0, [130.0, 140.0, 127.0]),
            (-25.0, 0.0, 50.0, [40.0, 55.0, 73.0]),
            (-25.0, 0.0, 128.0, [113.0, 137.0, 169.0]),
        ] {
            let got = run(c, &|px| white_balance(px, 1, t, n, Encoding::of(0.0)));
            assert!(got.iter().zip(ps).all(|(g, p)| (g - p).abs() < 5.0), "{t} {n} {c}: {got:?} vs {ps:?}");
        }
        // Calibration keeps white and greys; a primary's slider changes its colours.
        let m = calibration([(50.0, 0.0), (0.0, -40.0), (0.0, 0.0)], Encoding::of(0.0)).unwrap();
        let grey = apply(&m, [0.3; 3]);
        assert!(grey.iter().all(|v| (v - 0.3).abs() < 1e-4), "{grey:?}");
        let red = apply(&m, [0.6, 0.05, 0.05]);
        assert!((red[1] - 0.05).abs() > 0.02, "{red:?}");
    }

    #[test]
    fn vibrance_matches_photoshop_on_a_photo() {
        // Vibrance −50 on the scene: Photoshop lightens strong colours toward their brightest channel.
        for (c, ps) in [([216u8, 26, 24], [195.0, 75.0, 75.0]), ([36, 61, 227], [117.0, 123.0, 218.0]), ([240, 227, 37], [231.0, 222.0, 131.0])] {
            let mut px = [[f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0, 1.0]];
            vibrance_saturation(&mut px, 1, -50.0, 0.0, Encoding::of(0.0));
            let got = [px[0][0] * 255.0, px[0][1] * 255.0, px[0][2] * 255.0];
            assert!(got.iter().zip(ps).all(|(g, p)| (g - p).abs() < 6.0), "{c:?}: {got:?} vs {ps:?}");
        }
    }

    #[test]
    fn hsl_keeps_greys_and_a_neutral_setting() {
        for kind in 0..3 {
            let g = hsl_on([120, 120, 120], kind, 0, 100.0);
            assert!(g.iter().all(|v| (v - 120.0).abs() < 0.05), "{kind}: {g:?}");
        }
        let n = hsl_on([200, 90, 40], 0, 0, 0.0);
        assert!((n[0] - 200.0).abs() < 0.05 && (n[1] - 90.0).abs() < 0.05 && (n[2] - 40.0).abs() < 0.05, "{n:?}");
        // Out-of-range and non-finite sliders don't break a pixel.
        for v in [f32::NAN, f32::INFINITY, -1e9] {
            let o = hsl_on([200, 90, 40], 2, 1, v);
            assert!(o.iter().all(|x| x.is_finite() && *x >= 0.0), "{v}: {o:?}");
        }
    }

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
