//! Chromatic Aberration (PhotoCraft's own adjustment layer): red and blue shifted against green,
//! like a lens's colour fringes (see `photocraft_compose::adjust::chromatic`).

use super::basic::sliders;
use super::{Edit, RichText, Tokens, Value, label};
use photocraft_doc::adjust::ChromaticAberration;
use photocraft_engine::adjust_params;

/// Starting points: (name, params they set). Everything they don't name goes back to its default.
pub(crate) const PRESETS: [(&str, &str); 5] = [
    ("Subtle Lens", r##"{}"##),
    ("Strong Lens", r##"{"redShift":10,"blueShift":-10,"falloff":20,"softness":1.5}"##),
    ("Edge Fringe", r##"{"redShift":8,"blueShift":-6,"falloff":60,"softness":2}"##),
    ("RGB Split", r##"{"redShift":0,"blueShift":0,"offsetX":4,"offsetY":0}"##),
    ("Soft Glitch", r##"{"redShift":3,"blueShift":-3,"offsetX":-3,"offsetY":2,"softness":1}"##),
];

pub(crate) fn chromatic(ui: &mut egui::Ui, v: &mut Value, mem: egui::Id, saved: &[(String, Value)]) -> Edit {
    let t = Tokens::get(ui.ctx());
    let d = ChromaticAberration::default();
    let m = ChromaticAberration::MAX_SHIFT;
    let mut e = Edit::default();
    ui.label(
        RichText::new(tl!("Shifts red and blue against green, like a lens's colour fringes. Paint the mask to keep areas clean.")).color(t.text_dim).size(11.0),
    );
    ui.add_space(4.0);
    let defaults = adjust_params::to_params(&photocraft_doc::Adjustment::ChromaticAberration(d.clone()));
    e.add(super::presets::preset_row(ui, v, mem, "chromaticAberration", &PRESETS, &defaults, saved));
    ui.add_space(4.0);
    label(ui, tl!("Lens (at the corners)"));
    e.add(sliders(
        ui,
        v,
        &[("redShift", tl!("Red/Cyan"), -m, m, d.red_shift, " px", Some(1)), ("blueShift", tl!("Blue/Yellow"), -m, m, d.blue_shift, " px", Some(1))],
    ));
    ui.add_space(6.0);
    label(ui, tl!("Offset (red; blue the other way)"));
    e.add(sliders(
        ui,
        v,
        &[("offsetX", tl!("Horizontal"), -m, m, d.offset_x, " px", Some(1)), ("offsetY", tl!("Vertical"), -m, m, d.offset_y, " px", Some(1))],
    ));
    ui.add_space(6.0);
    label(ui, tl!("Centre"));
    e.add(sliders(
        ui,
        v,
        &[
            ("centerX", tl!("Centre X"), 0.0, 100.0, d.center[0] * 100.0, "%", None),
            ("centerY", tl!("Centre Y"), 0.0, 100.0, d.center[1] * 100.0, "%", None),
            ("falloff", tl!("Clean Centre"), 0.0, 100.0, d.falloff * 100.0, "%", None),
        ],
    ));
    ui.add_space(6.0);
    e.add(sliders(ui, v, &[("softness", tl!("Softness"), 0.0, ChromaticAberration::MAX_SOFTNESS, d.softness, " px", Some(1))]));
    e
}
