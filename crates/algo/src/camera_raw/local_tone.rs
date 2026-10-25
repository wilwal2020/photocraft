//! Shadows and Highlights as a local Laplacian filter.
//!
//! Adobe has said Camera Raw's process version 2012 rests on local Laplacian filters (S. Paris,
//! S. W. Hasinoff, J. Kautz, *Local Laplacian Filters: Edge-aware Image Processing with a
//! Laplacian Pyramid*, SIGGRAPH 2011). This follows that paper, in its fast form (M. Aubry et
//! al., *Fast Local Laplacian Filters*, ACM TOG 2014: the remapping sampled at a few reference
//! values, each pyramid coefficient interpolated between them), on the log2 luminance:
//!
//! - **Shadows** raises pixels darker than their surroundings, by an amount set by their own and
//!   their surroundings' place in the image's range (1st to 99th percentile); **Highlights**
//!   pulls pixels brighter than their surroundings toward them. Differences under a threshold
//!   are left alone, so fine detail stays.
//! - Shadows then keeps the image's 99th percentile, Highlights its 1st (Photoshop keeps the
//!   brightest content under Shadows and the darkest under Highlights).
//! - A uniform image is unchanged, edges keep no halo, and the result does not depend on the
//!   image size.
//! - Both remappings are the pixel plus the slider's amount times a function of the pixel and its
//!   reference, and the pyramids are linear, so each pass adds `amount · D` to the log luminance
//!   for one map `D` per direction: [`Cache`] keeps the maps, and moving a slider redoes only that
//!   sum. The filter also ignores a constant offset of the plane, which is all Exposure changes.
//!
//! The strengths were fitted to Photoshop 25.4's Camera Raw Filter on ten public-domain photos
//! (black-box measurements, no Adobe code or data). It is an approximation: on those photos the
//! mean error against Photoshop is 2.8–9.4 levels (of 255) where the previous heuristic's was
//! 3.7–14.3.

use super::{Encoding, luma};
use crate::photo_util::{par_map, par_rows};

/// Spacing of the fast filter's reference values in stops. Its remapping changes within about
/// half a stop, and coefficients interpolated across wider gaps show as contour lines on smooth
/// gradients (12 references over a whole raw's range left visible steps; at an eighth of a stop a
/// ramp's slope varies by under a third, as the remapping itself makes it).
const REF_STEP: f32 = 0.125;
/// The filter runs on a copy at most this many pixels across; the change it makes is brought back
/// to full size by a guided upsampling, so a preview and the full image agree.
const WORK_SIDE: usize = 1600;
/// Knots of the strength curves over the surroundings' place in the image range (0 … 1).
const KNOTS: usize = 5;

/// Shadows: how far a pixel darker than its surroundings rises, in stops per unit slider, as
/// a function of its own place in the image range (`own`) times a factor of its surroundings'
/// place (`around`); it rises fully once it is `sigma` stops below them. The lift depends on
/// the pixel's tone, not on the size of the difference, so it is the same at every pyramid
/// level and a big edge keeps no halo. Lowering (negative values) is `down` times as strong.
const SHADOWS: Lift = Lift { own: [-0.017, 0.466, 0.886, 1.221, -0.081], around: [4.566, 4.067, 1.088, 0.149, 0.878], sigma: 0.571, down: 0.808 };
const HIGHLIGHTS: Side = Side { pull: [0.179, 0.141, 0.196, 0.629, 2.07], push: [0.156, 0.08, 0.175, 0.049, 1.261], sigma: 0.05 };

/// See [`SHADOWS`].
struct Lift {
    own: [f32; KNOTS],
    around: [f32; KNOTS],
    sigma: f32,
    down: f32,
}

/// Highlights: strengths per unit slider over the surroundings' place in the image range, for
/// lowering (`pull`, toward the surroundings) and raising (`push`), and the detail threshold in
/// stops.
struct Side {
    pull: [f32; KNOTS],
    push: [f32; KNOTS],
    sigma: f32,
}

/// One channel, `w × h`.
struct Plane {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

/// Mirrors `i` into `0..n` (the edge sample is not repeated).
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let p = 2 * (n - 1);
    let m = i.rem_euclid(p);
    (if m >= n { p - m } else { m }) as usize
}

const TAPS: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// Planes smaller than this run their rows on one thread: the filter builds a whole pyramid per
/// reference (up to 96), and splitting its small levels into one-row tasks cost more in rayon's
/// scheduling than the rows themselves (a fifth of a Shadows render on 36 MP, measured).
const PAR_MIN: usize = 1 << 15;

