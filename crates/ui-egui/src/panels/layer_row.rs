use super::layer_drag::mask_drop;
use super::layer_drag::{MaskDrag, fx_drop, layer_drag_and_drop, mask_drag_key, start_fx_drag, starts_mask_drag};
use super::layers::simple_lock_toggle;
use super::{
    Color32, CornerRadius, Layer, LayerContent, PhotocraftApp, Pos2, Rect, Sense, Stroke, StrokeKind, Tokens, Value, icons, json, pos2, theme, vec2, widgets,
};
use egui::Vec2;

/// How a Layers panel row is selected: in the (multi-)selection, the primary/active layer, and
/// whether several layers are selected.
#[derive(Clone, Copy)]
pub(super) struct RowSel {
    pub(super) selected: bool,
    pub(super) primary: bool,
    pub(super) multi: bool,
    /// A group holding the layer is hidden: its own eye shows greyed out (Photoshop).
    pub(super) parent_hidden: bool,
    /// The colour of the nearest group holding the layer that has one: a layer or group with no
    /// colour of its own shows it, as in Photoshop (user report: a coloured Photoshop group's
    /// groups came in uncoloured).
    pub(super) inherited_label: photocraft_doc::LabelColor,
}

/// The colour label a row shows: its own, else its nearest coloured group's.
pub(super) fn shown_label(own: photocraft_doc::LabelColor, inherited: photocraft_doc::LabelColor) -> photocraft_doc::LabelColor {
    if own == photocraft_doc::LabelColor::None { inherited } else { own }
}

/// For each Layers panel row (`(depth, layer)`, a group listed before its contents), the colour
/// of the nearest group above it that shows one ([`RowSel::inherited_label`]).
pub(crate) fn inherited_labels(rows: &[(usize, &Layer)]) -> Vec<photocraft_doc::LabelColor> {
    let mut groups: Vec<photocraft_doc::LabelColor> = Vec::new();
    rows.iter()
        .map(|&(depth, l)| {
            groups.truncate(depth);
            let inherited = groups.last().copied().unwrap_or_default();
            groups.resize(depth, photocraft_doc::LabelColor::None);
            groups.push(shown_label(l.label, inherited));
            inherited
        })
        .collect()
}

/// The badge beside the pointer over a thumbnail ⌘-click loads with these modifiers: none for a
/// new selection, + for ⇧ (add), − for ⌥ (subtract), × for ⇧⌥ (intersect).
pub(crate) fn thumb_load_badge(m: egui::Modifiers) -> Option<crate::tool_feedback::Badge> {
    crate::tool_feedback::Badge::from_mode(crate::channels_panel::load_operation(m))
}

/// The cursor over a thumbnail ⌘-click loads as a selection, as Photoshop's (#7824; user report):
/// a pointing hand with a marquee square, and the selection mode's sign beside it (+ add, − subtract,
/// × intersect). One OS cursor bitmap, so it moves with the pointer.
fn thumb_load_cursor(ctx: &egui::Context, p: Pos2, m: egui::Modifiers) {
    ctx.set_cursor_icon(crate::tool_cursor::icon_parts(ctx, &thumb_load_parts(m), p));
}

/// The parts of [`thumb_load_cursor`] for modifiers `m`.
pub(crate) fn thumb_load_parts(m: egui::Modifiers) -> Vec<crate::tool_cursor::icon::Part> {
    use crate::tool_cursor::icon::Part;
    use crate::tool_feedback::Badge;
    // The tip of the pointing finger is at (8, 2) of the icon's 24-unit box.
    let mut parts =
        vec![Part::at_hotspot("pointer", vec2(8.0, 2.0) / 24.0, 20.0), Part { at: vec2(9.0, 12.0), ..Part::at_hotspot("square-dashed", Vec2::ZERO, 11.0) }];
    let sign = match thumb_load_badge(m) {
        Some(Badge::Add) => Some("plus"),
        Some(Badge::Subtract) => Some("minus"),
        Some(Badge::Intersect) => Some("x"),
        _ => None,
    };
    if let Some(name) = sign {
        parts.push(Part { at: vec2(19.0, 12.0), ..Part::at_hotspot(name, Vec2::ZERO, 9.0) });
    }
    parts
}

/// `layer.select` mode for a click with these modifiers (Photoshop: ⌘-click toggles, ⇧-click
/// selects a range, a plain click selects one layer).
fn select_mode(m: egui::Modifiers) -> &'static str {
    if m.command {
        "toggle"
    } else if m.shift {
        "range"
    } else {
        "replace"
    }
}

/// A drag down the Layers panel's eye column (Photoshop): the first eye toggles and every row
/// swept over gets the same visibility, once per row, all in one history step.
#[derive(Clone)]
struct EyeSweep {
    visible: bool,
    /// The `coalesce` key shared by the drag's `layer.setProps` calls.
    key: u64,
    swept: Vec<u64>,
}

/// Starts an eye-column sweep from `eye`, or applies a running one to the row `row` of `l`.
fn eye_sweep(ctx: &egui::Context, l: &Layer, row: Rect, eye: &egui::Response, actions: &mut Vec<(String, Value)>) {
    let id = egui::Id::unique("layer-eye-sweep");
    let set =
        |visible: bool, key: u64| ("layer.setProps".to_string(), json!({"layer": l.id.0, "visible": visible, "coalesce": format!("layer-eye-sweep:{key}")}));
    if eye.drag_started() {
        start_eye_sweep(ctx, l, id, actions, &set);
    } else {
        continue_eye_sweep(ctx, l, row, id, actions, &set);
    }
}

