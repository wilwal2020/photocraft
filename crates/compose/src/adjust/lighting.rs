//! Lighting, PhotoCraft's own adjustment (see [`photocraft_doc::adjust::Lighting`]): the mirror
//! of [`super::shading`]. The mask is the light amount, and each colour picks its own lit colour
//! in OKLab.
//!
//! Per pixel, at light amount `a` (the layer's opacity × mask, 1 without a mask):
//! - lightness rises toward white by `lift · a` (`L + (1 − L) · lift · a`, so whites can't clip);
//! - hue turns toward `light_hue` the shorter way round, by up to `hue_shift · a` degrees, easing
//!   off near the light hue (already there) and near its opposite (no tear between neighbours
//!   turning opposite ways);
//! - chroma keeps half its place against the gamut edge as the colour gets lighter (above the
//!   cusp the edge comes in, so lit colours go paler; lit yellows, below their cusp, get richer),
//!   then moves toward the edge by `saturation · a`;
//! - Highlight Fade: over the top 30% of the light amount, chroma gives way to half the light's
//!   own colour, so highlights read as light, not neon local colour;
//! - Gloss: a narrow near-white shine over the top of the light amount, in the light's colour
//!   (or, with Metallic, the surface's own: metal tints its reflection, and Highlight Fade then
//!   keeps the colour);
//! - Saturation Curve swings the picker saturation out and back on the way (as in Shading);
//! - greys get neither hue nor chroma changes but take on the light's colour (every colour leans
//!   toward it by the same small OKLab offset); near-black colours
//!   (linework) are left mostly alone (`protect_darks`);
//! - an optional Screen-style glow goes over the top.
//!
//! Tuned on a swatch sheet and the user's colouring (2026-11-09): skin lights to a warm cream,
//! cyan toward green, purple toward pink, blue (opposite the light) just lighter, brown to ochre; black lines stay black.

use photocraft_doc::adjust::Lighting;

use super::Transfer;
use super::shading::{bend, hue_at, hue_table, lin_to_oklab, max_chroma, oklab_to_lin, path_at, path_table, smoothstep};
use crate::Buffer;

/// How much of its place against the gamut edge a colour keeps as it gets lighter (0: keeps its
/// chroma, 1: keeps its saturation relative to the edge).
const KEEP: f32 = 0.5;

/// Lighting settings checked once per render, with the light colour's direction in OKLab.
pub struct Prepared {
    s: Lighting,
    light_dir: [f32; 2],
    light_chroma: f32,
    path: Option<Vec<[f32; 2]>>,
    hue: Option<Vec<f32>>,
}

impl Prepared {
    pub fn new(s: &Lighting) -> Self {
        let s = s.sanitized();
        let lin = s.light.map(photocraft_color::convert::srgb_to_linear);
        let [_, a, b] = lin_to_oklab(lin);
        let n = a.hypot(b);
        let light_dir = if n > 1e-5 { [a / n, b / n] } else { [0.0, 0.0] };
        let path = path_table(&s.saturation_points, s.saturation_curve);
        let hue = hue_table(&s.hue_points);
        Self { s, light_dir, light_chroma: if n > 1e-5 { n } else { 0.0 }, path, hue }
    }

    /// The settings, sanitized.
    pub fn settings(&self) -> &Lighting {
        &self.s
    }

    /// The Advanced Curve's table (`shading::path_table`), None when straight.
    pub fn path(&self) -> Option<&[[f32; 2]]> {
        self.path.as_deref()
    }

    /// The Advanced Curve's point at amount `a` (`shading::path_at`), None when straight.
    pub fn bent(&self, a: f32) -> Option<[f32; 2]> {
        Some(path_at(self.path.as_deref()?, a))
    }

    /// The Hue Curve's table (`shading::hue_table`), None when straight.
    pub fn hue_curve(&self) -> Option<&[f32]> {
        self.hue.as_deref()
    }

