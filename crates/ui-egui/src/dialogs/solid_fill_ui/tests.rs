use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use photocraft_color::{Color, ColorMode};
use photocraft_doc::{Fill, LayerContent, LayerId};
use serde_json::json;

use super::*;

fn harness(ppp: f32) -> (Harness<'static, PhotocraftApp>, LayerId) {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "depth": 16})).unwrap();
    let id = LayerId(s.execute("layer.newFillLayer.solidColor", json!({"color": "#33669980"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("layer.layerMask.revealAll", json!({})).unwrap();
    s.execute("layer.new.layer", json!({"name": "Other"})).unwrap();
    let mut h =
        Harness::builder().with_size(vec2(1440.0, 900.0)).with_pixels_per_point(ppp).with_step_dt(1.0 / 60.0).with_max_steps(64).build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            PhotocraftApp::new(s, crate::Services::default())
        });
    let ctx = h.ctx.clone();
    let (r, _) = crate::control::ControlRequest::new("ui.set", json!({"dock": {"collapsed": ["color", "properties", "history", "navigator"]}}));
    crate::control::handle(h.state_mut(), &ctx, &r);
    h.run_steps(8);
    (h, id)
}

fn double_click(h: &mut Harness<'_, PhotocraftApp>, at: Pos2) {
    h.run_steps(40);
    for _ in 0..2 {
        h.hover_at(at);
        h.step();
        for pressed in [true, false] {
            h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
            h.step();
        }
        h.run_steps(3);
    }
}

fn row(h: &Harness<'_, PhotocraftApp>, id: LayerId) -> crate::layer_row_ui::RowRects {
    crate::layer_row_ui::recorded(&h.ctx).into_iter().find(|r| r.layer == id.0).unwrap()
}

/// As in Photoshop (user report), Layer › New Fill Layer › Solid Color… makes the layer at once and
/// the picker changes it live on the canvas; Cancel takes the layer away again, leaving the
/// document and History as they were.
#[test]
fn new_solid_color_is_made_at_once_and_cancel_takes_it_away() {
    let (mut h, _) = harness(1.0);
    let ctx = h.ctx.clone();
    let app = h.state_mut();
    let before = app.session.active().unwrap().doc.clone();
    let active = app.session.active().unwrap().active_layer;
    let steps = app.session.active().unwrap().history.entries().len();

    let opened = crate::menus::invoke(app, &ctx, "layer.newFillLayer.solidColor", json!({})).unwrap();
    let layer = LayerId(opened["layer"].as_u64().expect("the layer is made at once"));
    let dialog = opened["dialog"].as_u64().unwrap();
    let st = app.session.active().unwrap();
    assert!(matches!(st.doc.layer(layer).unwrap().content, LayerContent::Fill(Fill::Solid(_))));
    assert_eq!(st.active_layer, Some(layer));
    assert_eq!(st.history.entries().len(), steps + 1);
    let picker = app.ui.dialogs.last().unwrap();
    assert_eq!(picker.id, dialog);
    assert!(owns(&picker.fields), "the picker edits the new layer");
    assert_eq!(picker.fields["__params"]["layer"], layer.0);

    // Picking a colour shows on the canvas at once.
    app.ui.dialog_mut(dialog).unwrap().fields.insert("color".into(), json!("#ff8800"));
    let (shown, _) = display_doc(app, 0).expect("a live preview");
    let LayerContent::Fill(Fill::Solid(previewed)) = shown.layer(layer).unwrap().content else { panic!("the fill layer") };
    assert_eq!(previewed.to_rgba8()[..3], [255, 136, 0]);

    crate::dialogs::cancel(app, dialog).unwrap();
    assert!(app.ui.dialogs.iter().all(|d| d.id != dialog));
    let st = app.session.active().unwrap();
    assert!(st.doc.layer(layer).is_none(), "Cancel takes the layer away");
    assert_eq!(*st.doc, *before);
    assert_eq!(st.active_layer, active);
    assert_eq!(st.history.entries().len(), steps, "and its history step");
}

#[test]
fn new_solid_color_from_menu_ok_creates_one_layer_with_chosen_or_default_color() {
    let (mut h, _) = harness(1.0);
    let ctx = h.ctx.clone();
    let app = h.state_mut();
    let steps = app.session.active().unwrap().history.entries().len();

    // OK with the initial foreground colour keeps the layer made when the picker opened; with a
    // picked colour the layer is made again with it, still one history step each.
    let first = crate::menus::invoke(app, &ctx, "layer.newFillLayer.solidColor", json!({})).unwrap();
    let created = crate::dialogs::confirm(app, first["dialog"].as_u64().unwrap()).unwrap();
    let id = LayerId(created["layer"].as_u64().unwrap());
    assert_eq!(first["layer"], id.0);
    assert!(matches!(app.session.active().unwrap().doc.layer(id).unwrap().content, LayerContent::Fill(Fill::Solid(_))));
    assert_eq!(app.session.active().unwrap().history.entries().len(), steps + 1);

    let second = crate::menus::invoke(app, &ctx, "layer.newFillLayer.solidColor", json!({})).unwrap();
    let dialog = second["dialog"].as_u64().unwrap();
    app.ui.dialog_mut(dialog).unwrap().fields.insert("color".into(), json!("#ff8800"));
    let created = crate::dialogs::confirm(app, dialog).unwrap();
    let id = LayerId(created["layer"].as_u64().unwrap());
    let LayerContent::Fill(Fill::Solid(color)) = app.session.active().unwrap().doc.layer(id).unwrap().content else {
        panic!("expected the newly created Solid Color fill");
    };
    assert_eq!(color.to_rgba8()[..3], [255, 136, 0]);
    assert_eq!(app.session.active().unwrap().active_layer, Some(id));
    assert_eq!(app.session.active().unwrap().history.entries().len(), steps + 2);

    let dialogs = app.ui.dialogs.len();
    let explicit = crate::menus::invoke(app, &ctx, "layer.newFillLayer.solidColor", json!({"color": "#123456"})).unwrap();
    assert!(explicit["layer"].as_u64().is_some());
    assert!(explicit.get("dialog").is_none());
    assert_eq!(app.ui.dialogs.len(), dialogs, "parameterised creation stays non-interactive");
    assert_eq!(app.session.active().unwrap().history.entries().len(), steps + 3);
}

#[test]
fn new_solid_color_picker_rejects_a_different_active_document() {
    let (mut h, _) = harness(1.0);
    let ctx = h.ctx.clone();
    let app = h.state_mut();
    let dialog = crate::menus::invoke(app, &ctx, "layer.newFillLayer.solidColor", json!({})).unwrap()["dialog"].as_u64().unwrap();
    app.ui.dialog_mut(dialog).unwrap().fields.insert("color".into(), json!("#ff8800"));
    app.run("file.new", json!({"width": 4, "height": 4})).unwrap();
    let before = app.session.active().unwrap().doc.clone();
    let steps = app.session.active().unwrap().history.entries().len();
    assert!(crate::dialogs::confirm(app, dialog).is_err());
    assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
    assert_eq!(app.session.active().unwrap().history.entries().len(), steps);
}

#[test]
fn only_the_colour_thumbnail_opens_the_solid_fill_picker() {
    for ppp in [1.0, 2.0] {
        let (mut h, id) = harness(ppp);
        let r = row(&h, id).row;
        let count = h.state().session.active().unwrap().history.entries().len();
        double_click(&mut h, pos2(r.left() + 46.0, r.center().y));
        assert_eq!(h.state().session.active().unwrap().active_layer, Some(id));
        assert!(!h.state().ui.mask_target && !h.state().ui.vector_mask_target, "the colour thumbnail targets the fill, not its mask");
        let d = h.state().ui.dialogs.last().expect("Color Picker opened");
        assert!(owns(&d.fields));
        assert_eq!(d.fields["__label"], "Color Picker (Solid Color)");
        assert_eq!(d.fields["color"], "#336699");
        assert_eq!(d.fields["__params"]["layer"], id.0);
        assert_eq!(h.state().session.active().unwrap().history.entries().len(), count);

        let (mut h, id) = harness(ppp);
        let r = row(&h, id).row;
        double_click(&mut h, pos2(r.right() - 35.0, r.center().y));
        assert_eq!(h.state().ui.dialogs.last().unwrap().kind, crate::state::DialogKind::LayerStyle, "row body keeps Layer Style");

        let (mut h, id) = harness(ppp);
        let r = crate::mask_thumbs_ui::recorded(&h.ctx, id.0).unwrap().0[0].1;
        double_click(&mut h, r.center());
        assert!(!h.state().ui.dialogs.iter().any(|d| owns(&d.fields)), "mask thumbnail is not the colour thumbnail");
    }
}

#[test]
fn gradient_and_adjustment_thumbnails_keep_their_properties_controls() {
    for command in ["layer.newFillLayer.gradient", "layer.newAdjustmentLayer.invert"] {
        let (mut h, _) = harness(1.0);
        let id = LayerId(h.state_mut().run(command, json!({})).unwrap()["layer"].as_u64().unwrap());
        h.run_steps(4);
        let r = row(&h, id).row;
        double_click(&mut h, pos2(r.left() + 46.0, r.center().y));
        assert!(h.state().ui.panels.properties);
        assert!(!h.state().ui.dialogs.iter().any(|d| owns(&d.fields)));
    }
}

#[test]
fn picker_previews_without_editing_cancel_discards_and_ok_is_one_step() {
    let (mut h, id) = harness(1.0);
    let app = h.state_mut();
    app.run("layer.select", json!({"layer": id.0})).unwrap();
    let before = app.session.active().unwrap().doc.clone();
    let LayerContent::Fill(Fill::Solid(original)) = before.layer(id).unwrap().content else { panic!() };
    let steps = app.session.active().unwrap().history.entries().len();
    let tools = app.session.tools.foreground;
    let dialog = open(app).unwrap();
    app.ui.dialog_mut(dialog).unwrap().fields.insert("color".into(), json!("#ff8800"));
    let (preview, key) = display_doc(app, 0).unwrap();
    let (again, again_key) = display_doc(app, 0).unwrap();
    assert_eq!(key, again_key);
    assert!(Arc::ptr_eq(&preview, &again), "preview is cached");
    assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc), "preview does not dirty the real document");
    assert_eq!(app.session.active().unwrap().history.entries().len(), steps);
    let LayerContent::Fill(Fill::Solid(color)) = preview.layer(id).unwrap().content else { panic!() };
    assert_eq!(color.to_rgba8(), [255, 136, 0, 128]);
    assert_eq!(thumbnail_color(app, before.id, id, original), color);
    app.ui.close_dialog(dialog);
    assert!(display_doc(app, 0).is_none());
    assert!(app.solid_fill_preview.is_none());
    assert_eq!(thumbnail_color(app, before.id, id, original), original);
    let dialog = open(app).unwrap();
    app.ui.dialog_mut(dialog).unwrap().fields.insert("color".into(), json!("#ff8800"));
    let (_, reopened_key) = display_doc(app, 0).unwrap();
    assert_ne!(key, reopened_key, "reopening a picker never reuses a cancelled preview key");
    crate::dialogs::confirm(app, dialog).unwrap();
    assert_eq!(app.session.active().unwrap().history.entries().len(), steps + 1);
    assert_eq!(app.session.tools.foreground, tools, "editing a fill doesn't change the tool colour");
    assert_eq!(app.session.active().unwrap().doc.layer(id).unwrap().content, LayerContent::Fill(Fill::Solid(color)));
    let bytes = photocraft_format::save_to_bytes(&app.session.active().unwrap().doc, &Default::default()).unwrap();
    let loaded = photocraft_format::load_from_bytes(&bytes).unwrap();
    assert_eq!(loaded.layer(id).unwrap().content, LayerContent::Fill(Fill::Solid(color)));
    assert_eq!(loaded.layer(id).unwrap().mask, before.layer(id).unwrap().mask);
    let bytes = photocraft_io::export(&app.session.active().unwrap().doc, "fill.psd", &Default::default()).unwrap().bytes;
    let loaded = photocraft_io::import("fill.psd", &bytes).unwrap().document;
    let fill = loaded.walk().into_iter().find(|(_, _, l)| matches!(l.content, LayerContent::Fill(Fill::Solid(_)))).unwrap().2;
    let LayerContent::Fill(Fill::Solid(saved)) = fill.content else { panic!() };
    // PSD's existing SoCo descriptor stores the fill's colour components, not Color.alpha.
    assert_eq!(&saved.to_rgba8()[..3], &color.to_rgba8()[..3]);
    assert!(fill.mask.is_some());
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(*app.session.active().unwrap().doc, *before);
    app.run("edit.redo", json!({})).unwrap();
    assert_eq!(app.session.active().unwrap().doc.layer(id).unwrap().content, LayerContent::Fill(Fill::Solid(color)));
}

