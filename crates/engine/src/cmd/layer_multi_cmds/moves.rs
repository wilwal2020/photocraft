use super::selection::{reselect, same_named_targets, selected, top_level};
use super::structure::check_group_depth;
use super::{Document, EngineError, Layer, LayerContent, LayerId, Result, Session, Value, bad, int_i32, json};

/// A Layers-panel drag of the selected layer records its drop target by name (and which of
/// several same-named layers it is), as [`recorded_selection`] does: the id means nothing in the
/// document the action plays in later, so playback failed with "no such layer" (#7845). Drags of
/// several layers, or of a layer that isn't the selected one, keep their ids.
pub(super) fn recorded_move(s: &Session, mut p: Value, pre_selected: Option<&[LayerId]>) -> Value {
    if p.get("layers").is_some() {
        return p;
    }
    let Some(st) = s.active() else { return p };
    let moved = match p.get("layer") {
        None => st.active_layer,
        Some(v) => match v.as_u64().map(LayerId) {
            Some(id) if pre_selected.map_or_else(|| st.selected_layers().as_slice() == [id], |sel| sel == [id]) => Some(id),
            _ => return p,
        },
    };
    if let Some(fields) = p.as_object_mut() {
        fields.remove("layer");
    }
    let Some(id) = p.get("target").and_then(Value::as_u64).map(LayerId) else { return p };
    let Some(layer) = st.doc.layer(id) else { return p };
    // A copy leaves its source where it is (it may even land beside it), so the source stays a
    // candidate; the new copy (the active layer once the step has run, which is when it is
    // recorded) is left out instead, as it doesn't exist yet when the step replays.
    let copy = p.get("copy").and_then(Value::as_bool).unwrap_or(false);
    let skip = if copy { st.active_layer.filter(|a| Some(*a) != moved) } else { moved };
    let matches = same_named_targets(&st.doc, &layer.name, skip.as_slice());
    let Some(occurrence) = matches.iter().position(|candidate| *candidate == id) else { return p };
    if let Some(fields) = p.as_object_mut() {
        fields.insert("target".into(), json!(layer.name));
        if matches.len() > 1 {
            fields.insert("targetOccurrence".into(), json!(occurrence));
        }
    }
    p
}

/// `layer.moveTo`'s drop target: an id, or a name with `targetOccurrence` choosing among layers
/// that share it (in document order, the `moved` layers aside; required when several do).
fn move_target(s: &Session, p: &Value, moved: &[LayerId]) -> Result<LayerId> {
    const CMD: &str = "layer.moveTo";
    match p.get("target") {
        Some(Value::String(name)) => {
            let occurrence = p
                .get("targetOccurrence")
                .map(|v| v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(CMD, "`targetOccurrence` must be a non-negative integer")))
                .transpose()?;
            let st = s.active().ok_or(EngineError::NoDocument)?;
            let matches = same_named_targets(&st.doc, name, moved);
            if matches.len() > 1 && occurrence.is_none() {
                return Err(bad(CMD, format!("multiple layers named `{name}`; specify `targetOccurrence`")));
            }
            let index = occurrence.unwrap_or(0);
            matches.get(index).copied().ok_or_else(|| bad(CMD, format!("no layer named `{name}` at occurrence {index}")))
        }
        Some(v) => v.as_u64().map(LayerId).ok_or_else(|| bad(CMD, "`target` must be a layer id or name")),
        None => Err(bad(CMD, "missing `target`")),
    }
}

// ---------- moving layers ----------

pub(super) fn link_groups(doc: &Document, ids: &[LayerId]) -> Vec<u64> {
    let mut g: Vec<u64> = ids.iter().filter_map(|id| doc.layer(*id)?.link_group).collect();
    g.sort_unstable();
    g.dedup();
    g
}

/// `ids` plus every layer linked to one of them.
pub(crate) fn with_links(doc: &Document, ids: &[LayerId]) -> Vec<LayerId> {
    let groups = link_groups(doc, ids);
    let mut out = ids.to_vec();
    if !groups.is_empty() {
        for (_, _, l) in doc.walk() {
            if l.link_group.is_some_and(|g| groups.contains(&g)) && !out.contains(&l.id) {
                out.push(l.id);
            }
        }
    }
    out
}

