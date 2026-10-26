//! Camera Raw's develop as a program for the GPU (run by `photocraft-gpu`'s Camera Raw kernels),
//! as Photoshop's Camera Raw draws its preview on the graphics processor.
//!
//! [`program`] turns settings and the image-wide measurements of a develop ([`Context`]: Dehaze's
//! haze, the Shadows and Highlights map, Contrast's pivot, Whites and Blacks' amounts) into
//! passes over the image:
//!
//! - **pixel passes**: one dispatch runs a list of per-pixel ops (the stages in order, each
//!   decoding and encoding as [`develop`](super::develop) does) over every pixel; ops may also
//!   read and write full-size planes at the pixel (Texture and Clarity, Detail);
//! - **plane passes**: box means, block means, the guided filter's coefficients and their
//!   evaluation, gradients, on single-channel planes (the blurs of Texture, Clarity, noise
//!   reduction, sharpening and defringe);
//! - **a resample pass** for Manual Distortion.
//!
//! The tables the stages read (the encoding's curves, every tone curve as Camera Raw's linear-light
//! table, the measured Vibrance, Color Mixer, Color Grading and vignette tables, the measured maps)
//! are built here with the CPU stages' own code, so the GPU result is the CPU's
//! ([`super::develop_cached`] with the same measurements) to float rounding. Nothing image-wide is
//! measured on the GPU: a develop that needs a measurement the context lacks is refused.

use super::{CameraRaw, Context, Encoding, Frame, acr, whites_blacks};

/// Pixel ops. `photocraft-gpu` declares them in its shader from [`op::ALL`], so the numbers
/// live here only.
pub mod op {
    pub const END: u32 = 0;
    /// `matrix?(1) m(9) tint`
    pub const CALIBRATE: u32 = 1;
    /// `g(3)`
    pub const WHITE_BALANCE: u32 = 2;
    /// `lut past`: a grey curve, hue-preserving in linear ProPhoto.
    pub const HUE_PRESERVING: u32 = 3;
    /// `(lut past) × 3` (lut `NONE` leaves the channel).
    pub const PER_CHANNEL: u32 = 4;
    pub const SAVE: u32 = 5;
    /// `refine`
    pub const REFINE: u32 = 6;
    /// `vibrance saturation k t hue levels`
    pub const VIBRANCE_SATURATION: u32 = 7;
    /// `hue(8) sat(8) lum(8) weights shift gain lum_l lum_c`
    pub const HSL: u32 = 8;
    /// `mix(8)`
    pub const GRAY_MIX: u32 = 9;
    /// `lut past n (strength dir(3) mask add keep) × n` (lut `NONE`: no luminance curve)
    pub const GRADE: u32 = 10;
    /// `air(3) t tw th fx fy amount` (`t` in [`GpuProgram::maps`])
    pub const DEHAZE: u32 = 11;
    /// `ma mb mw mh fx fy` (`ma`, `mb` in [`GpuProgram::maps`])
    pub const TONE_MAP: u32 = 12;
    /// `amount start cx cy diag`
    pub const LENS_VIGNETTE: u32 = 13;
    /// `n (amount lo hi) × n edge`
    pub const DEFRINGE: u32 = 14;
    /// `dst`: the luminance of the pixel's values into a plane.
    pub const LUMA: u32 = 15;
    /// `l lc`
    pub const LUMA_PLANES: u32 = 16;
    /// `l lc clar tex clarity texture` (plane `NONE`: that control is off)
    pub const TEXTURE_CLARITY: u32 = 17;
    /// `y cb cr`
    pub const DETAIL_SPLIT: u32 = 18;
    /// `y smooth wide keep contrast`
    pub const NOISE_LUMINANCE: u32 = 19;
    /// `c blurred wide smooth keep`
    pub const NOISE_COLOR: u32 = 20;
    /// `y blur grad amount detail threshold`
    pub const SHARPEN: u32 = 21;
    /// `y cb cr`
    pub const DETAIL_JOIN: u32 = 22;
    /// `style amount highlights ax ay circle round power centre width cx cy rows (amount lut past) × rows`
    pub const VIGNETTE: u32 = 23;
    /// `cell rough amount seed pscale`
    pub const GRAIN: u32 = 24;
    pub const CLAMP: u32 = 25;

    /// Every op with its name, for the shader's declarations.
    pub const ALL: [(&str, u32); 26] = [
        ("END", END),
        ("CALIBRATE", CALIBRATE),
        ("WHITE_BALANCE", WHITE_BALANCE),
        ("HUE_PRESERVING", HUE_PRESERVING),
        ("PER_CHANNEL", PER_CHANNEL),
        ("SAVE", SAVE),
        ("REFINE", REFINE),
        ("VIBRANCE_SATURATION", VIBRANCE_SATURATION),
        ("HSL", HSL),
        ("GRAY_MIX", GRAY_MIX),
        ("GRADE", GRADE),
        ("DEHAZE", DEHAZE),
        ("TONE_MAP", TONE_MAP),
        ("LENS_VIGNETTE", LENS_VIGNETTE),
        ("DEFRINGE", DEFRINGE),
        ("LUMA", LUMA),
        ("LUMA_PLANES", LUMA_PLANES),
        ("TEXTURE_CLARITY", TEXTURE_CLARITY),
        ("DETAIL_SPLIT", DETAIL_SPLIT),
        ("NOISE_LUMINANCE", NOISE_LUMINANCE),
        ("NOISE_COLOR", NOISE_COLOR),
        ("SHARPEN", SHARPEN),
        ("DETAIL_JOIN", DETAIL_JOIN),
        ("VIGNETTE", VIGNETTE),
        ("GRAIN", GRAIN),
        ("CLAMP", CLAMP),
    ];
}

