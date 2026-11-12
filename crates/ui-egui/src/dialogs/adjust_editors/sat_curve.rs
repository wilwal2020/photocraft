//! Saturation Curve and its Advanced Curve (Shading, Shading 2, Lighting; user's design,
//! 2026-11-10): the path a colour takes on the colour picker, drawn by hand. The box in the
//! middle of the graph runs from the base colour (top-left) to full shadow or light
//! (bottom-right), stretched over each colour's own; the curve may leave it, half a box each way,
//! where it runs on toward the picker's edges (right vivid, left grey, up brighter, down darker;
//! Lighting's up and down the other way round). The slider draws one bulge on the same graph;
//! a hand-drawn curve locks the slider until Reset (`saturationPoints`, see
//! `photocraft_compose::adjust::shading::path_table`).

use super::basic::sliders;
use super::{Edit, RichText, Tokens, Value, json, widgets};
use egui::{Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use photocraft_compose::adjust::shading::{PATH_MARGIN, path_polyline, slider_points};
use photocraft_doc::adjust::CurvePoint;

/// Most points a drawn curve keeps, both ends included.
const MAX_POINTS: usize = 16;
/// Pixels from a handle that still grab it.
const HIT: f32 = 9.0;
/// Dragged this far off the graph, a point is removed (back on, it returns).
const DRAG_OFF: f32 = 14.0;

/// The hand-drawn points in `v` (graph units: base (0, 1), full shadow (1, 0)); fewer than three
/// is nothing drawn.
fn read(v: &Value) -> Vec<[f32; 2]> {
    let pts: Vec<[f32; 2]> = v
        .get("saturationPoints")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|q| {
                    let q = q.as_array()?;
                    Some([q.first()?.as_f64()? as f32 / 255.0, q.get(1)?.as_f64()? as f32 / 255.0])
                })
                .filter(|q| q.iter().all(|x| x.is_finite()))
                .collect()
        })
        .unwrap_or_default();
    if pts.len() < 3 { Vec::new() } else { pts }
}

/// The curve shown and whether it is drawn by hand: the drawn one, else the slider's
/// (`saturationCurve`, -100..100), else the straight line.
fn shown(v: &Value) -> (Vec<[f32; 2]>, bool) {
    let drawn = read(v);
    if !drawn.is_empty() {
        return (drawn, true);
    }
    let curve = v.get("saturationCurve").and_then(Value::as_f64).unwrap_or(0.0) as f32 / 100.0;
    let pts: Vec<[f32; 2]> = slider_points(curve).iter().map(|p| [p.input, p.output]).collect();
    (if pts.is_empty() { vec![[0.0, 1.0], [1.0, 0.0]] } else { pts }, false)
}

fn write(v: &mut Value, pts: &[[f32; 2]]) {
    if pts.len() < 3 {
        // Nothing left between the ends: the straight line, and the slider back (at 0).
        v["saturationPoints"] = Value::Null;
        v["saturationCurve"] = json!(0.0);
    } else {
        v["saturationPoints"] = json!(pts.iter().map(|q| [(q[0] * 255.0).round(), (q[1] * 255.0).round()]).collect::<Vec<_>>());
    }
}

/// Distance from `p` to the segment `a`–`b`.
fn to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
    p.distance(a + ab * t)
}

/// Where a new point at `p` goes: after the point that starts the nearest stretch of the curve.
fn insert_at(pts: &[[f32; 2]], to_scr: impl Fn([f32; 2]) -> Pos2, p: Pos2) -> usize {
    let d = |i: usize| to_segment(p, to_scr(pts[i]), to_scr(pts[i + 1]));
    (0..pts.len().saturating_sub(1)).min_by(|i, j| d(*i).total_cmp(&d(*j))).unwrap_or(0) + 1
}

/// Gesture state: the selected point, and the one being dragged (its slot, grab offset, and
/// whether it is off the graph for now).
#[derive(Clone, Copy, Debug, Default)]
struct State {
    selected: Option<usize>,
    drag: Option<(usize, [f32; 2], bool)>,
}

