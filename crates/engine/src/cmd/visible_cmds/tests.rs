use serde_json::json;

use super::*;

/// A 40×40 document: A, an opaque green square at (0..10), under Top, an opaque blue square at
/// (5..15), both over the white Background.
fn covered() -> (Session, LayerId, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 40, "height": 40})).unwrap();
    let mut ids = Vec::new();
    for (name, x, color) in [("A", 0, "#00ff00"), ("Top", 5, "#0000ff")] {
        s.execute("layer.new.layer", json!({"name": name})).unwrap();
        s.execute("select.rect", json!({"x": x, "y": x, "width": 10, "height": 10})).unwrap();
        s.execute("edit.fill", json!({"color": color})).unwrap();
        ids.push(s.active().unwrap().active_layer.unwrap());
    }
    s.execute("select.deselect", json!({})).unwrap();
    (s, ids[0], ids[1])
}

/// The selection at pixel (x, y), 0..1.
fn at(s: &Session, x: i32, y: i32) -> f32 {
    s.active().unwrap().doc.selection.as_ref().map_or(0.0, |m| m.sample_channel(x, y, 0))
}

fn load(s: &mut Session, p: Value) -> Value {
    s.execute(ID, p).unwrap()
}

#[test]
fn only_what_shows_of_the_layer_is_selected() {
    let (mut s, a, top) = covered();
    assert_eq!(load(&mut s, json!({"layer": a.0}))["selected"], true);
    assert_eq!(at(&s, 2, 2), 1.0, "A shows here");
    assert_eq!(at(&s, 7, 7), 0.0, "Top covers A here");
    assert_eq!(at(&s, 12, 12), 0.0, "A has no pixels here");
    // Top at half opacity lets half of A through.
    s.execute("layer.setProps", json!({"layer": top.0, "opacity": 0.5})).unwrap();
    load(&mut s, json!({"layer": a.0}));
    assert!((at(&s, 7, 7) - 0.5).abs() < 0.01, "{}", at(&s, 7, 7));
    // A Lock Selection layer counts as invisible: none of A is covered.
    s.execute("layer.setProps", json!({"layer": top.0, "opacity": 1.0, "locks": {"selection": true}})).unwrap();
    load(&mut s, json!({"layer": a.0}));
    assert_eq!(at(&s, 7, 7), 1.0);
    // The top layer itself shows everywhere it has pixels, lock or not.
    load(&mut s, json!({"layer": top.0}));
    assert_eq!((at(&s, 7, 7), at(&s, 14, 14), at(&s, 2, 2)), (1.0, 1.0, 0.0));
}

#[test]
fn adjustments_cover_nothing_and_a_hidden_layer_counts_as_shown() {
    let (mut s, a, top) = covered();
    s.execute("layer.setProps", json!({"layer": top.0, "visible": false})).unwrap();
    s.execute("layer.newAdjustmentLayer.invert", json!({})).unwrap();
    s.execute("layer.setProps", json!({"layer": a.0, "visible": false})).unwrap();
    load(&mut s, json!({"layer": a.0}));
    assert_eq!((at(&s, 2, 2), at(&s, 7, 7), at(&s, 12, 12)), (1.0, 1.0, 0.0), "hidden Top covers nothing, nor does Invert");
    assert!(!s.active().unwrap().doc.layer(a).unwrap().visible, "the layer stays hidden");
}

#[test]
fn a_mask_alone_is_selected_where_it_shows() {
    let (mut s, a, _) = covered();
    s.execute("layer.select", json!({"layer": a.0})).unwrap();
    s.execute("layer.layerMask.revealAll", json!({})).unwrap();
    load(&mut s, json!({"layer": a.0, "source": "mask"}));
    // The mask reveals the whole canvas; only Top's square covers it.
    assert_eq!((at(&s, 30, 30), at(&s, 2, 2), at(&s, 7, 7)), (1.0, 1.0, 0.0));
    assert!(s.execute(ID, json!({"layer": a.0, "source": "vectorMask"})).is_err(), "A has no vector mask");
}

#[test]
fn a_canvas_click_measures_the_layer_under_it() {
    let (mut s, a, top) = covered();
    assert_eq!(load(&mut s, json!({"x": 7.5, "y": 7.5}))["layer"], top.0);
    assert_eq!((at(&s, 7, 7), at(&s, 2, 2)), (1.0, 0.0));
    // Through a Lock Selection layer: A, then all of A shows.
    s.execute("layer.setProps", json!({"layer": top.0, "locks": {"selection": true}})).unwrap();
    assert_eq!(load(&mut s, json!({"x": 7.5, "y": 7.5}))["layer"], a.0);
    assert_eq!((at(&s, 7, 7), at(&s, 2, 2), at(&s, 14, 14)), (1.0, 1.0, 0.0));
    // Off every layer but the Background, the Background (all of it shows where nothing covers it).
    let bg = load(&mut s, json!({"x": 30, "y": 30}))["layer"].as_u64().unwrap();
    assert!(bg != a.0 && bg != top.0);
    assert_eq!((at(&s, 30, 30), at(&s, 2, 2)), (1.0, 0.0));
}

#[test]
fn operations_combine_with_the_selection() {
    let (mut s, a, top) = covered();
    load(&mut s, json!({"layer": a.0}));
    load(&mut s, json!({"layer": top.0, "operation": "add"}));
    assert_eq!((at(&s, 2, 2), at(&s, 7, 7), at(&s, 14, 14), at(&s, 30, 30)), (1.0, 1.0, 1.0, 0.0));
    load(&mut s, json!({"layer": top.0, "operation": "subtract"}));
    assert_eq!((at(&s, 2, 2), at(&s, 7, 7)), (1.0, 0.0));
    assert!(s.execute(ID, json!({"layer": a.0, "operation": "nope"})).is_err());
    assert!(s.execute(ID, json!({})).is_err(), "nothing named");
    // One undo step each.
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(at(&s, 7, 7), 1.0);
}

/// A layer clipped to the one below shows only where that base has pixels, though the rest of
/// what is below is left out of the measurement.
#[test]
fn a_clipped_layer_shows_only_inside_its_base() {
    let (mut s, a, top) = covered();
    s.execute("layer.setProps", json!({"layer": top.0, "clipped": true})).unwrap();
    load(&mut s, json!({"layer": top.0}));
    // Top (5..15) clipped to A (0..10): only their overlap shows.
    assert_eq!((at(&s, 7, 7), at(&s, 12, 12), at(&s, 2, 2)), (1.0, 0.0, 0.0));
    // A under its clipped layer shows where Top doesn't cover it.
    load(&mut s, json!({"layer": a.0}));
    assert_eq!((at(&s, 2, 2), at(&s, 7, 7)), (1.0, 0.0));
}
