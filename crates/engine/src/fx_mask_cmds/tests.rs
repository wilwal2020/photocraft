use serde_json::json;

use crate::Session;
use photocraft_doc::LayerId;

/// A 60×40 white document with a red square (20..40 × 10..30) stroked 4 px outside in blue.
fn session() -> (Session, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 60, "height": 40, "background": "white"})).unwrap();
    s.execute("layer.new.layer", json!({"name": "Square"})).unwrap();
    s.execute("select.rect", json!({"x": 20, "y": 10, "width": 20, "height": 20})).unwrap();
    s.execute("edit.fill", json!({"contents": "color", "color": "#ff0000"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("layer.layerStyle.stroke", json!({"size": 4, "position": "outside", "color": "#0000ff"})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap();
    (s, id)
}

fn px(s: &Session, x: i32, y: i32) -> [f32; 4] {
    photocraft_compose::render(&s.active().unwrap().doc, photocraft_geom::Rect::from_xywh(x, y, 1, 1)).px[0]
}

/// The stroke shows at (x, y): the pixel is blue.
fn stroked(s: &Session, x: i32, y: i32) -> bool {
    let p = px(s, x, y);
    p[2] > 0.99 && p[0] < 0.01
}

#[test]
fn an_effect_mask_hides_the_effect_where_it_is_black_and_leaves_the_layer() {
    let (mut s, id) = session();
    assert!(stroked(&s, 18, 12) && stroked(&s, 42, 12), "the stroke runs round the square");
    // Only the stroke's left half: a mask from a selection of the left half of the canvas.
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 30, "height": 40})).unwrap();
    let r = s.execute(super::ADD, json!({"index": 0})).unwrap();
    assert_eq!(r["target"], "fxMask:0");
    assert!(s.active().unwrap().doc.selection.is_none(), "the selection became the mask");
    assert!(stroked(&s, 18, 12), "the stroke shows where the mask is white");
    assert_eq!(px(&s, 42, 12), [1.0, 1.0, 1.0, 1.0], "and is gone where it is black");
    assert_eq!(px(&s, 35, 20), [1.0, 0.0, 0.0, 1.0], "the layer itself isn't masked");
    // Painting with the mask targeted edits it: white brings the stroke back there.
    s.execute("select.rect", json!({"x": 40, "y": 0, "width": 20, "height": 40})).unwrap();
    s.execute("edit.fill", json!({"contents": "color", "color": "#ffffff", "target": "fxMask:0"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    assert!(stroked(&s, 42, 12));
    assert!(s.active().unwrap().doc.layer(id).unwrap().mask.is_none(), "the layer got no layer mask");
    // Disabled, it stops hiding; deleted, it is gone; undo brings it back.
    s.execute(super::ENABLED, json!({"index": 0, "enabled": false})).unwrap();
    assert!(stroked(&s, 30, 8));
    s.execute(super::ENABLED, json!({"index": 0, "enabled": true})).unwrap();
    assert!(!stroked(&s, 30, 8) && stroked(&s, 28, 8));
    s.execute(super::DELETE, json!({"index": 0})).unwrap();
    assert!(stroked(&s, 30, 8));
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!stroked(&s, 30, 8));
}

#[test]
fn an_effect_mask_moves_with_its_layer_and_survives_the_style_dialog() {
    let (mut s, id) = session();
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 30, "height": 40})).unwrap();
    s.execute(super::ADD, json!({"index": 0})).unwrap();
    s.execute("layer.translate", json!({"dx": 4, "dy": 0})).unwrap();
    assert!(stroked(&s, 22, 12) && !stroked(&s, 46, 12), "the mask moved with the square");
    assert_eq!(s.active().unwrap().doc.layer(id).unwrap().effects.items[0].mask().unwrap().value(33, 5), 1.0);
    // The Layer Style dialog sends the whole list back: an entry `from` the layer's first effect
    // keeps its mask; a new one has none.
    let fx = serde_json::to_value(&s.active().unwrap().doc.layer(id).unwrap().effects.items[0]).unwrap();
    s.execute(
        "layer.layerStyle.replace",
        json!({"effects": [{"kind": "stroke", "fx": fx, "params": {"size": 6}, "from": 0}, {"kind": "dropShadow", "params": {}}]}),
    )
    .unwrap();
    let items = &s.active().unwrap().doc.layer(id).unwrap().effects.items;
    assert!(items[0].mask().is_some() && items[1].mask().is_none());
    assert!(!stroked(&s, 47, 12));
}

#[test]
fn an_effect_mask_is_saved_and_never_kept_in_presets() {
    let (mut s, id) = session();
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 30, "height": 40})).unwrap();
    s.execute(super::ADD, json!({"index": 0})).unwrap();
    let doc = &s.active().unwrap().doc;
    let bytes = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
    let back = photocraft_format::load_from_bytes(&bytes).unwrap();
    let l = back.layers.iter().find(|l| l.name == "Square").unwrap();
    let m = l.effects.items[0].mask().expect("the mask was saved");
    assert_eq!((m.value(10, 10), m.value(50, 10)), (1.0, 0.0));
    // The effect's JSON (styles, presets, the dialog's snapshot) leaves the mask out.
    let json = serde_json::to_value(&doc.layer(id).unwrap().effects.items[0]).unwrap();
    let again: photocraft_doc::Effect = serde_json::from_value(json).unwrap();
    assert!(again.mask().is_none());
}

#[test]
fn the_commands_refuse_an_effect_the_layer_does_not_have() {
    let (mut s, _) = session();
    assert!(s.execute(super::ADD, json!({"index": 3})).is_err());
    assert!(s.execute(super::DELETE, json!({"index": 0})).is_err(), "no mask yet");
    assert!(s.execute(super::ADD, json!({})).is_err(), "needs an index");
    s.execute(super::ADD, json!({"index": 0})).unwrap();
    assert!(s.execute(super::ADD, json!({"index": 0})).is_err(), "already has one");
    assert!(s.execute("edit.fill", json!({"contents": "color", "color": "#000000", "target": "fxMask:5"})).is_err());
}
