//! Adjustment parameters (data only). Evaluation lives in `photocraft-compose`.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub input: f32,
    pub output: f32,
}

/// Whether `points` is the straight 0→0, 1→1 line (or too short to be a curve).
pub fn is_identity_curve(points: &[CurvePoint]) -> bool {
    match points {
        [a, b] => a.input == 0.0 && a.output == 0.0 && b.input == 1.0 && b.output == 1.0,
        p => p.len() < 2,
    }
}

/// A 3D LUT file format (Photoshop's `LUTFormat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LutFormat {
    #[serde(rename = "cube")]
    Cube,
    #[serde(rename = "3dl")]
    ThreeDl,
    #[serde(rename = "look")]
    Look,
}

/// A Color Lookup's LUT file as loaded: its format and bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LutSource {
    pub format: LutFormat,
    pub bytes: Arc<Vec<u8>>,
}

/// Entries in a pencil-drawn Curves map (one per 8-bit level, as in PSD `curv` maps).
pub const CURVE_MAP_LEN: usize = 256;

/// A pencil-mode curve from its map: `outputs[k]` (0..=1) is the output at input `k / 255`.
/// Missing entries repeat the last one; extra entries are ignored.
pub fn curve_map(outputs: &[f32]) -> Vec<CurvePoint> {
    let last = (CURVE_MAP_LEN - 1) as f32;
    let mut prev = 0.0;
    (0..CURVE_MAP_LEN)
        .map(|k| {
            let out = outputs.get(k).copied().filter(|v| v.is_finite()).map_or(prev, |v| v.clamp(0.0, 1.0));
            prev = out;
            CurvePoint { input: k as f32 / last, output: out }
        })
        .collect()
}

/// The [`CURVE_MAP_LEN`] outputs (0..=1) of `points` read as a pencil-mode curve: straight
/// segments between the points, constant outside the first/last (identity without two points).
pub fn curve_map_outputs(points: &[CurvePoint]) -> Vec<f32> {
    let mut pts: Vec<(f32, f32)> = points.iter().filter(|p| p.input.is_finite() && p.output.is_finite()).map(|p| (p.input, p.output)).collect();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let last = (CURVE_MAP_LEN - 1) as f32;
    (0..CURVE_MAP_LEN)
        .map(|k| {
            let x = k as f32 / last;
            let y = match (pts.first(), pts.last()) {
                (Some(a), Some(b)) if pts.len() >= 2 => {
                    if x <= a.0 {
                        a.1
                    } else if x >= b.0 {
                        b.1
                    } else {
                        let i = pts.partition_point(|p| p.0 < x);
                        match (pts.get(i.wrapping_sub(1)), pts.get(i)) {
                            (Some(p0), Some(p1)) if p1.0 > p0.0 => p0.1 + (p1.1 - p0.1) * (x - p0.0) / (p1.0 - p0.0),
                            (_, Some(p1)) => p1.1,
                            _ => b.1,
                        }
                    }
                }
                _ => x,
            };
            y.clamp(0.0, 1.0)
        })
        .collect()
}

/// Number of values in the table of a `size`³ Color Lookup (`size³ × 3`). `None` for a size
/// below 2 or one whose table length overflows: a stored document's size is untrusted.
pub fn lut3d_len(size: u32) -> Option<usize> {
    if size < 2 {
        return None;
    }
    usize::try_from(size).ok()?.checked_pow(3)?.checked_mul(3)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LevelsChannel {
    pub in_black: f32,
    pub in_white: f32,
    pub gamma: f32,
    pub out_black: f32,
    pub out_white: f32,
}

impl Default for LevelsChannel {
    fn default() -> Self {
        Self { in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0 }
    }
}

/// Which document channels the per-channel records of Levels and Curves address. Compositing
/// happens in display RGB; `Cmyk` and `Lab` records are applied to the pixel converted into that
/// space (the same conversions the document's surfaces use), so ink and Lab channel edits behave
/// like Photoshop's in CMYK and Lab documents. Grayscale documents use `Rgb` with the master record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToneSpace {
    /// `per_channel` = red, green, blue.
    #[default]
    Rgb,
    /// `per_channel` = cyan, magenta, yellow plus `black`; values are channel brightness
    /// (1 - ink), and the master record applies to all four inks.
    Cmyk,
    /// `per_channel` = lightness, a, b (stored 0..=1 like Lab surfaces); there is no composite
    /// record in Lab, so the master is ignored.
    Lab,
}

/// One of Hue/Saturation's six colour ranges (reds, yellows, greens, cyans, blues, magentas).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HueRange {
    /// Hue shift in degrees (-180..=180).
    pub hue: f32,
    /// Saturation change in percent (-100..=100).
    pub saturation: f32,
    /// Lightness change in percent (-100..=100).
    pub lightness: f32,
    /// Hue degrees where the range starts fading in, is fully on, starts fading out and ends
    /// (Photoshop's four range sliders). May wrap past 0/360.
    pub bounds: [f32; 4],
}

impl HueRange {
    /// The neutral range `i` (0 = reds … 5 = magentas) with Photoshop's default extent: fully on
    /// for 30° around its centre, fading over 30° each side.
    pub fn neutral(i: usize) -> Self {
        let c = 60.0 * i as f32;
        Self { hue: 0.0, saturation: 0.0, lightness: 0.0, bounds: [c - 45.0, c - 15.0, c + 15.0, c + 45.0] }
    }

    pub fn defaults() -> [HueRange; 6] {
        std::array::from_fn(Self::neutral)
    }