/// Plane ops (see [`GpuPass::Plane`]).
pub mod plane {
    /// Box mean along rows: `a` → `b`, radius `n`, edges repeated.
    pub const BOX_H: u32 = 1;
    /// Box mean along columns.
    pub const BOX_V: u32 = 2;
    /// Means of `n × n` blocks: `a` (`w × h`) → `b` (`⌈w/n⌉ × ⌈h/n⌉`).
    pub const BLOCK: u32 = 3;
    /// `b = a²`.
    pub const SQUARE: u32 = 4;
    /// The guided filter's coefficients of a plane against itself from its box means: `a` the
    /// mean, `b` the mean of the squares → `c` (a), `d` (b); `f` is ε.
    pub const GUIDED_AB: u32 = 5;
    /// `d = bilinear(a) · c + bilinear(b)`, `a` and `b` laid over `c` (`w × h`) in `n × n` cells.
    pub const EVAL: u32 = 6;
    /// `b` = the length of `a`'s central-difference gradient.
    pub const GRADIENT: u32 = 7;

    pub const ALL: [(&str, u32); 7] = [("BOX_H", BOX_H), ("BOX_V", BOX_V), ("BLOCK", BLOCK), ("SQUARE", SQUARE), ("GUIDED_AB", GUIDED_AB), ("EVAL", EVAL), ("GRADIENT", GRADIENT)];
}

/// No plane or table here.
pub const NONE: u32 = u32::MAX;

/// The largest box radius a plane pass takes (see `Builder::box_mean`).
pub const MAX_RADIUS: usize = 4096;

/// Words at the start of [`GpuProgram::ops`]: the encoding (gamma bits; its decode and encode
/// tables; sRGB's), linear document RGB to linear ProPhoto and back, ProPhoto's luminance row.
pub const HEADER: usize = 26;

/// One pass of a [`GpuProgram`].
#[derive(Clone, Debug, PartialEq)]
pub enum GpuPass {
    /// The pixel ops from word `start` of [`GpuProgram::ops`] to their `END`, over the image.
    Pixel { start: u32 },
    /// Manual Distortion: every pixel takes the image at its radius scaled by `1 − k r²`
    /// (see [`super::distortion_source`]), bilinearly, edges repeated.
    Distort { k: f32, cx: f32, cy: f32, diag: f32 },
    /// A [`plane`] op over a `w × h` plane; `a`…`d` are plane offsets (in floats) or [`NONE`].
    Plane { kind: u32, a: u32, b: u32, c: u32, d: u32, w: u32, h: u32, n: u32, f: f32 },
}

/// A develop as GPU passes (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct GpuProgram {
    pub passes: Vec<GpuPass>,
    /// The pixel ops: the [`HEADER`], then each pixel pass's ops ending in [`op::END`].
    pub ops: Vec<u32>,
    pub table: Vec<f32>,
    /// The measurements whose maps (Dehaze's transmission, the Shadows and Highlights
    /// coefficients) the passes read, laid out by [`map_data`]: kept apart from [`Self::table`] so
    /// a develop with the same measurements doesn't copy or upload them again.
    pub maps: Option<std::sync::Arc<Context>>,
    /// Floats of plane storage the passes use.
    pub plane_floats: usize,
    pub width: u32,
    pub height: u32,
    /// The frame the pixels lie in (positions of vignettes, grain and distortion).
    pub frame_x0: i32,
    pub frame_y0: i32,
    pub full_w: u32,
    pub full_h: u32,
}

impl GpuProgram {
    /// Nothing to run: the develop is an identity.
    pub fn is_empty(&self) -> bool {
        self.passes.is_empty()
    }
}

/// Plane storage handed out by size; blocks given back are reused by later planes of that size.
#[derive(Default)]
struct Planes {
    blocks: Vec<(u32, usize, bool)>,
    end: usize,
}

impl Planes {
    fn take(&mut self, len: usize) -> Result<u32, String> {
        if let Some(b) = self.blocks.iter_mut().find(|b| b.2 && b.1 == len) {
            b.2 = false;
            return Ok(b.0);
        }
        let off = u32::try_from(self.end).map_err(|_| "Camera Raw GPU planes too large".to_string())?;
        self.end = self.end.checked_add(len).ok_or("Camera Raw GPU planes too large")?;
        self.blocks.push((off, len, false));
        Ok(off)
    }
    fn give(&mut self, off: u32) {
        if let Some(b) = self.blocks.iter_mut().find(|b| b.0 == off) {
            b.2 = true;
        }
    }
}

