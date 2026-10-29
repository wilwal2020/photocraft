use super::panel::chevron_icon;
use super::scrub::take_label_scrub_name;
use super::slider::wheel_notches_of;
use super::{Color32, Pos2, Rect, Response, Stroke, Ui, Vec2, type_ahead};

/// A compact labelled dropdown in the studio style.
pub fn dropdown<T: PartialEq + Clone>(ui: &mut Ui, id: &str, name: &str, current: &mut T, options: &[(T, &str)], width: f32) -> bool {
    dropdown_hovered(ui, id, name, current, options, width).0
}

/// [`dropdown`] whose option labels are shown exactly as given: names the user typed (a channel
/// or layer name) must not be translated, unlike the built-in labels [`dropdown`] translates (#5335).
pub fn dropdown_literal<T: PartialEq + Clone>(ui: &mut Ui, id: &str, name: &str, current: &mut T, options: &[(T, &str)], width: f32) -> bool {
    let (chosen, _) = dropdown_with(ui, id, name, current, options, width, false, false, None);
    !chosen.is_empty()
}

/// [`dropdown`], also returning the option under the pointer in its open list (live previews).
pub fn dropdown_hovered<T: PartialEq + Clone>(ui: &mut Ui, id: &str, name: &str, current: &mut T, options: &[(T, &str)], width: f32) -> (bool, Option<T>) {
    let (chosen, hovered) = dropdown_with(ui, id, name, current, options, width, false, true, None);
    (!chosen.is_empty(), hovered)
}

/// Row height of a blend mode menu, and the height of the line between two of its sections:
/// compact, so the whole menu shows without scrolling (Photoshop).
pub(super) const BLEND_ROW_H: f32 = 18.0;
const BLEND_BREAK_H: f32 = 7.0;
/// The tightest a blend mode menu's rows get to fit a short window.
pub(super) const BLEND_MIN_ROW_H: f32 = 14.0;

/// The row height that fits a blend menu of `rows` options and `breaks` section lines in `room`
/// points: `max` ([`BLEND_ROW_H`]) when it fits, tighter down to [`BLEND_MIN_ROW_H`] when it
/// doesn't.
pub(super) fn blend_row_height(room: f32, rows: usize, breaks: usize, max: f32) -> f32 {
    let fit = (room - breaks as f32 * BLEND_BREAK_H - 12.0) / rows.max(1) as f32;
    if fit.is_finite() { fit.clamp(BLEND_MIN_ROW_H, max.max(BLEND_MIN_ROW_H)) } else { max }
}

/// The section of Photoshop's blend mode menus a mode's label falls in (Normal, darken, lighten,
/// contrast, inversion, component), `None` for anything else.
fn blend_section(label: &str) -> Option<u8> {
    if matches!(label, "Pass Through" | "Behind" | "Clear") {
        return Some(0);
    }
    let i = photocraft_color::BlendMode::LAYER_MODES.iter().position(|m| m.label() == label)?;
    Some(match i {
        0..2 => 0,
        2..7 => 1,
        7..12 => 2,
        12..19 => 3,
        19..23 => 4,
        _ => 5,
    })
}

/// The options a blend mode menu draws a section line above, or `None` when `labels` aren't
/// blend modes (any other dropdown keeps its look). Options that aren't Photoshop modes (an
/// imported Paint.NET mode) get a section of their own.
pub(crate) fn blend_breaks(labels: &[&str]) -> Option<Vec<usize>> {
    if labels.iter().filter(|l| blend_section(l).is_some()).count() < 12 {
        return None;
    }
    let sections: Vec<Option<u8>> = labels.iter().map(|l| blend_section(l)).collect();
    Some((1..labels.len()).filter(|&i| sections.get(i) != sections.get(i - 1)).collect())
}