/// Saturation Curve's slider, the "Advanced Curve" switch and, when on, its graph. `lighting`
/// names the far corner "Full light" and turns the brightness bands round.
pub(super) fn saturation_curve_editor(ui: &mut egui::Ui, v: &mut Value, mem: egui::Id, lighting: bool) -> Edit {
    let t = Tokens::get(ui.ctx());
    let mut e = Edit::default();
    let (mut pts, drawn) = shown(v);
    let slider = ui.add_enabled_ui(!drawn, |ui| sliders(ui, v, &[("saturationCurve", tl!("Saturation Curve"), -100.0, 100.0, 0.0, "", Some(0))]));
    e.add(slider.inner);
    if drawn {
        let _ = slider.response.on_hover_text(tl!("Locked: the Advanced Curve is drawn by hand. Reset it to use the slider again."));
    } else if slider.inner.changed {
        pts = shown(v).0;
    }
    let open_id = mem.with("sat-curve-open");
    let mut open: bool = ui.data(|d| d.get_temp(open_id)).unwrap_or(false);
    let mut reset = false;
    ui.horizontal(|ui| {
        widgets::checkbox(ui, &mut open, tl!("Advanced Curve"));
        if drawn {
            ui.label(RichText::new(tl!("(drawn)")).color(t.text_dim).size(11.0));
            reset = widgets::compact_button(ui, tl!("Reset"), 20.0).on_hover_text(tl!("Back to the Saturation Curve slider's curve")).clicked();
        }
    });
    if reset {
        v["saturationPoints"] = Value::Null;
        pts = shown(v).0;
        e.add(Edit::discrete(true));
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));
    if !open {
        return e;
    }
    // Literal Curve: the drawn shape kept on the picker rather than stretched to each colour's box.
    let mut literal = v.get("literalCurve").and_then(Value::as_bool).unwrap_or(false);
    let r = widgets::checkbox(ui, &mut literal, tl!("Literal Curve")).on_hover_text(tl!(
        "Keep the curve's drawn shape on the colour picker: the same scale both ways, sized to each colour's own path, right always more saturated and up always brighter. Off, the box is stretched to each colour's own base and end, and past it the curve runs on to the picker's edges."
    ));
    if r.changed() || r.clicked() {
        v["literalCurve"] = json!(literal);
        e.add(Edit::discrete(true));
    }

    let side = ui.available_width().clamp(150.0, 260.0);
    ui.add_space(2.0);
    let (full, resp) = ui
        .horizontal(|ui| {
            ui.add_space(((ui.available_width() - side) / 2.0).max(0.0));
            ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag())
        })
        .inner;
    let graph = full.shrink(6.0);
    let lo = -PATH_MARGIN;
    let span = 1.0 + 2.0 * PATH_MARGIN;
    let to_scr = |q: [f32; 2]| pos2(graph.left() + (q[0] - lo) / span * graph.width(), graph.bottom() - (q[1] - lo) / span * graph.height());
    let to_val = |p: Pos2| {
        [((p.x - graph.left()) / graph.width() * span + lo).clamp(lo, lo + span), ((graph.bottom() - p.y) / graph.height() * span + lo).clamp(lo, lo + span)]
    };
    let hit = |pts: &[[f32; 2]], p: Pos2| {
        (1..pts.len().saturating_sub(1)).map(|i| (i, to_scr(pts[i]).distance(p))).filter(|(_, d)| *d <= HIT).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i)
    };

    // ---- interaction
    let state_id = mem.with("sat-curve-state");
    let mut st: State = ui.data(|d| d.get_temp(state_id)).unwrap_or_default();
    st.selected = st.selected.filter(|i| *i > 0 && *i + 1 < pts.len());
    let mut changed = false;
    let mut commit = false;
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some(i) = hit(&pts, p)
    {
        pts.remove(i);
        st = State::default();
        changed = true;
        commit = true;
    }
    let press = resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed());
    if press && let Some(p) = ui.input(|i| i.pointer.press_origin()) {
        resp.request_focus();
        let cmd = ui.input(|i| i.modifiers.command);
        match hit(&pts, p) {
            Some(i) if cmd => {
                pts.remove(i);
                st = State::default();
                changed = true;
                commit = true;
            }
            Some(i) => {
                let g = to_val(p);
                st.selected = Some(i);
                st.drag = Some((i, [pts[i][0] - g[0], pts[i][1] - g[1]], false));
            }
            None if !cmd && graph.contains(p) && pts.len() < MAX_POINTS => {
                let i = insert_at(&pts, to_scr, p);
                pts.insert(i, to_val(p));
                st.selected = Some(i);
                st.drag = Some((i, [0.0, 0.0], false));
                changed = true;
            }
            None => st.drag = None,
        }
    } else if let Some((i, off, removed)) = st.drag
        && resp.is_pointer_button_down_on()
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
    {
        let outside = !graph.expand(DRAG_OFF).contains(p);
        let g = to_val(p);
        let target = [(g[0] + off[0]).clamp(lo, lo + span), (g[1] + off[1]).clamp(lo, lo + span)];
        if outside && !removed && i > 0 && i + 1 < pts.len() {
            pts.remove(i);
            st.selected = None;
            st.drag = Some((i, off, true));
            changed = true;
        } else if !outside && removed {
            let slot = i.clamp(1, pts.len() - 1);
            pts.insert(slot, target);
            st.selected = Some(slot);
            st.drag = Some((slot, off, false));
            changed = true;
        } else if !removed && i > 0 && i + 1 < pts.len() && pts[i] != target {
            pts[i] = target;
            changed = true;
        }
    }
    if st.drag.is_some() && !resp.is_pointer_button_down_on() {
        st.drag = None;
        commit = true;
    }
    if resp.has_focus() {
        crate::shortcut_dispatch::claim_delete_keys(ui.ctx(), resp.id);
        if let Some(i) = st.selected.filter(|i| *i > 0 && *i + 1 < pts.len())
            && ui.input_mut(|inp| inp.consume_key(Modifiers::NONE, Key::Delete) || inp.consume_key(Modifiers::NONE, Key::Backspace))
        {
            pts.remove(i);
            st.selected = None;
            changed = true;
            commit = true;
        }
    }
    ui.data_mut(|d| d.insert_temp(state_id, st));
    if changed {
        write(v, &pts);
        e.changed = true;
    }
    e.commit |= commit;

    // ---- drawing
    let p = ui.painter_at(full.expand(2.0));
    p.rect_filled(graph, 0.0, t.field.gamma_multiply(0.6));
    let inner = Rect::from_two_pos(to_scr([0.0, 1.0]), to_scr([1.0, 0.0]));
    p.rect_filled(inner, 0.0, t.field);
    let g = Stroke::new(1.0, t.separator);
    for i in 1..4 {
        let f = i as f32 / 4.0;
        p.line_segment([pos2(inner.left() + f * inner.width(), inner.top()), pos2(inner.left() + f * inner.width(), inner.bottom())], g);
        p.line_segment([pos2(inner.left(), inner.top() + f * inner.height()), pos2(inner.right(), inner.top() + f * inner.height())], g);
    }
    // The path itself: base colour to full shadow, straight.
    p.line_segment([inner.left_top(), inner.right_bottom()], Stroke::new(1.0, t.separator.gamma_multiply(1.6)));
    p.rect_stroke(inner, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Middle);
    p.rect_stroke(
        graph,
        0.0,
        Stroke::new(if resp.has_focus() { 1.5 } else { 1.0 }, if resp.has_focus() { t.accent } else { t.field_border }),
        StrokeKind::Outside,
    );
    let small = egui::FontId::proportional(10.0);
    let end = if lighting { tl!("Full light") } else { tl!("Full shadow") };
    p.text(inner.left_top() + vec2(3.0, 2.0), egui::Align2::LEFT_TOP, tl!("Base color"), small.clone(), t.text_faint);
    p.text(inner.right_bottom() - vec2(3.0, 2.0), egui::Align2::RIGHT_BOTTOM, end, small.clone(), t.text_faint);
    let (up, down) = if lighting { (tl!("darker"), tl!("brighter")) } else { (tl!("brighter"), tl!("darker")) };
    p.text(pos2(graph.center().x, graph.top() + 2.0), egui::Align2::CENTER_TOP, up, small.clone(), t.text_faint);
    p.text(pos2(graph.center().x, graph.bottom() - 2.0), egui::Align2::CENTER_BOTTOM, down, small.clone(), t.text_faint);
    p.text(pos2(graph.right() - 3.0, graph.center().y), egui::Align2::RIGHT_CENTER, tl!("vivid"), small.clone(), t.text_faint);
    p.text(pos2(graph.left() + 3.0, graph.center().y), egui::Align2::LEFT_CENTER, tl!("grey"), small, t.text_faint);
    let cp: Vec<CurvePoint> = pts.iter().map(|q| CurvePoint { input: q[0], output: q[1] }).collect();
    let line: Vec<Pos2> = path_polyline(&cp).into_iter().map(to_scr).collect();
    p.add(egui::Shape::line(line, Stroke::new(1.5, if drawn || changed { t.text } else { t.text_dim })));
    for (i, q) in pts.iter().enumerate() {
        let c = to_scr(*q);
        if i == 0 || i + 1 == pts.len() {
            p.circle_filled(c, 3.0, t.text_dim);
            continue;
        }
        let r = Rect::from_center_size(c, vec2(super::curves::CURVE_HANDLE_SIZE, super::curves::CURVE_HANDLE_SIZE));
        if Some(i) == st.selected {
            p.rect_filled(r, 0.0, t.text);
            p.rect_stroke(r, 0.0, Stroke::new(1.0, egui::Color32::BLACK), StrokeKind::Outside);
        } else {
            p.rect_filled(r, 0.0, t.field);
            p.rect_stroke(r, 0.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
        }
    }
    let what = match (literal, lighting) {
        (true, _) => tl!("The path each colour takes on the colour picker, in the shape drawn: sized to each colour's own path, the same scale both ways."),
        (false, true) => tl!("The path each colour takes on the colour picker, stretched from its own base colour to its full light. Outside the box it runs on toward the picker's edges."),
        (false, false) => tl!("The path each colour takes on the colour picker, stretched from its own base colour to its full shadow. Outside the box it runs on toward the picker's edges."),
    };
    let hint = crate::i18n::fmt(tl!("Click to add a point · drag off or {key}-click or Delete to remove"), &[("key", &crate::shortcuts::pretty("Cmd"))]);
    ui.label(RichText::new(what).color(t.text_dim).size(11.0));
    ui.label(RichText::new(hint).color(t.text_faint).size(11.0));
    ui.add_space(4.0);
    e.add(hue_curve_editor(ui, v, mem, lighting));
    e
}