#[test]
fn unchanged_ok_keeps_imported_model_precision_and_alpha() {
    let (mut h, id) = harness(1.0);
    let app = h.state_mut();
    app.run("layer.select", json!({"layer": id.0})).unwrap();
    let color = Color { mode: ColorMode::Lab, c: [0.512345, 0.498765, 0.56789, 0.0], alpha: 0.345678 };
    app.run(cmds::SET, json!({"color": color})).unwrap();
    let before = app.session.active().unwrap().doc.clone();
    let dialog = open(app).unwrap();
    crate::dialogs::confirm(app, dialog).unwrap();
    assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
}

#[test]
fn picker_never_applies_to_a_different_document_with_the_same_layer_id() {
    let (mut h, id) = harness(1.0);
    let app = h.state_mut();
    app.run("layer.select", json!({"layer": id.0})).unwrap();
    let dialog = open(app).unwrap();
    app.ui.dialog_mut(dialog).unwrap().fields.insert("color".into(), json!("#ff0000"));
    app.run("file.new", json!({"width": 4, "height": 4})).unwrap();
    app.run("layer.newFillLayer.solidColor", json!({})).unwrap();
    let before = app.session.active().unwrap().doc.clone();
    assert!(display_doc(app, app.session.active_index().unwrap()).is_none());
    assert!(crate::dialogs::confirm(app, dialog).is_err());
    assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
}