fn start_eye_sweep(ctx: &egui::Context, layer: &Layer, id: egui::Id, actions: &mut Vec<(String, Value)>, set: &impl Fn(bool, u64) -> (String, Value)) {
    let sweep = EyeSweep { visible: !layer.visible, key: ctx.cumulative_pass_nr(), swept: vec![layer.id.0] };
    actions.push(set(sweep.visible, sweep.key));
    ctx.data_mut(|data| data.insert_temp(id, sweep));
}

fn continue_eye_sweep(
    ctx: &egui::Context,
    layer: &Layer,
    row: Rect,
    id: egui::Id,
    actions: &mut Vec<(String, Value)>,
    set: &impl Fn(bool, u64) -> (String, Value),
) {
    let Some(mut sweep) = ctx.data(|data| data.get_temp::<EyeSweep>(id)) else { return };
    if !sweep_pointer_down(ctx) {
        ctx.data_mut(|data| data.remove::<EyeSweep>(id));
        return;
    }
    let Some((y0, y1)) = swept_pointer_range(ctx) else { return };
    if y1 < row.top() || y0 >= row.bottom() || sweep.swept.contains(&layer.id.0) {
        return;
    }
    if layer.visible != sweep.visible {
        actions.push(set(sweep.visible, sweep.key));
    }
    sweep.swept.push(layer.id.0);
    ctx.data_mut(|data| data.insert_temp(id, sweep));
}

fn sweep_pointer_down(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.pointer.primary_down())
}

/// The pointer segment between frames catches rows even when a fast drag skips them visually.
fn swept_pointer_range(ctx: &egui::Context) -> Option<(f32, f32)> {
    let (position, delta) = ctx.input(|input| (input.pointer.interact_pos(), input.pointer.delta()));
    let p = position?;
    Some(((p.y - delta.y).min(p.y), (p.y - delta.y).max(p.y)))
}

/// Marker in the clipped row's existing 12-point slot, beside its thumbnail.
pub(super) fn paint_clipping_indicator(painter: &egui::Painter, at: Pos2, color: Color32) {
    let tip = at + vec2(5.0, 3.0);
    let stroke = Stroke::new(1.0, color);
    painter.add(egui::Shape::line(vec![at + vec2(9.0, -4.0), at + vec2(5.0, -4.0), tip], stroke));
    painter.add(egui::Shape::line(vec![at + vec2(2.0, 0.0), tip, at + vec2(8.0, 0.0)], stroke));
}

/// Photoshop's Layers panel with medium thumbnails, measured at 100%: 41 pt layer rows, 25 pt
/// group rows (a group has no thumbnail, only a folder glyph), a 30 pt eye column, thumbnails
/// fitted into 32 pt and names 9 pt after the thumbnail.
pub(super) const PRO_ROW_H: f32 = 41.0;
pub(super) const PRO_GROUP_ROW_H: f32 = 25.0;
const EYE_COL_W: f32 = 30.0;
const PRO_THUMB: f32 = 32.0;
const PRO_THUMB_GAP: f32 = 9.0;
/// The Layers panel's name size in Photoshop's themes (Photoshop's small UI font).
const PRO_NAME_SIZE: f32 = 10.0;

/// The thumbnail size and row height of `l`'s Layers panel row at the Layers Panel Options
/// thumbnail size `size`. Medium, the default, is Photoshop's as measured; the other sizes are
/// [`crate::layers_panel_ui::thumbnail_geometry`]'s. In the Pro themes a group row (a folder glyph,
/// no thumbnail) is much shorter; a group with a mask gets a full layer row, so its mask
/// thumbnail shows at the same size as a layer's (Photoshop).
fn layer_row_metrics(l: &Layer, pro: bool, size: crate::state::LayerThumbnailSize) -> (f32, f32) {
    let (thumb, row) = match size {
        crate::state::LayerThumbnailSize::Medium => (if pro { PRO_THUMB } else { 34.0 }, if pro { PRO_ROW_H } else { 46.0 }),
        size => crate::layers_panel_ui::thumbnail_geometry(size, pro),
    };
    let masked = l.mask.is_some() || l.vector_mask.is_some();
    if pro && l.is_group() && !masked { (thumb, row.min(PRO_GROUP_ROW_H)) } else { (thumb, row) }
}

/// Height of `l`'s Layers panel row with medium thumbnails.
#[cfg(test)]
pub(super) fn layer_row_height(l: &Layer, pro: bool) -> f32 {
    layer_row_metrics(l, pro, crate::state::LayerThumbnailSize::Medium).1
}

/// The Layers panel's row dividers in the Pro themes: halfway between the theme's separator and
/// the panel, lighter than a full separator line, as Photoshop draws them.
pub(crate) fn layer_divider(t: &Tokens) -> Color32 {
    let mix = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
    let (s, c) = (t.separator, t.card);
    Color32::from_rgb(mix(s.r(), c.r()), mix(s.g(), c.g()), mix(s.b(), c.b()))
}

/// `c` darkened towards black by `k` (0 = unchanged, 1 = black), keeping its alpha.
fn darken(c: Color32, k: f32) -> Color32 {
    let f = |v: u8| (f32::from(v) * (1.0 - k.clamp(0.0, 1.0))).round() as u8;
    Color32::from_rgba_premultiplied(f(c.r()), f(c.g()), f(c.b()), c.a())
}