/// Move layers by whole pixels the way the Move tool does: pixels, linked masks, type and
/// vector content (shapes, linked vector masks) all move together. `before` is `doc` as it was
/// before any of the moves (the edit's starting document), read when type and shapes re-render.
pub(crate) fn move_layers(doc: &mut Document, before: &Document, moves: &[(LayerId, i32, i32)]) -> Result<()> {
    for &(id, dx, dy) in moves {
        if dx == 0 && dy == 0 {
            continue;
        }
        let locks = doc.effective_locks(id);
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if locks.position || locks.all {
            return Err(EngineError::Other(format!("layer \"{}\" is position-locked", l.name)));
        }
        crate::commands::translate_layer(before, l, dx, dy);
        crate::vector_cmds::translate_vectors(before, l, dx as f64, dy as f64);
    }
    Ok(())
}

/// Does a move aimed at the layer mask (`"target":"mask"`) move `l`'s mask alone? Only an
/// unlinked mask does (the chain off); a linked one moves with its layer, as in Photoshop.
pub fn moves_mask_alone(l: &Layer) -> bool {
    l.mask.as_ref().is_some_and(|m| !m.linked)
}

/// `layer.translate`: the explicit layer, or every selected layer, plus their linked layers.
/// With `"target":"mask"` a layer whose mask is unlinked moves that mask alone.
pub fn translate(s: &mut Session, p: &Value) -> Result<Value> {
    let (dx, dy) = match p.get("units").and_then(Value::as_str) {
        None | Some("px") => (int_i32("layer.translate", p, "dx")?.unwrap_or(0), int_i32("layer.translate", p, "dy")?.unwrap_or(0)),
        // Points (1/72 in), as a Photoshop action records a nudge: whole pixels at this
        // document's resolution.
        Some("pt") => {
            let dpi = s.active().map_or(72.0, |d| f64::from(d.doc.resolution_dpi));
            let to_px = |key: &str| -> Result<i32> {
                let pt = match p.get(key) {
                    None => 0.0,
                    Some(v) => v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| bad("layer.translate", format!("`{key}` must be a number")))?,
                };
                let px = (pt * dpi / 72.0).round();
                if px.abs() > f64::from(i32::MAX) {
                    return Err(bad("layer.translate", format!("`{key}` = {pt} pt is outside the 32-bit coordinate range")));
                }
                Ok(px as i32)
            };
            (to_px("dx")?, to_px("dy")?)
        }
        Some(other) => return Err(bad("layer.translate", format!("unknown units `{other}` (px|pt)"))),
    };
    let carry = match p.get("selection") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad("layer.translate", "`selection` must be a boolean"))?,
    };
    if dx == 0 && dy == 0 {
        return Ok(Value::Null);
    }
    let roots = match crate::commands::explicit_layer(s, p)? {
        Some(id) => vec![id],
        None => selected(s),
    };
    if roots.is_empty() {
        return Err(EngineError::Other("no active layer".into()));
    }
    let mask_target = p.get("target").and_then(Value::as_str) == Some("mask");
    let before = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    s.edit("Move", |doc, _| {
        let (alone, rest): (Vec<LayerId>, Vec<LayerId>) = roots.iter().partition(|id| mask_target && doc.layer(**id).is_some_and(moves_mask_alone));
        let mut ids = move_targets(doc, &rest);
        if ids.is_empty() && alone.is_empty() {
            return Err(EngineError::NoLayer(roots[0]));
        }
        let moves: Vec<_> = ids.iter().map(|&id| (id, dx, dy)).collect();
        move_layers(doc, &before, &moves)?;
        for id in alone {
            let locks = doc.effective_locks(id);
            let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
            if locks.position || locks.all {
                return Err(EngineError::Other(format!("layer \"{}\" is position-locked", l.name)));
            }
            if let Some(m) = &mut l.mask {
                m.surface = shift_surface(&m.surface, dx, dy);
            }
            ids.push(id);
        }
        // A board moved past any canvas edge grows the canvas, as a new one does (#1531, #3070).
        if ids.iter().any(|id| doc.layer(*id).is_some_and(|l| l.artboard().is_some())) {
            crate::artboard_cmds::fit_canvas(doc);
        }
        // The Move tool carries the selection along when it moves several layers whole, as
        // Photoshop does (#7538): one step, so Undo takes both back.
        if carry {
            doc.selection = doc.selection.as_ref().map(|sel| shift_surface(sel, dx, dy));
        }
        Ok(ids)
    })?;
    Ok(Value::Null)
}

