//! Filter › Camera Raw Filter: a float develop pipeline over RGB pixels.
//!
//! Stages, in order (each skipped when its controls are neutral, so defaults are an identity):
//!
//! 1. **White balance and exposure** in linear light (the document's values decoded with the sRGB
//!    curve as a perceptual working encoding; encoding is undone exactly at the end).
//! 2. **Dehaze**: K. He, J. Sun, X. Tang, *Single Image Haze Removal Using Dark Channel Prior*,
//!    CVPR 2009 (dark channel, atmospheric light from its brightest 0.1 %, transmission refined
//!    with the guided filter); negative amounts add haze toward the atmospheric light.
//! 3. **Tone** on luminance with colour ratios kept: Highlights / Shadows as exposure changes
//!    weighted by an edge-preserving base layer (guided filter: K. He, J. Sun, X. Tang, *Guided
//!    Image Filtering*, ECCV 2010), so local contrast survives; Contrast is an S-curve about
//!    middle grey; Clarity (large radius, midtone weighted) and Texture (small radius) add
//!    band-pass local contrast. Whites / Blacks follow as Photoshop's Camera Raw applies them:
//!    image-adaptive global curves measured on Photoshop 25.4 (see `whites_blacks`).
//! 4. **Presence**: Vibrance (weighted toward less saturated colours) and Saturation.
//! 5. **Tone Curve**: parametric regions (Highlights, Lights, Darks, Shadows with movable splits)
//!    then point curves (master and per channel), monotone cubic interpolation (F. Fritsch,
//!    R. Carlson, *Monotone Piecewise Cubic Interpolation*, SIAM J. Numer. Anal. 1980).
//! 6. **Color Mixer (HSL)**: eight hue bands (red, orange, yellow, green, aqua, blue, purple,
//!    magenta) with hat-function weights on the hue circle.
//! 7. **Color Grading**: shadows / midtones / highlights / global wheels (hue, saturation,
//!    luminance) with Blending and Balance.
//! 8. **Detail**: sharpening on luminance (unsharp mask with Detail as halo control and Masking as
//!    an edge mask) and noise reduction (luminance by guided filter, colour by chroma blur).
//! 9. **Effects**: post-crop vignette (amount, midpoint, roundness, feather, highlights,
//!    style) and grain (amount, size, roughness; position-hashed, deterministic).
//!
//! Before stage 1, as Camera Raw applies the lens and the camera profile first: **Optics**
//! (manual distortion, defringe, lens vignetting) and **Calibration** (the primaries' hue and
//! saturation as a matrix on linear light that keeps white, and the shadows' tint). The **Black &
//! White** treatment replaces the Color Mixer with the B&W Mix (stage 6); Color Grading still
//! tints the grey.

#![allow(clippy::needless_range_loop)] // index loops mirror the maths (pixels × channels)

use serde::{Deserialize, Serialize};

/// Most points a Camera Raw point curve accepts from commands and the control channel.
pub const MAX_CURVE_POINTS: usize = 16;

/// Checks a point curve from untrusted input: empty (linear) or 2..=16 finite points in 0..=255
/// with inputs increasing by at least one level. Deserialization stays lenient on purpose:
/// earlier editors saved curves this rejects, and [`curve_lut`] already sorts and de-duplicates
/// them, so stored Smart Filters keep rendering.
pub fn validate_curve(points: &[[f32; 2]]) -> Result<(), String> {
    if points.len() == 1 {
        return Err("a nonempty curve needs at least two points".into());
    }
    if points.len() > MAX_CURVE_POINTS {
        return Err(format!("at most {MAX_CURVE_POINTS} curve points"));
    }
    if !points.iter().flatten().all(|v| v.is_finite() && (0.0..=255.0).contains(v)) {
        return Err("curve coordinates must be finite and in 0..=255".into());
    }
    if points.windows(2).any(|w| w[1][0] - w[0][0] < 1.0) {
        return Err("curve inputs must increase by at least one level".into());
    }
    Ok(())
}

use crate::photo_util::{hash01, linear_to_srgb, par_rows, srgb_to_linear};

mod local_tone;
mod whites_blacks;

/// How the pixels encode light: the sRGB curve (gamma 0) or a pure power law (ProPhoto's 1.8,
/// Adobe RGB's 2.2). The tone controls work on light, so the document's own curve decodes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Encoding(f32);

impl Encoding {
    pub(crate) fn of(gamma: f32) -> Self {
        Encoding(if gamma.is_finite() && gamma > 0.0 { gamma.clamp(0.5, 4.0) } else { 0.0 })
    }
    /// A value in this encoding as linear light.
    pub(crate) fn decode(self, v: f32) -> f32 {
        if self.0 > 0.0 { v.max(0.0).powf(self.0) } else { srgb_to_linear(v) }
    }
    /// Linear light back into this encoding, the inverse of [`Encoding::decode`].
    pub(crate) fn encode(self, v: f32) -> f32 {
        if self.0 > 0.0 { v.max(0.0).powf(1.0 / self.0) } else { linear_to_srgb(v) }
    }
    /// The curves as tables (see [`Curves`]); sRGB's is built once.
    pub(crate) fn curves(self) -> std::borrow::Cow<'static, Curves> {
        static SRGB: std::sync::OnceLock<Curves> = std::sync::OnceLock::new();
        if self.0 > 0.0 { std::borrow::Cow::Owned(Curves::new(self)) } else { std::borrow::Cow::Borrowed(SRGB.get_or_init(|| Curves::new(Encoding(0.0)))) }
    }
}

/// Table entries per unit of [`Curves`].
const CURVE_STEPS: usize = 4096;
/// [`Curves::encode`] tables light up to this (an exposure raise takes values past 1).
const ENCODE_TOP: f32 = 16.0;

/// An [`Encoding`]'s curves sampled into tables, read with linear interpolation: a power per
/// channel and pixel was most of a develop's time (`powf` in every stage that works on light).
/// Decoding is tabled over 0 … 1 and encoding over 0 … [`ENCODE_TOP`], indexed by the square root
/// so the steep start of the curve gets its share of entries; values outside, and the first table
/// step, use the exact curve. The tables differ from it by under 2·10⁻⁷.
#[derive(Clone, Debug)]
pub(crate) struct Curves {
    exact: Encoding,
    decode: Vec<f32>,
    encode: Vec<f32>,
}

impl Curves {
    fn new(exact: Encoding) -> Self {
        let n = CURVE_STEPS as f32;
        let decode = (0..=CURVE_STEPS).map(|i| exact.decode(i as f32 / n)).collect();
        let encode = (0..=2 * CURVE_STEPS)
            .map(|i| {
                let u = i as f32 / (2.0 * n);
                exact.encode(u * u * ENCODE_TOP)
            })
            .collect();
        Curves { exact, decode, encode }
    }
    #[inline]
    pub(crate) fn decode(&self, v: f32) -> f32 {
        let n = CURVE_STEPS as f32;
        // Also NaN takes the exact curve.
        if !(v * n >= 1.0 && v <= 1.0) {
            return self.exact.decode(v);
        }
        let x = v * n;
        let i = (x as usize).min(CURVE_STEPS - 1);
        let t = x - i as f32;
        let (a, b) = (self.decode[i], self.decode[i + 1]);
        a + (b - a) * t
    }
    #[inline]
    pub(crate) fn encode(&self, v: f32) -> f32 {
        let n = (2 * CURVE_STEPS) as f32;
        let x = (v / ENCODE_TOP).sqrt() * n;
        if !(x >= 1.0 && v <= ENCODE_TOP) {
            return self.exact.encode(v);
        }
        let i = (x as usize).min(2 * CURVE_STEPS - 1);
        let t = x - i as f32;
        let (a, b) = (self.encode[i], self.encode[i + 1]);
        a + (b - a) * t
    }
}

fn luma_linear(c: [f32; 3]) -> f32 {
    luma(c)
}

/// One colour grading wheel.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Wheel {
    /// Degrees 0..360.
    pub hue: f32,
    /// 0..=100.
    pub sat: f32,
    /// −100..=100.
    pub lum: f32,
}

impl Wheel {
    fn neutral(&self) -> bool {
        self.sat == 0.0 && self.lum == 0.0
    }
}

/// Camera Raw settings in Adobe Camera Raw's slider units.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CameraRaw {
    // Basic
    pub temperature: f32,
    pub tint: f32,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub vibrance: f32,
    pub saturation: f32,
    // Tone curve
    pub curve_highlights: f32,
    pub curve_lights: f32,
    pub curve_darks: f32,
    pub curve_shadows: f32,
    /// Region splits (percent).
    pub curve_splits: [f32; 3],
    /// Point curves as `[input, output]` in 0..=255 (empty = linear).
    pub point_curve: Vec<[f32; 2]>,
    pub point_curve_red: Vec<[f32; 2]>,
    pub point_curve_green: Vec<[f32; 2]>,
    pub point_curve_blue: Vec<[f32; 2]>,
    // Color mixer: red, orange, yellow, green, aqua, blue, purple, magenta.
    pub hsl_hue: [f32; 8],
    pub hsl_sat: [f32; 8],
    pub hsl_lum: [f32; 8],
    // Color grading
    pub grade_shadows: Wheel,
    pub grade_midtones: Wheel,
    pub grade_highlights: Wheel,
    pub grade_global: Wheel,
    pub grade_blending: f32,
    pub grade_balance: f32,
    // Detail
    pub sharpen_amount: f32,
    pub sharpen_radius: f32,
    pub sharpen_detail: f32,
    pub sharpen_masking: f32,
    pub noise_luminance: f32,
    pub noise_luminance_detail: f32,
    pub noise_color: f32,
    pub noise_color_detail: f32,
    // Effects
    pub grain_amount: f32,
    pub grain_size: f32,
    pub grain_roughness: f32,
    pub vignette_amount: f32,
    pub vignette_midpoint: f32,
    pub vignette_roundness: f32,
    pub vignette_feather: f32,
    pub vignette_highlights: f32,
    /// `highlightPriority | colorPriority | paintOverlay`.
    pub vignette_style: String,
    // Profile
    /// Black & White treatment (`ConvertToGrayscale`): the B&W Mix replaces the Color Mixer.
    pub monochrome: bool,
    /// B&W Mix (`GrayMixerRed` …): each hue band's grey lighter or darker, −100..=100, in the
    /// Color Mixer's bands.
    pub gray_mix: [f32; 8],
    /// Refine Saturation (`CurveRefineSaturation`, 0..=100): how much of the saturation a
    /// contrasty curve adds is kept (100: all of it, as plain RGB curves do).
    pub curve_refine_saturation: f32,
    /// Noise reduction's Contrast (`LuminanceNoiseReductionContrast`) and Smoothness
    /// (`ColorNoiseReductionSmoothness`), 0..=100.
    pub noise_luminance_contrast: f32,
    pub noise_color_smoothness: f32,
    // Optics
    /// Manual distortion (`LensManualDistortionAmount`, −100..=100): positive straightens barrel
    /// distortion, negative pincushion.
    pub lens_distortion: f32,
    /// Lens vignetting (`VignetteAmount`, −100..=100: positive lightens the corners) and its
    /// midpoint (`VignetteMidpoint`, 0..=100).
    pub lens_vignette_amount: f32,
    pub lens_vignette_midpoint: f32,
    /// Defringe: the purple and green amounts (0..=20) and hue ranges (low, high in 0..=100).
    pub defringe_purple_amount: f32,
    pub defringe_purple_hue: [f32; 2],
    pub defringe_green_amount: f32,
    pub defringe_green_hue: [f32; 2],
    // Calibration
    /// Shadows tint (`ShadowTint`, −100 green … 100 magenta).
    pub shadow_tint: f32,
    /// The primaries' hue and saturation (`RedHue`, `RedSaturation`, …), −100..=100.
    pub red_hue: f32,
    pub red_saturation: f32,
    pub green_hue: f32,
    pub green_saturation: f32,
    pub blue_hue: f32,
    pub blue_saturation: f32,
    pub seed: u32,
    /// Pixel-size multiplier for previews on downsampled proxies (radii in px scale by it).
    pub pixel_scale: f32,
    /// The document's transfer curve: 0 for sRGB's, else the gamma of a power-law space (1.8
    /// for ProPhoto). Set from the document's profile; Whites and Blacks act on light.
    pub encoding_gamma: f32,
}

