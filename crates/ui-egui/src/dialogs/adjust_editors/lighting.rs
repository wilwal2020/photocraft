//! Lighting (PhotoCraft's own adjustment, the mirror of Shading): the light comes from the
//! layer's mask, painted with any brush; these settings decide how each colour lights up (see
//! `photocraft_compose::adjust::lighting`).

use super::basic::sliders;
use super::shading::{color_amount, hue_track};
use super::{Edit, RichText, Tokens, Value, flag, json, label, widgets};
use photocraft_engine::adjust_params;

/// Starting points: (name, params they set). Everything they don't name goes back to its default.
pub(crate) const PRESETS: [(&str, &str); 5] = [
    ("Universal", r##"{}"##),
    ("Warm Sun", r##"{"lift":40,"saturation":30,"lightHue":85,"light":"#ffd38a","lightAmount":35}"##),
    ("Cool Rim", r##"{"lift":45,"saturation":15,"hueShift":20,"lightHue":220,"light":"#bfe0ff","lightAmount":40,"glow":"#cfe6ff","glowAmount":15}"##),
    ("Soft Glow", r##"{"lift":30,"saturation":15,"glowAmount":30}"##),
    ("Cel (2 Tones)", r##"{"steps":2,"lift":35,"saturation":25}"##),
];

pub(crate) fn lighting(ui: &mut egui::Ui, v: &mut Value, mem: egui::Id, saved: &[(String, Value)]) -> Edit {
    let t = Tokens::get(ui.ctx());
    let d = photocraft_doc::adjust::Lighting::default();
    let mut e = Edit::default();
    ui.label(RichText::new(tl!("Paint the light on the layer mask; each colour picks its own lit colour.")).color(t.text_dim).size(11.0));
    ui.add_space(4.0);
    let defaults = adjust_params::to_params(&photocraft_doc::Adjustment::Lighting(d.clone()));
    e.add(super::presets::preset_row(ui, v, mem, "lighting", &PRESETS, &defaults, saved));
    ui.add_space(4.0);
    e.add(sliders(
        ui,
        v,
        &[
            ("lift", tl!("Brightness"), 0.0, 100.0, d.lift * 100.0, "%", None),
            ("saturation", tl!("Saturation"), 0.0, 100.0, d.saturation * 100.0, "%", None),
        ],
    ));
    e.add(super::sat_curve::saturation_curve_editor(ui, v, mem, true));
    e.add(sliders(
        ui,
        v,
        &[
            ("highlightFade", tl!("Highlight Fade"), 0.0, 100.0, d.highlight_fade * 100.0, "%", None),
            ("hueShift", tl!("Hue Shift"), 0.0, 90.0, d.hue_shift, "°", None),
        ],
    ));
    // The hue lit colours turn toward, on a hue track.
    let mut hue = super::num(v, "lightHue", d.light_hue).clamp(0.0, 360.0);
    let track = hue_track();
    let gestures = widgets::RowGestures { reset: Some(d.light_hue), wheel_step: None, ..Default::default() };
    let r = widgets::slider_row_with(ui, tl!("Light Hue"), &mut hue, 0.0..=360.0, "°", Some(&track), gestures);
    if r.changed() {
        v["lightHue"] = json!(hue.round());
    }
    e.add(Edit::of(&r));
    ui.add_space(6.0);
    label(ui, tl!("Greys and Whites"));
    e.add(sliders(ui, v, &[("protectNeutrals", tl!("Protect Neutrals"), 0.0, 100.0, d.protect_neutrals * 100.0, "%", None)]));
    e.add(color_amount(ui, v, "light", "lightAmount", tl!("Light Color"), d.light, d.light_amount));
    ui.add_space(6.0);
    label(ui, tl!("Shine"));
    e.add(sliders(
        ui,
        v,
        &[("gloss", tl!("Gloss"), 0.0, 100.0, d.gloss * 100.0, "%", None), ("glossSize", tl!("Gloss Size"), 2.0, 50.0, d.gloss_size * 100.0, "%", None)],
    ));
    let mut metallic = flag(v, "metallic", d.metallic);
    if widgets::checkbox(ui, &mut metallic, tl!("Metallic")).changed() {
        v["metallic"] = json!(metallic);
        e.add(Edit::discrete(true));
    }
    ui.add_space(6.0);
    label(ui, tl!("Glow"));
    e.add(color_amount(ui, v, "glow", "glowAmount", tl!("Glow Tint"), d.glow, d.glow_amount));
    ui.add_space(6.0);
    label(ui, tl!("Style"));
    e.add(sliders(
        ui,
        v,
        &[
            ("protectDarks", tl!("Protect Darks"), 0.0, 100.0, d.protect_darks * 100.0, "%", None),
            ("steps", tl!("Steps (0 = Smooth)"), 0.0, 8.0, 0.0, "", Some(0)),
        ],
    ));
    e
}
