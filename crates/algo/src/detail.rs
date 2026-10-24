//! Make a generated fill (LaMa's) sit in its surroundings the way they look up close (user report:
//! removals in a grainy blue sky showed as blotches once the contrast was raised, and on a
//! halftone page the fill's dots came out grey and irregular). Two passes:
//!
//! * **Tone.** A model wanders a few levels in tone and colour across a fill, which the eye
//!   finds in a smooth area. The fill's low frequencies are compared with a membrane (harmonic)
//!   interpolation of its surroundings' (P. Pérez et al., *Poisson Image Editing*, 2003), and a
//!   deviation of a few levels is taken out, softly, while a larger one (a shape the model drew:
//!   an edge carried through, a shadow) stays.
//! * **Detail.** The fill's detail, the image less a small blur, is compared locally with the
//!   detail of the known pixels around the hole, and where the fill has less, the missing detail
//!   is quilted from the surroundings' own (A. Efros, W. Freeman, *Image Quilting for Texture
//!   Synthesis and Transfer*, SIGGRAPH 2001): blocks are placed one after another, each the
//!   candidate that best continues what is already there across its overlap, joined to it along
//!   the minimum-error cut. Continuing a neighbour's source is always a candidate, so a regular
//!   pattern keeps its phase, and among nearly equal candidates one is picked at random, so grain
//!   doesn't repeat. Only faint texture (grain) is put back this way: strong detail (lines, a
//!   pattern) the model draws itself, and quilting it put lines where the model left paper.
//!   On a black-and-white page, where the hole's surroundings are a dot screen (a regular
//!   lattice of dots: two independent shifts under which the pattern matches itself), whole
//!   pixels are quilted from the screen around, guided by the fill's tone (Efros and Freeman's
//!   texture transfer), so the dots stay crisp and in phase rather than becoming the model's grey
//!   ones; elsewhere on the page (line art, hatching, paper) the model's fill stays as it is.
//!
//! Buffers are interleaved `w × h × ch` normalised floats whose first `colours` channels are
//! colour (the rest, alpha, is left alone), with a `w × h` hole mask. Deterministic for a seed.

use photocraft_raster::{Cancelled, Interrupt};

use crate::inpaint::Rng;

/// How [`restore_with`] works.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// Take the fill's small wanderings in tone out.
    pub tone: bool,
    /// Block side, in pixels.
    pub block: usize,
    /// Overlap between neighbouring blocks, in pixels.
    pub overlap: usize,
    /// Candidate sources tried per block (besides continuing the neighbours').
    pub candidates: usize,
    /// Radius of the box blur (applied twice) that separates detail from tone.
    pub blur: usize,
    pub seed: u64,
}

impl Default for Params {
    fn default() -> Self {
        Params { tone: true, block: 16, overlap: 5, candidates: 200, blur: 2, seed: 0x5EED }
    }
}

/// Radius of the box blur (applied twice) giving the tone the tone pass compares.
const TONE_BLUR: usize = 4;

/// A deviation from the membrane of `t` (0–1 units, per channel) is halved by the tone pass, a
/// smaller one goes almost entirely and a larger one stays (`d · d⁴ / (d⁴ + t⁴)` is kept: at
/// 5 levels, 94 % of 3 levels goes and 97 % of 12 stays). `t` is this where the surroundings
/// have shape of their own (foliage, a face: the model's variations are likely real)...
const TONE_KEEP: f32 = 5.0 / 255.0;
/// ...and this where they are smooth (a clear sky: a fill should be as smooth, and LaMa's has
/// been seen 6.5 levels off in one channel there).
const SMOOTH_KEEP: f32 = 16.0 / 255.0;
/// Shape around (the mean difference between the surroundings' tone at two scales) up to which
/// they count as smooth, and from which they count as shaped.
const SMOOTH: f32 = 1.0 / 255.0;
const SHAPED: f32 = 6.0 / 255.0;
/// Radius over which the surroundings' shape is measured.
const SHAPE_RADIUS: usize = 32;

/// Detail energy (mean square, 0–1 units) below which an area counts as flat: nothing to put back.
const FLAT: f32 = (0.75 / 255.0) * (0.75 / 255.0);

/// Below this mean share of missing detail over the hole the fill's detail is left as it is.
const ENOUGH: f32 = 0.08;

/// Candidates within this factor of the best one's error are equally good.
const TOLERANCE: f32 = 1.1;

/// Radius over which the surroundings' detail energy is measured.
const ENERGY_RADIUS: usize = 24;

/// Radius over which the fill's own detail energy is measured.
const FILL_RADIUS: usize = 8;

/// A black-and-white page (where dot screens are looked for): the share of the surroundings'
/// pixels at most [`NEUTRAL_SPREAD`] from grey.
const NEUTRAL_SHARE: f64 = 0.95;
const NEUTRAL_SPREAD: f32 = 0.06;

/// A black-and-white page: the share of its pixels near black or white ([`is_bilevel`]).
const BILEVEL_SHARE: f64 = 0.8;

/// A screen's fill that is within this of white or black is plain paper or solid ink: it stays.
const PLAIN: f32 = 0.04;

/// Grain is put back fully up to this amplitude (the root of the surroundings' detail energy)
/// and not at all from this one: stronger detail is lines and patterns, which the model draws.
const GRAIN_FULL: f32 = 8.0 / 255.0;
const GRAIN_NONE: f32 = 16.0 / 255.0;

/// The dot screen test: patches this wide, on a grid this fine, within this distance of the hole.
const LATTICE_PATCH: usize = 32;
const LATTICE_CELL: usize = 24;
const LATTICE_NEAR: usize = 96;
/// Shifts tried (up to this far, from this near) and the correlation both lattice shifts need.
const LATTICE_SHIFT: i32 = 12;
/// The shortest shift's square: (2, 2), a fine 45° screen's neighbour.
const LATTICE_MIN_SHIFT2: i32 = 8;
const LATTICE_CORR: f32 = 0.5;
/// A patch whose tones vary less than this (variance) is plain: it doesn't vote. Nor does one
/// that varies less than [`LATTICE_FAINT`] and isn't a screen: a screen's highlights (tiny, soft
/// dots) fail the test and outvoted the middle of a hole that was all screen.
const LATTICE_FLAT: f32 = 0.002;
const LATTICE_FAINT: f32 = 0.03;