/// The layers a move of `roots` takes along: their linked layers, without layers whose group
/// is also moving (the group carries them).
pub fn move_targets(doc: &Document, roots: &[LayerId]) -> Vec<LayerId> {
    top_level(doc, &with_links(doc, roots))
}

/// `doc` with the layers `ids` ([`move_targets`]) moved by whole pixels, as `layer.translate`
/// leaves them: the Move tool's live preview while it drags. Type, shape and smart-object pixels
/// shift as they are instead of re-rendering (the commit re-renders them), so a drag frame costs
/// copying the moving pixels only.
pub fn moved(doc: &Document, ids: &[LayerId], dx: i32, dy: i32) -> Result<Document> {
    moved_target(doc, ids, dx, dy, false)
}

/// [`moved`] for a drag aimed at the layer mask (`mask_target`): unlinked masks move alone.
pub fn moved_target(doc: &Document, ids: &[LayerId], dx: i32, dy: i32, mask_target: bool) -> Result<Document> {
    let mut out = doc.clone();
    for &id in ids {
        let locks = doc.effective_locks(id);
        let l = out.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        if locks.position || locks.all {
            return Err(EngineError::Other(format!("layer \"{}\" is position-locked", l.name)));
        }
        if mask_target && moves_mask_alone(l) {
            if let Some(m) = &mut l.mask {
                m.surface = shift_surface(&m.surface, dx, dy);
            }
            continue;
        }
        shift_shown(doc, l, dx, dy);
    }
    Ok(out)
}

/// `photocraft_algo::resample::translate_surface` with the content scan cached per tile (a Move
/// drag shifts the same layers every frame; scanning them each time cost more than the copy).
pub(crate) fn shift_surface(s: &photocraft_raster::Surface, dx: i32, dy: i32) -> photocraft_raster::Surface {
    s.translated(dx, dy, photocraft_compose::bounds::content_bounds(s))
}

/// [`crate::commands::translate_layer`] plus the vector side, without re-rendering anything.
fn shift_shown(doc: &Document, l: &mut Layer, dx: i32, dy: i32) {
    use self::shift_surface as translate_surface;
    let a = photocraft_geom::Affine::translate(f64::from(dx), f64::from(dy));
    if let Some(r) = &mut l.effects.reference {
        *r = (r.0 + f64::from(dx), r.1 + f64::from(dy));
    }
    for m in l.pixel_masks_mut().filter(|m| m.linked) {
        m.surface = translate_surface(&m.surface, dx, dy);
    }
    if let Some(vm) = l.vector_mask.as_mut().filter(|v| v.linked) {
        vm.path = vm.path.transform(&a);
    }
    match &mut l.content {
        LayerContent::Raster(s) => *s = translate_surface(s, dx, dy),
        LayerContent::Text(t) => {
            t.transform = a.mul(&t.transform);
            if let Some(c) = &mut t.cache {
                *c = translate_surface(c, dx, dy);
            }
        }
        LayerContent::Shape(sh) => crate::vector_cmds::shift_shape(doc, sh, f64::from(dx), f64::from(dy)),
        LayerContent::Smart(sm) => crate::smart_cmds::shift_smart(sm, dx, dy),
        LayerContent::Group(g) => {
            if let Some(ab) = &mut g.artboard {
                ab.rect = ab.rect.translate(dx, dy);
            }
            g.children.iter_mut().for_each(|c| shift_shown(doc, c, dx, dy));
        }
        LayerContent::Adjustment(_) | LayerContent::Fill(_) => {}
    }
}

/// One entry of Photoshop's flat layer list, bottom to top: a layer (a group's own entry sits
/// above its children) or the end of a group (below its children).
#[derive(Clone, Copy, Debug, PartialEq)]
enum StackItem {
    Layer(LayerId),
    GroupEnd(LayerId),
}