/// One row of the Layers panel for `l`, `depth` levels inside groups or artboards: it paints the
/// row, queues what a click or drag on it asks for in `actions`, and returns the row's rect.
#[allow(clippy::too_many_arguments)]
pub(super) fn layer_row(
    app: &mut PhotocraftApp,
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    doc: &photocraft_doc::Document,
    l: &Layer,
    depth: usize,
    row: RowSel,
    actions: &mut Vec<(String, Value)>,
) -> Rect {
    let selected = row.selected;
    let t = Tokens::get(ctx);
    // The Layers Panel Options thumbnail size sets the row and the layer and mask thumbnails;
    // group rows are much shorter in the Pro themes.
    let (base, row_h) = layer_row_metrics(l, t.pro, app.ui.layers_panel_options.thumbnail_size);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
    // With thumbnails off no mask thumbnail is drawn, so none may keep catching drags from older frames.
    if base == 0.0 {
        crate::mask_thumbs_ui::forget(ctx, l.id.0);
    }
    // A drag from a mask's thumbnail drags that mask, not the layer (#3113, #3119).
    let mask_drag = starts_mask_drag(ctx, l, &resp);
    if let Some(kind) = mask_drag {
        ctx.data_mut(|d| d.insert_temp::<MaskDrag>(mask_drag_key(), (l.id.0, kind)));
    }
    let mask_drag = mask_drag.is_some();
    layer_drag_and_drop(app, ctx, ui, l, rect, &resp, actions);
    fx_drop(ctx, ui, l, rect, actions);
    mask_drop(ctx, ui, l, rect, actions);
    crate::smart_ui::drop_on_row(ctx, ui, l, rect, actions);
    if resp.drag_started() && !mask_drag {
        crate::layer_transfer::begin_from_panel(app, ctx, l.id);
    }
    // Rows are painted: name them for screen readers and UI tests.
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &l.name));
    // A row scrolled out of view only keeps its place (#125): a layout's hundreds of rows would
    // otherwise lay out names, icons and thumbnails every frame. Group rows stay whole: their
    // disclosure triangle is a widget (accessibility, scroll-to).
    if !ui.is_rect_visible(rect) && !l.is_group() && !resp.context_menu_opened() && crate::layer_row_ui::renaming(ctx) != Some(l.id.0) {
        return rect;
    }
    let painter = ui.painter_at(rect.expand(1.0));
    let is_hovered = resp.hovered() || app.canvas_hover_layer == Some(l.id);
    if resp.hovered() {
        app.hover_layer = Some(l.id);
    }
    paint_row_background(&painter, &t, shown_label(l.label, row.inherited_label), rect, selected, is_hovered);
    let mut x = rect.left() + 6.0;
    let eye_resp = eye_column(app, ui, l, row, rect, actions);
    // Everything but the thumbnails, the indentation and the name, so a narrow panel squeezes
    // the indentation first, then the thumbnails. A second mask draws over the name rather than
    // taking room, so a layer with two masks keeps its indentation and thumbnail size.
    let others = 6.0
        + 28.0
        + if l.is_group() { crate::layer_tree_ui::TRIANGLE_W } else { 0.0 }
        + if l.clipped { 12.0 } else { 0.0 }
        + crate::layer_row_ui::reserved_width(l);
    let ts = if base == 0.0 { 0.0 } else { crate::mask_thumbs_ui::thumb_size(l, base, rect.width() - others) };
    let fixed = others + if ts == 0.0 { 0.0 } else { ts + 6.0 + crate::mask_thumbs_ui::room_width(l, ts).0 };
    // A layer inside a group (or an artboard) indents one step more than its depth, past the
    // triangle's column, so it sits right of its parent: with only its depth its thumbnail lined
    // up under the group's, hiding the hierarchy. Top-level rows don't move.
    let steps = if !l.is_group() && depth > 0 { depth + 1 } else { depth };
    x += 28.0 + crate::layer_row_ui::indent(steps, rect.width(), fixed);
    let toggled = crate::layer_tree_ui::disclosure(ui, rect, &mut x, l, actions);
    if l.clipped {
        paint_clipping_indicator(&painter, pos2(x, rect.center().y), t.text_faint);
        x += 12.0;
    }
    let thumb = if t.pro && l.is_group() {
        // Photoshop's group rows: a small folder glyph instead of a thumbnail.
        let folder = Rect::from_min_size(pos2(x.round(), (rect.center().y - 5.5).round()), vec2(15.0, 11.0));
        crate::layer_row_ui::paint_folder(&painter, folder, t.text_dim);
        x += folder.width() + 12.0;
        folder
    } else if ts > 0.0 {
        let cell = Rect::from_min_size(pos2(x, rect.center().y - ts / 2.0), vec2(ts, ts));
        let thumb = layer_thumb_rect(app, ctx, doc, l, cell, t.pro);
        draw_layer_thumb(app, ctx, ui, doc, l, thumb, row.primary);
        // The name follows the thumbnail as drawn, so a narrow one leaves it further left.
        x = thumb.right() + if t.pro { PRO_THUMB_GAP } else { 6.0 };
        thumb
    } else {
        Rect::from_min_size(pos2(x, rect.center().y), vec2(0.0, 0.0))
    };
    // Link chains and pixel / vector mask thumbnails (#153). With thumbnails off the labels and
    // disclosure controls remain interactive.
    let masks = if ts > 0.0 { crate::mask_thumbs_ui::paint(app, ctx, ui, &painter, doc, l, &mut x, rect.center().y, ts, actions) } else { Default::default() };
    let rc = RowCtx { l, doc, row, rect, ts, thumb, resp: &resp, masks: &masks };
    paint_target_brackets(app, &painter, &t, &rc);
    // Right-hand indicators first; the name gets what is left and ends in "…" (#144).
    let fx_open = app.session.active().is_none_or(|d| !d.fx_collapsed.contains(&l.id));
    let (name_right, indicators, fx_toggled) = crate::layer_row_ui::indicators(ui, &painter, rect, x, l, fx_open, actions);
    // Dragging the fx badge drags all the layer's effects, not the layer (Photoshop). Like the eye,
    // it takes the drag from the row; clicks still reach the row.
    if let Some(&(_, badge)) = indicators.iter().find(|(k, _)| *k == crate::layer_row_ui::Indicator::Fx)
        && ui.interact(badge, ui.scope_id().with(("fx-badge", l.id.0)), Sense::drag()).drag_started()
    {
        start_fx_drag(ctx, l.id, None);
    }
    // Clicking the lock icon unlocks the layer, and makes the Background a normal one (Photoshop,
    // #4723). It takes the click from the row, so the row isn't selected.
    if let Some(&(_, lock)) = indicators.iter().find(|(k, _)| *k == crate::layer_row_ui::Indicator::Lock)
        && ui.interact(lock, ui.scope_id().with(("row-lock", l.id.0)), Sense::click()).clicked()
    {
        actions.push(simple_lock_toggle(doc.is_background(l.id), l, &json!({"layer": l.id.0})));
    }
    let name_rect = paint_row_name(&painter, &t, &rc, x, name_right);
    crate::layer_row_ui::record(
        ctx,
        crate::layer_row_ui::RowRects { layer: l.id.0, row: rect, thumbnail: (ts > 0.0).then_some(thumb), name: name_rect, indicators },
    );
    // ⌘-click a layer, mask or vector-mask thumbnail loads its transparency / mask / path as a
    // selection (⇧ add, ⌥ subtract, ⇧⌥ intersect) instead of changing the layer selection.
    let load_at = |m: egui::Modifiers, p: Pos2| {
        if !m.command {
            return None;
        }
        if let Some(kind) = masks.hit(p) {
            return Some(crate::mask_thumbs_ui::load_params(l, kind, m));
        }
        // The Background has no transparency to load: Photoshop treats the click as a plain one.
        (ts > 0.0 && !doc.is_background(l.id) && thumb.expand(2.0).contains(p))
            .then(|| json!({"channel": "transparency", "layer": l.id.0, "operation": crate::channels_panel::load_operation(m)}))
    };
    let thumb_load = resp.clicked().then(|| (ui.input(|i| i.modifiers), resp.interact_pointer_pos())).and_then(|(m, pos)| load_at(m, pos?));
    // Over a thumbnail that ⌘ would load, the cursor says what the click does, as in Photoshop (#7824).
    if let Some(p) = resp.hover_pos() {
        let m = ui.input(|i| i.modifiers);
        if load_at(m, p).is_some() {
            thumb_load_cursor(ctx, p, m);
        }
    }
    // ⇧-click a mask thumbnail: disable / enable that mask; ⌥-click a layer mask: view it.
    let mask_toggle = resp.clicked().then(|| (ui.input(|i| i.modifiers), resp.interact_pointer_pos())).and_then(|(m, pos)| {
        let kind = masks.hit(pos?)?;
        crate::mask_thumbs_ui::click_command(l, kind, m)
    });
    if let Some(p) = thumb_load {
        actions.push(("select.loadSelection".into(), p));
    } else if let Some(cmd) = mask_toggle {
        actions.push(cmd);
    } else if resp.clicked() && !eye_resp.clicked() && !toggled && !fx_toggled && !masks.clicked {
        select_row_on_click(app, ui, &rc, actions);
    }
    // Double-click: the name renames in place (over the row's full height, not just the glyphs:
    // #651); the Background, which can't be renamed while it's locked, becomes a normal layer; an
    // adjustment, fill or shape thumbnail opens its settings (a solid colour: the Color Picker,
    // #2812) and a Smart Object thumbnail its contents, and a type thumbnail edits its text;
    // anywhere else on the row opens Layer Style (#350, #537).
    // The first click already made this the active layer.
    handle_row_double_click(app, ui.ctx(), &rc, name_rect, actions);
    let edit_rect = Rect::from_min_max(pos2(x - 3.0, rect.center().y - 11.0), pos2(name_right.max(x + 40.0), rect.center().y + 11.0));
    if let Some(done) = crate::layer_row_ui::rename_field(ui, l.id.0, edit_rect) {
        actions.push(done);
    }
    row_context_menu(app, ui, &rc, &eye_resp, actions);
    rect
}