/// Weight of matching the fill's tone against continuing the neighbours, when quilting whole
/// pixels.
const GUIDE: f32 = 60.0;

/// Radius of the box blur (applied twice) giving the tone a screen's quilting follows.
const GUIDE_BLUR: usize = 3;

/// [`restore_with`] with the default [`Params`].
pub fn restore(w: usize, h: usize, ch: usize, colours: usize, fill: &[f32], hole: &[bool], ctl: &Interrupt) -> Result<Vec<f32>, Cancelled> {
    restore_with(w, h, ch, colours, fill, hole, &Params::default(), ctl)
}

/// `fill` (the known pixels and a fill of the hole) with its tone settled and the hole's missing
/// fine detail quilted in from around it (see the module docs). Pixels outside the hole never
/// change; a buffer or mask of the wrong size, no hole or nothing known comes back unchanged.
#[allow(clippy::too_many_arguments)]
pub fn restore_with(w: usize, h: usize, ch: usize, colours: usize, fill: &[f32], hole: &[bool], p: &Params, ctl: &Interrupt) -> Result<Vec<f32>, Cancelled> {
    let n = w.saturating_mul(h);
    let cc = colours.min(ch);
    let b = p.block.max(4);
    let o = p.overlap.clamp(1, b / 2);
    if cc == 0 || n == 0 || hole.len() != n || fill.len() != n.saturating_mul(ch) || w < b || h < b {
        return Ok(fill.to_vec());
    }
    let holes = hole.iter().filter(|m| **m).count();
    if holes == 0 || holes == n {
        return Ok(fill.to_vec());
    }
    let known: Vec<f32> = hole.iter().map(|m| if *m { 0.0 } else { 1.0 }).collect();
    let inside: Vec<f32> = known.iter().map(|k| 1.0 - k).collect();
    // Sources: known pixels a blur's reach from the hole (their detail never saw it).
    let valid: Vec<bool> = {
        let near = box_sum(&inside, w, h, 2 * p.blur);
        (0..n).map(|i| !hole[i] && near[i] <= 0.0).collect()
    };
    let luma = |px: &[f32]| px.iter().take(cc).sum::<f32>() / cc as f32;
    let neutral = is_neutral(&(0..n).filter(|i| valid[*i]).map(|i| &fill[i * ch..i * ch + cc]).collect::<Vec<_>>());
    let lum: Vec<f32> = fill.chunks_exact(ch).map(luma).collect();
    let dots = if neutral { dot_screen(w, h, &lum, hole, &valid, ctl)? } else { None };
    let mut out = fill.to_vec();
    if let Some((sources, mut target)) = dots {
        // Whole pixels, guided by the fill's tone, where the hole's surroundings are a dot screen;
        // all or nothing, as a mix of the two is grey. A screen's tone is the density of its dots,
        // which the quilting follows, so the tone pass doesn't run; where the fill is plain paper
        // or solid ink, it stays.
        let values: Vec<f32> = fill.chunks_exact(ch).flat_map(|px| px.iter().take(cc).copied()).collect();
        let guide = {
            let masked: Vec<f32> = lum.iter().zip(&known).map(|(v, k)| v * k).collect();
            let kb = blur_twice(&masked, &known, w, h, GUIDE_BLUR);
            let fb = blur_twice(&lum.iter().zip(&inside).map(|(v, k)| v * k).collect::<Vec<_>>(), &inside, w, h, GUIDE_BLUR);
            (0..n).map(|i| if hole[i] { fb[i] } else { kb[i] }).collect::<Vec<f32>>()
        };
        for (t, g) in target.iter_mut().zip(&guide) {
            *t &= (PLAIN..=1.0 - PLAIN).contains(g);
        }
        let q = quilt(w, h, cc, &values, Some(&guide), hole, &target, &sources, b, o, p, ctl)?;
        for i in (0..n).filter(|i| target[*i]) {
            out[i * ch..i * ch + cc].copy_from_slice(&q[i * cc..(i + 1) * cc]);
        }
        return Ok(out);
    }
    // A black-and-white page has no grain, and its fill is lines and paper, which a tone shift
    // only greys and quilted detail only crosses with lines (user report): it stays as it is.
    if neutral && is_bilevel(&(0..n).filter(|i| valid[*i]).map(|i| lum[i]).collect::<Vec<_>>()) {
        return Ok(out);
    }
    if p.tone {
        settle_tone(w, h, ch, cc, &mut out, hole, &known, &valid, ctl)?;
    }
    // Detail per colour channel: the known pixels' against a blur of the known pixels only (the
    // fill doesn't leak into it), the hole's against a blur of the fill.
    let all = vec![1.0f32; n];
    let mut detail = vec![0f32; n * cc];
    for c in 0..cc {
        ctl.check()?;
        let plane: Vec<f32> = (0..n).map(|i| out[i * ch + c]).collect();
        let masked: Vec<f32> = plane.iter().zip(&known).map(|(v, k)| v * k).collect();
        let kb = blur_twice(&masked, &known, w, h, p.blur);
        let fb = blur_twice(&plane, &all, w, h, p.blur);
        for i in 0..n {
            detail[i * cc + c] = plane[i] - if hole[i] { fb[i] } else { kb[i] };
        }
    }
    let energy: Vec<f32> = detail.chunks_exact(cc).map(|d| d.iter().map(|v| v * v).sum::<f32>() / cc as f32).collect();
    let vw: Vec<f32> = valid.iter().map(|v| if *v { 1.0 } else { 0.0 }).collect();
    let count = valid.iter().filter(|v| **v).count();
    if count == 0 {
        return Ok(out);
    }
    let global = ((0..n).filter(|i| valid[*i]).map(|i| f64::from(energy[i])).sum::<f64>() / count as f64) as f32;
    // The surroundings' detail energy, spread into the hole (the global mean far from any known
    // pixel).
    let es = {
        let s = box_sum(&energy.iter().zip(&vw).map(|(e, v)| e * v).collect::<Vec<_>>(), w, h, ENERGY_RADIUS);
        let c = box_sum(&vw, w, h, ENERGY_RADIUS);
        let eps = 4.0;
        s.iter().zip(&c).map(|(s, c)| (s + global * eps) / (c + eps)).collect::<Vec<f32>>()
    };
    let ef = {
        let s = box_sum(&energy.iter().zip(&inside).map(|(e, v)| e * v).collect::<Vec<_>>(), w, h, FILL_RADIUS);
        let c = box_sum(&inside, w, h, FILL_RADIUS);
        s.iter().zip(&c).map(|(s, c)| if *c > 0.0 { s / c } else { 0.0 }).collect::<Vec<f32>>()
    };
    let gain: Vec<f32> = (0..n)
        .map(|i| {
            if !hole[i] || es[i] <= FLAT {
                return 0.0;
            }
            let faint = ((GRAIN_NONE - es[i].sqrt()) / (GRAIN_NONE - GRAIN_FULL)).clamp(0.0, 1.0);
            (1.0 - ef[i] / es[i]).clamp(0.0, 1.0) * faint
        })
        .collect();
    if gain.iter().sum::<f32>() / (holes as f32) < ENOUGH {
        return Ok(out);
    }
    let q = quilt(w, h, cc, &detail, None, hole, hole, &valid, b, o, p, ctl)?;
    for i in (0..n).filter(|i| hole[*i] && gain[*i] > 0.0) {
        for c in 0..cc {
            out[i * ch + c] += gain[i] * (q[i * cc + c] - detail[i * cc + c]);
        }
    }
    Ok(out)
}

