//! Camera Raw Filter › Save Settings… / Load Settings… (#4274): Adobe Camera Raw settings files
//! (`.xmp`), the `crs:` namespace (`http://ns.adobe.com/camera-raw-settings/1.0/`) on an
//! `x:xmpmeta/rdf:RDF/rdf:Description`, as attributes or as child elements.
//!
//! Loading applies only the settings the file holds, over the current ones, as Camera Raw's
//! Load Settings does; settings PhotoCraft has no control for (lens profiles, geometry, masks,
//! creative profiles, Point Color, …) are ignored and named in the report. Saving writes every
//! supported control, so loading the file in Photoshop reproduces the dialog.
//!
//! Presets are settings files with a name and a group (`crs:Name`, `crs:Group`, as Camera Raw's
//! Settings folder holds them): [`preset_info`] reads those, [`save_preset`] writes one holding
//! only the chosen [`Section`]s, and [`blend`] mixes a preset's settings with the ones it was
//! applied over (the preset's Amount).
//!
//! White balance: a Camera Raw Filter on a rendered image uses the relative
//! `IncrementalTemperature` / `IncrementalTint` (−100..=100), which are PhotoCraft's Temperature
//! and Tint. The absolute `Temperature` (kelvin) / `Tint` of raw-file settings have no
//! counterpart and are ignored.

use photocraft_algo::camera_raw::{CameraRaw, validate_curve};

/// The Camera Raw settings namespace.
pub const CRS_NS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
/// Largest settings file read; real ones are a few kilobytes.
pub const MAX_XMP_BYTES: usize = 4 * 1024 * 1024;

type Field = fn(&mut CameraRaw) -> &mut f32;

