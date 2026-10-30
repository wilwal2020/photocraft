//! Layers panel: ⌥-click the line between two layers to clip the upper one to the one below,
//! or to release it when it is clipped already (Photoshop, #967).
//!
//! The line is the top edge of a layer row, and the row drawn above it must be its sibling
//! directly above in the stack: a line inside an open group, under a group's last child or
//! across rows hidden by a filter clips nothing. While ⌥ is held over a line the pointer is the
//! clip cursor, and a click there goes to the line, not to the rows on either side of it.
//!
//! Measured on Photoshop 25.4 (Background, a pixel layer, an adjustment layer and a layer on
//! top; ⌥-hover and ⌥-click swept a pixel at a time across each line, the result read back by
//! script):
//! - the line takes the pointer from 3 px above it to 5 px below it;
//! - a click that clips keeps the active layer;
//! - a click that releases frees the layer above the line together with the clipped layers
//!   stacked above it (in one history step), and makes it the active layer;
//! - the release cursor is the clip cursor struck through.

use egui::{Pos2, Rangef, Rect, Sense, vec2};
use photocraft_doc::{Document, LayerContent, LayerId};
use serde_json::{Value, json};

use crate::layer_row_ui::{RIGHT_PAD, RowRects};

/// How far above and below a line its hit band reaches (Photoshop: 3 px up, 5 px down).
const ABOVE: f32 = 3.5;
const BELOW: f32 = 5.0;
/// The eye column, left of the band: ⌥-click there shows only that layer.
const EYE_W: f32 = 30.0;
/// How far the pointer may move between press and release for it to count as a click on the
/// line (a pen tip slides a little as it taps).
const SLIDE: f32 = 12.0;
/// Size of the clip cursors (points).
const CURSOR_SIZE: f32 = 22.0;

/// A line between two layers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipLine {
    /// The layer above the line: the one clipped or released.
    pub upper: LayerId,
    /// It is clipped already, so the click releases it.
    pub release: bool,
    /// The line's hit band.
    pub rect: Rect,
}

impl ClipLine {
    /// The commands a click runs: Create Clipping Mask on the upper layer, or Release Clipping
    /// Mask on it and the clipped layers stacked above it (`key` makes that one history step)
    /// before it becomes the active layer.
    pub fn commands(&self, doc: &Document, key: &str) -> Vec<(String, Value)> {
        if !self.release {
            return vec![("layer.createClippingMask".into(), json!({"layer": self.upper.0}))];
        }
        let mut out: Vec<(String, Value)> =
            release_chain(doc, self.upper).into_iter().map(|id| ("layer.releaseClippingMask".into(), json!({"layer": id.0, "coalesce": key}))).collect();
        out.push(("layer.select".into(), json!({"layer": self.upper.0, "mode": "replace"})));
        out
    }
}

/// The layers a release on `upper` frees: it and the clipped layers stacked directly above it.
fn release_chain(doc: &Document, upper: LayerId) -> Vec<LayerId> {
    let mut out = vec![upper];
    let Some(mut path) = doc.path_of(upper) else { return out };
    while let Some(last) = path.pop() {
        let Some(next) = last.checked_add(1) else { break };
        path.push(next);
        match doc.layer_at(&path) {
            Some(l) if l.clipped => out.push(l.id),
            _ => break,
        }
    }
    out
}

/// The line under `p` among the layer rows the panel drew (top to bottom), if two layers that
/// can clip meet there.
pub fn at(doc: &Document, rows: &[RowRects], p: Pos2) -> Option<ClipLine> {
    rows.windows(2).find_map(|pair| {
        let [above, below] = pair else { return None };
        // Between the eye column and the right-hand indicators (the effects triangle takes clicks).
        let right = above.indicators.iter().chain(&below.indicators).map(|(_, r)| r.left()).fold(below.row.right() - RIGHT_PAD, f32::min);
        let y = below.row.top();
        let rect = Rect::from_x_y_ranges(Rangef::new(below.row.left() + EYE_W, right), Rangef::new(y - ABOVE, y + BELOW));
        if !rect.contains(p) {
            return None;
        }
        let upper = LayerId(above.layer);
        let (up, low) = (doc.path_of(upper)?, doc.path_of(LayerId(below.layer))?);
        let ((iu, up_parent), (il, low_parent)) = (up.split_last()?, low.split_last()?);
        if up_parent != low_parent || il.checked_add(1) != Some(*iu) {
            return None;
        }
        let l = doc.layer_at(&up)?;
        // An open group's contents are listed between it and the layer below it.
        if matches!(&l.content, LayerContent::Group(g) if g.expanded && !g.children.is_empty()) {
            return None;
        }
        Some(ClipLine { upper, release: l.clipped, rect })
    })
}

/// After the panel drew its rows: while ⌥ is held over a line, show the clip cursor and turn a
/// click there into Create / Release Clipping Mask.
///
/// The line owns the press until the release, so the rows under it never start a drag: a pen
/// that slides a little while it taps (user report: "even a slight movement during the click
/// while holding alt won't work") still clips, as long as it comes up within [`SLIDE`] of where
/// it went down.
pub fn show(ui: &mut egui::Ui, doc: &Document, actions: &mut Vec<(String, Value)>) {
    let ctx = ui.ctx().clone();
    let id = ui.scope_id().with("clip-line");
    let (alt, hover, pressed, released) = ctx.input(|i| (i.modifiers.alt, i.pointer.hover_pos(), i.pointer.primary_pressed(), i.pointer.primary_released()));
    let held = ctx.data(|d| d.get_temp::<(ClipLine, Pos2)>(id));
    let line = match held {
        Some((line, _)) => Some(line),
        None => hover
            .filter(|_| alt && ctx.dragged_id().is_none())
            .and_then(|p| at(doc, &crate::layer_row_ui::recorded(&ctx), p))
            .filter(|l| ui.rect_contains_pointer(l.rect)),
    };
    let Some(line) = line else { return };
    // Registered after the rows, so it is on top of them and gets the press, and the drag too.
    let resp = ui.interact(line.rect, id, Sense::click_and_drag());
    let label = if line.release { "Release Clipping Mask" } else { "Create Clipping Mask" };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    // PhotoCraft's own cursors: a hooked arrow onto the layer below, and the same arrow with a
    // small "no" badge to release. The arrow's tip, (15, 16) of the 24-unit box, is the hot spot.
    if let Some(p) = hover {
        let icon = if line.release { "clip-release" } else { "clip-below" };
        crate::icons::cursor(&ctx, icon, p, vec2(15.0, 16.0) / 24.0, CURSOR_SIZE, false);
        ctx.set_cursor_icon(egui::CursorIcon::None);
    }
    match held {
        None if pressed && resp.contains_pointer() => {
            if let Some(p) = hover {
                ctx.data_mut(|d| d.insert_temp(id, (line, p)));
            }
        }
        Some((_, from)) if released || !ctx.input(|i| i.pointer.primary_down()) => {
            ctx.data_mut(|d| d.remove::<(ClipLine, Pos2)>(id));
            let up = ctx.input(|i| i.pointer.interact_pos()).or(hover);
            if released && up.is_some_and(|p| p.distance(from) <= SLIDE) {
                let key = format!("clip-line:{}:{}", line.upper.0, ctx.cumulative_pass_nr());
                actions.extend(line.commands(doc, &key));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