/// [`dropdown_hovered`] whose value also steps with the wheel over its button or open list: down
/// picks the next option, up the previous one, one per notch (Shift too), stopping at the ends
/// (Photoshop's Layers panel blend modes, #1747). The wheel then scrolls neither the list, which
/// follows the value instead, nor what the dropdown is in. Returns every option chosen this frame
/// in order (one per notch; the last is `current`) and the option under the pointer in the open
/// list, none while the pointer rests where the wheel last stepped.
pub fn dropdown_wheel_hovered<T: PartialEq + Clone>(
    ui: &mut Ui,
    id: &str,
    name: &str,
    current: &mut T,
    options: &[(T, &str)],
    width: f32,
) -> (Vec<T>, Option<T>) {
    dropdown_with(ui, id, name, current, options, width, true, true, None)
}

/// [`dropdown_wheel_hovered`] whose open list shows every option at once in compact rows, with a
/// line above each option in `sections`, moving up as far as it must to fit the window (as a
/// blend mode menu does), instead of a short list that scrolls (Color Lookup's looks, as in
/// Photoshop). Option labels are shown as they are (file names), not translated.
pub fn dropdown_sections<T: PartialEq + Clone>(
    ui: &mut Ui,
    id: &str,
    name: &str,
    current: &mut T,
    options: &[(T, &str)],
    width: f32,
    sections: &[usize],
) -> (Vec<T>, Option<T>) {
    dropdown_with(ui, id, name, current, options, width, true, false, Some(sections))
}

/// Row height of a [`dropdown_sections`] list: tighter than a blend menu's, as such a list can be
/// long (a LUT folder).
const SECTION_ROW_H: f32 = 16.0;