    /// The canonical form of four range sliders: increasing, within one turn, centre in
    /// -30..330 (so the default reds are -45, -15, 15, 45 whichever way they were written).
    pub fn canonical_bounds(b: [f32; 4]) -> [f32; 4] {
        if b.iter().any(|v| !v.is_finite()) {
            return [-45.0, -15.0, 15.0, 45.0];
        }
        let mut out = [b[0]; 4];
        for k in 1..4 {
            out[k] = (out[k - 1] + (b[k] - out[k - 1]).rem_euclid(360.0)).min(out[0] + 359.0);
        }
        let centre = (out[1] + out[2]) / 2.0;
        let shift = (centre + 30.0).rem_euclid(360.0) - 30.0 - centre;
        out.map(|v| v + shift)
    }

    pub fn is_neutral(&self) -> bool {
        self.hue == 0.0 && self.saturation == 0.0 && self.lightness == 0.0
    }

    /// How much a pixel of hue `deg` (degrees) belongs to this range, 0..=1.
    pub fn weight(&self, deg: f32) -> f32 {
        let [a, b, c, d] = self.bounds;
        // Measure from the range's start so wrap-around ranges (reds) work.
        let rel = |x: f32| if x.is_finite() { (x - a).rem_euclid(360.0) } else { 0.0 };
        let b = rel(b);
        let c = rel(c).max(b);
        let d = rel(d).max(c);
        let h = rel(deg);
        if h <= b {
            if b <= 0.0 { 1.0 } else { h / b }
        } else if h <= c {
            1.0
        } else if h <= d {
            if d <= c { 1.0 } else { (d - h) / (d - c) }
        } else {
            0.0
        }
    }
}

fn default_hue_ranges() -> [HueRange; 6] {
    HueRange::defaults()
}

/// Shading: PhotoCraft's own adjustment (no Photoshop equivalent) for painting shadows on a
/// colouring. The layer mask is the shadow amount, and each colour picks its own shadow colour:
/// darker; richer by the room it has before the gamut edge (greys stay grey, skin gets richer,
/// vivid colours barely change); and turned away from yellow toward blue-violet the shorter way
/// round the hue wheel (skin goes red, blonde gold, cyan cloth blue). Works in OKLab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shading {
    /// How much lightness full shadow takes away (0..=1).
    pub depth: f32,
    /// How far full shadow moves chroma toward the gamut edge (0..=1).
    pub saturation: f32,
    /// How the saturation runs between no shadow and full shadow (-1..=1): 0 as the rules give it, above
    /// 0 swung richer through the half-shadow tones (to the most vivid halfway at 1), below 0 greyer.
    /// The ends stay put.
    pub saturation_curve: f32,
    /// How far hues turn at full shadow, in degrees (0..=90).
    pub hue_shift: f32,
    /// The OKLCH hue (degrees) on either side of which colours turn opposite ways: below it
    /// toward red, above it toward blue; its opposite is where they meet. Default a little
    /// past yellow (125°), so pale blonde and gold stay warm (at 110–118° pale blonde went olive).
    pub split_hue: f32,
    /// How grey a colour must be for hue and saturation to leave it alone (0..=1).
    pub protect_neutrals: f32,
    /// The colour every shadowed colour leans toward, as a small OKLab offset (sRGB 0..=1), and
    /// how much (0..=1): the same offset turns greys and pastels a long way and vivid colours
    /// hardly at all, as sky-lit shade does.
    pub ambient: [f32; 3],
    pub ambient_amount: f32,
    /// A Multiply-style tint over the shadow (sRGB 0..=1), and how much (0..=1).
    pub mood: [f32; 3],
    pub mood_amount: f32,
    /// How much near-black colours are left alone (0..=1).
    pub protect_darks: f32,
    /// 0 or 1: smooth; 2..=8: the mask snaps to this many shadow tones (cel shading).
    pub steps: u32,
    /// How much chroma survives the darkening (0..=1): 0 dims it with the light (physical,
    /// C·L′/L), 0.5 is palette practice (C·√(L′/L)), 1 keeps it whole (graphic).
    pub chroma_retention: f32,
    /// Light bouncing within the colour itself (0..=1): each channel's shadow lifted by its own
    /// reflectance (multi-bounce occlusion), so yellows shade ochre and oranges red instead of
    /// olive and brown, at the same lightness. Missing from older files: 0 there.
    #[serde(default)]
    pub self_bounce: f32,
    /// The terminator band: a warm hue-and-chroma bump where light turns to shadow (skin's
    /// subsurface glow). Its colour (only the hue is used; sRGB 0..=1), amount (0..=1; 0 in
    /// older files), where it peaks on the shadow amount (0..=1) and how wide it is (0.05..=1).
    pub terminator: [f32; 3],
    #[serde(default)]
    pub terminator_amount: f32,
    pub terminator_position: f32,
    pub terminator_width: f32,
    /// Shadow colour from physics instead of the artistic hue turn: the local colour times the
    /// ambient colour as the light (linear RGB), at the depth rule's lightness.
    pub physical: bool,
    /// "Shading 2" (user test, 2026-11-10): the colour picker's axes. Depth is the brightness
    /// (straight down the picker: chroma dims with the light, C/L kept), and `saturation_shift`
    /// moves sideways at that lightness, toward the picker's vivid edge above 0 or grey below
    /// (chroma relative to the most vivid colour at that lightness and hue), replacing Saturation
    /// and Chroma Retention.
    #[serde(default)]
    pub picker_axes: bool,
    pub saturation_shift: f32,
    /// Advanced Curve, drawn by hand: the path a colour takes on the colour picker, from the base
    /// colour (0, 1) to full shadow (1, 0), stretched over each colour's own (x saturation, y
    /// brightness); points may sit half a box past it, toward the picker's edges. Empty: the
    /// Saturation Curve slider's curve (see `sanitized_points`).
    #[serde(default)]
    pub saturation_points: Vec<CurvePoint>,
    /// Hue Curve, drawn by hand: x the amount (0 no shadow, 1 full shadow), y how far the hue has
    /// turned (1 the base colour's, 0 full shadow's; down to -0.5 turns further, up to 1.5 back).
    /// Empty: the path's own timing (see `sanitized_hue_points`).
    #[serde(default)]
    pub hue_points: Vec<CurvePoint>,
    /// Literal Curve: the Advanced Curve keeps its drawn shape on the picker (the same scale both
    /// ways, sized to each colour's own path) instead of being stretched to each colour's box.
    #[serde(default)]
    pub literal_curve: bool,
}

