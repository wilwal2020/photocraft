use super::{PhotocraftApp, Tokens, label, vec2, widgets};

// ----------------------------------------------------------------------------- status bar

/// The status bar's height and the height of the controls in it (zoom field, Fit), which leaves
/// them an even margin above and below inside the bar (#2823).
/// The status bar's height and its controls' height: Photoshop's thin bar in the Pro themes.
pub fn status_bar_heights(t: &Tokens) -> (f32, f32) {
    if t.pro { (22.0, 16.0) } else { (30.0, 22.0) }
}

/// The status bar's free space past its controls: the horizontal scrollbar goes there
/// (`scrollbars::show_outside`), as in Photoshop. A few points in from each side and from the top
/// and bottom.
fn status_track(ui: &mut egui::Ui) {
    let r = ui.available_rect_before_wrap();
    let track = egui::Rect::from_min_max(egui::pos2(r.left() + 6.0, r.top() + 2.0), egui::pos2(r.right() - 2.0, r.bottom() - 2.0));
    if track.width() > 24.0 && track.height() > 4.0 {
        crate::scrollbars::set_status_track(ui.ctx(), track);
    }
}

/// The status bar, at the bottom of the document area (as Photoshop's document window has it, so
/// the docks reach the bottom of the window): the zoom, the document info and the horizontal
/// scrollbar in the space after them.
pub fn status_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::bottom("status_bar")
        .exact_size(status_bar_heights(&t).0)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            let control_h = status_bar_heights(&t).1;
            // Read from the right in a right-to-left UI, with the trailing group at the left.
            crate::direction::horizontal_centered(ui, |ui| {
                if t.pro {
                    crate::chrome_ui::status_bar_pro(app, ui);
                    // Background job progress with Cancel, at the far end (#210), and the
                    // horizontal scrollbar in between.
                    ui.with_layout(crate::direction::trailing(), |ui| {
                        crate::jobs_ui::status_progress(app, ui);
                        crate::background_redraw::status(app, ui);
                        crate::chrome_ui::render_path_status(app, ui);
                        status_track(ui);
                    });
                    return;
                }
                if let (Some(st), Some(i)) = (app.session.active(), app.session.active_index()) {
                    let (w, h, mode, bits, layers) =
                        (st.doc.size.width, st.doc.size.height, crate::canvas::mode_label(&st.doc), st.doc.depth.bits(), st.doc.layer_count());
                    let mut pct = app.ui.views[i].zoom * 100.0;
                    if widgets::compact_wrapping_value_field(ui, &mut pct, crate::zoom_levels::percent_range(&app.ui.views[i]), "%", vec2(78.0, control_h))
                        .accessible_name(tl!("Zoom"))
                        .changed()
                    {
                        app.ui.views[i].zoom = crate::zoom_levels::clamp(pct / 100.0, app.ui.views[i].doc_size);
                        app.ui.views[i].zoom_animating = false;
                        app.ui.views[i].zoom_anchor = None;
                        app.ui.views[i].fit_pending = false;
                    }
                    widgets::vline(ui, 16.0);
                    label(ui, &crate::i18n::fmt(tl!("{mode} Color · {bits} bit"), &[("mode", mode), ("bits", &bits.to_string())]));
                    widgets::vline(ui, 16.0);
                    label(ui, &format!("{w} × {h} px"));
                    widgets::vline(ui, 16.0);
                    label(ui, &crate::i18n::trn(crate::i18n::current(), layers as u64, "{n} layer", "{n} layers"));
                } else {
                    label(ui, tl!("No document"));
                }
                ui.with_layout(crate::direction::trailing(), |ui| {
                    if app.session.active().is_some()
                        && !t.pro
                        && widgets::compact_button(ui, tl!("Fit"), control_h).clicked()
                        && let Some(i) = app.session.active_index()
                    {
                        app.ui.views[i].fit_pending = true;
                    }
                    if app.session.has_jobs() {
                        ui.add_space(6.0);
                        crate::jobs_ui::status_progress(app, ui);
                    }
                    crate::background_redraw::status(app, ui);
                    crate::chrome_ui::render_path_status(app, ui);
                });
            });
        });
}