#[allow(clippy::too_many_arguments)]
fn dropdown_with<T: PartialEq + Clone>(
    ui: &mut Ui,
    id: &str,
    name: &str,
    current: &mut T,
    options: &[(T, &str)],
    width: f32,
    wheel: bool,
    translate: bool,
    // Show every option at once, with section lines above these ([`dropdown_sections`]).
    sections: Option<&[usize]>,
) -> (Vec<T>, Option<T>) {
    let shown = |l: &str| if translate { tl!(l).to_string() } else { l.to_string() };
    let label = options.iter().find(|(v, _)| v == current).map(|(_, l)| shown(l)).unwrap_or_else(|| "—".into());
    let (mut chosen, mut hovered) = (Vec::new(), None);
    // The wheel's notches add up over the button and the open list alike.
    let wheel_id = ui.make_persistent_id(id).with("wheel");
    let wheel = wheel && ui.is_enabled();
    let (mut over_list, mut wheeled, mut keyed) = (false, false, false);
    // A wheel step scrolls the open list to the new value: at once over the list, the next frame
    // after a step over the button.
    let reveal_id = wheel_id.with("reveal");
    let mut reveal = wheel && ui.data_mut(|d| d.remove_temp::<bool>(reveal_id)).unwrap_or(false);
    // The list opens on the press: press, drag onto an option and release to choose it (#2735).
    let (press, close) = crate::press_menu::PressCombo::before(ui, id);
    // A blend mode menu shows every mode at once, in Photoshop's sections.
    let max_row = if sections.is_some() { SECTION_ROW_H } else { BLEND_ROW_H };
    let breaks = sections.map(<[usize]>::to_vec).or_else(|| blend_breaks(&options.iter().map(|(_, l)| *l).collect::<Vec<_>>()));
    // A blend list opens at the button and moves up as far as it must to fit the window, as in
    // Photoshop ([`blend_list`]): when even the whole window is too short, the rows tighten (to
    // [`BLEND_MIN_ROW_H`]) so every mode still shows.
    // Fixed while the list is open, so its rows never shift under the pointer.
    let row_key = ui.make_persistent_id(id).with("blend-row-h");
    let list_id = press.popup_id().with("blend-list");
    let open = craft_ui::ComboBox::is_open(ui.ctx(), ui.make_persistent_id(id));
    let room = blend_room(ui.ctx());
    let row_h = match (&breaks, open.then(|| ui.data(|d| d.get_temp::<f32>(row_key))).flatten()) {
        (Some(_), Some(kept)) => kept,
        // Never below the text's own height: every row stays the same size.
        // Whole points: a fractional row height is rounded for the highlighted row only.
        (Some(b), None) => blend_row_height(room, options.len(), b.len(), max_row).max(ui.text_style_height(&egui::TextStyle::Button) + 2.0).ceil(),
        (None, _) => BLEND_ROW_H,
    };
    if breaks.is_some() {
        ui.data_mut(|d| d.insert_temp(row_key, row_h));
    }
    let height = breaks.as_ref().map_or(420.0, |b| (options.len() as f32 * row_h + b.len() as f32 * BLEND_BREAK_H + 12.0).max(420.0));
    let texts: Vec<String> = options.iter().map(|(_, l)| shown(l)).collect();
    let blend = breaks.is_some();
    // The combo box's own list for a blend menu is empty and invisible (it can't move above its
    // button): while it is open, [`blend_list`] shows the rows, so opening, closing, Esc and
    // press-drag stay the combo box's. Open as the frame began: the click choosing a row closes the
    // combo box's list before the rows see it.
    let listed = blend && egui::Popup::is_id_open(ui.ctx(), press.popup_id());
    let mut list = |ui: &mut Ui| {
        if breaks.is_some() {
            let sp = ui.spacing_mut();
            sp.item_spacing.y = 0.0;
            sp.interact_size.y = row_h;
            sp.button_padding.y = 1.0;
        }
        fit_widest_option(ui, texts.iter().map(|l| l.as_str()));
        // A typed letter jumps to the next option starting with it, ↩ closes the list (#1481).
        let labels: Vec<&str> = texts.iter().map(|l| l.as_str()).collect();
        let shown = options.iter().position(|(v, _)| v == current);
        if let Some((v, _)) = type_ahead::keys(ui, ui.make_persistent_id(id), shown, &labels).and_then(|i| options.get(i)) {
            *current = v.clone();
            chosen.push(v.clone());
            reveal = true;
            keyed = true;
        }
        // Over the open list the wheel steps the value too, instead of scrolling the list.
        over_list = wheel && ui.rect_contains_pointer(ui.clip_rect());
        if over_list {
            let stepped = wheel_steps(ui, wheel_id, true, current, options);
            wheeled = !stepped.is_empty();
            reveal |= wheeled;
            chosen.extend(stepped);
        }
        // The row nearest the pointer, by its distance to each row.
        let pointer = ui.ctx().pointer_hover_pos().filter(|_| ui.rect_contains_pointer(ui.clip_rect()));
        let mut nearest: Option<(f32, &T)> = None;
        for (i, ((v, _), text)) in options.iter().zip(texts.iter()).enumerate() {
            if breaks.as_ref().is_some_and(|b| b.contains(&i)) {
                ui.add(egui::Separator::default().spacing(BLEND_BREAK_H));
            }
            // Blend menus: every row exactly `row_h` tall, selected or not.
            let item = if breaks.is_some() {
                let w = ui.available_width();
                ui.add_sized([w, row_h], egui::Button::selectable(v == current, text.as_str()))
            } else {
                ui.selectable_label(v == current, text.as_str())
            };
            if reveal && v == current {
                item.scroll_to_me(None);
            }
            if item.hovered() {
                hovered = Some(v.clone());
            }
            if let Some(p) = pointer {
                let d = (item.rect.top() - p.y).max(p.y - item.rect.bottom()).max(0.0);
                if nearest.is_none_or(|(n, _)| d < n) {
                    nearest = Some((d, v));
                }
            }
            if press.chosen(ui, &item) {
                *current = v.clone();
                chosen.push(v.clone());
            }
        }
        // Between two rows (item spacing) the pointer hovers neither; it is still over the list,
        // so keep previewing the nearest row instead of flashing the current value back (#2553).
        if hovered.is_none() {
            hovered = nearest.map(|(_, v)| v.clone());
        }
    };
    let combo = craft_ui::ComboBox::from_id_salt(id).selected_text(label).width(width).height(height).icon(chevron_icon).close_behavior(close);
    let response = if blend { combo.popup_style(egui::style::StyleModifier::new(invisible_popup)).show_ui(ui, |_| {}) } else { combo.show_ui(ui, &mut list) };
    // Before `press.after`, as the combo box's list is: a press-drag gesture ends after its release was seen.
    if blend && (listed || egui::Popup::is_id_open(ui.ctx(), press.popup_id())) {
        let rows = breaks.as_ref().map_or(0.0, |b| options.len() as f32 * row_h + b.len() as f32 * BLEND_BREAK_H + 12.0);
        if blend_list(&response.response, list_id, close, room, rows.min(room), &mut list) {
            // A row chosen (or Esc) closes the combo box's list, which is what keeps the rows shown.
            egui::Popup::close_id(ui.ctx(), press.popup_id());
        }
    }
    // A scrubby caption drawn just before the dropdown names it; otherwise its own name does.
    let response = match take_label_scrub_name(ui, response.response.rect) {
        Some(label) => response.response.labelled_by(label),
        None => response.response.accessible_name(name),
    };
    press.after(&response);
    if combo_box_arrow_keys(ui, &response, current, options) {
        chosen.push(current.clone());
    }
    if wheel && !over_list {
        let stepped = wheel_steps(ui, wheel_id, response.hovered(), current, options);
        wheeled = !stepped.is_empty();
        if wheeled && craft_ui::ComboBox::is_open(ui.ctx(), response.id) {
            ui.data_mut(|d| d.insert_temp(reveal_id, true));
        }
        chosen.extend(stepped);
    }
    // Nor is the option under a pointer resting where the keyboard picked another.
    if wheel_rests(ui, wheel_id, wheeled || keyed) {
        hovered = None;
    }
    (chosen, hovered)
}