impl Default for CameraRaw {
    fn default() -> Self {
        CameraRaw {
            temperature: 0.0,
            tint: 0.0,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            texture: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            vibrance: 0.0,
            saturation: 0.0,
            curve_highlights: 0.0,
            curve_lights: 0.0,
            curve_darks: 0.0,
            curve_shadows: 0.0,
            curve_splits: [25.0, 50.0, 75.0],
            point_curve: Vec::new(),
            point_curve_red: Vec::new(),
            point_curve_green: Vec::new(),
            point_curve_blue: Vec::new(),
            hsl_hue: [0.0; 8],
            hsl_sat: [0.0; 8],
            hsl_lum: [0.0; 8],
            grade_shadows: Wheel::default(),
            grade_midtones: Wheel::default(),
            grade_highlights: Wheel::default(),
            grade_global: Wheel::default(),
            grade_blending: 50.0,
            grade_balance: 0.0,
            sharpen_amount: 0.0,
            sharpen_radius: 1.0,
            sharpen_detail: 25.0,
            sharpen_masking: 0.0,
            noise_luminance: 0.0,
            noise_luminance_detail: 50.0,
            noise_color: 0.0,
            noise_color_detail: 50.0,
            grain_amount: 0.0,
            grain_size: 25.0,
            grain_roughness: 50.0,
            vignette_amount: 0.0,
            vignette_midpoint: 50.0,
            vignette_roundness: 0.0,
            vignette_feather: 50.0,
            vignette_highlights: 0.0,
            vignette_style: "highlightPriority".into(),
            monochrome: false,
            gray_mix: [0.0; 8],
            curve_refine_saturation: 100.0,
            noise_luminance_contrast: 0.0,
            noise_color_smoothness: 50.0,
            lens_distortion: 0.0,
            lens_vignette_amount: 0.0,
            lens_vignette_midpoint: 50.0,
            defringe_purple_amount: 0.0,
            defringe_purple_hue: [30.0, 70.0],
            defringe_green_amount: 0.0,
            defringe_green_hue: [40.0, 60.0],
            shadow_tint: 0.0,
            red_hue: 0.0,
            red_saturation: 0.0,
            green_hue: 0.0,
            green_saturation: 0.0,
            blue_hue: 0.0,
            blue_saturation: 0.0,
            seed: 0,
            pixel_scale: 1.0,
            encoding_gamma: 0.0,
        }
    }
}

/// The Black & White treatment's grey for `c`: its luminance, each hue band's share made lighter
/// or darker by its B&W Mix slider in proportion to the colour's saturation.
fn gray_mix(c: [f32; 3], mix: &[f32; 8]) -> [f32; 3] {
    let l = luma(c);
    let [hh, s, _] = rgb_to_hsv(c.map(|x| x.max(0.0)));
    let d: f32 = if s > 1e-4 { band_weights(hh).iter().zip(mix).map(|(w, m)| w * m.clamp(-100.0, 100.0)).sum() } else { 0.0 };
    [l * 2f32.powf(d / 100.0 * 1.2 * s); 3]
}

/// Hue band centres of the Color Mixer (degrees).
pub const HSL_BANDS: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0];

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Box mean of a single-channel buffer (radius `r`, edge clamped): rows in parallel, then
/// columns in parallel bands with running column sums (cache-friendly, no transposes).
fn box_mean(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 || w == 0 || h == 0 {
        return v.to_vec();
    }
    let n = (2 * r + 1) as f64;
    let mut tmp = vec![0.0f32; w * h];
    par_rows(&mut tmp, w, 1, |y, row| {
        let src = &v[y * w..(y + 1) * w];
        let at = |k: i64| src[k.clamp(0, w as i64 - 1) as usize] as f64;
        let mut acc: f64 = (-(r as i64)..=r as i64).map(at).sum();
        for (x, o) in row.iter_mut().enumerate() {
            *o = (acc / n) as f32;
            acc += at(x as i64 + r as i64 + 1) - at(x as i64 - r as i64);
        }
    });
    // Columns: strips of rows in parallel, each sliding its own running column sums down (a strip
    // starts with the sums of its first window); written in place, no transposes or copies.
    let strip = h.div_ceil(rayon_strips()).max(2 * r + 1).max(16);
    let row = |y: i64| &tmp[y.clamp(0, h as i64 - 1) as usize * w..(y.clamp(0, h as i64 - 1) as usize + 1) * w];
    let mut out = vec![0.0f32; w * h];
    let fill = |s: usize, part: &mut [f32]| {
        let y0 = s * strip;
        let mut acc = vec![0.0f64; w];
        for y in y0 as i64 - r as i64..=y0 as i64 + r as i64 {
            for (a, v) in acc.iter_mut().zip(row(y)) {
                *a += *v as f64;
            }
        }
        for (dy, out_row) in part.chunks_mut(w).enumerate() {
            let y = (y0 + dy) as i64;
            for (o, a) in out_row.iter_mut().zip(&acc) {
                *o = (*a / n) as f32;
            }
            let (add, sub) = (row(y + r as i64 + 1), row(y - r as i64));
            for ((a, p), m) in acc.iter_mut().zip(add).zip(sub) {
                *a += (*p - *m) as f64;
            }
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        out.par_chunks_mut(strip * w).enumerate().for_each(|(s, part)| fill(s, part));
    }
    #[cfg(target_arch = "wasm32")]
    out.chunks_mut(strip * w).enumerate().for_each(|(s, part)| fill(s, part));
    out
}

/// How many strips [`box_mean`] splits its columns into: a few per thread.
fn rayon_strips() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        rayon::current_num_threads().max(1) * 4
    }
    #[cfg(target_arch = "wasm32")]
    {
        1
    }
}

/// Approximate Gaussian (three box passes).
fn gauss(v: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma < 0.3 {
        return v.to_vec();
    }
    let mut out = v.to_vec();
    for r in crate::fxutil::gauss_box_radii(sigma) {
        out = box_mean(&out, w, h, r);
    }
    out
}

/// Fast guided filter (K. He, J. Sun, *Fast Guided Filter*, arXiv 2015): the linear
/// coefficients are computed on a `s×` subsampled copy and upsampled bilinearly, which is
/// visually equivalent for the large radii used by tone controls at a fraction of the cost.
fn guided_fast(i: &[f32], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let s = (r / 8).clamp(1, 8);
    if s == 1 {
        return guided(i, p, w, h, r, eps);
    }
    let (sw, sh) = (w.div_ceil(s), h.div_ceil(s));
    let (si, sp) = (block_mean(i, w, h, s), block_mean(p, w, h, s));
    let rs = (r / s).max(1);
    let (ma, mb) = guided_coefficients(&si, &sp, sw, sh, rs, eps);
    let mut out = vec![0.0f32; w * h];
    let sf = s as f32;
    par_rows(&mut out, w, 1, |y, row| {
        let fy = (y as f32 + 0.5) / sf - 0.5;
        for (x, o) in row.iter_mut().enumerate() {
            let fx = (x as f32 + 0.5) / sf - 0.5;
            let av = crate::photo_util::bilinear(&ma, sw, sh, 1, 0, fx, fy);
            let bv = crate::photo_util::bilinear(&mb, sw, sh, 1, 0, fx, fy);
            *o = av * i[y * w + x] + bv;
        }
    });
    out
}

/// `v` averaged over `s × s` blocks (the last ones partial), rows of blocks in parallel.
fn block_mean(v: &[f32], w: usize, h: usize, s: usize) -> Vec<f32> {
    let (sw, sh) = (w.div_ceil(s), h.div_ceil(s));
    let rows = crate::photo_util::par_map(sh, |by| {
        let mut sum = vec![0.0f32; sw];
        let mut count = vec![0.0f32; sw];
        for y in by * s..((by + 1) * s).min(h) {
            for (x, value) in v[y * w..(y + 1) * w].iter().enumerate() {
                sum[x / s] += value;
                count[x / s] += 1.0;
            }
        }
        sum.iter().zip(&count).map(|(a, c)| a / c.max(1.0)).collect::<Vec<f32>>()
    });
    rows.concat()
}

/// The guided filter's smoothed linear coefficients `(mean a, mean b)` of `p` against `i`.
fn guided_coefficients(i: &[f32], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> (Vec<f32>, Vec<f32>) {
    use crate::photo_util::par_map as par;
    let mi = box_mean(i, w, h, r);
    let mp = box_mean(p, w, h, r);
    let mip = box_mean(&par(w * h, |k| i[k] * p[k]), w, h, r);
    let mii = box_mean(&par(w * h, |k| i[k] * i[k]), w, h, r);
    let a = par(w * h, |k| (mip[k] - mi[k] * mp[k]) / (mii[k] - mi[k] * mi[k] + eps));
    let b = par(w * h, |k| mp[k] - a[k] * mi[k]);
    (box_mean(&a, w, h, r), box_mean(&b, w, h, r))
}

/// Guided filter of `p` guided by `i` (He et al. 2010).
fn guided(i: &[f32], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let (ma, mb) = guided_coefficients(i, p, w, h, r, eps);
    crate::photo_util::par_map(w * h, |k| ma[k] * i[k] + mb[k])
}

/// Monotone cubic (Fritsch–Carlson) through `pts` (x ascending, 0..=1), evaluated into a LUT.
pub fn curve_lut(pts: &[[f32; 2]], n: usize) -> Vec<f32> {
    let mut p: Vec<[f32; 2]> = pts.iter().map(|q| [q[0] / 255.0, q[1] / 255.0]).collect();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]));
    p.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6);
    if p.len() < 2 {
        return (0..n).map(|k| k as f32 / (n - 1) as f32).collect();
    }
    let m = p.len();
    let d: Vec<f32> = (0..m - 1).map(|k| (p[k + 1][1] - p[k][1]) / (p[k + 1][0] - p[k][0]).max(1e-6)).collect();
    let mut t = vec![0.0f32; m];
    t[0] = d[0];
    t[m - 1] = d[m - 2];
    for k in 1..m - 1 {
        t[k] = if d[k - 1] * d[k] <= 0.0 { 0.0 } else { (d[k - 1] + d[k]) / 2.0 };
    }
    for k in 0..m - 1 {
        if d[k] == 0.0 {
            t[k] = 0.0;
            t[k + 1] = 0.0;
            continue;
        }
        let (a, b) = (t[k] / d[k], t[k + 1] / d[k]);
        let s = a * a + b * b;
        if s > 9.0 {
            let tau = 3.0 / s.sqrt();
            t[k] = tau * a * d[k];
            t[k + 1] = tau * b * d[k];
        }
    }
    (0..n)
        .map(|i| {
            let x = i as f32 / (n - 1) as f32;
            if x <= p[0][0] {
                return p[0][1];
            }
            if x >= p[m - 1][0] {
                return p[m - 1][1];
            }
            let k = p.windows(2).position(|wv| x <= wv[1][0]).unwrap_or(m - 2);
            let hh = p[k + 1][0] - p[k][0];
            let s = (x - p[k][0]) / hh;
            let (h00, h10, h01, h11) = (2.0 * s * s * s - 3.0 * s * s + 1.0, s * s * s - 2.0 * s * s + s, -2.0 * s * s * s + 3.0 * s * s, s * s * s - s * s);
            (h00 * p[k][1] + h10 * hh * t[k] + h01 * p[k + 1][1] + h11 * hh * t[k + 1]).clamp(0.0, 1.0)
        })
        .collect()
}

fn lut_eval(lut: &[f32], v: f32) -> f32 {
    if !(0.0..=1.0).contains(&v) {
        // Extend linearly outside (float documents).
        return if v < 0.0 { v + lut[0] } else { lut[lut.len() - 1] + (v - 1.0) };
    }
    let x = v * (lut.len() - 1) as f32;
    let i = (x as usize).min(lut.len() - 2);
    let t = x - i as f32;
    lut[i] + (lut[i + 1] - lut[i]) * t
}