/// Photoshop's flat list of `layers` (bottom to top) without `skip` and what it holds.
fn stack_items(layers: &[Layer], skip: LayerId, out: &mut Vec<StackItem>, depth: usize) {
    if depth > MAX_STACK_DEPTH {
        return;
    }
    for l in layers {
        if l.id == skip {
            continue;
        }
        if let Some(children) = l.children() {
            out.push(StackItem::GroupEnd(l.id));
            stack_items(children, skip, out, depth + 1);
        }
        out.push(StackItem::Layer(l.id));
    }
}

/// How deep [`stack_items`] follows groups (documents can be deep; the cap bounds recursion).
pub(crate) const MAX_STACK_DEPTH: usize = 256;

/// `layer.moveTo` with `"index"`: Photoshop's Move to a layer index, what dragging a layer in the
/// Layers panel records in an action. The index counts positions from the bottom of the stack
/// from 0, a group's end counting as a position; the layer lands at that position of the stack
/// without it. Measured in Photoshop 2026 (`log/photoshop-compare/ps_move_index.ps1`): with a
/// Background, 0 is refused (nothing goes below it); one or two past the top is the top; further
/// is refused; an index inside a group's span puts the layer in the group.
fn move_to_index(s: &mut Session, p: &Value, index: &Value) -> Result<Value> {
    const CMD: &str = "layer.moveTo";
    if p.get("target").is_some() || p.get("layers").is_some() || p.get("copy").is_some_and(|c| c.as_bool() == Some(true)) {
        return Err(bad(CMD, "`index` moves one layer: give it without `target`, `layers` or `copy`"));
    }
    let n = index.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(CMD, "`index` must be a position from the bottom (0, 1, …)"))?;
    let moved = crate::commands::layer_param(s, p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc = &st.doc;
    if doc.layer(moved).is_none() {
        return Err(EngineError::NoLayer(moved));
    }
    let mut items = Vec::new();
    stack_items(&doc.layers, moved, &mut items, 0);
    let background = crate::background_cmds::background_id(doc);
    if n == 0 && background.is_some() {
        return Err(bad(CMD, "nothing goes below the Background (index 0)"));
    }
    if n > items.len().saturating_add(1) {
        return Err(bad(CMD, format!("index {n} is past the top of the stack ({} positions)", items.len())));
    }
    let (target, position) = match items.get(n) {
        // Past the top: above the top layer.
        None => match doc.layers.iter().rev().find(|l| l.id != moved) {
            Some(top) => (top.id, "above"),
            None => return Ok(Value::Null),
        },
        // Under a group's own entry: inside it, on top.
        Some(StackItem::Layer(id)) if doc.layer(*id).is_some_and(Layer::is_group) => (*id, "into"),
        Some(StackItem::Layer(id)) => (*id, "below"),
        // Under a group's end: below the group, beside it.
        Some(StackItem::GroupEnd(g)) => (*g, "below"),
    };
    move_to(s, &json!({"layer": moved.0, "target": target.0, "position": position}))
}

/// Reorder one or multiple layers to a single destination in one undoable edit.
///
/// Explicit `layers` are ordered according to the source document, not caller ordering.
/// A transient snapshot for the Layers panel's "Preview on canvas when reordering layers".
/// Uses the same source ordering and insertion semantics as `move_to`, but never edits history.
/// Invalid/self-referential hover targets return None (the committed image stays visible).
pub fn preview_move(doc: &Document, p: &Value, add_copy: bool) -> Option<Document> {
    let target = LayerId(p.get("target")?.as_u64()?);
    let position = p.get("position")?.as_str()?;
    if !matches!(position, "above" | "below" | "into") {
        return None;
    }
    let copy = p.get("copy").and_then(Value::as_bool).unwrap_or(false);
    let ids = if let Some(ids) = p.get("layers") {
        ids.as_array()?.iter().map(|v| v.as_u64().map(LayerId)).collect::<Option<Vec<_>>>()?
    } else {
        vec![LayerId(p.get("layer")?.as_u64()?)]
    };
    if ids.is_empty() || doc.layer(target).is_none() {
        return None;
    }
    let moved_ids = top_level(doc, &ids);
    if moved_ids.is_empty() || moved_ids.iter().any(|id| doc.layer(*id).is_none()) {
        return None;
    }
    if moved_ids.len() == 1 && moved_ids[0] == target && !copy {
        return None;
    }
    let target_path = doc.path_of(target)?;
    for id in &moved_ids {
        let path = doc.path_of(*id)?;
        if target_path.starts_with(&path) && !(copy && *id == target && position != "into") {
            return None;
        }
    }
    if position == "into" && !doc.layer(target)?.is_group() {
        return None;
    }
    // The very structural move the drop commits, so the preview shows the same clip groups,
    // Background handling and group-depth refusals; a drop `move_to` would refuse has no preview.
    let mut preview = doc.clone();
    apply_move(&mut preview, &moved_ids, target, position, copy, add_copy).ok()?;
    Some(preview)
}

