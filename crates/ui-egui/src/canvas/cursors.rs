use super::tool_input::ToolEvent;
use super::view_xform::ViewXform;
use super::{Color32, PhotocraftApp, Pos2, Rect, Stroke, Tool, json, pos2, vec2};

/// Windows' crosshair cursor inverts the pixels under it, so over mid-grey (the pasteboard, many
/// photos) it vanishes (#737), and the Linux themes' and macOS's differ. Use the same black-and-white
/// bitmap on every desktop platform, carried by the OS so its movement does not wait for the canvas
/// to render (#1343).
pub(super) fn visible_crosshair(icon: egui::CursorIcon, painter: &egui::Painter, p: Pos2, draw: bool) -> egui::CursorIcon {
    if !draw || icon != egui::CursorIcon::Crosshair {
        return icon;
    }
    crate::tool_cursor::crosshair(painter, p, 8.0, 2.0)
}

/// The Pencil's cursor at `doc` (document pixels): the whole-pixel square its dab fills
/// (`paint::grid_square`), in screen points with its edges on physical pixels (`ppp` = pixels
/// per point), so it lines up with the pixel grid at any zoom.
pub(crate) fn pencil_cursor_rect(xf: &ViewXform, doc: [f64; 2], size: f32, ppp: f32) -> Rect {
    let [x0, y0, x1, y1] = photocraft_engine::paint::grid_square(doc[0], doc[1], size);
    let r = Rect::from_two_pos(xf.to_screen(x0 as f32, y0 as f32), xf.to_screen(x1 as f32, y1 as f32));
    let ppp = if ppp.is_finite() && ppp > 0.0 { ppp } else { 1.0 };
    let snap = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(pos2(snap(r.min.x), snap(r.min.y)), pos2(snap(r.max.x), snap(r.max.y)))
}

/// Whether an aliased (Pencil) dab of `size` pixels is round rather than its whole
/// [`pencil_cursor_rect`] square. The dab fills the pixels whose centres lie within its radius of
/// the grid centre (`paint::grid_center`), so up to 3 px the corner pixels are in and the dab is
/// a square; from 4 px on they fall outside and it is a disc (#2662).
pub(crate) fn pencil_tip_is_round(size: f32) -> bool {
    let n = if size.is_finite() { size.round().clamp(1.0, 100_000.0) } else { 1.0 };
    let h = n / 2.0;
    (h - 0.5) * std::f32::consts::SQRT_2 > h
}

/// Whether a brush-tip circle draws its centre mark.
/// Clone Stamp and Healing Brush stay an empty circle until Option is held, the source-point
/// cursor. Other brushes follow Preferences › Cursors › Show Crosshair in Brush Tip, as in
/// Photoshop; the Background Eraser always shows its sampling hotspot.
pub(super) fn brush_tip_centre(tool: Tool, option: bool, show_crosshair: bool) -> bool {
    if tool == Tool::QuickSelection {
        return false;
    }
    if matches!(tool, Tool::CloneStamp | Tool::Healing) {
        return option || show_crosshair;
    }
    show_crosshair || tool == Tool::BackgroundEraser
}

/// The display colour of an RGB triple (0..1 per channel) as an egui colour.
fn display_color(c: [f32; 3]) -> Color32 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(b(c[0]), b(c[1]), b(c[2]))
}

/// The Eyedropper ring's two colours at document point (x, y): the colour under the pointer
/// (what a click would pick) and the current colour, the foreground from before the sampling began
/// (`ring_before`; the foreground itself when none was kept), or `None` where there is no colour to
/// sample (#213).
pub(crate) fn eyedropper_ring_colors(app: &mut PhotocraftApp, x: f64, y: f64) -> Option<(Color32, Color32)> {
    let new = eyedropper_color(app, x, y)?;
    let fg = app.session.tools.foreground;
    let current = app.ring_before.unwrap_or([fg[0], fg[1], fg[2]]);
    Some((display_color(new), display_color(current)))
}

/// Keep the colour a sampling press is about to change (the background with ⌥ on the Eyedropper,
/// else the foreground): the sampling ring's lower half for the rest of the press.
pub(crate) fn keep_ring_before(app: &mut PhotocraftApp, background: bool) {
    let c = if background { app.session.tools.background } else { app.session.tools.foreground };
    app.ring_before = Some([c[0], c[1], c[2]]);
}

