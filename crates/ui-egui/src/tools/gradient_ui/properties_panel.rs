//! Live-gradient Properties controls laid out as Photoshop's (Gradient Controls, then Opacity
//! Controls, each with its own stops), sharing the editor's stop interaction.

use super::{
    Color32, EditorMode, Fill, GradientStyle, Layer, LayerId, Marker, PhotocraftApp, Rect, Sense, Sink, StripKind, Stroke, StrokeKind, Tokens, c32, cmds,
    edit_stop_color, fill_of, gf, json, paint_ramp, preset_stop_color, selection_after_edit, stops_editor_for, stops_editor_of, strip_key, toggle_editor, vec2,
    widgets,
};
use crate::{icons, props_layout, theme};

const STYLES: [(GradientStyle, &str); 5] = [
    (GradientStyle::Linear, "Linear Gradient"),
    (GradientStyle::Radial, "Radial Gradient"),
    (GradientStyle::Angle, "Angle Gradient"),
    (GradientStyle::Reflected, "Reflected Gradient"),
    (GradientStyle::Diamond, "Diamond Gradient"),
];

pub(super) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui, layer: &Layer) {
    let document = app.session.active().map(|s| s.doc.id);
    ui.push_id(("gradient-properties", document, layer.id.0), |ui| {
        layer_controls(app, ui, layer);
    });
}

/// The Gradient Editor window's controls for a selected gradient fill layer: the combined stops
/// strip and the selected stop's fields (the presets fill the window above them).
pub(super) fn editor(app: &mut PhotocraftApp, ui: &mut egui::Ui, layer: &Layer) {
    let document = app.session.active().map(|s| s.doc.id);
    ui.push_id(("gradient-editor-layer", document, layer.id.0), |ui| {
        if let Some(fill) = current_fill(app, layer.id) {
            stops_editor_for(app, ui, Sink::Layer(layer.id), fill);
        }
    });
}

/// Photoshop's Properties for a gradient fill (user request): Gradient Controls (the Presets
/// swatch, which opens the presets, style, angle, scale, Reverse and Dither, method and
/// smoothness, the colour stops with the selected one's location) and Opacity Controls (the
/// opacity stops with the selected one's opacity and location).
fn layer_controls(app: &mut PhotocraftApp, ui: &mut egui::Ui, layer: &Layer) {
    let Some(fill @ Fill::Gradient { angle, scale, style, reverse, dither, align, blend, .. }) = fill_of(layer).cloned() else { return };
    if props_layout::section(ui, "gradient-fill-controls", "Gradient Controls") {
        ui.horizontal(|ui| {
            row_label(ui, "Presets");
            if presets_swatch(ui, &fill).clicked() {
                toggle_editor(app, ui.ctx(), EditorMode::Presets);
            }
        });
        ui.horizontal(|ui| {
            row_label(ui, "Style");
            ui.spacing_mut().item_spacing.x = 3.0;
            for (value, name) in STYLES {
                if style_button(ui, value, value == style, name).clicked() {
                    let _ = app.run(cmds::SET, json!({"layer": layer.id.0, "style": cmds::style_name(value)}));
                }
            }
        });
        if let Some(value) = numeric_row(ui, "angle", "Angle", angle, -180.0..=180.0, "°") {
            let _ = app.run(cmds::SET, json!({"layer": layer.id.0, "angle": value}));
        }
        // Photoshop's field takes 1–1000 %; a gradient drawn shorter or longer with its handles
        // shows its own scale without the field clamping (and so rewriting) it.
        let (min, max) = cmds::SCALE_RANGE;
        let (min, max) = (min * 100.0, max * 100.0);
        let pct = scale * 100.0;
        let range = if pct.is_finite() { min.min(pct)..=max.max(pct) } else { min..=max };
        if let Some(value) = numeric_row(ui, "scale", "Scale", pct, range, "%") {
            let _ = app.run(cmds::SET, json!({"layer": layer.id.0, "scale": value}));
        }
        ui.horizontal_wrapped(|ui| {
            let mut value = reverse;
            if widgets::checkbox(ui, &mut value, "Reverse").changed() {
                let _ = app.run(cmds::SET, json!({"layer": layer.id.0, "reverse": value}));
            }
            let mut value = dither;
            if widgets::checkbox(ui, &mut value, "Dither").changed() {
                let _ = app.run(cmds::SET, json!({"layer": layer.id.0, "dither": value}));
            }
            let mut value = align;
            if widgets::checkbox(ui, &mut value, "Align with layer").changed() {
                let _ = app.run(cmds::SET, json!({"layer": layer.id.0, "align": value}));
            }
        });
        if widgets::secondary_button(ui, "Reset Alignment", ui.available_width()).on_hover_text(tl!("Centre the gradient (offset 0, 0)")).clicked() {
            let _ = app.run(cmds::SET, json!({"layer": layer.id.0, "offset": [0, 0]}));
        }
        ui.add_space(theme::ROW_GAP);
        widgets::hairline(ui);
        ui.add_space(theme::ROW_GAP);
        // Photoshop's interpolation: how the colours blend between the stops (a gradient from
        // Photoshop keeps its stops and its method, user report).
        ui.horizontal(|ui| {
            row_label(ui, "Method");
            use photocraft_doc::effects::GradientMethod as M;
            let mut method = blend.method;
            let opts = [(M::Classic, tl!("Classic")), (M::Perceptual, tl!("Perceptual")), (M::Linear, tl!("Linear")), (M::Smooth, tl!("Smooth"))];
            if widgets::dropdown(ui, "gradient-fill-method", tl!("Method:"), &mut method, &opts, ui.available_width().min(120.0)) {
                let _ = app.run(cmds::SET, json!({"layer": layer.id.0, "method": method.name()}));
            }
        });
        if let Some(value) = numeric_row(ui, "smoothness", "Smoothness", blend.smooth() * 100.0, 0.0..=100.0, "%") {
            let _ = app.run(cmds::SET, json!({"layer": layer.id.0, "smoothness": value}));
        }
        ui.add_space(theme::ROW_GAP);
        if let Some(fill) = current_fill(app, layer.id) {
            stops_editor_of(app, ui, Sink::Layer(layer.id), fill, StripKind::Color);
        }
    }
    if props_layout::section(ui, "gradient-fill-opacity", "Opacity Controls")
        && let Some(fill) = current_fill(app, layer.id)
    {
        stops_editor_of(app, ui, Sink::Layer(layer.id), fill, StripKind::Opacity);
    }
}