/// A plane: its offset and size.
#[derive(Clone, Copy)]
struct Plane {
    off: u32,
    w: usize,
    h: usize,
}

struct Builder {
    ops: Vec<u32>,
    table: Vec<f32>,
    passes: Vec<GpuPass>,
    planes: Planes,
    /// Where the open pixel pass's ops start, if one is open.
    open: Option<u32>,
    w: usize,
    h: usize,
    /// Offsets of tables already added (the measured ones are added once).
    shared: Vec<(usize, u32)>,
}

fn bits(v: f32) -> u32 {
    v.to_bits()
}

impl Builder {
    fn table(&mut self, data: &[f32]) -> Result<u32, String> {
        let off = u32::try_from(self.table.len()).map_err(|_| "Camera Raw GPU tables too large".to_string())?;
        self.table.extend_from_slice(data);
        Ok(off)
    }
    /// A table that never changes (a measured one), added once whatever asks for it.
    fn shared(&mut self, data: &'static [f32]) -> Result<u32, String> {
        let key = data.as_ptr() as usize;
        if let Some((_, off)) = self.shared.iter().find(|(k, _)| *k == key) {
            return Ok(*off);
        }
        let off = self.table(data)?;
        self.shared.push((key, off));
        Ok(off)
    }
    /// A tone curve as Camera Raw's linear-light table: its offset and its slope past 1.
    fn light(&mut self, tone: &acr::Tone) -> Result<(u32, f32), String> {
        let (lut, past) = acr::light_lut(tone);
        Ok((self.table(&lut)?, past))
    }
    /// Starts an op in the open pixel pass (opening one).
    fn op(&mut self, code: u32, args: &[u32]) {
        if self.open.is_none() {
            self.open = Some(self.ops.len() as u32);
        }
        self.ops.push(code);
        self.ops.extend_from_slice(args);
    }
    /// Ends the open pixel pass, so plane passes can read what it wrote.
    fn flush(&mut self) {
        if let Some(start) = self.open.take() {
            self.ops.push(op::END);
            self.passes.push(GpuPass::Pixel { start });
        }
    }
    fn full(&mut self) -> Result<Plane, String> {
        let (w, h) = (self.w, self.h);
        Ok(Plane { off: self.planes.take(w * h)?, w, h })
    }
    fn sized(&mut self, w: usize, h: usize) -> Result<Plane, String> {
        Ok(Plane { off: self.planes.take(w.saturating_mul(h).max(1))?, w, h })
    }
    fn give(&mut self, p: Plane) {
        self.planes.give(p.off);
    }
    fn plane_op(&mut self, kind: u32, src: Plane, abcd: [u32; 4], n: usize, f: f32) {
        self.flush();
        let [a, b, c, d] = abcd;
        self.passes.push(GpuPass::Plane { kind, a, b, c, d, w: src.w as u32, h: src.h as u32, n: n as u32, f });
    }
    /// [`super::box_mean`]: a new plane (a copy when `r` is 0). The GPU sums every tap, where the
    /// CPU slides a running sum: a radius past [`MAX_RADIUS`] (only absurd settings reach one) is
    /// refused, so the caller develops on the CPU instead of stalling the device.
    fn box_mean(&mut self, src: Plane, r: usize) -> Result<Plane, String> {
        if r > MAX_RADIUS {
            return Err(format!("Camera Raw GPU: a blur radius of {r} px is left to the CPU"));
        }
        let tmp = self.sized(src.w, src.h)?;
        let out = self.sized(src.w, src.h)?;
        self.plane_op(plane::BOX_H, src, [src.off, tmp.off, NONE, NONE], r, 0.0);
        self.plane_op(plane::BOX_V, tmp, [tmp.off, out.off, NONE, NONE], r, 0.0);
        self.give(tmp);
        Ok(out)
    }
    /// [`super::gauss`] (three box passes): a new plane.
    fn gauss(&mut self, src: Plane, sigma: f32) -> Result<Plane, String> {
        let radii = if sigma < 0.3 { [0, 0, 0] } else { crate::fxutil::gauss_box_radii(sigma) };
        let mut cur = self.box_mean(src, radii[0])?;
        for r in radii.into_iter().skip(1) {
            if r == 0 {
                continue;
            }
            let next = self.box_mean(cur, r)?;
            self.give(cur);
            cur = next;
        }
        Ok(cur)
    }
    /// The smoothed coefficients of the guided filter of `i` against itself (radius `r`).
    fn coefficients(&mut self, i: Plane, r: usize, eps: f32) -> Result<(Plane, Plane), String> {
        let mi = self.box_mean(i, r)?;
        let sq = self.sized(i.w, i.h)?;
        self.plane_op(plane::SQUARE, i, [i.off, sq.off, NONE, NONE], 0, 0.0);
        let mii = self.box_mean(sq, r)?;
        self.give(sq);
        let (a, b) = (self.sized(i.w, i.h)?, self.sized(i.w, i.h)?);
        self.plane_op(plane::GUIDED_AB, i, [mi.off, mii.off, a.off, b.off], 0, eps);
        self.give(mi);
        self.give(mii);
        let (ma, mb) = (self.box_mean(a, r)?, self.box_mean(b, r)?);
        self.give(a);
        self.give(b);
        Ok((ma, mb))
    }
    /// [`super::guided`] of a plane against itself: a new plane.
    fn guided(&mut self, i: Plane, r: usize, eps: f32) -> Result<Plane, String> {
        let (ma, mb) = self.coefficients(i, r, eps)?;
        let out = self.sized(i.w, i.h)?;
        self.plane_op(plane::EVAL, i, [ma.off, mb.off, i.off, out.off], 1, 0.0);
        self.give(ma);
        self.give(mb);
        Ok(out)
    }
    /// [`super::guided_fast`] of a plane against itself: a new plane.
    fn guided_fast(&mut self, i: Plane, r: usize, eps: f32) -> Result<Plane, String> {
        let s = (r / 8).clamp(1, 8);
        if s == 1 {
            return self.guided(i, r, eps);
        }
        let small = self.sized(i.w.div_ceil(s), i.h.div_ceil(s))?;
        self.plane_op(plane::BLOCK, i, [i.off, small.off, NONE, NONE], s, 0.0);
        let (ma, mb) = self.coefficients(small, (r / s).max(1), eps)?;
        self.give(small);
        let out = self.sized(i.w, i.h)?;
        self.plane_op(plane::EVAL, i, [ma.off, mb.off, i.off, out.off], s, 0.0);
        self.give(ma);
        self.give(mb);
        Ok(out)
    }
    fn gradient(&mut self, src: Plane) -> Result<Plane, String> {
        let out = self.sized(src.w, src.h)?;
        self.plane_op(plane::GRADIENT, src, [src.off, out.off, NONE, NONE], 0, 0.0);
        Ok(out)
    }
    fn hue_preserving(&mut self, tone: &acr::Tone) -> Result<(), String> {
        let (lut, past) = self.light(tone)?;
        self.op(op::HUE_PRESERVING, &[lut, bits(past)]);
        Ok(())
    }
}

