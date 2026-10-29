//! Mouse buttons on the canvas. Tools follow the left button. The right button never paints with
//! the tool: with a painting tool it opens the Brush Preset picker at the pointer (Photoshop), or,
//! with Preferences › Tools › Right-click with painting tools set to Erase, a right drag with the
//! Brush erases with the current brush (Krita, Paint). Each right stroke is one `paint.stroke`
//! with `"erase": true`, so one undo step, with the pen pressure and tilt of a normal stroke.

use egui::{PointerButton, Pos2, Response};
use photocraft_engine::BrushSettings;
use photocraft_engine::prefs::RightClickPaint;

use crate::PhotocraftApp;
use crate::state::Tool;

/// The brush is a tool option, kept per tool like Photoshop's options bar: switching from the
/// Brush to the Eraser saves the session brush for the old tool and loads the new tool's, so each
/// keeps its own size, hardness, mode, opacity, dynamics and smoothing, and the preset it was
/// picked from. A tool seen for the first time takes what the brush already has, at Photoshop's
/// 10 % smoothing. Other tools leave the brush alone. Every tool's brush is remembered across
/// restarts (`Preferences::tool_brushes`, [`tool_brushes_json`]).
///
/// The foreground and background colours, the erase flag and the stroke seed belong to the
/// session rather than to the tool, so a swap never carries them between tools: picking a colour
/// or painting with the Eraser works with any tool's brush.
pub fn sync_tool_brush(app: &mut PhotocraftApp) {
    let tool = app.ui.tool;
    if !has_brush_picker(tool) || app.ui.brush_tool == tool {
        return;
    }
    // The live brush is the outgoing tool's, so save it there before the new one takes over: any
    // edit it made since it was last active is in it, so its copy never goes stale.
    let current = app.session.tools.brush.clone();
    let preset = app.session.tools.current_preset.clone();
    save_brush(app, app.ui.brush_tool, current.clone(), preset);
    let (mut brush, preset) = match app.ui.tool_brushes.iter().find(|(t, _)| *t == tool) {
        Some((_, b)) => (session_fields(&current, b), app.ui.tool_presets.iter().find(|(t, _)| *t == tool).map(|(_, p)| p.clone())),
        // A tool seen for the first time takes what the brush has, at Photoshop's smoothing, but
        // in Normal mode: each tool's Mode starts at Normal, so a Multiply Brush doesn't make the
        // first Clone Stamp stroke multiply (or give Healing a mode its menu doesn't have).
        None => (BrushSettings { smoothing: first_smoothing(), mode: photocraft_color::BlendMode::Normal, ..current }, None),
    };
    // A brush remembered from the last run holds tip references: load the full tips (a missing
    // one keeps painting with its preview).
    let _ = app.session.load_brush_tips(&mut brush);
    app.session.tools.brush = brush;
    // The tool's own preset, never the outgoing tool's, so Update Brush can't overwrite the
    // Brush's preset with the Eraser's brush. One deleted since is forgotten.
    app.session.tools.current_preset = preset.filter(|n| photocraft_paint::presets::find(&app.session.tools.presets, n).is_some());
    app.session.tools.brush_changed();
    app.ui.brush_tool = tool;
    tool_brushes_changed(app);
}

/// Note that a tool's saved brush, or the tool the live brush belongs to, changed: the next
/// pointer-up remembers them (`last_brush::persist`).
pub(crate) fn tool_brushes_changed(app: &mut PhotocraftApp) {
    app.prefs_rt.tool_brushes_rev = app.prefs_rt.tool_brushes_rev.wrapping_add(1);
}

/// The shell's name for `tool` in `Preferences::tool_brushes`.
fn tool_key(tool: Tool) -> String {
    format!("{tool:?}")
}