/// What the row helpers share once the row's thumbnails are laid out.
struct RowCtx<'a> {
    l: &'a Layer,
    doc: &'a photocraft_doc::Document,
    row: RowSel,
    rect: Rect,
    ts: f32,
    thumb: Rect,
    resp: &'a egui::Response,
    masks: &'a crate::mask_thumbs_ui::MaskRects,
}

fn eye_cell(rect: Rect) -> Rect {
    Rect::from_min_max(rect.left_top(), pos2((rect.left() + EYE_COL_W).min(rect.right()), rect.bottom()))
}

/// The row's fill (selection / hover), the label colour behind the eye column and the pro dividers.
fn paint_row_background(painter: &egui::Painter, t: &Tokens, shown: photocraft_doc::LabelColor, rect: Rect, selected: bool, is_hovered: bool) {
    if t.pro {
        if selected {
            painter.rect_filled(rect, 0.0, t.row_selected);
        } else if is_hovered {
            painter.rect_filled(rect, 0.0, t.hover.gamma_multiply(0.5));
            painter.rect_filled(rect, 0.0, t.accent.gamma_multiply(0.12));
        }
    } else if selected {
        painter.rect_filled(rect, t.radius, t.hover);
        painter.rect_stroke(rect, t.radius, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
    } else if is_hovered {
        painter.rect_filled(rect, t.radius, t.hover.gamma_multiply(0.5));
        painter.rect_filled(rect, t.radius, t.accent.gamma_multiply(0.12));
    }
    let eye_cell = eye_cell(rect);
    let label = t.layer_label_colors(shown);
    if let Some((_, background)) = label {
        painter.rect_filled(eye_cell, 0.0, background);
    }
    if t.pro {
        // Dividers inside the row, so rows sit flush with no gap between them (Photoshop): the
        // theme's separator under the row and beside the eye column, and a darker shade of a
        // label colour between coloured eye cells.
        let bottom = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 1.0), rect.right_bottom());
        let divider = layer_divider(t);
        painter.rect_filled(bottom, 0.0, divider);
        painter.rect_filled(Rect::from_min_max(pos2(eye_cell.right() - 1.0, rect.top()), eye_cell.right_bottom()), 0.0, divider);
        if let Some((_, background)) = label {
            let line = Rect::from_min_max(pos2(eye_cell.left(), rect.bottom() - 2.0), pos2(eye_cell.right() - 1.0, rect.bottom()));
            painter.rect_filled(line, 0.0, darken(background, 0.22));
        }
    }
}