/// The document-only part of `move_to`, shared with [`preview_move`] so the Layers panel's hover
/// preview cannot drift from what the drop commits. Moves (or, with `copy`, duplicates) the
/// top-level `moved_ids` to `pos` (`above`, `below` or `into`) `target`, then applies the
/// group-depth cap, the Background-stays-at-the-bottom rule and the clip-group rules
/// (`clip_group::settle`). Returns the ids of the new copies (empty for a move). On `Err` `doc` is
/// half edited: run it inside `Session::edit` or on a scratch copy.
fn apply_move(doc: &mut Document, moved_ids: &[LayerId], target: LayerId, pos: &str, copy: bool, add_copy: bool) -> Result<Vec<LayerId>> {
    const CMD: &str = "layer.moveTo";
    let background = crate::background_cmds::background_id(doc);
    let before = if copy { Default::default() } else { crate::clip_group::Before::read(doc, moved_ids) };
    let mut moved = Vec::with_capacity(moved_ids.len());
    for &id in moved_ids {
        moved.push(if copy { crate::commands::layer_copy(doc, id, add_copy)? } else { doc.remove(id).ok_or(EngineError::NoLayer(id))? });
    }
    let copies: Vec<LayerId> = if copy { moved.iter().map(|l| l.id).collect() } else { Vec::new() };
    // Look up the destination AFTER removing every source: its indices may have shifted.
    let path = doc.path_of(target).ok_or(EngineError::NoLayer(target))?;
    if pos == "into" {
        let children = doc.layer_at_mut(&path).and_then(Layer::children_mut).ok_or_else(|| bad(CMD, "target is not a group"))?;
        children.extend(moved);
    } else {
        let (&idx, parent) = path.split_last().ok_or(EngineError::NoLayer(target))?;
        let siblings = if parent.is_empty() {
            &mut doc.layers
        } else {
            doc.layer_at_mut(parent).and_then(Layer::children_mut).ok_or_else(|| bad(CMD, "target's parent is not a group"))?
        };
        let at = if pos == "below" { idx } else { idx + 1 };
        siblings.splice(at..at, moved);
    }
    check_group_depth(doc, "Reorder Layer")?;
    crate::background_cmds::keep_background_at_bottom(background, doc)?;
    crate::clip_group::settle(doc, if copy { &copies } else { moved_ids }, &before, false);
    Ok(copies)
}