impl Default for Shading {
    fn default() -> Self {
        Self {
            depth: 0.35,
            saturation: 0.2,
            saturation_curve: 0.0,
            hue_shift: 22.0,
            split_hue: 125.0,
            protect_neutrals: 0.5,
            ambient: [0.45, 0.45, 0.8],
            ambient_amount: 0.06,
            mood: [0.8, 0.68, 0.82],
            mood_amount: 0.0,
            protect_darks: 0.5,
            steps: 0,
            chroma_retention: 0.5,
            self_bounce: 0.3,
            terminator: [0.9, 0.35, 0.25],
            terminator_amount: 0.3,
            terminator_position: 0.4,
            terminator_width: 0.3,
            physical: false,
            picker_axes: false,
            saturation_shift: 0.3,
            saturation_points: Vec::new(),
            hue_points: Vec::new(),
            literal_curve: false,
        }
    }
}

impl Shading {
    /// A new "Shading 2" (picker axes) at its defaults.
    pub fn picker() -> Self {
        Self { picker_axes: true, ..Self::default() }
    }

    /// The settings with every number finite and in its range.
    pub fn sanitized(&self) -> Self {
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        let c = |v: [f32; 3], d: [f32; 3]| [f(v[0], 0.0, 1.0, d[0]), f(v[1], 0.0, 1.0, d[1]), f(v[2], 0.0, 1.0, d[2])];
        let d = Self::default();
        Self {
            depth: f(self.depth, 0.0, 1.0, d.depth),
            saturation: f(self.saturation, 0.0, 1.0, d.saturation),
            saturation_curve: f(self.saturation_curve, -1.0, 1.0, d.saturation_curve),
            hue_shift: f(self.hue_shift, 0.0, 90.0, d.hue_shift),
            split_hue: if self.split_hue.is_finite() { self.split_hue.rem_euclid(360.0) } else { d.split_hue },
            protect_neutrals: f(self.protect_neutrals, 0.0, 1.0, d.protect_neutrals),
            ambient: c(self.ambient, d.ambient),
            ambient_amount: f(self.ambient_amount, 0.0, 1.0, d.ambient_amount),
            mood: c(self.mood, d.mood),
            mood_amount: f(self.mood_amount, 0.0, 1.0, d.mood_amount),
            protect_darks: f(self.protect_darks, 0.0, 1.0, d.protect_darks),
            steps: self.steps.min(8),
            chroma_retention: f(self.chroma_retention, 0.0, 1.0, d.chroma_retention),
            self_bounce: f(self.self_bounce, 0.0, 1.0, d.self_bounce),
            terminator: c(self.terminator, d.terminator),
            terminator_amount: f(self.terminator_amount, 0.0, 1.0, d.terminator_amount),
            terminator_position: f(self.terminator_position, 0.0, 1.0, d.terminator_position),
            terminator_width: f(self.terminator_width, 0.05, 1.0, d.terminator_width),
            physical: self.physical,
            picker_axes: self.picker_axes,
            saturation_shift: f(self.saturation_shift, -1.0, 1.0, d.saturation_shift),
            saturation_points: sanitized_points(&self.saturation_points),
            hue_points: sanitized_hue_points(&self.hue_points),
            literal_curve: self.literal_curve,
        }
    }
}

