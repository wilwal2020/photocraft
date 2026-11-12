//! Exports brush presets as a Photoshop `.abr` (v6) file that PhotoCraft's Brushes panel (and
//! Photoshop) can import, so a preset can be tried without rebuilding the app.
//!
//! `cargo run --release -p photocraft-io --example export_brushes_abr -- --out <file.abr>
//!   "<built-in preset>[=<exported name>][|<JSON patch>]" …` (the patch is merged into the
//! preset's settings, for a variant: `"Bristle Highlight=Bristle Highlight Opaque|{\"wetEdges\":false}"`).
//!
//! Sampled tips and texture patterns (procedural ones are rendered) go in at 16 bits, the
//! settings as the `brushPreset` descriptors [`photocraft_io::abr_map`] reads. The file is read
//! back with that importer and every preset must come back with the same settings.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use photocraft_color::BlendMode;
use photocraft_paint::BrushSettings;
use photocraft_paint::brush::{Control, Dynamic, MaskMode, Pattern, TipShape};
use photocraft_paint::tile::GrayTile;
use photocraft_psd::abr::{AbrSample, write_v6};
use photocraft_psd::descriptor::{Descriptor, Id, UnicodeString, Value};
use photocraft_psd::patterns::PsdPattern;

fn prc(v: f32) -> Value {
    Value::UnitFloat { unit: *b"#Prc", value: f64::from(v) * 100.0 }
}
fn px(v: f32) -> Value {
    Value::UnitFloat { unit: *b"#Pxl", value: f64::from(v) }
}
fn ang(v: f32) -> Value {
    Value::UnitFloat { unit: *b"#Ang", value: f64::from(v) }
}
fn text(s: &str) -> Value {
    Value::Text(UnicodeString::new_nul(s))
}
fn boolean(b: bool) -> Value {
    Value::Boolean(b)
}
fn enumerated(type_id: &str, value: &str) -> Value {
    Value::Enumerated { type_id: Id::new(type_id), value: Id::new(value) }
}

fn control_code(c: Control) -> i32 {
    match c {
        Control::Off => 0,
        Control::Fade => 1,
        Control::PenPressure => 2,
        Control::PenTilt => 3,
        Control::StylusWheel => 4,
        Control::InitialDirection => 5,
        Control::Direction => 6,
        Control::Rotation => 8,
    }
}

fn var(d: &Dynamic) -> Value {
    assert!(d.curve == Dynamic::default().curve, "per-dynamic response curves are not exported");
    Value::Descriptor(
        Descriptor::new("brVr")
            .with("bVTy", Value::Integer(control_code(d.control)))
            .with("fStp", Value::Integer(d.fade_steps as i32))
            .with("jitter", prc(d.jitter))
            .with("Mnm ", prc(d.minimum)),
    )
}

fn mask_code(m: MaskMode) -> &'static str {
    match m {
        MaskMode::Multiply => "Mltp",
        MaskMode::Subtract => "Sbtr",
        MaskMode::Darken => "Drkn",
        MaskMode::Overlay => "Ovrl",
        MaskMode::ColorDodge => "CDdg",
        MaskMode::ColorBurn => "CBrn",
        MaskMode::LinearBurn => "lbrn",
        MaskMode::HardMix => "hMix",
        MaskMode::LinearHeight => "lHgt",
        MaskMode::Height => "hght",
    }
}

fn blend_code(m: BlendMode) -> &'static str {
    match m {
        BlendMode::Normal => "Nrml",
        BlendMode::Multiply => "Mltp",
        BlendMode::Screen => "Scrn",
        BlendMode::Overlay => "Ovrl",
        other => panic!("blend mode {other:?} is not exported"),
    }
}