#[test]
fn cmyk_lab_preview_thumbnail_cancel_and_ok_are_consistent() {
    for (command, mode) in [("image.mode.cmyk", ColorMode::Cmyk), ("image.mode.lab", ColorMode::Lab)] {
        let (mut h, id) = harness(1.0);
        let app = h.state_mut();
        app.run(command, json!({})).unwrap();
        app.run("layer.select", json!({"layer": id.0})).unwrap();
        let before = app.session.active().unwrap().doc.clone();
        let LayerContent::Fill(Fill::Solid(original)) = before.layer(id).unwrap().content else { panic!() };
        assert_eq!(before.mode, mode);
        let count = app.session.active().unwrap().history.entries().len();
        let dialog = open(app).unwrap();
        app.ui.dialog_mut(dialog).unwrap().fields.insert("color".into(), json!("#ff8800"));
        let (shown, _) = display_doc(app, app.session.active_index().unwrap()).unwrap();
        let LayerContent::Fill(Fill::Solid(preview)) = shown.layer(id).unwrap().content else { panic!() };
        assert_eq!(preview, Color::rgba(1.0, 136.0 / 255.0, 0.0, original.alpha).in_mode(mode));
        assert_eq!(thumbnail_color(app, before.id, id, original), preview);
        assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
        assert_eq!(app.session.active().unwrap().history.entries().len(), count);
        app.ui.close_dialog(dialog);
        assert!(display_doc(app, app.session.active_index().unwrap()).is_none());
        assert!(Arc::ptr_eq(&before, &app.session.active().unwrap().doc));
        assert_eq!(app.session.active().unwrap().history.entries().len(), count);
        let dialog = open(app).unwrap();
        app.ui.dialog_mut(dialog).unwrap().fields.insert("color".into(), json!("#ff8800"));
        crate::dialogs::confirm(app, dialog).unwrap();
        assert_eq!(app.session.active().unwrap().doc.layer(id).unwrap().content, LayerContent::Fill(Fill::Solid(preview)));
        assert_eq!(app.session.active().unwrap().history.entries().len(), count + 1);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(*app.session.active().unwrap().doc, *before);
        app.run("edit.redo", json!({})).unwrap();
        assert_eq!(app.session.active().unwrap().doc.layer(id).unwrap().content, LayerContent::Fill(Fill::Solid(preview)));
    }
}