/// Parametric tone curve as a LUT (regions split at `splits`, each slider ±100 moves its region
/// by up to a quarter of the range, smoothly).
fn parametric_lut(p: &CameraRaw, n: usize) -> Vec<f32> {
    let s = p.curve_splits.map(|v| v.clamp(5.0, 95.0) / 100.0);
    let centres = [s[0] / 2.0, (s[0] + s[1]) / 2.0, (s[1] + s[2]) / 2.0, (s[2] + 1.0) / 2.0];
    let amounts = [p.curve_shadows, p.curve_darks, p.curve_lights, p.curve_highlights];
    let widths = [s[0].max(0.1), (s[1] - s[0]).max(0.1), (s[2] - s[1]).max(0.1), (1.0 - s[2]).max(0.1)];
    let mut lut: Vec<f32> = (0..n)
        .map(|i| {
            let x = i as f32 / (n - 1) as f32;
            let mut y = x;
            for k in 0..4 {
                let d = (x - centres[k]) / (widths[k] * 1.2);
                let bump = (-d * d * 2.0).exp() * x * (1.0 - x) * 4.0;
                y += amounts[k] / 100.0 * 0.25 * bump;
            }
            y.clamp(0.0, 1.0)
        })
        .collect();
    // Keep it monotone.
    for i in 1..n {
        lut[i] = lut[i].max(lut[i - 1]);
    }
    lut
}

/// The parametric tone curve of `p` (its four regions and their splits) at `n` points over
/// 0 … 1, for drawing.
pub fn parametric_curve(p: &CameraRaw, n: usize) -> Vec<f32> {
    parametric_lut(p, n.max(2))
}

fn rgb_to_hsv(c: [f32; 3]) -> [f32; 3] {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = mx - mn;
    let h = if d <= 1e-9 {
        0.0
    } else if mx == c[0] {
        60.0 * (((c[1] - c[2]) / d).rem_euclid(6.0))
    } else if mx == c[1] {
        60.0 * ((c[2] - c[0]) / d + 2.0)
    } else {
        60.0 * ((c[0] - c[1]) / d + 4.0)
    };
    [h, if mx > 1e-9 { d / mx } else { 0.0 }, mx]
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

/// Weights of the eight HSL bands at hue `h` (hat functions between neighbouring centres).
pub fn band_weights(h: f32) -> [f32; 8] {
    let mut w = [0.0f32; 8];
    // Only the two bands around `h` weigh anything: found directly (a remainder per band and
    // pixel was most of the Color Mixer's time).
    let h = if (0.0..360.0).contains(&h) { h } else { h.rem_euclid(360.0) };
    if !h.is_finite() {
        return w;
    }
    let k = HSL_BANDS.iter().rposition(|c| *c <= h).unwrap_or(7);
    let next = (k + 1) % 8;
    let span = (HSL_BANDS[next] - HSL_BANDS[k]).rem_euclid(360.0).max(1e-3);
    let t = ((h - HSL_BANDS[k]) / span).clamp(0.0, 1.0);
    w[k] = 1.0 - t;
    w[next] += t;
    w
}

impl CameraRaw {
    /// Rejects malformed point curves in new settings (see [`validate_curve`]).
    pub fn validate(&self) -> Result<(), String> {
        for (name, curve) in [
            ("pointCurve", &self.point_curve),
            ("pointCurveRed", &self.point_curve_red),
            ("pointCurveGreen", &self.point_curve_green),
            ("pointCurveBlue", &self.point_curve_blue),
        ] {
            validate_curve(curve).map_err(|e| format!("{name}: {e}"))?;
        }
        Ok(())
    }
    /// [`validate`](Self::validate) for an edit of `stored` settings: a curve left as stored is
    /// accepted, so editing another control of an older Smart Filter still works.
    pub fn validate_changes(&self, stored: &CameraRaw) -> Result<(), String> {
        for (name, curve, kept) in [
            ("pointCurve", &self.point_curve, &stored.point_curve),
            ("pointCurveRed", &self.point_curve_red, &stored.point_curve_red),
            ("pointCurveGreen", &self.point_curve_green, &stored.point_curve_green),
            ("pointCurveBlue", &self.point_curve_blue, &stored.point_curve_blue),
        ] {
            if curve != kept {
                validate_curve(curve).map_err(|e| format!("{name}: {e}"))?;
            }
        }
        Ok(())
    }
    fn wb_neutral(&self) -> bool {
        self.temperature == 0.0 && self.tint == 0.0 && self.exposure == 0.0
    }
    fn tone_neutral(&self) -> bool {
        self.local_tone_neutral() && self.whites == 0.0 && self.blacks == 0.0
    }
    /// Contrast, Highlights, Shadows, Texture and Clarity (the luminance stage).
    fn local_tone_neutral(&self) -> bool {
        [self.contrast, self.highlights, self.shadows, self.texture, self.clarity].iter().all(|v| *v == 0.0)
    }
    fn hsl_neutral(&self) -> bool {
        self.hsl_hue.iter().chain(&self.hsl_sat).chain(&self.hsl_lum).all(|v| *v == 0.0)
    }
    fn grade_neutral(&self) -> bool {
        self.grade_shadows.neutral() && self.grade_midtones.neutral() && self.grade_highlights.neutral() && self.grade_global.neutral()
    }
    fn curves_neutral(&self) -> bool {
        [self.curve_highlights, self.curve_lights, self.curve_darks, self.curve_shadows].iter().all(|v| *v == 0.0)
            && [&self.point_curve, &self.point_curve_red, &self.point_curve_green, &self.point_curve_blue].iter().all(|c| is_linear(c))
    }
    /// Optics and Calibration: stage 0 (see [`front_settings`]).
    fn front_neutral(&self) -> bool {
        [self.lens_distortion, self.lens_vignette_amount, self.defringe_purple_amount, self.defringe_green_amount].iter().all(|v| *v == 0.0)
            && self.calibration_neutral()
    }
    fn calibration_neutral(&self) -> bool {
        [self.shadow_tint, self.red_hue, self.red_saturation, self.green_hue, self.green_saturation, self.blue_hue, self.blue_saturation].iter().all(|v| *v == 0.0)
    }
    /// True when every control is neutral (the filter is an identity).
    pub fn is_identity(&self) -> bool {
        self.front_neutral()
            && !self.monochrome
            && self.wb_neutral()
            && self.tone_neutral()
            && self.dehaze == 0.0
            && self.vibrance == 0.0
            && self.saturation == 0.0
            && self.curves_neutral()
            && self.hsl_neutral()
            && self.grade_neutral()
            && self.sharpen_amount == 0.0
            && self.noise_luminance == 0.0
            && self.noise_color == 0.0
            && self.grain_amount == 0.0
            && self.vignette_amount == 0.0
    }
}

fn is_linear(c: &[[f32; 2]]) -> bool {
    c.len() < 2 || c.iter().all(|p| (p[0] - p[1]).abs() < 1e-3)
}

/// Camera Raw's white-balance channel gains for Temperature / Tint (−100…100), relative to the
/// as-shot balance: warmer raises red and lowers blue (±35 % at the ends); a negative tint
/// raises green, a positive one lowers it toward magenta (±25 %). An observed approximation of
/// Adobe Camera Raw's sliders, not a colour-temperature model; shared by the filter (in linear
/// RGB) and the raw open (on the camera's white-balance multipliers) so both agree.
pub fn white_balance_gains(temperature: f32, tint: f32) -> [f32; 3] {
    let t = temperature / 100.0;
    let tn = tint / 100.0;
    [1.0 + 0.35 * t, 1.0 - 0.25 * tn, 1.0 - 0.35 * t]
}

/// Runs the Camera Raw pipeline on straight RGBA pixels (`w × h`, display-encoded RGB).
/// `float` keeps values above 1 (32-bit documents).
pub fn develop(px: &mut [[f32; 4]], w: usize, h: usize, p: &CameraRaw, float: bool) {
    if px.is_empty() || p.is_identity() {
        return;
    }
    light(px, w, h, p, None);
    presence(px, w, h, p);
    finish(px, w, h, p, float);
}

/// The settings [`light`] reads.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LightKey {
    front: [f32; 16],
    temperature: f32,
    tint: f32,
    exposure: f32,
    dehaze: f32,
    shadows: f32,
    highlights: f32,
    gamma: f32,
    scale: f32,
}

impl LightKey {
    fn of(p: &CameraRaw) -> Self {
        LightKey {
            front: front_settings(p),
            temperature: p.temperature,
            tint: p.tint,
            exposure: p.exposure,
            dehaze: p.dehaze,
            shadows: p.shadows,
            highlights: p.highlights,
            gamma: p.encoding_gamma,
            scale: p.pixel_scale,
        }
    }
}

/// The settings [`light`] and [`presence`] read.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PresenceKey {
    light: LightKey,
    tone: [f32; 5],
}

impl PresenceKey {
    fn of(p: &CameraRaw) -> Self {
        PresenceKey { light: LightKey::of(p), tone: [p.contrast, p.texture, p.clarity, p.whites, p.blacks] }
    }
}

/// What [`develop_cached`] keeps between calls on the same pixels: the image after the stages
/// that look at its surroundings (white balance to Whites and Blacks), so moving a later slider
/// redoes only the colour, detail and effects stages, and the Shadows and Highlights maps (see
/// `local_tone`), so moving those sliders or Exposure doesn't filter the image again.
#[derive(Default)]
pub struct DevelopCache {
    source: Option<(u64, usize, usize)>,
    light: Option<(LightKey, Vec<[f32; 4]>)>,
    presence: Option<(PresenceKey, Vec<[f32; 4]>)>,
    tone: local_tone::Cache,
}

/// [`develop_cached`]'s pixels, and whether they are exactly [`develop`]'s.
pub struct Developed {
    pub px: Vec<[f32; 4]>,
    pub exact: bool,
}

/// [`develop`] of `src` for an interactive preview, reusing what `cache` kept from earlier calls.
/// `source` names the pixels: another value (or size) drops what was kept. With `draft` (while a
/// slider moves), Shadows and Highlights may reuse maps made for other settings before them (white
/// balance, Dehaze, the other of the two), which is close but not exact: `exact` is false then,
/// and a call without `draft` gives [`develop`]'s result.
#[allow(clippy::too_many_arguments)]
pub fn develop_cached(src: &[[f32; 4]], w: usize, h: usize, p: &CameraRaw, float: bool, source: u64, cache: &mut DevelopCache, draft: bool) -> Developed {
    if src.is_empty() || src.len() != w.saturating_mul(h) || p.is_identity() {
        return Developed { px: src.to_vec(), exact: true };
    }
    if cache.source != Some((source, w, h)) {
        *cache = DevelopCache { source: Some((source, w, h)), ..Default::default() };
    }
    let (lk, pk) = (LightKey::of(p), PresenceKey::of(p));
    let mut exact = true;
    let mut px = match &cache.presence {
        Some((k, v)) if *k == pk => v.clone(),
        _ => {
            let mut px = match &cache.light {
                Some((k, v)) if *k == lk => v.clone(),
                _ => {
                    let mut px = src.to_vec();
                    exact = light(&mut px, w, h, p, Some((&mut cache.tone, tone_key(source, p), draft)));
                    if exact {
                        cache.light = Some((lk, px.clone()));
                    }
                    px
                }
            };
            presence(&mut px, w, h, p);
            if exact {
                cache.presence = Some((pk, px.clone()));
            }
            px
        }
    };
    finish(&mut px, w, h, p, float);
    Developed { px, exact }
}

/// What the Shadows and Highlights maps depend on besides those two sliders: the pixels and the
/// settings before them. Exposure only shifts the log luminance they filter, which the filter
/// ignores, so it counts only when Dehaze (which it changes) runs between.
fn tone_key(source: u64, p: &CameraRaw) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    let exposure = if p.dehaze != 0.0 { p.exposure } else { 0.0 };
    for v in [p.temperature, p.tint, p.dehaze, exposure, p.encoding_gamma, p.pixel_scale].into_iter().chain(front_settings(p)) {
        v.to_bits().hash(&mut h);
    }
    h.finish()
}

/// The settings of stage 0 ([`optics`] and [`calibrate`]).
fn front_settings(p: &CameraRaw) -> [f32; 16] {
    [
        p.lens_distortion,
        p.lens_vignette_amount,
        p.lens_vignette_midpoint,
        p.defringe_purple_amount,
        p.defringe_purple_hue[0],
        p.defringe_purple_hue[1],
        p.defringe_green_amount,
        p.defringe_green_hue[0],
        p.defringe_green_hue[1],
        p.shadow_tint,
        p.red_hue,
        p.red_saturation,
        p.green_hue,
        p.green_saturation,
        p.blue_hue,
        p.blue_saturation,
    ]
}

