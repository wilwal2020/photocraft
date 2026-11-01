//! Window › Libraries: the libraries of reusable items (`photocraft_engine::library_cmds`) in the
//! shared preset browser. Each library is a folder; its items show as thumbnails (colours as
//! swatches) with their names in the tooltip or, at the smallest size, the list.
//!
//! The header picks the library new items go to and holds the Add menu (selected layers, selected
//! or merged pixels, an image file, the active layer's style, the foreground colour). Clicking an
//! item selects it; double-clicking, or dragging it onto the canvas, uses it: layers and images
//! are placed (where they are dropped), a style is applied to the selected layers (⇧ adds to their
//! effects), a colour becomes the foreground. Folder and item context menus rename, delete and
//! move. Every action is a `library.*` engine command; this module only draws and dispatches.

use egui::{Color32, ColorImage, Pos2, Rect, TextureHandle, pos2};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::preset_panels::{Ev, GroupView, ItemView, Place, browser, cached_texture, canvas_drop_rect, run};
use crate::theme::Tokens;
use photocraft_engine::library_cmds::{Item, ItemKind};

/// The panel's key in the preset browser state (sizes, selection, open folders).
pub const PANEL: &str = "libraries";

fn thumb_texture(ctx: &egui::Context, item: &Item) -> Option<TextureHandle> {
    let t = item.thumb.as_ref()?;
    // An item's content never changes: its id is the cache key.
    Some(cached_texture(ctx, ("library", item.id), || ColorImage::from_rgba_unmultiplied([t.width as usize, t.height as usize], &t.pixels)))
}

fn c32(c: [f32; 4]) -> Color32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(q(c[0]), q(c[1]), q(c[2]))
}

/// `r` shrunk to `w`×`h` proportions, centred.
fn fit(r: Rect, w: u32, h: u32) -> Rect {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let s = (r.width() / w).min(r.height() / h);
    Rect::from_center_size(r.center(), egui::vec2(w * s, h * s))
}

fn item_id(key: &str) -> Value {
    key.parse::<u64>().map_or(Value::Null, |id| json!(id))
}