/// Lighting: the mirror of [`Shading`], for painting light on a colouring. The layer mask is the
/// light amount, and each colour picks its own lit colour: lighter (toward white, so whites can't
/// clip); hue turned toward `light_hue` (warm yellow by default) the shorter way round, so skin
/// warms to golden, cyan goes toward green and purple toward pink; chroma kept against the gamut
/// edge (lit yellows glow, lit pastels stay clean); near-black linework stays put. Works in OKLab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lighting {
    /// How far full light lifts lightness toward white (0..=1).
    pub lift: f32,
    /// How far full light moves chroma toward the gamut edge (0..=1).
    pub saturation: f32,
    /// How the saturation runs between no light and full light (-1..=1): 0 as the rules give it, above
    /// 0 swung richer through the half-light tones (to the most vivid halfway at 1), below 0 greyer.
    /// The ends stay put.
    pub saturation_curve: f32,
    /// How much of their chroma the most lit tones (the top 30% of the light amount) give up for
    /// the light's colour, so highlights read as light rather than neon local colour (0..=1).
    /// Missing from files saved before it existed: 0 there, so they look as they did.
    #[serde(default)]
    pub highlight_fade: f32,
    /// How far hues turn at full light, in degrees (0..=90).
    pub hue_shift: f32,
    /// The OKLCH hue (degrees) lit colours turn toward (default yellow, 108°: the light pull
    /// measured across hue-shifted palette ramps is ≈109°, and at 95° lemon yellows went orange).
    pub light_hue: f32,
    /// How grey a colour must be for hue and saturation to leave it alone (0..=1).
    pub protect_neutrals: f32,
    /// The light's colour, which every lit colour leans toward as a small OKLab offset (greys and
    /// whites the most visibly; sRGB 0..=1), and how much (0..=1). Highlight Fade heads toward it.
    pub light: [f32; 3],
    pub light_amount: f32,
    /// A Screen-style glow over the light (sRGB 0..=1), and how much (0..=1).
    pub glow: [f32; 3],
    pub glow_amount: f32,
    /// How much near-black colours (linework) are left alone (0..=1).
    pub protect_darks: f32,
    /// 0 or 1: smooth; 2..=8: the mask snaps to this many light tones (cel shading).
    pub steps: u32,
    /// Gloss: a narrow, near-white shine at the very top of the light (hair, lips, plastic), in
    /// the light's colour; how strong (0..=1, 0 in older files) and how much of the top of the
    /// light amount it covers (0.02..=0.5).
    #[serde(default)]
    pub gloss: f32,
    pub gloss_size: f32,
    /// Metal: the shine and the top of the light take the surface's own colour (gold shines
    /// gold) instead of the light's, and Highlight Fade keeps the colour.
    pub metallic: bool,
    /// Advanced Curve, drawn by hand: the path a colour takes on the colour picker, from the base
    /// colour (0, 1) to full light (1, 0), stretched over each colour's own; points may sit half a
    /// box past it, toward the picker's edges. Empty: the Saturation Curve slider's curve.
    #[serde(default)]
    pub saturation_points: Vec<CurvePoint>,
    /// Hue Curve, drawn by hand: x the amount (0 no light, 1 full light), y how far the hue has
    /// turned (1 the base colour's, 0 full light's; below 0 further, above 1 back). Empty: the
    /// path's own timing.
    #[serde(default)]
    pub hue_points: Vec<CurvePoint>,
    /// Literal Curve: the Advanced Curve keeps its drawn shape on the picker (the same scale both
    /// ways, sized to each colour's own path) instead of being stretched to each colour's box.
    #[serde(default)]
    pub literal_curve: bool,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            lift: 0.35,
            saturation: 0.2,
            saturation_curve: 0.0,
            highlight_fade: 0.4,
            hue_shift: 15.0,
            light_hue: 108.0,
            protect_neutrals: 0.5,
            light: [1.0, 0.85, 0.6],
            light_amount: 0.25,
            glow: [1.0, 0.9, 0.7],
            glow_amount: 0.0,
            protect_darks: 0.9,
            steps: 0,
            gloss: 0.0,
            gloss_size: 0.15,
            metallic: false,
            saturation_points: Vec::new(),
            hue_points: Vec::new(),
            literal_curve: false,
        }
    }
}

impl Lighting {
    /// The settings with every number finite and in its range.
    pub fn sanitized(&self) -> Self {
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        let c = |v: [f32; 3], d: [f32; 3]| [f(v[0], 0.0, 1.0, d[0]), f(v[1], 0.0, 1.0, d[1]), f(v[2], 0.0, 1.0, d[2])];
        let d = Self::default();
        Self {
            lift: f(self.lift, 0.0, 1.0, d.lift),
            saturation: f(self.saturation, 0.0, 1.0, d.saturation),
            saturation_curve: f(self.saturation_curve, -1.0, 1.0, d.saturation_curve),
            highlight_fade: f(self.highlight_fade, 0.0, 1.0, d.highlight_fade),
            hue_shift: f(self.hue_shift, 0.0, 90.0, d.hue_shift),
            light_hue: if self.light_hue.is_finite() { self.light_hue.rem_euclid(360.0) } else { d.light_hue },
            protect_neutrals: f(self.protect_neutrals, 0.0, 1.0, d.protect_neutrals),
            light: c(self.light, d.light),
            light_amount: f(self.light_amount, 0.0, 1.0, d.light_amount),
            glow: c(self.glow, d.glow),
            glow_amount: f(self.glow_amount, 0.0, 1.0, d.glow_amount),
            protect_darks: f(self.protect_darks, 0.0, 1.0, d.protect_darks),
            steps: self.steps.min(8),
            gloss: f(self.gloss, 0.0, 1.0, d.gloss),
            gloss_size: f(self.gloss_size, 0.02, 0.5, d.gloss_size),
            metallic: self.metallic,
            saturation_points: sanitized_points(&self.saturation_points),
            hue_points: sanitized_hue_points(&self.hue_points),
            literal_curve: self.literal_curve,
        }
    }
}

/// Chromatic Aberration: PhotoCraft's lens-fringe adjustment layer (user request, 2026-11-21).
/// The red and blue channels of the composite beneath are shifted against green, which stays
/// put: radially (scaled from `center`, like a lens whose magnification differs per colour, so
/// edges far from the centre get red/cyan and blue/yellow fringes) and by a fixed offset (red
/// one way, blue the other: the "RGB split" look). Distances are in document pixels.
///
/// It samples neighbouring pixels, so a compositor renders the pixels within [`Self::reach`] of
/// what it draws too. Samples are taken inside the canvas (clamped to its edge); pixels outside
/// the canvas and every alpha value are left as they are. Saved in `.pcraft`, flattened in PSD.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChromaticAberration {
    /// How far the red channel moves outward from the centre at the farthest canvas corner, in
    /// pixels (-50..=50; below 0 it moves inward). It grows linearly from nothing at the centre.
    pub red_shift: f32,
    /// The same for the blue channel (-50..=50).
    pub blue_shift: f32,
    /// A fixed shift of the red channel, in pixels (-50..=50 each); blue moves the opposite way.
    pub offset_x: f32,
    pub offset_y: f32,
    /// The centre of the radial shift, as fractions of the canvas width and height (0..=1).
    pub center: [f32; 2],
    /// How much of the image centre is left clean (0..=1): the fraction of the centre-to-corner
    /// distance where the effect reaches full strength, fading in from half that distance. It
    /// scales the offset as well as the radial shift. 0: the effect reaches the centre.
    pub falloff: f32,
    /// Softness of the fringes, in pixels (0..=10): the shifted red and blue are blurred this
    /// much where they have moved a pixel or more, so a clean centre stays sharp.
    pub softness: f32,
}