/// `crs:` scalar name, PhotoCraft field, and the slider range values are clamped to.
const SCALARS: &[(&str, Field, f32, f32)] = &[
    ("IncrementalTemperature", |p| &mut p.temperature, -100.0, 100.0),
    ("IncrementalTint", |p| &mut p.tint, -100.0, 100.0),
    ("Exposure2012", |p| &mut p.exposure, -5.0, 5.0),
    ("Contrast2012", |p| &mut p.contrast, -100.0, 100.0),
    ("Highlights2012", |p| &mut p.highlights, -100.0, 100.0),
    ("Shadows2012", |p| &mut p.shadows, -100.0, 100.0),
    ("Whites2012", |p| &mut p.whites, -100.0, 100.0),
    ("Blacks2012", |p| &mut p.blacks, -100.0, 100.0),
    ("Texture", |p| &mut p.texture, -100.0, 100.0),
    ("Clarity2012", |p| &mut p.clarity, -100.0, 100.0),
    ("Dehaze", |p| &mut p.dehaze, -100.0, 100.0),
    ("Vibrance", |p| &mut p.vibrance, -100.0, 100.0),
    ("Saturation", |p| &mut p.saturation, -100.0, 100.0),
    ("ParametricShadows", |p| &mut p.curve_shadows, -100.0, 100.0),
    ("ParametricDarks", |p| &mut p.curve_darks, -100.0, 100.0),
    ("ParametricLights", |p| &mut p.curve_lights, -100.0, 100.0),
    ("ParametricHighlights", |p| &mut p.curve_highlights, -100.0, 100.0),
    ("ParametricShadowSplit", |p| &mut p.curve_splits[0], 0.0, 100.0),
    ("ParametricMidtoneSplit", |p| &mut p.curve_splits[1], 0.0, 100.0),
    ("ParametricHighlightSplit", |p| &mut p.curve_splits[2], 0.0, 100.0),
    ("HueAdjustmentRed", |p| &mut p.hsl_hue[0], -100.0, 100.0),
    ("HueAdjustmentOrange", |p| &mut p.hsl_hue[1], -100.0, 100.0),
    ("HueAdjustmentYellow", |p| &mut p.hsl_hue[2], -100.0, 100.0),
    ("HueAdjustmentGreen", |p| &mut p.hsl_hue[3], -100.0, 100.0),
    ("HueAdjustmentAqua", |p| &mut p.hsl_hue[4], -100.0, 100.0),
    ("HueAdjustmentBlue", |p| &mut p.hsl_hue[5], -100.0, 100.0),
    ("HueAdjustmentPurple", |p| &mut p.hsl_hue[6], -100.0, 100.0),
    ("HueAdjustmentMagenta", |p| &mut p.hsl_hue[7], -100.0, 100.0),
    ("SaturationAdjustmentRed", |p| &mut p.hsl_sat[0], -100.0, 100.0),
    ("SaturationAdjustmentOrange", |p| &mut p.hsl_sat[1], -100.0, 100.0),
    ("SaturationAdjustmentYellow", |p| &mut p.hsl_sat[2], -100.0, 100.0),
    ("SaturationAdjustmentGreen", |p| &mut p.hsl_sat[3], -100.0, 100.0),
    ("SaturationAdjustmentAqua", |p| &mut p.hsl_sat[4], -100.0, 100.0),
    ("SaturationAdjustmentBlue", |p| &mut p.hsl_sat[5], -100.0, 100.0),
    ("SaturationAdjustmentPurple", |p| &mut p.hsl_sat[6], -100.0, 100.0),
    ("SaturationAdjustmentMagenta", |p| &mut p.hsl_sat[7], -100.0, 100.0),
    ("LuminanceAdjustmentRed", |p| &mut p.hsl_lum[0], -100.0, 100.0),
    ("LuminanceAdjustmentOrange", |p| &mut p.hsl_lum[1], -100.0, 100.0),
    ("LuminanceAdjustmentYellow", |p| &mut p.hsl_lum[2], -100.0, 100.0),
    ("LuminanceAdjustmentGreen", |p| &mut p.hsl_lum[3], -100.0, 100.0),
    ("LuminanceAdjustmentAqua", |p| &mut p.hsl_lum[4], -100.0, 100.0),
    ("LuminanceAdjustmentBlue", |p| &mut p.hsl_lum[5], -100.0, 100.0),
    ("LuminanceAdjustmentPurple", |p| &mut p.hsl_lum[6], -100.0, 100.0),
    ("LuminanceAdjustmentMagenta", |p| &mut p.hsl_lum[7], -100.0, 100.0),
    // Color Grading keeps the split-toning names for the shadow and highlight wheels' hue and
    // saturation, and the balance.
    ("SplitToningShadowHue", |p| &mut p.grade_shadows.hue, 0.0, 360.0),
    ("SplitToningShadowSaturation", |p| &mut p.grade_shadows.sat, 0.0, 100.0),
    ("ColorGradeShadowLum", |p| &mut p.grade_shadows.lum, -100.0, 100.0),
    ("ColorGradeMidtoneHue", |p| &mut p.grade_midtones.hue, 0.0, 360.0),
    ("ColorGradeMidtoneSat", |p| &mut p.grade_midtones.sat, 0.0, 100.0),
    ("ColorGradeMidtoneLum", |p| &mut p.grade_midtones.lum, -100.0, 100.0),
    ("SplitToningHighlightHue", |p| &mut p.grade_highlights.hue, 0.0, 360.0),
    ("SplitToningHighlightSaturation", |p| &mut p.grade_highlights.sat, 0.0, 100.0),
    ("ColorGradeHighlightLum", |p| &mut p.grade_highlights.lum, -100.0, 100.0),
    ("ColorGradeGlobalHue", |p| &mut p.grade_global.hue, 0.0, 360.0),
    ("ColorGradeGlobalSat", |p| &mut p.grade_global.sat, 0.0, 100.0),
    ("ColorGradeGlobalLum", |p| &mut p.grade_global.lum, -100.0, 100.0),
    ("ColorGradeBlending", |p| &mut p.grade_blending, 0.0, 100.0),
    ("SplitToningBalance", |p| &mut p.grade_balance, -100.0, 100.0),
    ("Sharpness", |p| &mut p.sharpen_amount, 0.0, 150.0),
    ("SharpenRadius", |p| &mut p.sharpen_radius, 0.5, 3.0),
    ("SharpenDetail", |p| &mut p.sharpen_detail, 0.0, 100.0),
    ("SharpenEdgeMasking", |p| &mut p.sharpen_masking, 0.0, 100.0),
    ("LuminanceSmoothing", |p| &mut p.noise_luminance, 0.0, 100.0),
    ("LuminanceNoiseReductionDetail", |p| &mut p.noise_luminance_detail, 0.0, 100.0),
    ("ColorNoiseReduction", |p| &mut p.noise_color, 0.0, 100.0),
    ("ColorNoiseReductionDetail", |p| &mut p.noise_color_detail, 0.0, 100.0),
    ("GrainAmount", |p| &mut p.grain_amount, 0.0, 100.0),
    ("GrainSize", |p| &mut p.grain_size, 0.0, 100.0),
    ("GrainFrequency", |p| &mut p.grain_roughness, 0.0, 100.0),
    ("PostCropVignetteAmount", |p| &mut p.vignette_amount, -100.0, 100.0),
    ("PostCropVignetteMidpoint", |p| &mut p.vignette_midpoint, 0.0, 100.0),
    ("PostCropVignetteRoundness", |p| &mut p.vignette_roundness, -100.0, 100.0),
    ("PostCropVignetteFeather", |p| &mut p.vignette_feather, 0.0, 100.0),
    ("PostCropVignetteHighlightContrast", |p| &mut p.vignette_highlights, 0.0, 100.0),
    ("CurveRefineSaturation", |p| &mut p.curve_refine_saturation, 0.0, 100.0),
    ("LuminanceNoiseReductionContrast", |p| &mut p.noise_luminance_contrast, 0.0, 100.0),
    ("ColorNoiseReductionSmoothness", |p| &mut p.noise_color_smoothness, 0.0, 100.0),
    ("LensManualDistortionAmount", |p| &mut p.lens_distortion, -100.0, 100.0),
    ("VignetteAmount", |p| &mut p.lens_vignette_amount, -100.0, 100.0),
    ("VignetteMidpoint", |p| &mut p.lens_vignette_midpoint, 0.0, 100.0),
    ("DefringePurpleAmount", |p| &mut p.defringe_purple_amount, 0.0, 20.0),
    ("DefringePurpleHueLo", |p| &mut p.defringe_purple_hue[0], 0.0, 100.0),
    ("DefringePurpleHueHi", |p| &mut p.defringe_purple_hue[1], 0.0, 100.0),
    ("DefringeGreenAmount", |p| &mut p.defringe_green_amount, 0.0, 20.0),
    ("DefringeGreenHueLo", |p| &mut p.defringe_green_hue[0], 0.0, 100.0),
    ("DefringeGreenHueHi", |p| &mut p.defringe_green_hue[1], 0.0, 100.0),
    ("ShadowTint", |p| &mut p.shadow_tint, -100.0, 100.0),
    ("RedHue", |p| &mut p.red_hue, -100.0, 100.0),
    ("RedSaturation", |p| &mut p.red_saturation, -100.0, 100.0),
    ("GreenHue", |p| &mut p.green_hue, -100.0, 100.0),
    ("GreenSaturation", |p| &mut p.green_saturation, -100.0, 100.0),
    ("BlueHue", |p| &mut p.blue_hue, -100.0, 100.0),
    ("BlueSaturation", |p| &mut p.blue_saturation, -100.0, 100.0),
    ("GrayMixerRed", |p| &mut p.gray_mix[0], -100.0, 100.0),
    ("GrayMixerOrange", |p| &mut p.gray_mix[1], -100.0, 100.0),
    ("GrayMixerYellow", |p| &mut p.gray_mix[2], -100.0, 100.0),
    ("GrayMixerGreen", |p| &mut p.gray_mix[3], -100.0, 100.0),
    ("GrayMixerAqua", |p| &mut p.gray_mix[4], -100.0, 100.0),
    ("GrayMixerBlue", |p| &mut p.gray_mix[5], -100.0, 100.0),
    ("GrayMixerPurple", |p| &mut p.gray_mix[6], -100.0, 100.0),
    ("GrayMixerMagenta", |p| &mut p.gray_mix[7], -100.0, 100.0),
];