fn u16_be(t: &GrayTile) -> Vec<u8> {
    t.data.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// A preset's descriptor, adding its tip to `samples` and its texture to `patterns`.
fn descriptor(name: &str, b: &BrushSettings, samples: &mut Vec<AbrSample>, patterns: &mut Vec<PsdPattern>) -> Descriptor {
    let mut tip = match &b.tip {
        TipShape::Sampled(t) => {
            let id = format!("$pc-export-tip-{}", samples.len() + 1);
            samples.push(AbrSample { id: id.clone(), width: t.width, height: t.height, depth: 16, data: u16_be(t) });
            Descriptor::new("sampledBrush").with("sampledData", text(&id))
        }
        TipShape::Round => Descriptor::new("computedBrush").with("Hrdn", prc(b.hardness)),
        other => panic!("tip {other:?} is not exported"),
    };
    tip = tip
        .with("Dmtr", px(b.size))
        .with("Angl", ang(b.angle))
        .with("Rndn", prc(b.roundness))
        .with("Spcn", prc(b.spacing))
        .with("Intr", boolean(b.spacing_enabled))
        .with("flipX", boolean(b.flip_x))
        .with("flipY", boolean(b.flip_y));
    let mut d = Descriptor::new("brushPreset").with("Nm  ", text(name)).with("Brsh", Value::Descriptor(tip));
    let sd = &b.shape_dynamics;
    if sd.enabled {
        d = d
            .with("useTipDynamics", boolean(true))
            .with("szVr", var(&sd.size))
            .with("minimumDiameter", prc(sd.size.minimum))
            .with("angleDynamics", var(&sd.angle))
            .with("roundnessDynamics", var(&sd.roundness))
            .with("minimumRoundness", prc(sd.roundness.minimum))
            .with("flipX", boolean(sd.flip_x_jitter))
            .with("flipY", boolean(sd.flip_y_jitter))
            .with("tiltScale", prc(sd.tilt_scale))
            .with("brushProjection", boolean(sd.brush_projection));
    }
    let sc = &b.scattering;
    if sc.enabled {
        d = d
            .with("useScatter", boolean(true))
            .with("scatterDynamics", var(&sc.scatter))
            .with("bothAxes", boolean(sc.both_axes))
            .with("Cnt ", Value::Integer(sc.count as i32))
            .with("countDynamics", var(&sc.count_jitter));
    }
    let t = &b.texture;
    if t.enabled {
        let tile = match &t.pattern {
            Pattern::Procedural { style, size, seed } => {
                let img = photocraft_paint::procedural::pattern(*style, *size, *seed);
                GrayTile {
                    width: img.width as u32,
                    height: img.height as u32,
                    data: img.data.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).collect(),
                }
            }
            Pattern::Tile(g) => g.clone(),
            Pattern::Stored(_) => panic!("stored patterns are not exported"),
        };
        let id = format!("$pc-export-pattern-{}", patterns.len() + 1);
        let pname = format!("{name} texture");
        patterns.push(PsdPattern {
            mode: 1,
            width: tile.width,
            height: tile.height,
            name: pname.clone(),
            id: id.clone(),
            palette: None,
            depth: 16,
            channels: vec![u16_be(&tile)],
            alpha: None,
        });
        let contrast = if t.contrast < 0.0 { t.contrast * 50.0 } else { t.contrast * 100.0 };
        d = d
            .with("useTexture", boolean(true))
            .with("TxtC", boolean(t.each_tip))
            .with("InvT", boolean(t.invert))
            .with("textureScale", prc(t.scale))
            .with("textureBrightness", Value::Integer((t.brightness * 150.0).round() as i32))
            .with("textureContrast", Value::Integer(contrast.round() as i32))
            .with("textureBlendMode", enumerated("BlnM", mask_code(t.mode)))
            .with("textureDepth", prc(t.depth))
            .with("textureDepthDynamics", var(&t.depth_jitter))
            .with("minimumDepth", prc(t.depth_jitter.minimum))
            .with("Txtr", Value::Descriptor(Descriptor::new("Ptrn").with("Nm  ", text(&pname)).with("Idnt", text(&id))));
    }
    let db = &b.dual_brush;
    if db.enabled {
        let mut t = match &db.tip {
            TipShape::Sampled(t) => {
                let id = format!("$pc-export-tip-{}", samples.len() + 1);
                samples.push(AbrSample { id: id.clone(), width: t.width, height: t.height, depth: 16, data: u16_be(t) });
                Descriptor::new("sampledBrush").with("sampledData", text(&id))
            }
            TipShape::Round => Descriptor::new("computedBrush"),
            other => panic!("dual tip {other:?} is not exported"),
        };
        t = t
            .with("Dmtr", px(db.size))
            .with("Hrdn", prc(db.hardness))
            .with("Angl", ang(db.angle))
            .with("Rndn", prc(db.roundness))
            .with("Spcn", prc(db.spacing));
        d = d.with(
            "dualBrush",
            Value::Descriptor(
                Descriptor::new("dualBrush")
                    .with("useDualBrush", boolean(true))
                    .with("Flip", boolean(db.flip))
                    .with("Brsh", Value::Descriptor(t))
                    .with("BlnM", enumerated("BlnM", mask_code(db.mode)))
                    .with("scatterDynamics", var(&Dynamic { jitter: db.scatter, ..Dynamic::default() }))
                    .with("bothAxes", boolean(db.both_axes))
                    .with("Cnt ", Value::Integer(db.count as i32)),
            ),
        );
    }
    let c = &b.color_dynamics;
    if c.enabled {
        d = d
            .with("useColorDynamics", boolean(true))
            .with("colorDynamicsPerTip", boolean(c.per_tip))
            .with("clVr", var(&c.fg_bg))
            .with("H   ", prc(c.hue_jitter))
            .with("Strt", prc(c.saturation_jitter))
            .with("Brgh", prc(c.brightness_jitter))
            .with("purity", prc(c.purity));
    }
    let tr = &b.transfer;
    if tr.enabled {
        d = d
            .with("usePaintDynamics", boolean(true))
            .with("opVr", var(&tr.opacity))
            .with("prVr", var(&tr.flow))
            .with("wtVr", var(&tr.wetness))
            .with("mxVr", var(&tr.mix));
    }
    d = d.with("Nose", boolean(b.noise)).with("Wtdg", boolean(b.wet_edges)).with("Rpt ", boolean(b.build_up));
    let s = &b.smoothing;
    d.with(
        "toolOptions",
        Value::Descriptor(
            Descriptor::new("null")
                .with("Opct", prc(b.opacity))
                .with("flow", prc(b.flow))
                .with("Md  ", enumerated("BlnM", blend_code(b.mode)))
                .with("usePressureOverridesSize", boolean(b.pressure_size))
                .with("usePressureOverridesOpacity", boolean(b.pressure_opacity))
                .with("smoothing", boolean(s.enabled))
                .with("smoothingValue", prc(s.amount))
                .with("smoothingRadiusMode", boolean(s.pulled_string))
                .with("smoothingCatchup", boolean(s.catch_up))
                .with("smoothingCatchupAtEnd", boolean(s.catch_up_on_end))
                .with("smoothingZoomCompensation", boolean(s.adjust_for_zoom)),
        ),
    )
}

