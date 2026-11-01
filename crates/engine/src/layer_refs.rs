//! Layers named in recorded action steps. The Layers panel, the canvas and agents name a layer by
//! its id, which means nothing in another document (and the same id is another layer there), so a
//! recorded step that keeps it changes some other layer, or fails, when the action plays on
//! another document. Recording names it as Photoshop does instead: `{"current": true}` for the
//! layer that was current, `{"name": "Ink"}` (with `"occurrence"` among layers of that name, from
//! the bottom, when several share it) for another; dispatch turns those back into the ids of the
//! document the step runs on, so every command still reads a plain id.
//!
//! With Relative Layer Recording on (the Actions panel menu, user request) no names are kept: a
//! layer other than the current one is recorded as how many Layers panel rows it is above the
//! current layer (`{"relative": 2}`; below is negative), counted as ⌥[ / ⌥] count them, so the
//! action repeats what was done wherever the current layer is.

use photocraft_doc::{Document, LayerId};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

/// The params of `id` that name a layer by id. A relative recording names the layer a click
/// selects as well (`relative`).
fn keys(id: &str, relative: bool) -> &'static [&'static str] {
    match id {
        "layer.moveTo" => &["layer", "layers", "target"],
        "layer.layerStyle.transferEffects" | "layer.layerMask.transfer" | "layer.smartFilter.transfer" => &["from", "to"],
        "edit.autoAlignLayers" => &["layers", "reference"],
        "type.attachToPath" => &["layer", "layers", "shape"],
        "shape.presets.place" => &["layer", "layers", "addTo"],
        // Recorded by name and occurrence already (`layer_multi_cmds::recorded_selection`).
        "layer.select" if relative => &["layer"],
        "layer.select" => &[],
        _ if id.starts_with("actions.") => &[],
        _ => &["layer", "layers"],
    }
}

/// Steps that act on the current layer without a `layer`: theirs is left out for the current
/// layer, Photoshop's "Set Current Layer" (an eye ⌥-click, a rename).
fn current_by_default(id: &str) -> bool {
    matches!(id, "layer.setProps" | "layer.showOnly" | "edit.transform")
}

/// Steps that always act on the current layer when an action plays (user report: an action's
/// Show Only must show the layer you're on, not the one ⌥-clicked while recording): recorded
/// without a layer, and a layer an older recording named is ignored.
fn current_only(id: &str) -> bool {
    id == "layer.showOnly"
}

fn reference(doc: &Document, active: Option<LayerId>, id: LayerId, relative: bool) -> Option<Value> {
    if Some(id) == active {
        return Some(json!({"current": true}));
    }
    // A layer that is no row of its own (inside a collapsed group), or with no current layer to
    // count from, is named after all.
    if relative && let Some(rows) = rows_from_current(doc, active, id) {
        return Some(json!({"relative": rows}));
    }
    let name = &doc.layer(id)?.name;
    let same: Vec<LayerId> = doc.walk().into_iter().filter(|(_, _, l)| l.name == *name).map(|(_, _, l)| l.id).collect();
    let occurrence = same.iter().position(|l| *l == id)?;
    Some(if same.len() > 1 { json!({"name": name, "occurrence": occurrence}) } else { json!({"name": name}) })
}

/// How many Layers panel rows `id` is above the current layer (below: negative).
fn rows_from_current(doc: &Document, active: Option<LayerId>, id: LayerId) -> Option<i64> {
    let rows = crate::layer_nav_cmds::panel_rows(doc);
    let target = rows.iter().position(|r| *r == id)?;
    let current = crate::layer_nav_cmds::row_of(doc, &rows, active?)?;
    Some(i64::try_from(current).ok()? - i64::try_from(target).ok()?)
}