/// Where [`map_data`] puts each map: Dehaze's transmission, the Shadows and Highlights `a` and `b`.
fn map_offsets(context: &Context) -> Result<[u32; 3], String> {
    let t = context.haze.as_ref().map_or(0, |h| h.t.len());
    let (a, b) = context.tone.as_ref().map_or((0, 0), |m| (m.parts().0.len(), m.parts().1.len()));
    let at = |n: usize| u32::try_from(n).map_err(|_| "Camera Raw GPU maps too large".to_string());
    Ok([0, at(t)?, at(t.saturating_add(a))?]).and_then(|o| at(t.saturating_add(a).saturating_add(b)).map(|_| o))
}

/// The maps of `context` as [`GpuProgram::maps`] lays them out (for the GPU's buffer).
pub fn map_data(context: &Context) -> Vec<f32> {
    let mut out = Vec::new();
    if let Some(h) = &context.haze {
        out.extend_from_slice(&h.t);
    }
    if let Some(m) = &context.tone {
        let (a, b, _, _) = m.parts();
        out.extend_from_slice(a);
        out.extend_from_slice(b);
    }
    out
}

/// What [`program`] needs measured that `context` lacks, if anything.
fn missing(p: &CameraRaw, context: &Context) -> Option<&'static str> {
    if p.dehaze != 0.0 && context.haze.is_none() {
        return Some("Dehaze's haze");
    }
    if (p.highlights != 0.0 || p.shadows != 0.0) && context.tone.is_none() {
        return Some("the Shadows and Highlights map");
    }
    if p.contrast != 0.0 && context.pivot.is_none() {
        return Some("Contrast's pivot");
    }
    if (p.whites != 0.0 || p.blacks != 0.0) && context.amounts.is_none() {
        return Some("Whites and Blacks' amounts");
    }
    None
}

/// Whether a develop with `p` measures anything on the image (Dehaze, Shadows, Highlights,
/// Contrast, Whites, Blacks): without, [`program`] takes an empty [`Context`].
pub fn measures(p: &CameraRaw) -> bool {
    [p.dehaze, p.highlights, p.shadows, p.contrast, p.whites, p.blacks].iter().any(|v| *v != 0.0)
}