/// The Add menu's entries (its label, command and params).
fn add_menu(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    crate::widgets::style_spectrum_popup_menu(ui);
    ui.set_min_width(200.0);
    let has_layer = app.session.active().is_some_and(|d| d.active_layer.is_some());
    let entries: [(&str, &str, Value, bool); 5] = [
        (tl!("Selected Layers"), "library.addLayers", json!({}), has_layer),
        (tl!("Selected Pixels"), "library.addImage", json!({}), has_layer),
        (tl!("Merged Pixels"), "library.addImage", json!({"merged": true}), has_layer),
        (tl!("Layer Style"), "library.addStyle", json!({}), has_layer),
        (tl!("Foreground Color"), "library.addColor", json!({}), true),
    ];
    for (label, id, p, enabled) in entries {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            ui.close();
            let r = run(app, id, p);
            added(app, r);
        }
    }
    if ui.button(tl!("Image from File…")).clicked() {
        ui.close();
        let r = app.pick_file_bytes(|app, name, bytes| {
            let r = photocraft_engine::library_cmds::add_image_bytes(&mut app.session, &name, &bytes, &json!({})).map_err(|e| e.to_string())?;
            added(app, Some(r.clone()));
            Ok(r)
        });
        if let Err(e) = r
            && e != "cancelled"
        {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
}

/// A new item is selected and its folder shown.
fn added(app: &mut PhotocraftApp, r: Option<Value>) {
    let Some(id) = r.as_ref().and_then(|r| r.get("id")).and_then(Value::as_u64) else { return };
    app.ui.presets_ui.selected.insert(PANEL.into(), id.to_string());
}

/// Uses item `key`; `at` is where it was dropped on the canvas.
fn use_item(app: &mut PhotocraftApp, key: &str, at: Option<Pos2>, shift: bool) {
    let id = item_id(key);
    let kind = id.as_u64().and_then(|id| app.session.libraries.item(id)).map(|i| i.kind);
    let mut p = json!({"item": id});
    match kind {
        Some(ItemKind::Layers | ItemKind::Image) => {
            if let Some(c) = at.and_then(|pos| crate::canvas::doc_point_at(app, pos)) {
                p["at"] = json!(c);
            }
        }
        Some(ItemKind::Style) => p["add"] = json!(shift),
        _ => {}
    }
    run(app, "library.use", p);
}

/// The Libraries panel body.
pub fn libraries_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    // Header: the library new items go to, and the Add menu.
    let names: Vec<String> = app.session.libraries.libraries.iter().map(|l| l.name.clone()).collect();
    ui.horizontal(|ui| {
        if names.is_empty() {
            ui.label(egui::RichText::new(tl!("No libraries yet")).color(t.text_faint).size(11.5));
        } else {
            let mut current = app.session.libraries.current.min(names.len() - 1);
            let options: Vec<(usize, &str)> = names.iter().enumerate().map(|(i, n)| (i, n.as_str())).collect();
            if crate::widgets::dropdown_literal(ui, "library-current", tl!("Library"), &mut current, &options, (ui.available_width() - 34.0).max(80.0)) {
                run(app, "library.select", json!({"library": current}));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let resp = crate::icons::button(ui, "plus", 24.0, false, tl!("Add to library"));
            egui::Popup::menu(&resp).show(|ui| add_menu(app, ui));
        });
    });
    ui.add_space(4.0);
    let libs = app.session.libraries.libraries.clone();
    let groups: Vec<GroupView> = libs
        .iter()
        .map(|l| GroupView { name: l.name.clone(), items: l.items.iter().map(|i| ItemView { key: i.id.to_string(), name: i.name.clone() }).collect() })
        .collect();
    let texes: Vec<Vec<Option<TextureHandle>>> = libs.iter().map(|l| l.items.iter().map(|i| thumb_texture(&ctx, i)).collect()).collect();
    let canvas = canvas_drop_rect(app);
    let mut st = std::mem::take(&mut app.ui.presets_ui);
    let max_h = ctx.content_rect().height().max(120.0);
    let events = browser(
        ui,
        &mut st,
        PANEL,
        &groups,
        Place { canvas, max_h, new_tip: tl!("Add the selected layers to the library"), flush: false },
        &mut |ui, r, gi, ii| {
            let Some(item) = libs.get(gi).and_then(|l| l.items.get(ii)) else { return };
            ui.painter().rect_filled(r, 2.0, t.field);
            if let Some(c) = item.color {
                ui.painter().rect_filled(r.shrink(3.0), 2.0, c32(c));
            } else if let (Some(Some(tex)), Some(th)) = (texes.get(gi).and_then(|g| g.get(ii)), item.thumb.as_ref()) {
                let inner = fit(r.shrink(2.0), th.width, th.height);
                if item.kind != ItemKind::Style {
                    crate::widgets::checker(&ui.painter_at(inner), inner, 4.0);
                }
                ui.painter().image(tex.id(), inner, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
            }
        },
    );
    app.ui.presets_ui = st;
    if libs.is_empty() {
        ui.label(egui::RichText::new(tl!("Add layers, images, styles or colours with + to reuse them in any document.")).color(t.text_faint).size(11.0));
    }
    let shift = ui.input(|i| i.modifiers.shift);
    for e in events {
        match e {
            Ev::Select(k) => {
                app.ui.presets_ui.selected.insert(PANEL.into(), k);
            }
            Ev::Activate(k) => {
                app.ui.presets_ui.selected.insert(PANEL.into(), k.clone());
                use_item(app, &k, None, shift);
            }
            Ev::Drop(k, pos) => use_item(app, &k, Some(pos), shift),
            Ev::Rename(k, n) => drop(run(app, "library.renameItem", json!({"item": item_id(&k), "name": n}))),
            Ev::Delete(k) => {
                if run(app, "library.deleteItem", json!({"item": item_id(&k)})).is_some() {
                    app.ui.presets_ui.selected.remove(PANEL);
                }
            }
            Ev::Move(k, g) => drop(run(app, "library.moveItem", json!({"item": item_id(&k), "library": g}))),
            Ev::RenameGroup(g, n) => drop(run(app, "library.rename", json!({"library": g, "name": n}))),
            Ev::DeleteGroup(g) => drop(run(app, "library.delete", json!({"library": g}))),
            Ev::NewGroup => {
                if let Some(r) = run(app, "library.new", json!({"name": tl!("Library")})) {
                    let g = r["name"].as_str().unwrap_or_default().to_string();
                    app.ui.presets_ui.renaming = Some((PANEL.into(), format!("group:{g}"), g));
                }
            }
            Ev::New => {
                let r = run(app, "library.addLayers", json!({}));
                added(app, r);
            }
        }
    }
    if let Some(w) = app.session.libraries.take_warnings().into_iter().next() {
        app.ui.status = w;
        app.ui.status_error = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
        app.sync_views();
        app
    }

    fn double_click(h: &mut Harness<'_, PhotocraftApp>, at: Pos2) {
        h.run_steps(40);
        for _ in 0..2 {
            h.hover_at(at);
            h.step();
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
                h.step();
            }
            h.run_steps(3);
        }
    }

    /// The panel draws each library as a folder with its items (thumbnails and colour swatches,
    /// named for the accessibility tree), and double-clicking uses an item.
    #[test]
    fn the_panel_shows_libraries_with_their_items_and_double_click_uses_one() {
        let mut app = app();
        app.run("library.new", json!({"name": "Brand"})).unwrap();
        app.run("library.addLayers", json!({"name": "Logo"})).unwrap();
        app.run("library.addColor", json!({"color": "#ff8800", "name": "Orange"})).unwrap();
        app.run("library.new", json!({"name": "Icons"})).unwrap();
        let mut h = Harness::builder()
            .with_size(egui::vec2(300.0, 420.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(|ui, app: &mut PhotocraftApp| libraries_panel(app, ui), app);
        h.run_steps(3);
        for name in ["Brand", "Icons", "Logo", "Orange", "Add to library"] {
            assert!(h.query_all_by_label(name).next().is_some(), "{name} is drawn");
        }
        let logo = h.get_by_label("Logo").rect();
        assert!(logo.width() >= 20.0 && logo.height() >= 20.0, "a thumbnail cell: {logo:?}");

        let orange = h.get_by_label("Orange").rect().center();
        double_click(&mut h, orange);
        let fg = h.state().session.tools.foreground;
        assert!((fg[0] - 1.0).abs() < 1e-3 && (fg[1] - 0x88 as f32 / 255.0).abs() < 1e-3, "the colour became the foreground: {fg:?}");

        let layers = h.state().session.active().unwrap().doc.layers.len();
        double_click(&mut h, logo.center());
        assert_eq!(h.state().session.active().unwrap().doc.layers.len(), layers + 1, "the layer was placed");
    }

    #[test]
    fn an_empty_panel_explains_itself_and_the_window_menu_toggles_it() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert!(crate::menus::is_live("window.panel.libraries"));
        crate::menus::invoke(&mut app, &ctx, "window.panel.libraries", Value::Null).unwrap();
        assert!(app.ui.panels.libraries, "Window › Libraries opens its dock group");
        let mut h = Harness::builder().with_size(egui::vec2(300.0, 300.0)).build_ui_state(|ui, app: &mut PhotocraftApp| libraries_panel(app, ui), app);
        h.run_steps(2);
        assert!(h.query_by_label("No libraries yet").is_some());
        // The footer's New Group button creates a library and starts renaming it.
        h.get_by_label("Create new group").click();
        h.run_steps(2);
        assert_eq!(h.state().session.libraries.libraries.len(), 1);
        assert!(h.state().ui.presets_ui.renaming.is_some());
    }
}
