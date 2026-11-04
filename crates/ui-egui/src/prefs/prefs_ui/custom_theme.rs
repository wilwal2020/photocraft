//! Preferences › Interface › Custom Theme: a built-in theme to start from and its colours, edited
//! in the dialog's draft. The draft's appearance is the one on screen while Preferences is open
//! (`appearance::draft_interface`), so every edit shows in the whole app at once; a preview in the
//! group shows the colours too when the other appearance mode is the active one.

use super::fields::preference_matches;
use super::{Color32, Map, RichText, Sense, ThemeKind, Tokens, Value, json, prefs, vec2};
use crate::theme::{CUSTOM_COLOR_GROUPS, color_hex, parse_color_hex};

/// The tokens of a saved or drafted custom theme.
pub(super) fn custom_tokens(custom: &prefs::CustomTheme) -> Tokens {
    Tokens::custom(ThemeKind::from_name(custom.base.name()).unwrap_or_default(), &custom.colors)
}

/// Whether a search (`query`, lowercased; empty matches everything) shows the Custom Theme group.
pub(super) fn custom_theme_matches(query: &str) -> bool {
    query.is_empty()
        || preference_matches("interface", "customTheme", query)
        || CUSTOM_COLOR_GROUPS
            .iter()
            .flat_map(|(_, colors)| colors.iter())
            .any(|(_, label)| label.to_lowercase().contains(query) || tl!(label).to_lowercase().contains(query))
}

/// The Custom Theme group's body: Start From and Reset to Preset, a preview, and a swatch with a
/// hex field for every colour. Any edit also makes Custom the theme of the appearance mode on
/// screen, so it shows straight away.
pub(super) fn custom_theme_editor(ui: &mut egui::Ui, obj: &mut Map<String, Value>, system: Option<egui::Theme>) {
    let t = Tokens::get(ui.ctx());
    let mut custom: prefs::CustomTheme = obj.get("customTheme").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    let before = custom.clone();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Start From")).color(t.text_dim));
        let options: Vec<(String, &str)> = ThemeKind::ALL.iter().map(|k| (k.id().to_string(), tl!(k.label()))).collect();
        let mut base = custom.base.name().to_string();
        if crate::widgets::dropdown(ui, "custom-theme-base", tl!("Start From"), &mut base, &options, 200.0)
            && let Some(theme) = prefs::Theme::parse(&base)
        {
            // A new starting point brings its own colours.
            custom.base = theme;
            custom.colors.clear();
        }
        let reset = ui
            .add_enabled(!custom.colors.is_empty(), egui::Button::new(tl!("Reset to Preset")))
            .on_hover_text(tl!("Go back to the colours of the theme it starts from"));
        if reset.clicked() {
            custom.colors.clear();
        }
    });
    ui.add_space(6.0);
    let tokens = custom_tokens(&custom);
    preview(ui, &tokens);
    ui.add_space(6.0);
    for (group, colors) in CUSTOM_COLOR_GROUPS {
        ui.label(RichText::new(tl!(group)).color(t.text).strong());
        egui::Grid::new(("custom-theme-colors", *group)).num_columns(4).spacing([14.0, 4.0]).min_col_width(130.0).show(ui, |ui| {
            for (i, (name, label)) in colors.iter().enumerate() {
                color_row(ui, &mut custom, &tokens, name, label);
                if i % 2 == 1 || i + 1 == colors.len() {
                    ui.end_row();
                }
            }
        });
        ui.add_space(4.0);
    }
    if custom != before {
        obj.insert("customTheme".into(), json!(custom));
        let mode = obj.get("appearanceMode").and_then(Value::as_str).unwrap_or("auto");
        let light = mode == "light" || (mode == "auto" && system == Some(egui::Theme::Light));
        obj.insert(if light { "lightTheme" } else { "darkTheme" }.into(), json!("custom"));
    }
}

/// The side of a colour's reset button.
const RESET_SIZE: f32 = 20.0;

/// One colour's label and controls (two grid cells): its swatch, which opens the Color Picker and
/// previews while it is open, a hex field (`#rrggbb` or `#rrggbbaa`), and a button that puts back
/// the starting theme's colour once it has been changed.
fn color_row(ui: &mut egui::Ui, custom: &mut prefs::CustomTheme, tokens: &Tokens, name: &str, label: &str) {
    let Some(mut color) = tokens.color(name) else { return };
    let label = tl!(label);
    ui.label(label);
    ui.horizontal(|ui| {
        let mut changed = crate::widgets::color_edit_button_srgba(ui, &mut color, &crate::color_picker_ui::title_for(label)).changed();
        // A colour the theme leaves transparent (Studio's tab strip) keeps its alpha in the
        // picker: a picked colour becomes opaque so it shows.
        if changed && color.a() == 0 {
            let [r, g, b, _] = color.to_srgba_unmultiplied();
            color = Color32::from_rgb(r, g, b);
        }
        changed |= hex_input(ui, name, label, &mut color);
        if changed {
            custom.colors.insert(name.to_string(), color_hex(color));
        }
        // The button's room is kept when it is hidden, so the columns line up in every group.
        if custom.colors.contains_key(name) {
            let tip = crate::i18n::fmt(tl!("Reset {color}"), &[("color", label)]);
            if crate::icons::button(ui, "undo-2", RESET_SIZE, false, &tip).clicked() {
                custom.colors.remove(name);
            }
        } else {
            ui.add_space(RESET_SIZE);
        }
    });
}

