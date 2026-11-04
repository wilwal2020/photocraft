//! Preferences › Interface › Custom Theme: the colours apply live from the dialog's draft, Cancel
//! goes back, OK saves them, and they come back after a restart.

use super::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::sync::{Arc, Mutex};

const PINK: Color32 = Color32::from_rgb(0xc0, 0x30, 0x70);

/// An app whose preferences live in `store`.
fn app_on(store: &Arc<Mutex<Option<String>>>) -> PhotocraftApp {
    let (a, b) = (store.clone(), store.clone());
    let services = crate::Services {
        load_prefs: Some(Box::new(move || a.lock().unwrap().clone())),
        save_prefs: Some(Box::new(move |s: &str| {
            *b.lock().unwrap() = Some(s.to_string());
            Ok(())
        })),
        ..Default::default()
    };
    PhotocraftApp::new(photocraft_engine::Session::new(), services)
}

fn harness(app: PhotocraftApp) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1280.0, 900.0)).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, ThemeKind::default());
        app
    });
    h.run_steps(4);
    h
}

/// Open Preferences on Interface with the search narrowed to the Custom Theme group, so it is
/// at the top of the page.
fn open_custom_theme(h: &mut Harness<'static, PhotocraftApp>) -> u64 {
    let id = open_preferences(h.state_mut(), "interface");
    h.state_mut().ui.dialog_mut(id).unwrap().fields.insert("__search".into(), json!("custom theme"));
    h.run_steps(4);
    id
}

fn draft(h: &Harness<'static, PhotocraftApp>, id: u64) -> Value {
    h.state().ui.dialogs.iter().find(|d| d.id == id).unwrap().fields["values"]["interface"].clone()
}