/// [`par_rows`] for a plane, on one thread when it is small (see [`PAR_MIN`]).
fn rows(buf: &mut [f32], w: usize, f: impl Fn(usize, &mut [f32]) + Sync + Send) {
    rows_on(true, buf, w, f);
}

/// [`rows`], or always on this thread unless `parallel`.
fn rows_on(parallel: bool, buf: &mut [f32], w: usize, f: impl Fn(usize, &mut [f32]) + Sync + Send) {
    if !parallel || buf.len() < PAR_MIN {
        buf.chunks_mut(w.max(1)).enumerate().for_each(|(y, row)| f(y, row));
    } else {
        par_rows(buf, w, 1, f);
    }
}

/// The binomial 5-tap blur, sampled every `step` pixels from 0 (1 = full resolution).
fn blur_step(p: &Plane, step: usize) -> Plane {
    blur_step_on(true, p, step)
}

/// [`blur_step`], on this thread only unless `parallel`.
fn blur_step_on(parallel: bool, p: &Plane, step: usize) -> Plane {
    let (w2, h2) = (p.w.div_ceil(step), p.h.div_ceil(step));
    // Columns first at the kept rows, then rows at the kept columns. Each sum adds the same terms
    // in the same order as a `mirror` per tap would, so the result is bit for bit the same; the
    // source rows are found once per row and the columns mirrored only near the edges.
    let mut tmp = vec![0.0f32; p.w * h2];
    rows_on(parallel, &mut tmp, p.w, |y2, row| {
        let y = (y2 * step) as isize;
        let src: [&[f32]; 5] = std::array::from_fn(|k| {
            let r = mirror(y + k as isize - 2, p.h);
            &p.v[r * p.w..(r + 1) * p.w]
        });
        for (x, o) in row.iter_mut().enumerate() {
            *o = (0..5).map(|k| TAPS[k] * src[k][x]).sum();
        }
    });
    let mut out = vec![0.0f32; w2 * h2];
    rows_on(parallel, &mut out, w2, |y2, row| {
        let src = &tmp[y2 * p.w..(y2 + 1) * p.w];
        for (x2, o) in row.iter_mut().enumerate() {
            let x = x2 * step;
            *o = if x >= 2 && x + 2 < p.w {
                let s = &src[x - 2..x + 3];
                (0..5).map(|k| TAPS[k] * s[k]).sum()
            } else {
                (0..5).map(|k| TAPS[k] * src[mirror(x as isize + k as isize - 2, p.w)]).sum()
            };
        }
    });
    Plane { w: w2, h: h2, v: out }
}

/// The next coarser level.
fn down(p: &Plane) -> Plane {
    blur_step(p, 2)
}

/// `p` brought up to `w × h`: zeros between samples, then the blur times four.
///
/// Three samples in four are zero, so their taps are skipped instead of blurring a zero-filled
/// plane: the nonzero terms are added in the same order (skipping `+0.0` terms changes at most the
/// sign of a zero, and the filter only adds and exponentiates these values).
fn up(p: &Plane, w: usize, h: usize) -> Plane {
    let (lh, lw) = (p.h.min(h.div_ceil(2)), p.w.min(w.div_ceil(2)));
    // The source sample behind index `i` of a zero-stuffed axis of length `n` holding `lim` samples.
    let src = |i: isize, n: usize, lim: usize| {
        let m = mirror(i, n);
        (m.is_multiple_of(2) && m / 2 < lim).then_some(m / 2)
    };
    // Columns first, only at the even columns (the odd ones are all zero).
    let mut tmp = vec![0.0f32; lw * h];
    if lw > 0 {
        rows(&mut tmp, lw, |y, row| {
            let taps: [Option<&[f32]>; 5] = std::array::from_fn(|k| src(y as isize + k as isize - 2, h, lh).map(|r| &p.v[r * p.w..r * p.w + lw]));
            for (j, o) in row.iter_mut().enumerate() {
                *o = (0..5).filter_map(|k| taps[k].map(|s| TAPS[k] * (s[j] * 4.0))).sum();
            }
        });
    }
    let mut out = vec![0.0f32; w * h];
    rows(&mut out, w, |y, row| {
        let t = &tmp[y * lw..(y + 1) * lw];
        for (x, o) in row.iter_mut().enumerate() {
            *o = if x >= 2 && x + 2 < w {
                // Away from the edges no tap mirrors.
                (0..5)
                    .filter_map(|k| {
                        let i = x + k - 2;
                        (i.is_multiple_of(2) && i / 2 < lw).then(|| TAPS[k] * t[i / 2])
                    })
                    .sum()
            } else {
                (0..5).filter_map(|k| src(x as isize + k as isize - 2, w, lw).map(|j| TAPS[k] * t[j])).sum()
            };
        }
    });
    Plane { w, h, v: out }
}