/// The eye column: visibility toggle (⌥ shows only this layer), sweep-drag and its colour menu.
fn eye_column(app: &mut PhotocraftApp, ui: &mut egui::Ui, l: &Layer, row: RowSel, rect: Rect, actions: &mut Vec<(String, Value)>) -> egui::Response {
    let ctx = &ui.ctx().clone();
    let t = Tokens::get(ctx);
    let selected = row.selected;
    let eye_cell = eye_cell(rect);
    // The whole eye column is the eye's hit area, as in Photoshop: a click anywhere in it, however
    // far to the side or above or below the glyph, toggles visibility instead of selecting the row.
    // It takes drags too (so a drag starting on it never reorders the row): dragging down the eyes
    // gives every row swept over the visibility the first eye toggled to (#8467).
    let eye_resp = ui.interact(eye_cell, ui.scope_id().with(("eye", l.id.0)), Sense::click_and_drag());
    // A hidden layer's eye box is left empty (still clickable); inside a hidden group the eye
    // is greyed out, as the layer doesn't show either way (Photoshop).
    if l.visible {
        let color = t.layer_label_icon(l.label);
        let color = if row.parent_hidden { color.gamma_multiply(0.35) } else { color };
        if t.pro {
            crate::layer_row_ui::paint_eye(ui.painter(), eye_cell.center() - vec2(0.5, 0.0), color);
        } else {
            icons::paint(ui, eye_cell, "eye", 15.0, color);
        }
    }
    eye_sweep(ctx, l, rect, &eye_resp, actions);
    if eye_resp.clicked() {
        // ⌥-click shows only this layer; ⌥-click it again to restore the others.
        if ui.input(|i| i.modifiers.alt) {
            actions.push(("layer.showOnly".into(), json!({"layer": l.id.0})));
        } else {
            actions.push(("layer.setProps".into(), json!({"layer": l.id.0, "visible": !l.visible})));
        }
    }
    eye_resp.context_menu(|ui| {
        crate::widgets::style_spectrum_popup_menu(ui);
        crate::layer_menu_ui::color_choices(app, ui, l, row.multi && selected, actions);
    });
    eye_resp
}

fn paint_target_brackets(app: &PhotocraftApp, painter: &egui::Painter, t: &Tokens, rc: &RowCtx<'_>) {
    let (l, thumb) = (rc.l, rc.thumb);
    let mask_rect = rc.masks.thumb(crate::mask_thumbs_ui::MaskKind::Pixel);
    let second_rect = rc.masks.thumb(crate::mask_thumbs_ui::MaskKind::Second);
    let vector_rect = rc.masks.thumb(crate::mask_thumbs_ui::MaskKind::Vector);
    // Photoshop frames the targeted thumbnail (pixels, mask or vector mask) of the active layer
    // with corner brackets.
    if rc.row.primary && rc.ts > 0.0 {
        let target = if app.ui.filter_mask_target == Some(l.id.0) && app.filter_mask_targeted() {
            // The Smart Filters row's filter mask thumbnail carries the brackets.
            None
        } else if app.ui.vector_mask_target && vector_rect.is_some() {
            vector_rect
        } else if app.ui.mask_target && app.ui.second_mask_target && second_rect.is_some() {
            second_rect
        } else if app.ui.mask_target {
            mask_rect.or(Some(thumb))
        } else {
            Some(thumb)
        };
        // A group's folder glyph is no thumbnail: Photoshop frames nothing there.
        let target = target.filter(|r| !(t.pro && l.is_group() && *r == thumb));
        if let Some(r) = target {
            crate::mask_thumbs_ui::paint_brackets(painter, r, t.text);
        }
    }
}