/// The Presets row's swatch: the layer's gradient with a menu arrow. A click opens the presets.
fn presets_swatch(ui: &mut egui::Ui, fill: &Fill) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width().clamp(80.0, 220.0);
    let (r, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
    let ramp_rect = Rect::from_min_max(r.min, egui::pos2(r.right() - 20.0, r.bottom()));
    let mut shown = fill.clone();
    if let Fill::Gradient { reverse, .. } = &mut shown {
        *reverse = false;
    }
    if let Some(ramp) = gf::Ramp::new(&shown) {
        paint_ramp(ui.painter(), ramp_rect, |u| ramp.sample(u));
    }
    let arrow = Rect::from_min_max(egui::pos2(ramp_rect.right(), r.top()), r.max);
    ui.painter().rect_filled(arrow, 0.0, if resp.hovered() { t.hover } else { t.field });
    widgets::chevron_icon(ui, arrow, &ui.style().visuals.widgets.inactive, false);
    ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, if resp.hovered() { t.text_dim } else { t.field_border }), StrokeKind::Inside);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tl!("Gradient presets")));
    resp.on_hover_text(tl!("Click to open the gradient presets"))
}

fn current_fill(app: &PhotocraftApp, id: LayerId) -> Option<Fill> {
    fill_of(app.session.active()?.doc.layer(id)?).cloned()
}

fn row_label(ui: &mut egui::Ui, name: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    ui.add_sized(vec2(72.0, 24.0), egui::Label::new(egui::RichText::new(tl!(name)).color(t.text_dim)).halign(egui::Align::Min))
}

/// Scrubbing/typing commits once, as the existing Properties fields do.
fn numeric_row(ui: &mut egui::Ui, id: &str, name: &str, current: f32, range: std::ops::RangeInclusive<f32>, suffix: &str) -> Option<f32> {
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            let label = row_label(ui, name);
            let key = ui.scope_id().with("draft");
            let mut value = ui.data(|d| d.get_temp::<f32>(key)).unwrap_or(current);
            let response = widgets::value_field(ui, &mut value, range, suffix, ui.available_width().min(88.0)).labelled_by(label.id);
            if response.dragged() || response.has_focus() {
                ui.data_mut(|d| d.insert_temp(key, value));
                return None;
            }
            ui.data_mut(|d| d.remove::<f32>(key));
            (response.drag_stopped() || response.lost_focus() || response.changed()).then_some(value).filter(|v| (*v - current).abs() > 1e-3)
        })
        .inner
    })
    .inner
}

