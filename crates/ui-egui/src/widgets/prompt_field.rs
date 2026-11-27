//! A generative prompt field (user request, 2026-11-25): it grows with its text instead of
//! scrolling the start out of sight, and the ▾ beside it lists the prompts last used
//! (`dialog_memory::recent_prompts`) to pick one again. Edit › Generative Fill's dialog and the
//! Remove tool's options bar share it.

use egui::{Response, Sense, Ui, vec2};

use super::text_field::TEXT_FIELD_H;

/// How the field makes room for a long prompt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grow {
    /// One line that widens with the text, up to `max` points (an options bar has no rows to
    /// spare); past that it scrolls as before.
    Wide { max: f32 },
    /// Wraps at the given width and gains rows as the text does (a dialog).
    Tall,
}

/// Most characters a prompt takes.
pub const PROMPT_LIMIT: usize = 500;
/// A recent prompt's row in the list is cut after this many characters (the full text is its
/// tooltip).
const ROW_CHARS: usize = 70;

/// The width a one-line field needs for `text` (padding and a little room for the caret).
fn wide_width(ui: &Ui, text: &str) -> f32 {
    let font = egui::FontId::proportional(12.5);
    let w = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE).size().x);
    w + 20.0
}

/// `width` grown to fit `text` as `grow` says (for [`Grow::Wide`]; `Tall` keeps `width`).
pub fn grown_width(ui: &Ui, text: &str, width: f32, grow: Grow) -> f32 {
    match grow {
        Grow::Wide { max } => wide_width(ui, text).clamp(width, max.max(width)),
        Grow::Tall => width,
    }
}

/// A prompt field `width` points wide (at least), growing as `grow` says, with the recent prompts
/// `recent` (newest first) under its ▾. Picking one replaces the text and marks the response
/// changed. Control characters (a pasted line break) become spaces: a prompt is one line.
pub fn prompt_field(ui: &mut Ui, id: egui::Id, text: &mut String, hint: &str, width: f32, grow: Grow, recent: &[String]) -> Response {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let mut field = match grow {
            Grow::Wide { .. } => super::text_field_ex(ui, id, text, hint, grown_width(ui, text, width, grow), false, Some(PROMPT_LIMIT)),
            Grow::Tall => {
                let r = ui.add(
                    egui::TextEdit::multiline(text)
                        .id(id)
                        .hint_text(hint.to_owned())
                        .desired_width(width)
                        .desired_rows(1)
                        .char_limit(PROMPT_LIMIT)
                        // Enter is the dialog's OK, not a new line.
                        .return_key(None),
                );
                if text.chars().any(char::is_control) {
                    *text = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
                }
                r
            }
        };
        let (rect, arrow) = ui.allocate_exact_size(vec2(16.0, TEXT_FIELD_H), Sense::click());
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&arrow));
        super::chevron_icon(ui, rect, ui.style().interact(&arrow), open);
        let arrow = arrow.on_hover_text(tl!("Recent prompts"));
        arrow.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tl!("Recent prompts")));
        let picked = super::press_menu::show(&arrow, |ui, menu| {
            ui.set_min_width(160.0);
            if recent.is_empty() {
                ui.add_enabled(false, egui::Label::new(tl!("No recent prompts")));
                return None;
            }
            let mut picked = None;
            for p in recent {
                let row: String = if p.chars().count() > ROW_CHARS { p.chars().take(ROW_CHARS).chain(std::iter::once('…')).collect() } else { p.clone() };
                if menu.item(ui, row) {
                    picked = Some(p.clone());
                }
            }
            picked
        })
        .flatten();
        if let Some(p) = picked {
            *text = p;
            field.mark_changed();
        }
        field
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_prompt_widens_the_one_line_field_up_to_its_max() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let g = Grow::Wide { max: 400.0 };
            assert_eq!(grown_width(ui, "", 180.0, g), 180.0);
            let mid = grown_width(ui, &"word ".repeat(9), 180.0, g);
            assert!(mid > 180.0 && mid < 400.0, "{mid}");
            assert_eq!(grown_width(ui, &"word ".repeat(80), 180.0, g), 400.0);
            assert_eq!(grown_width(ui, &"word ".repeat(80), 180.0, Grow::Tall), 180.0);
        });
        out.textures_delta.clear();
    }

    /// ▾ lists the recent prompts; choosing one fills the field (user request, 2026-11-25).
    #[test]
    fn choosing_a_recent_prompt_fills_the_field() {
        use egui_kittest::kittest::Queryable;
        for grow in [Grow::Wide { max: 400.0 }, Grow::Tall] {
            let recent = vec!["a red door".to_string(), "a stone wall".to_string()];
            let mut h = egui_kittest::Harness::builder().with_size(vec2(600.0, 300.0)).build_ui_state(
                move |ui, s: &mut (String, bool)| {
                    let r = prompt_field(ui, egui::Id::unique("prompt-field-test"), &mut s.0, "", 180.0, grow, &recent).accessible_name("Prompt");
                    s.1 |= r.changed();
                },
                (String::new(), false),
            );
            h.run();
            h.get_by_label("Recent prompts").click();
            h.run();
            h.get_by_label("a stone wall").click();
            h.run();
            assert_eq!(h.state().0, "a stone wall", "{grow:?}");
            assert!(h.state().1, "the field reports the change");
        }
    }
}