/// The layer name (and the kind sub-label of non-pixel rows); returns the name's rect.
fn paint_row_name(painter: &egui::Painter, t: &Tokens, rc: &RowCtx<'_>, x: f32, name_right: f32) -> Option<Rect> {
    let (l, doc, rect, ts, selected) = (rc.l, rc.doc, rc.rect, rc.ts, rc.row.selected);
    let name_color = if l.visible { t.text } else { t.text_faint };
    let font = if selected && !t.pro { theme::medium(13.0) } else { egui::FontId::proportional(if t.pro { PRO_NAME_SIZE } else { 13.0 }) };
    // Photoshop before 2026 set the Background layer's name in italics; 2026 sets it upright.
    let italic = !t.pro && doc.is_background(l.id);
    // Photoshop rows show only the name; the kind sub-label is a Studio-theme addition.
    let is_pixel = t.pro || ts == 0.0 || matches!(l.content, LayerContent::Raster(_));
    let name_rect = crate::layer_row_ui::truncated(painter, &l.name, font, name_color, italic, name_right - x).map(|galley| {
        let text_pos = pos2(x, rect.center().y - galley.size().y / 2.0 - if is_pixel { 0.0 } else { 7.0 });
        let r = Rect::from_min_size(text_pos, galley.size());
        painter.galley(text_pos, galley, name_color);
        r
    });
    if !is_pixel {
        let sub = match &l.content {
            LayerContent::Adjustment(a) => tl!(a.label()).to_string(),
            LayerContent::Group(g) => crate::i18n::trn(crate::i18n::current(), g.children.len() as u64, "Group · {n} layer", "Group · {n} layers"),
            other => tl!(other.kind_name()).to_string(),
        };
        crate::layer_row_ui::label(painter, x, rect.center().y + 8.0, name_right, &sub, egui::FontId::proportional(11.0), t.text_faint);
    }
    name_rect
}

/// A plain click on the row: select the layer and retarget pixels / mask / vector mask.
fn select_row_on_click(app: &mut PhotocraftApp, ui: &egui::Ui, rc: &RowCtx<'_>, actions: &mut Vec<(String, Value)>) {
    let (l, doc, resp, masks, row, ts, thumb) = (rc.l, rc.doc, rc.resp, rc.masks, rc.row, rc.ts, rc.thumb);
    let ctx = ui.ctx();
    let mask_rect = masks.thumb(crate::mask_thumbs_ui::MaskKind::Pixel);
    let m = ui.input(|i| i.modifiers);
    let pos = resp.interact_pointer_pos();
    let on_thumb = ts > 0.0 && pos.is_some_and(|p| thumb.expand(2.0).contains(p));
    // ⌘-click on the Background thumbnail stands in for the plain click (there is no
    // transparency to load): it selects the Background alone, not toggled into the set.
    let mode = if m.command && on_thumb && doc.is_background(l.id) { "replace" } else { select_mode(m) };
    actions.push(("layer.select".into(), json!({"layer": l.id.0, "mode": mode})));
    crate::smart_ui::deselect(ctx, doc.id);
    // A thumbnail explicitly selects content or a mask. A click elsewhere on an
    // adjustment/fill row targets the layer itself (its settings show in Properties); only its
    // mask thumbnail targets the mask.
    let on_mask = mask_rect.zip(pos).is_some_and(|(r, p)| r.expand(2.0).contains(p));
    let on_vector = pos.and_then(|p| masks.hit(p)) == Some(crate::mask_thumbs_ui::MaskKind::Vector);
    let on_second = pos.and_then(|p| masks.hit(p)) == Some(crate::mask_thumbs_ui::MaskKind::Second);
    // Clicking the layer thumbnail leaves mask view (#196).
    let viewing = app.session.active().and_then(photocraft_engine::mask_view_cmds::current).filter(|v| v.layer == l.id);
    if let Some(v) = viewing.filter(|_| on_thumb) {
        let mask = if v.filter { "filter" } else { "layer" };
        actions.push((photocraft_engine::mask_view_cmds::ID.into(), json!({"layer": l.id.0, "mode": "off", "mask": mask})));
    }
    let content_less = matches!(l.content, LayerContent::Adjustment(_) | LayerContent::Fill(_));
    if on_vector {
        // The vector mask thumbnail targets the vector mask: the path tools edit it and the
        // Paths panel selects the layer's path.
        app.ui.selected_path = Some("layer".into());
        actions.push(("ui.vectorMaskTarget".into(), json!(true)));
    } else if on_second {
        // The second mask's thumbnail targets it: painting and filters edit it.
        app.ui.properties_layer = None;
        actions.push(("ui.secondMaskTarget".into(), json!(true)));
    } else if on_mask {
        if on_mask {
            // The mask thumbnail also switches Properties to the mask (#3369).
            app.ui.properties_layer = None;
        }
        actions.push(("ui.maskTarget".into(), json!(true)));
    } else if on_thumb || !row.primary || content_less {
        actions.push(("ui.maskTarget".into(), json!(false)));
    }
    // ⌥-click the layer thumbnail: fit the layer on screen, as Photoshop does.
    if on_thumb && m.alt && !m.shift && !m.command {
        actions.push(("view.fitLayersOnScreen".into(), Value::Null));
    }
    // A plain click on a disabled mask turns it back on.
    if let Some(cmd) = pos.and_then(|p| masks.hit(p)).and_then(|kind| crate::mask_thumbs_ui::enable_on_click(l, kind, m)) {
        actions.push(cmd);
    }
}