/// The height a blend list may take: the window's visible part, less the popup's frame and a gap.
fn blend_room(ctx: &egui::Context) -> f32 {
    (crate::work_area::visible_rect(ctx).height() - 2.0 * MENU_EDGE - 16.0).max(MENU_MIN_HEIGHT)
}

/// The style of egui's own list for a blend menu: nothing to see, nothing to hit.
fn invisible_popup(style: &mut egui::Style) {
    let v = &mut style.visuals;
    v.window_fill = Color32::TRANSPARENT;
    v.panel_fill = Color32::TRANSPARENT;
    v.window_stroke = Stroke::NONE;
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;
    style.spacing.menu_margin = egui::Margin::ZERO;
    style.spacing.window_margin = egui::Margin::ZERO;
}

/// A blend menu's open list (`id`): its top at `button`'s top, moved up just enough for its
/// `height` (the rows', [`dropdown_with`]) to fit the window, `room` tall at most, so all of
/// Photoshop's modes show at once. True when it asks to close (a row chosen, Esc).
fn blend_list(button: &Response, id: egui::Id, close: egui::PopupCloseBehavior, room: f32, height: f32, list: &mut dyn FnMut(&mut Ui)) -> bool {
    let visible = crate::work_area::visible_rect(&button.ctx);
    // The frame around the rows (menu margins and stroke).
    let frame = button.ctx.global_style().spacing.menu_margin.sum().y + 2.0;
    let bottom = visible.bottom() - MENU_EDGE - frame;
    let top = button.rect.top().min(bottom - height).max(visible.top() + MENU_EDGE);
    let top_left = egui::RectAlign { parent: egui::Align2::LEFT_TOP, child: egui::Align2::LEFT_TOP };
    egui::Popup::new(id, button.ctx.clone(), egui::PopupAnchor::Position(egui::pos2(button.rect.left(), top)), button.layer_id)
        .open(true)
        .kind(egui::PopupKind::Menu)
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .style(egui::containers::menu::menu_style)
        .align(top_left)
        .align_alternatives(&[])
        .gap(0.0)
        .width(button.rect.width())
        .close_behavior(close)
        .show(|ui| {
            ui.set_min_width(ui.available_width());
            // A popup's Ui starts 400 pt tall at most and keeps its size: ask for the rows' height.
            egui::ScrollArea::vertical().max_height(room).min_scrolled_height(height).show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                list(ui);
            });
        })
        .is_some_and(|r| r.response.should_close())
}