/// Every tool's brush but the live one (that is `Preferences::last_brush`), for
/// `Preferences::tool_brushes`: `{"active": <the live brush's tool>, "tools": {<tool>: <brush>}}`.
/// A brush with a sampled bitmap too big to embed is left out (it stays the tool's this run).
pub(crate) fn tool_brushes_json(app: &PhotocraftApp) -> serde_json::Value {
    let owner = saved_brush_owner(app);
    let mut tools = serde_json::Map::new();
    for (tool, brush) in &app.ui.tool_brushes {
        if *tool == app.ui.brush_tool || *tool == owner {
            continue;
        }
        let preset = app.ui.tool_presets.iter().find(|(t, _)| t == tool).map(|(_, p)| p.as_str());
        if let Some(v) = photocraft_engine::brush_cmds::remembered(brush, preset) {
            tools.insert(tool_key(*tool), v);
        }
    }
    serde_json::json!({"active": tool_key(owner), "tools": tools})
}

/// The tool whose brush `Preferences::last_brush` holds: the one that owns the live brush once
/// that brush is saved. Until then (a sampled tip too big to embed, before the preset store takes
/// it) `last_brush` still holds the previously saved owner's brush, so that owner stands (#6212);
/// with none saved, the Brush.
pub(crate) fn saved_brush_owner(app: &PhotocraftApp) -> Tool {
    let t = &app.session.tools;
    if t.brush_rev == t.brush_saved_rev {
        return app.ui.brush_tool;
    }
    app.session.prefs().tool_brushes.get("active").and_then(serde_json::Value::as_str).and_then(Tool::from_name).unwrap_or(Tool::Brush)
}

/// Restore `Preferences::tool_brushes` at launch, after `last_brush` (whose brush is `active`'s):
/// the tool owning the live brush and every other tool's own brush and preset. Anything
/// unreadable (an unknown tool, a malformed brush) is skipped; that tool then starts from the
/// live brush as if it was never used.
pub(crate) fn restore_tool_brushes(app: &mut PhotocraftApp) {
    let saved = app.session.prefs().tool_brushes.clone();
    let active = saved.get("active").and_then(serde_json::Value::as_str).and_then(Tool::from_name).filter(|t| has_brush_picker(*t));
    let Some(active) = active else { return };
    app.ui.brush_tool = active;
    app.ui.tool_brushes.clear();
    app.ui.tool_presets.clear();
    for (name, v) in saved.get("tools").and_then(serde_json::Value::as_object).into_iter().flatten() {
        let Some(tool) = Tool::from_name(name).filter(|t| has_brush_picker(*t) && *t != active) else { continue };
        let Some((brush, preset)) = photocraft_engine::brush_cmds::recalled(v) else {
            log::warn!("Preferences: ignoring the remembered brush of the {name} tool");
            continue;
        };
        if app.ui.tool_brushes.iter().all(|(t, _)| *t != tool) {
            save_brush(app, tool, brush, preset);
        }
    }
    // What was just restored is what the preferences hold.
    app.prefs_rt.tool_brushes_saved_rev = app.prefs_rt.tool_brushes_rev;
}

/// The painting tool a preset's [`photocraft_paint::BrushPreset::tool`] names.
fn preset_tool(tool: &str) -> Option<Tool> {
    Some(match tool {
        "brush" => Tool::Brush,
        "pencil" => Tool::Pencil,
        "eraser" => Tool::Eraser,
        "smudge" => Tool::Smudge,
        "mixerBrush" => Tool::MixerBrush,
        _ => return None,
    })
}

/// Before `tools.setBrush` picks a preset (`params.preset`) that belongs to a tool: switch a
/// painting tool to that tool, so the preset becomes its brush (Photoshop: an Eraser preset
/// picked with the Brush tool selects the Eraser, and the Brush keeps its own brush).
pub fn switch_to_preset_tool(app: &mut PhotocraftApp, params: &serde_json::Value) {
    let Some(name) = params.get("preset").and_then(serde_json::Value::as_str) else { return };
    if !app.ui.tool.is_brushlike() {
        return;
    }
    let tool = photocraft_paint::presets::find(&app.session.tools.presets, name).and_then(|p| p.tool.as_deref()).and_then(preset_tool);
    if let Some(tool) = tool.filter(|t| *t != app.ui.tool) {
        app.ui.tool = tool;
        sync_tool_brush(app);
    }
}