/// A step's params as recorded, read before it runs (a rename changes the name): its layer ids
/// as references. `None`: recorded as given.
pub(crate) fn record(s: &Session, id: &str, p: &Value) -> Option<Value> {
    let st = s.active()?;
    let relative = s.actions.prefs.relative_layers;
    let keys = keys(id, relative);
    // A Free Transform of the selected layers leaves its `layers` out, so it plays on the layers
    // selected then (`layer_multi_cmds::recorded_step`, #6501); only other layers are named.
    let mut out = if id == "edit.transform" { crate::layer_multi_cmds::recorded_step(s, id, p.clone(), None) } else { p.clone() };
    let fields = out.as_object_mut()?;
    let mut changed = false;
    for key in keys {
        let Some(v) = fields.get(*key) else { continue };
        let new = match v {
            Value::Number(n) => n.as_u64().and_then(|l| reference(&st.doc, st.active_layer, LayerId(l), relative)),
            // Every element a layer id, or the list stays as it is.
            Value::Array(a) => {
                let refs = a.iter().map(|e| e.as_u64().and_then(|l| reference(&st.doc, st.active_layer, LayerId(l), relative)));
                refs.collect::<Option<Vec<_>>>().map(Value::Array)
            }
            _ => None,
        };
        if let Some(new) = new {
            fields.insert((*key).to_string(), new);
            changed = true;
        }
    }
    if current_only(id) {
        let named = fields.remove("layer").is_some() | fields.remove("layers").is_some();
        return (changed || named).then_some(out);
    }
    if current_by_default(id) {
        let current = json!({"current": true});
        if fields.get("layer") == Some(&current) {
            fields.remove("layer");
        }
        if fields.get("layers").and_then(Value::as_array).is_some_and(|a| a.len() == 1 && a.first() == Some(&current)) {
            fields.remove("layers");
        }
    }
    changed.then_some(out)
}

fn resolve_one(s: &Session, cmd: &str, v: &Value) -> Result<Option<LayerId>> {
    let Some(o) = v.as_object() else { return Ok(None) };
    let bad = |msg: String| EngineError::BadParams { cmd: cmd.to_string(), msg };
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if o.get("current").and_then(Value::as_bool) == Some(true) {
        return st.active_layer.map(Some).ok_or_else(|| bad("no current layer".into()));
    }
    if let Some(n) = o.get("relative") {
        let n = n.as_i64().ok_or_else(|| bad(format!("`relative` must be a whole number of rows, got {n}")))?;
        let current = st.active_layer.ok_or_else(|| bad("no current layer to count the rows from".into()))?;
        let rows = crate::layer_nav_cmds::panel_rows(&st.doc);
        let from = crate::layer_nav_cmds::row_of(&st.doc, &rows, current).and_then(|r| i64::try_from(r).ok());
        let row = from.and_then(|r| r.checked_sub(n)).and_then(|r| usize::try_from(r).ok()).and_then(|r| rows.get(r));
        return row.map(|l| Some(*l)).ok_or_else(|| bad(no_row(n)));
    }
    let Some(name) = o.get("name") else { return Ok(None) };
    let name = name.as_str().ok_or_else(|| bad(format!("a layer's `name` must be a string, got {name}")))?;
    let occurrence = match o.get("occurrence") {
        None => None,
        Some(n) => Some(n.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(format!("`occurrence` must be a non-negative integer, got {n}")))?),
    };
    let same: Vec<LayerId> = st.doc.walk().into_iter().filter(|(_, _, l)| l.name == name).map(|(_, _, l)| l.id).collect();
    // User report: a step recorded while two layers shared a name failed on a document with one.
    // Like Photoshop, a name with no such occurrence (or none given) means the top-most layer of
    // that name.
    occurrence.and_then(|i| same.get(i)).or_else(|| same.last()).map(|l| Some(*l)).ok_or_else(|| bad(format!("no layer named \"{name}\"")))
}

/// Why a relative reference found no layer: the stack ends before that row.
fn no_row(n: i64) -> String {
    let (count, way) = (n.unsigned_abs(), if n > 0 { "above" } else { "below" });
    let rows = if count == 1 { "row" } else { "rows" };
    format!("there is no layer {count} {rows} {way} the current one in the Layers panel")
}

/// Dispatch: the layer references of a recorded step back to the layer ids of the active
/// document. Params without any are returned as they are.
pub(crate) fn resolve(s: &Session, id: &str, mut p: Value) -> Result<Value> {
    let keys = keys(id, true);
    let Some(fields) = p.as_object_mut() else { return Ok(p) };
    if current_only(id) {
        // Only a recorded reference: a plain id is a live ⌥-click on that layer's eye.
        for key in ["layer", "layers"] {
            let recorded = |v: &Value| v.is_object() || v.as_array().is_some_and(|a| a.iter().any(Value::is_object));
            if fields.get(key).is_some_and(recorded) {
                fields.remove(key);
            }
        }
        return Ok(p);
    }
    for key in keys {
        let Some(v) = fields.get(*key) else { continue };
        let new = match v {
            Value::Object(_) => resolve_one(s, id, v)?.map(|l| json!(l.0)),
            Value::Array(a) if a.iter().any(Value::is_object) => {
                let mut ids = Vec::with_capacity(a.len());
                for e in a {
                    ids.push(match resolve_one(s, id, e)? {
                        Some(l) => json!(l.0),
                        None => e.clone(),
                    });
                }
                Some(Value::Array(ids))
            }
            _ => None,
        };
        if let Some(new) = new {
            fields.insert((*key).to_string(), new);
        }
    }
    Ok(p)
}

