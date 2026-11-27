use super::selection::{reselect, selected, top_level};
use super::{Document, EngineError, Layer, LayerContent, LayerId, PixelFormat, Result, Session, Value, bad, json};

// ---------- structural commands ----------

/// Set an explicit link state idempotently, or toggle when omitted, with singleton cleanup and Undo.
pub(super) fn link_layers(s: &mut Session, p: &Value) -> Result<Value> {
    let requested = match p.get("linked") {
        None => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err(bad("layer.linkLayers", "`linked` must be true or false")),
    };
    let sel = selected(s);
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let groups = sel.iter().map(|id| doc.layer(*id).map(|l| l.link_group).ok_or(EngineError::NoLayer(*id))).collect::<Result<Vec<_>>>()?;
    let first = groups.first().copied().flatten();
    let same_group = first.is_some() && groups.iter().all(|group| *group == first);
    // A recorded Link/Unlink is an operation, not the UI's state-dependent toggle.
    // An explicit repeat must neither allocate a group nor add an undo entry.
    let linked_group = same_group && doc.walk().iter().filter(|(_, _, layer)| layer.link_group == first).take(2).count() == 2;
    if requested == Some(true) && linked_group || requested == Some(false) && groups.iter().all(Option::is_none) {
        return Ok(json!({"linked": requested == Some(true)}));
    }
    let label = if requested == Some(false) { "Unlink Layers" } else { "Link Layers" };
    let linked = s.edit(label, |doc, _| {
        // Already one link group (or a single linked layer): unlink, like Photoshop's toggle.
        let unlink = requested.map_or(same_group, |linked| !linked) || sel.len() < 2;
        // A loaded file can store any id up to u64::MAX, so the new id must not be a bare
        // max + 1 (#961). Unlinking needs no new id at all.
        let next = if unlink { None } else { Some(doc.unused_link_group().ok_or_else(|| bad("layer.linkLayers", "no free link group id is left"))?) };
        for id in &sel {
            let l = doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?;
            l.link_group = next;
        }
        // A group left with a single member is no longer a link.
        let mut counts = std::collections::HashMap::<u64, usize>::new();
        for (_, _, l) in doc.walk() {
            if let Some(g) = l.link_group {
                *counts.entry(g).or_default() += 1;
            }
        }
        let lonely: Vec<LayerId> =
            doc.walk().iter().filter(|(_, _, l)| l.link_group.is_some_and(|g| counts.get(&g).is_some_and(|&c| c < 2))).map(|(_, _, l)| l.id).collect();
        for id in lonely {
            if let Some(l) = doc.layer_mut(id) {
                l.link_group = None;
            }
        }
        Ok(!unlink)
    })?;
    Ok(json!({"linked": linked}))
}

pub(super) fn lock_layers(s: &mut Session, p: &Value) -> Result<Value> {
    let sel = selected(s);
    let keys = ["transparency", "pixels", "position", "artboard", "all", "selection"];
    let given: Vec<(&str, bool)> = keys.iter().filter_map(|k| Some((*k, p.get(*k)?.as_bool()?))).collect();
    s.edit("Lock Layers", |doc, _| {
        // No explicit locks: toggle "lock all" (on unless every selected layer is already locked).
        let all_locked = sel.iter().all(|id| doc.layer(*id).is_some_and(|l| l.locks.all));
        let given: Vec<(&str, bool)> = if given.is_empty() { vec![("all", !all_locked)] } else { given.clone() };
        for id in &sel {
            let l = doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?;
            for (k, v) in &given {
                l.locks.set(k, *v);
            }
        }
        Ok(())
    })?;
    Ok(json!({"layers": sel.len()}))
}

pub(super) fn rename_layer(s: &mut Session, p: &Value) -> Result<Value> {
    let name = p.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty()).ok_or_else(|| bad("layer.renameLayer", "missing `name`"))?.to_string();
    let id = crate::commands::layer_param(s, p)?;
    s.edit("Rename Layer", |doc, _| {
        doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?.name.clone_from(&name);
        Ok(())
    })?;
    crate::type_cmds::note_typed_name(s, id, &name);
    Ok(Value::Null)
}