/// The Black & White treatment (`ConvertToGrayscale`, `True`/`False`).
const MONOCHROME: &str = "ConvertToGrayscale";

/// Camera Raw's profiles for rendered images that change nothing here (the Black & White one sets
/// the treatment). Creative profiles carry colour tables PhotoCraft doesn't read yet.
const NEUTRAL_LOOKS: &[&str] = &["Adobe Color", "Adobe Standard", "Adobe Default", "Adobe Monochrome"];

type Curve = fn(&mut CameraRaw) -> &mut Vec<[f32; 2]>;
const CURVES: &[(&str, Curve)] = &[
    ("ToneCurvePV2012", |p| &mut p.point_curve),
    ("ToneCurvePV2012Red", |p| &mut p.point_curve_red),
    ("ToneCurvePV2012Green", |p| &mut p.point_curve_green),
    ("ToneCurvePV2012Blue", |p| &mut p.point_curve_blue),
];

const STYLES: &[(&str, &str)] = &[("1", "highlightPriority"), ("2", "colorPriority"), ("3", "paintOverlay")];

/// Bookkeeping fields Camera Raw writes that carry no setting.
const METADATA: &[&str] = &[
    "Version",
    "ProcessVersion",
    "HasSettings",
    "HasCrop",
    "WhiteBalance",
    "ToneCurveName2012",
    "CameraProfile",
    "PresetType",
    "UUID",
    "SupportsAmount",
    "SupportsColor",
    "SupportsMonochrome",
    "SupportsHighDynamicRange",
    "SupportsNormalDynamicRange",
    "SupportsSceneReferred",
    "SupportsOutputReferred",
    "CameraModelRestriction",
    "Copyright",
    "ContactInfo",
    "Name",
    "ShortName",
    "SortName",
    "Group",
    "Description",
    "Cluster",
    "SupportsAmount2",
    "RequiresRGBTables",
    "ShowInPresets",
    "ShowInQuickActions",
    "CameraProfileDigest",
    "LensProfileSetup",
    "Stubbed",
];

/// What [`load`] applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadReport {
    /// How many settings were applied.
    pub applied: usize,
    /// `crs:` settings without a PhotoCraft control (set to something other than their neutral
    /// value), or with an unusable value, in file order.
    pub ignored: Vec<String>,
    /// The file asks for Auto white balance (`WhiteBalance="Auto"`), which depends on the image:
    /// the caller measures it.
    pub auto_white_balance: bool,
}

fn number(s: &str) -> Option<f32> {
    let s = s.trim();
    let v: f32 = s.strip_prefix('+').unwrap_or(s).parse().ok()?;
    v.is_finite().then_some(v)
}

/// `"0, 0"` items of an `rdf:Seq` → curve points.
fn curve(node: roxmltree::Node<'_, '_>) -> Option<Vec<[f32; 2]>> {
    let seq = node.children().find(|c| c.is_element() && c.tag_name().namespace() == Some(RDF_NS) && c.tag_name().name() == "Seq")?;
    let mut points = Vec::new();
    for li in seq.children().filter(|c| c.is_element() && c.tag_name().name() == "li") {
        let text = li.text().unwrap_or("");
        let (x, y) = text.split_once(',')?;
        points.push([number(x)?, number(y)?]);
    }
    validate_curve(&points).ok()?;
    // Camera Raw's linear curve is the identity's two end points; PhotoCraft's is empty.
    if points == [[0.0, 0.0], [255.0, 255.0]] {
        points.clear();
    }
    Some(points)
}

/// An unsupported setting at its neutral value changes nothing, so it isn't reported: zero,
/// `False`, and the few neutral values that aren't zero.
fn neutral(name: &str, value: &str) -> bool {
    let v = value.trim();
    if v.eq_ignore_ascii_case("false") || number(v) == Some(0.0) {
        return true;
    }
    match name {
        "GrainSeed" => true,
        "VignetteMidpoint" => number(v) == Some(50.0),
        "PerspectiveScale" => number(v) == Some(100.0),
        _ => name.starts_with("Defringe") && name.contains("Hue"),
    }
}

fn apply(params: &mut CameraRaw, report: &mut LoadReport, name: &str, value: &str) {
    if name == MONOCHROME {
        match value.trim() {
            v if v.eq_ignore_ascii_case("true") => params.monochrome = true,
            v if v.eq_ignore_ascii_case("false") => params.monochrome = false,
            _ => return report.ignored.push(name.to_string()),
        }
        report.applied += 1;
    } else if name == "WhiteBalance" {
        report.auto_white_balance = value.trim() == "Auto";
    } else if let Some((_, field, lo, hi)) = SCALARS.iter().find(|(n, ..)| *n == name) {
        match number(value) {
            Some(v) => {
                *field(params) = v.clamp(*lo, *hi);
                report.applied += 1;
            }
            None => report.ignored.push(name.to_string()),
        }
    } else if name == "PostCropVignetteStyle" {
        match STYLES.iter().find(|(n, _)| *n == value.trim()) {
            Some((_, style)) => {
                params.vignette_style = (*style).into();
                report.applied += 1;
            }
            None => report.ignored.push(name.to_string()),
        }
    } else if !METADATA.contains(&name) && !neutral(name, value) {
        report.ignored.push(name.to_string());
    }
}