/// The develop of `p` on `w × h` straight RGBA pixels (display-encoded, as
/// [`develop_cached`](super::develop_cached) takes them) lying at `frame`, with the image-wide
/// measurements of `context` (a develop's [`Developed::context`](super::Developed), possibly of a
/// smaller copy of the image), as GPU passes. `distort`: Manual Distortion moves the pixels (a
/// whole image's develop); without, the input is taken as already moved (as
/// [`develop_region`](super::develop_region) takes it). `Err` when `context` lacks a measurement
/// the settings need, or the planes would not fit 32-bit offsets.
pub fn program(p: &CameraRaw, w: usize, h: usize, frame: Frame, context: &std::sync::Arc<Context>, float: bool, distort: bool) -> Result<GpuProgram, String> {
    let mut prog = GpuProgram {
        width: u32::try_from(w).map_err(|_| "image too wide")?,
        height: u32::try_from(h).map_err(|_| "image too tall")?,
        frame_x0: i32::try_from(frame.x0).map_err(|_| "frame out of range")?,
        frame_y0: i32::try_from(frame.y0).map_err(|_| "frame out of range")?,
        full_w: u32::try_from(frame.full_w).map_err(|_| "frame too wide")?,
        full_h: u32::try_from(frame.full_h).map_err(|_| "frame too tall")?,
        ..Default::default()
    };
    if w == 0 || h == 0 || p.is_identity() {
        return Ok(prog);
    }
    if let Some(what) = missing(p, context) {
        return Err(format!("Camera Raw on the GPU needs {what} measured first"));
    }
    let [t_off, ma_off, mb_off] = map_offsets(context)?;
    if p.dehaze != 0.0 || p.highlights != 0.0 || p.shadows != 0.0 {
        prog.maps = Some(context.clone());
    }
    let enc = Encoding::of(p.encoding_gamma);
    let mut b = Builder { ops: Vec::new(), table: Vec::new(), passes: Vec::new(), planes: Planes::default(), open: None, w, h, shared: Vec::new() };
    // The header: the encoding's curves and sRGB's, the ProPhoto matrices.
    let curves = enc.curves();
    let (dec, en) = curves.tables();
    let (dec, en) = (b.table(dec)?, b.table(en)?);
    let srgb = Encoding(0.0).curves();
    let (sdec, sen) = srgb.tables();
    let (sdec, sen) = (b.table(sdec)?, b.table(sen)?);
    let space = acr::Space::of(enc);
    b.ops.extend([bits(enc.0), dec, en, sdec, sen]);
    b.ops.extend(space.matrix_to_prophoto().iter().flatten().map(|v| bits(*v)));
    b.ops.extend(space.matrix_from_prophoto().iter().flatten().map(|v| bits(*v)));
    b.ops.extend(acr::PROPHOTO_Y.iter().map(|v| bits(*v)));
    debug_assert_eq!(b.ops.len(), HEADER);

    // 0. Optics.
    if !p.optics_neutral() {
        let (cx, cy, diag) = frame.centre();
        if distort && p.lens_distortion != 0.0 {
            b.flush();
            b.passes.push(GpuPass::Distort { k: p.lens_distortion.clamp(-100.0, 100.0) / 100.0 * 0.25, cx, cy, diag });
        }
        let fringes = super::fringes(p);
        if !fringes.is_empty() {
            let ps = p.pixel_scale.clamp(0.01, 1.0);
            let l = b.full()?;
            b.op(op::LUMA, &[l.off]);
            let grad = b.gradient(l)?;
            b.give(l);
            let edge = b.gauss(grad, (2.0 * ps).max(0.5))?;
            b.give(grad);
            let mut args = vec![fringes.len() as u32];
            for (amount, lo, hi) in &fringes {
                args.extend([bits(*amount), bits(*lo), bits(*hi)]);
            }
            args.push(edge.off);
            b.op(op::DEFRINGE, &args);
            b.flush();
            b.give(edge);
        }
        if p.lens_vignette_amount != 0.0 {
            let amount = p.lens_vignette_amount.clamp(-100.0, 100.0) / 100.0;
            let start = 0.8 * (1.0 - p.lens_vignette_midpoint.clamp(0.0, 100.0) / 100.0);
            b.op(op::LENS_VIGNETTE, &[bits(amount), bits(start), bits(cx), bits(cy), bits(diag)]);
        }
    }
    // 0–1. Calibration, white balance, Exposure.
    if !p.calibration_neutral() {
        let primaries = [(p.red_hue, p.red_saturation), (p.green_hue, p.green_saturation), (p.blue_hue, p.blue_saturation)];
        let matrix = acr::calibration(primaries, enc);
        let tint = p.shadow_tint.clamp(-100.0, 100.0) / 100.0 * 0.12;
        let mut args = vec![u32::from(matrix.is_some())];
        args.extend(matrix.unwrap_or_default().iter().flatten().map(|v| bits(*v)));
        args.push(bits(tint));
        b.op(op::CALIBRATE, &args);
    }
    if p.temperature != 0.0 || p.tint != 0.0 {
        let g = acr::white_balance_gains_prophoto(p.temperature, p.tint);
        b.op(op::WHITE_BALANCE, &g.map(bits));
    }
    if p.exposure != 0.0 {
        b.hue_preserving(&acr::exposure(p.exposure))?;
    }
    // 2. Dehaze, from the measured haze.
    if p.dehaze != 0.0 {
        let haze = context.haze.as_ref().ok_or("no haze")?;
        let (fx, fy) = (haze.per_w / frame.full_w as f32, haze.per_h / frame.full_h as f32);
        let mut args: Vec<u32> = haze.air.map(bits).to_vec();
        args.extend([t_off, haze.tw as u32, haze.th as u32, bits(fx), bits(fy), bits(p.dehaze / 100.0)]);
        b.op(op::DEHAZE, &args);
    }
    // 3. Shadows and Highlights, from the measured map.
    if p.highlights != 0.0 || p.shadows != 0.0 {
        let map = context.tone.as_ref().ok_or("no map")?;
        let (_, _, mw, mh) = map.parts();
        let (fx, fy) = (mw as f32 / frame.full_w as f32, mh as f32 / frame.full_h as f32);
        b.op(op::TONE_MAP, &[ma_off, mb_off, mw as u32, mh as u32, bits(fx), bits(fy)]);
    }
    // 3, continued: Texture and Clarity, Contrast, Whites and Blacks.
    if p.texture != 0.0 || p.clarity != 0.0 {
        let ps = p.pixel_scale.clamp(0.01, 1.0);
        let long = frame.full_w.max(frame.full_h) as f32;
        let (l, lc) = (b.full()?, b.full()?);
        b.op(op::LUMA_PLANES, &[l.off, lc.off]);
        let clar = if p.clarity != 0.0 { Some(b.guided_fast(lc, ((long * 0.02) as usize).max(3), 0.005)?) } else { None };
        let tex = if p.texture != 0.0 { Some(b.gauss(lc, (long * 0.002).max(1.5 * ps))?) } else { None };
        b.op(op::TEXTURE_CLARITY, &[l.off, lc.off, clar.map_or(NONE, |c| c.off), tex.map_or(NONE, |t| t.off), bits(p.clarity), bits(p.texture)]);
        b.flush();
        for plane in [Some(l), Some(lc), clar, tex].into_iter().flatten() {
            b.give(plane);
        }
    }
    if p.contrast != 0.0 {
        let pivot = context.pivot.ok_or("no pivot")?;
        b.hue_preserving(&acr::contrast(p.contrast, pivot))?;
    }
    if p.whites != 0.0 || p.blacks != 0.0 {
        let (xw, xb) = context.amounts.ok_or("no amounts")?;
        if let Some(tone) = whites_blacks::tone(xw, xb) {
            b.hue_preserving(&tone)?;
        }
    }
    // 4. Tone curve.
    if !p.curves_neutral() {
        let refine = p.curve_refine_saturation.clamp(0.0, 100.0) / 100.0;
        if refine < 1.0 {
            b.op(op::SAVE, &[]);
        }
        let (grey, channels) = super::curve_tones(p);
        if let Some(t) = grey {
            b.hue_preserving(&t)?;
        }
        if channels.iter().any(Option::is_some) {
            let mut args = Vec::with_capacity(6);
            for t in &channels {
                let (lut, past) = match t {
                    Some(t) => b.light(t)?,
                    None => (NONE, 1.0),
                };
                args.extend([lut, bits(past)]);
            }
            b.op(op::PER_CHANNEL, &args);
        }
        if refine < 1.0 {
            b.op(op::REFINE, &[bits(refine)]);
        }
    }
    // 5. Vibrance and Saturation.
    if p.vibrance != 0.0 || p.saturation != 0.0 {
        let v = if p.vibrance.is_finite() { p.vibrance.clamp(-100.0, 100.0) / 100.0 } else { 0.0 };
        let s = if p.saturation.is_finite() { p.saturation.clamp(-100.0, 100.0) / 100.0 } else { 0.0 };
        let [k, t, hue, levels] = acr::vibrance_tables().map(|t| b.shared(t));
        b.op(op::VIBRANCE_SATURATION, &[bits(v), bits(s), k?, t?, hue?, levels?]);
    }
    // 6. The B&W mix, or the Color Mixer.
    if p.monochrome {
        b.op(op::GRAY_MIX, &p.gray_mix.map(bits));
    } else if !p.hsl_neutral() {
        let fin = |v: f32| if v.is_finite() { v.clamp(-100.0, 100.0) / 100.0 } else { 0.0 };
        let mut args: Vec<u32> = p.hsl_hue.iter().chain(&p.hsl_sat).chain(&p.hsl_lum).map(|v| bits(fin(*v))).collect();
        for t in acr::hsl_tables() {
            args.push(b.shared(t)?);
        }
        b.op(op::HSL, &args);
    }
    // 7. Color Grading.
    if !p.grade_neutral() {
        let (tone, colours) = acr::grade_parts(p);
        let (lut, past) = match &tone {
            Some(t) => b.light(t)?,
            None => (NONE, 1.0),
        };
        let mut args = vec![lut, bits(past), colours.len() as u32];
        for c in &colours {
            let mask = b.table(&c.mask)?;
            args.extend([bits(c.strength), bits(c.dir[0]), bits(c.dir[1]), bits(c.dir[2]), mask, bits(c.add), u32::from(c.keep)]);
        }
        b.op(op::GRADE, &args);
    }
    // 8. Detail, on Y and chroma.
    let ps = p.pixel_scale.clamp(0.01, 1.0);
    if p.noise_luminance > 0.0 || p.noise_color > 0.0 || p.sharpen_amount > 0.0 {
        let (y, cb, cr) = (b.full()?, b.full()?, b.full()?);
        b.op(op::DETAIL_SPLIT, &[y.off, cb.off, cr.off]);
        if p.noise_luminance > 0.0 {
            let n = p.noise_luminance / 100.0;
            let r = ((1.0 + 3.0 * n) * ps).round().max(1.0) as usize;
            let smooth = b.guided(y, r, (n * 0.06).powi(2) + 1e-6)?;
            let keep = p.noise_luminance_detail / 100.0 * 0.5;
            let contrast = p.noise_luminance_contrast.clamp(0.0, 100.0) / 100.0;
            let wide = if contrast > 0.0 { Some(b.gauss(smooth, 2.0 * r as f32)?) } else { None };
            b.op(op::NOISE_LUMINANCE, &[y.off, smooth.off, wide.map_or(NONE, |p| p.off), bits(keep), bits(contrast)]);
            b.flush();
            b.give(smooth);
            if let Some(wide) = wide {
                b.give(wide);
            }
        }
        if p.noise_color > 0.0 {
            let smooth = (p.noise_color_smoothness.clamp(0.0, 100.0) - 50.0) / 50.0;
            let sigma = (p.noise_color / 100.0 * 6.0 * ps * (1.0 + 0.3 * smooth.min(0.0))).max(0.3);
            let keep = p.noise_color_detail / 100.0 * 0.3;
            for c in [cb, cr] {
                let blurred = b.gauss(c, sigma)?;
                let wide = if smooth > 0.0 { Some(b.gauss(blurred, sigma * 3.0)?) } else { None };
                b.op(op::NOISE_COLOR, &[c.off, blurred.off, wide.map_or(NONE, |p| p.off), bits(smooth), bits(keep)]);
                b.flush();
                b.give(blurred);
                if let Some(wide) = wide {
                    b.give(wide);
                }
            }
        }
        if p.sharpen_amount > 0.0 {
            let sigma = (p.sharpen_radius.clamp(0.5, 3.0) * ps).max(0.3);
            let blur = b.gauss(y, sigma)?;
            let grad = if p.sharpen_masking > 0.0 {
                let s = b.gauss(y, ps.max(0.5))?;
                let g = b.gradient(s)?;
                b.give(s);
                Some(g)
            } else {
                None
            };
            let (amount, detail, t) = (p.sharpen_amount / 100.0 * 1.2, p.sharpen_detail / 100.0, p.sharpen_masking / 100.0 * 0.08);
            b.op(op::SHARPEN, &[y.off, blur.off, grad.map_or(NONE, |g| g.off), bits(amount), bits(detail), bits(t)]);
            b.flush();
            b.give(blur);
            if let Some(g) = grad {
                b.give(g);
            }
        }
        b.op(op::DETAIL_JOIN, &[y.off, cb.off, cr.off]);
        b.flush();
        for plane in [y, cb, cr] {
            b.give(plane);
        }
    }
    // 9. Effects.
    if p.vignette_amount != 0.0 {
        let style = match p.vignette_style.as_str() {
            "colorPriority" => 1u32,
            "paintOverlay" => 2,
            _ => 0,
        };
        let mask = acr::VignetteMask::new(p, frame.full_w, frame.full_h).parts();
        let (cx, cy) = (frame.full_w as f32 / 2.0, frame.full_h as f32 / 2.0);
        let amount = p.vignette_amount.clamp(-100.0, 100.0);
        let hl = p.vignette_highlights.clamp(0.0, 100.0) / 100.0;
        let mut args = vec![style, bits(amount), bits(hl)];
        args.extend(mask.map(bits));
        args.extend([bits(cx), bits(cy)]);
        // The rows never change: their tables are made once.
        static ROWS: std::sync::OnceLock<Vec<(f32, Vec<f32>, f32)>> = std::sync::OnceLock::new();
        let rows: &[(f32, Vec<f32>, f32)] = if style == 0 {
            ROWS.get_or_init(|| {
                acr::vignette_tones()
                    .iter()
                    .map(|(a, tone)| {
                        let (lut, past) = acr::light_lut(tone);
                        (*a, lut, past)
                    })
                    .collect()
            })
        } else {
            &[]
        };
        args.push(rows.len() as u32);
        for (a, lut, past) in rows {
            let lut = b.table(lut)?;
            args.extend([bits(*a), lut, bits(*past)]);
        }
        b.op(op::VIGNETTE, &args);
    }
    if p.grain_amount > 0.0 {
        let cell = 1.0 + p.grain_size / 25.0;
        b.op(op::GRAIN, &[bits(cell), bits(p.grain_roughness / 100.0), bits(p.grain_amount / 100.0 * 0.12), p.seed, bits(1.0 / ps)]);
    }
    if !float {
        b.op(op::CLAMP, &[]);
    }
    b.flush();
    prog.passes = b.passes;
    prog.ops = b.ops;
    prog.table = b.table;
    prog.plane_floats = b.planes.end;
    Ok(prog)
}