/// Outer radius and width (points) of the sampling ring's colour band.
const RING_RADIUS: f32 = 40.0;
const RING_BAND: f32 = 10.0;

/// Draw Photoshop's sampling ring at `p`: the sampled colour on the upper half, the colour from
/// before the sampling began on the lower half, edged in grey inside and out so it reads on any
/// image, with a crosshair on the sampled point (#213). Like Photoshop it shows only while the
/// mouse button is held (`held`): hovering shows the plain cursor and samples nothing. `None` when
/// not held, there is nothing to sample, or the Precise-cursor preference wants the plain
/// crosshair.
pub(super) fn eyedropper_ring(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, p: Pos2, held: bool) -> Option<egui::CursorIcon> {
    // The options bar's Show Sampling Ring turns it off (#1649).
    if !held || !app.ui.tool_options.eyedropper_ring || app.session.prefs().cursors.other == photocraft_engine::prefs::OtherCursor::Precise {
        return None;
    }
    let [x, y] = xf.to_doc(p);
    let (new, current) = eyedropper_ring_colors(app, x, y)?;
    paint_sampling_ring(painter, p, new, current);
    Some(egui::CursorIcon::None)
}

/// The sampling ring's shapes at `p`: `new` on the upper half, `current` on the lower.
fn paint_sampling_ring(painter: &egui::Painter, p: Pos2, new: Color32, current: Color32) {
    use std::f32::consts::{PI, TAU};
    let mid = RING_RADIUS - RING_BAND / 2.0;
    // Screen y grows down, so PI..TAU is the upper half and 0..PI the lower. Each half overlaps
    // the other a little so no seam shows at 3 and 9 o'clock.
    let arc = |a0: f32, a1: f32| -> Vec<Pos2> {
        (0..=48)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / 48.0;
                p + vec2(a.cos(), a.sin()) * mid
            })
            .collect()
    };
    painter.add(egui::Shape::line(arc(-0.01, PI + 0.01), Stroke::new(RING_BAND, current)));
    painter.add(egui::Shape::line(arc(PI, TAU), Stroke::new(RING_BAND, new)));
    // Each edge is a thin dark and a thin light hairline, one physical pixel each, so the ring
    // reads over any colour, also a flat area of the sampled colour itself (#6973).
    let px = 1.0 / painter.ctx().pixels_per_point().max(1.0);
    let (dark, light) = (Stroke::new(px, Color32::from_black_alpha(170)), Stroke::new(px, Color32::from_white_alpha(170)));
    painter.circle_stroke(p, RING_RADIUS, dark);
    painter.circle_stroke(p, RING_RADIUS + px, light);
    painter.circle_stroke(p, RING_RADIUS - RING_BAND, dark);
    painter.circle_stroke(p, RING_RADIUS - RING_BAND - px, light);
    // The crosshair on the sampled pixel: dark with a light halo, readable on any colour.
    let arm = 5.0;
    for (stroke, w) in [(Color32::from_white_alpha(180), 3.0), (Color32::BLACK, 1.0)] {
        painter.line_segment([p - vec2(arm, 0.0), p + vec2(arm, 0.0)], Stroke::new(w, stroke));
        painter.line_segment([p - vec2(0.0, arm), p + vec2(0.0, arm)], Stroke::new(w, stroke));
    }
}

/// Tools on which holding ⌥ (Alt) switches to the Eyedropper: a click or drag sets the
/// foreground colour, as with the Eyedropper itself (#417).
pub(crate) fn alt_samples(tool: Tool, mods: egui::Modifiers) -> bool {
    // Control+Alt is the brush-resize drag (`brush_resize`), not sampling.
    mods.alt && !mods.ctrl && matches!(tool, Tool::Brush | Tool::Pencil | Tool::Gradient | Tool::PaintBucket | Tool::Fill)
}

/// Preferences › Cursors › Other Cursors asks for the precise crosshair, or Caps Lock swaps the
/// standard cursor for it (and back), as in Photoshop (#4639).
pub(super) fn precise_other_cursor(app: &PhotocraftApp) -> bool {
    (app.session.prefs().cursors.other == photocraft_engine::prefs::OtherCursor::Precise) != app.caps_lock
}