/// Apply a Camera Raw settings file over `params`. Fails on input that isn't XMP or holds no
/// Camera Raw settings; never panics on malformed input.
pub fn load(params: &mut CameraRaw, bytes: &[u8]) -> Result<LoadReport, String> {
    if bytes.len() > MAX_XMP_BYTES {
        return Err("the settings file is too large".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "not a Camera Raw settings file (not UTF-8 text)".to_string())?;
    // Some writers put a byte order mark or an `<?xpacket?>` wrapper first; roxmltree takes both.
    let text = text.trim_start_matches('\u{feff}');
    let doc = roxmltree::Document::parse(text).map_err(|e| format!("not a Camera Raw settings file ({e})"))?;
    let mut next = params.clone();
    let mut report = LoadReport::default();
    let mut found = false;
    for desc in doc.descendants().filter(|n| n.is_element() && n.tag_name().namespace() == Some(RDF_NS) && n.tag_name().name() == "Description") {
        // A structured setting's own description (a profile's, a mask's) belongs to that setting.
        if desc.ancestors().skip(1).any(|a| a.tag_name().namespace() == Some(CRS_NS)) {
            continue;
        }
        for a in desc.attributes().filter(|a| a.namespace() == Some(CRS_NS)) {
            found = true;
            apply(&mut next, &mut report, a.name(), a.value());
        }
        for c in desc.children().filter(|c| c.is_element() && c.tag_name().namespace() == Some(CRS_NS)) {
            found = true;
            let name = c.tag_name().name();
            if let Some((_, field)) = CURVES.iter().find(|(n, _)| *n == name) {
                match curve(c) {
                    Some(points) => {
                        *field(&mut next) = points;
                        report.applied += 1;
                    }
                    None => report.ignored.push(name.to_string()),
                }
            } else if name == "Look" {
                // A profile: the neutral ones for rendered images are nothing to apply.
                match look_name(c) {
                    Some(n) if n == "Adobe Monochrome" => {
                        next.monochrome = true;
                        report.applied += 1;
                    }
                    Some(n) if NEUTRAL_LOOKS.contains(&n.as_str()) => {}
                    _ => report.ignored.push(name.to_string()),
                }
            } else if c.children().any(|g| g.is_element()) {
                // Structured settings (masks, lens profiles, Point Color…) have no counterpart.
                if !METADATA.contains(&name) {
                    report.ignored.push(name.to_string());
                }
            } else {
                apply(&mut next, &mut report, name, c.text().unwrap_or(""));
            }
        }
    }
    if !found {
        return Err("no Camera Raw settings in the file".into());
    }
    *params = next;
    Ok(report)
}

/// The `crs:Name` of a `crs:Look` element (an attribute of its description, or a child).
fn look_name(look: roxmltree::Node<'_, '_>) -> Option<String> {
    look.descendants().filter(|n| n.is_element()).find_map(|n| {
        n.attributes().find(|a| a.namespace() == Some(CRS_NS) && a.name() == "Name").map(|a| a.value().to_string()).or_else(|| {
            (n.tag_name().namespace() == Some(CRS_NS) && n.tag_name().name() == "Name").then(|| alt_text(n).or_else(|| n.text().map(str::to_string))).flatten()
        })
    })
}

/// The default text of an `rdf:Alt` language alternative under `node` (`xml:lang="x-default"`,
/// else the first).
fn alt_text(node: roxmltree::Node<'_, '_>) -> Option<String> {
    let items: Vec<_> = node.descendants().filter(|n| n.is_element() && n.tag_name().namespace() == Some(RDF_NS) && n.tag_name().name() == "li").collect();
    let default = items.iter().find(|li| li.attributes().any(|a| a.name() == "lang" && a.value() == "x-default"));
    default.or(items.first()).map(|li| li.text().unwrap_or("").trim().to_string())
}

/// What a preset file says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PresetInfo {
    /// `crs:Name` (empty when the file has none: the caller names it after the file).
    pub name: String,
    /// `crs:Group`, the folder Camera Raw lists it under (empty: none).
    pub group: String,
    /// `crs:UUID`.
    pub uuid: String,
    /// `crs:PresetType`: `Normal` for presets, `Look` for profiles.
    pub kind: String,
    /// `crs:SupportsAmount`.
    pub supports_amount: bool,
}

/// The name, group and kind of a Camera Raw preset file. Fails like [`load`] on input that isn't
/// XMP or holds no Camera Raw settings.
pub fn preset_info(bytes: &[u8]) -> Result<PresetInfo, String> {
    if bytes.len() > MAX_XMP_BYTES {
        return Err("the preset file is too large".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "not a Camera Raw preset (not UTF-8 text)".to_string())?;
    let doc = roxmltree::Document::parse(text.trim_start_matches('\u{feff}')).map_err(|e| format!("not a Camera Raw preset ({e})"))?;
    let mut info = PresetInfo::default();
    let mut found = false;
    for desc in doc.descendants().filter(|n| n.is_element() && n.tag_name().namespace() == Some(RDF_NS) && n.tag_name().name() == "Description") {
        // A Look's own description (nested in the preset's) names the profile, not the preset.
        if desc.ancestors().skip(1).any(|a| a.tag_name().namespace() == Some(CRS_NS)) {
            continue;
        }
        for a in desc.attributes().filter(|a| a.namespace() == Some(CRS_NS)) {
            found = true;
            match a.name() {
                "Name" => info.name = a.value().to_string(),
                "Group" => info.group = a.value().to_string(),
                "UUID" => info.uuid = a.value().to_string(),
                "PresetType" => info.kind = a.value().to_string(),
                "SupportsAmount" => info.supports_amount = a.value().eq_ignore_ascii_case("true"),
                _ => {}
            }
        }
        for c in desc.children().filter(|c| c.is_element() && c.tag_name().namespace() == Some(CRS_NS)) {
            found = true;
            match c.tag_name().name() {
                "Name" => info.name = alt_text(c).unwrap_or_default(),
                "Group" => info.group = alt_text(c).unwrap_or_default(),
                _ => {}
            }
        }
    }
    if !found {
        return Err("no Camera Raw settings in the file".into());
    }
    Ok(info)
}

/// The groups of settings a preset can hold, as Camera Raw's Create Preset dialog lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    WhiteBalance,
    Light,
    Presence,
    Curve,
    ColorMixer,
    ColorGrading,
    Detail,
    Effects,
    Optics,
    Calibration,
    Treatment,
}