/// The brush `tool` had when it was last active, and the preset it was picked from.
fn save_brush(app: &mut PhotocraftApp, tool: Tool, brush: BrushSettings, preset: Option<String>) {
    match app.ui.tool_brushes.iter_mut().find(|(t, _)| *t == tool) {
        Some(e) => e.1 = brush,
        None => app.ui.tool_brushes.push((tool, brush)),
    }
    app.ui.tool_presets.retain(|(t, _)| *t != tool);
    if let Some(p) = preset {
        app.ui.tool_presets.push((tool, p));
    }
}

/// The session's own brush fields, which a tool swap never carries: the colours, the erase flag
/// and the stroke seed (a stroke's replay seed is the command's, not the tool's).
fn session_fields(current: &BrushSettings, brush: &BrushSettings) -> BrushSettings {
    BrushSettings { color: current.color, background: current.background, erase: current.erase, seed: current.seed, ..brush.clone() }
}

/// A tool's brush as it starts out (Reset Tool, #2825): the default brush at Photoshop's
/// smoothing, with the session's own fields of `current`.
pub(crate) fn fresh_brush(current: &BrushSettings) -> BrushSettings {
    session_fields(current, &BrushSettings { smoothing: first_smoothing(), ..BrushSettings::default() })
}

/// Photoshop's 10 % stroke smoothing, what a tool's brush starts at.
fn first_smoothing() -> photocraft_engine::paint::brush::Smoothing {
    photocraft_engine::paint::brush::Smoothing { amount: 0.1, ..Default::default() }
}

/// Does a stroke with `tool` starting now paint in Photoshop's Clear mode, erasing with its own
/// brush (#3439)? Clear chosen in its Mode dropdown, or `~` held (`hold_keys::erase_held`).
pub fn clears(app: &PhotocraftApp, tool: Tool) -> bool {
    let chosen = match tool {
        Tool::Brush => app.ui.tool_options.brush_clear,
        Tool::Pencil => app.ui.tool_options.pencil_clear,
        _ => false,
    };
    crate::hold_keys::erases_while_held(tool) && (chosen || app.held_erase)
}

/// Which tool events this frame's canvas response produces.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Buttons {
    pub started: bool,
    pub dragged: bool,
    pub stopped: bool,
    pub clicked: bool,
    /// A right-click on the canvas ([`right_click`]): where it was pressed.
    pub right_click: Option<Pos2>,
}

/// Tools with the options-bar brush chip, which opens the Brush Preset picker.
pub fn has_brush_picker(tool: Tool) -> bool {
    tool.is_brushlike() || tool == Tool::QuickSelection
}

/// Tools whose canvas right-click opens the Brush Preset picker: the painting tools. The Quick
/// Selection tool has the picker too, but its right-click opens the selection menu (#7268).
pub fn right_click_picker(tool: Tool) -> bool {
    has_brush_picker(tool) && !crate::canvas_tool_menu::applies(tool)
}

/// Does a right-button drag with `tool` erase (rather than open the picker)?
pub fn right_erases(app: &PhotocraftApp, tool: Tool) -> bool {
    matches!(tool, Tool::Brush | Tool::Eraser) && app.session.prefs().tools.right_click_with_painting_tools == RightClickPaint::Erase
}