/// Selected descendants of a moved group remain inside their parent; only top-level
/// selected nodes are detached, and every reference is validated before editing.
pub fn move_to(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layer.moveTo";
    if let Some(index) = p.get("index") {
        return move_to_index(s, p, index);
    }
    let pos = p.get("position").and_then(Value::as_str).unwrap_or("above");
    if !matches!(pos, "above" | "below" | "into") {
        return Err(bad(CMD, "position must be above, below, or into"));
    }
    // ⌥-drag in the Layers panel: duplicates land at the target and the layers stay put.
    let copy = p.get("copy").and_then(Value::as_bool).unwrap_or(false);

    let batch = p.get("layers");
    if batch.is_some() && p.get("layer").is_some() {
        return Err(bad(CMD, "give either `layer` or `layers`, not both"));
    }
    let requested = if let Some(raw) = batch {
        let arr = raw.as_array().filter(|a| !a.is_empty()).ok_or_else(|| bad(CMD, "`layers` must be a nonempty array of ids"))?;
        let mut ids = Vec::with_capacity(arr.len());
        for item in arr {
            let id = LayerId(item.as_u64().ok_or_else(|| bad(CMD, "every layer id must be an unsigned integer"))?);
            if ids.contains(&id) {
                return Err(bad(CMD, "duplicate layer id"));
            }
            ids.push(id);
        }
        ids
    } else {
        vec![crate::commands::layer_param(s, p)?]
    };
    // A copy moves nothing, so its source stays a candidate (⌥-dragging a layer beside itself).
    let target = move_target(s, p, if copy { &[] } else { &requested })?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc = &st.doc;
    let target_path = doc.path_of(target).ok_or(EngineError::NoLayer(target))?;
    for &id in &requested {
        if doc.layer(id).is_none() {
            return Err(EngineError::NoLayer(id));
        }
    }
    let moved_ids = top_level(doc, &requested);
    if moved_ids.len() == 1 && moved_ids[0] == target && batch.is_none() && !copy {
        // Preserve single-layer drag-to-self semantics.
        return Ok(Value::Null);
    }
    // Also rejects dragging a selected group onto any of its descendants.
    for id in &moved_ids {
        let path = doc.path_of(*id).ok_or(EngineError::NoLayer(*id))?;
        // A copy may land beside its own source (not inside it).
        let beside_itself = copy && *id == target && pos != "into";
        if target_path.starts_with(&path) && !beside_itself {
            return Err(bad(CMD, "target cannot be one of the moved layers or its descendant"));
        }
    }
    if pos == "into" && doc.layer(target).is_none_or(|l| !l.is_group()) {
        return Err(bad(CMD, "target is not a group"));
    }

    let original_active = st.active_layer;
    let add_copy = crate::commands::layer_panel_option(s, "addCopyToCopiedLayersAndGroups", true);
    let (count, copies) = s.edit(if copy { "Duplicate Layer" } else { "Reorder Layers" }, |doc, active| {
        let copies = apply_move(doc, &moved_ids, target, pos, copy, add_copy)?;
        *active = if copy {
            copies.last().copied()
        } else if batch.is_some() {
            original_active.filter(|id| requested.contains(id)).or_else(|| requested.last().copied())
        } else {
            moved_ids.first().copied()
        };
        Ok((moved_ids.len(), copies))
    })?;
    if copy {
        let ids: Vec<u64> = copies.iter().map(|id| id.0).collect();
        if batch.is_some() {
            let active = copies.last().copied();
            reselect(s, copies, active);
            return Ok(json!({"moved": count, "layers": ids}));
        }
        return Ok(json!({"layer": ids.first()}));
    }
    if batch.is_some() {
        let active = original_active.filter(|id| requested.contains(id)).or_else(|| requested.last().copied());
        reselect(s, requested, active);
        Ok(json!({"moved": count}))
    } else {
        Ok(Value::Null)
    }
}

/// `layer.moveTo` with Photoshop's layer index, checked against Photoshop 2026
/// (`log/photoshop-compare/ps_move_index.ps1`, 2026-10-18): the top layer X moved to each index.
#[cfg(test)]
mod move_index_tests {
    use super::*;