/// Stages 1–3a: white balance and exposure, Dehaze, Shadows and Highlights. `tone`: the cache of
/// the last, its key ([`tone_key`]) and whether stale maps may serve; false when one did.
fn light(px: &mut [[f32; 4]], w: usize, h: usize, p: &CameraRaw, tone: Option<(&mut local_tone::Cache, u64, bool)>) -> bool {
    // 0. The lens and the camera profile.
    if !p.front_neutral() {
        optics(px, w, h, p);
        calibrate(px, w, p);
    }
    // 1. White balance + exposure in linear light.
    if !p.wb_neutral() {
        let mut g = white_balance_gains(p.temperature, p.tint);
        let norm = luma(g);
        g = g.map(|v| v / norm * 2f32.powf(p.exposure));
        let srgb = Encoding(0.0).curves();
        par_rows(px, w, 1, |_, row| {
            for q in row.iter_mut() {
                for c in 0..3 {
                    q[c] = srgb.encode((srgb.decode(q[c]) * g[c]).max(0.0));
                }
            }
        });
    }
    // 2. Dehaze.
    if p.dehaze != 0.0 {
        dehaze(px, w, h, p.dehaze / 100.0);
    }
    // 3. Tone. Highlights and Shadows: a local Laplacian filter (see `local_tone`).
    if p.highlights != 0.0 || p.shadows != 0.0 {
        return local_tone::apply(px, w, h, p.shadows, p.highlights, Encoding::of(p.encoding_gamma), tone);
    }
    true
}

/// Stage 3, continued: Contrast, Texture and Clarity on luminance, then Whites and Blacks.
fn presence(px: &mut [[f32; 4]], w: usize, h: usize, p: &CameraRaw) {
    use crate::photo_util::par_map as par;
    let ps = p.pixel_scale.clamp(0.01, 1.0);
    let long = w.max(h) as f32;
    if [p.contrast, p.texture, p.clarity].iter().any(|v| *v != 0.0) {
        let l: Vec<f32> = par(px.len(), |i| luma([px[i][0], px[i][1], px[i][2]]).max(0.0));
        let lc: Vec<f32> = par(l.len(), |i| l[i].min(1.0));
        let clar = (p.clarity != 0.0).then(|| guided_fast(&lc, &lc, w, h, ((long * 0.02) as usize).max(3), 0.005));
        let tex = (p.texture != 0.0).then(|| gauss(&lc, w, h, (long * 0.002).max(1.5 * ps)));
        let k_con = p.contrast / 100.0 * 0.6;
        let tau = std::f32::consts::TAU;
        par_rows(px, w, 1, |y, row| {
            for (x, q) in row.iter_mut().enumerate() {
                let i = y * w + x;
                let l0 = l[i];
                let mut lv = lc[i];
                if let Some(c) = &clar {
                    let mw = 1.0 - (2.0 * lv.clamp(0.0, 1.0) - 1.0).powi(2);
                    lv += p.clarity / 100.0 * 0.8 * (lc[i] - c[i]) * mw;
                }
                if let Some(t) = &tex {
                    lv += p.texture / 100.0 * 0.9 * (lc[i] - t[i]).clamp(-0.1, 0.1);
                }
                if k_con != 0.0 {
                    let xx = lv.clamp(0.0, 1.0);
                    lv += -k_con * (tau * xx).sin() / tau;
                }
                let lv = lv.max(0.0) + (l0 - lc[i]);
                let ratio = if l0 > 1e-5 { lv / l0 } else { 1.0 };
                for c in 0..3 {
                    q[c] = if l0 > 1e-5 { q[c] * ratio } else { q[c] + (lv - l0) };
                }
            }
        });
    }
    // 3b. Whites and Blacks: global curves adapted to the image's black and white points.
    if p.whites != 0.0 || p.blacks != 0.0 {
        let enc = Encoding::of(p.encoding_gamma);
        let (bp, wp) = luminance_points(px, w, h, enc);
        let (xw, xb) = whites_blacks::amounts(p.whites.clamp(-100.0, 100.0), p.blacks.clamp(-100.0, 100.0), bp, wp);
        whites_blacks::apply(px, w, xw, xb, enc);
    }
}

/// Stages 4–9: presence, curves, colour, detail and effects, then the range kept.
fn finish(px: &mut [[f32; 4]], w: usize, h: usize, p: &CameraRaw, float: bool) {
    let ps = p.pixel_scale.clamp(0.01, 1.0);
    // 4–7: per-pixel colour stages.
    let need_color = p.vibrance != 0.0 || p.saturation != 0.0 || !p.curves_neutral() || !p.hsl_neutral() || !p.grade_neutral() || p.monochrome;
    if need_color {
        let par = (!(p.curve_highlights == 0.0 && p.curve_lights == 0.0 && p.curve_darks == 0.0 && p.curve_shadows == 0.0)).then(|| parametric_lut(p, 1024));
        let pts = |c: &Vec<[f32; 2]>| (!is_linear(c)).then(|| curve_lut(c, 1024));
        let (master, cr, cg, cb) = (pts(&p.point_curve), pts(&p.point_curve_red), pts(&p.point_curve_green), pts(&p.point_curve_blue));
        let hsl = !p.hsl_neutral() && !p.monochrome;
        let grade = !p.grade_neutral();
        let curved = par.is_some() || master.is_some() || cr.is_some() || cg.is_some() || cb.is_some();
        let refine = p.curve_refine_saturation.clamp(0.0, 100.0) / 100.0;
        let wheel_tint = |wh: &Wheel| -> [f32; 3] {
            let c = hsv_to_rgb(wh.hue, 1.0, 1.0);
            let l = luma(c);
            [c[0] - l, c[1] - l, c[2] - l].map(|v| v * wh.sat / 100.0 * 0.3)
        };
        let tints = [wheel_tint(&p.grade_shadows), wheel_tint(&p.grade_midtones), wheel_tint(&p.grade_highlights), wheel_tint(&p.grade_global)];
        let blend = 0.15 + p.grade_blending.clamp(0.0, 100.0) / 100.0 * 0.35;
        let bal = p.grade_balance.clamp(-100.0, 100.0) / 100.0 * 0.25;
        par_rows(px, w, 1, |_, row| {
            for q in row.iter_mut() {
                let mut c = [q[0], q[1], q[2]];
                if p.vibrance != 0.0 || p.saturation != 0.0 {
                    let l = luma(c);
                    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
                    let sat = if mx > 1e-6 { (mx - mn) / mx } else { 0.0 };
                    // Vibrance protects skin-ish hues (oranges) a little, like ACR.
                    let hue = rgb_to_hsv(c)[0];
                    let skin = (1.0 - ((hue - 25.0).abs() / 25.0)).clamp(0.0, 1.0) * 0.5;
                    let k = 1.0 + p.saturation / 100.0 + p.vibrance / 100.0 * (1.0 - sat) * (1.0 - skin);
                    c = c.map(|v| l + (v - l) * k.max(0.0));
                }
                let uncurved = c;
                if let Some(lut) = &par {
                    c = c.map(|v| lut_eval(lut, v));
                }
                if let Some(lut) = &master {
                    c = c.map(|v| lut_eval(lut, v));
                }
                for (ch, lut) in [&cr, &cg, &cb].iter().enumerate() {
                    if let Some(lut) = lut {
                        c[ch] = lut_eval(lut, c[ch]);
                    }
                }
                if curved && refine < 1.0 {
                    // Refine Saturation: the colours' spread around their grey, scaled with the
                    // grey as the curve moved it, instead of all the spread the curve made.
                    let (l0, l1) = (luma(uncurved), luma(c));
                    let scale = if l0 > 1e-5 { l1 / l0 } else { 1.0 };
                    for ch in 0..3 {
                        let kept = (uncurved[ch] - l0) * scale;
                        c[ch] = l1 + kept + (c[ch] - l1 - kept) * refine;
                    }
                }
                if p.monochrome {
                    c = gray_mix(c, &p.gray_mix);
                } else if hsl {
                    let [hh, s, v] = rgb_to_hsv(c.map(|x| x.max(0.0)));
                    if s > 1e-4 {
                        let wts = band_weights(hh);
                        let (mut dh, mut ds, mut dl) = (0.0f32, 0.0f32, 0.0f32);
                        for k in 0..8 {
                            dh += wts[k] * p.hsl_hue[k];
                            ds += wts[k] * p.hsl_sat[k];
                            dl += wts[k] * p.hsl_lum[k];
                        }
                        let nh = hh + dh / 100.0 * 30.0;
                        let ns = (s * (1.0 + ds / 100.0)).clamp(0.0, 1.0);
                        let mut out = hsv_to_rgb(nh, ns, v);
                        // Keep the luminance, then apply the luminance shift weighted by saturation.
                        let (l0, l1) = (luma(c), luma(out));
                        if l1 > 1e-6 {
                            out = out.map(|x| x * l0 / l1);
                        }
                        let k = 2f32.powf(dl / 100.0 * 1.0 * s);
                        c = out.map(|x| x * k);
                    }
                }
                if grade {
                    let l = luma(c).clamp(0.0, 1.0);
                    let sh = 1.0 - smoothstep(0.0, 0.5 + bal + blend, l);
                    let hi = smoothstep(0.5 + bal - blend, 1.0, l);
                    let mid = (1.0 - sh - hi).max(0.0);
                    let masks = [sh, mid, hi, 1.0];
                    let lums = [p.grade_shadows.lum, p.grade_midtones.lum, p.grade_highlights.lum, p.grade_global.lum];
                    for k in 0..4 {
                        for ch in 0..3 {
                            c[ch] += masks[k] * tints[k][ch];
                        }
                        if lums[k] != 0.0 {
                            let f = 2f32.powf(masks[k] * lums[k] / 100.0 * 0.6);
                            c = c.map(|x| x * f);
                        }
                    }
                }
                q[0] = c[0];
                q[1] = c[1];
                q[2] = c[2];
            }
        });
    }
    // 8. Detail.
    if p.noise_luminance > 0.0 || p.noise_color > 0.0 || p.sharpen_amount > 0.0 {
        detail(px, w, h, p, ps);
    }
    // 9. Effects.
    if p.vignette_amount != 0.0 {
        vignette(px, w, h, p);
    }
    if p.grain_amount > 0.0 {
        let cell = 1.0 + p.grain_size / 25.0;
        let rough = p.grain_roughness / 100.0;
        let amt = p.grain_amount / 100.0 * 0.12;
        let seed = p.seed as u64;
        let pscale = 1.0 / ps;
        par_rows(px, w, 1, |y, row| {
            for (x, q) in row.iter_mut().enumerate() {
                // Sample in full-resolution coordinates so previews match the result.
                let (fx, fy) = (x as f32 * pscale / cell, y as f32 * pscale / cell);
                let n = value_noise(fx, fy, seed) * (1.0 - rough) + (value_noise(fx * 2.3, fy * 2.3, seed + 7) - 0.0) * rough;
                let l = luma([q[0], q[1], q[2]]).clamp(0.0, 1.0);
                let g = (n - 0.5) * 2.0 * amt * (0.4 + 2.4 * l * (1.0 - l));
                for c in 0..3 {
                    q[c] += g;
                }
            }
        });
    }
    if !float {
        par_rows(px, w, 1, |_, row| {
            for q in row.iter_mut() {
                for c in 0..3 {
                    q[c] = q[c].clamp(0.0, 1.0);
                }
            }
        });
    }
}

