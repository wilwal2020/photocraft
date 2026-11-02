use super::{PhotocraftApp, Tokens};

// ----------------------------------------------------------------------------- status bar

/// The status bar's height and the height of the controls in it (zoom field), which leaves them
/// an even margin above and below inside the bar (#2823): Photoshop's thin bar, in every theme.
pub fn status_bar_heights() -> (f32, f32) {
    (22.0, 16.0)
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
        .exact_size(status_bar_heights().0)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            // Read from the right in a right-to-left UI, with the trailing group at the left.
            crate::direction::horizontal_centered(ui, |ui| {
                crate::chrome_ui::status_bar_pro(app, ui);
                // Background job progress with Cancel, at the far end (#210), and the
                // horizontal scrollbar in between.
                ui.with_layout(crate::direction::trailing(), |ui| {
                    crate::jobs_ui::status_progress(app, ui);
                    crate::background_redraw::status(app, ui);
                    crate::chrome_ui::render_path_status(app, ui);
                    status_track(ui);
                });
            });
        });
}