/// A recorded layer reference, as the Actions panel shows it.
#[derive(Debug, PartialEq, Eq)]
pub enum Described {
    Current,
    /// The layer's name, with its occurrence when several share it: "Twin (2)".
    Named(String),
    /// Rows above the current layer in the Layers panel (below: negative).
    Relative(i64),
}

/// How a recorded reference reads in the Actions panel. `None` for anything else.
pub fn describe(v: &Value) -> Option<Described> {
    let o = v.as_object()?;
    if o.get("current").and_then(Value::as_bool) == Some(true) {
        return Some(Described::Current);
    }
    if let Some(n) = o.get("relative") {
        return n.as_i64().map(Described::Relative);
    }
    let name = o.get("name")?.as_str()?;
    Some(Described::Named(match o.get("occurrence").and_then(Value::as_u64) {
        Some(n) if n > 0 => format!("{name} ({})", n + 1),
        _ => name.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> (Session, LayerId, LayerId, LayerId) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        s.execute("layer.new.layer", json!({"name": "A"})).unwrap();
        let a = s.active().unwrap().active_layer.unwrap();
        s.execute("layer.new.layer", json!({"name": "Twin"})).unwrap();
        let t1 = s.active().unwrap().active_layer.unwrap();
        s.execute("layer.new.layer", json!({"name": "Twin"})).unwrap();
        let t2 = s.active().unwrap().active_layer.unwrap();
        (s, a, t1, t2)
    }

    #[test]
    fn ids_are_recorded_as_the_current_layer_or_by_name() {
        let (s, a, t1, t2) = session();
        assert_eq!(record(&s, "layer.hideLayers", &json!({"layer": t2.0})), Some(json!({"layer": {"current": true}})));
        assert_eq!(record(&s, "layer.hideLayers", &json!({"layer": a.0})), Some(json!({"layer": {"name": "A"}})));
        assert_eq!(
            record(&s, "layer.moveTo", &json!({"layers": [a.0, t1.0], "target": t2.0})),
            Some(json!({"layers": [{"name": "A"}, {"name": "Twin", "occurrence": 0}], "target": {"current": true}}))
        );
        // The current layer is left out where that is the step's default; Show Only always
        // plays on the current layer, whichever eye was ⌥-clicked.
        assert_eq!(record(&s, "layer.showOnly", &json!({"layer": t2.0})), Some(json!({})));
        assert_eq!(record(&s, "layer.showOnly", &json!({"layer": a.0})), Some(json!({})));
        assert_eq!(record(&s, "layer.showOnly", &json!({})), None);
        assert_eq!(
            record(&s, "layer.setProps", &json!({"layer": t1.0, "visible": false})),
            Some(json!({"layer": {"name": "Twin", "occurrence": 0}, "visible": false}))
        );
        // Not layer ids: as given.
        assert_eq!(record(&s, "layer.hideLayers", &json!({"layer": u64::MAX})), None);
        assert_eq!(record(&s, "layer.hideLayers", &json!({"layers": [a.0, u64::MAX]})), None);
        assert_eq!(record(&s, "edit.fill", &json!({"color": "#ff0000"})), None);
        assert_eq!(record(&s, "layer.select", &json!({"layer": a.0})), None);
    }

    #[test]
    fn references_resolve_in_the_document_they_play_on() {
        let (s, a, t1, t2) = session();
        assert_eq!(resolve(&s, "layer.hideLayers", json!({"layer": {"name": "A"}})).unwrap(), json!({"layer": a.0}));
        assert_eq!(resolve(&s, "layer.hideLayers", json!({"layer": {"current": true}})).unwrap(), json!({"layer": t2.0}));
        assert_eq!(
            resolve(&s, "layer.moveTo", json!({"layers": [{"name": "Twin", "occurrence": 1}, t1.0], "target": {"name": "A"}})).unwrap(),
            json!({"layers": [t2.0, t1.0], "target": a.0})
        );
        // Plain params pass through.
        assert_eq!(resolve(&s, "layer.hideLayers", json!({"layer": 5})).unwrap(), json!({"layer": 5}));
        // A Show Only step an older recording tied to a layer plays on the current one; a live
        // ⌥-click (a plain id) still names its layer.
        assert_eq!(resolve(&s, "layer.showOnly", json!({"layer": {"name": "A"}})).unwrap(), json!({}));
        assert_eq!(resolve(&s, "layer.showOnly", json!({"layers": [{"name": "A"}]})).unwrap(), json!({}));
        assert_eq!(resolve(&s, "layer.showOnly", json!({"layer": a.0})).unwrap(), json!({"layer": a.0}));
        // User report: an occurrence the document doesn't have (recorded among more layers of
        // that name), or none with duplicates, is the top-most layer of the name, as in Photoshop.
        assert_eq!(resolve(&s, "layer.hideLayers", json!({"layer": {"name": "Twin", "occurrence": 2}})).unwrap(), json!({"layer": t2.0}));
        assert_eq!(resolve(&s, "layer.hideLayers", json!({"layer": {"name": "Twin"}})).unwrap(), json!({"layer": t2.0}));
        // Bad references are errors, not panics.
        for bad in [
            json!({"layer": {"name": "nope"}}),
            json!({"layer": {"name": 3}}),
            json!({"layer": {"name": "Twin", "occurrence": -1}}),
            json!({"layers": [{"name": "nope"}]}),
        ] {
            assert!(resolve(&s, "layer.hideLayers", bad.clone()).is_err(), "{bad}");
        }
        assert!(resolve(&Session::new(), "layer.hideLayers", json!({"layer": {"current": true}})).is_err());
    }

    #[test]
    fn relative_references_count_panel_rows_from_the_current_layer() {
        let (mut s, a, t1, t2) = session();
        let bg = s.active().unwrap().doc.layers[0].id;
        s.actions.prefs.relative_layers = true;
        // Rows: Twin (t2, current), Twin (t1), A, Background.
        assert_eq!(record(&s, "layer.hideLayers", &json!({"layer": a.0})), Some(json!({"layer": {"relative": -2}})));
        assert_eq!(record(&s, "layer.select", &json!({"layer": t1.0, "mode": "add"})), Some(json!({"layer": {"relative": -1}, "mode": "add"})));
        assert_eq!(
            record(&s, "layer.moveTo", &json!({"layers": [a.0, t2.0], "target": bg.0})),
            Some(json!({"layers": [{"relative": -2}, {"current": true}], "target": {"relative": -3}}))
        );
        assert_eq!(resolve(&s, "layer.select", json!({"layer": {"relative": -3}})).unwrap(), json!({"layer": bg.0}));
        assert_eq!(resolve(&s, "layer.hideLayers", json!({"layers": [{"relative": 0}, {"relative": -1}]})).unwrap(), json!({"layers": [t2.0, t1.0]}));
        s.execute("layer.select", json!({"layer": a.0})).unwrap();
        assert_eq!(resolve(&s, "layer.hideLayers", json!({"layer": {"relative": 2}})).unwrap(), json!({"layer": t2.0}));
        // Past either end of the stack, or not a row count: an error that says so.
        let err = |p: Value| resolve(&s, "layer.hideLayers", p).unwrap_err().to_string();
        assert!(err(json!({"layer": {"relative": 3}})).contains("no layer 3 rows above the current one"));
        assert!(err(json!({"layer": {"relative": -2}})).contains("no layer 2 rows below the current one"));
        assert!(err(json!({"layer": {"relative": i64::MIN}})).contains("rows below"));
        for bad in [json!(1.5), json!("1"), json!(null), json!(u64::MAX)] {
            assert!(resolve(&s, "layer.hideLayers", json!({"layer": {"relative": bad}})).is_err(), "{bad}");
        }
        // A layer inside a collapsed group is no row: it is named instead.
        s.execute("layer.select", json!({"layer": t1.0})).unwrap();
        s.execute("layer.groupLayers", json!({})).unwrap();
        s.execute("layer.select", json!({"layer": a.0})).unwrap();
        assert_eq!(record(&s, "layer.hideLayers", &json!({"layer": t1.0})), Some(json!({"layer": {"name": "Twin", "occurrence": 0}})));
    }

    #[test]
    fn describe_names_the_layer() {
        assert_eq!(describe(&json!({"relative": -2})), Some(Described::Relative(-2)));
        assert_eq!(describe(&json!({"name": "A"})), Some(Described::Named("A".into())));
        assert_eq!(describe(&json!({"name": "A", "occurrence": 1})), Some(Described::Named("A (2)".into())));
        assert_eq!(describe(&json!({"current": true})), Some(Described::Current));
        assert_eq!(describe(&json!(4)), None);
    }
}