impl Section {
    pub const ALL: [Section; 11] = [
        Section::Treatment,
        Section::WhiteBalance,
        Section::Light,
        Section::Presence,
        Section::Curve,
        Section::ColorMixer,
        Section::ColorGrading,
        Section::Detail,
        Section::Effects,
        Section::Optics,
        Section::Calibration,
    ];
    /// Its id in control-channel requests.
    pub fn id(self) -> &'static str {
        match self {
            Section::WhiteBalance => "whiteBalance",
            Section::Light => "light",
            Section::Presence => "presence",
            Section::Curve => "curve",
            Section::ColorMixer => "colorMixer",
            Section::ColorGrading => "colorGrading",
            Section::Detail => "detail",
            Section::Effects => "effects",
            Section::Optics => "optics",
            Section::Calibration => "calibration",
            Section::Treatment => "treatment",
        }
    }
    pub fn from_id(id: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|s| s.id() == id)
    }
    /// The label Camera Raw gives it.
    pub fn label(self) -> &'static str {
        match self {
            Section::WhiteBalance => "White Balance",
            Section::Light => "Light",
            Section::Presence => "Presence",
            Section::Curve => "Curve",
            Section::ColorMixer => "Color Mixer",
            Section::ColorGrading => "Color Grading",
            Section::Detail => "Detail",
            Section::Effects => "Effects",
            Section::Optics => "Optics",
            Section::Calibration => "Calibration",
            Section::Treatment => "Treatment & Profile",
        }
    }
    /// The section a `crs:` setting belongs to.
    pub fn of(name: &str) -> Option<Section> {
        Some(match name {
            "IncrementalTemperature" | "IncrementalTint" | "WhiteBalance" => Section::WhiteBalance,
            "Exposure2012" | "Contrast2012" | "Highlights2012" | "Shadows2012" | "Whites2012" | "Blacks2012" => Section::Light,
            "Texture" | "Clarity2012" | "Dehaze" | "Vibrance" | "Saturation" => Section::Presence,
            "ConvertToGrayscale" => Section::Treatment,
            n if n.starts_with("Parametric") || n.starts_with("ToneCurve") || n == "CurveRefineSaturation" => Section::Curve,
            n if n.starts_with("HueAdjustment") || n.starts_with("SaturationAdjustment") || n.starts_with("LuminanceAdjustment") || n.starts_with("GrayMixer") => {
                Section::ColorMixer
            }
            n if n.starts_with("SplitToning") || n.starts_with("ColorGrade") => Section::ColorGrading,
            n if n.starts_with("Sharpen") || n == "Sharpness" || n.contains("NoiseReduction") || n == "LuminanceSmoothing" => Section::Detail,
            n if n.starts_with("PostCropVignette") || n.starts_with("Grain") => Section::Effects,
            n if n.starts_with("Defringe") || n.starts_with("Lens") || n.starts_with("Vignette") => Section::Optics,
            "ShadowTint" | "RedHue" | "RedSaturation" | "GreenHue" | "GreenSaturation" | "BlueHue" | "BlueSaturation" => Section::Calibration,
            _ => return None,
        })
    }
}

/// A Camera Raw preset file named `name` in `group` holding the settings of `params` in the
/// chosen `sections` (and only those, so applying it changes only those), laid out as Camera Raw
/// writes presets: it can be copied into Photoshop's Camera Raw Settings folder.
pub fn save_preset(params: &CameraRaw, name: &str, group: &str, uuid: &str, sections: &[Section]) -> String {
    let mut p = params.clone();
    let has = |n: &str| Section::of(n).is_some_and(|s| sections.contains(&s));
    let mut out = String::from(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"PhotoCraft\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\"\n    xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"\n",
    );
    out.push_str(&format!("   crs:PresetType=\"Normal\"\n   crs:Cluster=\"\"\n   crs:UUID=\"{}\"\n", escape(uuid)));
    out.push_str("   crs:SupportsAmount=\"True\"\n   crs:SupportsColor=\"True\"\n   crs:SupportsMonochrome=\"True\"\n");
    out.push_str("   crs:SupportsHighDynamicRange=\"True\"\n   crs:SupportsNormalDynamicRange=\"True\"\n");
    out.push_str("   crs:SupportsSceneReferred=\"True\"\n   crs:SupportsOutputReferred=\"True\"\n");
    out.push_str("   crs:CameraModelRestriction=\"\"\n   crs:Copyright=\"\"\n   crs:ContactInfo=\"\"\n");
    out.push_str("   crs:Version=\"18.4\"\n   crs:ProcessVersion=\"15.4\"\n");
    if has("WhiteBalance") {
        let wb = if p.temperature != 0.0 || p.tint != 0.0 { "Custom" } else { "As Shot" };
        out.push_str(&format!("   crs:WhiteBalance=\"{wb}\"\n"));
    }
    for (name, field, lo, _) in SCALARS {
        if !has(name) {
            continue;
        }
        out.push_str(&format!("   crs:{name}=\"{}\"\n", scalar_text(name, *field(&mut p), *lo)));
    }
    if has("PostCropVignetteStyle") {
        let style = STYLES.iter().find(|(_, s)| *s == p.vignette_style).map_or("1", |(n, _)| n);
        out.push_str(&format!("   crs:PostCropVignetteStyle=\"{}\"\n", escape(style)));
    }
    if has(MONOCHROME) {
        out.push_str(&format!("   crs:{MONOCHROME}=\"{}\"\n", if p.monochrome { "True" } else { "False" }));
    }
    if sections.contains(&Section::Curve) {
        out.push_str("   crs:ToneCurveName2012=\"Custom\"\n");
    }
    out.push_str("   crs:HasSettings=\"True\">\n");
    for (tag, text) in [("Name", name), ("ShortName", ""), ("SortName", ""), ("Group", group), ("Description", "")] {
        out.push_str(&format!("   <crs:{tag}>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </crs:{tag}>\n", escape(text)));
    }
    if sections.contains(&Section::Curve) {
        write_curves(&mut out, &mut p);
    }
    out.push_str("  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n");
    out
}