/// Like the Color panel (user report), the picker's colour field and slider pick on the press,
/// not on release, and follow the drag while the button is held.
#[test]
fn picker_field_and_slider_pick_on_press_and_follow_the_drag() {
    use egui_kittest::kittest::Queryable;
    let (mut h, _) = harness(1.0);
    let ctx = h.ctx.clone();
    let opened = crate::menus::invoke(h.state_mut(), &ctx, "layer.newFillLayer.solidColor", json!({})).unwrap();
    let dialog = opened["dialog"].as_u64().unwrap();
    h.run_steps(6);
    let color = |h: &Harness<'_, PhotocraftApp>| h.state().ui.dialogs.iter().find(|d| d.id == dialog).unwrap().fields["color"].clone();
    let button = |h: &mut Harness<'_, PhotocraftApp>, at: Pos2, pressed: bool| {
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    };
    for label in ["Color field", "Color slider"] {
        let r = h.get_by_label(label).rect();
        let (a, b) = (pos2(r.left() + r.width() * 0.3, r.top() + r.height() * 0.25), pos2(r.left() + r.width() * 0.7, r.top() + r.height() * 0.6));
        h.hover_at(a);
        h.step();
        let before = color(&h);
        button(&mut h, a, true);
        let pressed = color(&h);
        assert_ne!(pressed, before, "{label}: the press picks at once");
        h.hover_at(b);
        h.step();
        h.step();
        let dragged = color(&h);
        assert_ne!(dragged, pressed, "{label}: the drag follows the pointer");
        button(&mut h, b, false);
        assert_eq!(color(&h), dragged, "{label}: release keeps the dragged colour");
    }
}

