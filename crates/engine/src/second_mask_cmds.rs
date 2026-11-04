//! PhotoCraft's second layer mask (`Layer::second_mask`): a pixel mask applied together with the
//! layer mask and the vector mask. Add Mask on a layer that has a layer mask adds it (Photoshop
//! adds a vector mask there); its thumbnail's menu turns it into the vector mask. Painting, fills,
//! filters and adjustments edit it with `"target":"secondMask"`, as `"target":"mask"` edits the
//! first.

use photocraft_doc::{Layer, LayerContent, LayerId, LayerMask};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, layer_param};
use crate::{EngineError, Result, Session};

pub const ADD: &str = "layer.secondMask.add";
pub const DELETE: &str = "layer.secondMask.delete";
pub const ENABLED: &str = "layer.secondMask.enabled";
pub const TO_VECTOR: &str = "layer.secondMask.toVectorMask";
pub const LINKED: &str = "layer.secondMask.linked";
/// The Layers panel's Add Mask button as one command (user request): an action records "Add
/// Mask", and playing it adds whichever mask the layer is missing then, as the button would.
pub const ADD_MASK: &str = "layer.addMask";

/// The layer mask Add Layer Mask makes: an active selection becomes the mask; `hide` (⌥) inverts
/// it (Hide Selection, or Hide All without a selection).
pub fn layer_mask_command(has_selection: bool, hide: bool) -> &'static str {
    match (has_selection, hide) {
        (true, false) => "layer.layerMask.revealSelection",
        (true, true) => "layer.layerMask.hideSelection",
        (false, false) => "layer.layerMask.revealAll",
        (false, true) => "layer.layerMask.hideAll",
    }
}

/// What Add Mask does for `layer` (#2075). Like Photoshop it never replaces a mask: a layer
/// without a layer mask gets one ([`layer_mask_command`]); a layer that already has one gets the
/// second pixel mask ([`ADD`], `"hide"` with ⌥; Photoshop adds the vector mask there), then a
/// vector mask (⌥: Hide All); a layer with all three (or a shape layer, whose path is already its
/// vector mask, with both pixel masks) gets nothing (`None`).
pub fn add_mask_step(l: &Layer, has_selection: bool, hide: bool) -> Option<&'static str> {
    if l.mask.is_none() {
        Some(layer_mask_command(has_selection, hide))
    } else if l.second_mask.is_none() {
        Some(ADD)
    } else if l.vector_mask.is_none() && !matches!(l.content, LayerContent::Shape(_)) {
        Some(if hide { "layer.vectorMask.hideAll" } else { "layer.vectorMask.revealAll" })
    } else {
        None
    }
}

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

/// The layer the params name (or the active one), which must have a second mask.
fn with_second(s: &Session, p: &Value, cmd: &str) -> Result<LayerId> {
    let id = layer_param(s, p)?;
    let has = s.active().and_then(|d| d.doc.layer(id)).is_some_and(|l| l.second_mask.is_some());
    if !has {
        return Err(bad(cmd, "the layer has no second mask"));
    }
    Ok(id)
}

