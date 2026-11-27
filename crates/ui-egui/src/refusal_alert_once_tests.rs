//! User report: "Could not use the brush tool because the target layer is hidden" opened three
//! times. One press reaches the tool up to three times through the real canvas (on the press, as
//! the drag starts, and as a click on the release); each refused one opened its own alert.

use egui::{Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::{DialogKind, Tool};

fn harness(app: PhotocraftApp) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(900.0, 600.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(4);
    let v = &mut h.state_mut().ui.views[0];
    v.zoom = 1.0;
    v.center = [100.0, 50.0];
    v.fit_pending = false;
    h.run_steps(2);
    h
}

fn screen(h: &Harness<'static, PhotocraftApp>, x: f32, y: f32) -> Pos2 {
    let app = h.state();
    let v = &app.ui.views[0];
    let xf = ViewXform {
        rect: crate::rulers::content_rect(app, app.last_canvas_rect),
        zoom: v.zoom,
        center: v.center,
        flip: app.ui.view.flip_horizontal,
        rotation: v.rotation,
        aspect: app.ui.view.display_aspect(),
    };
    xf.to_screen(x, y)
}

fn button(h: &mut Harness<'static, PhotocraftApp>, p: Pos2, down: bool) {
    h.event(egui::Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: down, modifiers: Modifiers::NONE });
    h.run_steps(1);
}

/// A press at `from`, moved through `steps` points to `to`, and the release there.
fn gesture(h: &mut Harness<'static, PhotocraftApp>, from: [f32; 2], to: [f32; 2], steps: usize) {
    let p = screen(h, from[0], from[1]);
    h.event(egui::Event::PointerMoved(p));
    h.run_steps(1);
    button(h, p, true);
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        h.event(egui::Event::PointerMoved(screen(h, from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t)));
        h.run_steps(1);
    }
    button(h, screen(h, to[0], to[1]), false);
    h.run_steps(2);
}

fn alerts(h: &Harness<'static, PhotocraftApp>) -> Vec<String> {
    h.state()
        .ui
        .dialogs
        .iter()
        .filter(|d| d.kind == DialogKind::Error)
        .filter_map(|d| d.fields.get("message").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// What a case does to the document before the press.
type Setup = dyn Fn(&mut PhotocraftApp);

/// A 200 × 100 document with a painted layer active, and `setup` run on it.
fn document(tool: Tool, setup: impl Fn(&mut PhotocraftApp)) -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 200, "height": 100, "background": "white"})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    app.run("edit.fill", json!({"color": "#dc1e1e"})).unwrap();
    setup(&mut app);
    app.ui.tool = tool;
    app.sync_views();
    app
}

#[test]
fn a_refused_press_opens_one_alert_whether_clicked_or_dragged() {
    let hide = |app: &mut PhotocraftApp| {
        let id = app.session.active().unwrap().active_layer.unwrap().0;
        app.run("layer.setProps", json!({"layer": id, "visible": false})).unwrap();
    };
    let lock = |app: &mut PhotocraftApp| {
        app.run("layer.lockLayers", json!({"all": true})).unwrap();
    };
    let group = |app: &mut PhotocraftApp| {
        app.run("layer.new.group", json!({})).unwrap();
    };
    let cases: [(&str, &Setup); 3] = [("hidden", &hide), ("locked", &lock), ("group", &group)];
    for (name, setup) in cases {
        for (how, to, steps) in [("click", [50.0, 50.0], 0), ("drag", [150.0, 60.0], 8), ("wobble", [51.0, 50.0], 3)] {
            let mut h = harness(document(Tool::Brush, setup));
            let rev = h.state().session.active().unwrap().revision;
            gesture(&mut h, [50.0, 50.0], to, steps);
            assert_eq!(alerts(&h).len(), 1, "{name}, {how}: one alert, not {:?}", alerts(&h));
            assert_eq!(h.state().session.active().unwrap().revision, rev, "{name}, {how}: nothing painted");
        }
    }
}