impl Default for ChromaticAberration {
    fn default() -> Self {
        Self { red_shift: 3.0, blue_shift: -3.0, offset_x: 0.0, offset_y: 0.0, center: [0.5, 0.5], falloff: 0.0, softness: 0.0 }
    }
}

impl ChromaticAberration {
    /// Largest shift either way, in pixels.
    pub const MAX_SHIFT: f32 = 50.0;
    /// Largest softness, in pixels.
    pub const MAX_SOFTNESS: f32 = 10.0;

    /// The settings with every number finite and in its range.
    pub fn sanitized(&self) -> Self {
        let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        let d = Self::default();
        let m = Self::MAX_SHIFT;
        Self {
            red_shift: f(self.red_shift, -m, m, d.red_shift),
            blue_shift: f(self.blue_shift, -m, m, d.blue_shift),
            offset_x: f(self.offset_x, -m, m, d.offset_x),
            offset_y: f(self.offset_y, -m, m, d.offset_y),
            center: [f(self.center[0], 0.0, 1.0, d.center[0]), f(self.center[1], 0.0, 1.0, d.center[1])],
            falloff: f(self.falloff, 0.0, 1.0, d.falloff),
            softness: f(self.softness, 0.0, Self::MAX_SOFTNESS, d.softness),
        }
    }

    /// The softness blur's radius in pixels (a Gaussian of sigma `softness / 2`, cut at 3 sigma).
    pub fn blur_radius(&self) -> i32 {
        let s = self.sanitized().softness;
        if s <= 0.0 { 0 } else { (s * 1.5).ceil() as i32 }
    }

    /// How far from a pixel the result there can read, in pixels: the largest shift, the softness
    /// blur and one more for the bilinear sample. 0 when it changes nothing.
    pub fn reach(&self) -> i32 {
        let s = self.sanitized();
        if s.is_identity() {
            return 0;
        }
        let shift = s.red_shift.abs().max(s.blue_shift.abs()) + s.offset_x.hypot(s.offset_y);
        shift.ceil() as i32 + self.blur_radius() + 1
    }

    /// Whether it leaves every pixel as it is (no shift at all).
    pub fn is_identity(&self) -> bool {
        self.red_shift == 0.0 && self.blue_shift == 0.0 && self.offset_x == 0.0 && self.offset_y == 0.0
    }

    /// The same settings for a copy of the document scaled down by `k` (a proxy or a reduced
    /// preview): the pixel distances shrink with it.
    pub fn scaled(&self, k: f32) -> Self {
        let k = if k.is_finite() && k > 0.0 { k } else { 1.0 };
        Self {
            red_shift: self.red_shift / k,
            blue_shift: self.blue_shift / k,
            offset_x: self.offset_x / k,
            offset_y: self.offset_y / k,
            softness: self.softness / k,
            ..self.clone()
        }
    }
}

/// Shading's and Lighting's Advanced Curve kept usable: a path in the order drawn, from the base
/// colour (0, 1) to full shadow or light (1, 0), its points within the graph (half a box past
/// the box on each side: -0.5..=1.5), at most 16, none on top of the one before. Fewer than one
/// point between the ends is no curve (empty).
pub fn sanitized_points(points: &[CurvePoint]) -> Vec<CurvePoint> {
    let mut p: Vec<CurvePoint> = Vec::new();
    for q in points.iter().filter(|q| q.input.is_finite() && q.output.is_finite()) {
        let q = CurvePoint { input: q.input.clamp(-0.5, 1.5), output: q.output.clamp(-0.5, 1.5) };
        if p.last().is_none_or(|l: &CurvePoint| (l.input - q.input).hypot(l.output - q.output) >= 1e-4) {
            p.push(q);
        }
    }
    if p.len() < 3 {
        return Vec::new();
    }
    // The ends are the box's corners; the middle keeps at most 14 points.
    let mut mid: Vec<CurvePoint> = p[1..p.len() - 1].to_vec();
    mid.truncate(14);
    let mut out = vec![CurvePoint { input: 0.0, output: 1.0 }];
    out.extend(mid);
    out.push(CurvePoint { input: 1.0, output: 0.0 });
    out
}

/// Shading's and Lighting's Hue Curve kept usable: points sorted by amount (0..=1), how far the
/// hue has turned in -0.5..=1.5, no two at one amount, the ends pinned at (0, 1) and (1, 0), at
/// most 14 between them. None between the ends is no curve (empty).
pub fn sanitized_hue_points(points: &[CurvePoint]) -> Vec<CurvePoint> {
    let mut mid: Vec<CurvePoint> = points
        .iter()
        .filter(|q| q.input.is_finite() && q.output.is_finite() && q.input > 1e-3 && q.input < 1.0 - 1e-3)
        .map(|q| CurvePoint { input: q.input, output: q.output.clamp(-0.5, 1.5) })
        .collect();
    mid.sort_by(|a, b| a.input.total_cmp(&b.input));
    mid.dedup_by(|later, earlier| (later.input - earlier.input).abs() < 1e-3);
    mid.truncate(14);
    if mid.is_empty() {
        return Vec::new();
    }
    let mut out = vec![CurvePoint { input: 0.0, output: 1.0 }];
    out.extend(mid);
    out.push(CurvePoint { input: 1.0, output: 0.0 });
    out
}