/// Take the fill's small deviations in tone from a membrane interpolation of its surroundings
/// out (see the module docs).
#[allow(clippy::too_many_arguments)]
fn settle_tone(
    w: usize,
    h: usize,
    ch: usize,
    cc: usize,
    out: &mut [f32],
    hole: &[bool],
    known: &[f32],
    valid: &[bool],
    ctl: &Interrupt,
) -> Result<(), Cancelled> {
    let n = w * h;
    let inside: Vec<f32> = known.iter().map(|k| 1.0 - k).collect();
    // The membrane through the known pixels themselves: a blur of them would be biased at the
    // hole's edge (on a gradient, by the slope times the blur's reach on one side).
    let colours: Vec<f32> = out.chunks_exact(ch).flat_map(|px| px.iter().take(cc).copied()).collect();
    let membrane = crate::poisson::membrane_fill_with(w, h, cc, &colours, hole, ctl)?;
    // How much shape the surroundings have at the tone's scale: their tone less a coarser one.
    let mut shape = vec![0f32; n];
    for c in 0..cc {
        ctl.check()?;
        let masked: Vec<f32> = (0..n).map(|i| known[i] * colours[i * cc + c]).collect();
        let fine = blur_twice(&masked, known, w, h, TONE_BLUR);
        let coarse = blur_twice(&masked, known, w, h, 4 * TONE_BLUR);
        for i in (0..n).filter(|i| valid[*i]) {
            shape[i] += (fine[i] - coarse[i]).abs() / cc as f32;
        }
    }
    let vw: Vec<f32> = valid.iter().map(|v| if *v { 1.0 } else { 0.0 }).collect();
    let count = vw.iter().sum::<f32>().max(1.0);
    let global = shape.iter().sum::<f32>() / count;
    let around = {
        let s = box_sum(&shape, w, h, SHAPE_RADIUS);
        let c = box_sum(&vw, w, h, SHAPE_RADIUS);
        let eps = 4.0;
        s.iter().zip(&c).map(|(s, c)| (s + global * eps) / (c + eps)).collect::<Vec<f32>>()
    };
    for c in 0..cc {
        ctl.check()?;
        // The fill's deviation from the membrane, blurred over the hole only.
        let d: Vec<f32> = (0..n).map(|i| inside[i] * (colours[i * cc + c] - membrane[i * cc + c])).collect();
        let d = blur_twice(&d, &inside, w, h, TONE_BLUR);
        for i in (0..n).filter(|i| hole[*i]) {
            let smooth = ((SHAPED - around[i]) / (SHAPED - SMOOTH)).clamp(0.0, 1.0);
            let t4 = (TONE_KEEP + smooth * (SMOOTH_KEEP - TONE_KEEP)).powi(4);
            let d4 = d[i].powi(4);
            if d4.is_finite() {
                out[i * ch + c] -= d[i] * t4 / (d4 + t4);
            }
        }
    }
    Ok(())
}

/// A dot screen's sources (valid pixels in it) and the hole pixels to quilt from them.
type ScreenMasks = (Vec<bool>, Vec<bool>);