/// The image's black and white points as sRGB-encoded grey levels: the 3rd percentile of the
/// luminance and the 99.5th of the brightest channel, on a copy at most 256 pixels across (so a
/// preview proxy and the full image agree).
fn luminance_points(px: &[[f32; 4]], w: usize, h: usize, enc: Encoding) -> (f32, f32) {
    let f = w.max(h).div_ceil(256).max(1);
    let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
    let curves = enc.curves();
    // Rows of cells in parallel; each cell adds its pixels in the same order as one pass would.
    let rows = crate::photo_util::par_map(sh, |sy| {
        let mut sum = vec![[0.0f64; 4]; sw];
        for y in sy * f..((sy + 1) * f).min(h) {
            for (x, q) in px[y * w..(y + 1) * w].iter().enumerate() {
                let s = &mut sum[x / f];
                for c in 0..3 {
                    s[c] += f64::from(curves.decode(q[c]).clamp(0.0, 1.0));
                }
                s[3] += 1.0;
            }
        }
        sum
    });
    let cells: Vec<[f32; 3]> = rows.iter().flatten().filter(|s| s[3] > 0.0).map(|s| [0, 1, 2].map(|c| (s[c] / s[3]) as f32)).collect();
    if cells.is_empty() {
        return (0.0, 1.0);
    }
    let pct = |mut v: Vec<f32>, q: f32| {
        v.sort_by(f32::total_cmp);
        v[((v.len() - 1) as f32 * q).round() as usize]
    };
    let bp = pct(cells.iter().map(|c| luma(*c)).collect(), 0.03);
    let wp = pct(cells.iter().map(|c| c[0].max(c[1]).max(c[2])).collect(), 0.995);
    (linear_to_srgb(bp), linear_to_srgb(wp))
}

/// Bilinear sample of a straight RGBA buffer at `(x, y)` (pixel centres at integers), edges
/// repeated.
fn sample_rgba(src: &[[f32; 4]], w: usize, h: usize, x: f32, y: f32) -> [f32; 4] {
    let x = if x.is_finite() { x.clamp(0.0, (w - 1) as f32) } else { 0.0 };
    let y = if y.is_finite() { y.clamp(0.0, (h - 1) as f32) } else { 0.0 };
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let (a, b, c, d) = (src[y0 * w + x0], src[y0 * w + x1], src[y1 * w + x0], src[y1 * w + x1]);
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bottom = c[k] + (d[k] - c[k]) * tx;
        top + (bottom - top) * ty
    })
}

/// Stage 0, the Optics panel: manual distortion, defringe and lens vignetting, all measured on
/// the frame (radius 1 at the corners) so a proxy and the full image agree.
fn optics(px: &mut [[f32; 4]], w: usize, h: usize, p: &CameraRaw) {
    if w == 0 || h == 0 || px.len() != w * h {
        return;
    }
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let diag = cx.hypot(cy).max(1e-3);
    if p.lens_distortion != 0.0 {
        // A radial model: each pixel samples the source at its radius scaled by 1 − k r², so
        // positive amounts push the frame's edges out (straightening barrel distortion).
        let k = p.lens_distortion.clamp(-100.0, 100.0) / 100.0 * 0.25;
        let src = px.to_vec();
        par_rows(px, w, 1, |y, row| {
            for (x, q) in row.iter_mut().enumerate() {
                let (dx, dy) = ((x as f32 + 0.5 - cx) / diag, (y as f32 + 0.5 - cy) / diag);
                let f = 1.0 - k * (dx * dx + dy * dy);
                *q = sample_rgba(&src, w, h, cx + dx * f * diag - 0.5, cy + dy * f * diag - 0.5);
            }
        });
    }
    let fringes: Vec<(f32, f32, f32)> = [(p.defringe_purple_amount, p.defringe_purple_hue, 200.0, 340.0), (p.defringe_green_amount, p.defringe_green_hue, 40.0, 180.0)]
        .into_iter()
        .filter(|f| f.0 > 0.0)
        .map(|(amount, range, lo, hi)| {
            let (a, b) = (range[0].clamp(0.0, 100.0).min(range[1].clamp(0.0, 100.0)), range[1].clamp(0.0, 100.0).max(range[0].clamp(0.0, 100.0)));
            (amount.clamp(0.0, 20.0) / 20.0, lo + (hi - lo) * a / 100.0, lo + (hi - lo) * b / 100.0)
        })
        .collect();
    if !fringes.is_empty() {
        // Fringes are coloured edges: pixels of those hues next to strong luminance edges lose
        // their colour. The edges are the luminance gradient spread over about two pixels.
        let ps = p.pixel_scale.clamp(0.01, 1.0);
        let l: Vec<f32> = crate::photo_util::par_map(px.len(), |i| luma([px[i][0], px[i][1], px[i][2]]));
        let grad: Vec<f32> = crate::photo_util::par_map(w * h, |i| {
            let (x, y) = (i % w, i / w);
            let gx = l[y * w + (x + 1).min(w - 1)] - l[y * w + x.saturating_sub(1)];
            let gy = l[(y + 1).min(h - 1) * w + x] - l[y.saturating_sub(1) * w + x];
            gx.hypot(gy)
        });
        let edge = gauss(&grad, w, h, (2.0 * ps).max(0.5));
        par_rows(px, w, 1, |y, row| {
            for (x, q) in row.iter_mut().enumerate() {
                let e = smoothstep(0.02, 0.12, edge[y * w + x]);
                if e <= 0.0 {
                    continue;
                }
                let c = [q[0], q[1], q[2]];
                let [hue, sat, _] = rgb_to_hsv(c.map(|v| v.max(0.0)));
                let k = fringes.iter().map(|(amount, a, b)| amount * smoothstep(a - 10.0, *a, hue) * (1.0 - smoothstep(*b, b + 10.0, hue))).fold(0.0f32, f32::max)
                    * e
                    * smoothstep(0.05, 0.25, sat);
                let lum = luma(c);
                for ch in 0..3 {
                    q[ch] = lum + (q[ch] - lum) * (1.0 - k.clamp(0.0, 1.0));
                }
            }
        });
    }
    if p.lens_vignette_amount != 0.0 {
        let amount = p.lens_vignette_amount.clamp(-100.0, 100.0) / 100.0;
        let start = 0.8 * (1.0 - p.lens_vignette_midpoint.clamp(0.0, 100.0) / 100.0);
        let curves = Encoding::of(p.encoding_gamma).curves();
        par_rows(px, w, 1, |y, row| {
            for (x, q) in row.iter_mut().enumerate() {
                let r = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy) / diag;
                let m = smoothstep(start, 1.0, r);
                if m <= 0.0 {
                    continue;
                }
                let gain = 2f32.powf(amount * 1.5 * m * m);
                for c in 0..3 {
                    q[c] = curves.encode(curves.decode(q[c]) * gain);
                }
            }
        });
    }
}

/// Stage 0, the Calibration panel: the primaries' hue and saturation as a matrix on linear light
/// (see [`calibration_matrix`]), then the shadows' tint (green to magenta).
fn calibrate(px: &mut [[f32; 4]], w: usize, p: &CameraRaw) {
    if p.calibration_neutral() {
        return;
    }
    let primaries = [(p.red_hue, p.red_saturation), (p.green_hue, p.green_saturation), (p.blue_hue, p.blue_saturation)];
    let matrix = primaries.iter().any(|(h, s)| *h != 0.0 || *s != 0.0).then(|| calibration_matrix(primaries)).flatten();
    let tint = p.shadow_tint.clamp(-100.0, 100.0) / 100.0 * 0.12;
    let srgb = Encoding(0.0).curves();
    par_rows(px, w, 1, |_, row| {
        for q in row.iter_mut() {
            let mut lin = [0, 1, 2].map(|c| srgb.decode(q[c]));
            if let Some(m) = &matrix {
                lin = std::array::from_fn(|r| m[r][0] * lin[0] + m[r][1] * lin[1] + m[r][2] * lin[2]);
            }
            if tint != 0.0 {
                let shade = 1.0 - smoothstep(0.0, 0.5, srgb.encode(luma(lin).clamp(0.0, 1.0)));
                let k = tint * shade;
                lin = [lin[0] * (1.0 + k * 0.5), lin[1] * (1.0 - k), lin[2] * (1.0 + k * 0.5)];
            }
            for c in 0..3 {
                q[c] = srgb.encode(lin[c].max(0.0));
            }
        }
    });
}

/// The Calibration matrix (rows act on linear RGB) for the primaries' `(hue, saturation)`
/// sliders: each primary's colour around its own grey is turned about the grey axis (positive hue
/// toward the next hue: red to yellow, green to cyan, blue to magenta; up to 25°) and scaled
/// (saturation ±100 → 0.4 … 1.6), its luminance kept; then the three are scaled so white stays
/// white. `None` when that is impossible (the primaries no longer span the colours).
fn calibration_matrix(primaries: [(f32, f32); 3]) -> Option<[[f32; 3]; 3]> {
    let weights = [0.2126f32, 0.7152, 0.0722];
    let n = 1.0 / 3f32.sqrt();
    let mut cols = [[0.0f32; 3]; 3];
    for (k, (hue, sat)) in primaries.iter().enumerate() {
        let l = weights[k];
        let mut v = [-l; 3];
        v[k] += 1.0;
        let (sin, cos) = (hue.clamp(-100.0, 100.0) / 100.0 * 25f32.to_radians()).sin_cos();
        // Rodrigues' rotation about the unit grey axis (n, n, n).
        let cross = [n * (v[2] - v[1]), n * (v[0] - v[2]), n * (v[1] - v[0])];
        let along = n * (v[0] + v[1] + v[2]) * (1.0 - cos);
        let scale = 1.0 + sat.clamp(-100.0, 100.0) / 100.0 * 0.6;
        let col: [f32; 3] = std::array::from_fn(|c| l + (v[c] * cos + cross[c] * sin + n * along) * scale);
        let dl = l - luma(col);
        cols[k] = col.map(|x| x + dl);
    }
    // Scale the columns so that (1, 1, 1) maps to itself: solve cols · s = 1 (Cramer's rule).
    let m = |r: usize, c: usize| cols[c][r];
    let det3 = |a: [[f32; 3]; 3]| a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0]) + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    let a: [[f32; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| m(r, c)));
    let det = det3(a);
    if !det.is_finite() || det.abs() < 1e-6 {
        return None;
    }
    let s: [f32; 3] = std::array::from_fn(|k| {
        let mut b = a;
        for row in b.iter_mut() {
            row[k] = 1.0;
        }
        det3(b) / det
    });
    if s.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return None;
    }
    Some(std::array::from_fn(|r| std::array::from_fn(|c| m(r, c) * s[c])))
}

/// Temperature and Tint (−100..=100) that make `lin` (linear RGB, before any setting) grey: the
/// inverse of [`white_balance_gains`]. `None` for a colour too dark or odd to judge.
fn neutralize_linear(lin: [f32; 3]) -> Option<(f32, f32)> {
    if lin.iter().any(|v| !v.is_finite() || *v <= 1e-4) {
        return None;
    }
    // Gains g with g·lin grey: g_r / g_g = a, g_b / g_g = b.
    let (a, b) = (lin[1] / lin[0], lin[1] / lin[2]);
    let tint = 4.0 * (1.0 - 2.0 / (a + b));
    let temperature = 2.0 * (a - b) / (0.7 * (a + b));
    let round = |v: f32| (v * 100.0).clamp(-100.0, 100.0).round();
    Some((round(temperature), round(tint)))
}

/// The White Balance tool: Temperature and Tint (−100..=100) that make the colour `c` (as the
/// image holds it, display-encoded) neutral. `None` for a colour too dark or clipped to judge.
pub fn neutralize(c: [f32; 3]) -> Option<(f32, f32)> {
    if c.iter().any(|v| !v.is_finite() || *v >= 0.995) {
        return None;
    }
    neutralize_linear(c.map(srgb_to_linear))
}

/// Auto white balance: Temperature and Tint that make the image's near-neutral colours grey (a
/// grey world weighted toward the less saturated pixels, skipping clipped and very dark ones).
pub fn auto_white_balance(px: &[[f32; 4]]) -> Option<(f32, f32)> {
    let step = (px.len() / 400_000).max(1);
    let mut sum = [0.0f64; 3];
    for q in px.iter().step_by(step) {
        if q.iter().any(|v| !v.is_finite()) || q[3] <= 0.0 {
            continue;
        }
        let c = [q[0], q[1], q[2]];
        let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
        if mx >= 0.98 || luma(c) < 0.05 {
            continue;
        }
        let weight = f64::from((1.0 - (mx - mn) / mx.max(1e-6)).powi(4));
        for (s, v) in sum.iter_mut().zip(c) {
            *s += weight * f64::from(srgb_to_linear(v));
        }
    }
    neutralize_linear(sum.map(|v| v as f32))
}

/// The Light panel's Auto: settings that spread the image's tones (see [`auto_tone`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AutoTone {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
}