/// A pen tool's standard cursor: its toolbar icon with the nib (the Convert Point tool: the
/// anchor) on `p`. Draws it at `p` and returns the OS cursor to set (hidden).
pub(crate) fn pen_cursor(ctx: &egui::Context, tool: Tool, p: Pos2) -> egui::CursorIcon {
    // Where each icon's point is in its 24-unit box: the full-size pen's nib at (2.3, 2.3); the
    // Freeform and Curvature pens draw it at 0.68 × from (7, 7); Convert Point's apex anchor.
    let hot = match tool {
        Tool::FreeformPen | Tool::CurvaturePen => vec2(8.56, 8.56),
        Tool::ConvertPoint => vec2(12.0, 7.0),
        _ => vec2(2.3, 2.3),
    };
    crate::tool_cursor::icon(ctx, crate::icons::tool_icon(tool), p, hot / 24.0, 22.0)
}

/// The icon a tool's pointer shows (Preferences › Cursors › Other Cursors: Standard), and where
/// in its 24-unit box the point that acts is.
pub(crate) fn standard_icon_cursor(tool: Tool) -> Option<crate::tool_cursor::icon::Part> {
    // `hot`: the icon's point, in its 24-unit box.
    let at = |name, hot: egui::Vec2| crate::tool_cursor::icon::Part::at_hotspot(name, hot / 24.0, 22.0);
    Some(match tool {
        // The end of the rope.
        Tool::Lasso => at("lasso", vec2(7.0, 22.0)),
        // The end of the rope, as the Lasso's (the toolbar's pentagon has no point to aim with).
        Tool::PolygonLasso => at("lasso-polygon", vec2(7.0, 22.0)),
        // The wand's tip, flipped to point up-left as Photoshop's (the point is of the flipped icon).
        Tool::MagicWand => at("wand-sparkles", vec2(2.7, 2.7)).mirrored(),
        // Where the crop marks cross.
        Tool::Crop => at("crop", vec2(6.0, 6.0)),
        Tool::PerspectiveCrop => at("perspective-crop", vec2(8.0, 6.0)),
        // The blade's edge.
        Tool::Slice => at("slice-knife", vec2(4.0, 20.0)),
        Tool::Ruler => at("ruler", vec2(3.5, 4.5)),
        _ => return None,
    })
}

/// The sampling cursor: a pipette whose tip is the sampled pixel. Draws it at `p` and returns the
/// OS cursor to set (hidden).
pub(crate) fn pipette_cursor(ctx: &egui::Context, p: Pos2) -> egui::CursorIcon {
    // The tip of the icon's pipette is at (2, 22) of its 24-unit box.
    crate::tool_cursor::icon(ctx, "pipette", p, vec2(2.0, 22.0) / 24.0, 20.0)
}

/// The Paint Bucket's cursor: a bucket pouring to the left, as Photoshop's, whose drip is the
/// filled pixel. Draws it at `p` and returns the OS cursor to set (hidden).
pub(crate) fn paint_bucket_cursor(ctx: &egui::Context, p: Pos2) -> egui::CursorIcon {
    // The bottom of the icon's drip is at (20, 22) of its 24-unit box: (4, 22) flipped.
    crate::tool_cursor::icon_part(ctx, crate::tool_cursor::icon::Part::at_hotspot("paint-bucket", vec2(4.0, 22.0) / 24.0, 22.0).mirrored(), p)
}

/// Decided when the press starts, so ⌥ pressed or released mid-stroke never switches between
/// painting and sampling.
pub(super) fn alt_eyedropper(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers) -> bool {
    if matches!(ev, ToolEvent::Down { .. }) {
        let physical_right = app.services.right_button_down.as_ref().is_some_and(|f| f());
        app.alt_sampling = !physical_right && !app.secondary_erase && alt_samples(app.active_tool(), mods);
    }
    if !app.alt_sampling {
        return false;
    }
    match ev {
        // Without ⌥: the sample sets the foreground colour, whatever the Eyedropper would do.
        ToolEvent::Down { x, y, .. } => {
            keep_ring_before(app, false);
            sample_eyedropper(app, x, y, egui::Modifiers::NONE, false);
        }
        ToolEvent::Move { x, y, .. } => sample_eyedropper(app, x, y, egui::Modifiers::NONE, true),
        ToolEvent::Up { .. } => app.alt_sampling = false,
    }
    true
}