/// Photoshop adjustment layers (Layer → New Adjustment Layer).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Adjustment {
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
        legacy: bool,
    },
    /// R, G, B (or the channels `space` names) first, then the master (composite) record.
    /// In CMYK this also applies to `black`; in Lab the master is ignored.
    Levels {
        master: LevelsChannel,
        per_channel: [LevelsChannel; 3],
        #[serde(default)]
        space: ToneSpace,
        /// The black ink channel when `space` is [`ToneSpace::Cmyk`].
        #[serde(default)]
        black: LevelsChannel,
    },
    /// R, G, B curves (or the channels `space` names) first, then the master (composite) curve.
    /// In CMYK this also applies to `black`; in Lab the master is ignored.
    Curves {
        master: Vec<CurvePoint>,
        per_channel: [Vec<CurvePoint>; 3],
        #[serde(default)]
        space: ToneSpace,
        /// The black ink curve when `space` is [`ToneSpace::Cmyk`] (empty = identity).
        #[serde(default)]
        black: Vec<CurvePoint>,
        /// Drawn with the pencil (Photoshop's "Draw to modify the curve", PSD `curv` map form):
        /// every curve is a [`CURVE_MAP_LEN`]-entry map (see [`curve_map`]) evaluated
        /// piecewise-linearly instead of a spline through its points.
        #[serde(default)]
        pencil: bool,
    },
    Exposure {
        exposure: f32,
        offset: f32,
        gamma: f32,
    },
    Vibrance {
        vibrance: f32,
        saturation: f32,
    },
    /// Master hue / saturation / lightness (or the Colorize colour) plus six hue ranges.
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
        colorize: bool,
        #[serde(default = "default_hue_ranges")]
        ranges: [HueRange; 6],
    },
    ColorBalance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        preserve_luminosity: bool,
    },
    BlackWhite {
        weights: [f32; 6],
        tint: Option<[f32; 3]>,
    },
    PhotoFilter {
        color: [f32; 3],
        density: f32,
        preserve_luminosity: bool,
    },
    ChannelMixer {
        matrix: [[f32; 4]; 3],
        monochrome: bool,
    },
    /// A 3D LUT: `size`³ RGB triplets, red varying fastest (`((b·size + g)·size + r)·3`), in 0..=1.
    /// `lut` is None for a lookup Photoshop stores as an ICC profile (identity here).
    ColorLookup {
        name: String,
        lut: Option<Arc<Vec<f32>>>,
        size: u32,
        /// Tetrahedral instead of trilinear interpolation.
        #[serde(default)]
        tetrahedral: bool,
        /// Ordered dither of ±½ an 8-bit step to hide banding (Photoshop's "Dither").
        #[serde(default)]
        dither: bool,
        /// The LUT file `lut` was read from, embedded unchanged on PSD save as Photoshop does.
        /// None for a table made in PhotoCraft (a built-in look); a save also ignores it when
        /// it no longer parses to `lut`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<LutSource>,
    },
    Invert,
    Posterize {
        levels: u32,
    },
    Threshold {
        level: f32,
    },
    GradientMap {
        /// The gradient as rendered: linear sRGB stops.
        stops: Vec<(f32, [f32; 3])>,
        reverse: bool,
        /// Ordered dither of ±½ an 8-bit step to hide banding.
        #[serde(default)]
        dither: bool,
        /// Colour midpoints, one per segment between consecutive stops (missing entries 0.5).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        midpoints: Vec<f32>,
        /// How the colours blend between the stops (Photoshop's method and Smoothness).
        #[serde(default, skip_serializing_if = "crate::effects::GradientBlend::is_plain")]
        blend: crate::effects::GradientBlend,
    },
    /// Per range (reds, yellows, greens, cyans, blues, magentas, whites, neutrals, blacks) the
    /// cyan, magenta, yellow and black change in percent (-100..=100), as in Photoshop.
    SelectiveColor {
        relative: bool,
        adjustments: [[f32; 4]; 9],
    },
    /// Photoshop 2026's Grain: film-style monochrome noise, the same for a document every time it
    /// renders (it depends only on the pixel's position). Amount, Size and Roughness in 0..=100.
    Grain {
        amount: f32,
        size: f32,
        roughness: f32,
        /// Photoshop's seed for its noise, kept so a PSD round trip gives Photoshop back the same
        /// grain; PhotoCraft's own grain depends only on the pixel's position.
        #[serde(default)]
        seed: u32,
    },
    /// PhotoCraft's Shading (see [`Shading`]); saved in `.pcraft`, flattened in PSD.
    Shading(Shading),
    /// PhotoCraft's Lighting (see [`Lighting`]); saved in `.pcraft`, flattened in PSD.
    Lighting(Lighting),
    /// PhotoCraft's Chromatic Aberration (see [`ChromaticAberration`]): the one adjustment that
    /// reads neighbouring pixels; saved in `.pcraft`, flattened in PSD.
    ChromaticAberration(ChromaticAberration),
    /// A PSD adjustment we can't evaluate yet; preserved raw for round-trip, shared between
    /// document snapshots (#4535).
    Unsupported {
        psd_key: String,
        raw: std::sync::Arc<[u8]>,
    },
}