/// The source rows of [`up`]`(p, w, h)` at output row `y`, one per tap (`None` where the
/// zero-stuffed row is all zeros).
fn up_rows(p: &Plane, h: usize, y: usize) -> [Option<usize>; 5] {
    let lh = p.h.min(h.div_ceil(2));
    std::array::from_fn(|k| {
        let m = mirror(y as isize + k as isize - 2, h);
        (m.is_multiple_of(2) && m / 2 < lh).then_some(m / 2)
    })
}

/// One sample of [`up`]`(p, w, h)`: column `x` of the output row whose [`up_rows`] are `rows`.
/// It adds the same terms in the same order as `up`, so it gives the same bits.
fn up_at(p: &Plane, w: usize, rows: &[Option<usize>; 5], x: usize) -> f32 {
    let lw = p.w.min(w.div_ceil(2));
    let col = |j: usize| -> f32 { (0..5).filter_map(|k| rows[k].map(|r| TAPS[k] * (p.v[r * p.w + j] * 4.0))).sum() };
    if x >= 2 && x + 2 < w {
        // Away from the edges no tap mirrors.
        (0..5)
            .filter_map(|k| {
                let i = x + k - 2;
                (i.is_multiple_of(2) && i / 2 < lw).then(|| TAPS[k] * col(i / 2))
            })
            .sum()
    } else {
        (0..5)
            .filter_map(|k| {
                let m = mirror(x as isize + k as isize - 2, w);
                (m.is_multiple_of(2) && m / 2 < lw).then(|| TAPS[k] * col(m / 2))
            })
            .sum()
    }
}

/// Levels down to about 2 pixels on the short side.
fn depth(w: usize, h: usize) -> usize {
    let mut s = w.min(h);
    let mut n = 1;
    while s > 2 {
        s = s.div_ceil(2);
        n += 1;
    }
    n
}

fn gaussian(p: Plane, n: usize) -> Vec<Plane> {
    let mut g = vec![p];
    for _ in 1..n {
        let next = down(&g[g.len() - 1]);
        g.push(next);
    }
    g
}

/// [`gaussian`] on this thread only (for work already split across threads).
fn gaussian_serial(p: Plane, n: usize) -> Vec<Plane> {
    let mut g = vec![p];
    for _ in 1..n {
        let next = blur_step_on(false, &g[g.len() - 1], 2);
        g.push(next);
    }
    g
}