/// `p` without the stages after Whites and Blacks (the tone curve, colour, detail, effects):
/// what a develop must run to take [`program`]'s measurements.
fn measured_part(p: &CameraRaw) -> CameraRaw {
    let d = CameraRaw::default();
    CameraRaw {
        curve_highlights: 0.0,
        curve_lights: 0.0,
        curve_darks: 0.0,
        curve_shadows: 0.0,
        point_curve: Vec::new(),
        point_curve_red: Vec::new(),
        point_curve_green: Vec::new(),
        point_curve_blue: Vec::new(),
        vibrance: 0.0,
        saturation: 0.0,
        hsl_hue: d.hsl_hue,
        hsl_sat: d.hsl_sat,
        hsl_lum: d.hsl_lum,
        monochrome: false,
        grade_shadows: d.grade_shadows,
        grade_midtones: d.grade_midtones,
        grade_highlights: d.grade_highlights,
        grade_global: d.grade_global,
        sharpen_amount: 0.0,
        noise_luminance: 0.0,
        noise_color: 0.0,
        grain_amount: 0.0,
        vignette_amount: 0.0,
        ..p.clone()
    }
}

/// The image-wide measurements [`program`] needs for `p`, taken on `w × h` pixels (the image or
/// a smaller copy of it: the measurements are relative to the image, so a copy's serve it, as a
/// proxy's serve the full image; `p.pixel_scale` should be the copy's). `cache` and `source` work
/// as for [`develop_cached`](super::develop_cached), whose stages up to Whites and Blacks this
/// runs; `draft` lets Shadows and Highlights reuse maps made for nearby settings. Nothing is run
/// when `p` measures nothing ([`measures`]).
pub fn measure(src: &[[f32; 4]], w: usize, h: usize, p: &CameraRaw, source: u64, cache: &mut super::DevelopCache, draft: bool) -> std::sync::Arc<Context> {
    if !measures(p) || src.len() != w.saturating_mul(h) || src.is_empty() {
        return Default::default();
    }
    let developed = super::develop_cached(src, w, h, &measured_part(p), true, source, cache, draft);
    // The develop it ran is no longer needed: its buffer serves the next one.
    let context = developed.context.clone();
    cache.recycle(developed.px);
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(w: usize, h: usize) -> Vec<[f32; 4]> {
        (0..w * h).map(|i| [(i % w) as f32 / w as f32, 0.5, (i / w) as f32 / h as f32, 1.0]).collect()
    }

    #[test]
    fn an_identity_is_an_empty_program() {
        let prog = program(&CameraRaw::default(), 8, 8, Frame::whole(8, 8), &Default::default(), false, true).unwrap();
        assert!(prog.is_empty());
        let prog = program(&CameraRaw { exposure: 1.0, ..Default::default() }, 0, 0, Frame::whole(0, 0), &Default::default(), false, true).unwrap();
        assert!(prog.is_empty(), "nothing to develop in an empty image");
    }

    #[test]
    fn a_missing_measurement_is_refused() {
        for p in [
            CameraRaw { contrast: 20.0, ..Default::default() },
            CameraRaw { shadows: 20.0, ..Default::default() },
            CameraRaw { dehaze: 20.0, ..Default::default() },
            CameraRaw { blacks: -20.0, ..Default::default() },
        ] {
            assert!(program(&p, 8, 8, Frame::whole(8, 8), &Default::default(), false, true).is_err(), "{p:?}");
            // With the measurements a develop takes, it builds.
            let src = grey(16, 12);
            let context = measure(&src, 16, 12, &p, 1, &mut Default::default(), false);
            let prog = program(&p, 16, 12, Frame::whole(16, 12), &context, false, true).unwrap();
            assert!(!prog.is_empty());
        }
    }

    #[test]
    fn nothing_is_measured_for_settings_that_need_nothing() {
        let p = CameraRaw { exposure: 1.0, vibrance: 30.0, clarity: 40.0, ..Default::default() };
        assert!(!measures(&p));
        let context = measure(&grey(16, 12), 16, 12, &p, 1, &mut Default::default(), false);
        assert!(context.haze.is_none() && context.tone.is_none() && context.pivot.is_none() && context.amounts.is_none());
    }

    /// Hostile settings and sizes build a program or an error, never a panic.
    #[test]
    fn hostile_settings_never_panic() {
        let nan = f32::NAN;
        let p = CameraRaw {
            exposure: nan,
            temperature: f32::INFINITY,
            texture: nan,
            clarity: -f32::INFINITY,
            vibrance: nan,
            hsl_lum: [nan; 8],
            grade_shadows: crate::camera_raw::Wheel { hue: nan, sat: nan, lum: nan },
            sharpen_amount: 1e30,
            sharpen_radius: nan,
            noise_luminance: 1e9,
            noise_color: 1e9,
            vignette_amount: nan,
            grain_amount: 1e9,
            grain_size: nan,
            lens_distortion: 1e9,
            defringe_purple_amount: nan,
            pixel_scale: nan,
            point_curve: vec![[nan, 3.0], [1e9, -5.0]],
            ..Default::default()
        };
        let huge = CameraRaw { noise_luminance: 1e9, ..Default::default() };
        assert!(program(&huge, 40, 30, Frame::whole(40, 30), &Default::default(), false, true).is_err(), "a huge blur radius is left to the CPU");
        for (w, h) in [(1, 1), (3, 1), (1, 7), (40, 30)] {
            let _ = program(&p, w, h, Frame::whole(w, h), &Default::default(), true, true);
            let _ = program(&p, w, h, Frame { x0: -5, y0: 1 << 40, full_w: 1, full_h: 0 }, &Default::default(), false, false);
        }
        let _ = measure(&grey(4, 4), 5, 4, &CameraRaw { contrast: nan, ..Default::default() }, 1, &mut Default::default(), true);
    }
}