/// Add Mask a second time: from the selection when there is one (dropped, as for the first mask),
/// else revealing all; `hide` inverts it (⌥-click in Photoshop).
fn add(s: &mut Session, p: &Value) -> Result<Value> {
    let hide = flag(ADD, p, "hide")?.unwrap_or(false);
    let id = layer_param(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    if l.mask.is_none() {
        return Err(bad(ADD, "add a layer mask first"));
    }
    if l.second_mask.is_some() {
        return Err(bad(ADD, "the layer already has a second mask"));
    }
    let (depth, bounds) = (d.doc.depth, d.doc.bounds());
    let selection = d.doc.selection.clone();
    let mut mask = match (&selection, hide) {
        (Some(sel), false) => LayerMask { surface: sel.clone(), ..LayerMask::reveal_all() },
        // Hiding the selected area, as Layer › Layer Mask › Hide Selection.
        (Some(sel), true) => {
            let mut v = crate::allocation::read_region(sel, bounds, "adding the second mask")?;
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
    s.edit("Add Second Mask", |doc, _| {
        doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.second_mask = Some(mask);
        if selection.is_some() {
            doc.selection = None;
        }
        Ok(())
    })?;
    Ok(json!({"layer": id.0}))
}

/// [`ADD_MASK`]: the step [`add_mask_step`] picks for the layer as it is now, run as this one
/// command, so recording keeps a single "Add Mask" step. Without `"layer"` a new layer mask goes
/// on every selected layer that can take one, as the button's does.
fn add_mask(s: &mut Session, p: &Value) -> Result<Value> {
    let hide = flag(ADD_MASK, p, "hide")?.unwrap_or(false);
    let id = layer_param(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let step = add_mask_step(l, d.doc.selection.is_some(), hide).ok_or_else(|| bad(ADD_MASK, "the layer already has all its masks"))?;
    let mut params = json!({});
    if let Some(layer) = p.get("layer").filter(|v| !v.is_null()) {
        params["layer"] = layer.clone();
    }
    if step == ADD {
        params["hide"] = json!(hide);
    }
    let r = s.execute(step, params)?;
    let added = match step {
        ADD => "secondMask",
        s if s.starts_with("layer.vectorMask.") => "vectorMask",
        _ => "mask",
    };
    Ok(json!({"layer": r.get("layer").cloned().unwrap_or(json!(id.0)), "added": added}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let id = with_second(s, p, DELETE)?;
    s.edit("Delete Second Mask", |doc, _| {
        doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.second_mask = None;
        Ok(())
    })?;
    Ok(json!({"layer": id.0}))
}

fn enabled(s: &mut Session, p: &Value) -> Result<Value> {
    let on = flag(ENABLED, p, "enabled")?.ok_or_else(|| bad(ENABLED, "needs `enabled`"))?;
    let id = with_second(s, p, ENABLED)?;
    let label = if on { "Enable Second Mask" } else { "Disable Second Mask" };
    s.edit(label, |doc, _| {
        if let Some(m) = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.second_mask.as_mut() {
            m.enabled = on;
        }
        Ok(())
    })?;
    Ok(json!({"layer": id.0, "enabled": on}))
}

/// The chain before the thumbnail: whether the second mask moves with the layer.
fn linked(s: &mut Session, p: &Value) -> Result<Value> {
    let on = flag(LINKED, p, "linked")?.ok_or_else(|| bad(LINKED, "needs `linked`"))?;
    let id = with_second(s, p, LINKED)?;
    s.edit(if on { "Link Second Mask" } else { "Unlink Second Mask" }, |doc, _| {
        if let Some(m) = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.second_mask.as_mut() {
            m.linked = on;
        }
        Ok(())
    })?;
    Ok(json!({"layer": id.0, "linked": on}))
}

/// The second mask becomes the vector mask: its revealed area (above half) traced into a path,
/// as Make Work Path traces a selection, at `tolerance` pixels.
fn to_vector(s: &mut Session, p: &Value) -> Result<Value> {
    let tol = match p.get("tolerance") {
        None | Some(Value::Null) => 2.0,
        Some(v) => v.as_f64().filter(|t| t.is_finite() && (0.5..=10.0).contains(t)).ok_or_else(|| bad(TO_VECTOR, "`tolerance` must be 0.5..10"))?,
    };
    let id = with_second(s, p, TO_VECTOR)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let area = d.doc.bounds();
    let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    if l.vector_mask.is_some() {
        return Err(bad(TO_VECTOR, "the layer already has a vector mask"));
    }
    let m = l.second_mask.as_ref().ok_or_else(|| bad(TO_VECTOR, "the layer has no second mask"))?;
    let values = photocraft_algo::selection::mask_from_surface(Some(&m.surface), area);
    let path = photocraft_vector::trace::trace_mask(&values, area, 0.0, tol);
    let (enabled, linked, density, feather) = (m.enabled, m.linked, m.density, m.feather);
    s.edit("Convert to Vector Mask", |doc, _| {
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let mut vm = photocraft_doc::vector::VectorMask::new(path);
        (vm.enabled, vm.linked, vm.density, vm.feather) = (enabled, linked, density, feather);
        l.vector_mask = Some(vm);
        l.second_mask = None;
        Ok(())
    })?;
    Ok(json!({"layer": id.0}))
}

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    s.active().and_then(|d| d.active_layer).map(|_| ()).ok_or_else(|| "no active layer".into())
}

pub fn specs() -> Vec<CommandSpec> {
    let spec = |id, label, params, run| CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: has_layer, run, journal: true };
    vec![
        spec(
            ADD,
            "Add Second Mask",
            r##"{"layer":id?,"hide":bool? (inverted)} — a second pixel mask on a layer that has a layer mask, from the selection when there is one (which it drops), else revealing all"##,
            add,
        ),
        spec(
            ADD_MASK,
            "Add Mask",
            r##"{"layer":id?,"hide":bool? (inverted)} → {layer, added:"mask"|"secondMask"|"vectorMask"} — the Layers panel's Add Mask: a layer mask (from the selection when there is one; without `layer`, on every selected layer), else the second mask, else a vector mask"##,
            add_mask,
        ),
        spec(DELETE, "Delete Second Mask", r##"{"layer":id?}"##, delete),
        spec(ENABLED, "Enable Second Mask", r##"{"layer":id?,"enabled":bool}"##, enabled),
        spec(LINKED, "Link Second Mask", r##"{"layer":id?,"linked":bool}"##, linked),
        spec(
            TO_VECTOR,
            "Convert Second Mask to Vector Mask",
            r##"{"layer":id?,"tolerance":0.5..10 px=2} — traces the mask's revealed area into the vector mask (the layer must have none)"##,
            to_vector,
        ),
    ]
}

#[cfg(test)]
mod tests;