/// The gradient presets by folder, as swatches: a click selects one (`gradient.presets.select`),
/// which also puts it on the selected gradient fill layer, as in Photoshop's Gradient Editor.
/// Folders start closed but the first; each keeps its state.
pub(super) fn presets(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let (fg, bg) = (app.session.tools.foreground, app.session.tools.background);
    let groups = app.session.presets.gradients.clone();
    let current = app.session.presets.gradient.name.clone();
    let mut picked = None;
    // The Gradient Editor window's height is the user's: the presets take all of it (user
    // report: the window couldn't be made taller than a fixed list height).
    egui::ScrollArea::vertical().id_salt("gradient-presets").auto_shrink([false, false]).show(ui, |ui| {
        for (gi, g) in groups.iter().enumerate() {
            egui::CollapsingHeader::new(egui::RichText::new(&g.name).color(t.text_dim)).id_salt(("gradient-preset-group", gi)).default_open(gi == 0).show(
                ui,
                |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                        for item in &g.items {
                            let (r, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), egui::Sense::click());
                            crate::preset_panels::paint_gradient(ui, r.shrink(1.0), &item.resolve(fg, bg));
                            let selected = item.name == current;
                            let stroke = if selected {
                                egui::Stroke::new(2.0, t.accent)
                            } else if resp.hovered() {
                                egui::Stroke::new(1.0, t.text)
                            } else {
                                egui::Stroke::new(1.0, t.card_border)
                            };
                            ui.painter().rect_stroke(r, 2.0, stroke, egui::StrokeKind::Inside);
                            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &item.name));
                            if resp.on_hover_text(&item.name).clicked() {
                                picked = Some(item.name.clone());
                            }
                        }
                    });
                },
            );
        }
    });
    if let Some(name) = picked
        && let Err(e) = app.run("gradient.presets.select", json!({"preset": name}))
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Small procedural previews: no proprietary icon assets, colours follow the theme.
fn style_button(ui: &mut egui::Ui, style: GradientStyle, selected: bool, name: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(25.0, 25.0), Sense::click());
    icons::button_chrome(ui, rect, selected, response.hovered());
    let preview = rect.shrink(5.0);
    // One mesh avoids antialiased seams between the small preview's cells.
    let mut mesh = egui::Mesh::default();
    const N: usize = 16;
    for y in 0..N {
        for x in 0..N {
            let (u, v) = ((x as f32 + 0.5) / N as f32, (y as f32 + 0.5) / N as f32);
            let (dx, dy) = ((u - 0.5) * 2.0, (v - 0.5) * 2.0);
            let amount = match style {
                GradientStyle::Linear => u,
                GradientStyle::Radial => dx.hypot(dy).min(1.0),
                GradientStyle::Angle => (dy.atan2(dx) / std::f32::consts::TAU + 1.0).fract(),
                GradientStyle::Reflected => dx.abs(),
                GradientStyle::Diamond => (dx.abs() + dy.abs()).min(1.0),
            };
            let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount) as u8;
            let color = Color32::from_rgb(channel(t.field.r(), t.text.r()), channel(t.field.g(), t.text.g()), channel(t.field.b(), t.text.b()));
            let min = preview.min + vec2(x as f32, y as f32) * preview.size() / N as f32;
            mesh.add_colored_rect(Rect::from_min_size(min, preview.size() / N as f32), color);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
    ui.painter().rect_stroke(preview, 0.0, Stroke::new(1.0, t.text_dim), StrokeKind::Outside);
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), selected, tl!(name)));
    response.on_hover_text(tl!(name))
}

pub(super) fn stop_fields(app: &mut PhotocraftApp, ui: &mut egui::Ui, sink: Sink) {
    let Some(Fill::Gradient { mut stops, mut opacity_stops, midpoints, .. }) = sink.fill(app) else { return };
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    if opacity_stops.is_empty() {
        opacity_stops = vec![(0.0, 1.0), (1.0, 1.0)];
    }
    opacity_stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    let key = strip_key(app, sink).with("selection");
    let selected = ui.data(|d| d.get_temp::<Marker>(key)).unwrap_or(Marker::Color(0));
    let (index, location, count) = match selected {
        Marker::Color(i) => {
            let Some((location, color)) = stops.get(i) else {
                ui.data_mut(|d| d.remove::<Marker>(key));
                return;
            };
            ui.horizontal(|ui| {
                row_label(ui, "Color");
                let (rect, response) = ui.allocate_exact_size(vec2(88.0, 24.0), Sense::click());
                let rgb = color.to_rgb();
                ui.painter().rect_filled(rect.shrink(2.0), Tokens::get(ui.ctx()).radius_sm, c32([rgb[0], rgb[1], rgb[2], 1.0]));
                ui.painter().rect_stroke(rect, Tokens::get(ui.ctx()).radius_sm, Stroke::new(1.0, Tokens::get(ui.ctx()).field_border), StrokeKind::Inside);
                response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tl!("Color")));
                if response.on_hover_text(tl!("Color")).clicked() {
                    match sink {
                        Sink::Layer(id) => edit_stop_color(app, id, i),
                        Sink::Preset => preset_stop_color(app, i),
                    }
                }
            });
            (i, *location, stops.len())
        }
        Marker::Opacity(i) => {
            let Some((location, opacity)) = opacity_stops.get(i) else {
                ui.data_mut(|d| d.remove::<Marker>(key));
                return;
            };
            if let Some(value) = numeric_row(ui, &format!("opacity-{i}"), "Opacity", *opacity * 100.0, 0.0..=100.0, "%") {
                let _ = app.run(sink.cmd(), sink.params(json!({"action": "opacity", "kind": "opacity", "index": i, "opacity": value})));
            }
            (i, *location, opacity_stops.len())
        }
        Marker::Mid(i) => {
            if stops.get(i).is_none() || stops.get(i.saturating_add(1)).is_none() {
                ui.data_mut(|d| d.remove::<Marker>(key));
                return;
            }
            if let Some(value) = numeric_row(ui, &format!("midpoint-{i}"), "Midpoint", midpoints.get(i).copied().unwrap_or(0.5) * 100.0, 5.0..=95.0, "%") {
                let _ = app.run(sink.cmd(), sink.params(json!({"action": "midpoint", "index": i, "location": value / 100.0})));
            }
            return;
        }
    };
    let kind = if matches!(selected, Marker::Opacity(_)) { "opacity" } else { "color" };
    if let Some(value) = numeric_row(ui, &format!("{kind}-location-{index}"), "Location", location * 100.0, 0.0..=100.0, "%") {
        let p = sink.params(json!({"action": "move", "kind": kind, "index": index, "location": value / 100.0}));
        let selection = sink.fill(app).and_then(|f| selection_after_edit(&f, &p, Some(selected)));
        if app.run(sink.cmd(), p).is_ok() {
            if let Some(marker) = selection {
                ui.data_mut(|d| d.insert_temp(key, marker));
            }
            // Index belongs to the old ordering; render the remaining controls next frame.
            return;
        }
    }
    ui.horizontal(|ui| {
        ui.add_space(72.0 + ui.spacing().item_spacing.x);
        ui.add_enabled_ui(count > 2, |ui| {
            let response = icons::button(ui, "trash", 24.0, false, "Delete");
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tl!("Delete")));
            if response.clicked() {
                let _ = app.run(sink.cmd(), sink.params(json!({"action": "delete", "kind": kind, "index": index})));
                ui.data_mut(|d| d.remove::<Marker>(key));
            }
        });
    });
    ui.add_space(theme::ROW_GAP);
}