/// Where a black-and-white page's hole lies in a dot screen: the valid pixels of the screen
/// around the hole (quilting's sources) and the hole pixels to quilt, or `None` when there is no
/// screen around it. Patches near the hole are tested ([`is_lattice`]) on a coarse grid, and the
/// verdicts are carried into the hole by a membrane, so its middle is judged by all its sides
/// rather than by the nearest few pixels (user report: a screentone fill grew blurrier towards
/// the middle).
fn dot_screen(w: usize, h: usize, lum: &[f32], hole: &[bool], valid: &[bool], ctl: &Interrupt) -> Result<Option<ScreenMasks>, Cancelled> {
    let n = w * h;
    let half = LATTICE_PATCH / 2;
    let near = box_sum(&hole.iter().map(|m| if *m { 1.0 } else { 0.0 }).collect::<Vec<f32>>(), w, h, LATTICE_NEAR);
    let invalid = box_sum(&valid.iter().map(|v| if *v { 0.0 } else { 1.0 }).collect::<Vec<f32>>(), w, h, half);
    let (gw, gh) = (w.div_ceil(LATTICE_CELL), h.div_ceil(LATTICE_CELL));
    let mut cells = vec![0f32; gw * gh];
    let mut judged = vec![false; gw * gh];
    for gy in 0..gh {
        ctl.check()?;
        for gx in 0..gw {
            let (cx, cy) = (gx * LATTICE_CELL + LATTICE_CELL / 2, gy * LATTICE_CELL + LATTICE_CELL / 2);
            if cx < half || cy < half || cx + half > w || cy + half > h {
                continue;
            }
            let c = cy * w + cx;
            if near[c] <= 0.0 || invalid[c] > 0.0 {
                continue;
            }
            if let Some(dots) = is_lattice(lum, w, cx - half, cy - half) {
                judged[gy * gw + gx] = true;
                cells[gy * gw + gx] = if dots { 1.0 } else { 0.0 };
            }
        }
    }
    if !cells.iter().any(|v| *v > 0.0) {
        return Ok(None);
    }
    let unjudged: Vec<bool> = judged.iter().map(|j| !j).collect();
    let mut spread = cells.clone();
    crate::poisson::solve_membrane(gw, gh, &unjudged, &mut spread);
    let at = |x: usize, y: usize| {
        let u = ((x as f32 + 0.5) / LATTICE_CELL as f32 - 0.5).clamp(0.0, (gw - 1) as f32);
        let v = ((y as f32 + 0.5) / LATTICE_CELL as f32 - 0.5).clamp(0.0, (gh - 1) as f32);
        let (x0, y0) = (u.floor() as usize, v.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(gw - 1), (y0 + 1).min(gh - 1));
        let (fx, fy) = (u - x0 as f32, v - y0 as f32);
        let g = |x: usize, y: usize| spread[y * gw + x];
        (g(x0, y0) * (1.0 - fx) + g(x1, y0) * fx) * (1.0 - fy) + (g(x0, y1) * (1.0 - fx) + g(x1, y1) * fx) * fy
    };
    let target: Vec<bool> = (0..n).map(|i| hole[i] && at(i % w, i / w) >= 0.5).collect();
    if !target.iter().any(|t| *t) {
        return Ok(None);
    }
    // Sources: valid pixels in a cell judged a screen.
    let sources: Vec<bool> = (0..n)
        .map(|i| {
            let k = (i / w / LATTICE_CELL) * gw + (i % w) / LATTICE_CELL;
            valid[i] && judged[k] && cells[k] > 0.0
        })
        .collect();
    Ok(Some((sources, target)))
}

/// Whether the [`LATTICE_PATCH`]-square patch of `lum` (a plane `w` wide) at `(x0, y0)` is a dot
/// screen: it repeats (matches itself under a shift, and under twice that shift) along two
/// directions that aren't one line (a line, or hatching, repeats only along its own direction);
/// `None` when the patch is plain and can't tell.
fn is_lattice(lum: &[f32], w: usize, x0: usize, y0: usize) -> Option<bool> {
    let s = LATTICE_PATCH;
    let patch: Vec<f32> = (0..s * s).map(|k| lum.get((y0 + k / s) * w + x0 + k % s).copied().unwrap_or(0.0)).collect();
    let mean = patch.iter().sum::<f32>() / (s * s) as f32;
    let var = patch.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / (s * s) as f32;
    // Plain paper or ink says nothing either way (a screen's highlights have tiny dots).
    if var < LATTICE_FLAT || !var.is_finite() {
        return None;
    }
    let corr = |dx: i32, dy: i32| {
        let (mut sum, mut count) = (0f32, 0u32);
        for y in 0..s as i32 {
            let yy = y + dy;
            if yy < 0 || yy >= s as i32 {
                continue;
            }
            for x in 0..s as i32 {
                let xx = x + dx;
                if xx < 0 || xx >= s as i32 {
                    continue;
                }
                sum += (patch[(y as usize) * s + x as usize] - mean) * (patch[(yy as usize) * s + xx as usize] - mean);
                count += 1;
            }
        }
        if count == 0 { 0.0 } else { sum / (count as f32 * var) }
    };
    // Half the shifts: a shift and its opposite match equally.
    let shifts: Vec<(i32, i32, f32)> = (0..=LATTICE_SHIFT)
        .flat_map(|dy| (-LATTICE_SHIFT..=LATTICE_SHIFT).map(move |dx| (dx, dy)))
        .filter(|&(dx, dy)| (dy > 0 || dx > 0) && dx * dx + dy * dy >= LATTICE_MIN_SHIFT2)
        .map(|(dx, dy)| (dx, dy, corr(dx, dy)))
        .collect();
    // A repeat: a shift under which the patch matches itself, and matches again twice as far
    // (two lines a gap apart match once; a screen repeats).
    let repeats = |dx: i32, dy: i32, c: f32| c >= LATTICE_CORR && corr(2 * dx, 2 * dy) >= LATTICE_CORR;
    let faint = var < LATTICE_FAINT;
    let Some(&(ax, ay, _)) = shifts.iter().filter(|s| repeats(s.0, s.1, s.2)).max_by(|p, q| p.2.total_cmp(&q.2)) else {
        return (!faint).then_some(false);
    };
    let la = ((ax * ax + ay * ay) as f32).sqrt();
    let dots = shifts.iter().any(|&(bx, by, c)| {
        let lb = ((bx * bx + by * by) as f32).sqrt();
        ((ax * by - ay * bx) as f32).abs() > 0.5 * la * lb && repeats(bx, by, c)
    });
    (dots || !faint).then_some(dots)
}

/// Whether `tones` (0–1) are mostly black or white: at least [`BILEVEL_SHARE`] of them within
/// [`PLAIN`]·5 of 0 or 1.
fn is_bilevel(tones: &[f32]) -> bool {
    let ends = tones.iter().filter(|t| **t <= 5.0 * PLAIN || **t >= 1.0 - 5.0 * PLAIN).count();
    !tones.is_empty() && ends as f64 >= BILEVEL_SHARE * tones.len() as f64
}

/// Whether the surroundings' colours (`colours` per pixel, 0–1) are black and white or grey
/// (a manga page, even in an RGB document): dot screens are looked for there only.
fn is_neutral(pixels: &[&[f32]]) -> bool {
    if pixels.is_empty() {
        return false;
    }
    let grey = pixels.iter().filter(|px| px.iter().fold(f32::MAX, |a, v| a.min(*v)) + NEUTRAL_SPREAD >= px.iter().fold(f32::MIN, |a, v| a.max(*v))).count();
    grey as f64 >= NEUTRAL_SHARE * pixels.len() as f64
}