/// Route the canvas response's buttons: the left one drives the tool; the right one resizes the
/// brush with Alt held (#297), erases (Erase preference) or opens the Brush Preset picker. Arms
/// `secondary_erase` or `brush_resize_armed` for this frame's `Down`.
pub fn canvas_buttons(app: &mut PhotocraftApp, response: &Response, tool: Tool) -> Buttons {
    let physical_right = app.services.right_button_down.as_ref().is_some_and(|f| f());
    let (mods, mut right_down) = response.ctx.input(|i| (i.modifiers, i.pointer.secondary_down()));
    right_down |= physical_right;
    let right_started = response.drag_started_by(PointerButton::Secondary) || (physical_right && response.drag_started());
    let right_dragged = response.dragged_by(PointerButton::Secondary) || (physical_right && response.dragged());
    let resizing = app.brush_resize.is_some_and(|r| r.secondary);
    let right_stopped = response.drag_stopped_by(PointerButton::Secondary) || (resizing && response.drag_stopped());

    if physical_right && crate::brush_resize::is_right_gesture(crate::workspace_ui::sticky_mods(app, mods)) {
        app.alt_sampling = false;
    }

    // Alt+right-drag resizes the brush (brush_resize.rs); its events reach `tool_event` like a
    // left drag's, and nothing paints. A resize whose release was missed ends here.
    crate::brush_resize::release_stale(app, right_down || response.drag_stopped_by(PointerButton::Secondary) || response.drag_stopped());
    let resize_start = crate::brush_resize::applies(tool)
        && app.drag.is_none()
        && crate::brush_resize::is_right_gesture(crate::workspace_ui::sticky_mods(app, mods))
        && (right_started || (physical_right && response.dragged() && app.brush_resize.is_none()));
    let resizing = app.brush_resize.is_some_and(|r| r.secondary);
    app.brush_resize_armed = resize_start || (physical_right && resizing);
    // ⌘/Ctrl+right-click lists the layers under the pointer instead (layer_pick_ui.rs, #307).
    let layer_menu = crate::layer_pick_ui::is_gesture(tool, mods);
    let erase = right_erases(app, tool) && !resize_start && !resizing && !layer_menu;
    let right_stroke = erase && app.drag.is_some();
    let right_start = erase && right_started;
    let menu_click = right_click(app, response, tool, mods);
    if !erase
        && !layer_menu
        && right_click_picker(tool)
        && let Some(p) = menu_click
    {
        crate::brush_picker::open(&mut app.ui, &response.ctx, [p.x, p.y]);
    }
    let erase_click = erase && response.secondary_clicked();
    app.secondary_erase = right_start || erase_click;
    let right_drag = right_stroke || resizing;
    Buttons {
        started: response.drag_started_by(PointerButton::Primary) || right_start || resize_start,
        dragged: response.dragged_by(PointerButton::Primary) || (right_drag && right_dragged),
        stopped: response.drag_stopped_by(PointerButton::Primary) || (right_drag && right_stopped),
        clicked: response.clicked() || erase_click,
        right_click: menu_click,
    }
}

/// A right-click on the canvas `response`, for its menus: the right button pressed on it and let
/// go on it, however far the pointer drifted or however long it was held in between. egui's click
/// allows only a few points of movement and under a second, which a pen's jitter or a slow click
/// breaks (user report: the canvas menu sometimes didn't open). Not a press that starts something
/// of its own: an Alt+right-drag brush resize or an erasing right stroke. Where it was pressed.
/// Call once a frame: the release takes the press.
fn right_click(app: &PhotocraftApp, response: &Response, tool: Tool, mods: egui::Modifiers) -> Option<Pos2> {
    let ctx = &response.ctx;
    let key = response.id.with("right-press");
    let (pressed, released, at) =
        ctx.input(|i| (i.pointer.button_pressed(PointerButton::Secondary), i.pointer.button_released(PointerButton::Secondary), i.pointer.interact_pos()));
    let resize = crate::brush_resize::applies(tool) && crate::brush_resize::is_right_gesture(crate::workspace_ui::sticky_mods(app, mods));
    let own = resize || right_erases(app, tool);
    if pressed {
        let on = at.filter(|p| !own && response.hovered() && response.rect.contains(*p));
        ctx.data_mut(|d| {
            if let Some(p) = on {
                d.insert_temp(key, p);
            } else {
                d.remove_temp::<Pos2>(key);
            }
        });
    }
    if response.secondary_clicked() && !own {
        ctx.data_mut(|d| d.remove_temp::<Pos2>(key));
        return response.interact_pointer_pos().or(at);
    }
    if !released {
        return None;
    }
    let start = ctx.data_mut(|d| d.remove_temp::<Pos2>(key))?;
    at.filter(|p| response.rect.contains(*p)).map(|_| start)
}