/// The Hue Curve's graph height in Curves' 0..255 units where the base hue (the box's top) and
/// full shadow's (its bottom) sit; the box is the middle half, half a box of room each way.
const HUE_TOP: f32 = 191.25;
const HUE_BOTTOM: f32 = 63.75;

/// The Hue Curve's points in `v` as Curves-style graph points (0..255 both ways); the straight
/// line, and false, when nothing is drawn.
fn read_hue(v: &Value) -> (Vec<[f32; 2]>, bool) {
    let pts: Vec<[f32; 2]> = v
        .get("huePoints")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|q| {
                    let q = q.as_array()?;
                    Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32 / 2.0 + HUE_BOTTOM])
                })
                .filter(|q| q.iter().all(|x| x.is_finite()))
                .collect()
        })
        .unwrap_or_default();
    if pts.len() < 3 { (vec![[0.0, HUE_TOP], [255.0, HUE_BOTTOM]], false) } else { (pts, true) }
}

fn write_hue(v: &mut Value, pts: &[[f32; 2]]) {
    if pts.len() < 3 {
        v["huePoints"] = Value::Null;
        return;
    }
    let last = pts.len() - 1;
    let out: Vec<[f32; 2]> = pts
        .iter()
        .enumerate()
        .map(|(i, q)| match i {
            0 => [0.0, 255.0],
            i if i == last => [255.0, 0.0],
            _ => [q[0].round(), ((q[1] - HUE_BOTTOM) * 2.0 * 10.0).round() / 10.0],
        })
        .collect();
    v["huePoints"] = json!(out);
}