pub(super) fn reverse(s: &mut Session) -> Result<Value> {
    let sel = selected(s);
    s.edit("Reverse", |doc, _| {
        let background = crate::background_cmds::background_id(doc);
        let ids = top_level(doc, &sel);
        if ids.len() < 2 {
            return Err(EngineError::Other("select two or more layers to reverse".into()));
        }
        let paths: Vec<Vec<usize>> = ids.iter().map(|id| doc.path_of(*id).ok_or(EngineError::NoLayer(*id))).collect::<Result<_>>()?;
        let layers: Vec<Layer> = ids.iter().map(|id| doc.layer(*id).cloned().ok_or(EngineError::NoLayer(*id))).collect::<Result<_>>()?;
        // Slots are disjoint (no layer is nested in another), so writing whole layers back to the
        // same paths never shifts another slot.
        for (path, l) in paths.iter().zip(layers.into_iter().rev()) {
            *doc.layer_at_mut(path).ok_or_else(|| EngineError::Other("layer tree changed".into()))? = l;
        }
        crate::background_cmds::keep_background_at_bottom(background, doc)
    })?;
    Ok(Value::Null)
}

/// Refuses an edit that left `doc` nested deeper than [`photocraft_doc::MAX_GROUP_DEPTH`]
/// groups. Called at the end of a `Session::edit` closure, so an `Err` leaves the document and
/// history untouched.
pub(crate) fn check_group_depth(doc: &Document, what: &str) -> Result<()> {
    if doc.max_group_depth() > photocraft_doc::MAX_GROUP_DEPTH {
        return Err(EngineError::Other(format!("{what} would nest layers deeper than {} groups", photocraft_doc::MAX_GROUP_DEPTH)));
    }
    Ok(())
}

/// Group Layers (⌘G) / Group from Layers: the explicit layer, or every selected layer, moves
/// into a new, collapsed group placed where the top-most of them was. Bottom-to-top order is
/// preserved. The New Group dialog's colour, mode and opacity are set in the same history step.
pub fn group_layers(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = match crate::commands::explicit_layer(s, p)? {
        Some(id) => vec![id],
        None => selected(s),
    };
    if ids.is_empty() {
        return Err(EngineError::Other("no layer selected".into()));
    }
    let opts = crate::group_options::GroupOptions::parse("layer.groupLayers", p)?;
    let gid = s.edit("Group Layers", |doc, active| {
        let ids = top_level(doc, &ids);
        // Artboards stay at the top level: inside a group they would stop being boards.
        if ids.iter().any(|id| doc.layer(*id).is_some_and(|l| l.artboard().is_some())) {
            return Err(EngineError::Other("artboards can't go into a group".into()));
        }
        let top = *ids.last().ok_or(EngineError::NoLayer(LayerId(0)))?;
        let name = opts.name.clone().unwrap_or_else(|| doc.next_layer_name("Group"));
        // Photoshop creates the group collapsed, so the panel shows one row in place of the layers.
        let mut group = Layer::group(name, vec![]);
        if let LayerContent::Group(g) = &mut group.content {
            g.expanded = false;
        }
        let gid = doc.insert_above(Some(top), group);
        let mut children = Vec::with_capacity(ids.len());
        for id in &ids {
            children.push(doc.remove(*id).ok_or(EngineError::NoLayer(*id))?);
        }
        let group = doc.layer_mut(gid).ok_or(EngineError::NoLayer(gid))?;
        *group.children_mut().ok_or(EngineError::NoLayer(gid))? = children;
        opts.apply(group);
        check_group_depth(doc, "Group Layers")?;
        *active = Some(gid);
        Ok(gid)
    })?;
    reselect(s, vec![gid], Some(gid));
    Ok(json!({ "layer": gid.0 }))
}