/// The selected marker of a single-kind strip, if it is still there.
fn selected_marker(ui: &egui::Ui, key: egui::Id) -> Option<Marker> {
    ui.data(|d| d.get_temp::<Marker>(key))
}

/// The Gradient Controls' selected colour stop: its location and the delete button (a midpoint:
/// its position). Double-click a stop for its colour, as in Photoshop.
pub(super) fn color_fields(app: &mut PhotocraftApp, ui: &mut egui::Ui, sink: Sink, key: egui::Id) {
    let Some(Fill::Gradient { mut stops, midpoints, .. }) = sink.fill(app) else { return };
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    match selected_marker(ui, key) {
        Some(Marker::Color(i)) => {
            let Some((location, _)) = stops.get(i) else {
                ui.data_mut(|d| d.remove::<Marker>(key));
                return;
            };
            location_row(app, ui, sink, key, Marker::Color(i), *location, stops.len());
        }
        Some(Marker::Mid(i)) if stops.get(i.saturating_add(1)).is_some() => {
            if let Some(value) = numeric_row(ui, &format!("midpoint-{i}"), "Midpoint", midpoints.get(i).copied().unwrap_or(0.5) * 100.0, 5.0..=95.0, "%") {
                let _ = app.run(sink.cmd(), sink.params(json!({"action": "midpoint", "index": i, "location": value / 100.0})));
            }
        }
        _ => {
            ui.data_mut(|d| d.insert_temp(key, Marker::Color(0)));
        }
    }
}

/// The Opacity Controls' selected opacity stop: its opacity, location and delete button.
pub(super) fn opacity_fields(app: &mut PhotocraftApp, ui: &mut egui::Ui, sink: Sink, key: egui::Id) {
    let Some(Fill::Gradient { mut opacity_stops, .. }) = sink.fill(app) else { return };
    if opacity_stops.is_empty() {
        opacity_stops = vec![(0.0, 1.0), (1.0, 1.0)];
    }
    opacity_stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    let Some(Marker::Opacity(i)) = selected_marker(ui, key) else {
        ui.data_mut(|d| d.insert_temp(key, Marker::Opacity(0)));
        return;
    };
    let Some((location, opacity)) = opacity_stops.get(i).copied() else {
        ui.data_mut(|d| d.remove::<Marker>(key));
        return;
    };
    if let Some(value) = numeric_row(ui, &format!("opacity-{i}"), "Opacity", opacity * 100.0, 0.0..=100.0, "%") {
        let _ = app.run(sink.cmd(), sink.params(json!({"action": "opacity", "kind": "opacity", "index": i, "opacity": value})));
    }
    location_row(app, ui, sink, key, Marker::Opacity(i), location, opacity_stops.len());
}