/// Auto tone from the image's luminance percentiles: Exposure takes the median toward middle
/// grey (half way, in stops), Highlights pulls down a bright top, Shadows lifts a dark bottom,
/// Whites and Blacks bring the ends near white and black, and Contrast widens a narrow middle.
/// A heuristic in Camera Raw's spirit, not its (learned) model.
pub fn auto_tone(px: &[[f32; 4]]) -> AutoTone {
    let step = (px.len() / 400_000).max(1);
    let mut l: Vec<f32> =
        px.iter().step_by(step).filter(|q| q[3] > 0.0 && q.iter().all(|v| v.is_finite())).map(|q| luma([q[0], q[1], q[2]]).clamp(0.0, 1.0)).collect();
    if l.len() < 16 {
        return AutoTone::default();
    }
    l.sort_by(f32::total_cmp);
    let pct = |q: f32| l[((l.len() - 1) as f32 * q).round() as usize];
    let exposure = ((srgb_to_linear(0.46).log2() - srgb_to_linear(pct(0.5).max(0.01)).log2()) * 0.5).clamp(-2.0, 2.0);
    let gain = exposure.exp2();
    let at = |q: f32| linear_to_srgb((srgb_to_linear(pct(q)) * gain).min(1.0));
    let highlights = -((at(0.95) - 0.75) / 0.25).clamp(0.0, 1.0) * 70.0;
    let shadows = ((0.25 - at(0.05)) / 0.25).clamp(0.0, 1.0) * 60.0;
    let whites = ((0.97 - at(0.995)) * 120.0).clamp(-40.0, 50.0);
    let blacks = ((0.03 - at(0.005)) * 150.0).clamp(-50.0, 20.0);
    let contrast = ((0.35 - (at(0.75) - at(0.25))) * 100.0).clamp(-20.0, 40.0);
    AutoTone {
        exposure: (exposure * 20.0).round() / 20.0,
        contrast: contrast.round(),
        highlights: highlights.round(),
        shadows: shadows.round(),
        whites: whites.round(),
        blacks: blacks.round(),
    }
}

fn value_noise(x: f32, y: f32, seed: u64) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (tx, ty) = (x - xi, y - yi);
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let h = |dx: i64, dy: i64| hash01(xi as i64 + dx, yi as i64 + dy, seed);
    let top = h(0, 0) + (h(1, 0) - h(0, 0)) * sx;
    let bot = h(0, 1) + (h(1, 1) - h(0, 1)) * sx;
    top + (bot - top) * sy
}

fn dehaze(px: &mut [[f32; 4]], w: usize, h: usize, amount: f32) {
    // Transmission on a ≤ 768 px proxy, upsampled.
    let k = w.max(h).div_ceil(768).max(1);
    let (sw, sh) = (w.div_ceil(k), h.div_ceil(k));
    // Rows of blocks in parallel, each block summed in the order one pass would.
    let small: Vec<[f32; 3]> = crate::photo_util::par_map(sh, |by| {
        let mut sum = vec![[0.0f32; 3]; sw];
        let mut cnt = vec![0f32; sw];
        for y in by * k..((by + 1) * k).min(h) {
            for (x, q) in px[y * w..(y + 1) * w].iter().enumerate() {
                for c in 0..3 {
                    sum[x / k][c] += q[c].clamp(0.0, 1.0);
                }
                cnt[x / k] += 1.0;
            }
        }
        sum.iter().zip(&cnt).map(|(s, c)| s.map(|v| v / c.max(1.0))).collect::<Vec<_>>()
    })
    .concat();
    let r = (sw.max(sh) / 60).max(2);
    let min_filter = |v: &[f32]| -> Vec<f32> {
        let a = crate::photo_util::par_map(sw * sh, |i| {
            let (x, y) = (i % sw, i / sw);
            let (x0, x1) = (x.saturating_sub(r), (x + r).min(sw - 1));
            (x0..=x1).map(|xx| v[y * sw + xx]).fold(f32::MAX, f32::min)
        });
        crate::photo_util::par_map(sw * sh, |i| {
            let (x, y) = (i % sw, i / sw);
            let (y0, y1) = (y.saturating_sub(r), (y + r).min(sh - 1));
            (y0..=y1).map(|yy| a[yy * sw + x]).fold(f32::MAX, f32::min)
        })
    };
    let dark = min_filter(&small.iter().map(|c| c[0].min(c[1]).min(c[2])).collect::<Vec<_>>());
    // Atmospheric light: mean colour of the brightest 0.1 % of the dark channel (selected, not
    // sorted: the order among them doesn't matter, and ties are broken by index as a stable
    // sort would).
    let mut idx: Vec<usize> = (0..sw * sh).collect();
    let top = (sw * sh / 1000).max(1);
    idx.select_nth_unstable_by(top - 1, |&a, &b| dark[b].total_cmp(&dark[a]).then(a.cmp(&b)));
    let mut air = [0.0f32; 3];
    for &i in &idx[..top] {
        for c in 0..3 {
            air[c] += small[i][c] / top as f32;
        }
    }
    let air = air.map(|v| v.max(0.2));
    let norm_dark = min_filter(&small.iter().map(|c| (c[0] / air[0]).min(c[1] / air[1]).min(c[2] / air[2])).collect::<Vec<_>>());
    let t_raw: Vec<f32> = norm_dark.iter().map(|d| 1.0 - 0.95 * d).collect();
    let guide: Vec<f32> = small.iter().map(|c| luma(*c)).collect();
    let t = guided(&guide, &t_raw, sw, sh, r * 2, 1e-3);
    let ks = k as f32;
    par_rows(px, w, 1, |y, row| {
        for (x, q) in row.iter_mut().enumerate() {
            let tv = crate::photo_util::bilinear(&t, sw, sh, 1, 0, (x as f32 + 0.5) / ks - 0.5, (y as f32 + 0.5) / ks - 0.5).clamp(0.1, 1.0);
            for c in 0..3 {
                let i = q[c];
                let j = (i - air[c]) / tv + air[c];
                q[c] = if amount >= 0.0 { i + (j - i) * amount } else { i + (air[c] - i) * (-amount) * 0.5 * (1.0 - tv * 0.5) };
            }
        }
    });
}

fn detail(px: &mut [[f32; 4]], w: usize, h: usize, p: &CameraRaw, ps: f32) {
    use crate::photo_util::par_map as par;
    // Work in Y + chroma (BT.709 luma, B−Y, R−Y).
    let mut y: Vec<f32> = par(px.len(), |i| luma([px[i][0], px[i][1], px[i][2]]));
    let mut cb: Vec<f32> = par(px.len(), |i| px[i][2] - y[i]);
    let mut cr: Vec<f32> = par(px.len(), |i| px[i][0] - y[i]);
    if p.noise_luminance > 0.0 {
        let n = p.noise_luminance / 100.0;
        let r = ((1.0 + 3.0 * n) * ps).round().max(1.0) as usize;
        let smooth = guided(&y, &y, w, h, r, (n * 0.06).powi(2) + 1e-6);
        let keep = p.noise_luminance_detail / 100.0 * 0.5;
        // Contrast: some of the smoothed image's local contrast back, over twice the radius.
        let contrast = p.noise_luminance_contrast.clamp(0.0, 100.0) / 100.0;
        let wide = (contrast > 0.0).then(|| gauss(&smooth, w, h, 2.0 * r as f32));
        y = par(w * h, |i| smooth[i] + (y[i] - smooth[i]) * keep + wide.as_ref().map_or(0.0, |b| contrast * 0.6 * (smooth[i] - b[i])));
    }
    if p.noise_color > 0.0 {
        // Smoothness 50 is the plain blur; less narrows it, more also averages the blotches of
        // colour over three times the area.
        let smooth = (p.noise_color_smoothness.clamp(0.0, 100.0) - 50.0) / 50.0;
        let sigma = (p.noise_color / 100.0 * 6.0 * ps * (1.0 + 0.3 * smooth.min(0.0))).max(0.3);
        let (bcb, bcr) = (gauss(&cb, w, h, sigma), gauss(&cr, w, h, sigma));
        let wide = (smooth > 0.0).then(|| (gauss(&bcb, w, h, sigma * 3.0), gauss(&bcr, w, h, sigma * 3.0)));
        let keep = p.noise_color_detail / 100.0 * 0.3;
        let base = |b: &[f32], wide: Option<&Vec<f32>>, i: usize| wide.map_or(b[i], |v| b[i] + (v[i] - b[i]) * smooth * 0.5);
        cb = par(w * h, |i| base(&bcb, wide.as_ref().map(|v| &v.0), i) + (cb[i] - bcb[i]) * keep);
        cr = par(w * h, |i| base(&bcr, wide.as_ref().map(|v| &v.1), i) + (cr[i] - bcr[i]) * keep);
    }
    if p.sharpen_amount > 0.0 {
        let sigma = (p.sharpen_radius.clamp(0.5, 3.0) * ps).max(0.3);
        let blur = gauss(&y, w, h, sigma);
        // Masking: an edge mask from the gradient of a smoothed luminance.
        let mask: Option<Vec<f32>> = (p.sharpen_masking > 0.0).then(|| {
            let s = gauss(&y, w, h, 1.0 * ps.max(0.5));
            let t = p.sharpen_masking / 100.0 * 0.08;
            crate::photo_util::par_map(w * h, |i| {
                let (x, yy) = (i % w, i / w);
                let gx = s[yy * w + (x + 1).min(w - 1)] - s[yy * w + x.saturating_sub(1)];
                let gy = s[(yy + 1).min(h - 1) * w + x] - s[yy.saturating_sub(1) * w + x];
                smoothstep(t * 0.5, t + 1e-4, gx.hypot(gy))
            })
        });
        let amt = p.sharpen_amount / 100.0 * 1.2;
        let det = p.sharpen_detail / 100.0;
        y = par(w * h, |i| {
            let hp = y[i] - blur[i];
            // Low Detail damps large (halo-producing) differences.
            let limit = 0.03 + det * 0.3;
            let hp = hp.clamp(-limit, limit) * (0.5 + 0.5 * det);
            let m = mask.as_ref().map_or(1.0, |m| m[i]);
            y[i] + amt * hp * m
        });
    }
    par_rows(px, w, 1, |row_y, row| {
        for (x, q) in row.iter_mut().enumerate() {
            let i = row_y * w + x;
            let (l, b, r) = (y[i], cb[i], cr[i]);
            let (rr, bb) = (r + l, b + l);
            let g = (l - 0.2126 * rr - 0.0722 * bb) / 0.7152;
            q[0] = rr;
            q[1] = g;
            q[2] = bb;
        }
    });
}