pub(super) fn merge_layers(s: &mut Session) -> Result<Value> {
    let sel = selected(s);
    if sel.len() < 2 {
        // ⌘E on a lone group is Photoshop's Merge Group (#3832); on a layer, Merge Down.
        if let Some(&id) = sel.first()
            && s.active().and_then(|d| d.doc.layer(id)).is_some_and(|l| matches!(l.content, LayerContent::Group(_)))
        {
            return s.execute("layer.mergeGroup", json!({ "layer": id.0 }));
        }
        return s.execute("layer.mergeDown", json!({}));
    }
    let mid = s.edit("Merge Layers", |doc, active| {
        let ids = top_level(doc, &sel);
        let lowest_id = *ids.first().ok_or_else(|| EngineError::Other("Merge Layers needs two or more layers".into()))?;
        let is_background = doc.is_background(lowest_id);

        let drawn: Vec<LayerId> = ids.iter().copied().filter(|&id| draws(doc, id)).collect();
        if drawn.is_empty() {
            return Err(EngineError::Other("the selected layers are all hidden".into()));
        }
        // Clipped layers of one chain merge into a layer still clipped to their base. Clipped
        // layers of different chains don't: the result, at the top one's slot, would clip the
        // lower chain's pixels to the upper chain's base.
        let clip_to = drawn.first().and_then(|&id| crate::clip_group::base_of(doc, id)).filter(|base| {
            !ids.contains(base) && drawn.iter().all(|&id| doc.layer(id).is_some_and(|l| l.clipped) && crate::clip_group::base_of(doc, id) == Some(*base))
        });
        let solo = merge_source(doc, &ids, clip_to);

        if is_background {
            let base = doc.layer(lowest_id).ok_or(EngineError::NoLayer(lowest_id))?.clone();
            let fmt = doc.pixel_format();
            let mut merged = Layer::raster(base.name.clone(), fmt);
            merged.locks = base.locks;
            merged.label = base.label;
            merged.id = lowest_id;
            merged.blend = base.blend;
            merged.opacity = 1.0;
            *crate::pixels_mut(&mut merged)? = crate::pixels::composite_layers(&solo, fmt, None);
            for id in &ids[1..] {
                doc.remove(*id);
            }
            *doc.layer_mut(lowest_id).ok_or(EngineError::NoLayer(lowest_id))? = merged;
            *active = Some(lowest_id);
            Ok(lowest_id)
        } else {
            let top_id = *ids.last().ok_or_else(|| EngineError::Other("Merge Layers needs two or more layers".into()))?;
            let top = doc.layer(top_id).ok_or(EngineError::NoLayer(top_id))?.clone();
            let fmt = doc.pixel_format();
            let fmt = PixelFormat::new(fmt.mode, fmt.sample, true);
            let mut merged = Layer::raster(top.name.clone(), fmt);
            merged.locks = top.locks;
            merged.label = top.label;
            merged.clipped = clip_to.is_some();
            *crate::pixels_mut(&mut merged)? = crate::pixels::composite_layers_uncropped(&solo, fmt);
            let mid = merged.id;
            // Hidden selected layers are discarded, as in Photoshop (so are those that show nothing:
            // in a hidden group, or clipped to a hidden base).
            for id in &ids[..ids.len() - 1] {
                doc.remove(*id);
            }
            *doc.layer_mut(top_id).ok_or(EngineError::NoLayer(top_id))? = merged;
            *active = Some(mid);
            Ok(mid)
        }
    })?;
    reselect(s, vec![mid], Some(mid));
    Ok(json!({ "layer": mid.0 }))
}

/// Whether layer `id` shows on the canvas: it and every group around it are visible, and when it is
/// clipped, so is its base (a hidden base hides its clipped layers).
fn draws(doc: &Document, id: LayerId) -> bool {
    crate::hidden_target::shown(doc, id)
        && (!doc.layer(id).is_some_and(|l| l.clipped) || crate::clip_group::base_of(doc, id).is_none_or(|base| crate::hidden_target::shown(doc, base)))
}