#[test]
fn custom_tokens_replace_only_the_listed_colours() {
    let colors = [("card", "#102030"), ("accent_soft", "#8b7cf62e"), ("text", "red"), ("nonsense", "#ffffff"), ("canvas", "#202020")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let t = Tokens::custom(ThemeKind::ProMedium, &colors);
    let base = Tokens::for_kind(ThemeKind::ProMedium);
    assert_eq!(t.kind, ThemeKind::ProMedium, "layout and appearance family stay the base theme's");
    assert_eq!(t.card, Color32::from_rgb(0x10, 0x20, 0x30));
    assert_eq!(t.accent_soft, Color32::from_rgba_unmultiplied(0x8b, 0x7c, 0xf6, 0x2e));
    assert_eq!(t.text, base.text, "an unreadable colour is skipped");
    assert_eq!((t.field, t.radius, t.pro), (base.field, base.radius, base.pro));
    // ProMedium draws no dots (dots the canvas colour): they follow a new canvas colour.
    assert_eq!((t.canvas, t.canvas_dot), (Color32::from_gray(0x20), Color32::from_gray(0x20)));
    // Every editable colour names a token, and the hex form round-trips.
    for (_, colors) in crate::theme::CUSTOM_COLOR_GROUPS {
        for (name, _) in *colors {
            let c = base.color(name).unwrap_or_else(|| panic!("{name} is not a token"));
            assert_eq!(crate::theme::parse_color_hex(&crate::theme::color_hex(c)), Some(c), "{name}");
        }
    }
    assert_eq!(crate::theme::color_hex(Color32::from_rgb(1, 2, 3)), "#010203");
}

#[test]
fn edits_show_live_cancel_reverts_and_ok_saves_across_a_restart() {
    let store: Arc<Mutex<Option<String>>> = Arc::default();
    let mut h = harness(app_on(&store));
    let id = open_custom_theme(&mut h);
    let base = Tokens::for_kind(ThemeKind::ProMedium);
    assert_eq!(Tokens::get(&h.ctx).card, base.card);

    // Typing a panel colour edits the draft, makes Custom the dark theme and shows at once.
    let hex = h.get_by_label("Panel Background hex");
    hex.click();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Panel Background hex").type_text("#c03070");
    h.run_steps(3);
    let d = draft(&h, id);
    assert_eq!(d["customTheme"]["colors"]["card"], "#c03070");
    assert_eq!(d["darkTheme"], "custom");
    assert_eq!(Tokens::get(&h.ctx).card, PINK, "the whole app previews the draft");
    assert_eq!(h.ctx.global_style().visuals.window_fill, PINK, "egui's style follows the tokens");
    assert_eq!(h.state().session.prefs().interface.dark_theme, prefs::DarkTheme::ProMedium, "nothing is saved yet");

    // Cancel goes back to the saved theme.
    h.get_by_label("Cancel").click();
    h.run_steps(4);
    assert_eq!(Tokens::get(&h.ctx), base);
    assert!(h.state().prefs_rt.custom_theme.is_none());

    // Start from Studio, change the accent through the draft, and OK.
    let id = open_custom_theme(&mut h);
    {
        let values = h.state_mut().ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap();
        values["interface"]["customTheme"] = json!({"base": "studio", "colors": {"accent": "#c03070"}});
        values["interface"]["darkTheme"] = json!("custom");
    }
    h.run_steps(3);
    assert_eq!(h.state().ui.theme, ThemeKind::Studio, "Studio's layout");
    assert_eq!(Tokens::get(&h.ctx).accent, PINK);
    assert_eq!(Tokens::get(&h.ctx).card, Tokens::for_kind(ThemeKind::Studio).card);
    h.get_by_label("OK").click();
    h.run_steps(6);
    assert!(h.state().ui.dialogs.is_empty());
    assert_eq!(Tokens::get(&h.ctx).accent, PINK, "OK keeps it");
    let saved: Value = serde_json::from_str(store.lock().unwrap().as_deref().unwrap()).unwrap();
    assert_eq!(saved["interface"]["darkTheme"], "custom");
    assert_eq!(saved["interface"]["customTheme"]["base"], "studio");
    assert_eq!(saved["interface"]["customTheme"]["colors"]["accent"], "#c03070");

    // A restart reads it back.
    let restarted = harness(app_on(&store));
    assert_eq!(restarted.state().ui.theme, ThemeKind::Studio);
    assert_eq!(Tokens::get(&restarted.ctx).accent, PINK);
    assert!(restarted.state().prefs_rt.custom_theme.is_some());
}

#[test]
fn start_from_and_reset_go_back_to_the_preset() {
    let store: Arc<Mutex<Option<String>>> = Arc::default();
    let mut h = harness(app_on(&store));
    let id = open_custom_theme(&mut h);
    {
        let values = h.state_mut().ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap();
        values["interface"]["customTheme"] = json!({"base": "proMedium", "colors": {"card": "#c03070", "text": "#ffffff"}});
    }
    h.run_steps(3);
    assert!(!h.get_by_label("Reset to Preset").accesskit_node().is_disabled());
    // One colour's own reset button.
    h.get_by_label("Reset Panel Background").click();
    h.run_steps(3);
    assert_eq!(draft(&h, id)["customTheme"]["colors"], json!({"text": "#ffffff"}));
    h.get_by_label("Reset to Preset").click();
    h.run_steps(3);
    assert_eq!(draft(&h, id)["customTheme"]["colors"], json!({}));
    assert!(h.get_by_label("Reset to Preset").accesskit_node().is_disabled());
    assert!(h.query_by_label("Reset Panel Background").is_none());

    // Start From picks another preset (and drops edits made on the old one).
    {
        let values = h.state_mut().ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap();
        values["interface"]["customTheme"]["colors"] = json!({"card": "#c03070"});
    }
    h.run_steps(2);
    h.get_by_role_and_label(Role::ComboBox, "Start From").click();
    h.run_steps(3);
    h.get_by_label("Solarized Dark").click();
    h.run_steps(4);
    let d = draft(&h, id);
    assert_eq!(d["customTheme"], json!({"base": "solarizedDark", "colors": {}, "unlinked": []}));
    assert_eq!(d["darkTheme"], "custom");
    assert_eq!(Tokens::get(&h.ctx), Tokens::for_kind(ThemeKind::SolarizedDark));
    // The Custom swatch is offered in both theme rows, without a favourite star.
    h.state_mut().ui.dialog_mut(id).unwrap().fields.insert("__search".into(), json!(""));
    h.run_steps(3);
    assert_eq!(h.query_all_by_label("Custom").filter(|n| n.accesskit_node().role() == Role::RadioButton).count(), 2);
    assert!(h.query_by_label("Add to favorites: Custom").is_none());
}

/// Rendered: the dialog's preview and the dialog itself are drawn in the edited colours.
#[test]
fn the_preview_and_the_dialog_render_in_the_custom_colours() {
    let store: Arc<Mutex<Option<String>>> = Arc::default();
    let mut h = harness(app_on(&store));
    let id = open_custom_theme(&mut h);
    {
        let values = h.state_mut().ui.dialog_mut(id).unwrap().fields.get_mut("values").unwrap();
        values["interface"]["customTheme"] = json!({"base": "proMedium", "colors": {"dock": "#c03070", "card": "#204060"}});
        values["interface"]["darkTheme"] = json!("custom");
    }
    h.run_steps(4);
    let preview = h.get_by_label("Custom theme preview").rect();
    let image = h.render().unwrap();
    if let Ok(dir) = std::env::var("PHOTOCRAFT_SHOTS") {
        image.save(format!("{dir}/custom_theme.png")).unwrap();
    }
    let ppp = h.ctx.pixels_per_point();
    let at = |p: egui::Pos2| image.get_pixel((p.x * ppp) as u32, (p.y * ppp) as u32).0;
    let near = |px: [u8; 4], c: Color32| px[..3].iter().zip([c.r(), c.g(), c.b()]).all(|(a, b)| a.abs_diff(b) <= 3);
    // Between the preview's canvas and panel, below both: the dock colour.
    let dock = at(egui::pos2(preview.center().x, preview.bottom() - 3.0));
    assert!(near(dock, PINK), "preview dock {dock:?}");
    // The dialog's own background, just left of the preview: the edited panel colour.
    let card = at(egui::pos2(preview.left() - 4.0, preview.center().y));
    assert!(near(card, Color32::from_rgb(0x20, 0x40, 0x60)), "dialog {card:?}");
}

fn typed_hex(h: &mut Harness<'static, PhotocraftApp>, label: &str, hex: &str) {
    h.get_by_label(label).click();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label(label).type_text(hex);
    h.run_steps(3);
}

#[test]
fn colours_that_start_out_the_same_are_linked_until_unlinked() {
    let store: Arc<Mutex<Option<String>>> = Arc::default();
    let mut h = harness(app_on(&store));
    let id = open_custom_theme(&mut h);
    const LINKED: &str = "Panel Background is linked to Title Bar and Toolbars: changing one changes all. Click to unlink.";
    const UNLINKED: &str = "Panel Background is not linked. Click to link it to Title Bar and Toolbars.";
    // Pro (Medium Gray) panels and title bar are both #535353: an edit to one changes both.
    assert!(h.query_by_label(UNLINKED).is_none());
    h.get_by_label(LINKED);
    typed_hex(&mut h, "Panel Background hex", "#c03070");
    let colors = draft(&h, id)["customTheme"]["colors"].clone();
    assert_eq!(colors, json!({"card": "#c03070", "chrome": "#c03070"}));
    assert_eq!(Tokens::get(&h.ctx).chrome, PINK, "the linked title bar shows it too");
    // The canvas and its dots, and the accent, its border and the primary button, are links too.
    let base = Tokens::for_kind(ThemeKind::ProMedium);
    let mut custom = prefs::CustomTheme::default();
    assert_eq!(super::custom_theme::linked(&custom, &base, "canvas"), ["canvas_dot"]);
    assert_eq!(super::custom_theme::linked(&custom, &base, "accent"), ["accent_border", "primary_bg"]);
    assert!(super::custom_theme::linked(&custom, &base, "danger").is_empty());
    // Studio's transparent tab strip and selected row draw nothing: no link.
    assert!(super::custom_theme::linked(&custom, &Tokens::for_kind(ThemeKind::Studio), "tab_strip").is_empty());

    // Unlinked, the panel colour changes on its own; the title bar keeps the shared one.
    h.get_by_label(LINKED).click();
    h.run_steps(3);
    assert_eq!(draft(&h, id)["customTheme"]["unlinked"], json!(["card"]));
    typed_hex(&mut h, "Panel Background hex", "#204060");
    let colors = draft(&h, id)["customTheme"]["colors"].clone();
    assert_eq!(colors, json!({"card": "#204060", "chrome": "#c03070"}));
    assert!(h.query_by_label(LINKED).is_none());

    // Linked again, it takes the link's colour; a reset then resets the whole link.
    h.get_by_label(UNLINKED).click();
    h.run_steps(3);
    let d = draft(&h, id);
    assert_eq!(d["customTheme"]["unlinked"], json!([]));
    assert_eq!(d["customTheme"]["colors"], json!({"card": "#c03070", "chrome": "#c03070"}));
    h.get_by_label("Reset Panel Background").click();
    h.run_steps(3);
    assert_eq!(draft(&h, id)["customTheme"]["colors"], json!({}));

    // Links are saved with the theme and survive a restart; Reset to Preset brings them back.
    h.get_by_label(LINKED).click();
    h.run_steps(3);
    h.get_by_label("OK").click();
    h.run_steps(6);
    let saved: Value = serde_json::from_str(store.lock().unwrap().as_deref().unwrap()).unwrap();
    assert_eq!(saved["interface"]["customTheme"]["unlinked"], json!(["card"]));
    let mut restarted = harness(app_on(&store));
    assert_eq!(restarted.state().session.prefs().interface.custom_theme.unlinked.len(), 1);
    let id = open_custom_theme(&mut restarted);
    restarted.get_by_label(UNLINKED);
    restarted.get_by_label("Reset to Preset").click();
    restarted.run_steps(3);
    assert_eq!(draft(&restarted, id)["customTheme"]["unlinked"], json!([]));
    restarted.get_by_label(LINKED);

    // A link of two with one colour taken out: linking the other puts both back together.
    custom.unlinked.insert("chrome".into());
    custom.colors.insert("chrome".into(), "#102030".into());
    assert!(super::custom_theme::linked(&custom, &base, "card").is_empty());
    super::custom_theme::relink(&mut custom, &base, "card");
    assert!(custom.unlinked.is_empty());
    assert_eq!(custom.colors.get("card").map(String::as_str), Some("#102030"));
}
