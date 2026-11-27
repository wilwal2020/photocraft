//! User report: opening a Solid Color fill's picker sometimes didn't work, "maybe if you've done
//! something right before trying to open one, like interacting with a group".

use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use photocraft_doc::LayerId;
use serde_json::json;

use crate::PhotocraftApp;

fn harness() -> (Harness<'static, PhotocraftApp>, LayerId, LayerId) {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "depth": 8})).unwrap();
    let paint = s.execute("layer.new.layer", json!({"name": "Paint"})).unwrap()["layer"].as_u64().unwrap();
    let group = LayerId(s.execute("layer.new.group", json!({"name": "G"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("layer.select", json!({"layer": group.0})).unwrap();
    s.execute("layer.new.layer", json!({"name": "Inside"})).unwrap();
    s.execute("layer.select", json!({"layer": paint})).unwrap();
    let fill = LayerId(s.execute("layer.newFillLayer.solidColor", json!({"color": "#336699"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("layer.select", json!({"layer": group.0})).unwrap();
    let mut h =
        Harness::builder().with_size(vec2(1440.0, 900.0)).with_pixels_per_point(1.0).with_step_dt(1.0 / 60.0).with_max_steps(64).build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            PhotocraftApp::new(s, crate::Services::default())
        });
    let ctx = h.ctx.clone();
    let (r, _) = crate::control::ControlRequest::new("ui.set", json!({"dock": {"collapsed": ["color", "properties", "history", "navigator"]}}));
    crate::control::handle(h.state_mut(), &ctx, &r);
    h.run_steps(40);
    (h, group, fill)
}

fn row(h: &Harness<'_, PhotocraftApp>, id: LayerId) -> crate::layer_row_ui::RowRects {
    let rows = crate::layer_row_ui::recorded(&h.ctx);
    let names: Vec<_> = h.state().session.active().unwrap().doc.layers.iter().map(|l| (l.id.0, l.name.clone())).collect();
    rows.iter().find(|r| r.layer == id.0).cloned().unwrap_or_else(|| panic!("no row for {}: rows {:?}, top layers {names:?}", id.0, rows.iter().map(|r| r.layer).collect::<Vec<_>>()))
}

fn click(h: &mut Harness<'_, PhotocraftApp>, at: Pos2, modifiers: Modifiers) {
    h.hover_at(at);
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers });
        h.step();
    }
}

fn double_click(h: &mut Harness<'_, PhotocraftApp>, at: Pos2) {
    for _ in 0..2 {
        click(h, at, Modifiers::NONE);
        h.run_steps(3);
    }
}

fn drag(h: &mut Harness<'_, PhotocraftApp>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(4);
}

fn picker_open(h: &Harness<'_, PhotocraftApp>, fill: LayerId) -> bool {
    h.state().ui.dialogs.iter().any(|d| crate::solid_fill_ui::owns(&d.fields) && d.fields["__params"]["layer"] == fill.0)
}

/// Each way of handling a group, then a double-click on the fill's thumbnail: the picker opens.
#[test]
fn the_solid_color_picker_opens_after_handling_a_group() {
    type Before = fn(&mut Harness<'_, PhotocraftApp>, crate::layer_row_ui::RowRects);
    let cases: [(&str, Before); 7] = [
        ("click the group", |h, g| click(h, g.name.map_or(g.row.center(), |n| n.center()), Modifiers::NONE)),
        ("click the group's arrow", |h, g| click(h, pos2(g.thumbnail.map_or(g.row.left() + 30.0, |t| t.left() - 9.0), g.row.center().y), Modifiers::NONE)),
        ("rename the group", |h, g| {
            double_click(h, g.name.map_or(g.row.center(), |n| n.center()));
            h.run_steps(30);
        }),
        ("Layer Style on the group, cancelled", |h, g| {
            double_click(h, pos2(g.row.right() - 60.0, g.row.center().y));
            h.run_steps(30);
            let id = h.state().ui.dialogs.last().map(|d| d.id);
            if let Some(id) = id {
                let _ = crate::dialogs::cancel(h.state_mut(), id);
            }
            h.run_steps(30);
        }),
        ("ctrl-click the group", |h, g| click(h, g.name.map_or(g.row.center(), |n| n.center()), Modifiers::COMMAND)),
        ("nudge the group row", |h, g| drag(h, g.row.center(), g.row.center() + vec2(2.0, 3.0))),
        ("drag the group a row down and back", |h, g| {
            drag(h, g.row.center(), g.row.center() + vec2(0.0, g.row.height() * 1.6));
            h.run_steps(30);
        }),
    ];
    let mut failed = Vec::new();
    for (name, before) in cases {
        let (mut h, group, fill) = harness();
        let g = row(&h, group);
        before(&mut h, g);
        h.run_steps(60);
        let thumb = row(&h, fill).thumbnail.unwrap().center();
        double_click(&mut h, thumb);
        h.run_steps(10);
        if !picker_open(&h, fill) {
            failed.push(format!("{name}: status {:?}, dialogs {:?}", h.state().ui.status, h.state().ui.dialogs.iter().map(|d| format!("{:?}", d.kind)).collect::<Vec<_>>()));
        }
    }
    assert!(failed.is_empty(), "no picker after:\n{}", failed.join("\n"));
}

/// Layer › New Fill Layer › Solid Color… with a group selected, or a layer inside it: the
/// picker opens on the new layer.
#[test]
fn a_new_solid_color_opens_its_picker_with_a_group_selected() {
    for inside in [false, true] {
        let (mut h, group, _) = harness();
        let ctx = h.ctx.clone();
        if inside {
            let child = h.state().session.active().unwrap().doc.layer(group).and_then(|g| g.children().and_then(|c| c.first().map(|l| l.id))).unwrap();
            h.state_mut().run("layer.select", json!({"layer": child.0})).unwrap();
        }
        h.run_steps(10);
        let opened = crate::menus::invoke(h.state_mut(), &ctx, "layer.newFillLayer.solidColor", json!({}));
        let opened = opened.unwrap_or_else(|e| panic!("inside {inside}: {e}"));
        let layer = opened["layer"].as_u64().unwrap();
        h.run_steps(10);
        assert!(picker_open(&h, LayerId(layer)), "inside {inside}: no picker");
    }
}

/// A fill inside a group opened by its arrow: double-clicking its thumbnail opens the picker.
#[test]
fn a_fill_inside_an_opened_group_opens_its_picker() {
    let (mut h, group, _) = harness();
    let made = h.state_mut().run("layer.select", json!({"layer": group.0}));
    made.unwrap();
    let child = h.state().session.active().unwrap().doc.layer(group).and_then(|g| g.children().and_then(|c| c.first().map(|l| l.id))).unwrap();
    h.state_mut().run("layer.select", json!({"layer": child.0})).unwrap();
    let fill = LayerId(h.state_mut().run("layer.newFillLayer.solidColor", json!({"color": "#aa3300"})).unwrap()["layer"].as_u64().unwrap());
    h.state_mut().run("layer.select", json!({"layer": group.0})).unwrap();
    h.run_steps(30);
    if crate::layer_row_ui::recorded(&h.ctx).iter().all(|r| r.layer != fill.0) {
        let g = row(&h, group);
        click(&mut h, pos2(g.thumbnail.map_or(g.row.left() + 30.0, |t| t.left() - 9.0), g.row.center().y), Modifiers::NONE);
        h.run_steps(30);
    }
    let thumb = row(&h, fill).thumbnail.unwrap().center();
    double_click(&mut h, thumb);
    h.run_steps(10);
    assert!(picker_open(&h, fill), "status {:?}", h.state().ui.status);
}