/// The local Laplacian filter's change for the remapping `x + delta(x, reference)`: the sum of
/// the remapped Laplacian levels of `delta` alone (the plane's own levels add up to the plane, the
/// interpolation weights summing to one), over a coarsest level of zero.
fn llf_delta(u: &Plane, delta: impl Fn(f32, f32) -> f32 + Sync) -> Plane {
    let n = depth(u.w, u.h);
    let g = gaussian(Plane { w: u.w, h: u.h, v: u.v.clone() }, n);
    // References span the image's tones; the deepest 0.1 % (noise down to the floor) share the
    // lowest reference instead of stretching the spacing.
    let hi = u.v.iter().fold(f32::NEG_INFINITY, |a, &v| a.max(v));
    let lo = quantile(&u.v, 0.001).min(hi);
    if hi - lo <= 1e-6 || (hi - lo).is_nan() {
        return Plane { w: u.w, h: u.h, v: vec![0.0; u.v.len()] };
    }
    let refs = (((hi - lo) / REF_STEP).ceil() as usize + 1).clamp(2, 96);
    let step = (hi - lo) / (refs - 1) as f32;
    let mut lap: Vec<Vec<f32>> = g[..n - 1].iter().map(|p| vec![0.0; p.v.len()]).collect();
    // One reference's weighted Laplacian coefficients where its weight isn't zero (a pixel lies
    // between two references, so that is about two in `refs` of them), as (index, value) per level.
    let contribution = |k: usize| -> Vec<Vec<(u32, f32)>> {
        let r = lo + step * k as f32;
        let m = Plane { w: u.w, h: u.h, v: u.v.iter().map(|v| delta(*v, r)).collect() };
        let gr = gaussian_serial(m, n);
        (0..n - 1)
            .map(|l| {
                let (gl, fine, lw, lh, coarse) = (&g[l].v, &gr[l].v, g[l].w, g[l].h, &gr[l + 1]);
                let mut out = Vec::new();
                for y in 0..lh {
                    let taps = up_rows(coarse, lh, y);
                    for x in 0..lw {
                        let i = y * lw + x;
                        let wgt = (1.0 - (gl[i].clamp(lo, hi) - r).abs() / step).max(0.0);
                        if wgt > 0.0 {
                            out.push((i as u32, wgt * (fine[i] - up_at(coarse, lw, &taps, x))));
                        }
                    }
                }
                out
            })
            .collect()
    };
    // References in parallel batches (each one's pyramid is small work to split further: rayon's
    // scheduling of its levels' rows was a large part of the filter), added in reference order,
    // so the result doesn't depend on the threads and is the one a single pass gives.
    let batch = refs.min(32);
    for first in (0..refs).step_by(batch) {
        let parts = par_map((refs - first).min(batch), |j| contribution(first + j));
        for part in parts {
            for (acc, level) in lap.iter_mut().zip(part) {
                for (i, v) in level {
                    if let Some(a) = acc.get_mut(i as usize) {
                        *a += v;
                    }
                }
            }
        }
    }
    let mut o = Plane { w: g[n - 1].w, h: g[n - 1].h, v: vec![0.0; g[n - 1].v.len()] };
    for l in (0..n - 1).rev() {
        let mut next = up(&o, g[l].w, g[l].h);
        for (a, b) in next.v.iter_mut().zip(&lap[l]) {
            *a += b;
        }
        o = next;
    }
    o
}

/// The `q` quantile (0 … 1) of `v`.
fn quantile(v: &[f32], q: f32) -> f32 {
    let mut s = v.to_vec();
    // Selecting the one element is linear, a full sort is not (5 sorts of the work copy were a
    // fifteenth of a render). `total_cmp` ties are bit-identical, so it is the sorted element.
    let i = ((s.len() - 1) as f32 * q).round() as usize;
    *s.select_nth_unstable_by(i, f32::total_cmp).1
}

/// 1st and 99th percentiles of the log luminance on a copy at most 256 pixels across.
fn range(u: &Plane) -> (f32, f32) {
    let f = u.w.max(u.h).div_ceil(256).max(1);
    let (sw, sh) = (u.w.div_ceil(f), u.h.div_ceil(f));
    // Rows of cells in parallel; each cell adds its pixels in the order one pass would.
    let rows = par_map(sh, |sy| {
        let mut sum = vec![(0.0f64, 0u32); sw];
        for y in sy * f..((sy + 1) * f).min(u.h) {
            for (x, v) in u.v[y * u.w..(y + 1) * u.w].iter().enumerate() {
                let c = &mut sum[x / f];
                c.0 += f64::from(*v);
                c.1 += 1;
            }
        }
        sum
    });
    let cells: Vec<f32> = rows.iter().flatten().filter(|c| c.1 > 0).map(|c| (c.0 / f64::from(c.1)) as f32).collect();
    (quantile(&cells, 0.01), quantile(&cells, 0.99))
}

/// `d` past the detail threshold: about 0 below `sigma`, `d − sigma` well above it.
fn excess(d: f32, sigma: f32) -> f32 {
    d - sigma * (d / sigma).tanh()
}

fn knot(t: f32, k: &[f32; KNOTS]) -> f32 {
    let x = t.clamp(0.0, 1.0) * (KNOTS - 1) as f32;
    let i = (x as usize).min(KNOTS - 2);
    k[i] + (k[i + 1] - k[i]) * (x - i as f32)
}

/// The Highlights map of the log luminance `u`, for raising it (`push`) or lowering it: pixels
/// brighter than their surroundings move toward them (or away) by the part of the difference
/// past `sigma`, per unit amount.
fn highlights_map(u: &Plane, push: bool, side: &Side) -> Plane {
    let (lo, hi) = range(u);
    let span = (hi - lo).max(1.0);
    let strengths = if push { &side.push } else { &side.pull };
    llf_delta(u, |x, g| knot((g - lo) / span, strengths) * excess((x - g).max(0.0), side.sigma))
}

