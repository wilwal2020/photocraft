use photocraft_doc::{BlendMode, Effect, LayerContent, LayerId};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use super::store::{self, MANIFEST_FILE, item_file, thumb_file};
use super::*;
use crate::preset_store::{MemBackend, PresetBackend};

/// A 40×30 document with a red 10×10 square at (4, 6) on layer "paint", which has a Reveal All
/// mask and a drop shadow. "paint" is active.
fn painted() -> (Session, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 40, "height": 30})).unwrap();
    let paint = LayerId(s.execute("layer.new.layer", json!({"name": "paint"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("select.rect", json!({"x": 4, "y": 6, "width": 10, "height": 10})).unwrap();
    s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("layer.layerMask.revealAll", json!({})).unwrap();
    s.execute("layer.layerStyle.dropShadow", json!({"size": 4, "distance": 3})).unwrap();
    (s, paint)
}

/// A session with a store on `backend` attached.
fn attached(backend: &MemBackend) -> (Session, Vec<String>) {
    let mut s = Session::new();
    let w = s.attach_library_store(store::open(Box::new(backend.clone())));
    (s, w)
}

fn files(backend: &MemBackend) -> Vec<String> {
    backend.list().unwrap().into_iter().map(|(n, _)| n).collect()
}

fn id_of(r: &Value) -> u64 {
    r["id"].as_u64().unwrap()
}

#[test]
fn sessions_have_no_library_store_by_default() {
    let (mut s, _) = painted();
    s.execute("library.addLayers", json!({})).unwrap();
    assert!(!s.libraries.persistent(), "headless sessions keep their libraries in memory");
    assert_eq!(s.libraries.libraries[0].name, "My Library", "the first item makes a library");
}

#[test]
fn layers_round_trip_with_their_mask_effects_and_a_groups_children() {
    let backend = MemBackend::default();
    let (mut s, paint) = painted();
    s.attach_library_store(store::open(Box::new(backend.clone())));
    // A group holding "paint" and a second layer.
    s.execute("layer.new.layer", json!({"name": "top"})).unwrap();
    s.execute("layer.select", json!({"layer": paint.0, "mode": "add"})).unwrap();
    s.execute("layer.groupLayers", json!({})).unwrap();
    let r = s.execute("library.addLayers", json!({"name": "Badge"})).unwrap();
    let id = id_of(&r);
    assert_eq!(r["kind"], "layers");
    let names = files(&backend);
    assert!(names.contains(&item_file(id)) && names.contains(&thumb_file(id)) && names.contains(&MANIFEST_FILE.to_string()), "{names:?}");
    let item = s.libraries.item(id).unwrap();
    let thumb = item.thumb.as_ref().unwrap();
    assert_eq!((thumb.width, thumb.height), (10, 10), "cropped to the content");

    // A restart: the item comes back from disk and places into another document.
    let (mut t, warnings) = attached(&backend);
    assert!(warnings.is_empty(), "{warnings:?}");
    let item = t.libraries.item(id).unwrap();
    assert_eq!((item.name.as_str(), item.kind), ("Badge", ItemKind::Layers));
    assert!(item.payload.is_none() && item.thumb.is_some(), "only the thumbnail is read at start");
    t.execute("file.new", json!({"width": 60, "height": 50})).unwrap();
    let placed = t.execute("library.use", json!({"item": id})).unwrap();
    let d = &t.active().unwrap().doc;
    assert_eq!(d.layers.len(), 2);
    let group = &d.layers[1];
    let LayerContent::Group(g) = &group.content else { panic!("a group: {:?}", group.content) };
    assert_eq!(g.children.len(), 2);
    let paint = g.children.iter().find(|l| l.name == "paint").unwrap();
    assert!(paint.mask.is_some(), "the mask comes along");
    assert!(paint.effects.items.iter().any(|e| matches!(e, Effect::DropShadow(_))), "the effects come along");
    assert_eq!(placed["layer"], group.id.0);
    // Centred on the canvas.
    assert_eq!(crate::layer_multi_cmds::layer_bounds(paint).unwrap(), Rect::new(25, 20, 35, 30));
    assert_eq!(paint.surface().unwrap().read_region(Rect::new(26, 21, 27, 22)), vec![1.0, 0.0, 0.0, 1.0]);
    // One history step.
    assert!(t.undo());
    assert_eq!(t.active().unwrap().doc.layers.len(), 1);
}

#[test]
fn placing_lands_on_a_point_and_converts_to_the_documents_depth() {
    let (mut s, _) = painted();
    let id = id_of(&s.execute("library.addLayers", json!({})).unwrap());
    s.execute("file.new", json!({"width": 60, "height": 50, "depth": 16})).unwrap();
    let r = s.execute("library.use", json!({"item": id, "at": [10.0, 40.0]})).unwrap();
    let l = s.active().unwrap().doc.layer(LayerId(r["layer"].as_u64().unwrap())).unwrap();
    assert_eq!(crate::layer_multi_cmds::layer_bounds(l).unwrap(), Rect::new(5, 35, 15, 45));
    assert_eq!(l.surface().unwrap().format().sample, photocraft_color::SampleType::U16);
}

#[test]
fn an_image_from_the_selection_round_trips_as_a_pixel_layer() {
    let backend = MemBackend::default();
    let (mut s, _) = painted();
    s.attach_library_store(store::open(Box::new(backend.clone())));
    s.execute("select.rect", json!({"x": 6, "y": 8, "width": 4, "height": 3})).unwrap();
    let r = s.execute("library.addImage", json!({})).unwrap();
    let id = id_of(&r);
    assert_eq!((r["kind"].as_str(), r["name"].as_str()), (Some("image"), Some("paint")));

    let (mut t, _) = attached(&backend);
    t.execute("file.new", json!({"width": 20, "height": 20})).unwrap();
    let placed = t.execute("library.use", json!({"item": id})).unwrap();
    let l = t.active().unwrap().doc.layer(LayerId(placed["layer"].as_u64().unwrap())).unwrap();
    assert!(matches!(l.content, LayerContent::Raster(_)), "a pixel layer");
    assert_eq!(l.name, "paint");
    assert!(l.mask.is_none() && l.effects.items.is_empty(), "pixels only");
    assert_eq!(crate::layer_multi_cmds::layer_bounds(l).unwrap(), Rect::new(8, 9, 12, 12));
    assert_eq!(l.surface().unwrap().read_region(Rect::new(8, 9, 9, 10)), vec![1.0, 0.0, 0.0, 1.0]);
}

#[test]
fn an_image_from_a_file_is_flattened_and_named_after_it() {
    let dir = std::env::temp_dir().join(format!("pc-library-image-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Leaf.png");
    let px: Vec<u8> = (0..6 * 4).flat_map(|i| [0u8, 200, (i * 10) as u8, 255]).collect();
    let img = photocraft_codecs::Image::from_u8(6, 4, photocraft_codecs::ChannelLayout::Rgba, px).unwrap();
    std::fs::write(&path, photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()).unwrap();
    let mut s = Session::new();
    let r = s.execute("library.addImage", json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!(r["name"], "Leaf");
    let id = id_of(&r);
    s.execute("file.new", json!({"width": 30, "height": 30})).unwrap();
    let placed = s.execute("library.use", json!({"item": id})).unwrap();
    let l = s.active().unwrap().doc.layer(LayerId(placed["layer"].as_u64().unwrap())).unwrap();
    assert_eq!(crate::layer_multi_cmds::layer_bounds(l).unwrap(), Rect::new(12, 13, 18, 17));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(s.execute("library.addImage", json!({"path": dir.join("missing.png").to_string_lossy()})).is_err());
}

#[test]
fn a_layer_style_round_trips_and_applies_to_the_selected_layers() {
    let backend = MemBackend::default();
    let (mut s, paint) = painted();
    s.attach_library_store(store::open(Box::new(backend.clone())));
    s.execute("layer.setProps", json!({"layer": paint.0, "blend": "multiply", "opacity": 0.5})).unwrap();
    let r = s.execute("library.addStyle", json!({})).unwrap();
    let id = id_of(&r);
    assert_eq!((r["kind"].as_str(), r["name"].as_str()), (Some("style"), Some("paint Style")));
    assert!(s.libraries.item(id).unwrap().thumb.is_some());

    let (mut t, _) = attached(&backend);
    t.execute("file.new", json!({"width": 20, "height": 20})).unwrap();
    let a = LayerId(t.execute("layer.new.layer", json!({"name": "a"})).unwrap()["layer"].as_u64().unwrap());
    let b = LayerId(t.execute("layer.new.layer", json!({"name": "b"})).unwrap()["layer"].as_u64().unwrap());
    t.execute("layer.select", json!({"layer": a.0, "mode": "add"})).unwrap();
    let applied = t.execute("library.use", json!({"item": id})).unwrap();
    assert_eq!(applied["layers"].as_array().unwrap().len(), 2);
    for lid in [a, b] {
        let l = t.active().unwrap().doc.layer(lid).unwrap();
        assert!(l.effects.items.iter().any(|e| matches!(e, Effect::DropShadow(_))), "{}", l.name);
        assert_eq!(l.blend, BlendMode::Multiply);
        assert!((l.opacity - 0.5).abs() < 1e-3);
    }
    assert!(t.undo());
    assert!(t.active().unwrap().doc.layer(a).unwrap().effects.items.is_empty(), "one history step");
}

#[test]
fn colours_round_trip_and_become_the_foreground() {
    let backend = MemBackend::default();
    let (mut s, _) = attached(&backend);
    let id = id_of(&s.execute("library.addColor", json!({"color": "#3366cc", "name": "Sky"})).unwrap());
    assert!(!files(&backend).iter().any(|f| f.starts_with("item-")), "a colour lives in the manifest");
    let (mut t, _) = attached(&backend);
    let item = t.libraries.item(id).unwrap();
    assert_eq!((item.name.as_str(), item.kind), ("Sky", ItemKind::Color));
    let r = t.execute("library.use", json!({"item": id})).unwrap();
    assert_eq!(r["foreground"], "#3366cc");
    let fg = t.tools.foreground;
    assert!((fg[0] - 0.2).abs() < 1e-3 && (fg[2] - 0.8).abs() < 1e-3, "{fg:?}");
    // The foreground by default; malformed colours are refused.
    t.tools.foreground = [1.0, 0.0, 0.0, 1.0];
    assert_eq!(t.execute("library.addColor", json!({})).unwrap()["color"], "#ff0000");
    for bad in [json!({"color": "#12"}), json!({"color": [2.0, 0.0, 0.0]}), json!({"color": 7})] {
        assert!(t.execute("library.addColor", bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn libraries_and_items_are_created_renamed_moved_and_deleted_persistently() {
    let backend = MemBackend::default();
    let (mut s, _) = attached(&backend);
    s.execute("file.new", json!({"width": 10, "height": 10})).unwrap();
    assert_eq!(s.execute("library.new", json!({"name": "Brand"})).unwrap()["name"], "Brand");
    assert_eq!(s.execute("library.new", json!({"name": "brand"})).unwrap()["name"], "brand 2", "names are unique");
    s.execute("library.rename", json!({"library": "brand 2", "name": "Icons"})).unwrap();
    s.execute("library.select", json!({"library": "Brand"})).unwrap();
    let layer = id_of(&s.execute("library.addLayers", json!({})).unwrap());
    let colour = id_of(&s.execute("library.addColor", json!({"color": "#00ff00"})).unwrap());
    s.execute("library.renameItem", json!({"item": layer, "name": "Logo"})).unwrap();
    s.execute("library.moveItem", json!({"item": colour, "library": "Icons"})).unwrap();

    let (mut t, _) = attached(&backend);
    let names: Vec<&str> = t.libraries.libraries.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Brand", "Icons"]);
    assert_eq!(t.libraries.current, 0, "the current library is remembered");
    assert_eq!(t.libraries.item(layer).unwrap().name, "Logo");
    assert_eq!(t.libraries.locate(colour), Some((1, 0)));

    t.execute("library.deleteItem", json!({"item": layer})).unwrap();
    assert!(!files(&backend).contains(&item_file(layer)), "a deleted item's files go");
    t.execute("library.delete", json!({"library": "Icons"})).unwrap();
    let (u, _) = attached(&backend);
    assert_eq!(u.libraries.libraries.len(), 1);
    assert!(u.libraries.libraries[0].items.is_empty());
    // Ids are never reused: a new item never takes a deleted item's files.
    let mut u = u;
    u.execute("file.new", json!({"width": 10, "height": 10})).unwrap();
    assert!(id_of(&u.execute("library.addLayers", json!({})).unwrap()) > colour);
}

#[test]
fn a_corrupt_manifest_is_left_alone_and_never_overwritten() {
    let backend = MemBackend::default();
    backend.write(MANIFEST_FILE, b"{ not json").unwrap();
    let (mut s, warnings) = attached(&backend);
    assert!(warnings.iter().any(|w| w.contains(MANIFEST_FILE)), "{warnings:?}");
    s.execute("library.addColor", json!({"color": "#123456"})).unwrap();
    assert_eq!(backend.read(MANIFEST_FILE, 1 << 20).unwrap(), b"{ not json");
    assert!(s.libraries.take_warnings().iter().any(|w| w.contains("left unchanged")));
}

#[test]
fn missing_and_orphaned_item_files_are_handled() {
    let backend = MemBackend::default();
    let (mut s, _) = attached(&backend);
    s.execute("file.new", json!({"width": 10, "height": 10})).unwrap();
    let a = id_of(&s.execute("library.addLayers", json!({})).unwrap());
    let b = id_of(&s.execute("library.addLayers", json!({})).unwrap());
    backend.remove(&item_file(a)).unwrap();
    // A bundle no manifest names (a crash between writes).
    backend.write(&item_file(999), b"junk").unwrap();
    let (mut t, warnings) = attached(&backend);
    assert!(warnings.iter().any(|w| w.contains("missing")), "{warnings:?}");
    assert!(t.libraries.item(a).is_none() && t.libraries.item(b).is_some());
    assert!(!files(&backend).contains(&item_file(999)), "the orphan is deleted");
    // A bundle that went bad after start fails when used, without a panic.
    backend.write(&item_file(b), b"junk").unwrap();
    t.execute("file.new", json!({"width": 10, "height": 10})).unwrap();
    assert!(t.execute("library.use", json!({"item": b})).is_err());
}

#[test]
fn items_made_before_the_store_loads_are_kept() {
    let backend = MemBackend::default();
    {
        let (mut s, _) = attached(&backend);
        s.execute("library.addColor", json!({"color": "#010203"})).unwrap();
    }
    let mut s = Session::new();
    s.execute("library.addColor", json!({"color": "#040506"})).unwrap();
    s.execute("library.new", json!({"name": "Early"})).unwrap();
    s.attach_library_store(store::open(Box::new(backend.clone())));
    let lib = &s.libraries.libraries;
    assert_eq!(lib.len(), 2);
    assert_eq!(lib[0].items.len(), 2, "same-named libraries merge");
    assert_ne!(lib[0].items[0].id, lib[0].items[1].id);
    assert_eq!(s.libraries.libraries[s.libraries.current].name, "Early");
    let (t, _) = attached(&backend);
    assert_eq!(t.libraries.libraries.len(), 2, "and are saved");
}

#[test]
fn a_directory_store_round_trips() {
    let dir = std::env::temp_dir().join(format!("pc-library-dir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut s, _) = painted();
    s.attach_library_store(store::open_dir(&dir));
    let id = id_of(&s.execute("library.addLayers", json!({})).unwrap());
    let mut t = Session::new();
    let w = t.attach_library_store(store::open_dir(&dir));
    assert!(w.is_empty(), "{w:?}");
    t.execute("file.new", json!({"width": 20, "height": 20})).unwrap();
    t.execute("library.use", json!({"item": id})).unwrap();
    assert_eq!(t.active().unwrap().doc.layers.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_params_are_errors_not_panics() {
    let (mut s, _) = painted();
    for (id, p) in [
        ("library.use", json!({})),
        ("library.use", json!({"item": "x"})),
        ("library.use", json!({"item": 42})),
        ("library.rename", json!({"name": "x"})),
        ("library.select", json!({"library": 9})),
        ("library.new", json!({"name": "  "})),
        ("library.new", json!({"name": 3})),
        ("library.addLayers", json!({"layers": [123456789]})),
        ("library.addLayers", json!({"layers": "all"})),
        ("library.addImage", json!({"path": 5})),
        ("library.addImage", json!({"merged": "yes"})),
        ("library.moveItem", json!({"item": 1})),
    ] {
        assert!(s.execute(id, p.clone()).is_err(), "{id} {p}");
    }
    let id = id_of(&s.execute("library.addLayers", json!({})).unwrap());
    assert!(s.execute("library.use", json!({"item": id, "at": [f64::MAX, 0.0]})).is_err());
    assert!(s.execute("library.use", json!({"item": id, "at": "middle"})).is_err());
    assert!(s.execute("library.renameItem", json!({"item": id, "name": ""})).is_err());
}

#[test]
fn library_commands_record_and_play_back_in_actions() {
    let (mut s, _) = painted();
    let id = id_of(&s.execute("library.addLayers", json!({})).unwrap());
    s.execute("actions.record", json!({"name": "Stamp"})).unwrap();
    s.execute("library.use", json!({"item": id})).unwrap();
    s.execute("actions.stop", json!({})).unwrap();
    let before = s.active().unwrap().doc.layers.len();
    s.execute("actions.play", json!({"action": "Stamp"})).unwrap();
    assert_eq!(s.active().unwrap().doc.layers.len(), before + 1);
}