/// A hex field for a theme colour; true when a typed value changed it.
fn hex_input(ui: &mut egui::Ui, name: &str, label: &str, color: &mut Color32) -> bool {
    let id = egui::Id::unique(("custom-theme-hex", name));
    let shown = color_hex(*color);
    let mut text = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| shown.clone());
    let response = crate::widgets::text_field_ex(ui, id, &mut text, "", 80.0, true, Some(9)).accessible_name(format!("{label} hex"));
    let mut changed = false;
    if response.changed()
        && let Some(c) = parse_color_hex(&text)
        && c != *color
    {
        *color = c;
        changed = true;
    }
    if response.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, text));
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
    }
    changed
}

/// A small mock-up of the window in the custom theme's colours: the title bar, the canvas with its
/// dots, and a panel with a tab strip, a selected row, a field with a slider and a primary button.
fn preview(ui: &mut egui::Ui, p: &Tokens) {
    use egui::{Align2, FontId, Rect, Stroke, StrokeKind, pos2};
    let width = ui.available_width().min(520.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 104.0), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, tl!("Custom theme preview")));
    let painter = ui.painter_at(rect);
    let font = FontId::proportional(11.0);
    let r = p.radius_sm;
    painter.rect_filled(rect, p.radius, p.dock);
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 20.0));
    painter.rect_filled(bar, egui::CornerRadius { nw: p.radius as u8, ne: p.radius as u8, sw: 0, se: 0 }, p.chrome);
    painter.text(bar.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, "PhotoCraft", font.clone(), p.text);
    painter.circle_filled(bar.right_center() - vec2(12.0, 0.0), 4.0, p.icon);
    painter.line_segment([pos2(rect.left(), bar.bottom()), pos2(rect.right(), bar.bottom())], Stroke::new(1.0, p.separator));

    let canvas = Rect::from_min_max(pos2(rect.left() + 6.0, bar.bottom() + 6.0), pos2(rect.center().x - 4.0, rect.bottom() - 6.0));
    painter.rect_filled(canvas, r, p.canvas);
    let mut y = canvas.top() + 8.0;
    while y < canvas.bottom() - 4.0 {
        let mut x = canvas.left() + 8.0;
        while x < canvas.right() - 4.0 {
            painter.circle_filled(pos2(x, y), 1.0, p.canvas_dot);
            x += 12.0;
        }
        y += 12.0;
    }
    let doc = Rect::from_center_size(canvas.center(), vec2((canvas.width() * 0.5).min(90.0), canvas.height() * 0.6));
    painter.rect_filled(doc, 0.0, Color32::WHITE);

    let card = Rect::from_min_max(pos2(rect.center().x + 4.0, bar.bottom() + 6.0), rect.max - vec2(6.0, 6.0));
    painter.rect_filled(card, r, p.card);
    let tabs = Rect::from_min_size(card.min, vec2(card.width(), 17.0));
    painter.rect_filled(tabs, egui::CornerRadius { nw: r as u8, ne: r as u8, sw: 0, se: 0 }, p.tab_strip);
    let layers = painter.text(tabs.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, tl!("Layers"), crate::theme::semibold(11.0), p.text);
    painter.text(pos2(layers.right() + 10.0, tabs.center().y), Align2::LEFT_CENTER, tl!("Channels"), font.clone(), p.text_faint);
    painter.rect_stroke(card, r, Stroke::new(1.0, p.card_border), StrokeKind::Inside);

    let row = Rect::from_min_size(pos2(card.left() + 4.0, tabs.bottom() + 4.0), vec2(card.width() - 8.0, 18.0));
    let selected = if p.row_selected.a() > 0 { p.row_selected } else { p.accent_soft };
    painter.rect_filled(row, r, selected);
    painter.text(row.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, tl!("Layer 1"), font.clone(), p.text);
    painter.text(row.right_center() - vec2(6.0, 0.0), Align2::RIGHT_CENTER, tl!("Selected"), FontId::proportional(10.0), p.accent_text);

    let line = row.bottom() + 15.0;
    let opacity = painter.text(pos2(card.left() + 8.0, line), Align2::LEFT_CENTER, tl!("Opacity"), font.clone(), p.text_dim);
    let field = Rect::from_min_size(pos2(opacity.right() + 6.0, line - 9.0), vec2(44.0, 18.0));
    painter.rect_filled(field, r, p.field);
    painter.rect_stroke(field, r, Stroke::new(1.0, p.field_border), StrokeKind::Inside);
    painter.text(field.center(), Align2::CENTER_CENTER, "100%", crate::theme::mono(10.5), p.text);
    let button = Rect::from_min_size(pos2(card.right() - 48.0, line - 9.0), vec2(40.0, 18.0));
    painter.rect_filled(button, r, p.primary_bg);
    painter.text(button.center(), Align2::CENTER_CENTER, tl!("OK"), font, p.primary_text);
    let track_left = field.right() + 8.0;
    let track_right = button.left() - 8.0;
    if track_right > track_left + 12.0 {
        let mid = track_left + (track_right - track_left) * 0.6;
        painter.line_segment([pos2(track_left, line), pos2(track_right, line)], Stroke::new(3.0, p.field_border));
        painter.line_segment([pos2(track_left, line), pos2(mid, line)], Stroke::new(3.0, p.accent));
        painter.circle(pos2(mid, line), 5.0, Color32::WHITE, Stroke::new(1.0, p.accent_border));
    }
}
