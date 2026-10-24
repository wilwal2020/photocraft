//! Small dialog choices remembered across restarts (`Preferences::dialog_choices`): Select ›
//! Color Range's Selection Preview and Edit › Generative Fill's model (user report: neither came
//! back after a restart). A hand-edited file can't break them: an unknown value keeps the default.

use serde_json::{Map, Value, json};

use crate::PhotocraftApp;

const PREVIEW: &str = "colorRangePreview";
const MODEL: &str = "generativeFillModel";

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