/// `values` (`vc` per pixel) quilted into the `target` pixels of the hole from the valid pixels'
/// (outside the hole, the known pixels' own). With a `guide` (one plane), a candidate also has to
/// match its tone.
#[allow(clippy::too_many_arguments)]
fn quilt(
    w: usize,
    h: usize,
    vc: usize,
    values: &[f32],
    guide: Option<&[f32]>,
    hole: &[bool],
    target: &[bool],
    valid: &[bool],
    b: usize,
    o: usize,
    p: &Params,
    ctl: &Interrupt,
) -> Result<Vec<f32>, Cancelled> {
    let n = w * h;
    // Block origins whose whole block is valid, by a summed-area table of the invalid pixels.
    let sat = {
        let mut s = vec![0u32; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0u32;
            for x in 0..w {
                row += u32::from(!valid[y * w + x]);
                s[(y + 1) * (w + 1) + x + 1] = s[y * (w + 1) + x + 1] + row;
            }
        }
        s
    };
    let ok = |x: usize, y: usize| {
        x + b <= w && y + b <= h && {
            let (x1, y1) = (x + b, y + b);
            sat[y1 * (w + 1) + x1] + sat[y * (w + 1) + x] == sat[y * (w + 1) + x1] + sat[y1 * (w + 1) + x]
        }
    };
    let origins: Vec<(u32, u32)> =
        (0..=h - b).flat_map(|y| (0..=w - b).map(move |x| (x, y))).filter(|&(x, y)| ok(x, y)).map(|(x, y)| (x as u32, y as u32)).collect();
    // The output: the known pixels' values, the hole's as it is quilted.
    let mut q: Vec<f32> = (0..n * vc).map(|k| if hole[k / vc] { 0.0 } else { values[k] }).collect();
    let mut set: Vec<bool> = hole.iter().map(|m| !*m).collect();
    if origins.is_empty() {
        return Ok(q);
    }
    let (mut bx0, mut by0, mut bx1, mut by1) = (w, h, 0, 0);
    for i in (0..n).filter(|i| target[*i]) {
        let (x, y) = (i % w, i / w);
        (bx0, by0, bx1, by1) = (bx0.min(x), by0.min(y), bx1.max(x + 1), by1.max(y + 1));
    }
    let step = b - o;
    // Block origins over the hole, the first overlapping the known pixels above and left of it.
    let along = |lo: usize, hi: usize, len: usize| {
        let mut v = Vec::new();
        let mut t = lo.saturating_sub(o).min(len - b);
        loop {
            v.push(t);
            if t + b >= hi || t + b >= len {
                break;
            }
            t = (t + step).min(len - b);
        }
        v
    };
    let (xs, ys) = (along(bx0, bx1, w), along(by0, by1, h));
    // How far the nearby candidates reach: past the hole's half-extent, into its surroundings.
    let reach = i32::try_from((bx1 - bx0).max(by1 - by0) / 2 + 4 * b).unwrap_or(i32::MAX / 2);
    let mut rng = Rng::new(p.seed);
    let mut placed: Vec<Option<(usize, usize)>> = vec![None; xs.len() * ys.len()];
    let mut cand: Vec<(usize, usize)> = Vec::with_capacity(p.candidates + 8);
    let mut scored: Vec<(f32, (usize, usize))> = Vec::with_capacity(p.candidates + 8);
    for (bj, &ty) in ys.iter().enumerate() {
        ctl.check()?;
        for (bi, &tx) in xs.iter().enumerate() {
            if !(ty..ty + b).any(|y| target[y * w + tx..y * w + tx + b].iter().any(|m| *m)) {
                continue;
            }
            cand.clear();
            // Continue the neighbours' sources.
            let near = |dbi: isize, dbj: isize| -> Option<(usize, usize)> {
                let (i, j) = (bi.checked_add_signed(dbi)?, bj.checked_add_signed(dbj)?);
                let (sx, sy) = (*placed.get(j * xs.len() + i)?)?;
                let sx = (sx as isize).checked_add(tx as isize - *xs.get(i)? as isize)?;
                let sy = (sy as isize).checked_add(ty as isize - *ys.get(j)? as isize)?;
                let (sx, sy) = (usize::try_from(sx).ok()?, usize::try_from(sy).ok()?);
                ok(sx, sy).then_some((sx, sy))
            };
            cand.extend([near(-1, 0), near(0, -1), near(-1, -1), near(1, -1)].into_iter().flatten());
            // Nearby sources (the grain changes across a picture), then any.
            let local = p.candidates / 2;
            let mut tries = 0;
            while cand.len() < local + 4 && tries < local * 8 {
                tries += 1;
                let x = tx as i64 + i64::from(rng.range(-reach, reach));
                let y = ty as i64 + i64::from(rng.range(-reach, reach));
                if let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y))
                    && ok(x, y)
                {
                    cand.push((x, y));
                }
            }
            while cand.len() < p.candidates + 4 {
                let Some(&(x, y)) = origins.get((rng.next() % origins.len() as u64) as usize) else { break };
                cand.push((x as usize, y as usize));
            }
            // Error over what is already set in the block, per pixel set, plus the guide's.
            scored.clear();
            for &(sx, sy) in &cand {
                let (mut e, mut m, mut g) = (0f32, 0u32, 0f32);
                for y in 0..b {
                    let (t, s) = ((ty + y) * w + tx, (sy + y) * w + sx);
                    for x in 0..b {
                        if set[t + x] {
                            for c in 0..vc {
                                let d = values[(s + x) * vc + c] - q[(t + x) * vc + c];
                                e += d * d;
                            }
                            m += 1;
                        }
                        if let Some(gd) = guide {
                            let d = gd[s + x] - gd[t + x];
                            g += d * d;
                        }
                    }
                }
                let overlap = if m > 0 { e / (m as f32 * vc as f32) } else { 0.0 };
                scored.push((overlap + GUIDE * g / (b * b) as f32, (sx, sy)));
            }
            let best = scored.iter().map(|s| s.0).fold(f32::MAX, f32::min);
            let good: Vec<(usize, usize)> = scored.iter().filter(|s| s.0 <= best * TOLERANCE + 1e-12).map(|s| s.1).collect();
            let Some(&(sx, sy)) = good.get((rng.next() % good.len().max(1) as u64) as usize) else { continue };
            // The minimum-error cuts through the left and top overlaps.
            let err = |x: usize, y: usize| {
                let (t, s) = ((ty + y) * w + tx + x, (sy + y) * w + sx + x);
                if !set[t] {
                    return 0.0;
                }
                (0..vc).map(|c| (values[s * vc + c] - q[t * vc + c]).powi(2)).sum::<f32>()
            };
            let vcut = cut(b, o, |along, across| err(across, along));
            let hcut = cut(b, o, err);
            for (y, &vy) in vcut.iter().enumerate() {
                for (x, &hx) in hcut.iter().enumerate() {
                    let t = (ty + y) * w + tx + x;
                    if !target[t] || (set[t] && (x < vy || y < hx)) {
                        continue;
                    }
                    let s = (sy + y) * w + sx + x;
                    q[t * vc..(t + 1) * vc].copy_from_slice(&values[s * vc..(s + 1) * vc]);
                    set[t] = true;
                }
            }
            placed[bj * xs.len() + bi] = Some((sx, sy));
        }
    }
    Ok(q)
}