/// What Merge Layers composites: the document with only the layers `ids` (and what they contain)
/// showing, so the result looks as they do on screen. A layer in a hidden group or clipped to a
/// hidden base adds nothing, and the opacity, mask and blend mode of a group around them still
/// apply. A layer clipped to a base outside `ids` stays clipped to that base's shape without
/// drawing the base itself (its fill and effects off, its clipped layers blended one by one), unless the result will be clipped to that
/// base (`clip_to`): then it is drawn unclipped, so the base's shape isn't applied twice.
fn merge_source(doc: &Document, ids: &[LayerId], clip_to: Option<LayerId>) -> Document {
    let walk = doc.walk();
    let picked: Vec<_> = walk.iter().filter(|(_, _, l)| ids.contains(&l.id)).map(|(p, _, _)| p.clone()).collect();
    // The picked layers, what they contain, and the groups around them.
    let mut keep: Vec<LayerId> = walk.iter().filter(|(p, _, _)| picked.iter().any(|q| p.starts_with(q) || q.starts_with(p))).map(|(_, _, l)| l.id).collect();
    let mut solo = doc.clone();
    for &id in ids {
        if !draws(doc, id) || !doc.layer(id).is_some_and(|l| l.clipped) {
            continue;
        }
        let Some(base) = crate::clip_group::base_of(doc, id).filter(|base| !ids.contains(base)) else { continue };
        if Some(base) == clip_to {
            if let Some(l) = solo.layer_mut(id) {
                l.clipped = false;
            }
        } else if let Some(b) = solo.layer_mut(base) {
            // Clipped one by one, the layers keep to the base's alpha × opacity, which ignores its
            // fill: with fill 0 the base itself draws nothing.
            b.fill_opacity = 0.0;
            b.effects.enabled = false;
            b.advanced.blend_clipped = false;
            keep.push(base);
        }
    }
    for (_, _, l) in &walk {
        if !keep.contains(&l.id)
            && let Some(hidden) = solo.layer_mut(l.id)
        {
            hidden.visible = false;
        }
    }
    solo
}

/// Next visible layer row below the deleted target, then rows above in reverse order.
/// Group headers precede their children in the UI, unlike Document::walk's storage order.
pub(crate) fn deletion_neighbours(doc: &Document, target: LayerId) -> Vec<LayerId> {
    let rows = |expand_all: bool| {
        let mut stack: Vec<&Layer> = doc.layers.iter().collect();
        let mut order = Vec::new();
        while let Some(layer) = stack.pop() {
            order.push(layer.id);
            if let LayerContent::Group(group) = &layer.content
                && (expand_all || group.expanded)
            {
                stack.extend(group.children.iter());
            }
        }
        order
    };
    let mut order = rows(false);
    if !order.contains(&target) {
        order = rows(true);
    }
    let Some(index) = order.iter().position(|id| *id == target) else {
        return Vec::new();
    };
    order.iter().skip(index + 1).chain(order.iter().take(index).rev()).copied().collect()
}

/// Delete Layer with several layers selected.
pub fn delete_selected(s: &mut Session) -> Result<Value> {
    let sel = selected(s);
    s.edit("Delete Layers", |doc, active| {
        let ids = top_level(doc, &sel);
        let neighbours = active.map(|id| deletion_neighbours(doc, id)).unwrap_or_default();
        let before = crate::clip_group::Before::read(doc, &ids);
        for id in &ids {
            doc.remove(*id).ok_or(EngineError::NoLayer(*id))?;
        }
        crate::clip_group::settle(doc, &[], &before, false);
        if doc.layers.is_empty() {
            return Err(EngineError::Other("a document must keep at least one layer".into()));
        }
        *active = neighbours.into_iter().find(|candidate| doc.layer(*candidate).is_some());
        Ok(())
    })?;
    Ok(json!({"deleted": sel.len()}))
}

/// `ids` in Layers panel order, topmost row first: a group's row comes before its contents.
fn panel_order(doc: &Document, ids: &[LayerId]) -> Vec<LayerId> {
    fn rec(layers: &[Layer], ids: &[LayerId], out: &mut Vec<LayerId>) {
        for l in layers.iter().rev() {
            if ids.contains(&l.id) {
                out.push(l.id);
            }
            if let LayerContent::Group(g) = &l.content {
                rec(&g.children, ids, out);
            }
        }
    }
    let mut out = Vec::with_capacity(ids.len());
    rec(&doc.layers, ids, &mut out);
    out
}