    /// Names bottom to top, a group as `G[children]`.
    fn stack(layers: &[Layer]) -> String {
        layers
            .iter()
            .map(|l| match l.children() {
                Some(c) => format!("{}[{}]", l.name, stack(c)),
                None => l.name.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn id(v: &Value) -> u64 {
        v["layer"].as_u64().unwrap()
    }

    /// Background (or a plain L0), L1…L4, X on top; with `group`, L1 and L2 in a group G below X.
    fn document(background: bool, group: bool) -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16, "background": if background { "white" } else { "transparent" }})).unwrap();
        if !background {
            let bottom = s.active().unwrap().doc.layers[0].id.0;
            s.execute("layer.setProps", json!({"layer": bottom, "name": "L0"})).unwrap();
        }
        let ids: Vec<u64> = (1..=4).map(|i| id(&s.execute("layer.new.layer", json!({"name": format!("L{i}")})).unwrap())).collect();
        if group {
            let g = id(&s.execute("layer.new.group", json!({"name": "G"})).unwrap());
            s.execute("layer.moveTo", json!({"layer": ids[1], "target": g, "position": "into"})).unwrap();
            s.execute("layer.moveTo", json!({"layer": ids[0], "target": g, "position": "into"})).unwrap();
            s.execute("layer.select", json!({"layer": g})).unwrap();
        }
        let x = id(&s.execute("layer.new.layer", json!({"name": "X"})).unwrap());
        let top = s.active().unwrap().doc.layers.last().unwrap().id.0;
        if top != x {
            s.execute("layer.moveTo", json!({"layer": x, "target": top, "position": "above"})).unwrap();
        }
        s
    }

    fn moved(background: bool, group: bool, index: u64) -> std::result::Result<String, String> {
        let mut s = document(background, group);
        s.execute("layer.moveTo", json!({"layer": "X", "index": index})).map_err(|e| e.to_string())?;
        Ok(stack(&s.active().unwrap().doc.layers))
    }

    #[test]
    fn with_a_background() {
        assert_eq!(stack(&document(true, false).active().unwrap().doc.layers), "Background L1 L2 L3 L4 X");
        assert!(moved(true, false, 0).is_err(), "nothing goes below the Background");
        assert_eq!(moved(true, false, 1).unwrap(), "Background X L1 L2 L3 L4");
        assert_eq!(moved(true, false, 2).unwrap(), "Background L1 X L2 L3 L4");
        assert_eq!(moved(true, false, 4).unwrap(), "Background L1 L2 L3 X L4");
        assert_eq!(moved(true, false, 5).unwrap(), "Background L1 L2 L3 L4 X");
        assert_eq!(moved(true, false, 6).unwrap(), "Background L1 L2 L3 L4 X", "one past the top is the top");
        assert!(moved(true, false, 7).is_err(), "further is refused");
    }

    #[test]
    fn without_a_background() {
        assert_eq!(moved(false, false, 0).unwrap(), "X L0 L1 L2 L3 L4");
        assert_eq!(moved(false, false, 1).unwrap(), "L0 X L1 L2 L3 L4");
        assert_eq!(moved(false, false, 4).unwrap(), "L0 L1 L2 L3 X L4");
        assert_eq!(moved(false, false, 6).unwrap(), "L0 L1 L2 L3 L4 X");
        assert!(moved(false, false, 7).is_err());
    }

    /// A group's end counts as a position: indexes inside its span put the layer in the group.
    #[test]
    fn a_group_takes_positions_for_its_end_and_itself() {
        assert_eq!(stack(&document(true, true).active().unwrap().doc.layers), "Background L3 L4 G[L2 L1] X");
        assert_eq!(moved(true, true, 1).unwrap(), "Background X L3 L4 G[L2 L1]");
        assert_eq!(moved(true, true, 3).unwrap(), "Background L3 L4 X G[L2 L1]");
        assert_eq!(moved(true, true, 4).unwrap(), "Background L3 L4 G[X L2 L1]");
        assert_eq!(moved(true, true, 5).unwrap(), "Background L3 L4 G[L2 X L1]");
        assert_eq!(moved(true, true, 6).unwrap(), "Background L3 L4 G[L2 L1 X]");
        assert_eq!(moved(true, true, 7).unwrap(), "Background L3 L4 G[L2 L1] X");
        assert_eq!(moved(true, true, 8).unwrap(), "Background L3 L4 G[L2 L1] X");
    }

    #[test]
    fn bad_index_params_fail_gracefully() {
        let mut s = document(false, false);
        let before = stack(&s.active().unwrap().doc.layers);
        for p in [
            json!({"index": -1}),
            json!({"index": 1.5}),
            json!({"index": "2"}),
            json!({"index": u64::MAX}),
            json!({"index": 1, "target": 1}),
            json!({"index": 1, "layers": [1]}),
            json!({"index": 1, "copy": true}),
            json!({"index": 1, "layer": "nope"}),
        ] {
            assert!(s.execute("layer.moveTo", p.clone()).is_err(), "{p}");
        }
        assert_eq!(stack(&s.active().unwrap().doc.layers), before, "a refused move changes nothing");
    }
}
