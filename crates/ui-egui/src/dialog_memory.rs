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
/// Generative prompts last used, newest first (Edit › Generative Fill and the Remove tool share
/// them; user request, 2026-11-25).
const PROMPTS: &str = "recentPrompts";
/// How many prompts the list keeps.
pub const RECENT_PROMPTS: usize = 5;
/// Longest prompt kept (the fields take 500 characters).
const PROMPT_MAX_CHARS: usize = 500;
/// The Home screen's Recent view: "grid" or "list" (user request, 2026-11-11: it was forgotten
/// at every launch).
const HOME_RECENT_VIEW: &str = "homeRecentView";
/// Shading's, Shading 2's and Lighting's saved presets: `{kind: [{name, params}, …]}`.
const ADJUST_PRESETS: &str = "adjustPresets";
/// Most presets kept per kind.
const ADJUST_PRESETS_MAX: usize = 100;

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

/// The prompts last used (Generative Fill, the Remove tool), newest first, at most
/// [`RECENT_PROMPTS`]. Whatever else a settings file holds there is left out.
pub fn recent_prompts(app: &PhotocraftApp) -> Vec<String> {
    let saved = app.session.prefs().dialog_choices.get(PROMPTS).and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for p in saved.iter().filter_map(Value::as_str).map(clean_prompt) {
        if !p.is_empty() && !out.contains(&p) && out.len() < RECENT_PROMPTS {
            out.push(p);
        }
    }
    out
}

/// A prompt as the list keeps it: trimmed, on one line, at most [`PROMPT_MAX_CHARS`].
fn clean_prompt(p: &str) -> String {
    let one_line: String = p.chars().map(|c| if c.is_control() { ' ' } else { c }).take(PROMPT_MAX_CHARS).collect();
    one_line.trim().to_string()
}

/// After a generative run with `prompt`: put it first in the recent list (moved up if it was
/// there already). An empty prompt (the model's own guess) is not kept.
pub fn remember_prompt(app: &mut PhotocraftApp, prompt: &str) {
    let p = clean_prompt(prompt);
    if p.is_empty() {
        return;
    }
    let mut list = recent_prompts(app);
    if list.first() == Some(&p) {
        return;
    }
    list.retain(|q| *q != p);
    list.insert(0, p);
    list.truncate(RECENT_PROMPTS);
    set(app, PROMPTS, json!(list));
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

/// The saved presets of adjustment `kind` (name, params), in the order saved. Entries without a
/// name or a params object are skipped.
pub fn adjust_presets(app: &PhotocraftApp, kind: &str) -> Vec<(String, Value)> {
    app.session
        .prefs()
        .dialog_choices
        .get(ADJUST_PRESETS)
        .and_then(|m| m.get(kind))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    Some((e.get("name")?.as_str().filter(|n| !n.trim().is_empty())?.to_string(), e.get("params").filter(|p| p.is_object())?.clone()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn set_adjust_presets(app: &mut PhotocraftApp, kind: &str, list: Vec<(String, Value)>) {
    let mut all = app.session.prefs().dialog_choices.get(ADJUST_PRESETS).and_then(Value::as_object).cloned().unwrap_or_default();
    all.insert(kind.into(), Value::Array(list.into_iter().map(|(name, params)| json!({"name": name, "params": params})).collect()));
    set(app, ADJUST_PRESETS, Value::Object(all));
}

/// Saves `params` as preset `name` of `kind` (replacing one of that name, else added last).
pub fn save_adjust_preset(app: &mut PhotocraftApp, kind: &str, name: &str, params: Value) {
    let name = name.trim();
    if name.is_empty() || !params.is_object() {
        return;
    }
    let mut list = adjust_presets(app, kind);
    if let Some(entry) = list.iter_mut().find(|(n, _)| n == name) {
        entry.1 = params;
    } else if list.len() < ADJUST_PRESETS_MAX {
        list.push((name.to_string(), params));
    } else {
        return;
    }
    set_adjust_presets(app, kind, list);
}

/// Deletes preset `name` of `kind`.
pub fn delete_adjust_preset(app: &mut PhotocraftApp, kind: &str, name: &str) {
    let mut list = adjust_presets(app, kind);
    let before = list.len();
    list.retain(|(n, _)| n != name);
    if list.len() != before {
        set_adjust_presets(app, kind, list);
    }
}

/// Whether the Home screen lists recent files as a grid of cards (the default) rather than a list.
pub fn home_recent_grid(app: &PhotocraftApp) -> bool {
    app.session.prefs().dialog_choices.get(HOME_RECENT_VIEW).and_then(Value::as_str) != Some("list")
}

/// Remembers the Home screen's Recent view across launches.
pub fn set_home_recent_grid(app: &mut PhotocraftApp, grid: bool) {
    if home_recent_grid(app) != grid {
        set(app, HOME_RECENT_VIEW, json!(if grid { "grid" } else { "list" }));
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

    /// The prompts last used come back newest first, five at most, each once; empty ones aren't
    /// kept, and a settings file's junk is left out (user request, 2026-11-25).
    #[test]
    fn recent_prompts_keep_the_last_five_newest_first() {
        let mut a = app();
        assert!(recent_prompts(&a).is_empty());
        for p in ["a cat", "  ", "a dog", "sky
blue", "a cat", "grass", "sea", "rocks", "trees"] {
            remember_prompt(&mut a, p);
        }
        assert_eq!(recent_prompts(&a), ["trees", "rocks", "sea", "grass", "a cat"]);
        // A run through the app keeps its prompt too.
        let _ = a.run("edit.generativeFill", json!({"prompt": "a red door", "model": "lama"}));
        remember_prompt(&mut a, "sky
blue");
        assert_eq!(recent_prompts(&a)[0], "sky blue");
        let mut b = app();
        b.session.prefs.edit(|p| p.dialog_choices = json!({"recentPrompts": [3, "", "x", "x", "y", null, "z", "w", "v", "u"]}));
        assert_eq!(recent_prompts(&b), ["x", "y", "z", "w", "v"]);
        b.session.prefs.edit(|p| p.dialog_choices = json!({"recentPrompts": "nope"}));
        assert!(recent_prompts(&b).is_empty());
    }
}