/// User report (again: a double-click on a Solid Color thumbnail sometimes didn't open the
/// picker). A pen tap slides a few points between press and release, which egui counts as a drag;
/// and a slow frame can take both clicks at once, which egui counts as one click. Both open it.
#[test]
fn a_sliding_pen_double_tap_or_a_double_click_in_one_frame_opens_the_picker() {
    let opened = |h: &Harness<'_, PhotocraftApp>| h.state().ui.dialogs.iter().any(|d| owns(&d.fields));
    let (mut h, id) = harness(1.0);
    let at = pos2(row(&h, id).row.left() + 46.0, row(&h, id).row.center().y);
    h.run_steps(40);
    for _ in 0..2 {
        h.hover_at(at);
        h.step();
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        for i in 1..=3 {
            h.hover_at(at + vec2(3.0 * i as f32, 1.0));
            h.step();
        }
        h.event(Event::PointerButton { pos: at + vec2(9.0, 1.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.step();
        h.run_steps(3);
    }
    assert!(opened(&h), "a 9 pt slide is still a tap");
    assert_eq!(h.state().session.active().unwrap().active_layer, Some(id));
    let names: Vec<String> = h.state().session.active().unwrap().doc.layers.iter().map(|l| l.name.clone()).collect();

    let (mut h, id) = harness(1.0);
    let at = pos2(row(&h, id).row.left() + 46.0, row(&h, id).row.center().y);
    h.run_steps(40);
    h.hover_at(at);
    h.step();
    for pressed in [true, false, true, false] {
        h.input_mut().events.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    }
    h.step();
    h.run_steps(3);
    assert!(opened(&h), "two clicks in one frame");
    assert_eq!(h.state().session.active().unwrap().doc.layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>(), names, "no layer moved");

    // A real drag of the row is no tap.
    let (mut h, id) = harness(1.0);
    let at = pos2(row(&h, id).row.left() + 46.0, row(&h, id).row.center().y);
    h.run_steps(40);
    for _ in 0..2 {
        h.hover_at(at);
        h.step();
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        h.hover_at(at + vec2(30.0, 0.0));
        h.step();
        h.event(Event::PointerButton { pos: at + vec2(30.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.step();
        h.run_steps(3);
    }
    assert!(!opened(&h));
}

/// User report: dragging in the Solid Color picker felt low-rate. Each colour was a new document
/// to the canvas, recomposited in full (94 ms a frame on a 2000×3000 canvas); between the picker's
/// previews only the fill layer's area changes, and only that is refreshed.
#[test]
fn picker_previews_refresh_only_the_fill_layers_area() {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 400, "height": 300})).unwrap();
    s.execute("select.rect", json!({"x": 10, "y": 20, "width": 50, "height": 40})).unwrap();
    // A fill made with a selection is masked to it: its area is the selection's.
    s.execute("layer.newFillLayer.solidColor", json!({"color": "#336699"})).unwrap();
    assert!(s.active().unwrap().doc.selection.is_none(), "the selection became the mask");
    let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).with_step_dt(1.0 / 60.0).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        PhotocraftApp::new(s, crate::Services::default())
    });
    h.run_steps(8);
    let dialog = open(h.state_mut()).unwrap();
    h.run_steps(4);
    for (i, color) in ["#ff0000", "#00ff00", "#0000ff", "#336699"].into_iter().enumerate() {
        h.state_mut().ui.dialog_mut(dialog).unwrap().fields.insert("color".into(), json!(color));
        h.run_steps(2);
        let p = &h.state().perf;
        assert_eq!(p.last_refresh, "rect", "colour {i}");
        assert!(p.last_refresh_px <= 64 * 64, "colour {i}: {} px", p.last_refresh_px);
    }
}