/// A Gradient Map's stops to render: as stored, or dense stops following its midpoints and
/// blend (Photoshop's method and Smoothness).
pub fn gradient_map_stops<'a>(
    stops: &'a [(f32, [f32; 3])],
    midpoints: &[f32],
    blend: &crate::effects::GradientBlend,
) -> std::borrow::Cow<'a, [(f32, [f32; 3])]> {
    if blend.is_plain() && midpoints.iter().all(|m| (m - 0.5).abs() < 1e-3) {
        return std::borrow::Cow::Borrowed(stops);
    }
    let colors = stops.iter().map(|(t, c)| (*t, photocraft_color::Color::rgb(c[0], c[1], c[2]))).collect();
    std::borrow::Cow::Owned(blend.bake(colors, midpoints).into_iter().map(|(t, c)| (t, c.to_rgb())).collect())
}

impl Adjustment {
    pub fn label(&self) -> &str {
        match self {
            Adjustment::BrightnessContrast { .. } => "Brightness/Contrast",
            Adjustment::Levels { .. } => "Levels",
            Adjustment::Curves { .. } => "Curves",
            Adjustment::Exposure { .. } => "Exposure",
            Adjustment::Vibrance { .. } => "Vibrance",
            Adjustment::HueSaturation { .. } => "Hue/Saturation",
            Adjustment::ColorBalance { .. } => "Color Balance",
            Adjustment::BlackWhite { .. } => "Black & White",
            Adjustment::PhotoFilter { .. } => "Photo Filter",
            Adjustment::ChannelMixer { .. } => "Channel Mixer",
            Adjustment::ColorLookup { .. } => "Color Lookup",
            Adjustment::Invert => "Invert",
            Adjustment::Posterize { .. } => "Posterize",
            Adjustment::Threshold { .. } => "Threshold",
            Adjustment::GradientMap { .. } => "Gradient Map",
            Adjustment::SelectiveColor { .. } => "Selective Color",
            Adjustment::Grain { .. } => "Grain",
            Adjustment::Shading(s) if s.picker_axes => "Shading 2",
            Adjustment::Shading(_) => "Shading",
            Adjustment::Lighting(_) => "Lighting",
            Adjustment::ChromaticAberration(_) => "Chromatic Aberration",
            Adjustment::Unsupported { .. } => "Adjustment",
        }
    }

    pub fn default_hue_saturation() -> Self {
        Adjustment::HueSaturation { hue: 0.0, saturation: 0.0, lightness: 0.0, colorize: false, ranges: HueRange::defaults() }
    }

    pub fn identity_curves() -> Self {
        let line = || vec![CurvePoint { input: 0.0, output: 0.0 }, CurvePoint { input: 1.0, output: 1.0 }];
        Adjustment::Curves { master: line(), per_channel: [line(), line(), line()], space: ToneSpace::Rgb, black: Vec::new(), pencil: false }
    }