/// The Shadows map of the log luminance `u`, per unit amount.
fn shadows_map(u: &Plane) -> Plane {
    let (lo, hi) = range(u);
    let span = (hi - lo).max(1.0);
    llf_delta(u, |x, g| {
        let below = ((g - x) / SHADOWS.sigma).clamp(0.0, 1.0);
        if below == 0.0 {
            return 0.0;
        }
        let w = below * below * (3.0 - 2.0 * below);
        knot((x - lo) / span, &SHADOWS.own) * knot((g - lo) / span, &SHADOWS.around) * w
    })
}

/// `u + k · d`, shifted so that its `q` quantile stays where `u`'s was (Highlights keeps the
/// darkest content, Shadows the brightest, as Photoshop does).
fn add_kept(u: &Plane, k: f32, d: &Plane, q: f32) -> Plane {
    let v: Vec<f32> = par_map(u.v.len(), |i| u.v[i] + k * d.v[i]);
    let shift = quantile(&u.v, q) - quantile(&v, q);
    Plane { w: u.w, h: u.h, v: par_map(v.len(), |i| v[i] + shift) }
}

/// The filter maps [`apply`] keeps between calls, each with the key of the plane it was made
/// from (the caller's: the pixels and the settings before this stage, see the module docs).
#[derive(Default)]
pub(crate) struct Cache {
    /// Highlights: the key, whether the map raises, the map.
    highlights: Option<(u64, bool, Plane)>,
    /// Shadows: the key, the Highlights value it ran after (its bits), the map.
    shadows: Option<(u64, u32, Plane)>,
}

/// Applies Shadows and Highlights (−100 … 100) to straight RGBA pixels encoded with `enc`.
/// `cache`: maps kept from earlier calls, the key of these pixels, and whether a map made for
/// another key (or Highlights value) may serve, which is close but not exact. Returns false when
/// one did.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    px: &mut [[f32; 4]],
    w: usize,
    h: usize,
    shadows: f32,
    highlights: f32,
    enc: Encoding,
    cache: Option<(&mut Cache, u64, bool)>,
    record: Option<&mut Option<Map>>,
) -> bool {
    if (shadows == 0.0 && highlights == 0.0) || w < 2 || h < 2 || px.len() != w * h {
        return true;
    }
    const FLOOR: f32 = 1.0 / 65536.0;
    let curves = enc.curves();
    let u = Plane { w, h, v: par_map(px.len(), |i| (luma([0, 1, 2].map(|c| curves.decode(px[i][c]))).max(0.0) + FLOOR).log2()) };
    // The filter on a copy at most WORK_SIDE across (box-averaged in log).
    let f = w.max(h).div_ceil(WORK_SIDE).max(1);
    let small = if f == 1 { Plane { w, h, v: u.v.clone() } } else { shrink(&u, f) };
    let mut fresh = Cache::default();
    let (cache, key, draft) = match cache {
        Some((c, key, draft)) => (c, key, draft),
        None => (&mut fresh, 0, false),
    };
    let mut exact = true;
    let mut o = Plane { w: small.w, h: small.h, v: small.v.clone() };
    if highlights != 0.0 {
        let amount = highlights.clamp(-100.0, 100.0) / 100.0;
        let push = amount > 0.0;
        let fits = |c: &&(u64, bool, Plane)| c.1 == push && (c.2.w, c.2.h) == (o.w, o.h) && (c.0 == key || draft);
        if cache.highlights.as_ref().filter(fits).is_none() {
            cache.highlights = Some((key, push, highlights_map(&o, push, &HIGHLIGHTS)));
        }
        if let Some((made, _, d)) = &cache.highlights {
            exact &= *made == key;
            o = add_kept(&o, amount, d, 0.01);
        }
    }
    if shadows != 0.0 {
        let amount = shadows.clamp(-100.0, 100.0) / 100.0;
        let k = if amount > 0.0 { amount } else { amount * SHADOWS.down };
        let after = highlights.to_bits();
        let fits = |c: &&(u64, u32, Plane)| (c.2.w, c.2.h) == (o.w, o.h) && ((c.0, c.1) == (key, after) || draft);
        if cache.shadows.as_ref().filter(fits).is_none() {
            cache.shadows = Some((key, after, shadows_map(&o)));
        }
        if let Some((made, made_after, d)) = &cache.shadows {
            exact &= (*made, *made_after) == (key, after);
            o = add_kept(&o, k, d, 0.99);
        }
    }
    // The change in log luminance, at full size.
    let d: Vec<f32> = par_map(o.v.len(), |i| o.v[i] - small.v[i]);
    if let Some(r) = record {
        // For a region's develop: the change as the guided filter's coefficients over the image.
        let (ma, mb) = super::guided_coefficients(&small.v, &d, small.w, small.h, 2, 0.01);
        *r = Some(Map { ma, mb, w: small.w, h: small.h });
    }
    // Through the guided filter at every size (at the work copy's own size too), so a region's
    // develop from the coefficients ([`apply_map`]) gives the same.
    let change = guided_upsample(&small, &d, &u);
    par_rows(px, w, 1, |y, row| {
        for (x, q) in row.iter_mut().enumerate() {
            let gain = change[y * w + x].exp2();
            for c in 0..3 {
                q[c] = curves.encode(curves.decode(q[c]) * gain);
            }
        }
    });
    exact
}