/// Double-click: rename, un-background, open the thumbnail's settings or the row's Layer Style.
fn handle_row_double_click(app: &mut PhotocraftApp, ctx: &egui::Context, rc: &RowCtx<'_>, name_rect: Option<Rect>, actions: &mut Vec<(String, Value)>) {
    let (l, doc, resp, masks, ts, thumb, rect) = (rc.l, rc.doc, rc.resp, rc.masks, rc.ts, rc.thumb, rc.rect);
    // Both clicks on the same part of this layer's row within Windows' double-click time count
    // (`layer_row_ui::second_click`).
    let pos = resp.interact_pointer_pos();
    let on = |r: Rect| pos.is_some_and(|p| r.expand(2.0).contains(p));
    let part = match pos.and_then(|p| masks.hit(p)) {
        _ if name_rect.is_some_and(|n| on(Rect::from_x_y_ranges(n.x_range(), rect.y_range()))) => 1,
        Some(crate::mask_thumbs_ui::MaskKind::Pixel) => 2,
        Some(crate::mask_thumbs_ui::MaskKind::Second) => 3,
        Some(crate::mask_thumbs_ui::MaskKind::Vector) => 4,
        None if ts > 0.0 && on(thumb) => 5,
        None => 0,
    };
    if crate::layer_row_ui::second_click(ctx, resp, l.id.0, part) {
        if crate::doc_props_ui::is_background(doc, l) {
            actions.push(("layer.new.layerFromBackground".into(), json!({})));
        } else if name_rect.is_some_and(|n| on(Rect::from_x_y_ranges(n.x_range(), rect.y_range()))) {
            if let Some(done) = crate::layer_row_ui::start_rename(ctx, l.id.0, &l.name) {
                // One rename at a time: starting this one commits any other (#314).
                actions.push(done);
            }
        } else if let Some(kind) = pos.and_then(|p| masks.hit(p)) {
            match kind {
                crate::mask_thumbs_ui::MaskKind::Pixel => actions.push(("ui.maskTarget".into(), json!(true))),
                crate::mask_thumbs_ui::MaskKind::Second => actions.push(("ui.secondMaskTarget".into(), json!(true))),
                crate::mask_thumbs_ui::MaskKind::Vector => actions.push(("ui.vectorMaskTarget".into(), json!(true))),
            }
            if let Some((cmd, val)) = crate::mask_thumbs_ui::double_click_command(app, kind) {
                // The preference launches Select and Mask on a pixel mask.
                actions.push((cmd.into(), val));
            } else {
                // Otherwise double-click opens the mask's settings (Density/Feather).
                actions.push(("ui.properties".into(), json!(true)));
            }
        } else if pos.and_then(|p| masks.hit(p)).is_none() {
            let id = match &l.content {
                LayerContent::Adjustment(_) | LayerContent::Fill(_) | LayerContent::Shape(_) if ts > 0.0 && on(thumb) => "layer.layerContentOptions",
                LayerContent::Smart(_) if ts > 0.0 && on(thumb) => "layer.smartObjects.editContents",
                LayerContent::Text(_) if ts > 0.0 && on(thumb) => "type.editText",
                _ => "layer.layerStyle.blendingOptions",
            };
            if crate::menus::is_enabled(app, id) {
                // Null params: run like the menu item, dialog included.
                actions.push((id.into(), Value::Null));
            }
        }
    }
}

/// The visibility icon has its own color menu, and the layer-mask thumbnail its mask menu (#4575).
fn row_context_menu(app: &mut PhotocraftApp, ui: &mut egui::Ui, rc: &RowCtx<'_>, eye_resp: &egui::Response, actions: &mut Vec<(String, Value)>) {
    let (l, resp, masks, row) = (rc.l, rc.resp, rc.masks, rc.row);
    let (ctx, selected) = (&ui.ctx().clone(), row.selected);
    // The visibility icon has its own color menu, and the layer-mask thumbnail its mask menu
    // (#4575); elsewhere use the full layer menu.
    let on_mask_id = ui.scope_id().with(("mask-menu", l.id.0));
    let on_second_id = ui.scope_id().with(("second-mask-menu", l.id.0));
    if resp.secondary_clicked() {
        let hit = resp.interact_pointer_pos().and_then(|p| masks.hit(p));
        ctx.data_mut(|d| {
            d.insert_temp(on_mask_id, hit == Some(crate::mask_thumbs_ui::MaskKind::Pixel));
            d.insert_temp(on_second_id, hit == Some(crate::mask_thumbs_ui::MaskKind::Second));
        });
    }
    if !eye_resp.secondary_clicked() && !eye_resp.context_menu_opened() {
        resp.context_menu(|ui| {
            crate::widgets::style_spectrum_popup_menu(ui);
            if ui.ctx().data(|d| d.get_temp::<bool>(on_mask_id)).unwrap_or(false) {
                crate::layer_menu_ui::show_mask(app, ui, l, actions);
                return;
            }
            if ui.ctx().data(|d| d.get_temp::<bool>(on_second_id)).unwrap_or(false) {
                crate::layer_menu_ui::show_second_mask(ui, l, actions);
                return;
            }
            // Right-clicking inside a multi-selection keeps it and acts on every selected layer.
            let on_set = row.multi && selected;
            if crate::layer_menu_ui::show(app, ui, l, on_set, actions)
                && let Some(done) = crate::layer_row_ui::start_rename(ui.ctx(), l.id.0, &l.name)
            {
                actions.push(done);
            }
        });
    }
}

/// The thin border around Layers panel thumbnails in Photoshop's themes: a little lighter than
/// the panel in the dark greys (Photoshop's #666 on its darkest grey), darker in the light ones.
pub(crate) fn pro_thumb_border(t: &Tokens) -> Color32 {
    let card = t.card.r();
    if card < 128 { Color32::from_gray(card.saturating_add(20).max(102)) } else { Color32::from_gray(card.saturating_sub(70)) }
}