    /// The Hue Curve's turn at amount `a` (`shading::hue_at`), None when straight.
    pub fn hue_bent(&self, a: f32) -> Option<f32> {
        Some(hue_at(self.hue.as_deref()?, a))
    }

    /// The light colour's unit direction in OKLab's a/b plane.
    pub fn light_direction(&self) -> [f32; 2] {
        self.light_dir
    }

    /// The light colour's OKLab chroma.
    pub fn light_chroma(&self) -> f32 {
        self.light_chroma
    }

    /// Whether this lighting changes nothing at any amount.
    pub fn is_identity(&self) -> bool {
        let s = &self.s;
        s.lift == 0.0
            && s.saturation == 0.0
            && self.path.is_none()
            && self.hue.is_none()
            && s.highlight_fade == 0.0
            && s.gloss == 0.0
            && s.hue_shift == 0.0
            && s.light_amount == 0.0
            && s.glow_amount == 0.0
    }

    /// The amount after Steps: the mask snapped to `steps` tones (none below half a step).
    pub fn stepped(&self, a: f32) -> f32 {
        let a = if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 };
        match self.s.steps {
            n @ 2.. => (a * n as f32 + 0.5).floor() / n as f32,
            _ => a,
        }
    }

    /// Light linear-light `c` at amount `a` (already stepped).
    pub fn light_lin(&self, c: [f32; 3], a: f32) -> [f32; 3] {
        self.light_lin_weighted(c, a).0
    }

    /// [`Self::light_lin`], with how much of Saturation Curve's [`swing`] the colour takes.
    pub fn light_lin_weighted(&self, c: [f32; 3], a: f32) -> ([f32; 3], f32) {
        let s = &self.s;
        if a <= 0.0 || c.iter().any(|v| !v.is_finite()) {
            return (c, 0.0);
        }
        let c = c.map(|v| v.clamp(0.0, 1.0));
        let [l, la, lb] = lin_to_oklab(c);
        let chroma = la.hypot(lb);
        let hue = lb.atan2(la).to_degrees().rem_euclid(360.0);
        // Linework and dark accents barely move; greys get no hue or chroma change.
        let keep = self.dark_keep(l);
        let ae = a * keep;
        let c0 = 0.05 * s.protect_neutrals;
        let w = if c0 > 0.0 { smoothstep(0.3 * c0, 1.3 * c0, chroma) } else { 1.0 };
        let away = (s.light_hue + 180.0).rem_euclid(360.0);
        let d = (hue - away + 180.0).rem_euclid(360.0) - 180.0;
        let to_light = 180.0 - d.abs();
        let taper = smoothstep(0.0, 30.0, d.abs()) * smoothstep(0.0, 70.0, to_light);
        let cm0 = max_chroma(l, hue).max(1e-4);
        let rel = (chroma / cm0).min(1.0);
        let l2 = l + (1.0 - l).max(0.0) * s.lift * ae;
        // Hue: toward the light hue, the shorter way round (away from its opposite).
        let turned = (s.hue_shift * ae * w * taper).min(to_light);
        let h2 = hue + d.signum() * turned;
        // Chroma: keep part of its place against the gamut edge, then toward the edge.
        let cm1 = max_chroma(l2, h2).max(0.0);
        let kept = (chroma + (rel * cm1 - chroma) * KEEP).min(cm1);
        let c2 = kept + (cm1 - kept).max(0.0) * (s.saturation * ae * w).clamp(0.0, 1.0);
        let (sin, cos) = h2.to_radians().sin_cos();
        // One OKLab offset for every colour (greys and whites turn the most).
        let tint = 0.06 * s.light_amount * ae;
        // Highlight Fade: the most lit tones give up chroma for half the light's own colour (a lit
        // surface's top is the light's colour, not neon local colour).
        // (Metal keeps its colour there.)
        let fade = if s.metallic { 0.0 } else { s.highlight_fade * smoothstep(0.7, 1.0, a) * keep };
        let (fa, fb) = (self.light_dir[0] * self.light_chroma * 0.5 * fade, self.light_dir[1] * self.light_chroma * 0.5 * fade);
        let c2 = c2 * (1.0 - fade);
        let mut out = [l2, c2 * cos + self.light_dir[0] * tint + fa, c2 * sin + self.light_dir[1] * tint + fb];
        // Gloss: over the top `gloss_size` of the light amount, lightness toward white and colour
        // toward a pale light colour (a dielectric's shine is the light's) or, for metal, toward
        // the surface's own rich colour at that lightness.
        let g = s.gloss * smoothstep(1.0 - s.gloss_size, 1.0, a) * keep;
        if g > 0.0 {
            // (Metal stops short of white, where there is room for its colour.)
            let lg = out[0] + (1.0 - out[0]).max(0.0) * g * if s.metallic { 0.5 } else { 0.85 };
            let target = if s.metallic {
                // (Greys have no hue of their own: silver shines neutral.)
                let cm = max_chroma(lg, h2).max(0.0) * 0.8 * w;
                [cm * cos, cm * sin]
            } else {
                [self.light_dir[0] * self.light_chroma * 0.35, self.light_dir[1] * self.light_chroma * 0.35]
            };
            out = [lg, out[1] + (target[0] - out[1]) * g, out[2] + (target[1] - out[2]) * g];
        }
        (oklab_to_lin(out).map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }), keep * w)
    }

    /// How much of the light a colour of OKLab lightness `l` takes: near-black colours
    /// (linework) keep themselves by `protect_darks`.
    fn dark_keep(&self, l: f32) -> f32 {
        if !l.is_finite() {
            return 0.0;
        }
        1.0 - self.s.protect_darks * (1.0 - smoothstep(0.15, 0.35, l))
    }

    /// The Screen-style glow at amount `a`, on document-encoded `c`, for a colour that takes
    /// `keep` of the light (glow would otherwise lift black lines to grey).
    fn glow(&self, c: [f32; 3], a: f32, keep: f32) -> [f32; 3] {
        let k = self.s.glow_amount * a * keep;
        if k <= 0.0 {
            return c;
        }
        std::array::from_fn(|i| 1.0 - (1.0 - c[i]) * (1.0 - k * self.s.glow[i]))
    }
}