/// The Shadows and Highlights change of a whole image as the guided filter's coefficients on its
/// work copy (`w × h`, laid over the image): the change in log luminance at a pixel is
/// `a · u + b`, `u` its log luminance (see [`guided_upsample`]).
#[derive(Clone, Debug)]
pub(crate) struct Map {
    ma: Vec<f32>,
    mb: Vec<f32>,
    w: usize,
    h: usize,
}

/// Shadows and Highlights on part of an image (`frame`) from the whole image's [`Map`].
pub(crate) fn apply_map(px: &mut [[f32; 4]], w: usize, map: &Map, frame: super::Frame, enc: Encoding) {
    const FLOOR: f32 = 1.0 / 65536.0;
    let curves = enc.curves();
    let (fx, fy) = (map.w as f32 / frame.full_w as f32, map.h as f32 / frame.full_h as f32);
    par_rows(px, w, 1, |y, row| {
        let sy = (frame.y(y) + 0.5) * fy - 0.5;
        for (x, q) in row.iter_mut().enumerate() {
            let sx = (frame.x(x) + 0.5) * fx - 0.5;
            let lin = [0, 1, 2].map(|c| curves.decode(q[c]));
            let u = (luma(lin).max(0.0) + FLOOR).log2();
            let a = crate::photo_util::bilinear(&map.ma, map.w, map.h, 1, 0, sx, sy);
            let b = crate::photo_util::bilinear(&map.mb, map.w, map.h, 1, 0, sx, sy);
            let gain = (a * u + b).exp2();
            for c in 0..3 {
                q[c] = curves.encode(lin[c] * gain);
            }
        }
    });
}

/// `u` averaged over `f × f` blocks.
fn shrink(u: &Plane, f: usize) -> Plane {
    let (sw, sh) = (u.w.div_ceil(f), u.h.div_ceil(f));
    let mut v = vec![0.0f32; sw * sh];
    par_rows(&mut v, sw, 1, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let (x0, y0) = (x * f, y * f);
            let (x1, y1) = ((x0 + f).min(u.w), (y0 + f).min(u.h));
            let mut s = 0.0f64;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    s += f64::from(u.v[yy * u.w + xx]);
                }
            }
            *o = (s / ((x1 - x0) * (y1 - y0)) as f64) as f32;
        }
    });
    Plane { w: sw, h: sh, v }
}