/// The settings as JSON for comparison: procedural textures as the tile they render to, floats
/// rounded (the file stores percentages as doubles).
fn comparable(b: &BrushSettings) -> serde_json::Value {
    let mut b = b.clone();
    if let Pattern::Procedural { style, size, seed } = b.texture.pattern {
        let img = photocraft_paint::procedural::pattern(style, size, seed);
        b.texture.pattern = Pattern::Tile(GrayTile {
            width: img.width as u32,
            height: img.height as u32,
            data: img.data.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).collect(),
        });
    }
    b.seed = 0;
    fn round(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Number(n) if n.is_f64() => *v = serde_json::json!((n.as_f64().unwrap() * 1e4).round() / 1e4),
            serde_json::Value::Array(a) => a.iter_mut().for_each(round),
            serde_json::Value::Object(o) => o.values_mut().for_each(round),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(&b).unwrap();
    round(&mut v);
    v
}

fn diff(path: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
    match (a, b) {
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                diff(&format!("{path}.{k}"), x.get(k).unwrap_or(&serde_json::Value::Null), y.get(k).unwrap_or(&serde_json::Value::Null), out);
            }
        }
        _ if a != b => out.push(format!("{path}: {} → {}", short(a), short(b))),
        _ => {}
    }
}

fn short(v: &serde_json::Value) -> String {
    let s = v.to_string();
    if s.len() > 60 { format!("{}… ({} chars)", &s[..60], s.len()) } else { s }
}

fn merge(a: &mut serde_json::Value, b: &serde_json::Value) {
    match (a, b) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            for (k, v) in b {
                merge(a.entry(k.clone()).or_insert(serde_json::Value::Null), v);
            }
        }
        (a, b) => *a = b.clone(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.iter().position(|a| a == "--out").map(|i| args[i + 1].clone()).expect("--out <file.abr>");
    let specs: Vec<&String> = args.iter().enumerate().filter(|(i, a)| *a != "--out" && (*i == 0 || args[i - 1] != "--out")).map(|(_, a)| a).collect();
    let all = photocraft_paint::presets::builtin();
    let (mut samples, mut patterns, mut presets, mut originals) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for spec in specs {
        // `<built-in>[=<name>][|<JSON patch merged into the settings>]`
        let (spec, patch) = spec.split_once('|').map_or((spec.as_str(), None), |(a, b)| (a, Some(b)));
        let (from, name) = spec.split_once('=').unwrap_or((spec, spec));
        let mut b = photocraft_paint::presets::find(&all, from).unwrap_or_else(|| panic!("no built-in preset {from:?}")).brush.clone();
        if let Some(patch) = patch {
            let mut v = serde_json::to_value(&b).unwrap();
            merge(&mut v, &serde_json::from_str(patch).expect("patch JSON"));
            b = serde_json::from_value(v).expect("patched settings");
        }
        presets.push(descriptor(name, &b, &mut samples, &mut patterns));
        originals.push((name.to_string(), b));
    }
    let bytes = write_v6(2, &samples, &patterns, &presets, true).unwrap();
    // Read it back as PhotoCraft's Brushes panel will.
    let imported = photocraft_io::abr_map::read_abr(&bytes, "Export").unwrap();
    assert_eq!(imported.presets.len(), originals.len(), "every preset imports");
    let mut bad = false;
    for ((name, b), p) in originals.iter().zip(&imported.presets) {
        assert_eq!(&p.name, name);
        let mut d = Vec::new();
        diff("", &comparable(b), &comparable(&p.brush), &mut d);
        if d.is_empty() {
            println!("{name}: imports with identical settings");
        } else {
            bad = true;
            println!("{name}: differs after import:\n  {}", d.join("\n  "));
        }
    }
    for w in &imported.warnings {
        println!("import warning: {w}");
    }
    std::fs::write(&out, &bytes).unwrap();
    println!("{out}: {} presets, {:.1} KB", originals.len(), bytes.len() as f64 / 1024.0);
    assert!(!bad, "settings changed on import");
}
