//! An effect's own mask (`FxCommon::mask`, a PhotoCraft extension): where it is black that one
//! effect doesn't show, so a stroke can run along part of a shape only. Select the effect's row
//! in the Layers panel and click Add Mask to add one; painting, fills, filters and adjustments then
//! edit it with `"target":"fxMask:N"`, N the effect's index in the layer's style.

use photocraft_doc::{LayerId, LayerMask};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, layer_param};
use crate::{EngineError, Result, Session};

pub const ADD: &str = "layer.layerStyle.addMask";
pub const DELETE: &str = "layer.layerStyle.deleteMask";
pub const ENABLED: &str = "layer.layerStyle.maskEnabled";

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn flag(cmd: &str, p: &Value, key: &str) -> Result<Option<bool>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(bad(cmd, format!("`{key}` must be true or false"))),
    }
}

/// The layer the params name (or the active one) and the effect `index` they name, which must be
/// one of its effects; with `has_mask`, one with a mask.
fn effect_of(s: &Session, p: &Value, cmd: &str, has_mask: Option<bool>) -> Result<(LayerId, usize)> {
    let id = layer_param(s, p)?;
    let index = p.get("index").and_then(Value::as_u64).ok_or_else(|| bad(cmd, "needs `index`, the effect's place in the layer's style"))?;
    let l = s.active().and_then(|d| d.doc.layer(id)).ok_or(EngineError::NoLayer(id))?;
    let index = usize::try_from(index).ok().filter(|i| *i < l.effects.items.len()).ok_or_else(|| bad(cmd, format!("the layer has no effect {index}")))?;
    match (has_mask, l.effects.items.get(index).and_then(photocraft_doc::Effect::mask).is_some()) {
        (Some(true), false) => Err(bad(cmd, "the effect has no mask")),
        (Some(false), true) => Err(bad(cmd, "the effect already has a mask")),
        _ => Ok((id, index)),
    }
}

/// A mask for effect `index`: from the selection when there is one (dropped, as Add Mask drops it),
/// else revealing all; `hide` inverts it (⌥-click in Photoshop).
fn add(s: &mut Session, p: &Value) -> Result<Value> {
    let hide = flag(ADD, p, "hide")?.unwrap_or(false);
    let (id, index) = effect_of(s, p, ADD, Some(false))?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let (depth, bounds) = (d.doc.depth, d.doc.bounds());
    let selection = d.doc.selection.clone();
    let mut mask = match (&selection, hide) {
        (Some(sel), false) => LayerMask { surface: sel.clone(), ..LayerMask::reveal_all() },
        (Some(sel), true) => {
            let mut v = crate::allocation::read_region(sel, bounds, "adding the effect mask")?;
            for x in &mut v {
                *x = 1.0 - *x;
            }
            let mut m = LayerMask::reveal_all();
            m.surface.write_region(bounds, &v);
            m.surface.prune();
            m
        }
        (None, false) => LayerMask::reveal_all(),
        (None, true) => LayerMask::hide_all(),
    };
    let format = mask.surface.format().with_sample(depth);
    if mask.surface.format() != format {
        mask.surface = mask.surface.try_convert(format)?;
    }
    s.edit("Add Effect Mask", |doc, _| {
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let e = l.effects.items.get_mut(index).ok_or_else(|| bad(ADD, format!("the layer has no effect {index}")))?;
        *e.mask_mut() = Some(mask);
        if selection.is_some() {
            doc.selection = None;
        }
        Ok(())
    })?;
    Ok(json!({"layer": id.0, "index": index, "target": crate::commands::fx_mask_target(index)}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let (id, index) = effect_of(s, p, DELETE, Some(true))?;
    s.edit("Delete Effect Mask", |doc, _| {
        if let Some(e) = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.effects.items.get_mut(index) {
            *e.mask_mut() = None;
        }
        Ok(())
    })?;
    Ok(json!({"layer": id.0, "index": index}))
}

fn enabled(s: &mut Session, p: &Value) -> Result<Value> {
    let on = flag(ENABLED, p, "enabled")?.ok_or_else(|| bad(ENABLED, "needs `enabled`"))?;
    let (id, index) = effect_of(s, p, ENABLED, Some(true))?;
    let label = if on { "Enable Effect Mask" } else { "Disable Effect Mask" };
    s.edit(label, |doc, _| {
        if let Some(m) = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.effects.items.get_mut(index).and_then(|e| e.mask_mut().as_mut()) {
            m.enabled = on;
        }
        Ok(())
    })?;
    Ok(json!({"layer": id.0, "index": index, "enabled": on}))
}

fn has_effect(s: &Session) -> std::result::Result<(), String> {
    let l = s.active().and_then(|d| d.active_layer.and_then(|id| d.doc.layer(id))).ok_or_else(|| "no active layer".to_string())?;
    if l.effects.items.is_empty() { Err("the layer has no layer style".into()) } else { Ok(()) }
}

pub fn specs() -> Vec<CommandSpec> {
    let spec = |id, label, params, run| CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_effect, run, journal: true };
    vec![
        spec(
            ADD,
            "Add Effect Mask",
            r##"{"layer":id?,"index":i (the effect's place in the layer's style, from 0),"hide":bool? (inverted)} → {layer,index,target:"fxMask:i"} — a mask for that effect alone: where it is black the effect doesn't show. From the selection when there is one (which it drops), else revealing all. Paint, fill, filter or adjust it with that target"##,
            add,
        ),
        spec(DELETE, "Delete Effect Mask", r##"{"layer":id?,"index":i}"##, delete),
        spec(ENABLED, "Enable Effect Mask", r##"{"layer":id?,"index":i,"enabled":bool} (a disabled mask lets the effect show everywhere)"##, enabled),
    ]
}

#[cfg(test)]
mod tests;