/// Does `l`'s row show its pixels (pixel, shape and Smart Object layers), rather than a tile?
fn thumb_shows_pixels(l: &Layer) -> bool {
    !matches!(l.content, LayerContent::Adjustment(_) | LayerContent::Group(_) | LayerContent::Text(_) | LayerContent::Fill(_))
}

/// `w` x `h` scaled to fit `cell`, keeping its aspect ratio (at least 4 pt each way), left-aligned
/// (Photoshop's themes) or centred, and vertically centred.
pub(super) fn fit_in_cell(cell: Rect, w: f32, h: f32, left: bool) -> Rect {
    let (w, h) = (if w.is_finite() { w.max(1.0) } else { 1.0 }, if h.is_finite() { h.max(1.0) } else { 1.0 });
    let k = (cell.width() / w).min(cell.height() / h);
    let size = vec2((w * k).round().max(4.0), (h * k).round().max(4.0)).min(cell.size());
    let min = if left { pos2(cell.left(), cell.center().y - size.y / 2.0) } else { cell.center() - size / 2.0 };
    Rect::from_min_size(pos2(min.x.round(), min.y.round()), size)
}

/// Where a Layers panel row draws `l`'s thumbnail inside its square `cell`. Like Photoshop's
/// default (Layers Panel Options > Thumbnail Contents > Layer Bounds), a layer with pixels shows
/// only what it covers, at that area's own shape: a wide strip gets a wide thumbnail, a tall one a
/// tall one, an empty layer the canvas. Other kinds fill the cell.
fn layer_thumb_rect(app: &mut PhotocraftApp, ctx: &egui::Context, doc: &photocraft_doc::Document, l: &Layer, cell: Rect, pro: bool) -> Rect {
    if !thumb_shows_pixels(l) {
        return cell;
    }
    // Without Clip Thumbnails to Layer Bounds the thumbnail frames the whole canvas, letterboxed
    // in the cell (`draw_layer_thumb`).
    if !app.session.prefs().interface.clip_thumbnails_to_layer_bounds {
        return cell;
    }
    let (_, [w, h]) = app.layer_bounds_thumb(ctx, doc, l);
    fit_in_cell(cell, w as f32, h as f32, pro)
}

/// Screen rect and UVs for a layer thumbnail: the document-shaped part of the square cell and of
/// the letterboxed square texture, so tall and wide documents don't show empty bars.
pub(super) fn layer_thumb_fit(cell: Rect, w: u32, h: u32) -> (Rect, Rect) {
    crate::channels_panel::fit_thumb(cell, w, h)
}

fn draw_layer_thumb(app: &mut PhotocraftApp, ctx: &egui::Context, ui: &egui::Ui, doc: &photocraft_doc::Document, l: &Layer, rect: Rect, selected: bool) {
    let t = Tokens::get(ctx);
    let p = ui.painter();
    match &l.content {
        LayerContent::Adjustment(adj) => {
            // Photoshop: each adjustment type's own glyph, on the panel itself (no tile).
            crate::adjust_icons::paint(ui, rect, adj, t.icon);
        }
        LayerContent::Group(_) => {
            p.rect_filled(rect, 6.0, t.field);
            icons::paint(ui, rect, "folder", 18.0, t.icon);
        }
        LayerContent::Text(_) => {
            // Photoshop: type layers show a "T" tile rather than a pixel thumbnail.
            p.rect_filled(rect, if t.pro { 0.0 } else { 6.0 }, Color32::from_gray(if t.pro { 222 } else { 236 }));
            icons::paint(ui, rect, "type", 20.0, Color32::from_gray(40));
        }
        // Gradient fills show the gradient itself (Photoshop); solid ones their colour.
        LayerContent::Fill(f) if crate::gradient_ui::paint_thumbnail(ui, l.id, f, rect) => {}
        LayerContent::Fill(photocraft_doc::Fill::Solid(color)) => {
            crate::solid_fill_ui::paint_thumbnail(ui, crate::solid_fill_ui::thumbnail_color(app, doc.id, l.id, *color), rect);
        }
        LayerContent::Fill(_) => {
            let c = [128, 128, 128, 255];
            p.rect_filled(rect, 6.0, Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]));
        }
        _ => {
            // Keep row layout and outside thumbnail decorations when the image is clipped.
            if ui.is_rect_visible(rect) {
                if app.session.prefs().interface.clip_thumbnails_to_layer_bounds {
                    // `rect` already has the layer bounds' shape (`layer_thumb_rect`).
                    widgets::checker(p, rect, if t.pro { 4.0 } else { 5.0 });
                    let (tex, _) = app.layer_bounds_thumb(ctx, doc, l);
                    p.image(tex, rect, Rect::from_min_max(egui::Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
                } else {
                    // The texture is a letterboxed square: draw only the part its frame (the document,
                    // or the layer's bounds) fills.
                    let (tex, frame) = app.layer_thumb(ctx, doc, l);
                    let (fitted, uv) = layer_thumb_fit(rect, frame.width(), frame.height());
                    widgets::checker(p, fitted, 5.0);
                    p.image(tex, fitted, uv, Color32::WHITE);
                }
            }
        }
    }
    let stroke = if t.pro {
        Stroke::new(1.0, pro_thumb_border(&t))
    } else if selected {
        Stroke::new(1.5, t.text)
    } else {
        Stroke::new(1.0, t.field_border)
    };
    p.rect_stroke(rect, CornerRadius::same(if t.pro { 0 } else { 5 }), stroke, StrokeKind::Outside);
    crate::smart_ui::thumb_badge(ui, l, rect);
}
