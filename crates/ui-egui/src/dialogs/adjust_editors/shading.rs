//! Shading (PhotoCraft's own adjustment): the shadow comes from the layer's mask, painted with any
//! brush; these settings decide how each colour shades (see `photocraft_compose::adjust::shading`).

use super::basic::sliders;
use super::{Color32, Edit, RichText, Tokens, Value, color_button, flag, json, label, rgb_of, widgets};
use photocraft_engine::adjust_params;

/// Starting points: (name, params they set). Everything they don't name goes back to its default.
pub(crate) const PRESETS: [(&str, &str); 5] = [
    ("Universal", r##"{}"##),
    ("Warm Skin", r##"{"depth":30,"saturation":40,"hueShift":28,"ambientAmount":4}"##),
    ("Cool Ambient", r##"{"depth":35,"saturation":15,"hueShift":26,"splitHue":60,"ambientAmount":35,"mood":"#a9b4dc","moodAmount":20}"##),
    ("Sunset", r##"{"depth":30,"saturation":30,"hueShift":30,"splitHue":200,"ambient":"#b3667a","ambientAmount":30,"mood":"#e0a3b0","moodAmount":25}"##),
    ("Cel (2 Tones)", r##"{"steps":2,"depth":30,"saturation":25}"##),
];

/// A hue-wheel track for the split slider, in OKLCH hues (the ones the shading turns through).
pub(super) fn hue_track() -> Vec<Color32> {
    (0..=24)
        .map(|i| {
            let h = (i as f32 * 15.0).to_radians();
            let lin = photocraft_compose::adjust::shading::oklab_to_lin([0.75, 0.12 * h.cos(), 0.12 * h.sin()]);
            let [r, g, b] = lin.map(|c| (photocraft_color::convert::linear_to_srgb(c.clamp(0.0, 1.0)) * 255.0).round() as u8);
            Color32::from_rgb(r, g, b)
        })
        .collect()
}

/// The Shading editor; `picker` for Shading 2 (the colour picker's axes: Brightness down,
/// Saturation sideways, no presets or Chroma Retention).
pub(crate) fn shading(ui: &mut egui::Ui, v: &mut Value, mem: egui::Id, picker: bool, saved: &[(String, Value)]) -> Edit {
    let t = Tokens::get(ui.ctx());
    let d = if picker { photocraft_doc::adjust::Shading::picker() } else { photocraft_doc::adjust::Shading::default() };
    let mut e = Edit::default();
    ui.label(RichText::new(tl!("Paint the shadow on the layer mask; each colour picks its own shadow colour.")).color(t.text_dim).size(11.0));
    ui.add_space(4.0);
    let defaults = adjust_params::to_params(&photocraft_doc::Adjustment::Shading(d.clone()));
    let builtin: &[(&str, &str)] = if picker { &[] } else { &PRESETS };
    e.add(super::presets::preset_row(ui, v, mem, if picker { "shading2" } else { "shading" }, builtin, &defaults, saved));
    ui.add_space(4.0);
    if picker {
        ui.label(RichText::new(tl!("Brightness moves down the colour picker; Saturation moves sideways (+ richer, − greyer).")).color(t.text_dim).size(11.0));
        e.add(sliders(
            ui,
            v,
            &[
                ("depth", tl!("Brightness"), 0.0, 100.0, d.depth * 100.0, "%", None),
                ("saturationShift", tl!("Saturation"), -100.0, 100.0, d.saturation_shift * 100.0, "", Some(0)),
            ],
        ));
    } else {
        e.add(sliders(
            ui,
            v,
            &[("depth", tl!("Depth"), 0.0, 100.0, d.depth * 100.0, "%", None), ("saturation", tl!("Saturation"), 0.0, 100.0, d.saturation * 100.0, "%", None)],
        ));
    }
    e.add(super::sat_curve::saturation_curve_editor(ui, v, mem, false));
    e.add(sliders(ui, v, &[("hueShift", tl!("Hue Shift"), 0.0, 90.0, d.hue_shift, "°", None)]));
    // Warm/cool split: colours on either side turn opposite ways; a hue track shows where.
    let mut split = super::num(v, "splitHue", d.split_hue).clamp(0.0, 360.0);
    let track = hue_track();
    let gestures = widgets::RowGestures { reset: Some(d.split_hue), wheel_step: None, ..Default::default() };
    let r = widgets::slider_row_with(ui, tl!("Warm/Cool Split"), &mut split, 0.0..=360.0, "°", Some(&track), gestures);
    if r.changed() {
        v["splitHue"] = json!(split.round());
    }
    e.add(Edit::of(&r));
    ui.add_space(6.0);
    label(ui, tl!("Shadow Light"));
    if !picker {
        e.add(sliders(ui, v, &[("chromaRetention", tl!("Chroma Retention"), 0.0, 100.0, d.chroma_retention * 100.0, "%", None)]));
    }
    e.add(sliders(ui, v, &[("selfBounce", tl!("Self-bounce"), 0.0, 100.0, d.self_bounce * 100.0, "%", None)]));
    ui.add_space(6.0);
    label(ui, tl!("Greys and Whites"));
    e.add(sliders(ui, v, &[("protectNeutrals", tl!("Protect Neutrals"), 0.0, 100.0, d.protect_neutrals * 100.0, "%", None)]));
    let mut physical = flag(v, "physical", d.physical);
    if widgets::checkbox(ui, &mut physical, tl!("Physical Shadow Color (from Ambient)")).changed() {
        v["physical"] = json!(physical);
        e.add(Edit::discrete(true));
    }
    e.add(color_amount(ui, v, "ambient", "ambientAmount", tl!("Ambient"), d.ambient, d.ambient_amount));
    ui.add_space(6.0);
    label(ui, tl!("Mood"));
    e.add(color_amount(ui, v, "mood", "moodAmount", tl!("Mood Tint"), d.mood, d.mood_amount));
    e.add(sliders(ui, v, &[("protectDarks", tl!("Protect Darks"), 0.0, 100.0, d.protect_darks * 100.0, "%", None)]));
    ui.add_space(6.0);
    label(ui, tl!("Terminator"));
    e.add(color_amount(ui, v, "terminator", "terminatorAmount", tl!("Terminator"), d.terminator, d.terminator_amount));
    e.add(sliders(
        ui,
        v,
        &[
            ("terminatorPosition", tl!("Position"), 0.0, 100.0, d.terminator_position * 100.0, "%", None),
            ("terminatorWidth", tl!("Width"), 5.0, 100.0, d.terminator_width * 100.0, "%", None),
        ],
    ));
    ui.add_space(6.0);
    label(ui, tl!("Style"));
    e.add(sliders(ui, v, &[("steps", tl!("Steps (0 = Smooth)"), 0.0, 8.0, 0.0, "", Some(0))]));
    e
}

/// A colour button and its amount slider on one row.
pub(super) fn color_amount(ui: &mut egui::Ui, v: &mut Value, color_key: &str, amount_key: &str, name: &str, color: [f32; 3], amount: f32) -> Edit {
    let mut e = Edit::default();
    ui.horizontal(|ui| {
        let mut c = rgb_of(v, color_key, color);
        let r = color_button(ui, &mut c, name);
        if r.changed {
            v[color_key] = json!(adjust_params::hex(c));
        }
        e.add(r);
        ui.vertical(|ui| e.add(sliders(ui, v, &[(amount_key, name, 0.0, 100.0, amount * 100.0, "%", None)])));
    });
    e
}