/// Location of the selected stop, with Photoshop's delete button beside it.
fn location_row(app: &mut PhotocraftApp, ui: &mut egui::Ui, sink: Sink, key: egui::Id, marker: Marker, location: f32, count: usize) {
    let (index, kind) = match marker {
        Marker::Opacity(i) => (i, "opacity"),
        Marker::Color(i) | Marker::Mid(i) => (i, "color"),
    };
    ui.push_id((kind, "location", index), |ui| {
        ui.horizontal(|ui| {
            let label = row_label(ui, "Location");
            let draft = ui.scope_id().with("draft");
            let current = location * 100.0;
            let mut value = ui.data(|d| d.get_temp::<f32>(draft)).unwrap_or(current);
            let response = widgets::value_field(ui, &mut value, 0.0..=100.0, "%", ui.available_width().min(88.0) - 30.0).labelled_by(label.id);
            let commit = if response.dragged() || response.has_focus() {
                ui.data_mut(|d| d.insert_temp(draft, value));
                None
            } else {
                ui.data_mut(|d| d.remove::<f32>(draft));
                (response.drag_stopped() || response.lost_focus() || response.changed()).then_some(value).filter(|v| (*v - current).abs() > 1e-3)
            };
            ui.add_enabled_ui(count > 2, |ui| {
                let delete = icons::button(ui, "trash", 24.0, false, "Delete");
                delete.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tl!("Delete")));
                if delete.clicked() {
                    let _ = app.run(sink.cmd(), sink.params(json!({"action": "delete", "kind": kind, "index": index})));
                    ui.data_mut(|d| d.remove::<Marker>(key));
                }
            });
            if let Some(value) = commit {
                let p = sink.params(json!({"action": "move", "kind": kind, "index": index, "location": value / 100.0}));
                let selection = sink.fill(app).and_then(|f| selection_after_edit(&f, &p, Some(marker)));
                if app.run(sink.cmd(), p).is_ok()
                    && let Some(marker) = selection
                {
                    ui.data_mut(|d| d.insert_temp(key, marker));
                }
            }
        });
    });
    ui.add_space(theme::ROW_GAP);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;
    use egui_kittest::{
        Harness,
        kittest::{NodeT, Queryable},
    };

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 200, "height": 120})).unwrap();
        app.run(cmds::CREATE, json!({"from": [20, 60], "to": [180, 60], "stops": [[0, "#ff0000"], [1, "#0000ff"]], "transparency": [[0, 20], [1, 100]]}))
            .unwrap();
        app
    }

    fn panel(ui: &mut egui::Ui, app: &mut PhotocraftApp) {
        if ui.ctx().cumulative_pass_nr() == 0 {
            // Font definitions take effect on the next pass, before section headers draw.
            PhotocraftApp::setup_context(ui.ctx(), theme::ThemeKind::ProMedium);
            return;
        }
        let state = app.session.active().unwrap();
        let layer = state.doc.layer(state.active_layer.unwrap()).unwrap().clone();
        show(app, ui, &layer);
    }

    fn fill(app: &PhotocraftApp) -> Fill {
        let state = app.session.active().unwrap();
        current_fill(app, state.active_layer.unwrap()).unwrap()
    }

    /// The y of a single strip's markers, from its top.
    const MARKER_Y: f32 = 39.0;

    fn click_strip(h: &mut Harness<'_, PhotocraftApp>, strip: &str, location: f32, y: f32) {
        let rect = h.get_by_label(strip).rect();
        let pos = pos2(rect.left() + 8.0 + (rect.width() - 16.0) * location, rect.top() + y);
        h.event(egui::Event::PointerMoved(pos));
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
    }

    /// User request: the Presets swatch opens the Gradient Editor's presets alone, as Photoshop's
    /// picker does; a preset there puts it on the layer, in one step, and a click outside closes
    /// the editor.
    #[test]
    fn the_presets_swatch_opens_the_presets_and_a_preset_goes_on_the_layer() {
        let app = app();
        let group = app.session.presets.gradients[0].clone();
        let preset = group.items.iter().find(|i| i.name != app.session.presets.gradient.name).unwrap().clone();
        let mut h = Harness::builder().with_size(vec2(900.0, 900.0)).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                ui.allocate_ui(vec2(320.0, 880.0), |ui| panel(ui, app));
                let ctx = ui.ctx().clone();
                super::super::editor_window(app, &ctx);
            },
            app,
        );
        h.run_steps(3);
        assert!(h.query_by_label(&preset.name).is_none(), "the panel has no preset list of its own");
        h.get_by_label("Gradient presets").click();
        h.run_steps(3);
        assert!(h.state().ui.panels.gradient_editor && h.state().gradient.editor == EditorMode::Presets, "the swatch opened the presets");
        assert!(h.query_by_label("Gradient stops").is_none(), "the presets alone, not the stops");
        let past = h.state().session.active().unwrap().history.past_len();
        h.get_by_label(&preset.name).click();
        h.run_steps(3);
        let (colour, _) = preset.fill_stops(h.state().session.tools.foreground, h.state().session.tools.background);
        let want: Vec<[f32; 4]> = colour
            .iter()
            .map(|s| {
                let c = s.1.to_rgb();
                [c[0], c[1], c[2], s.1.alpha]
            })
            .collect();
        let Fill::Gradient { stops, .. } = fill(h.state()) else { panic!() };
        let got: Vec<[f32; 4]> = stops
            .iter()
            .map(|s| {
                let c = s.1.to_rgb();
                [c[0], c[1], c[2], s.1.alpha]
            })
            .collect();
        assert_eq!(got.len(), want.len(), "the preset's stops");
        assert!(got.iter().zip(&want).all(|(a, b)| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-3)), "{got:?} vs {want:?}");
        assert_eq!(h.state().session.active().unwrap().history.past_len(), past + 1, "one step");
        assert_eq!(h.state().session.presets.gradient.name, preset.name);
        assert!(h.state().ui.panels.gradient_editor, "a click inside keeps it open");
        // A click outside it (the empty right of the panel) closes it.
        let outside = egui::pos2(880.0, 880.0);
        h.event(egui::Event::PointerMoved(outside));
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(2);
        assert!(!h.state().ui.panels.gradient_editor, "a click outside closed it");
    }

    /// A Photoshop gradient keeps its method: the panel shows it and changes it in one step.
    #[test]
    fn method_and_smoothness_edit_the_gradient() {
        let mut app = app();
        let id = app.session.active().unwrap().active_layer.unwrap();
        app.run(cmds::SET, json!({"layer": id.0, "method": "perceptual", "smoothness": 40})).unwrap();
        let Fill::Gradient { blend, stops, .. } = fill(&app) else { panic!() };
        assert_eq!(blend.method, photocraft_doc::effects::GradientMethod::Perceptual);
        assert!((blend.smoothness - 0.4).abs() < 1e-6);
        let n = stops.len();
        // Editing a stop keeps the method; the stops stay the user's own.
        app.run(cmds::SET, json!({"layer": id.0, "reverse": true})).unwrap();
        let Fill::Gradient { blend, stops, .. } = fill(&app) else { panic!() };
        assert_eq!((blend.method, stops.len()), (photocraft_doc::effects::GradientMethod::Perceptual, n));
        assert!(app.run(cmds::SET, json!({"layer": id.0, "method": "wavy"})).is_err());
        let mut h = Harness::builder().with_size(vec2(320.0, 700.0)).build_ui_state(panel, app);
        h.run_steps(3);
        assert!(h.query_by_label("Method").is_some() && h.query_by_label("Smoothness").is_some());
    }

    #[test]
    fn style_icons_edit_the_selected_layer_with_one_undo_step() {
        let mut h = Harness::builder().with_size(vec2(280.0, 800.0)).build_ui_state(panel, app());
        PhotocraftApp::setup_context(&h.ctx, theme::ThemeKind::ProMedium);
        h.run_steps(3);
        for (style, name) in STYLES.into_iter().skip(1) {
            let before = fill(h.state());
            let count = h.state().session.active().unwrap().history.past_len();
            h.get_by_label(name).click();
            h.run_steps(3);
            let after = fill(h.state());
            assert!(matches!(&after, Fill::Gradient { style: actual, .. } if *actual == style));
            assert_eq!(h.state().session.active().unwrap().doc.layers.len(), 2);
            assert_eq!(h.state().session.active().unwrap().history.past_len(), count + 1);
            h.state_mut().run("edit.undo", json!({})).unwrap();
            assert_eq!(fill(h.state()), before);
            h.state_mut().run("edit.redo", json!({})).unwrap();
            assert_eq!(fill(h.state()), after);
            h.run_steps(2);
        }
    }

    /// Photoshop's layout (user request): the colour stops under Gradient Controls, the opacity
    /// stops under Opacity Controls, each strip adding its own kind; a double-click on a colour
    /// stop opens its Color Picker.
    #[test]
    fn color_and_opacity_strips_add_their_own_kind_and_a_double_click_picks_a_colour() {
        // Real frame times, so two clicks make a double-click.
        let mut h = Harness::builder().with_size(vec2(280.0, 900.0)).with_step_dt(1.0 / 60.0).build_ui_state(panel, app());
        PhotocraftApp::setup_context(&h.ctx, theme::ThemeKind::ProMedium);
        h.run_steps(3);
        assert!(h.query_by_label("Gradient stops").is_none(), "no combined strip in the panel");
        assert!(h.get_by_label("Color stops").rect().bottom() < h.get_by_label("Opacity Controls").rect().top());
        assert!(h.get_by_label("Opacity Controls").rect().bottom() < h.get_by_label("Opacity stops").rect().top());
        for opacity in [false, true] {
            let before = fill(h.state());
            let count = h.state().session.active().unwrap().history.past_len();
            click_strip(&mut h, if opacity { "Opacity stops" } else { "Color stops" }, 0.5, 12.0);
            let after = fill(h.state());
            let Fill::Gradient { stops: old_colors, opacity_stops: old_opacity, .. } = &before else { panic!() };
            let Fill::Gradient { stops: colors, opacity_stops, .. } = &after else { panic!() };
            if opacity {
                assert_eq!(colors, old_colors);
                assert_eq!(opacity_stops.len(), old_opacity.len() + 1);
            } else {
                assert_eq!(opacity_stops, old_opacity);
                assert_eq!(colors.len(), old_colors.len() + 1);
            }
            assert_eq!(h.state().session.active().unwrap().history.past_len(), count + 1);
            h.state_mut().run("edit.undo", json!({})).unwrap();
            assert_eq!(fill(h.state()), before);
            h.state_mut().run("edit.redo", json!({})).unwrap();
            assert_eq!(fill(h.state()), after);
            h.run_steps(2);
        }
        // Selecting a stop edits nothing; double-clicking a colour stop opens its picker.
        let before = fill(h.state());
        let count = h.state().session.active().unwrap().history.past_len();
        click_strip(&mut h, "Color stops", 0.0, MARKER_Y);
        assert_eq!(fill(h.state()), before);
        assert_eq!(h.state().session.active().unwrap().history.past_len(), count);
        let rect = h.get_by_label("Color stops").rect();
        let pos = pos2(rect.left() + 8.0, rect.top() + MARKER_Y);
        // Well past the double-click time since the last click.
        h.run_steps(40);
        h.hover_at(pos);
        h.run_steps(1);
        for _ in 0..2 {
            h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
            h.run_steps(1);
            h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
            h.run_steps(1);
        }
        h.run_steps(2);
        assert!(!h.state().ui.dialogs.is_empty(), "the stop's Color Picker");
    }

    #[test]
    fn opacity_numeric_edit_commits_once_and_undo_restores_the_gradient() {
        let mut h = Harness::builder().with_size(vec2(280.0, 800.0)).build_ui_state(panel, app());
        PhotocraftApp::setup_context(&h.ctx, theme::ThemeKind::ProMedium);
        h.run_steps(3);
        click_strip(&mut h, "Opacity stops", 0.0, MARKER_Y);
        let before = fill(h.state());
        let count = h.state().session.active().unwrap().history.past_len();
        // Angle, Scale, Smoothness, the colour stop's Location, then the opacity stop's Opacity.
        h.get_all_by_role(egui::accesskit::Role::SpinButton).nth(4).unwrap().click();
        h.run_steps(2);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("35".into()));
        h.run_steps(2);
        assert_eq!(fill(h.state()), before, "typing waits until committed");
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert!(matches!(fill(h.state()), Fill::Gradient { opacity_stops, .. } if (opacity_stops[0].1 - 0.35).abs() < 1e-5));
        assert_eq!(h.state().session.active().unwrap().history.past_len(), count + 1);
        h.state_mut().run("edit.undo", json!({})).unwrap();
        assert_eq!(fill(h.state()), before);
    }

    #[test]
    fn opacity_delete_is_one_undo_step_and_preserves_two_stop_minimum() {
        let mut app = app();
        app.run(cmds::STOP, json!({"action": "add", "kind": "opacity", "location": 0.5})).unwrap();
        let mut h = Harness::builder().with_size(vec2(280.0, 800.0)).build_ui_state(panel, app);
        PhotocraftApp::setup_context(&h.ctx, theme::ThemeKind::ProMedium);
        h.run_steps(3);
        // The colour stop's Delete, then the opacity stop's.
        assert!(h.get_all_by_label("Delete").next().unwrap().accesskit_node().is_disabled(), "two colors cannot be reduced further");
        click_strip(&mut h, "Opacity stops", 0.0, MARKER_Y);
        assert!(!h.get_all_by_label("Delete").nth(1).unwrap().accesskit_node().is_disabled(), "three opacity points can be reduced");
        let before = fill(h.state());
        let count = h.state().session.active().unwrap().history.past_len();
        h.get_all_by_label("Delete").nth(1).unwrap().click();
        h.run_steps(3);
        let after = fill(h.state());
        let Fill::Gradient { stops: old_colors, opacity_stops: old_opacity, .. } = &before else { panic!() };
        let Fill::Gradient { stops: colors, opacity_stops, .. } = &after else { panic!() };
        assert_eq!(colors, old_colors);
        assert_eq!(opacity_stops, &old_opacity[1..]);
        assert_eq!(h.state().session.active().unwrap().history.past_len(), count + 1);
        click_strip(&mut h, "Opacity stops", 1.0, MARKER_Y);
        assert!(h.get_all_by_label("Delete").nth(1).unwrap().accesskit_node().is_disabled(), "two opacity points cannot be reduced further");
        h.state_mut().run("edit.undo", json!({})).unwrap();
        assert_eq!(fill(h.state()), before);
        h.run_steps(3);
        assert!(!h.get_all_by_label("Delete").nth(1).unwrap().accesskit_node().is_disabled());
        h.state_mut().run("edit.redo", json!({})).unwrap();
        assert_eq!(fill(h.state()), after);
    }

    #[test]
    fn controls_fit_a_narrow_panel_in_every_theme() {
        assert!(icons::exists("trash"), "the stop deletion button has an embedded icon");
        for theme in theme::ThemeKind::ALL {
            let mut h = Harness::builder().with_size(vec2(260.0, 820.0)).build_ui_state(panel, app());
            PhotocraftApp::setup_context(&h.ctx, theme);
            h.run_steps(3);
            for (_, name) in STYLES {
                let rect = h.get_by_label(name).rect();
                assert!(rect.right() <= 260.0 && rect.left() >= 0.0, "{theme:?}: {name} {rect:?}");
            }
            for strip in ["Color stops", "Opacity stops", "Gradient presets"] {
                let rect = h.get_by_label(strip).rect();
                assert!(rect.right() <= 260.0, "{theme:?}: {strip} {rect:?}");
            }
        }
    }

    /// Pictures of the panel and the editor for a look (PHOTOCRAFT_SHOTS=<dir>).
    #[test]
    #[ignore = "pictures"]
    fn render_the_gradient_panels() {
        let Ok(dir) = std::env::var("PHOTOCRAFT_SHOTS") else { return };
        let mut h = Harness::builder().with_size(vec2(300.0, 760.0)).build_ui_state(panel, app());
        PhotocraftApp::setup_context(&h.ctx, theme::ThemeKind::ProMedium);
        h.run_steps(4);
        h.render().unwrap().save(format!("{dir}/fill_panel.png")).unwrap();
        let mut h = Harness::builder().with_size(vec2(700.0, 700.0)).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                ui.allocate_ui(vec2(300.0, 680.0), |ui| panel(ui, app));
                let ctx = ui.ctx().clone();
                super::super::editor_window(app, &ctx);
            },
            app(),
        );
        PhotocraftApp::setup_context(&h.ctx, theme::ThemeKind::ProMedium);
        h.run_steps(4);
        h.get_by_label("Gradient presets").click();
        h.run_steps(4);
        h.render().unwrap().save(format!("{dir}/fill_presets.png")).unwrap();
        h.state_mut().ui.panels.gradient_editor = false;
        h.run_steps(2);
        h.state_mut().gradient.editor = EditorMode::Full;
        h.state_mut().ui.panels.gradient_editor = true;
        h.run_steps(4);
        h.render().unwrap().save(format!("{dir}/editor_full.png")).unwrap();
    }

    #[test]
    fn selection_follows_sorted_stops_including_equal_positions() {
        let f = Fill::gradient(
            vec![(0.0, photocraft_color::Color::BLACK), (0.5, photocraft_color::Color::WHITE), (1.0, photocraft_color::Color::WHITE)],
            0.0,
            1.0,
            GradientStyle::Linear,
            false,
        );
        assert_eq!(selection_after_edit(&f, &json!({"action": "move", "index": 0, "location": 0.5}), None), Some(Marker::Color(0)));
        assert_eq!(selection_after_edit(&f, &json!({"action": "move", "index": 0, "location": 0.75}), None), Some(Marker::Color(1)));
        assert_eq!(selection_after_edit(&f, &json!({"action": "add", "location": 0.5}), None), Some(Marker::Color(2)));
        assert_eq!(selection_after_edit(&f, &json!({"action": "move", "kind": "opacity", "index": 0, "location": 1.0}), None), Some(Marker::Opacity(0)));
    }

    #[test]
    fn dragging_a_stop_across_another_keeps_selection_and_commits_once() {
        let mut app = app();
        app.run(cmds::STOP, json!({"action": "add", "location": 0.4})).unwrap();
        let id = app.session.active().unwrap().active_layer.unwrap();
        let mut h = Harness::builder().with_size(vec2(280.0, 800.0)).build_ui_state(panel, app);
        PhotocraftApp::setup_context(&h.ctx, theme::ThemeKind::ProMedium);
        h.run_steps(3);
        let before = fill(h.state());
        let count = h.state().session.active().unwrap().history.past_len();
        let rect = h.get_by_label("Color stops").rect();
        let from = pos2(rect.left() + 8.0, rect.top() + MARKER_Y);
        let to = pos2(rect.left() + 8.0 + (rect.width() - 16.0) * 0.7, from.y);
        h.event(egui::Event::PointerMoved(from));
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
        h.event(egui::Event::PointerMoved(to));
        h.run_steps(2);
        assert_eq!(fill(h.state()), before, "only preview changes during dragging");
        assert!(h.state().gradient.panel.is_some(), "large first move still grabbed the original stop");
        h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
        assert_eq!(h.state().session.active().unwrap().history.past_len(), count + 1);
        assert!(matches!(fill(h.state()), Fill::Gradient { stops, .. } if (stops[1].0 - 0.7).abs() < 1e-5));
        let key = strip_key(h.state(), Sink::Layer(id)).with("selection");
        assert_eq!(h.ctx.data(|d| d.get_temp::<Marker>(key)), Some(Marker::Color(1)));
        h.get_all_by_label("Delete").next().unwrap().click();
        h.run_steps(3);
        assert!(matches!(fill(h.state()), Fill::Gradient { stops, .. } if stops.len() == 2 && (stops[0].0 - 0.4).abs() < 1e-5));
        h.state_mut().run("edit.undo", json!({})).unwrap();
        h.state_mut().run("edit.undo", json!({})).unwrap();
        assert_eq!(fill(h.state()), before);
    }
}