fn vignette(px: &mut [[f32; 4]], w: usize, h: usize, p: &CameraRaw) {
    let amt = p.vignette_amount / 100.0;
    let mid = p.vignette_midpoint.clamp(0.0, 100.0) / 100.0;
    let feather = p.vignette_feather.clamp(0.0, 100.0) / 100.0;
    let round = p.vignette_roundness.clamp(-100.0, 100.0) / 100.0;
    let hl = p.vignette_highlights.clamp(0.0, 100.0) / 100.0;
    let style = p.vignette_style.as_str();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    // Roundness: 0 follows the frame's aspect, +1 a circle, −1 a rounded rectangle.
    let (ax, ay) = if round >= 0.0 {
        let r = cx.min(cy) + (cx.max(cy) - cx.min(cy)) * (1.0 - round);
        (if cx >= cy { r } else { cx }, if cy > cx { r } else { cy })
    } else {
        (cx, cy)
    };
    let pw = 2.0 + (-round).max(0.0) * 6.0;
    let inner = 1.0 - mid * 0.8;
    let edge0 = inner - feather * 0.5 * inner;
    let edge1 = inner + feather * 0.5 * (1.4 - inner) + 0.02;
    par_rows(px, w, 1, |y, row| {
        for (x, q) in row.iter_mut().enumerate() {
            let (dx, dy) = (((x as f32 + 0.5 - cx) / ax).abs(), ((y as f32 + 0.5 - cy) / ay).abs());
            let r = if pw == 2.0 { dx.hypot(dy) } else { (dx.powf(pw) + dy.powf(pw)).powf(1.0 / pw) };
            let m = smoothstep(edge0, edge1, r);
            if m <= 0.0 {
                continue;
            }
            let l = luma([q[0], q[1], q[2]]).clamp(0.0, 1.0);
            match style {
                "paintOverlay" => {
                    let target = if amt < 0.0 { 0.0 } else { 1.0 };
                    let k = m * amt.abs();
                    for c in 0..3 {
                        q[c] += (target - q[c]) * k;
                    }
                }
                _ => {
                    let mut k = 2f32.powf(amt * 2.0 * m);
                    if amt < 0.0 && hl > 0.0 {
                        // Highlights: let bright areas punch through a darkening vignette.
                        let protect = smoothstep(0.6, 1.0, l) * hl;
                        k += (1.0 - k) * protect;
                    }
                    if style == "colorPriority" {
                        let nl = l * k;
                        for c in 0..3 {
                            q[c] += nl - l;
                        }
                    } else {
                        for c in 0..3 {
                            q[c] *= k;
                        }
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_curves_are_validated_but_saved_curves_still_load_and_render() {
        for field in ["pointCurve", "pointCurveRed", "pointCurveGreen", "pointCurveBlue"] {
            for points in [
                serde_json::json!([[128, 128]]),
                serde_json::json!([[255, 255], [0, 0]]),
                serde_json::json!([[0, 0], [0, 255]]),
                serde_json::json!([[0, -1], [255, 255]]),
                serde_json::json!([[0, 0], [256, 255]]),
                serde_json::json!([[0, 0], [1e30, 255]]),
                serde_json::json!((0..17).map(|i| [i * 15, i * 15]).collect::<Vec<_>>()),
            ] {
                let p: CameraRaw = serde_json::from_value(serde_json::json!({field: points})).unwrap();
                assert!(p.validate().is_err(), "{field}: {points}");
            }
            for points in
                [serde_json::json!([]), serde_json::json!([[0, 0], [255, 255]]), serde_json::json!((0..16).map(|i| [i * 17, i * 17]).collect::<Vec<_>>())]
            {
                let p: CameraRaw = serde_json::from_value(serde_json::json!({field: points})).unwrap();
                assert!(p.validate().is_ok(), "{field}: {points}");
            }
        }
        assert_eq!(serde_json::from_str::<CameraRaw>("{}").unwrap(), CameraRaw::default());
        // An older editor could stack two points on one input and exceed 16 points; such a
        // saved Smart Filter must still develop instead of silently disappearing.
        let mut legacy: Vec<[f32; 2]> = (0..20).map(|i| [i as f32 * 12.0, i as f32 * 12.0]).collect();
        legacy.insert(5, [60.0, 200.0]);
        let p: CameraRaw = serde_json::from_value(serde_json::json!({"pointCurve": legacy})).unwrap();
        assert!(p.validate().is_err());
        let edited = CameraRaw { exposure: 1.0, ..p.clone() };
        assert!(edited.validate_changes(&p).is_ok(), "an unchanged stored curve stays editable");
        let broken = CameraRaw { point_curve: vec![[10.0, 10.0]], ..p.clone() };
        assert!(broken.validate_changes(&p).is_err(), "a changed curve is validated");
        let mut px = img(4, 4);
        develop(&mut px, 4, 4, &p, false);
        assert!(px.iter().flatten().all(|v| v.is_finite()));
    }

    fn img(w: usize, h: usize) -> Vec<[f32; 4]> {
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
                [0.2 + 0.6 * x, 0.3 + 0.4 * y, 0.5 - 0.3 * x * y, 1.0]
            })
            .collect()
    }

    fn mean(px: &[[f32; 4]], c: usize) -> f32 {
        px.iter().map(|q| q[c]).sum::<f32>() / px.len() as f32
    }

    #[test]
    fn defaults_are_identity() {
        let mut a = img(40, 30);
        let b = a.clone();
        develop(&mut a, 40, 30, &CameraRaw::default(), false);
        assert_eq!(a, b);
    }

    #[test]
    fn basic_panel_moves_the_right_way() {
        let (w, h) = (64, 48);
        let base = img(w, h);
        let run = |p: CameraRaw| {
            let mut a = base.clone();
            develop(&mut a, w, h, &p, false);
            a
        };
        let warm = run(CameraRaw { temperature: 50.0, ..Default::default() });
        assert!(mean(&warm, 0) > mean(&base, 0) && mean(&warm, 2) < mean(&base, 2));
        let bright = run(CameraRaw { exposure: 1.0, ..Default::default() });
        assert!(mean(&bright, 1) > mean(&base, 1) + 0.1);
        let shadows = run(CameraRaw { shadows: 100.0, ..Default::default() });
        let dark_px = |a: &[[f32; 4]]| a[0][1];
        assert!(dark_px(&shadows) > dark_px(&base));
        let hl = run(CameraRaw { highlights: -100.0, ..Default::default() });
        assert!(hl[w * h - 1][0] < base[w * h - 1][0]);
        let con = run(CameraRaw { contrast: 100.0, ..Default::default() });
        // More contrast: darker darks, brighter brights.
        assert!(con[0][1] < base[0][1] && con[w * h - 1][0] > base[w * h - 1][0]);
        let desat = run(CameraRaw { saturation: -100.0, ..Default::default() });
        assert!(desat.iter().all(|q| (q[0] - q[1]).abs() < 1e-4 && (q[1] - q[2]).abs() < 1e-4));
        let vib = run(CameraRaw { vibrance: 100.0, ..Default::default() });
        let spread = |q: [f32; 4]| q[0].max(q[1]).max(q[2]) - q[0].min(q[1]).min(q[2]);
        assert!(spread(vib[w * h / 2]) > spread(base[w * h / 2]));
        for p in [
            CameraRaw { clarity: 80.0, ..Default::default() },
            CameraRaw { texture: 80.0, ..Default::default() },
            CameraRaw { dehaze: 60.0, ..Default::default() },
            CameraRaw { dehaze: -60.0, ..Default::default() },
            CameraRaw { whites: 50.0, blacks: -50.0, ..Default::default() },
        ] {
            let out = run(p.clone());
            assert!(out.iter().all(|q| q[..3].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))), "{p:?}");
            assert_ne!(out, base);
        }
    }

    #[test]
    fn curves_hsl_grading_detail_effects() {
        // Monotone cubic LUT through the points.
        let lut = curve_lut(&[[0.0, 0.0], [64.0, 40.0], [192.0, 220.0], [255.0, 255.0]], 256);
        assert!((lut[64] - 40.0 / 255.0).abs() < 0.01 && (lut[192] - 220.0 / 255.0).abs() < 0.01);
        assert!(lut.windows(2).all(|p| p[1] >= p[0]));
        // Band weights sum to one everywhere.
        for hh in (0..360).step_by(7) {
            let s: f32 = band_weights(hh as f32).iter().sum();
            assert!((s - 1.0).abs() < 1e-4, "{hh}: {s}");
        }
        let (w, h) = (48, 32);
        let reds: Vec<[f32; 4]> = vec![[0.8, 0.2, 0.2, 1.0]; w * h];
        let mut a = reds.clone();
        let mut hs = [0.0; 8];
        hs[0] = -100.0;
        develop(&mut a, w, h, &CameraRaw { hsl_sat: hs, ..Default::default() }, false);
        assert!(a[0][0] - a[0][1] < 0.1, "reds desaturated: {:?}", a[0]);
        let blues: Vec<[f32; 4]> = vec![[0.2, 0.2, 0.8, 1.0]; w * h];
        let mut b = blues.clone();
        develop(&mut b, w, h, &CameraRaw { hsl_sat: hs, ..Default::default() }, false);
        assert!((b[0][2] - 0.8).abs() < 1e-4, "blues untouched");
        // Grading: blue shadows tint dark pixels, leave bright ones nearly alone.
        let mut g: Vec<[f32; 4]> = vec![[0.1, 0.1, 0.1, 1.0], [0.95, 0.95, 0.95, 1.0]];
        develop(&mut g, 2, 1, &CameraRaw { grade_shadows: Wheel { hue: 220.0, sat: 80.0, lum: 0.0 }, ..Default::default() }, false);
        assert!(g[0][2] > g[0][0] + 0.05 && (g[1][2] - g[1][0]).abs() < 0.02, "{g:?}");
        // Point curve + parametric.
        let mut c = img(w, h);
        develop(&mut c, w, h, &CameraRaw { point_curve: vec![[0.0, 30.0], [255.0, 255.0]], curve_shadows: 50.0, ..Default::default() }, false);
        assert!(c.iter().all(|q| q[1] >= 30.0 / 255.0 - 1e-4));
        // Noise reduction reduces variance; sharpening increases it; grain is deterministic.
        let noisy: Vec<[f32; 4]> = (0..w * h)
            .map(|i| {
                let n = hash01(i as i64, 0, 3) * 0.2;
                [0.4 + n, 0.4 + n, 0.4 + n, 1.0]
            })
            .collect();
        let var = |a: &[[f32; 4]]| {
            let m = mean(a, 1);
            a.iter().map(|q| (q[1] - m).powi(2)).sum::<f32>() / a.len() as f32
        };
        let mut nr = noisy.clone();
        develop(&mut nr, w, h, &CameraRaw { noise_luminance: 80.0, ..Default::default() }, false);
        assert!(var(&nr) < var(&noisy) * 0.5);
        let mut sh = noisy.clone();
        develop(&mut sh, w, h, &CameraRaw { sharpen_amount: 100.0, ..Default::default() }, false);
        assert!(var(&sh) > var(&noisy));
        let mut g1 = img(w, h);
        let mut g2 = img(w, h);
        let gp = CameraRaw { grain_amount: 50.0, seed: 4, ..Default::default() };
        develop(&mut g1, w, h, &gp, false);
        develop(&mut g2, w, h, &gp, false);
        assert_eq!(g1, g2);
        assert_ne!(g1, img(w, h));
        // Post-crop vignette darkens corners, leaves the centre.
        let flat = vec![[0.5f32, 0.5, 0.5, 1.0]; w * h];
        let mut v = flat.clone();
        develop(&mut v, w, h, &CameraRaw { vignette_amount: -80.0, ..Default::default() }, false);
        assert!(v[0][0] < 0.35 && (v[(h / 2) * w + w / 2][0] - 0.5).abs() < 0.01, "{:?}", v[0]);
    }

    #[test]
    fn curve_tables_follow_the_exact_curves() {
        for gamma in [0.0, 1.8, 2.2] {
            let enc = Encoding::of(gamma);
            let curves = enc.curves();
            for i in 0..=20_000 {
                let v = i as f32 / 20_000.0;
                assert!((curves.decode(v) - enc.decode(v)).abs() < 2e-6, "decode {gamma} {v}");
                let l = v * ENCODE_TOP;
                assert!((curves.encode(l) - enc.encode(l)).abs() < 2e-5 * enc.encode(l).max(1.0), "encode {gamma} {l}");
            }
            // Outside the tables the exact curves answer.
            for odd in [-0.5, 1.5, 40.0, f32::NAN] {
                assert_eq!(curves.decode(odd).to_bits(), enc.decode(odd).to_bits());
            }
            for odd in [-0.5, 40.0, f32::NAN] {
                assert_eq!(curves.encode(odd).to_bits(), enc.encode(odd).to_bits());
            }
        }
    }

    #[test]
    fn band_weights_are_the_hat_functions() {
        // The former per-band formula, the reference.
        let reference = |h: f32| {
            let mut w = [0.0f32; 8];
            for k in 0..8 {
                let (c, prev, next) = (HSL_BANDS[k], HSL_BANDS[(k + 7) % 8], HSL_BANDS[(k + 1) % 8]);
                let diff = ((h - c + 540.0) % 360.0) - 180.0;
                let span = if diff >= 0.0 { (next - c).rem_euclid(360.0) } else { (c - prev).rem_euclid(360.0) };
                w[k] = (1.0 - diff.abs() / span.max(1e-3)).max(0.0);
            }
            w
        };
        for i in 0..3600 {
            let h = i as f32 / 10.0;
            let (a, b) = (band_weights(h), reference(h));
            assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4), "{h}: {a:?} vs {b:?}");
        }
        assert_eq!(band_weights(f32::NAN), [0.0; 8]);
        assert!((band_weights(-30.0)[7] - band_weights(330.0)[7]).abs() < 1e-6);
    }

    #[test]
    fn the_cache_gives_develops_result_and_drafts_come_close() {
        let (w, h) = (96, 64);
        let src: Vec<[f32; 4]> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
                let v = if (x - 0.5).abs() < 0.2 && (y - 0.5).abs() < 0.2 { 0.85 } else { 0.08 + 0.3 * x * y };
                [v, v * 0.9, v * 0.7, 1.0]
            })
            .collect();
        let plain = |p: &CameraRaw| {
            let mut a = src.clone();
            develop(&mut a, w, h, p, false);
            a
        };
        let mut cache = DevelopCache::default();
        let base = CameraRaw { shadows: 60.0, highlights: -40.0, exposure: 0.2, contrast: 20.0, whites: 15.0, ..Default::default() };
        // A series of edits as a drag makes them: each result is the plain develop's.
        for p in [
            base.clone(),
            CameraRaw { saturation: 30.0, ..base.clone() },
            CameraRaw { contrast: -10.0, ..base.clone() },
            CameraRaw { shadows: 20.0, ..base.clone() },
            CameraRaw { highlights: -80.0, ..base.clone() },
            CameraRaw { exposure: -0.4, ..base.clone() },
        ] {
            let got = develop_cached(&src, w, h, &p, false, 7, &mut cache, false);
            assert!(got.exact);
            let want = plain(&p);
            let worst = got.px.iter().flatten().zip(want.iter().flatten()).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            // Exposure only shifts what Shadows and Highlights filter: their maps are reused, equal
            // up to rounding; everything else is the same arithmetic.
            assert!(worst < 2e-4, "{p:?}: {worst}");
        }
        // A white balance drag reuses the maps (a draft), close to the exact result.
        let warm = CameraRaw { temperature: 30.0, ..base.clone() };
        let draft = develop_cached(&src, w, h, &warm, false, 7, &mut cache, true);
        assert!(!draft.exact);
        let want = plain(&warm);
        let worst = draft.px.iter().flatten().zip(want.iter().flatten()).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.03, "draft {worst}");
        let exact = develop_cached(&src, w, h, &warm, false, 7, &mut cache, false);
        assert!(exact.exact);
        // Other pixels drop what was kept.
        let other: Vec<[f32; 4]> = src.iter().map(|q| [q[0] * 0.5, q[1], q[2], 1.0]).collect();
        let got = develop_cached(&other, w, h, &base, false, 8, &mut cache, true);
        assert!(got.exact);
        // Odd input is returned as it is.
        assert_eq!(develop_cached(&src, w + 1, h, &base, false, 9, &mut cache, false).px, src);
        assert!(develop_cached(&[], 0, 0, &base, false, 9, &mut cache, false).px.is_empty());
    }

    #[test]
    fn black_and_white_mixes_by_hue_and_still_grades() {
        let (w, h) = (8, 1);
        let src: Vec<[f32; 4]> = vec![[0.8, 0.2, 0.2, 1.0], [0.2, 0.3, 0.8, 1.0], [0.5, 0.5, 0.5, 1.0], [0.1, 0.1, 0.1, 1.0], [0.9, 0.9, 0.9, 1.0], [0.2, 0.7, 0.2, 1.0], [0.6, 0.4, 0.1, 1.0], [0.0, 0.0, 0.0, 1.0]];
        let run = |p: CameraRaw| {
            let mut a = src.clone();
            develop(&mut a, w, h, &p, false);
            a
        };
        let bw = run(CameraRaw { monochrome: true, ..Default::default() });
        assert!(bw.iter().all(|q| (q[0] - q[1]).abs() < 1e-5 && (q[1] - q[2]).abs() < 1e-5), "{bw:?}");
        assert!((bw[2][0] - 0.5).abs() < 1e-4, "a grey stays");
        let mut mix = [0.0; 8];
        mix[0] = 80.0;
        let lighter_reds = run(CameraRaw { monochrome: true, gray_mix: mix, ..Default::default() });
        assert!(lighter_reds[0][0] > bw[0][0] + 0.05 && (lighter_reds[1][0] - bw[1][0]).abs() < 1e-4);
        let toned = run(CameraRaw { monochrome: true, grade_shadows: Wheel { hue: 220.0, sat: 60.0, lum: 0.0 }, ..Default::default() });
        assert!(toned[3][2] > toned[3][0] + 0.02, "split toning tints the grey: {:?}", toned[3]);
        assert!(!CameraRaw { monochrome: true, ..Default::default() }.is_identity());
    }

    #[test]
    fn calibration_turns_the_primaries_and_keeps_white() {
        let (w, h) = (4, 1);
        let src: Vec<[f32; 4]> = vec![[0.9, 0.1, 0.1, 1.0], [0.1, 0.8, 0.1, 1.0], [0.5, 0.5, 0.5, 1.0], [1.0, 1.0, 1.0, 1.0]];
        let run = |p: CameraRaw| {
            let mut a = src.clone();
            develop(&mut a, w, h, &p, false);
            a
        };
        let warm_reds = run(CameraRaw { red_hue: 100.0, ..Default::default() });
        assert!(warm_reds[0][1] > src[0][1] + 0.05, "red turns toward yellow: {:?}", warm_reds[0]);
        for p in [CameraRaw { red_hue: 80.0, green_saturation: -60.0, blue_hue: -50.0, ..Default::default() }, CameraRaw { red_saturation: 100.0, ..Default::default() }] {
            let out = run(p);
            for g in [&out[2], &out[3]] {
                assert!((g[0] - g[1]).abs() < 2e-3 && (g[1] - g[2]).abs() < 2e-3, "neutrals stay neutral: {g:?}");
            }
        }
        let richer = run(CameraRaw { red_saturation: 100.0, ..Default::default() });
        assert!(richer[0][0] - richer[0][1] > src[0][0] - src[0][1], "{:?}", richer[0]);
        let magenta = run(CameraRaw { shadow_tint: 100.0, ..Default::default() });
        let dark = vec![[0.1f32, 0.1, 0.1, 1.0]; 4];
        let mut d = dark.clone();
        develop(&mut d, 4, 1, &CameraRaw { shadow_tint: 100.0, ..Default::default() }, false);
        assert!(d[0][1] < d[0][0], "magenta shadows: {:?}", d[0]);
        assert!((magenta[3][1] - 1.0).abs() < 1e-3, "white is no shadow");
        assert!(calibration_matrix([(100.0, -100.0); 3]).is_some());
    }

    #[test]
    fn optics_and_noise_reduction_controls_act() {
        let (w, h) = (64, 48);
        let base = img(w, h);
        let run = |p: CameraRaw| {
            let mut a = base.clone();
            develop(&mut a, w, h, &p, false);
            a
        };
        let flat = vec![[0.5f32, 0.5, 0.5, 1.0]; w * h];
        let mut v = flat.clone();
        develop(&mut v, w, h, &CameraRaw { lens_vignette_amount: 100.0, ..Default::default() }, false);
        assert!(v[0][0] > 0.6 && (v[(h / 2) * w + w / 2][0] - 0.5).abs() < 1e-3, "lens vignetting lightens corners: {:?}", v[0]);
        // Distortion moves content radially and leaves the centre.
        let ring: Vec<[f32; 4]> = (0..w * h).map(|i| if (i % w) == 8 { [1.0, 1.0, 1.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] }).collect();
        let mut d = ring.clone();
        develop(&mut d, w, h, &CameraRaw { lens_distortion: 100.0, ..Default::default() }, false);
        let column = |a: &[[f32; 4]]| (0..w).max_by(|x, y| a[(h / 2) * w + x][0].total_cmp(&a[(h / 2) * w + y][0])).unwrap();
        assert!(column(&d) < 8, "barrel straightening pushes the edge content out: {}", column(&d));
        // Defringe takes the colour out of purple edges only.
        let mut fringe: Vec<[f32; 4]> = (0..w * h).map(|i| if (i % w) < w / 2 { [0.05, 0.05, 0.05, 1.0] } else { [0.95, 0.95, 0.95, 1.0] }).collect();
        for y in 0..h {
            fringe[y * w + w / 2] = [0.6, 0.2, 0.8, 1.0];
            for x in 2..16 {
                fringe[y * w + x] = [0.6, 0.2, 0.8, 1.0];
            }
        }
        let mut f = fringe.clone();
        develop(&mut f, w, h, &CameraRaw { defringe_purple_amount: 20.0, defringe_purple_hue: [0.0, 100.0], ..Default::default() }, false);
        let spread = |q: [f32; 4]| q[0].max(q[1]).max(q[2]) - q[0].min(q[1]).min(q[2]);
        assert!(spread(f[10 * w + w / 2]) < spread(fringe[10 * w + w / 2]) * 0.5, "the fringe at the edge loses its colour");
        assert!((spread(f[10 * w + 9]) - spread(fringe[10 * w + 9])).abs() < 1e-3, "a purple area away from edges keeps its colour");
        // The new noise controls change the result and stay finite; their defaults change nothing.
        let nr = CameraRaw { noise_luminance: 50.0, noise_color: 50.0, ..Default::default() };
        let plain = run(nr.clone());
        for p in [CameraRaw { noise_luminance_contrast: 80.0, ..nr.clone() }, CameraRaw { noise_color_smoothness: 100.0, ..nr.clone() }, CameraRaw { noise_color_smoothness: 0.0, ..nr.clone() }] {
            let out = run(p);
            assert!(out.iter().flatten().all(|v| v.is_finite()));
            assert_ne!(out, plain);
        }
        // Refine Saturation 0 keeps the colours' saturation through a contrasty curve.
        let curve = CameraRaw { point_curve: vec![[0.0, 0.0], [64.0, 30.0], [192.0, 225.0], [255.0, 255.0]], ..Default::default() };
        let full = run(curve.clone());
        let refined = run(CameraRaw { curve_refine_saturation: 0.0, ..curve });
        let sat = |q: [f32; 4]| (q[0].max(q[1]).max(q[2]) - q[0].min(q[1]).min(q[2])) / q[0].max(q[1]).max(q[2]).max(1e-6);
        let i = (h / 2) * w + w / 2;
        assert!((sat(refined[i]) - sat(base[i])).abs() < (sat(full[i]) - sat(base[i])).abs(), "{:?} {:?} {:?}", base[i], full[i], refined[i]);
    }

    #[test]
    fn white_balance_tools_and_auto_tone() {
        // A warm cast: the tool's settings neutralize it.
        let cast = [0.6f32, 0.5, 0.42];
        let (t, n) = neutralize(cast).unwrap();
        let mut px = vec![[cast[0], cast[1], cast[2], 1.0]];
        develop(&mut px, 1, 1, &CameraRaw { temperature: t, tint: n, ..Default::default() }, false);
        assert!(t < 0.0 && (px[0][0] - px[0][2]).abs() < 0.03 && (px[0][0] - px[0][1]).abs() < 0.03, "{t} {n} {:?}", px[0]);
        assert!(neutralize([1.0, 0.5, 0.5]).is_none() && neutralize([0.0, 0.0, 0.0]).is_none() && neutralize([f32::NAN, 0.5, 0.5]).is_none());
        let warm: Vec<[f32; 4]> = img(32, 32).iter().map(|q| [q[0] * 1.1, q[1], q[2] * 0.7, 1.0]).collect();
        let (t, _) = auto_white_balance(&warm).unwrap();
        assert!(t < 0.0, "a warm image cools: {t}");
        assert!(auto_white_balance(&[]).is_none());
        // A dark, flat image gets more exposure and contrast; a normal one little.
        let dark: Vec<[f32; 4]> = (0..4096).map(|i| [0.08 + 0.1 * (i % 64) as f32 / 64.0; 4]).collect();
        let a = auto_tone(&dark);
        assert!(a.exposure > 0.5 && a.contrast > 0.0 && a.whites > 0.0, "{a:?}");
        assert_eq!(auto_tone(&[]), AutoTone::default());
        assert!([a.exposure, a.contrast, a.highlights, a.shadows, a.whites, a.blacks].iter().all(|v| v.is_finite()));
    }

    #[test]
    fn float_documents_keep_overrange() {
        let mut a = vec![[2.0f32, 1.5, 0.5, 1.0]; 16];
        develop(&mut a, 4, 4, &CameraRaw { exposure: 0.5, ..Default::default() }, true);
        assert!(a[0][0] > 2.0);
    }
}

