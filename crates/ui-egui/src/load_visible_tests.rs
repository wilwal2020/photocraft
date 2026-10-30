//! ⌘Space (Ctrl+Space off the Mac) held, user request: a click on a layer's row, a mask
//! thumbnail or the canvas selects where that layer or mask shows (`select.loadVisible`), and
//! the canvas neither zooms nor pans nor hands the click to the tool.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use photocraft_doc::LayerId;
use photocraft_geom::Rect;
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::Tool;

/// The app with a 64×48 document: Low, a square at (0..20), under High, a square at (10..30).
fn harness() -> (Harness<'static, PhotocraftApp>, LayerId, LayerId) {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
    let mut ids = Vec::new();
    for (name, x) in [("Low", 0), ("High", 10)] {
        s.execute("layer.new.layer", json!({"name": name})).unwrap();
        s.execute("select.rect", json!({"x": x, "y": x, "width": 20, "height": 20})).unwrap();
        s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
        ids.push(s.active().unwrap().active_layer.unwrap());
    }
    s.execute("select.deselect", json!({})).unwrap();
    let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).with_step_dt(1.0 / 60.0).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut app = PhotocraftApp::new(s, crate::Services::default());
        app.ui.tool = Tool::Brush;
        app
    });
    let ctx = h.ctx.clone();
    let (r, _) = crate::control::ControlRequest::new("ui.set", json!({"dock": {"collapsed": ["color", "properties", "history", "navigator"]}}));
    crate::control::handle(h.state_mut(), &ctx, &r);
    h.run_steps(8);
    (h, ids[0], ids[1])
}

/// ⌘ (Ctrl off the Mac) as egui-winit reports it.
fn cmd() -> Modifiers {
    if cfg!(target_os = "macos") { Modifiers::MAC_CMD | Modifiers::COMMAND } else { Modifiers::CTRL | Modifiers::COMMAND }
}

fn space(h: &mut Harness<'static, PhotocraftApp>, pressed: bool, m: Modifiers) {
    h.event(Event::ModifiersChanged(m));
    h.event(Event::Key { key: Key::Space, physical_key: None, pressed, repeat: false, modifiers: m });
    h.run_steps(1);
}

fn click(h: &mut Harness<'static, PhotocraftApp>, at: Pos2, m: Modifiers) {
    h.event(Event::ModifiersChanged(m));
    h.hover_at(at);
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: m });
        h.step();
    }
    h.run_steps(2);
}

fn selection(h: &Harness<'static, PhotocraftApp>) -> Rect {
    h.state().session.active().unwrap().doc.selection.as_ref().map_or(Rect::EMPTY, |s| s.content_bounds())
}

fn selected(h: &Harness<'static, PhotocraftApp>, x: i32, y: i32) -> f32 {
    h.state().session.active().unwrap().doc.selection.as_ref().map_or(0.0, |s| s.sample_channel(x, y, 0))
}

/// Document point (x, y) on screen in the main view.
fn screen(h: &Harness<'static, PhotocraftApp>, x: f32, y: f32) -> Pos2 {
    let app = h.state();
    let v = &app.ui.views[0];
    let xf = crate::canvas::ViewXform {
        rect: crate::rulers::content_rect(app, app.last_canvas_rect),
        zoom: v.zoom,
        center: v.center,
        flip: app.ui.view.flip_horizontal,
        rotation: v.rotation,
        aspect: app.ui.view.display_aspect(),
    };
    xf.to_screen(x, y)
}

#[test]
fn a_canvas_click_selects_what_shows_of_the_layer_under_it() {
    let (mut h, low, _) = harness();
    let (zoom, center) = (h.state().ui.views[0].zoom, h.state().ui.views[0].center);
    let steps = h.state().session.active().unwrap().history.entries().len();
    space(&mut h, true, cmd());
    assert_eq!(crate::hold_keys::held_tool(h.state(), &h.ctx), Some(crate::hold_keys::Temporary::LoadVisible));
    let at = screen(&h, 5.0, 5.0);
    click(&mut h, at, cmd());
    // Low shows where High doesn't cover it.
    assert_eq!(selection(&h), Rect::new(0, 0, 20, 20));
    assert_eq!((selected(&h, 5, 5), selected(&h, 15, 15)), (1.0, 0.0));
    // A drag doesn't pan.
    let (a, b) = (screen(&h, 40.0, 40.0), screen(&h, 50.0, 44.0));
    h.hover_at(a);
    h.step();
    h.event(Event::PointerButton { pos: a, button: PointerButton::Primary, pressed: true, modifiers: cmd() });
    h.step();
    for i in 1..=5 {
        h.hover_at(a + (b - a) * (i as f32 / 5.0));
        h.step();
    }
    h.event(Event::PointerButton { pos: b, button: PointerButton::Primary, pressed: false, modifiers: cmd() });
    h.step();
    space(&mut h, false, cmd());
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
    let app = h.state();
    assert_eq!((app.ui.views[0].zoom, app.ui.views[0].center), (zoom, center), "no zoom, no pan");
    assert_eq!(app.ui.tool, Tool::Brush);
    assert_eq!(app.session.active().unwrap().active_layer, Some(app.session.active().unwrap().active_layer.unwrap()));
    let hist = app.session.active().unwrap().history.entries();
    assert_eq!(hist.len(), steps + 1, "one step, nothing painted");
    assert!(app.session.active().unwrap().doc.layer(low).is_some());
}

#[test]
fn a_row_click_selects_the_layer_and_a_mask_thumbnail_the_mask() {
    let (mut h, low, high) = harness();
    let row = |h: &Harness<'static, PhotocraftApp>, id: LayerId| crate::layer_row_ui::recorded(&h.ctx).into_iter().find(|r| r.layer == id.0).unwrap();
    let r = row(&h, low).row;
    space(&mut h, true, cmd());
    click(&mut h, r.right_center() - vec2(40.0, 0.0), cmd());
    assert_eq!((selected(&h, 5, 5), selected(&h, 15, 15), selected(&h, 25, 25)), (1.0, 0.0, 0.0));
    assert_eq!(h.state().session.active().unwrap().active_layer, Some(high), "the click doesn't change the active layer");
    // ⇧ adds High (all of it shows).
    let r = row(&h, high).row;
    click(&mut h, r.right_center() - vec2(40.0, 0.0), cmd() | Modifiers::SHIFT);
    assert_eq!((selected(&h, 5, 5), selected(&h, 15, 15), selected(&h, 25, 25)), (1.0, 1.0, 1.0));
    space(&mut h, false, cmd());
    // Low's mask, revealing all: everything High doesn't cover.
    h.state_mut().run("layer.select", json!({"layer": low.0})).unwrap();
    h.state_mut().run("layer.layerMask.revealAll", json!({})).unwrap();
    h.run_steps(4);
    let m = crate::mask_thumbs_ui::recorded(&h.ctx, low.0).unwrap().0[0].1;
    space(&mut h, true, cmd());
    click(&mut h, m.center(), cmd());
    space(&mut h, false, cmd());
    assert_eq!((selected(&h, 5, 5), selected(&h, 15, 15), selected(&h, 50, 40)), (1.0, 0.0, 1.0));
}