/// Is a held sampling drag at screen point `p` off the image, so the screen is read there
/// ([`sample_eyedropper`])? Then the cursor is the system's crosshair: a pipette PhotoCraft paints
/// itself is in the window's pixels, and the screen read would pick the pipette's own outline over
/// the pasteboard and panels (outside the window the system cursor never is).
pub(super) fn samples_screen(app: &PhotocraftApp, xf: &ViewXform, p: Pos2, held: bool) -> bool {
    if !held || !cfg!(target_os = "windows") {
        return false;
    }
    let [x, y] = xf.to_doc(p);
    app.session.active().is_some_and(|st| !(x >= 0.0 && y >= 0.0 && x < f64::from(st.doc.size.width) && y < f64::from(st.doc.size.height)))
}

/// Sample at document point (x, y); `dragging` (a press that started on the canvas, moving): off
/// the image, onto the pasteboard, a panel or another application, the screen's colour under the
/// pointer is picked, as in Photoshop. On a mask or channel the pick is its grey.
pub(crate) fn sample_eyedropper(app: &mut PhotocraftApp, x: f64, y: f64, mods: egui::Modifiers, dragging: bool) {
    let on_image = app.session.active().is_some_and(|st| x >= 0.0 && y >= 0.0 && x < f64::from(st.doc.size.width) && y < f64::from(st.doc.size.height));
    let colour = if on_image || !dragging { eyedropper_color(app, x, y) } else { crate::screen_sample::under_cursor(app.ui.tool_options.eyedropper_size) };
    if let Some(mut rgb) = colour {
        if super::paint_target(app) != json!("pixels") {
            rgb = [photocraft_color::convert::rgb_to_gray(rgb); 3];
        }
        let key = if mods.alt { "background" } else { "foreground" };
        let _ = app.run("tools.setColors", json!({ key: [rgb[0],rgb[1],rgb[2],1.0] }));
    }
}

/// The colour at document point (x, y) with the Eyedropper's Sample Size (the composite pixel,
/// or the average of the square around it) from the layers `sample_layer` names (`document.sampleColor`).
/// `None` off the image or over transparency.
fn sample_color(app: &mut PhotocraftApp, x: f64, y: f64, sample_layer: &str) -> Option<[f32; 3]> {
    if !(x.is_finite() && y.is_finite()) {
        return None;
    }
    let size = app.ui.tool_options.eyedropper_size;
    let v = app.run("document.sampleColor", json!({"x": x, "y": y, "size": size, "sampleLayer": sample_layer})).ok()?;
    match serde_json::from_value::<Vec<f32>>(v).ok()?[..] {
        [r, g, b, a] if a > 0.0 => Some([r, g, b]),
        _ => None,
    }
}

/// What the Eyedropper tool (and a painting tool's ⌥-click) picks at document point (x, y): its
/// options bar's Sample Size and Sample (#1649).
pub(crate) fn eyedropper_color(app: &mut PhotocraftApp, x: f64, y: f64) -> Option<[f32; 3]> {
    let layers = app.ui.tool_options.eyedropper_sample.clone();
    sample_color(app, x, y, &layers)
}

/// The active document's composite colour at document point (x, y), averaged over the
/// Eyedropper's Sample Size as Photoshop's dialog eyedroppers (Curves, Color Picker) do.
/// `None` off the image or over transparency.
pub(crate) fn composite_color(app: &mut PhotocraftApp, x: f64, y: f64) -> Option<[f32; 3]> {
    sample_color(app, x, y, "all")
}

pub(crate) fn alt_loads_mixer(tool: Tool, mods: egui::Modifiers) -> bool {
    mods.alt && !mods.ctrl && tool == Tool::MixerBrush
}
pub(crate) fn alt_picks(tool: Tool, mods: egui::Modifiers) -> bool {
    alt_samples(tool, mods) || alt_loads_mixer(tool, mods)
}