    pub fn identity_levels() -> Self {
        Adjustment::Levels { master: LevelsChannel::default(), per_channel: Default::default(), space: ToneSpace::Rgb, black: LevelsChannel::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_range_weights() {
        let reds = HueRange::neutral(0);
        assert_eq!(reds.weight(0.0), 1.0);
        assert_eq!(reds.weight(350.0), 1.0);
        assert!((reds.weight(30.0) - 0.5).abs() < 1e-5);
        assert!((reds.weight(330.0) - 0.5).abs() < 1e-5);
        assert_eq!(reds.weight(60.0), 0.0);
        assert_eq!(reds.weight(180.0), 0.0);
        let blues = HueRange::neutral(4);
        assert_eq!(blues.weight(240.0), 1.0);
        assert_eq!(blues.weight(120.0), 0.0);
        // Degenerate bounds never divide by zero.
        let odd = HueRange { bounds: [10.0, 10.0, 10.0, 10.0], ..HueRange::neutral(0) };
        assert!(odd.weight(10.0).is_finite() && odd.weight(200.0).is_finite());
        let nan = HueRange { bounds: [f32::NAN; 4], ..HueRange::neutral(0) };
        assert!(nan.weight(10.0).is_finite());
    }

    #[test]
    fn old_files_get_neutral_new_fields() {
        let a: Adjustment = serde_json::from_str(r#"{"HueSaturation":{"hue":1.0,"saturation":2.0,"lightness":3.0,"colorize":false}}"#).unwrap();
        assert!(matches!(a, Adjustment::HueSaturation { ranges, .. } if ranges == HueRange::defaults()));
        let c: Adjustment = serde_json::from_str(r#"{"Curves":{"master":[],"per_channel":[[],[],[]]}}"#).unwrap();
        assert!(matches!(c, Adjustment::Curves { space: ToneSpace::Rgb, pencil: false, .. }));
        let g: Adjustment = serde_json::from_str(r#"{"GradientMap":{"stops":[],"reverse":true}}"#).unwrap();
        assert!(matches!(g, Adjustment::GradientMap { dither: false, .. }));
    }

    #[test]
    fn chromatic_aberration_reads_missing_fields_as_defaults_and_sanitizes() {
        let a: Adjustment = serde_json::from_str(r#"{"ChromaticAberration":{"red_shift":5.0}}"#).unwrap();
        assert_eq!(a, Adjustment::ChromaticAberration(ChromaticAberration { red_shift: 5.0, ..ChromaticAberration::default() }));
        assert_eq!(a.label(), "Chromatic Aberration");
        let wild = ChromaticAberration {
            red_shift: f32::NAN,
            blue_shift: -1e9,
            offset_x: f32::INFINITY,
            offset_y: 7.0,
            center: [-3.0, f32::NAN],
            falloff: 2.0,
            softness: 99.0,
        };
        let s = wild.sanitized();
        assert_eq!((s.red_shift, s.blue_shift, s.offset_x, s.offset_y), (3.0, -50.0, 0.0, 7.0));
        assert_eq!((s.center, s.falloff, s.softness), ([0.0, 0.5], 1.0, 10.0));
        assert_eq!(ChromaticAberration::default().sanitized(), ChromaticAberration::default());
        // Reach: the largest shift, the blur and the bilinear sample; nothing when it shifts nothing.
        assert_eq!(ChromaticAberration::default().reach(), 4);
        assert_eq!(ChromaticAberration { offset_x: 3.0, offset_y: 4.0, softness: 2.0, ..Default::default() }.reach(), 3 + 5 + 3 + 1);
        assert_eq!(wild.reach(), 50 + 7 + 15 + 1);
        assert_eq!(ChromaticAberration { red_shift: 0.0, blue_shift: 0.0, softness: 5.0, ..Default::default() }.reach(), 0);
        let half = ChromaticAberration { offset_x: 4.0, softness: 2.0, ..Default::default() }.scaled(2.0);
        assert_eq!((half.red_shift, half.blue_shift, half.offset_x, half.softness, half.center), (1.5, -1.5, 2.0, 1.0, [0.5, 0.5]));
    }

    #[test]
    fn shading_reads_missing_fields_as_defaults_and_sanitizes() {
        let a: Adjustment = serde_json::from_str(r#"{"Shading":{"depth":0.5}}"#).unwrap();
        assert_eq!(a, Adjustment::Shading(Shading { depth: 0.5, self_bounce: 0.0, terminator_amount: 0.0, ..Shading::default() }));
        let wild = Shading {
            depth: f32::NAN,
            hue_shift: 1e9,
            split_hue: -90.0,
            steps: 99,
            saturation_curve: -7.0,
            ambient: [2.0, -1.0, f32::INFINITY],
            ..Shading::default()
        };
        let s = wild.sanitized();
        assert_eq!((s.depth, s.hue_shift, s.split_hue, s.steps, s.saturation_curve), (0.35, 90.0, 270.0, 8, -1.0));
        assert_eq!(s.ambient, [1.0, 0.0, 0.8]);
        assert_eq!(Shading::default().sanitized(), Shading::default());
    }

    #[test]
    fn lighting_reads_missing_fields_as_defaults_and_sanitizes() {
        let a: Adjustment = serde_json::from_str(r#"{"Lighting":{"lift":0.5}}"#).unwrap();
        // Highlight Fade came later: files without it keep their look (no fade).
        assert_eq!(a, Adjustment::Lighting(Lighting { lift: 0.5, highlight_fade: 0.0, ..Lighting::default() }));
        let wild = Lighting {
            lift: f32::NAN,
            hue_shift: 1e9,
            light_hue: -90.0,
            steps: 99,
            saturation_curve: f32::NAN,
            highlight_fade: 3.0,
            light: [2.0, -1.0, f32::INFINITY],
            ..Lighting::default()
        };
        let s = wild.sanitized();
        assert_eq!((s.lift, s.hue_shift, s.light_hue, s.steps, s.saturation_curve, s.highlight_fade), (0.35, 90.0, 270.0, 8, 0.0, 1.0));
        assert_eq!(s.light, [1.0, 0.0, 0.6]);
        assert_eq!(Lighting::default().sanitized(), Lighting::default());
    }

    /// Variation was taken out of Shading and Lighting (user request, 2026-11-09); files saved
    /// with it still open, and Saturation Curve reads as 0 (a straight line) from them.
    #[test]
    fn shading_and_lighting_saved_with_variation_still_open() {
        let a: Adjustment = serde_json::from_str(r#"{"Shading":{"depth":0.4,"variation":0.5,"variation_size":40.0}}"#).unwrap();
        assert_eq!(a, Adjustment::Shading(Shading { depth: 0.4, self_bounce: 0.0, terminator_amount: 0.0, ..Shading::default() }));
        let a: Adjustment = serde_json::from_str(r#"{"Lighting":{"lift":0.6,"variation":0.2,"variation_size":12.0}}"#).unwrap();
        assert_eq!(a, Adjustment::Lighting(Lighting { lift: 0.6, highlight_fade: 0.0, ..Lighting::default() }));
        // Self-bounce and the terminator band came later too: off in files without them.
        let a: Adjustment = serde_json::from_str(r#"{"Shading":{"depth":0.4}}"#).unwrap();
        assert_eq!(a, Adjustment::Shading(Shading { depth: 0.4, self_bounce: 0.0, terminator_amount: 0.0, ..Shading::default() }));
        assert_eq!((Shading::default().saturation_curve, Lighting::default().saturation_curve), (0.0, 0.0));
    }

    #[test]
    fn lut3d_len_rejects_tiny_and_overflowing_sizes() {
        assert_eq!(lut3d_len(0), None);
        assert_eq!(lut3d_len(1), None);
        assert_eq!(lut3d_len(2), Some(24));
        assert_eq!(lut3d_len(33), Some(33 * 33 * 33 * 3));
        assert_eq!(lut3d_len(1 << 22), None, "(2²²)³ overflows a 64-bit length");
        assert_eq!(lut3d_len(u32::MAX), None);
    }
}