/// The "Hue Curve" switch and, when on, its graph: when the hue turns, from the base colour's
/// (top) to full shadow's or light's (bottom), along the amount (left to right). Below the box
/// it turns further, above it back the other way. It times the path's own turn (Hue Shift,
/// Warm/Cool Split, the ambient or light colour).
fn hue_curve_editor(ui: &mut egui::Ui, v: &mut Value, mem: egui::Id, lighting: bool) -> Edit {
    let t = Tokens::get(ui.ctx());
    let mut e = Edit::default();
    let (mut pts, drawn) = read_hue(v);
    let open_id = mem.with("hue-curve-open");
    let mut open: bool = ui.data(|d| d.get_temp(open_id)).unwrap_or(drawn);
    let mut reset = false;
    ui.horizontal(|ui| {
        widgets::checkbox(ui, &mut open, tl!("Hue Curve"));
        if drawn {
            ui.label(RichText::new(tl!("(drawn)")).color(t.text_dim).size(11.0));
            reset = widgets::compact_button(ui, tl!("Reset"), 20.0).on_hover_text(tl!("Back to the straight line")).clicked();
        }
    });
    if reset {
        v["huePoints"] = Value::Null;
        pts = read_hue(v).0;
        e.add(Edit::discrete(true));
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));
    if !open {
        return e;
    }
    let side = ui.available_width().clamp(150.0, 260.0);
    ui.add_space(2.0);
    let (full, resp) = ui
        .horizontal(|ui| {
            ui.add_space(((ui.available_width() - side) / 2.0).max(0.0));
            ui.allocate_exact_size(vec2(side, side * 0.8), Sense::click_and_drag())
        })
        .inner;
    let graph = full.shrink(6.0);
    let state_id = mem.with("hue-curve-state");
    let mut st: crate::state::PointCurveState = ui.data(|d| d.get_temp(state_id)).unwrap_or_default();
    let it = crate::point_curve::interact(ui, &resp, graph, &mut pts, &mut st);
    ui.data_mut(|d| d.insert_temp(state_id, st));
    if it.changed {
        // The ends stay on the box's corners.
        if let Some(first) = pts.first_mut() {
            *first = [0.0, HUE_TOP];
        }
        if let Some(last) = pts.last_mut() {
            *last = [255.0, HUE_BOTTOM];
        }
        write_hue(v, &pts);
        e.changed = true;
    }
    e.commit |= it.commit;

    let to_scr = |q: [f32; 2]| pos2(graph.left() + q[0] / 255.0 * graph.width(), graph.bottom() - q[1] / 255.0 * graph.height());
    let p = ui.painter_at(full.expand(2.0));
    p.rect_filled(graph, 0.0, t.field.gamma_multiply(0.6));
    let inner = Rect::from_two_pos(to_scr([0.0, HUE_TOP]), to_scr([255.0, HUE_BOTTOM]));
    p.rect_filled(inner, 0.0, t.field);
    let g = Stroke::new(1.0, t.separator);
    for i in 1..4 {
        let f = i as f32 / 4.0;
        p.line_segment([pos2(inner.left() + f * inner.width(), inner.top()), pos2(inner.left() + f * inner.width(), inner.bottom())], g);
        p.line_segment([pos2(inner.left(), inner.top() + f * inner.height()), pos2(inner.right(), inner.top() + f * inner.height())], g);
    }
    p.line_segment([inner.left_top(), inner.right_bottom()], Stroke::new(1.0, t.separator.gamma_multiply(1.6)));
    p.rect_stroke(inner, 0.0, Stroke::new(1.0, t.field_border), StrokeKind::Middle);
    p.rect_stroke(
        graph,
        0.0,
        Stroke::new(if resp.has_focus() { 1.5 } else { 1.0 }, if resp.has_focus() { t.accent } else { t.field_border }),
        StrokeKind::Outside,
    );
    let small = egui::FontId::proportional(10.0);
    let end = if lighting { tl!("Full light's hue") } else { tl!("Full shadow's hue") };
    p.text(inner.left_top() + vec2(3.0, 2.0), egui::Align2::LEFT_TOP, tl!("Base hue"), small.clone(), t.text_faint);
    p.text(inner.right_bottom() - vec2(3.0, 2.0), egui::Align2::RIGHT_BOTTOM, end, small.clone(), t.text_faint);
    p.text(pos2(graph.center().x, graph.top() + 2.0), egui::Align2::CENTER_TOP, tl!("turns back"), small.clone(), t.text_faint);
    p.text(pos2(graph.center().x, graph.bottom() - 2.0), egui::Align2::CENTER_BOTTOM, tl!("turns further"), small, t.text_faint);
    let cp: Vec<CurvePoint> = pts.iter().map(|q| CurvePoint { input: q[0] / 255.0, output: q[1] / 255.0 }).collect();
    let line: Vec<Pos2> = photocraft_compose::adjust::curve_samples::<128>(&cp)
        .into_iter()
        .enumerate()
        .map(|(i, y)| pos2(graph.left() + i as f32 / 127.0 * graph.width(), graph.bottom() - y * graph.height()))
        .collect();
    p.add(egui::Shape::line(line, Stroke::new(1.5, t.text)));
    for (i, q) in pts.iter().enumerate() {
        let c = to_scr(*q);
        if i == 0 || i + 1 == pts.len() {
            p.circle_filled(c, 3.0, t.text_dim);
            continue;
        }
        let r = Rect::from_center_size(c, vec2(super::curves::CURVE_HANDLE_SIZE, super::curves::CURVE_HANDLE_SIZE));
        if Some(i) == st.selected {
            p.rect_filled(r, 0.0, t.text);
            p.rect_stroke(r, 0.0, Stroke::new(1.0, egui::Color32::BLACK), StrokeKind::Outside);
        } else {
            p.rect_filled(r, 0.0, t.field);
            p.rect_stroke(r, 0.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
        }
    }
    let what = if lighting {
        tl!("When the hue turns, from no light (left) to full light (right). It times the turn Hue Shift and the light colour give; below the box it turns further, above it turns back.")
    } else {
        tl!("When the hue turns, from no shadow (left) to full shadow (right). It times the turn Hue Shift and the ambient colour give; below the box it turns further, above it turns back.")
    };
    ui.label(RichText::new(what).color(t.text_dim).size(11.0));
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slider_draws_the_curve_until_one_is_drawn() {
        assert_eq!(shown(&json!({})), (vec![[0.0, 1.0], [1.0, 0.0]], false));
        assert!(!shown(&json!({"saturationPoints": [[0, 255], [255, 0]]})).1, "the straight line is nothing drawn");
        let (pts, drawn) = shown(&json!({"saturationCurve": 100}));
        assert!(!drawn && pts.len() == 3 && (pts[1][0] - 1.25).abs() < 1e-6, "{pts:?}");
        let (pts, drawn) = shown(&json!({"saturationCurve": 100, "saturationPoints": [[0, 255], [-60, 128], [255, 0]]}));
        assert!(drawn && (pts[1][0] + 60.0 / 255.0).abs() < 1e-6, "{pts:?}");
        // Removing the last point between the ends goes back to the straight line and the slider at 0.
        let mut v = json!({"saturationCurve": 40, "saturationPoints": [[0, 255], [100, 100], [255, 0]]});
        write(&mut v, &[[0.0, 1.0], [1.0, 0.0]]);
        assert_eq!((v["saturationPoints"].clone(), v["saturationCurve"].as_f64()), (Value::Null, Some(0.0)));
    }

    #[test]
    fn the_hue_curve_keeps_its_ends_on_the_box() {
        assert_eq!(read_hue(&json!({})), (vec![[0.0, HUE_TOP], [255.0, HUE_BOTTOM]], false));
        let (pts, drawn) = read_hue(&json!({"huePoints": [[0, 255], [100, -60], [255, 0]]}));
        assert!(drawn && pts[1] == [100.0, 33.75], "{pts:?}");
        let mut v = json!({});
        write_hue(&mut v, &[[0.0, 190.0], [100.0, 33.75], [255.0, 60.0]]);
        assert_eq!(v["huePoints"], json!([[0.0, 255.0], [100.0, -60.0], [255.0, 0.0]]));
        write_hue(&mut v, &[[0.0, HUE_TOP], [255.0, HUE_BOTTOM]]);
        assert_eq!(v["huePoints"], Value::Null);
    }

    #[test]
    fn new_points_go_into_the_nearest_stretch() {
        let to_scr = |q: [f32; 2]| pos2(q[0] * 100.0, (1.0 - q[1]) * 100.0);
        let pts = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
        assert_eq!(insert_at(&pts, to_scr, pos2(50.0, 5.0)), 1);
        assert_eq!(insert_at(&pts, to_scr, pos2(95.0, 60.0)), 2);
    }
}