/// `ui.pointer` with `"button": "secondary"`: true when its events should reach the tool (an
/// Alt+right-drag brush resize, `brush_resize_armed` for its `Down`; or an erasing right stroke,
/// `secondary_erase` armed for its `Down`). Otherwise a right-click with a painting tool opens
/// the Brush Preset picker at screen point `at`, and nothing paints.
pub fn pointer_secondary(app: &mut PhotocraftApp, ctx: &egui::Context, down: bool, mods: egui::Modifiers, at: [f32; 2]) -> bool {
    let tool = app.ui.tool;
    if crate::brush_resize::applies(tool) && (crate::brush_resize::is_right_gesture(mods) || app.brush_resize.is_some_and(|r| r.secondary)) {
        app.brush_resize_armed = down && app.drag.is_none();
        return true;
    }
    if right_erases(app, tool) {
        app.secondary_erase = down;
        return true;
    }
    if down && right_click_picker(tool) {
        crate::brush_picker::open(&mut app.ui, ctx, at);
    }
    false
}

/// The Brush Preset picker a right-click or the options-bar brush chip opened, at the pointer. It
/// edits the session brush like the Brushes panel; a press outside, Escape, Enter or a
/// double-click on a preset closes it.
pub fn show_picker(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some([x, y]) = app.ui.brush_picker else { return };
    if !has_brush_picker(app.ui.tool) {
        app.ui.brush_picker = None;
        return;
    }
    // Escape and Enter close it once this frame's edits are in (a typed size applies), unless they
    // end a rename in its rename bar.
    let key_close = app.ui.brush_picker_list.renaming.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Enter));
    let screen = ctx.content_rect();
    // The picker's content size: the one its grip was dragged to, or the one it last closed with
    // (kept in egui's own memory, which outlives the app's), or the default. The frame's chrome
    // sits around it, and together they keep the whole picker on screen.
    let id = crate::brush_picker::picker_id();
    let size_id = id.with("content-size");
    let content = app.ui.brush_picker_size.or_else(|| ctx.data_mut(|d| d.get_persisted::<[f32; 2]>(size_id))).map_or_else(
        || egui::vec2(crate::brush_picker::DEFAULT_SIZE[0], crate::brush_picker::DEFAULT_SIZE[1]),
        |s| crate::brush_picker::clamp_size(s, screen.size()),
    );
    // Remember it for the picker's next open, and — egui's memory being saved — the next run.
    ctx.data_mut(|d| d.insert_persisted(size_id, [content.x, content.y]));
    let style = ctx.global_style();
    let size = content + style.spacing.menu_margin.sum() + egui::Vec2::splat(2.0 * style.visuals.window_stroke().width);
    let pos = egui::pos2(x.min(screen.right() - size.x).max(screen.left()), y.min(screen.bottom() - size.y).max(screen.top()));
    let area = egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        let before = app.session.tools.brush.clone();
        let mut b = before.clone();
        let list = &mut app.ui.brush_picker_list;
        let repeat = &mut app.ui.tool_options.brush_key_repeat;
        let frame = egui::Frame::popup(ui.style()).show(ui, |ui| {
            crate::brush_picker::body(ui, &mut b, &app.session.tools.presets, app.session.tools.current_preset.as_deref(), list, content, repeat)
        });
        let picks = frame.inner;
        // The corner grip resizes the picker, and the size is remembered across opens.
        if let Some(new) = crate::brush_picker::resize_grip(ui, frame.response.rect, content) {
            app.ui.brush_picker_size = Some([new.x, new.y]);
        }
        crate::brush_panel::commit_gesture(app, ui.ctx(), &before, &b);
        let closes = picks.iter().any(crate::brush_picker::Pick::closes);
        crate::brush_picker::apply(app, ui.ctx(), picks);
        closes
    });
    // Not a press on a menu the picker opened (the gear, a preset's context menu), nor on the
    // options-bar chip, whose click toggles the picker. A menu floats over the picker in the
    // foreground order, so the press is told apart by the layer it lands on, not by any popup
    // just being open: a press elsewhere closes the picker in the same click that closes the menu.
    let press = ctx.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten());
    let on_menu = press.is_some_and(|p| ctx.layer_id_at(p).is_some_and(|l| l.order == egui::Order::Foreground));
    let outside = crate::canvas_popup::outside_pressed(ctx, id, area.response.rect) && press.is_some_and(|p| !crate::brush_picker::on_chip(ctx, p)) && !on_menu;
    if outside || key_close || area.inner {
        crate::brush_picker::close(&mut app.ui);
    }
}

#[cfg(test)]
mod tests;