/// A setting's value as Camera Raw writes it.
fn scalar_text(name: &str, v: f32, lo: f32) -> String {
    // Exposure has two decimals in Camera Raw, the sharpening radius one; the rest are levels.
    match name {
        "Exposure2012" => signed(v, 2),
        "SharpenRadius" => signed(v, 1),
        _ if lo < 0.0 => signed(v.round(), 0),
        _ => format!("{:.0}", v.round()),
    }
}

fn write_curves(out: &mut String, p: &mut CameraRaw) {
    for (name, field) in CURVES {
        let points = field(p);
        let points = if points.is_empty() { vec![[0.0, 0.0], [255.0, 255.0]] } else { points.clone() };
        out.push_str(&format!("   <crs:{name}>\n    <rdf:Seq>\n"));
        for [x, y] in points {
            out.push_str(&format!("     <rdf:li>{}, {}</rdf:li>\n", x.round(), y.round()));
        }
        out.push_str(&format!("    </rdf:Seq>\n   </crs:{name}>\n"));
    }
}

/// `base` moved toward `target` by `amount` (0 → `base`, 1 → `target`, up to 2 past it): the
/// Amount of an applied preset. Numbers (and lists of them of equal length, curves included)
/// interpolate; anything else switches at half way. Values stay within the sliders' ranges, and a
/// curve the mix would break is the target's.
pub fn blend(base: &CameraRaw, target: &CameraRaw, amount: f32) -> CameraRaw {
    fn mix(b: &serde_json::Value, t: &serde_json::Value, a: f64) -> serde_json::Value {
        use serde_json::Value as J;
        match (b, t) {
            (J::Number(x), J::Number(y)) => match (x.as_f64(), y.as_f64()) {
                (Some(xf), Some(yf)) if xf != yf => {
                    let v = xf + (yf - xf) * a;
                    // Whole-number settings (the grain seed) stay whole.
                    if y.is_u64() { serde_json::json!(v.round().max(0.0) as u64) } else if y.is_i64() { serde_json::json!(v.round() as i64) } else { serde_json::json!(v) }
                }
                _ => t.clone(),
            },
            (J::Array(x), J::Array(y)) if x.len() == y.len() => J::Array(x.iter().zip(y).map(|(x, y)| mix(x, y, a)).collect()),
            (J::Object(x), J::Object(y)) => J::Object(y.iter().map(|(k, v)| (k.clone(), x.get(k).map_or_else(|| v.clone(), |bv| mix(bv, v, a)))).collect()),
            _ => if a >= 0.5 { t.clone() } else { b.clone() },
        }
    }
    let amount = if amount.is_finite() { amount.clamp(0.0, 2.0) } else { 1.0 };
    let (Ok(b), Ok(t)) = (serde_json::to_value(base), serde_json::to_value(target)) else { return target.clone() };
    let mut out: CameraRaw = serde_json::from_value(mix(&b, &t, f64::from(amount))).unwrap_or_else(|_| target.clone());
    for (_, field, lo, hi) in SCALARS {
        let v = field(&mut out);
        *v = v.clamp(*lo, *hi);
    }
    let mut fallback = target.clone();
    for (_, field) in CURVES {
        if validate_curve(field(&mut out)).is_err() {
            *field(&mut out) = field(&mut fallback).clone();
        }
    }
    out
}