/// Duplicate Layer with several layers selected: like Photoshop, the copies go together, in their
/// original order, directly above the topmost selected layer, and become the selection. Artboard
/// copies go beside their boards unless `in_place` ([`crate::artboard_cmds::place_copy`]).
/// `label` names the history step (Layer via Copy reuses this path, #2777).
pub fn duplicate_selected(s: &mut Session, in_place: bool, label: &str) -> Result<Value> {
    let sel = selected(s);
    let old_active = s.active().and_then(|d| d.active_layer);
    let add_copy = crate::commands::layer_panel_option(s, "addCopyToCopiedLayersAndGroups", true);
    let mut shift = (0i32, 0i32);
    let (copies, active) = s.edit(label, |doc, active| {
        let mut copies = Vec::new();
        let mut new_active = None;
        let order = panel_order(doc, &top_level(doc, &sel));
        // Each copy lands above the previous one, starting above the topmost original, so the
        // bottom-most original's copy ends up lowest in the block.
        let mut anchor = order.first().copied();
        for &id in order.iter().rev() {
            // Match the single-layer duplicate path: Background copies become
            // ordinary layers, while normal copies retain their original locks.
            let dup = crate::commands::layer_copy(doc, id, add_copy)?;
            let nid = doc.insert_above(anchor.or(Some(id)), dup);
            anchor = Some(nid);
            if !in_place {
                // Each copy's shift moved the whole document, so they add up.
                let (dx, dy) = crate::artboard_cmds::place_copy(doc, nid)?;
                shift = (shift.0.saturating_add(dx), shift.1.saturating_add(dy));
            }
            if Some(id) == old_active {
                new_active = Some(nid);
            }
            copies.push(nid);
        }
        relink_copies(doc, &copies)?;
        let a = new_active.or(copies.last().copied());
        *active = a;
        Ok((copies, a))
    })?;
    crate::artboard_cmds::follow_shift(s, shift);
    let out = copies.iter().map(|l| l.0).collect::<Vec<_>>();
    reselect(s, copies, active);
    Ok(json!({"layers": out, "layer": active.map(|l| l.0)}))
}

/// Link groups of fresh copies, as the reference application does (measured, #11217): a copy of
/// one layer alone (Duplicate, Layer via Copy) joins its original's link group. With several
/// layers copied, the copies of several layers of one link group are linked to each other under a
/// group of their own, as `layer_copy_cmds::adapt_copies` remaps copies landing in another
/// document, and a copy that is the only one from its link group is unlinked. `copies` are the
/// copies just inserted in `doc` (the layers in copied groups are remapped with them); the
/// originals keep their groups.
pub(crate) fn relink_copies(doc: &mut Document, copies: &[LayerId]) -> Result<()> {
    // One layer copied on its own keeps the link group it was copied with.
    if let [only] = copies
        && doc.layer(*only).is_some_and(|l| !matches!(l.content, LayerContent::Group(_)))
    {
        return Ok(());
    }
    // Every copied layer with the link group it was copied with. An explicit stack rather than
    // recursion: documents can nest deeply.
    let mut linked: Vec<(LayerId, u64)> = Vec::new();
    let mut stack: Vec<&Layer> = copies.iter().filter_map(|id| doc.layer(*id)).collect();
    while let Some(l) = stack.pop() {
        if let Some(group) = l.link_group {
            linked.push((l.id, group));
        }
        if let LayerContent::Group(g) = &l.content {
            stack.extend(g.children.iter());
        }
    }
    let mut groups: Vec<u64> = linked.iter().map(|(_, group)| *group).collect();
    groups.sort_unstable();
    groups.dedup();
    for group in groups {
        let members: Vec<LayerId> = linked.iter().filter(|(_, g)| *g == group).map(|(id, _)| *id).collect();
        // The id is picked after the previous group's copies took theirs, so no two share one.
        let fresh =
            if members.len() < 2 { None } else { Some(doc.unused_link_group().ok_or_else(|| EngineError::Other("no free link group id is left".into()))?) };
        for id in members {
            if let Some(l) = doc.layer_mut(id) {
                l.link_group = fresh;
            }
        }
    }
    Ok(())
}

/// Show/hide every selected layer in one step.
pub fn set_visible_selected(s: &mut Session, visible: bool) -> Result<Value> {
    let sel = selected(s);
    s.edit_visibility(if visible { "Show Layers" } else { "Hide Layers" }, |doc, _| {
        for id in &sel {
            doc.layer_mut(*id).ok_or(EngineError::NoLayer(*id))?.visible = visible;
            if visible {
                crate::extra_cmds::show_enclosing_groups(doc, *id);
            }
        }
        Ok(())
    })?;
    Ok(Value::Null)
}
