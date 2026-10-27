//! Small dialog choices remembered across restarts (`Preferences::dialog_choices`): Select ›
//! Color Range's Selection Preview and Edit › Generative Fill's model (user report: neither came
//! back after a restart), and the settings and colours Color Range last selected with (user
//! report: it should remember them, as Photoshop's does). A hand-edited file can't break them: an
//! unknown value keeps the default.

use serde_json::{Map, Value, json};

use crate::PhotocraftApp;

const PREVIEW: &str = "colorRangePreview";
const MODEL: &str = "generativeFillModel";
const COLOR_RANGE: &str = "colorRange";

/// Color Range's numeric settings that come back at the next opening.
const COLOR_RANGE_NUMBERS: &[&str] = &["fuzziness", "range", "toneFuzziness", "shadowsLevel", "highlightsLevel", "midtonesLow", "midtonesHigh"];
/// Its on/off settings.
const COLOR_RANGE_FLAGS: &[&str] = &["localized", "invert"];

/// The models Generative Fill's dialog offers here (Nano Banana is online only).
pub fn generative_models() -> &'static [&'static str] {
    if cfg!(target_arch = "wasm32") { &["diffusion", "lama"] } else { &["diffusion", "lama", "nanoBanana"] }
}

/// Generative Fill's model as last used, else the diffusion one.
pub fn generative_model(app: &PhotocraftApp) -> &'static str {
    let saved = app.session.prefs().dialog_choices.get(MODEL).and_then(Value::as_str).map(str::to_string);
    generative_models().iter().copied().find(|m| saved.as_deref() == Some(*m)).unwrap_or("diffusion")
}

/// After Generative Fill ran with `params`: remember its model.
pub fn used_generative_fill(app: &mut PhotocraftApp, params: &Value) {
    let Some(model) = params.get("model").and_then(Value::as_str) else { return };
    if generative_models().contains(&model) && generative_model(app) != model {
        set(app, MODEL, json!(model));
    }
}

fn set(app: &mut PhotocraftApp, key: &str, v: Value) {
    let mut m = app.session.prefs().dialog_choices.as_object().cloned().unwrap_or_else(Map::new);
    m.insert(key.into(), v);
    app.session.prefs.edit(|p| p.dialog_choices = Value::Object(m));
}

/// After Color Range's OK: remember its settings (`fields`, the dialog's) and the colours it
/// sampled, as the CIE Lab box an action records (`labRange`): eyedropper points are positions
/// in this document, which mean nothing in the next one. Localized clusters need positions, so
/// they fall back to the foreground colour next time.
pub fn used_color_range(app: &mut PhotocraftApp, fields: &Map<String, Value>, params: &Value) {
    let mut keep = sanitized_color_range(fields);
    keep.remove("labRange");
    let sampled = params.get("select").and_then(Value::as_str) == Some("sampledColors");
    let localized = params.get("localized").and_then(Value::as_bool) == Some(true);
    if sampled && !localized {
        let recorded = photocraft_engine::selection_cmds::recorded_color_range(&app.session, params.clone());
        if let Some(range) = recorded.get("labRange").filter(|r| lab_range(r)) {
            keep.insert("labRange".into(), range.clone());
        }
    }
    let keep = Value::Object(keep);
    if app.session.prefs().dialog_choices.get(COLOR_RANGE) != Some(&keep) {
        set(app, COLOR_RANGE, keep);
    }
}

/// The Color Range settings remembered by [`used_color_range`], checked: only known keys with
/// values of the right kind come back.
pub fn color_range(app: &PhotocraftApp) -> Map<String, Value> {
    app.session.prefs().dialog_choices.get(COLOR_RANGE).and_then(Value::as_object).map(sanitized_color_range).unwrap_or_default()
}

fn sanitized_color_range(f: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(select) = f.get("select").and_then(Value::as_str).filter(|s| crate::color_range_ui::SELECTS.iter().any(|(id, _)| id == s)) {
        out.insert("select".into(), json!(select));
    }
    for key in COLOR_RANGE_NUMBERS {
        if let Some(v) = f.get(*key).and_then(Value::as_f64).filter(|v| v.is_finite() && (-1000.0..=1000.0).contains(v)) {
            out.insert((*key).into(), json!(v));
        }
    }
    for key in COLOR_RANGE_FLAGS {
        if let Some(v) = f.get(*key).and_then(Value::as_bool) {
            out.insert((*key).into(), json!(v));
        }
    }
    if let Some(view) = f.get("__view").and_then(Value::as_str).filter(|v| matches!(*v, "selection" | "image")) {
        out.insert("__view".into(), json!(view));
    }
    if let Some(range) = f.get("labRange").filter(|r| lab_range(r)) {
        out.insert("labRange".into(), range.clone());
    }
    out
}

/// `{"min": [L, a, b], "max": [L, a, b]}` of finite numbers.
fn lab_range(v: &Value) -> bool {
    let corner = |k: &str| v.get(k).and_then(Value::as_array).is_some_and(|a| a.len() == 3 && a.iter().all(|x| x.as_f64().is_some_and(f64::is_finite)));
    corner("min") && corner("max")
}

/// Remember Color Range's Selection Preview when it changes. Cheap: one string compare a frame.
pub fn persist(app: &mut PhotocraftApp) {
    let now = app.ui.color_range.selection_preview.clone();
    let saved = app.session.prefs().dialog_choices.get(PREVIEW).and_then(Value::as_str).unwrap_or("none").to_string();
    if now != saved {
        set(app, PREVIEW, json!(now));
    }
}

/// Restore Color Range's Selection Preview at launch.
pub fn restore(app: &mut PhotocraftApp) {
    let saved = app.session.prefs().dialog_choices.get(PREVIEW).and_then(Value::as_str).map(str::to_string);
    if let Some(mode) = saved.filter(|m| crate::color_range_ui::is_preview(m)) {
        app.ui.color_range.selection_preview = mode;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> PhotocraftApp {
        PhotocraftApp::new(photocraft_engine::Session::new(), Default::default())
    }

    /// User report: Color Range's Selection Preview and Generative Fill's model were forgotten at
    /// a restart. Both come back from the preferences; junk keeps the defaults.
    #[test]
    fn dialog_choices_survive_a_restart() {
        let mut a = app();
        assert_eq!(generative_model(&a), "diffusion");
        a.ui.color_range.selection_preview = "blackMatte".into();
        persist(&mut a);
        used_generative_fill(&mut a, &json!({"model": "lama", "prompt": ""}));
        let saved = a.session.prefs().clone();
        let mut b = app();
        b.session.prefs.edit(|p| *p = saved);
        restore(&mut b);
        assert_eq!(b.ui.color_range.selection_preview, "blackMatte");
        assert_eq!(generative_model(&b), "lama");
        let mut c = app();
        c.session.prefs.edit(|p| p.dialog_choices = json!({"colorRangePreview": "lava", "generativeFillModel": 3}));
        restore(&mut c);
        assert_eq!(c.ui.color_range.selection_preview, "none");
        assert_eq!(generative_model(&c), "diffusion");
        used_generative_fill(&mut c, &json!({"model": "nope"}));
        assert_eq!(generative_model(&c), "diffusion");
    }
}