fn signed(v: f32, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if v > 0.0 && !s.starts_with('+') { format!("+{s}") } else { s }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Write `params` as a Camera Raw settings file Photoshop's Load Settings… reads.
pub fn save(params: &CameraRaw) -> String {
    let mut p = params.clone();
    let wb = if p.temperature != 0.0 || p.tint != 0.0 { "Custom" } else { "As Shot" };
    let mut out = String::from(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"PhotoCraft\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\"\n    xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"\n",
    );
    out.push_str("   crs:Version=\"18.4\"\n   crs:ProcessVersion=\"15.4\"\n");
    out.push_str(&format!("   crs:WhiteBalance=\"{wb}\"\n"));
    for (name, field, lo, _) in SCALARS {
        out.push_str(&format!("   crs:{name}=\"{}\"\n", scalar_text(name, *field(&mut p), *lo)));
    }
    let style = STYLES.iter().find(|(_, s)| *s == p.vignette_style).map_or("1", |(n, _)| n);
    out.push_str(&format!("   crs:PostCropVignetteStyle=\"{}\"\n", escape(style)));
    out.push_str(&format!("   crs:{MONOCHROME}=\"{}\"\n", if p.monochrome { "True" } else { "False" }));
    out.push_str("   crs:ToneCurveName2012=\"Custom\"\n   crs:HasSettings=\"True\">\n");
    write_curves(&mut out, &mut p);
    out.push_str("  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n");
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A hand-written settings file laid out as Photoshop 26's Camera Raw Filter saves one (the
    /// field set and order follow Camera Raw 18; the values are synthetic), plus element-form
    /// settings, which XMP allows and some writers use.
    const FIXTURE: &[u8] = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 7.0-c000 1.000000, 0000/00/00-00:00:00        ">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   crs:Version="18.1.1" crs:ProcessVersion="15.4" crs:WhiteBalance="Custom"
   crs:IncrementalTemperature="0" crs:IncrementalTint="+10" crs:Exposure2012="+0.05"
   crs:Contrast2012="-5" crs:Highlights2012="-20" crs:Shadows2012="+10" crs:Whites2012="+35"
   crs:Blacks2012="0" crs:Texture="+30" crs:Clarity2012="+5" crs:Vibrance="+20" crs:Saturation="-5"
   crs:ParametricShadowSplit="25" crs:ParametricMidtoneSplit="50" crs:ParametricHighlightSplit="75"
   crs:Sharpness="15" crs:SharpenRadius="+1.0" crs:LuminanceSmoothing="5"
   crs:LuminanceNoiseReductionContrast="0" crs:HueAdjustmentGreen="+25" crs:SaturationAdjustmentGreen="+20"
   crs:ColorGradeBlending="50" crs:LensProfileEnable="0" crs:VignetteAmount="-20"
   crs:PerspectiveScale="100" crs:GrainAmount="10" crs:RedHue="-5" crs:GrainSeed="2834497962"
   crs:ToneCurveName2012="Linear" crs:CameraProfile="Default Color" crs:HasSettings="True" crs:HasCrop="False">
   <crs:Dehaze>+12</crs:Dehaze>
   <crs:ToneCurvePV2012>
    <rdf:Seq>
     <rdf:li>0, 0</rdf:li>
     <rdf:li>255, 255</rdf:li>
    </rdf:Seq>
   </crs:ToneCurvePV2012>
   <crs:ToneCurvePV2012Red>
    <rdf:Seq>
     <rdf:li>0, 0</rdf:li>
     <rdf:li>128, 140</rdf:li>
     <rdf:li>255, 255</rdf:li>
    </rdf:Seq>
   </crs:ToneCurvePV2012Red>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    #[test]
    fn reads_a_photoshop_preset() {
        let mut p = CameraRaw::default();
        let r = load(&mut p, FIXTURE).unwrap();
        assert_eq!(p.tint, 10.0);
        assert_eq!(p.exposure, 0.05);
        assert_eq!((p.contrast, p.highlights, p.shadows, p.whites), (-5.0, -20.0, 10.0, 35.0));
        assert_eq!((p.texture, p.clarity, p.vibrance, p.saturation), (30.0, 5.0, 20.0, -5.0));
        assert_eq!((p.hsl_hue[3], p.hsl_sat[3]), (25.0, 20.0));
        assert_eq!((p.sharpen_amount, p.noise_luminance, p.grain_amount), (15.0, 5.0, 10.0));
        assert_eq!(p.grade_blending, 50.0);
        // The linear tone curve is PhotoCraft's empty one; an element-form setting is read too.
        assert!(p.point_curve.is_empty());
        assert_eq!(p.point_curve_red, vec![[0.0, 0.0], [128.0, 140.0], [255.0, 255.0]]);
        assert_eq!(p.dehaze, 12.0);
        // Lens vignetting and calibration are read now.
        assert_eq!((p.lens_vignette_amount, p.red_hue), (-20.0, -5.0));
        assert!(r.ignored.is_empty(), "{:?}", r.ignored);
    }

    #[test]
    fn save_then_load_round_trips() {
        let mut p = CameraRaw {
            temperature: -12.0,
            exposure: 1.25,
            contrast: 30.0,
            sharpen_radius: 1.4,
            vignette_amount: -40.0,
            vignette_style: "paintOverlay".into(),
            curve_splits: [20.0, 45.0, 80.0],
            point_curve: vec![[0.0, 10.0], [128.0, 150.0], [255.0, 240.0]],
            ..CameraRaw::default()
        };
        p.hsl_lum[5] = -33.0;
        p.grade_shadows.hue = 210.0;
        p.grade_shadows.sat = 40.0;
        p.grade_global.lum = 7.0;
        let text = save(&p);
        assert!(text.contains("crs:Exposure2012=\"+1.25\""));
        assert!(text.contains("crs:WhiteBalance=\"Custom\""));
        let mut back = CameraRaw::default();
        let r = load(&mut back, text.as_bytes()).unwrap();
        assert!(r.ignored.is_empty(), "{:?}", r.ignored);
        assert_eq!(back, p);
        // The newer controls round-trip too.
        let mut q = CameraRaw { monochrome: true, shadow_tint: 12.0, red_hue: -30.0, blue_saturation: 44.0, lens_distortion: 20.0, ..CameraRaw::default() };
        q.gray_mix[3] = -40.0;
        q.defringe_purple_amount = 7.0;
        q.curve_refine_saturation = 60.0;
        q.noise_color_smoothness = 80.0;
        let mut back = CameraRaw::default();
        load(&mut back, save(&q).as_bytes()).unwrap();
        assert_eq!(back, q);
    }

    /// A preset as Camera Raw keeps one in its Settings folder (the layout of Camera Raw 13's;
    /// synthetic values).
    const PRESET: &[u8] = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 5.6-c140">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   crs:PresetType="Normal" crs:Cluster="" crs:UUID="8A0F6CAA22291B0FE0578E335AB19FEF" crs:SupportsAmount="False"
   crs:Version="13.1" crs:ProcessVersion="11.0" crs:WhiteBalance="Auto" crs:Contrast2012="+20" crs:RedHue="+19"
   crs:GrainFrequency="85" crs:ConvertToGrayscale="False" crs:CameraProfile="Adobe Standard" crs:HasSettings="True">
   <crs:Name><rdf:Alt><rdf:li xml:lang="x-default">02 VSCO</rdf:li></rdf:Alt></crs:Name>
   <crs:Group><rdf:Alt><rdf:li xml:lang="x-default">installed</rdf:li></rdf:Alt></crs:Group>
   <crs:Look><rdf:Description crs:Name="Adobe Color" crs:Amount="1"><crs:Group><rdf:Alt><rdf:li xml:lang="x-default">Profiles</rdf:li></rdf:Alt></crs:Group></rdf:Description></crs:Look>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    #[test]
    fn presets_name_themselves_and_apply_their_settings() {
        let info = preset_info(PRESET).unwrap();
        assert_eq!(info, PresetInfo { name: "02 VSCO".into(), group: "installed".into(), uuid: "8A0F6CAA22291B0FE0578E335AB19FEF".into(), kind: "Normal".into(), supports_amount: false });
        let mut p = CameraRaw::default();
        let r = load(&mut p, PRESET).unwrap();
        assert!(r.ignored.is_empty(), "the neutral profile is nothing to report: {:?}", r.ignored);
        assert!(r.auto_white_balance);
        assert_eq!((p.contrast, p.red_hue, p.grain_roughness), (20.0, 19.0, 85.0));
        // A creative profile is reported, the black-and-white one sets the treatment.
        let creative = String::from_utf8_lossy(PRESET).replace("Adobe Color", "Modern 10");
        assert_eq!(load(&mut CameraRaw::default(), creative.as_bytes()).unwrap().ignored, vec!["Look".to_string()]);
        let mono = String::from_utf8_lossy(PRESET).replace("Adobe Color", "Adobe Monochrome");
        let mut m = CameraRaw::default();
        load(&mut m, mono.as_bytes()).unwrap();
        assert!(m.monochrome);
        assert!(preset_info(b"<x/>").is_err() && preset_info(b"\xff").is_err());
    }

    #[test]
    fn a_saved_preset_holds_only_its_sections() {
        let mut p = CameraRaw { exposure: 0.5, contrast: 25.0, temperature: 10.0, clarity: 30.0, red_hue: 20.0, ..CameraRaw::default() };
        p.point_curve = vec![[0.0, 10.0], [255.0, 250.0]];
        let text = save_preset(&p, "Warm & <crisp>", "Mine", "ABC", &[Section::Light, Section::Curve]);
        let info = preset_info(text.as_bytes()).unwrap();
        assert_eq!((info.name.as_str(), info.group.as_str(), info.kind.as_str()), ("Warm & <crisp>", "Mine", "Normal"));
        let mut applied = CameraRaw { clarity: -10.0, ..CameraRaw::default() };
        let r = load(&mut applied, text.as_bytes()).unwrap();
        assert!(r.ignored.is_empty(), "{:?}", r.ignored);
        assert_eq!((applied.exposure, applied.contrast, applied.point_curve.clone()), (0.5, 25.0, p.point_curve.clone()));
        assert_eq!((applied.temperature, applied.clarity, applied.red_hue), (0.0, -10.0, 0.0), "other sections stay as they were");
        for s in Section::ALL {
            assert!(!s.label().is_empty());
            assert_eq!(Section::from_id(s.id()), Some(s));
        }
        assert_eq!(Section::of("GrayMixerRed"), Some(Section::ColorMixer));
        assert_eq!(Section::of("Unknown"), None);
    }

    #[test]
    fn blending_interpolates_and_stays_in_range() {
        let base = CameraRaw { exposure: 0.0, contrast: 0.0, monochrome: false, ..CameraRaw::default() };
        let target = CameraRaw { exposure: 3.0, contrast: 80.0, monochrome: true, point_curve: vec![[0.0, 40.0], [255.0, 255.0]], ..CameraRaw::default() };
        let half = blend(&base, &target, 0.5);
        assert_eq!((half.exposure, half.contrast, half.monochrome), (1.5, 40.0, true));
        let none = blend(&base, &target, 0.0);
        assert_eq!((none.exposure, none.monochrome), (0.0, false));
        let double = blend(&base, &target, 2.0);
        assert_eq!((double.exposure, double.contrast), (5.0, 100.0), "clamped to the sliders' ranges");
        assert_eq!(blend(&base, &target, 1.0), target);
        assert_eq!(blend(&base, &target, f32::NAN), target);
        // Curves of different lengths switch; equal lengths interpolate.
        assert!(blend(&base, &target, 0.25).point_curve.is_empty());
        let curved = CameraRaw { point_curve: vec![[0.0, 0.0], [255.0, 255.0]], ..base.clone() };
        assert_eq!(blend(&curved, &target, 0.5).point_curve, vec![[0.0, 20.0], [255.0, 255.0]]);
    }

    #[test]
    fn load_applies_only_the_settings_in_the_file() {
        let mut p = CameraRaw { contrast: 40.0, ..CameraRaw::default() };
        let xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><crs:Exposure2012>-9</crs:Exposure2012>
            <crs:Clarity2012>abc</crs:Clarity2012></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let r = load(&mut p, xmp).unwrap();
        assert_eq!((p.exposure, p.contrast, p.clarity), (-5.0, 40.0, 0.0));
        assert_eq!(r, LoadReport { applied: 1, ignored: vec!["Clarity2012".into()], auto_white_balance: false });
    }

    #[test]
    fn malformed_input_is_an_error_and_leaves_settings_alone() {
        let before = CameraRaw { exposure: 0.5, ..CameraRaw::default() };
        for bad in [
            &b""[..],
            b"\xff\xfe\x00garbage",
            b"<x:xmpmeta><unclosed",
            b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>",
            b"<?xml version=\"1.0\"?><!DOCTYPE a [<!ENTITY e \"x\">]><a>&e;</a>",
        ] {
            let mut p = before.clone();
            assert!(load(&mut p, bad).is_err());
            assert_eq!(p, before);
        }
        // A broken curve or NaN is skipped, not fatal.
        let xmp = br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description
            xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Contrast2012="NaN"><crs:ToneCurvePV2012><rdf:Seq>
            <rdf:li>10</rdf:li></rdf:Seq></crs:ToneCurvePV2012></rdf:Description></rdf:RDF>"#;
        let mut p = before.clone();
        let r = load(&mut p, xmp).unwrap();
        assert_eq!(p, before);
        assert_eq!(r.ignored, vec!["Contrast2012".to_string(), "ToneCurvePV2012".to_string()]);
    }
}