/// The minimum-error path through an overlap `o` wide along a `b`-long block edge: for each
/// position along the edge, the first offset across it that the new block takes. `err(along,
/// across)` is the error of a pixel; positions past the overlap are the new block's.
fn cut(b: usize, o: usize, err: impl Fn(usize, usize) -> f32) -> Vec<usize> {
    // Cumulative error, row by row along the edge, each step moving at most one across.
    let mut cost = vec![0f32; b * o];
    for a in 0..b {
        for c in 0..o {
            let prev = if a == 0 {
                0.0
            } else {
                let row = &cost[(a - 1) * o..a * o];
                row[c.saturating_sub(1)..=(c + 1).min(o - 1)].iter().copied().fold(f32::MAX, f32::min)
            };
            cost[a * o + c] = err(a, c) + prev;
        }
    }
    let mut path = vec![0usize; b];
    let last = &cost[(b - 1) * o..b * o];
    let mut c = (0..o).min_by(|&i, &j| last[i].total_cmp(&last[j])).unwrap_or(0);
    for a in (0..b).rev() {
        path[a] = c;
        if a > 0 {
            let row = &cost[(a - 1) * o..a * o];
            c = (c.saturating_sub(1)..=(c + 1).min(o - 1)).min_by(|&i, &j| row[i].total_cmp(&row[j])).unwrap_or(c);
        }
    }
    path
}

/// Sum over a `(2r + 1)²` box around each pixel of a `w × h` plane (what lies past the edges
/// counts as zero).
fn box_sum(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let pass = |src: &[f32], along_x: bool| -> Vec<f32> {
        let mut out = vec![0f32; src.len()];
        let (len, lines) = if along_x { (w, h) } else { (h, w) };
        let idx = |line: usize, i: usize| if along_x { line * w + i } else { i * w + line };
        let mut pre = vec![0f64; len + 1];
        for line in 0..lines {
            for i in 0..len {
                pre[i + 1] = pre[i] + f64::from(src[idx(line, i)]);
            }
            for i in 0..len {
                let (a, b) = (i.saturating_sub(r), (i + r + 1).min(len));
                out[idx(line, i)] = (pre[b] - pre[a]) as f32;
            }
        }
        out
    };
    let x = pass(v, true);
    pass(&x, false)
}