/// Apply Lighting to `buf` (document-encoded through `transfer`). `amounts` holds each pixel's
/// light amount (the layer's opacity × mask), or None for full light everywhere.
pub fn apply_lighting(s: &Lighting, buf: &mut Buffer, amounts: Option<&[f32]>, transfer: Transfer) {
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
        let lin = [px[0], px[1], px[2]].map(|v| transfer.decode(v));
        let (out, k) = p.light_lin_weighted(lin, a);
        let mut lit = out.map(|v| transfer.encode(v));
        let dark_keep = || p.dark_keep(lin_to_oklab(lin.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }))[0]);
        let (at, turn) = (p.bent(a), p.hue_bent(a));
        if at.is_some() || turn.is_some() {
            let end = p.light_lin_weighted(lin, 1.0).0.map(|v| transfer.encode(v));
            lit = bend(lit, [px[0], px[1], px[2]], end, at.unwrap_or([a, a]), turn.unwrap_or(a), a, k, dark_keep(), true, p.s.literal_curve);
        }
        let keep = if p.s.glow_amount > 0.0 { dark_keep() } else { 0.0 };
        let out = p.glow(lit, a, keep);
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

    fn light(srgb: [f32; 3], a: f32, s: &Lighting) -> [f32; 3] {
        let mut b = Buffer::filled(Rect::new(0, 0, 1, 1), [srgb[0], srgb[1], srgb[2], 1.0]);
        apply_lighting(s, &mut b, Some(&[a]), Transfer::Srgb);
        [b.px[0][0], b.px[0][1], b.px[0][2]]
    }

    fn hue_turn(from: f32, to: f32) -> f32 {
        (to - from + 180.0).rem_euclid(360.0) - 180.0
    }

    /// Skin warms toward yellow, cyan turns toward green, purple toward pink, each lighter; greys
    /// stay grey but warm a little; black lines stay black (2026-11-09).
    #[test]
    fn each_colour_picks_its_own_light() {
        let s = Lighting::default();
        let skin = [0.80, 0.55, 0.38];
        let (before, after) = (lch(skin), lch(light(skin, 1.0, &s)));
        assert!(after[0] > before[0] + 0.05, "skin lightens: {before:?} → {after:?}");
        assert!(hue_turn(before[2], after[2]) > 5.0, "skin turns toward yellow: {before:?} → {after:?}");
        // Blue sits short of the light's opposite (288°), so it lightens and turns a little toward
        // cyan, as palette blue ramps do (2026-11-09 research); cyan turns toward green.
        let blue = [0.25, 0.40, 0.85];
        let (before, after) = (lch(blue), lch(light(blue, 1.0, &s)));
        let turn = hue_turn(before[2], after[2]);
        assert!(after[0] > before[0] + 0.1 && turn <= 0.0 && turn > -25.0, "blue lightens: {before:?} → {after:?}");
        let cyan = [0.45, 0.80, 0.85];
        let (before, after) = (lch(cyan), lch(light(cyan, 1.0, &s)));
        assert!(hue_turn(before[2], after[2]) < -5.0, "cyan turns toward green: {before:?} → {after:?}");
        let purple = [0.55, 0.35, 0.75];
        let (before, after) = (lch(purple), lch(light(purple, 1.0, &s)));
        assert!(hue_turn(before[2], after[2]) > 5.0, "purple turns toward pink: {before:?} → {after:?}");
        let grey = [0.5, 0.5, 0.5];
        // (Highlight Fade off too: it gives the top of the light the light's colour.)
        let plain = lch(light(grey, 1.0, &Lighting { light_amount: 0.0, highlight_fade: 0.0, ..s.clone() }));
        assert!(plain[0] > lch(grey)[0] + 0.05 && plain[1] < 0.005, "grey lightens and stays grey: {plain:?}");
        let warm = lch(light(grey, 1.0, &Lighting { light_amount: 1.0, ..s.clone() }));
        assert!(warm[1] > 0.02 && hue_turn(lch(s.light)[2], warm[2]).abs() < 15.0, "grey takes on the light: {warm:?}");
        let black = [0.04, 0.04, 0.04];
        let o = light(black, 1.0, &s);
        assert!((0..3).all(|k| (o[k] - black[k]).abs() < 0.03), "linework stays: {o:?}");
        // White can't get any lighter and doesn't blow out; at the top of the light it takes on a
        // little of the light's warmth (Highlight Fade).
        let w = light([1.0; 3], 1.0, &s);
        assert!(w.iter().all(|v| (0.88..=1.0).contains(v)) && w[0] >= w[1] && w[1] >= w[2], "{w:?}");
        let w = light([1.0; 3], 1.0, &Lighting { highlight_fade: 0.0, ..s.clone() });
        assert!(w.iter().all(|v| (0.95..=1.0).contains(v)), "{w:?}");
    }

    #[test]
    fn vivid_colours_stay_in_gamut() {
        let s = Lighting { saturation: 1.0, hue_shift: 90.0, lift: 1.0, light_amount: 1.0, glow_amount: 1.0, ..Lighting::default() };
        for c in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0, 0.0], [1.0, 1.0, 1.0], [0.12, 0.16, 0.38]] {
            for a in [0.1, 0.5, 1.0] {
                let o = light(c, a, &s);
                assert!(o.iter().all(|v| (0.0..=1.0).contains(v)), "{c:?} at {a}: {o:?}");
            }
        }
    }

    #[test]
    fn amount_steps_and_glow() {
        let s = Lighting::default();
        let skin = [0.80, 0.55, 0.38];
        assert_eq!(light(skin, 0.0, &s), skin);
        let ls: Vec<f32> = [0.2, 0.4, 0.6, 0.8, 1.0].iter().map(|&a| lch(light(skin, a, &s))[0]).collect();
        assert!(ls.windows(2).all(|w| w[1] > w[0]), "lighter with more light: {ls:?}");
        let cel = Lighting { steps: 2, ..s.clone() };
        assert_eq!(light(skin, 0.2, &cel), skin);
        assert_eq!(light(skin, 0.4, &cel), light(skin, 0.6, &cel));
        assert_eq!(light(skin, 0.8, &cel), light(skin, 1.0, &s));
        // Glow: Screen toward its colour, except on linework while Protect Darks is up.
        let glow = Lighting { lift: 0.0, light_amount: 0.0, glow: [0.5, 1.0, 0.0], glow_amount: 1.0, ..s.clone() };
        let g = light([0.0; 3], 1.0, &Lighting { protect_darks: 0.0, ..glow.clone() });
        assert!((g[0] - 0.5).abs() < 1e-3 && (g[1] - 1.0).abs() < 1e-3 && g[2].abs() < 1e-3, "{g:?}");
        let line = light([0.03; 3], 1.0, &Lighting { protect_darks: 1.0, ..glow.clone() });
        assert!(line.iter().all(|v| (v - 0.03).abs() < 1e-3), "black lines don't glow: {line:?}");
        let skin_glow = light(skin, 1.0, &glow);
        assert!(skin_glow[1] > skin[1] + 0.1, "colours do: {skin_glow:?}");
    }

    /// Saturation Curve swings the picker saturation out and back on the way to full light, as in
    /// Shading: brightness and hue stay on the path, 0 is the path itself, ±100 reach the vivid
    /// edge or grey halfway, full light is unchanged. Black linework stays put.
    #[test]
    fn saturation_curve_swings_the_saturation() {
        use crate::adjust::shading::rgb_to_hsb;
        let s = Lighting::default();
        let curved = |k: f32| Lighting { saturation_curve: k, ..s.clone() };
        for start in [[0.55, 0.38, 0.30], [0.62, 0.45, 0.22], [0.45, 0.30, 0.55]] {
            let end = rgb_to_hsb(light(start, 1.0, &s));
            for k in [-1.0, -0.5, 0.0, 0.5, 1.0] {
                assert_eq!(light(start, 1.0, &curved(k)), light(start, 1.0, &s), "{start:?} {k}: full light unchanged");
                for i in 1..20 {
                    let t = i as f32 / 20.0;
                    let (p0, p) = (rgb_to_hsb(light(start, t, &s)), rgb_to_hsb(light(start, t, &curved(k))));
                    // (Lit colours lose saturation, so inside the box "left" is back toward the
                    // base's richer saturation; only past the box does it head for grey.)
                    assert!((p[2] - p0[2]).abs() < 0.15 && (p[1] < 0.01 || hue_turn(p0[0], p[0]).abs() < 0.5), "{start:?} {k} at {t}: {p0:?} → {p:?}");
                }
            }
            let along = |k: f32| (1..40).map(|i| rgb_to_hsb(light(start, i as f32 / 40.0, &curved(k)))[1]).collect::<Vec<_>>();
            let (most, least) = (along(1.0).into_iter().fold(0.0f32, f32::max), along(-1.0).into_iter().fold(1.0f32, f32::min));
            assert!(most > end[1] + (1.0 - end[1]) * 0.5 - 0.03 && least < 0.08, "{start:?}: {most} {least}");
        }
        // Black linework stays put, whichever way it swings.
        let black = [0.03, 0.03, 0.04];
        let o = light(black, 0.5, &curved(1.0));
        assert!((0..3).all(|k| (o[k] - black[k]).abs() < 0.03), "{o:?}");
    }

    /// Highlight Fade: only the top 30% of the light amount gives up chroma, toward the light's
    /// colour; at 0 nothing changes.
    #[test]
    fn highlight_fade_turns_the_top_toward_the_light() {
        let s = Lighting { highlight_fade: 0.0, ..Lighting::default() };
        let faded = |f: f32| Lighting { highlight_fade: f, ..s.clone() };
        let red = [0.85, 0.25, 0.25];
        assert_eq!(light(red, 0.65, &faded(1.0)), light(red, 0.65, &s), "below the top: untouched");
        let (plain, full) = (lch(light(red, 1.0, &s)), lch(light(red, 1.0, &faded(1.0))));
        assert!(full[1] < plain[1] * 0.6 && (full[0] - plain[0]).abs() < 0.01, "{plain:?} → {full:?}");
        let half = lch(light(red, 1.0, &faded(0.5)))[1];
        assert!(full[1] < half && half < plain[1], "{} {half} {}", full[1], plain[1]);
        // A grey takes on the light's hue at the top.
        let grey = lch(light([0.5; 3], 1.0, &Lighting { light_amount: 0.0, ..faded(1.0) }));
        let lit = lch(s.light);
        assert!(grey[1] > 0.02 && hue_turn(lit[2], grey[2]).abs() < 10.0, "{grey:?} vs {lit:?}");
    }

    /// Gloss: only the top of the light, lighter and paler (the light's colour); Metallic keeps
    /// the surface's colour in the shine and under Highlight Fade.
    #[test]
    fn gloss_and_metallic() {
        let s = Lighting::default();
        let g = Lighting { gloss: 1.0, ..s.clone() };
        let m = Lighting { metallic: true, ..g.clone() };
        let red = [0.85, 0.25, 0.25];
        assert_eq!(light(red, 0.8, &g), light(red, 0.8, &s), "below the shine: untouched");
        let (plain, shine, metal) = (lch(light(red, 1.0, &s)), lch(light(red, 1.0, &g)), lch(light(red, 1.0, &m)));
        assert!(shine[0] > plain[0] + 0.05 && shine[1] < plain[1], "{plain:?} → {shine:?}");
        assert!(metal[1] > shine[1] + 0.02 && hue_turn(plain[2], metal[2]).abs() < 10.0, "{shine:?} vs {metal:?}");
        // Silver (grey metal) shines neutral, not in a stray hue.
        let silver = lch(light([0.6; 3], 1.0, &Lighting { light_amount: 0.0, ..m.clone() }));
        assert!(silver[1] < 0.01, "{silver:?}");
        // Metal: no Highlight Fade.
        let mf = lch(light(red, 1.0, &Lighting { metallic: true, ..s.clone() }));
        assert!(mf[1] > plain[1], "{plain:?} vs {mf:?}");
    }

    #[test]
    fn hostile_input_stays_finite() {
        let s = Lighting { lift: f32::NAN, saturation: f32::INFINITY, saturation_curve: f32::NEG_INFINITY, hue_shift: -5.0, light_hue: f32::NAN, ..Lighting::default() };
        for c in [[f32::NAN, 0.5, 0.5], [2.0, -1.0, 0.5], [0.5; 3]] {
            for a in [f32::NAN, -1.0, 0.5, 9.0] {
                let mut b = Buffer::filled(Rect::new(-3, -3, -2, -2), [c[0], c[1], c[2], 1.0]);
                apply_lighting(&s, &mut b, Some(&[a]), Transfer::Srgb);
                assert!(b.px[0].iter().all(|v| v.is_finite() || c.iter().any(|x| x.is_nan())), "{c:?} at {a}: {:?}", b.px[0]);
            }
        }
        let mut b = Buffer::filled(Rect::new(0, 0, 2, 1), [0.9, 0.7, 0.5, 1.0]);
        apply_lighting(&Lighting::default(), &mut b, Some(&[1.0]), Transfer::Srgb);
        assert_eq!(b.px[1], [0.9, 0.7, 0.5, 1.0]);
    }
}
