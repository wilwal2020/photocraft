//! The Preset row of Shading, Shading 2 and Lighting: the built-in starting points plus the
//! user's own, saved from the current settings (user request, 2026-11-10). Saved presets live in
//! Preferences (`crate::dialog_memory::adjust_presets`), one list per kind; the editor asks its
//! host to save or delete one ([`apply_pending`]), as it has no access to the app.

use super::{Edit, RichText, Tokens, Value, label, widgets};
use crate::PhotocraftApp;

/// What the editor asked for this frame, taken by the host after the editor ran.
#[derive(Clone, Debug)]
enum Pending {
    Save { kind: String, name: String, params: Value },
    Delete { kind: String, name: String },
}

fn pending_id() -> egui::Id {
    egui::Id::unique("adjust-preset-pending")
}

/// Longest preset name kept.
const NAME_MAX: usize = 64;

/// `v` without the host's own keys (layer, coalescing, dialog fields).
fn params_only(v: &Value) -> Value {
    let mut o = v.as_object().cloned().unwrap_or_default();
    o.retain(|k, _| !k.starts_with("__") && !matches!(k.as_str(), "layer" | "coalesce" | "target"));
    Value::Object(o)
}

/// `defaults` with what `set` names (a preset's full params; keys added since it was saved
/// keep their defaults).
pub(super) fn merged(defaults: &Value, set: &Value) -> Value {
    let mut v = defaults.clone();
    if let (Some(o), Some(e)) = (v.as_object_mut(), set.as_object()) {
        o.extend(e.iter().map(|(k, x)| (k.clone(), x.clone())));
    }
    v
}

/// Whether two param sets are the same settings (numbers within rounding: 30 and 30.0 match).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => (x.as_f64().unwrap_or(f64::NAN) - y.as_f64().unwrap_or(f64::NAN)).abs() < 1e-3,
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, xv)| y.get(k).is_some_and(|yv| same(xv, yv))),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        _ => a == b,
    }
}