/// `values` (already multiplied by `weights`) box-blurred twice with radius `r`, normalised by the
/// weights: the mean of the weighted pixels near each one.
fn blur_twice(values: &[f32], weights: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let (mut s, mut c) = (box_sum(values, w, h, r), box_sum(weights, w, h, r));
    s = box_sum(&s, w, h, r);
    c = box_sum(&c, w, h, r);
    s.iter().zip(&c).map(|(s, c)| if *c > 1e-6 { s / c } else { 0.0 }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(w: usize, h: usize, seed: u64, amp: f32) -> Vec<f32> {
        let mut r = Rng::new(seed);
        (0..w * h * 3).map(|_| 0.5 + amp * ((r.next() % 2001) as f32 / 1000.0 - 1.0)).collect()
    }

    fn disc(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> Vec<bool> {
        (0..w * h).map(|i| ((i % w) as f32 - cx).hypot((i / w) as f32 - cy) <= r).collect()
    }

    fn mean_in(img: &[f32], mask: &[bool], ch: usize) -> f32 {
        let v: Vec<f32> = mask.iter().enumerate().filter(|(_, m)| **m).map(|(i, _)| img[i * ch]).collect();
        v.iter().sum::<f32>() / v.len() as f32
    }

    fn std_in(img: &[f32], mask: &[bool], ch: usize) -> f32 {
        let m = mean_in(img, mask, ch);
        let v: Vec<f32> = mask.iter().enumerate().filter(|(_, m)| **m).map(|(i, _)| img[i * ch]).collect();
        (v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
    }

    fn filled(img: &[f32], hole: &[bool], f: impl Fn(usize) -> [f32; 3]) -> Vec<f32> {
        let mut out = img.to_vec();
        for (i, m) in hole.iter().enumerate() {
            if *m {
                out[i * 3..i * 3 + 3].copy_from_slice(&f(i));
            }
        }
        out
    }

    /// A flat fill in a grainy area gets the grain back, and keeps its tone; the known pixels stay.
    #[test]
    fn a_smooth_fill_in_grain_gets_the_grain_back() {
        let (w, h) = (160, 160);
        let img = noise(w, h, 3, 0.04);
        let hole = disc(w, h, 80.0, 80.0, 40.0);
        let out = restore(w, h, 3, 3, &filled(&img, &hole, |_| [0.5; 3]), &hole, &Interrupt::NONE).unwrap();
        let ring: Vec<bool> = (0..w * h).map(|i| !hole[i] && disc(w, h, 80.0, 80.0, 60.0)[i]).collect();
        let (got, want) = (std_in(&out, &hole, 3), std_in(&img, &ring, 3));
        assert!(got > 0.7 * want && got < 1.3 * want, "grain {got} against {want} around");
        assert!((mean_in(&out, &hole, 3) - 0.5).abs() < 0.01, "tone {}", mean_in(&out, &hole, 3));
        for i in (0..w * h).filter(|i| !hole[*i]) {
            assert_eq!(out[i * 3..i * 3 + 3], img[i * 3..i * 3 + 3]);
        }
    }

    /// User report (a blue sky): the fill wandered 2–3 levels in tone, a blotch once the contrast
    /// went up. Small wanderings go; a shape the model drew (40 levels) stays.
    #[test]
    fn small_wanderings_in_tone_go_and_drawn_shapes_stay() {
        let (w, h) = (160, 160);
        let ramp: Vec<f32> = (0..w * h).flat_map(|i| [0.3 + 0.2 * (i / w) as f32 / h as f32; 3]).collect();
        let hole = disc(w, h, 80.0, 80.0, 45.0);
        let blotch = |i: usize| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let v = ramp[i * 3] + 1.5 / 255.0 * ((x / 9.0).sin() + (y / 13.0).cos());
            [v; 3]
        };
        let out = restore(w, h, 3, 3, &filled(&ramp, &hole, blotch), &hole, &Interrupt::NONE).unwrap();
        let worst = (0..w * h).filter(|i| hole[*i]).map(|i| (out[i * 3] - ramp[i * 3]).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.75 / 255.0, "still {:.2} levels off", worst * 255.0);
        // A dark square the model drew in the middle stays dark.
        let square = |i: usize| if ((i % w) as i32 - 80).abs() < 15 && ((i / w) as i32 - 80).abs() < 15 { [0.1; 3] } else { [ramp[i * 3]; 3] };
        let out = restore(w, h, 3, 3, &filled(&ramp, &hole, square), &hole, &Interrupt::NONE).unwrap();
        assert!(out[(80 * w + 80) * 3] < 0.13, "{}", out[(80 * w + 80) * 3]);
    }

    /// A fill that already has its surroundings' detail and tone is left alone, and so is a flat
    /// area's.
    #[test]
    fn a_fill_that_fits_is_left_alone() {
        let (w, h) = (128, 128);
        let img = noise(w, h, 5, 0.04);
        let hole = disc(w, h, 64.0, 64.0, 30.0);
        let out = restore(w, h, 3, 3, &img, &hole, &Interrupt::NONE).unwrap();
        // The grain (6 levels' deviation) may have its own lowest frequencies evened out a little.
        let moved: Vec<f32> = (0..w * h * 3).filter(|k| hole[k / 3]).map(|k| (out[k] - img[k]).abs()).collect();
        let (mean, max) = (moved.iter().sum::<f32>() / (moved.len() as f32), moved.iter().copied().fold(0.0f32, f32::max));
        assert!(mean < 1.0 / 255.0 && max < 3.0 / 255.0, "moved {} levels on average, {} at most", mean * 255.0, max * 255.0);
        let flat = vec![0.3f32; w * h * 3];
        let out = restore(w, h, 3, 3, &flat, &hole, &Interrupt::NONE).unwrap();
        assert!(out.iter().all(|v| (v - 0.3).abs() < 1e-5));
    }

    /// User report (a halftone page): the model's dots were grey and irregular. A black-and-white
    /// screen is quilted whole: the hole gets crisp dots in phase with the screen it cut.
    #[test]
    fn a_halftone_screen_continues_crisp_and_in_phase() {
        let (w, h) = (192, 192);
        let dot = |x: usize, y: usize| if (x % 6 < 3) == (y % 6 < 3) { 1.0 } else { 0.0 };
        let img: Vec<f32> = (0..w * h).flat_map(|i| [dot(i % w, i / w); 3]).collect();
        let hole = disc(w, h, 96.0, 96.0, 36.0);
        let out = restore(w, h, 3, 3, &filled(&img, &hole, |_| [0.5; 3]), &hole, &Interrupt::NONE).unwrap();
        let n = hole.iter().filter(|m| **m).count() as f32;
        let err = hole.iter().enumerate().filter(|(_, m)| **m).map(|(i, _)| (out[i * 3] - img[i * 3]).abs()).sum::<f32>() / n;
        assert!(err < 0.05, "mean error {err} against the screen");
    }

    /// A page of round dots (period 8) on white.
    fn dots(x: usize, y: usize) -> f32 {
        let (dx, dy) = ((x % 8) as f32 - 3.5, (y % 8) as f32 - 3.5);
        if dx.hypot(dy) < 2.6 { 0.0 } else { 1.0 }
    }

    /// Pen lines on white: diagonal, at irregular spacing (about 9 px apart), over the left two
    /// thirds.
    fn hatching(x: usize, y: usize) -> f32 {
        let t = (x + y / 2) as u64;
        let line = crate::photo_util::splitmix64(&mut t.wrapping_mul(0x9E37)).is_multiple_of(9);
        if x < 170 && line { 0.0 } else { 1.0 }
    }

    /// User report: the middle of a large screentone fill was blurrier than its edges. The whole
    /// hole gets crisp dots (no grey), as dense as around it.
    #[test]
    fn a_large_hole_in_a_dot_screen_gets_crisp_dots_to_its_middle() {
        let (w, h) = (256, 256);
        let img: Vec<f32> = (0..w * h).flat_map(|i| [dots(i % w, i / w); 3]).collect();
        let hole = disc(w, h, 128.0, 128.0, 75.0);
        // The model's fill: grey, a blur of the screen.
        let out = restore(w, h, 3, 3, &filled(&img, &hole, |_| [0.8; 3]), &hole, &Interrupt::NONE).unwrap();
        let middle = disc(w, h, 128.0, 128.0, 25.0);
        let grey = (0..w * h).filter(|i| hole[*i] && (0.1..0.9).contains(&out[i * 3])).count();
        assert_eq!(grey, 0, "grey pixels in the fill");
        let dark = |m: &[bool], v: &[f32]| (0..w * h).filter(|i| m[*i] && v[i * 3] < 0.5).count() as f32 / m.iter().filter(|m| **m).count() as f32;
        let (got, want) = (dark(&middle, &out), dark(&middle, &img));
        assert!((got - want).abs() < 0.05, "dots cover {got} of the middle, {want} around");
    }

    /// User report: a fine screentone (45°, about 4.6 px, soft-edged dots, as scanned or
    /// resized) kept LaMa's grey fill: its pixels aren't pure black and white, and its highlights'
    /// tiny dots failed the screen test. The whole hole gets the screen, in tone with the fill.
    #[test]
    fn a_fine_soft_screen_is_filled_with_its_dots() {
        let (w, h) = (256, 256);
        // Drawn at 4× and averaged: soft edges, a period that isn't a whole number of pixels.
        let ink = |x: f32, y: f32| {
            let tone = (y / 256.0).clamp(0.05, 0.95);
            let (u, v) = ((x + y) / 2f32.sqrt() / 4.6 * std::f32::consts::TAU, (x - y) / 2f32.sqrt() / 4.6 * std::f32::consts::TAU);
            if tone > (u.cos() + v.cos()) / 4.0 + 0.5 { 1.0 } else { 0.0 }
        };
        let img: Vec<f32> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let s: f32 = (0..16).map(|k| ink(x + (k % 4) as f32 / 4.0, y + (k / 4) as f32 / 4.0)).sum();
                [1.0 - s / 16.0; 3]
            })
            .collect();
        let hole = disc(w, h, 128.0, 128.0, 60.0);
        // The model's fill: the screen's tone, grey.
        let blur = blur_twice(&img.iter().step_by(3).copied().collect::<Vec<_>>(), &vec![1.0; w * h], w, h, 4);
        let out = restore(w, h, 3, 3, &filled(&img, &hole, |i| [blur[i]; 3]), &hole, &Interrupt::NONE).unwrap();
        let n = hole.iter().filter(|m| **m).count() as f32;
        let grey = (0..w * h).filter(|i| hole[*i] && (0.3..0.7).contains(&out[i * 3])).count() as f32 / n;
        let around = (0..w * h).filter(|i| !hole[*i] && (0.3..0.7).contains(&img[i * 3])).count() as f32 / (w * h) as f32 / (1.0 - n / (w * h) as f32);
        assert!(grey < around * 1.5, "{grey} of the fill is mid-grey, {around} of the screen");
        let mean = |v: &[f32]| (0..w * h).filter(|i| hole[*i]).map(|i| v[i * 3]).sum::<f32>() / n;
        assert!((mean(&out) - mean(&img)).abs() < 0.05, "tone {} against {}", mean(&out), mean(&img));
    }

    /// User report: on a manga page the pass greyed the model's lines and drew hatching where the
    /// model had left paper. Line art isn't a dot screen: the model's fill stays as it is.
    #[test]
    fn line_art_keeps_the_models_fill() {
        let (w, h) = (256, 160);
        let img: Vec<f32> = (0..w * h).flat_map(|i| [hatching(i % w, i / w); 3]).collect();
        let hole = disc(w, h, 150.0, 80.0, 45.0);
        // The model left paper where the lines thin out, and a soft line elsewhere.
        let fill = filled(&img, &hole, |i| if i % w < 150 { [0.6; 3] } else { [1.0; 3] });
        assert_eq!(restore(w, h, 3, 3, &fill, &hole, &Interrupt::NONE).unwrap(), fill);
        assert_ne!(is_lattice(&img.iter().step_by(3).copied().collect::<Vec<_>>(), w, 40, 40), Some(true));
        let screen: Vec<f32> = (0..w * h).map(|i| dots(i % w, i / w)).collect();
        assert_eq!(is_lattice(&screen, w, 40, 40), Some(true));
        assert_eq!(is_lattice(&vec![1.0; w * h], w, 40, 40), None, "paper doesn't vote");
    }

    /// Strong texture on a photo (not black and white) is the model's to draw: only faint grain
    /// is quilted.
    #[test]
    fn strong_texture_is_not_quilted() {
        let (w, h) = (160, 160);
        let img = noise(w, h, 9, 0.3);
        let hole = disc(w, h, 80.0, 80.0, 40.0);
        let fill = filled(&img, &hole, |_| [0.5; 3]);
        let out = restore_with(w, h, 3, 3, &fill, &hole, &Params { tone: false, ..Params::default() }, &Interrupt::NONE).unwrap();
        assert_eq!(out, fill);
    }

    #[test]
    fn grey_pages_are_told_from_colour_ones() {
        assert!(is_bilevel(&(0..100).map(|i| if i % 3 == 0 { 0.05 } else { 0.97 }).collect::<Vec<_>>()));
        assert!(!is_bilevel(&(0..100).map(|i| i as f32 / 100.0).collect::<Vec<_>>()));
        let grey: Vec<[f32; 3]> = (0..100).map(|i| [i as f32 / 100.0; 3]).collect();
        assert!(is_neutral(&grey.iter().map(|p| &p[..]).collect::<Vec<_>>()));
        let colour: Vec<[f32; 3]> = (0..100).map(|i| [i as f32 / 100.0, 0.5, 0.2]).collect();
        assert!(!is_neutral(&colour.iter().map(|p| &p[..]).collect::<Vec<_>>()));
        assert!(!is_neutral(&[]));
    }

    #[test]
    fn wrong_sizes_and_odd_inputs_come_back_unchanged() {
        let img = vec![0.5f32; 30];
        assert_eq!(restore(10, 1, 3, 3, &img, &[true; 3], &Interrupt::NONE).unwrap(), img);
        assert_eq!(restore(2, 5, 3, 3, &img, &[true; 10], &Interrupt::NONE).unwrap(), img);
        assert_eq!(restore(5, 2, 3, 0, &img, &[false; 10], &Interrupt::NONE).unwrap(), img);
        assert_eq!(restore(0, 0, 3, 3, &[], &[], &Interrupt::NONE).unwrap(), Vec::<f32>::new());
        let nan = vec![f32::NAN; 64 * 64 * 3];
        let hole = disc(64, 64, 32.0, 32.0, 10.0);
        assert_eq!(restore(64, 64, 3, 3, &nan, &hole, &Interrupt::NONE).unwrap().len(), nan.len());
    }
}