/// Makes an open dropdown list as wide as its widest option is when hovered, from the start. egui
/// takes a plain row's frame stroke off its padding but draws a hovered row with the stroke, so a
/// row grows by two stroke widths under the pointer, and the list never shrinks back: scrolling
/// the widest option under the pointer widened the list (#2607).
fn fit_widest_option<'a>(ui: &mut Ui, labels: impl Iterator<Item = &'a str>) {
    let extend = Some(egui::TextWrapMode::Extend);
    let widest = labels.map(|l| egui::WidgetText::from(l).into_galley(ui, extend, f32::INFINITY, egui::FontSelection::Default).size().x).fold(0.0, f32::max);
    ui.set_min_width(widest + 2.0 * ui.spacing().button_padding.x);
}

/// Whether the pointer still rests where the wheel last stepped dropdown `id` (`wheeled`: it did
/// this frame). The option under it then isn't the one to preview: the value moved on without it.
fn wheel_rests(ui: &Ui, id: egui::Id, wheeled: bool) -> bool {
    let key = id.with("at");
    let pointer = ui.input(|i| i.pointer.hover_pos());
    if wheeled && let Some(p) = pointer {
        ui.data_mut(|d| d.insert_temp(key, p));
    }
    let rests = pointer.is_some() && ui.data(|d| d.get_temp::<Pos2>(key)) == pointer;
    if !rests {
        ui.data_mut(|d| d.remove::<Pos2>(key));
    }
    rests
}

/// The options the wheel over dropdown `id` (while `hovered`) steps `current` through this frame,
/// one per notch, in order; the last is the new `current`. While the pointer is over it the wheel
/// scrolls nothing else.
fn wheel_steps<T: PartialEq + Clone>(ui: &mut Ui, id: egui::Id, hovered: bool, current: &mut T, options: &[(T, &str)]) -> Vec<T> {
    if hovered {
        ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
    }
    let notches = wheel_notches_of(ui, id, hovered, 1.0);
    let Some(mut index) = options.iter().position(|(v, _)| v == current) else { return Vec::new() };
    let mut chosen = Vec::new();
    // Down (negative) is the next option. No more steps than there are options.
    for _ in 0..(notches.abs().min(options.len() as f32) as usize) {
        let next = if notches < 0.0 { index.checked_add(1) } else { index.checked_sub(1) };
        let Some((next, (v, _))) = next.and_then(|i| options.get(i).map(|o| (i, o))) else { break };
        index = next;
        chosen.push(v.clone());
    }
    if let Some(v) = chosen.last() {
        *current = v.clone();
    }
    chosen
}

/// Give a dropdown keyboard focus when it opens, then use the arrow keys to move through its
/// choices. `craft_ui::ComboBox` opens a popup but leaves focus on the canvas by default, which makes
/// controls such as the Layers panel's Blend Mode dropdown unreachable from the keyboard.
fn combo_box_arrow_keys<T: PartialEq + Clone>(ui: &mut Ui, response: &Response, current: &mut T, options: &[(T, &str)]) -> bool {
    if response.clicked() {
        response.request_focus();
    }
    // The popup, rather than its button, becomes the focused egui layer after it opens. While it
    // is open it owns its navigation keys, even though `response.has_focus()` is then false.
    if !craft_ui::ComboBox::is_open(ui.ctx(), response.id) || options.is_empty() {
        return false;
    }
    let step = if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
        1
    } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
        -1
    } else {
        return false;
    };
    let index = options.iter().position(|(value, _)| value == current).unwrap_or(0);
    let next = if step > 0 { (index + 1).min(options.len() - 1) } else { index.saturating_sub(1) };
    match options.get(next) {
        Some((value, _)) if next != index => {
            *current = value.clone();
            true
        }
        _ => false,
    }
}