/// The Preset row: a menu of `builtin` (name, params JSON they set) and `saved` presets, Save…
/// (names the current settings) and Delete (the saved preset shown). `defaults` is the kind's
/// full default params.
pub(super) fn preset_row(ui: &mut egui::Ui, v: &mut Value, mem: egui::Id, kind: &str, builtin: &[(&str, &str)], defaults: &Value, saved: &[(String, Value)]) -> Edit {
    let t = Tokens::get(ui.ctx());
    let mut e = Edit::default();
    let shown_id = mem.with("preset-shown");
    let naming_id = mem.with("preset-naming");
    // The preset last picked or saved, while the settings are still its own.
    let shown: Option<(String, bool, Value)> = ui.data(|d| d.get_temp(shown_id));
    let current = shown.filter(|(_, _, p)| same(p, &params_only(v)));
    let mut naming: Option<String> = ui.data(|d| d.get_temp(naming_id));
    let mut pick: Option<(String, bool, Value)> = None;
    ui.horizontal(|ui| {
        label(ui, tl!("Preset:"));
        let text = match &current {
            Some((name, false, _)) => tl!(name).to_string(),
            Some((name, true, _)) => name.clone(),
            None => tl!("Custom").to_string(),
        };
        let combo = craft_ui::ComboBox::from_id_salt(mem.with(("preset", kind)))
            .selected_text(text)
            .width((ui.available_width() - 120.0).clamp(90.0, 160.0))
            .icon(widgets::chevron_icon)
            .show_ui(ui, |ui| {
                for (name, set) in builtin {
                    if ui.selectable_label(false, tl!(name)).clicked() {
                        let set = serde_json::from_str::<Value>(set).unwrap_or(Value::Null);
                        pick = Some(((*name).to_string(), false, merged(defaults, &set)));
                    }
                }
                if !builtin.is_empty() && !saved.is_empty() {
                    ui.separator();
                }
                for (name, set) in saved {
                    if ui.selectable_label(false, name).clicked() {
                        pick = Some((name.clone(), true, merged(defaults, set)));
                    }
                }
                if builtin.is_empty() && saved.is_empty() {
                    ui.label(RichText::new(tl!("No saved presets yet")).color(t.text_dim));
                }
            });
        let _ = combo.response.accessible_name(tl!("Preset:"));
        if widgets::compact_button(ui, tl!("Save…"), 22.0).on_hover_text(tl!("Save the current settings as a preset")).clicked() {
            naming = if naming.is_some() { None } else { Some(current.as_ref().filter(|c| c.1).map(|c| c.0.clone()).unwrap_or_default()) };
        }
        if let Some((name, true, _)) = &current
            && widgets::compact_button(ui, tl!("Delete"), 22.0).on_hover_text(tl!("Delete this saved preset")).clicked()
        {
            ui.data_mut(|d| d.insert_temp(pending_id(), Some(Pending::Delete { kind: kind.to_string(), name: name.clone() })));
            ui.data_mut(|d| d.remove::<Option<(String, bool, Value)>>(shown_id));
        }
    });
    if let Some((name, user, params)) = pick {
        *v = params;
        ui.data_mut(|d| d.insert_temp(shown_id, Some((name, user, params_only(v)))));
        e.add(Edit::discrete(true));
    }
    if let Some(name) = naming.as_mut() {
        let mut save = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            label(ui, tl!("Name:"));
            let r = widgets::text_field_ex(ui, mem.with("preset-name-field"), name, tl!("Preset name"), 150.0, false, Some(NAME_MAX)).accessible_name(tl!("Name:"));
            if !r.has_focus() && !r.lost_focus() && name.is_empty() {
                r.request_focus();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            cancel = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
            let ok = !name.trim().is_empty();
            save = ui.add_enabled_ui(ok, |ui| widgets::compact_button(ui, tl!("Save"), 22.0)).inner.clicked() || (enter && ok);
            cancel |= widgets::compact_button(ui, tl!("Cancel"), 22.0).clicked();
        });
        if save {
            let name: String = name.trim().chars().take(NAME_MAX).collect();
            let params = params_only(v);
            ui.data_mut(|d| {
                d.insert_temp(pending_id(), Some(Pending::Save { kind: kind.to_string(), name: name.clone(), params: params.clone() }));
                d.insert_temp(shown_id, Some((name, true, params)));
            });
            naming = None;
        } else if cancel {
            naming = None;
        }
    }
    ui.data_mut(|d| match naming {
        Some(n) => {
            d.insert_temp(naming_id, Some(n));
        }
        None => d.remove::<Option<String>>(naming_id),
    });
    e
}

/// The host's half: saves or deletes the preset the editor asked for this frame.
pub fn apply_pending(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(p) = ctx.data_mut(|d| d.get_temp::<Option<Pending>>(pending_id()).flatten().inspect(|_| d.remove::<Option<Pending>>(pending_id())))
    else {
        return;
    };
    match p {
        Pending::Save { kind, name, params } => crate::dialog_memory::save_adjust_preset(app, &kind, &name, params),
        Pending::Delete { kind, name } => crate::dialog_memory::delete_adjust_preset(app, &kind, &name),
    }
}

/// The saved presets of `kind` (the editors' [`super::EditorCx::presets`]).
pub fn saved(app: &PhotocraftApp, kind: &str) -> Vec<(String, Value)> {
    if matches!(kind, "shading" | "shading2" | "lighting") { crate::dialog_memory::adjust_presets(app, kind) } else { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn presets_compare_by_value_and_merge_over_defaults() {
        assert!(same(&json!({"depth": 30, "mood": "#fff"}), &json!({"mood": "#fff", "depth": 30.0004})));
        assert!(!same(&json!({"depth": 30}), &json!({"depth": 31})));
        assert!(!same(&json!({"depth": 30}), &json!({"depth": 30, "steps": 2})));
        let m = merged(&json!({"depth": 40, "steps": 0}), &json!({"steps": 2, "extra": true}));
        assert_eq!(m, json!({"depth": 40, "steps": 2, "extra": true}));
        assert_eq!(params_only(&json!({"layer": 3, "coalesce": "x", "__k": 1, "target": 0, "depth": 5})), json!({"depth": 5}));
    }
}