/// The change `d` computed on `small` brought to the size of `full`, following its edges: a local
/// linear model of the change against the log luminance (the guided filter of K. He, J. Sun,
/// *Fast Guided Filter*, 2015), fitted on the small copy and evaluated on the full one.
fn guided_upsample(small: &Plane, d: &[f32], full: &Plane) -> Vec<f32> {
    let (sw, sh) = (small.w, small.h);
    let (ma, mb) = super::guided_coefficients(&small.v, d, sw, sh, 2, 0.01);
    let (fx, fy) = (sw as f32 / full.w as f32, sh as f32 / full.h as f32);
    let mut out = vec![0.0f32; full.w * full.h];
    par_rows(&mut out, full.w, 1, |y, row| {
        let sy = (y as f32 + 0.5) * fy - 0.5;
        for (x, o) in row.iter_mut().enumerate() {
            let sx = (x as f32 + 0.5) * fx - 0.5;
            let av = crate::photo_util::bilinear(&ma, sw, sh, 1, 0, sx, sy);
            let bv = crate::photo_util::bilinear(&mb, sw, sh, 1, 0, sx, sy);
            *o = av * full.v[y * full.w + x] + bv;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRGB: Encoding = Encoding(0.0);

    /// The plain blur: a `mirror` per tap over the whole plane, the reference for [`blur_step`].
    fn blur_step_reference(p: &Plane, step: usize) -> Plane {
        let (w2, h2) = (p.w.div_ceil(step), p.h.div_ceil(step));
        let mut tmp = vec![0.0f32; p.w * h2];
        for y2 in 0..h2 {
            for x in 0..p.w {
                tmp[y2 * p.w + x] = (0..5).map(|k| TAPS[k] * p.v[mirror((y2 * step) as isize + k as isize - 2, p.h) * p.w + x]).sum();
            }
        }
        let mut out = vec![0.0f32; w2 * h2];
        for y2 in 0..h2 {
            for x2 in 0..w2 {
                out[y2 * w2 + x2] = (0..5).map(|k| TAPS[k] * tmp[y2 * p.w + mirror((x2 * step) as isize + k as isize - 2, p.w)]).sum();
            }
        }
        Plane { w: w2, h: h2, v: out }
    }

    /// The plain upsampling: a zero-stuffed plane blurred whole, the reference for [`up`].
    fn up_reference(p: &Plane, w: usize, h: usize) -> Plane {
        let mut z = Plane { w, h, v: vec![0.0; w * h] };
        for y in 0..p.h.min(h.div_ceil(2)) {
            for x in 0..p.w.min(w.div_ceil(2)) {
                z.v[2 * y * w + 2 * x] = p.v[y * p.w + x] * 4.0;
            }
        }
        blur_step_reference(&z, 1)
    }

    fn noise_plane(w: usize, h: usize, seed: u32) -> Plane {
        let mut s = seed.wrapping_mul(2_654_435_761).max(1);
        let v = (0..w * h)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s % 20_000) as f32 / 1000.0 - 16.0
            })
            .collect();
        Plane { w, h, v }
    }

    #[test]
    fn the_fast_pyramid_matches_the_plain_one_exactly() {
        // Odd, even, tiny and large-enough-to-run-in-parallel sizes, as the pyramid's levels are.
        for (i, (w, h)) in [(1, 1), (1, 7), (2, 2), (3, 5), (5, 3), (8, 6), (37, 23), (64, 64), (301, 211)].into_iter().enumerate() {
            let p = noise_plane(w, h, i as u32 + 1);
            for step in [1, 2] {
                assert_eq!(blur_step(&p, step).v, blur_step_reference(&p, step).v, "blur {w}×{h} step {step}");
            }
            for (uw, uh) in [(w * 2, h * 2), (w * 2 - 1, h * 2 - 1), (w * 2, h * 2 - 1), (w.max(2) * 2 - 3, h * 2)] {
                let full = up(&p, uw, uh);
                assert_eq!(full.v, up_reference(&p, uw, uh).v, "up {w}×{h} → {uw}×{uh}");
                // The filter samples `up` one pixel at a time; every sample has the same bits.
                for y in 0..uh {
                    let taps = up_rows(&p, uh, y);
                    for x in 0..uw {
                        assert_eq!(up_at(&p, uw, &taps, x).to_bits(), full.v[y * uw + x].to_bits(), "up_at {w}×{h} → {uw}×{uh} at {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn quantile_is_the_sorted_element() {
        let p = noise_plane(301, 211, 9);
        let mut v = p.v.clone();
        // Ties and signed zeros, which `total_cmp` orders by their bits.
        v.extend([0.0, -0.0, 0.0, -0.0, 3.5, 3.5, f32::MIN_POSITIVE]);
        let mut sorted = v.clone();
        sorted.sort_by(f32::total_cmp);
        for q in [0.0, 0.001, 0.01, 0.25, 0.5, 0.99, 1.0] {
            let want = sorted[((sorted.len() - 1) as f32 * q).round() as usize];
            assert_eq!(quantile(&v, q).to_bits(), want.to_bits(), "q {q}");
        }
    }

    fn grey(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Vec<[f32; 4]> {
        (0..w * h).map(|i| f(i % w, i / w)).map(|v| [v, v, v, 1.0]).collect()
    }

    #[test]
    fn a_flat_image_is_unchanged() {
        let mut px = grey(64, 48, |_, _| 0.4);
        apply(&mut px, 64, 48, 100.0, -100.0, SRGB, None, None);
        assert!(px.iter().all(|q| (q[0] - 0.4).abs() < 1e-5));
    }

    #[test]
    fn shadows_raise_a_dark_area_without_a_halo() {
        // A 0.1 | 0.6 step (the dark half is the image's black point, which Photoshop barely
        // moves either): the bright half stays, and no halo forms on either side of the edge.
        let (w, h) = (256, 128);
        let mut px = grey(w, h, |x, _| if x < w / 2 { 0.1 } else { 0.6 });
        apply(&mut px, w, h, 100.0, 0.0, SRGB, None, None);
        let row = &px[64 * w..65 * w];
        assert!((row[w / 2 + 1][0] - 0.6).abs() < 0.002 && (row[w - 5][0] - 0.6).abs() < 0.002);
        let (far, near) = (row[10][0], row[w / 2 - 1][0]);
        assert!((far - 0.1).abs() < 0.004 && (far - near).abs() < 0.004, "{far} {near}");
    }

    #[test]
    fn grey_moves_as_in_photoshop() {
        // Mid-grey with 1 % black (0.015) and white (0.97) corners, Photoshop 25.4: Shadows +100
        // takes the grey from 127.5 to 165.3, Highlights −100 to 89.6 (of 255).
        let n = 256;
        let image = |c: f32| {
            grey(n, n, |x, y| {
                if x >= n - 26 && y >= n - 26 {
                    0.97
                } else if x >= n - 26 && y < 26 {
                    0.015
                } else {
                    c
                }
            })
        };
        for (s, hl, want) in [(100.0, 0.0, 165.3), (0.0, -100.0, 89.6)] {
            let mut px = image(0.5);
            apply(&mut px, n, n, s, hl, SRGB, None, None);
            let got = px[175 * n + 100][0] * 255.0;
            assert!((got - want).abs() < 5.0, "{s} {hl}: {got} vs {want}");
        }
    }

    #[test]
    fn highlights_lower_a_bright_area_and_keep_the_darkest() {
        let (w, h) = (192, 192);
        let mut px = grey(w, h, |x, y| if (64..128).contains(&x) && (64..128).contains(&y) { 0.8 } else { 0.2 });
        apply(&mut px, w, h, 0.0, -100.0, SRGB, None, None);
        assert!(px[96 * w + 96][0] < 0.75, "{}", px[96 * w + 96][0]);
        assert!((px[5 * w + 5][0] - 0.2).abs() < 0.01, "{}", px[5 * w + 5][0]);
    }

    #[test]
    fn smooth_gradients_stay_smooth() {
        // A soft ramp over eight stops, as in an out-of-focus background. Coefficients interpolated
        // between references too far apart drew contour lines across such ramps (Shadows +100 on
        // a D4 raw): the output must rise steadily, without ripples.
        let (w, h) = (640, 48);
        let mut px = grey(w, h, |x, _| crate::photo_util::linear_to_srgb((x as f32 / (w - 1) as f32 * 8.0 - 9.0).exp2()));
        for (s, hl) in [(100.0, 0.0), (0.0, -100.0), (-100.0, 100.0)] {
            let mut p = px.clone();
            apply(&mut p, w, h, s, hl, SRGB, None, None);
            let row: Vec<f32> = (0..w).map(|x| crate::photo_util::srgb_to_linear(p[24 * w + x][0]).max(1e-6).log2()).collect();
            let steps: Vec<f32> = row.windows(2).map(|d| d[1] - d[0]).collect();
            assert!(steps.iter().all(|d| *d > 0.0), "{s} {hl}: the ramp reverses");
            let jumps: Vec<f32> = steps.windows(2).map(|d| (d[1] - d[0]).abs()).collect();
            let ripple = jumps[16..jumps.len() - 16].iter().fold(0.0f32, |a, b| a.max(*b));
            // The ramp rises 0.0125 stops per pixel; its slope may change by a third at most.
            assert!(ripple < 0.004, "{s} {hl}: slope jumps by {ripple} stops per pixel");
        }
        px.clear();
    }

    #[test]
    fn the_result_does_not_depend_on_the_image_size() {
        let at = |n: usize| {
            let mut px = grey(n, n, |x, y| if x < n / 3 || y < n / 4 { 0.08 } else { 0.7 });
            apply(&mut px, n, n, 70.0, -60.0, SRGB, None, None);
            [px[(n / 8) * n + n / 8][0], px[(3 * n / 4) * n + 3 * n / 4][0]]
        };
        let (a, b) = (at(96), at(384));
        assert!((a[0] - b[0]).abs() < 0.02 && (a[1] - b[1]).abs() < 0.02, "{a:?} {b:?}");
    }
}