/// The body of a right-click menu: as tall as its items up to the part of the window that can be
/// seen (clear of a taskbar a too-tall window runs under, #315), scrolling past that, so long
/// context menus (a layer's, the canvas tools') stay reachable on small windows instead of running
/// off the bottom. egui moves a popup up to keep it in the window, so a menu opened low on the
/// screen first shifts up and only scrolls when it is taller than the window.
pub fn menu_scroll<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    // The popup frame's margin and stroke, and a small gap to the window's edges.
    let frame = ui.spacing().menu_margin.sum().y + 2.0 + 2.0 * MENU_EDGE;
    let room = (crate::work_area::visible_rect(ui.ctx()).height() - frame).max(MENU_MIN_HEIGHT);
    // A popup's Ui is only as tall as the popup was last frame (400 pt on the first), so ask for
    // the whole room: the area still shrinks to its rows when they need less.
    egui::ScrollArea::vertical().max_height(room).min_scrolled_height(room).show(ui, add_contents).inner
}

/// Gap kept between a context menu and the window's edges, and the shortest it gets.
const MENU_EDGE: f32 = 4.0;

const MENU_MIN_HEIGHT: f32 = 120.0;

/// The colour picker popup of a colour swatch: a click on `swatch` toggles it, a click outside
/// closes it. While open, its left edge stays where it first showed (at the swatch, or further
/// left when the window edge needs it), below the swatch or above it as room allows. The picker's
/// width follows its value readouts, and placing it anew every frame moved it under the pointer,
/// flipping it from side to side near the right edge of the window (#534).
pub fn swatch_popup(swatch: &Response) -> egui::Popup<'static> {
    // Room for the readouts to widen after the picker opened.
    const SLACK: f32 = 32.0;
    let popup = egui::Popup::from_toggle_button_response(swatch).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    let ctx = &swatch.ctx;
    let key = popup.get_id().with("left");
    if !popup.is_open() {
        ctx.data_mut(|d| d.remove::<f32>(key));
        return popup;
    }
    crate::press_menu::above_parent(ctx, swatch.layer_id, egui::LayerId::new(egui::Order::Foreground, popup.get_id()));
    // Its width is known from the frame after it opened (egui sizes it unseen first).
    let left = ctx.data(|d| d.get_temp::<f32>(key)).or_else(|| {
        let width = popup.get_expected_size()?.x;
        let screen = ctx.content_rect();
        let left = swatch.rect.left().min(screen.right() - width - SLACK).max(screen.left());
        ctx.data_mut(|d| d.insert_temp(key, left));
        Some(left)
    });
    let Some(left) = left else { return popup };
    popup
        .anchor(Rect::from_x_y_ranges(left..=left, swatch.rect.y_range()))
        .align(egui::RectAlign::BOTTOM_START)
        .align_alternatives(&[egui::RectAlign::TOP_START])
}

pub fn dropdown_with_tooltips<T: PartialEq + Clone>(ui: &mut Ui, id: &str, name: &str, current: &mut T, options: &[(T, &str, &str)], width: f32) -> bool {
    let label = options.iter().find(|(v, _, _)| v == current).map(|(_, l, _)| tl!(l)).unwrap_or("—");
    let mut changed = false;
    let (press, close) = crate::press_menu::PressCombo::before(ui, id);
    let response =
        craft_ui::ComboBox::from_id_salt(id).selected_text(label).width(width).height(420.0).icon(chevron_icon).close_behavior(close).show_ui(ui, |ui| {
            fit_widest_option(ui, options.iter().map(|(_, l, _)| tl!(l)));
            let labels: Vec<&str> = options.iter().map(|(_, l, _)| tl!(l)).collect();
            let shown = options.iter().position(|(v, _, _)| v == current);
            if let Some((v, _, _)) = type_ahead::keys(ui, ui.make_persistent_id(id), shown, &labels).and_then(|i| options.get(i)) {
                *current = v.clone();
                changed = true;
            }
            for (v, l, tip) in options {
                let item = ui.selectable_label(v == current, tl!(l)).on_hover_text(tl!(tip));
                if press.chosen(ui, &item) {
                    *current = v.clone();
                    changed = true;
                }
            }
        });
    let response = match take_label_scrub_name(ui, response.response.rect) {
        Some(label) => response.response.labelled_by(label),
        None => response.response.accessible_name(name),
    };
    press.after(&response);
    if let Some((_, _, tip)) = options.iter().find(|(v, _, _)| v == current) {
        response.on_hover_text(tl!(tip));
    }
    changed
}
