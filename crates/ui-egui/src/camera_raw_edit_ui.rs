//! Camera Raw › Edit: the panels of settings in Camera Raw's order (the treatment and Auto,
//! Light, Color, Effects, Curve, Color Mixer, Color Grading, Detail, Optics, Calibration).

use egui::{Color32, FontId, Mesh, Pos2, Rect as ERect, Sense, Shape, Stroke, pos2, vec2};
use photocraft_algo::camera_raw::{CameraRaw, Wheel, curve_lut};

use super::{BANDS, CameraRawDialog, ToneZone};
use crate::theme::Tokens;
use crate::widgets;

/// What a click in the panels asks of the dialog beyond a setting (it needs the image).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Requests {
    pub auto_tone: bool,
    pub auto_white_balance: bool,
}

/// The Curve panel's views: the parametric curve, then the point curve of RGB, red, green, blue.
pub(super) const CURVE_MODES: [&str; 5] = ["Parametric", "Point", "Red", "Green", "Blue"];
/// The Color Grading panel's views.
pub(super) const GRADING_VIEWS: [&str; 5] = ["3-Way", "Shadows", "Midtones", "Highlights", "Global"];

fn tr(label: &str) -> &str {
    crate::i18n::tr_ctx(crate::i18n::current(), "cameraRaw", label)
}

fn grey(v: u8) -> Color32 {
    Color32::from_gray(v)
}

/// One labelled slider; marks the dialog dirty when it moves, double-click resets it to `reset`.
fn slider_to(ui: &mut egui::Ui, dirty: &mut bool, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, grad: Option<&[Color32]>, reset: f32) -> egui::Response {
    let r = widgets::slider_row_compact(ui, tr(label), v, range, grad);
    if r.changed() {
        *dirty = true;
    }
    if r.double_clicked() {
        *v = reset;
        *dirty = true;
    }
    r
}

/// [`slider_to`] resetting to 0.
fn row(ui: &mut egui::Ui, dirty: &mut bool, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, grad: Option<&[Color32]>) -> egui::Response {
    slider_to(ui, dirty, label, v, range, grad, 0.0)
}

/// A Light tone slider; Alt-dragging it shows that zone's channel clipping on the preview.
fn tone_row(ui: &mut egui::Ui, dirty: &mut bool, alt: &mut Option<ToneZone>, zone: ToneZone, v: &mut f32, grad: &[Color32]) {
    let r = row(ui, dirty, zone.name(), v, -zone.limit()..=zone.limit(), Some(grad));
    if r.dragged() && ui.input(|i| i.modifiers.alt) {
        *alt = Some(zone);
    }
}

fn section(ui: &mut egui::Ui, title: &str, open: bool, body: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(egui::RichText::new(tr(title)).font(FontId::proportional(13.0))).id_salt(("camera-raw-section", title)).default_open(open).show(ui, body);
}

/// A row of pill tabs over `labels`; returns the clicked one.
fn tabs(ui: &mut egui::Ui, labels: &[&str], selected: usize) -> Option<usize> {
    tabs_named(ui, labels, selected, None)
}

/// [`tabs`] whose accessible names start with `group` (when a tab's text is also a panel's).
fn tabs_named(ui: &mut egui::Ui, labels: &[&str], selected: usize, group: Option<&str>) -> Option<usize> {
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, label) in labels.iter().enumerate() {
            let name = tr(label);
            let response = widgets::pill_tab(ui, name, selected == i);
            let accessible = group.map_or_else(|| name.to_string(), |g| format!("{g}: {name}"));
            response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), selected == i, &accessible));
            if response.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

fn temp_stops() -> Vec<Color32> {
    vec![Color32::from_rgb(70, 120, 230), grey(200), Color32::from_rgb(235, 200, 60)]
}
fn tint_stops() -> Vec<Color32> {
    vec![Color32::from_rgb(70, 190, 80), grey(200), Color32::from_rgb(210, 80, 200)]
}
fn dark_light() -> Vec<Color32> {
    vec![grey(30), grey(225)]
}
fn grey_to_colour() -> Vec<Color32> {
    let mut stops = vec![grey(128)];
    stops.extend(widgets::hue_stops());
    stops
}

/// The open-time demosaic choice (at the top of Detail). Photoshop's Camera Raw picks one method
/// for every file; here it is the user's. Open re-develops with it; the preview keeps the method
/// the file was opened with (the differences are finer than the preview's pixels).
fn demosaic_row(ui: &mut egui::Ui, m: &mut photocraft_io::raw::Demosaic) {
    use photocraft_io::raw::Demosaic;
    ui.horizontal(|ui| {
        ui.label(tr("Demosaic"))
            .on_hover_text(tl!("How the camera's colour filter mosaic is filled in. It is applied when the file opens; the preview shows the method it was opened with."));
        let options = [
            (Demosaic::Ahd, "AHD (adaptive homogeneity)", "Hirakawa–Parks: the cleanest edges and the least colour fringing, the slowest."),
            (Demosaic::Mhc, "Malvar–He–Cutler", "Gradient-corrected interpolation: sharp and fast, with some colour fringing on fine detail."),
            (Demosaic::Bilinear, "Bilinear", "The fastest and the softest, with colour fringing on fine detail."),
        ];
        widgets::dropdown_with_tooltips(ui, "camera-raw-demosaic", m, &options, 180.0);
    });
    ui.add_space(4.0);
}

/// The settings panels. `alt` collects the tone zone being Alt-dragged; `dirty` is set when a
/// setting moves.
pub(super) fn panel(ui: &mut egui::Ui, d: &mut CameraRawDialog, alt: &mut Option<ToneZone>, dirty: &mut bool) -> Requests {
    let mut requests = Requests::default();
    let CameraRawDialog { params: p, curve_state, curve_rect, mixer_tab, curve_mode, grading_view, wb_tool, raw_open, auto_wb, .. } = d;
    let dark = dark_light();
    // The treatment and Auto, above the panels as in Camera Raw.
    ui.horizontal(|ui| {
        if let Some(i) = tabs_named(ui, &["Color", "Black & White"], usize::from(p.monochrome), Some(tl!("Profile"))) {
            p.monochrome = i == 1;
            *dirty = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::secondary_button(ui, tl!("Auto"), 52.0).on_hover_text(tl!("Set the Light sliders from the image's tones")).clicked() {
                requests.auto_tone = true;
            }
        });
    });
    let profile = if p.monochrome { tl!("Adobe Monochrome") } else { tl!("Adobe Color") };
    ui.label(egui::RichText::new(format!("{}: {profile}", tl!("Profile"))).color(Tokens::get(ui.ctx()).text_faint));
    ui.add_space(2.0);
    section(ui, "Light", true, |ui| {
        tone_row(ui, dirty, alt, ToneZone::Exposure, &mut p.exposure, &dark);
        row(ui, dirty, "Contrast", &mut p.contrast, -100.0..=100.0, Some(&[grey(110), grey(150)]));
        tone_row(ui, dirty, alt, ToneZone::Highlights, &mut p.highlights, &dark);
        tone_row(ui, dirty, alt, ToneZone::Shadows, &mut p.shadows, &dark);
        tone_row(ui, dirty, alt, ToneZone::Whites, &mut p.whites, &dark);
        tone_row(ui, dirty, alt, ToneZone::Blacks, &mut p.blacks, &dark);
    });
    section(ui, "Color", false, |ui| {
        ui.horizontal(|ui| {
            ui.label(tr("White Balance"));
            let current = if (p.temperature, p.tint) == (0.0, 0.0) {
                0
            } else if *auto_wb == Some((p.temperature, p.tint)) {
                1
            } else {
                2
            };
            let mut choice = current;
            let options = [(0usize, tr("As Shot")), (1, tr("Auto")), (2, tr("Custom"))];
            if widgets::dropdown(ui, "cr-white-balance", &mut choice, &options, 110.0) && choice != current {
                match choice {
                    0 => {
                        (p.temperature, p.tint) = (0.0, 0.0);
                        *dirty = true;
                    }
                    1 => requests.auto_white_balance = true,
                    _ => {}
                }
            }
            let picker = crate::icons::button(ui, "pipette", 24.0, *wb_tool, tl!("White Balance Tool (I): click something that should be neutral grey"));
            if picker.clicked() {
                *wb_tool = !*wb_tool;
            }
        });
        let (ts, tn) = (temp_stops(), tint_stops());
        row(ui, dirty, "Temperature", &mut p.temperature, -100.0..=100.0, Some(&ts));
        row(ui, dirty, "Tint", &mut p.tint, -100.0..=100.0, Some(&tn));
        let colourful = grey_to_colour();
        ui.add_enabled_ui(!p.monochrome, |ui| {
            row(ui, dirty, "Vibrance", &mut p.vibrance, -100.0..=100.0, Some(&colourful));
            row(ui, dirty, "Saturation", &mut p.saturation, -100.0..=100.0, Some(&colourful));
        });
    });
    section(ui, "Effects", false, |ui| {
        row(ui, dirty, "Texture", &mut p.texture, -100.0..=100.0, None);
        row(ui, dirty, "Clarity", &mut p.clarity, -100.0..=100.0, None);
        row(ui, dirty, "Dehaze", &mut p.dehaze, -100.0..=100.0, None);
        widgets::section_label(ui, tl!("Vignetting"));
        let mut style = p.vignette_style.clone();
        if widgets::dropdown(
            ui,
            "cr-vig-style",
            &mut style,
            &[
                ("highlightPriority".to_string(), tl!("Highlight Priority")),
                ("colorPriority".to_string(), tl!("Color Priority")),
                ("paintOverlay".to_string(), tl!("Paint Overlay")),
            ],
            200.0,
        ) {
            p.vignette_style = style;
            *dirty = true;
        }
        row(ui, dirty, "Amount", &mut p.vignette_amount, -100.0..=100.0, Some(&[grey(20), grey(128), grey(235)]));
        slider_to(ui, dirty, "Midpoint", &mut p.vignette_midpoint, 0.0..=100.0, None, 50.0);
        row(ui, dirty, "Roundness", &mut p.vignette_roundness, -100.0..=100.0, None);
        slider_to(ui, dirty, "Feather", &mut p.vignette_feather, 0.0..=100.0, None, 50.0);
        row(ui, dirty, "Highlights", &mut p.vignette_highlights, 0.0..=100.0, None);
        widgets::section_label(ui, tl!("Grain"));
        row(ui, dirty, "Amount", &mut p.grain_amount, 0.0..=100.0, None);
        slider_to(ui, dirty, "Size", &mut p.grain_size, 0.0..=100.0, None, 25.0);
        slider_to(ui, dirty, "Roughness", &mut p.grain_roughness, 0.0..=100.0, None, 50.0);
    });
    section(ui, "Curve", false, |ui| {
        if let Some(i) = tabs(ui, &CURVE_MODES, *curve_mode) {
            if i != *curve_mode {
                *curve_state = Default::default();
            }
            *curve_mode = i;
        }
        if *curve_mode == 0 {
            *curve_rect = Some(parametric_editor(ui, p, dirty));
            row(ui, dirty, "Highlights", &mut p.curve_highlights, -100.0..=100.0, None);
            row(ui, dirty, "Lights", &mut p.curve_lights, -100.0..=100.0, None);
            row(ui, dirty, "Darks", &mut p.curve_darks, -100.0..=100.0, None);
            row(ui, dirty, "Shadows", &mut p.curve_shadows, -100.0..=100.0, None);
        } else {
            let t = Tokens::get(ui.ctx());
            let (curve, colour) = match *curve_mode {
                2 => (&mut p.point_curve_red, Color32::from_rgb(235, 80, 80)),
                3 => (&mut p.point_curve_green, Color32::from_rgb(90, 200, 90)),
                4 => (&mut p.point_curve_blue, Color32::from_rgb(90, 140, 240)),
                _ => (&mut p.point_curve, t.text),
            };
            *curve_rect = Some(curve_editor(ui, curve, colour, dirty, curve_state));
            slider_to(ui, dirty, "Refine Saturation", &mut p.curve_refine_saturation, 0.0..=100.0, None, 100.0)
                .on_hover_text(tl!("How much of the saturation a contrasty curve adds is kept"));
        }
    });
    section(ui, if p.monochrome { "B&W Mix" } else { "Color Mixer" }, false, |ui| {
        if p.monochrome {
            for (k, name) in BANDS.iter().enumerate() {
                let base = widgets::hue_color(photocraft_algo::camera_raw::HSL_BANDS[k]);
                row(ui, dirty, name, &mut p.gray_mix[k], -100.0..=100.0, Some(&[grey(40), base, grey(220)]));
            }
            return;
        }
        if let Some(i) = tabs(ui, &["Hue", "Saturation", "Luminance", "All"], *mixer_tab) {
            *mixer_tab = i;
        }
        let groups: &[usize] = if *mixer_tab == 3 { &[0, 1, 2] } else { std::slice::from_ref(mixer_tab) };
        for &g in groups {
            if *mixer_tab == 3 {
                widgets::section_label(ui, tr(["Hue", "Saturation", "Luminance"][g]));
            }
            let arr = match g {
                0 => &mut p.hsl_hue,
                1 => &mut p.hsl_sat,
                _ => &mut p.hsl_lum,
            };
            for (k, name) in BANDS.iter().enumerate() {
                let c = photocraft_algo::camera_raw::HSL_BANDS[k];
                let base = widgets::hue_color(c);
                // The tracks show where each slider takes its colours, as Camera Raw's do.
                let grad: Vec<Color32> = match g {
                    0 => vec![widgets::hue_color(c - 30.0), base, widgets::hue_color(c + 30.0)],
                    1 => vec![grey(128), base],
                    _ => vec![grey(30), base, grey(235)],
                };
                row(ui, dirty, name, &mut arr[k], -100.0..=100.0, Some(&grad));
            }
        }
    });
    section(ui, "Color Grading", false, |ui| {
        if let Some(i) = tabs(ui, &GRADING_VIEWS, *grading_view) {
            *grading_view = i;
        }
        let full = ui.available_width();
        match *grading_view {
            0 => {
                ui.vertical_centered(|ui| {
                    widgets::section_label(ui, tr("Midtones"));
                    grading_wheel(ui, "Midtones", &mut p.grade_midtones, (full * 0.4).clamp(80.0, 120.0), dirty);
                });
                row(ui, dirty, "Luminance", &mut p.grade_midtones.lum, -100.0..=100.0, Some(&dark));
                ui.columns(2, |cols| {
                    for (col, (title, w)) in cols.iter_mut().zip([("Shadows", &mut p.grade_shadows), ("Highlights", &mut p.grade_highlights)]) {
                        col.vertical_centered(|ui| {
                            widgets::section_label(ui, tr(title));
                            grading_wheel(ui, title, w, (full * 0.3).clamp(64.0, 96.0), dirty);
                            let mut lum = w.lum;
                            let r = widgets::slider_row_compact(ui, "", &mut lum, -100.0..=100.0, Some(&dark));
                            if r.changed() {
                                w.lum = lum;
                                *dirty = true;
                            }
                        });
                    }
                });
            }
            v => {
                let (title, w) = match v {
                    1 => ("Shadows", &mut p.grade_shadows),
                    2 => ("Midtones", &mut p.grade_midtones),
                    3 => ("Highlights", &mut p.grade_highlights),
                    _ => ("Global", &mut p.grade_global),
                };
                ui.vertical_centered(|ui| grading_wheel(ui, title, w, (full * 0.5).clamp(100.0, 160.0), dirty));
                let hs = widgets::hue_stops();
                row(ui, dirty, "Hue", &mut w.hue, 0.0..=360.0, Some(&hs));
                row(ui, dirty, "Saturation", &mut w.sat, 0.0..=100.0, Some(&[grey(128), widgets::hue_color(w.hue)]));
                row(ui, dirty, "Luminance", &mut w.lum, -100.0..=100.0, Some(&dark));
            }
        }
        slider_to(ui, dirty, "Blending", &mut p.grade_blending, 0.0..=100.0, None, 50.0);
        row(ui, dirty, "Balance", &mut p.grade_balance, -100.0..=100.0, None);
    });
    section(ui, "Detail", false, |ui| {
        if let Some(raw) = raw_open.as_mut() {
            demosaic_row(ui, &mut raw.demosaic);
        }
        widgets::section_label(ui, tl!("Sharpening"));
        row(ui, dirty, "Amount", &mut p.sharpen_amount, 0.0..=150.0, None);
        slider_to(ui, dirty, "Radius", &mut p.sharpen_radius, 0.5..=3.0, None, 1.0);
        slider_to(ui, dirty, "Detail", &mut p.sharpen_detail, 0.0..=100.0, None, 25.0);
        row(ui, dirty, "Masking", &mut p.sharpen_masking, 0.0..=100.0, None);
        widgets::section_label(ui, tl!("Noise Reduction"));
        row(ui, dirty, "Luminance", &mut p.noise_luminance, 0.0..=100.0, None);
        slider_to(ui, dirty, "Luminance Detail", &mut p.noise_luminance_detail, 0.0..=100.0, None, 50.0);
        row(ui, dirty, "Luminance Contrast", &mut p.noise_luminance_contrast, 0.0..=100.0, None);
        row(ui, dirty, "Color", &mut p.noise_color, 0.0..=100.0, None);
        slider_to(ui, dirty, "Color Detail", &mut p.noise_color_detail, 0.0..=100.0, None, 50.0);
        slider_to(ui, dirty, "Color Smoothness", &mut p.noise_color_smoothness, 0.0..=100.0, None, 50.0);
    });
    section(ui, "Optics", false, |ui| {
        widgets::section_label(ui, tl!("Defringe"));
        let purple = [widgets::hue_color(200.0), widgets::hue_color(270.0), widgets::hue_color(340.0)];
        let green = [widgets::hue_color(40.0), widgets::hue_color(110.0), widgets::hue_color(180.0)];
        row(ui, dirty, "Purple Amount", &mut p.defringe_purple_amount, 0.0..=20.0, None);
        range_row(ui, dirty, "Purple Hue", &mut p.defringe_purple_hue, [30.0, 70.0], &purple);
        row(ui, dirty, "Green Amount", &mut p.defringe_green_amount, 0.0..=20.0, None);
        range_row(ui, dirty, "Green Hue", &mut p.defringe_green_hue, [40.0, 60.0], &green);
        widgets::section_label(ui, tl!("Distortion"));
        row(ui, dirty, "Distortion", &mut p.lens_distortion, -100.0..=100.0, None).on_hover_text(tl!("Positive values straighten barrel distortion, negative values pincushion"));
        widgets::section_label(ui, tl!("Lens Vignetting"));
        row(ui, dirty, "Amount", &mut p.lens_vignette_amount, -100.0..=100.0, Some(&[grey(20), grey(128), grey(235)]));
        ui.add_enabled_ui(p.lens_vignette_amount != 0.0, |ui| {
            slider_to(ui, dirty, "Midpoint", &mut p.lens_vignette_midpoint, 0.0..=100.0, None, 50.0);
        });
    });
    section(ui, "Calibration", false, |ui| {
        widgets::section_label(ui, tr("Shadows"));
        let tn = [Color32::from_rgb(70, 190, 80), grey(200), Color32::from_rgb(210, 80, 200)];
        row(ui, dirty, "Tint", &mut p.shadow_tint, -100.0..=100.0, Some(&tn));
        for (title, centre, hue, sat) in [
            ("Red Primary", 0.0, &mut p.red_hue, &mut p.red_saturation),
            ("Green Primary", 120.0, &mut p.green_hue, &mut p.green_saturation),
            ("Blue Primary", 240.0, &mut p.blue_hue, &mut p.blue_saturation),
        ] {
            widgets::section_label(ui, tr(title));
            let hues = [widgets::hue_color(centre - 30.0), widgets::hue_color(centre), widgets::hue_color(centre + 30.0)];
            row(ui, dirty, "Hue", hue, -100.0..=100.0, Some(&hues));
            row(ui, dirty, "Saturation", sat, -100.0..=100.0, Some(&[grey(128), widgets::hue_color(centre)]));
        }
    });
    requests
}

/// Two-handled slider over 0 … 100 (a defringe hue range), handles at least 10 apart.
fn range_row(ui: &mut egui::Ui, dirty: &mut bool, label: &str, v: &mut [f32; 2], reset: [f32; 2], stops: &[Color32]) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(tr(label));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format!("{:.0} / {:.0}", v[0], v[1])).color(t.text_faint));
        });
    });
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 16.0), Sense::click_and_drag());
    let track = ERect::from_min_max(pos2(rect.left() + 6.0, rect.center().y - 3.0), pos2(rect.right() - 6.0, rect.center().y + 3.0));
    let painter = ui.painter_at(rect.expand(2.0));
    // The gradient track, a band of quads.
    let mut mesh = Mesh::default();
    let n = stops.len().max(2);
    for (i, c) in stops.iter().enumerate() {
        let x = track.left() + track.width() * i as f32 / (n - 1) as f32;
        mesh.colored_vertex(pos2(x, track.top()), *c);
        mesh.colored_vertex(pos2(x, track.bottom()), *c);
        if i > 0 {
            let k = (2 * i) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    painter.add(Shape::mesh(mesh));
    let to_x = |value: f32| track.left() + track.width() * value.clamp(0.0, 100.0) / 100.0;
    let handles = [to_x(v[0]), to_x(v[1])];
    if response.drag_started() || response.clicked() {
        let at = response.interact_pointer_pos().map_or(0.0, |p| p.x);
        let nearest = usize::from((at - handles[1]).abs() < (at - handles[0]).abs());
        ui.ctx().data_mut(|data| data.insert_temp(response.id, nearest));
    }
    if (response.dragged() || response.clicked())
        && let Some(p) = response.interact_pointer_pos()
    {
        let which = ui.ctx().data(|data| data.get_temp::<usize>(response.id)).unwrap_or(0).min(1);
        let value = ((p.x - track.left()) / track.width().max(1.0) * 100.0).round().clamp(0.0, 100.0);
        if which == 0 {
            v[0] = value.min(v[1] - 10.0).max(0.0);
        } else {
            v[1] = value.max(v[0] + 10.0).min(100.0);
        }
        *dirty = true;
    }
    if response.double_clicked() {
        *v = reset;
        *dirty = true;
    }
    for x in [to_x(v[0]), to_x(v[1])] {
        let c = pos2(x, track.center().y);
        painter.circle_filled(c, 5.0, t.text);
        painter.circle_stroke(c, 5.0, Stroke::new(1.0, t.separator));
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, tr(label)));
}

/// A Color Grading wheel: angle is hue (red to the right, counter-clockwise), distance from the
/// centre saturation. Click or drag to set both; double-click resets.
fn grading_wheel(ui: &mut egui::Ui, title: &str, w: &mut Wheel, size: f32, dirty: &mut bool) {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let centre = rect.center();
    let radius = size / 2.0 - 3.0;
    let painter = ui.painter_at(rect.expand(2.0));
    const RINGS: usize = 6;
    const SEGMENTS: usize = 48;
    let colour = |hue: f32, s: f32| {
        let c = widgets::hue_color(hue);
        let g = 150.0;
        let mix = |a: u8| (g + (f32::from(a) - g) * s).round().clamp(0.0, 255.0) as u8;
        Color32::from_rgb(mix(c.r()), mix(c.g()), mix(c.b()))
    };
    let mut mesh = Mesh::default();
    mesh.colored_vertex(centre, colour(0.0, 0.0));
    for ring in 1..=RINGS {
        let s = ring as f32 / RINGS as f32;
        for seg in 0..SEGMENTS {
            let a = seg as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            mesh.colored_vertex(centre + vec2(a.cos(), -a.sin()) * radius * s, colour(a.to_degrees(), s));
        }
    }
    let at = |ring: usize, seg: usize| -> u32 { if ring == 0 { 0 } else { (1 + (ring - 1) * SEGMENTS + seg % SEGMENTS) as u32 } };
    for ring in 0..RINGS {
        for seg in 0..SEGMENTS {
            if ring == 0 {
                mesh.add_triangle(0, at(1, seg), at(1, seg + 1));
            } else {
                mesh.add_triangle(at(ring, seg), at(ring + 1, seg), at(ring + 1, seg + 1));
                mesh.add_triangle(at(ring, seg), at(ring + 1, seg + 1), at(ring, seg + 1));
            }
        }
    }
    painter.add(Shape::mesh(mesh));
    painter.circle_stroke(centre, radius, Stroke::new(1.0, t.separator));
    if (response.dragged() || response.clicked())
        && let Some(p) = response.interact_pointer_pos()
    {
        let v = p - centre;
        let hue = (-v.y).atan2(v.x).to_degrees().rem_euclid(360.0);
        w.hue = hue.round() % 360.0;
        w.sat = (v.length() / radius * 100.0).clamp(0.0, 100.0).round();
        *dirty = true;
    }
    if response.double_clicked() {
        (w.hue, w.sat) = (0.0, 0.0);
        *dirty = true;
    }
    let a = w.hue.to_radians();
    let puck: Pos2 = centre + vec2(a.cos(), -a.sin()) * radius * (w.sat / 100.0).clamp(0.0, 1.0);
    painter.circle_filled(puck, 4.5, Color32::WHITE);
    painter.circle_stroke(puck, 4.5, Stroke::new(1.5, Color32::from_black_alpha(160)));
    let label = format!("{}: {} {:.0}°, {} {:.0}", tr(title), tr("Hue"), w.hue, tr("Saturation"), w.sat);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, &label));
    response.on_hover_text(label);
}

// The shared Curve editor must never produce a curve `CameraRaw::validate` rejects.
const _: () = assert!(crate::point_curve::MAX_POINTS == photocraft_algo::camera_raw::MAX_CURVE_POINTS && crate::point_curve::MIN_GAP >= 1.0);

/// The curve's square: grid lines in quarters.
fn curve_square(ui: &mut egui::Ui) -> (ERect, egui::Response) {
    let t = Tokens::get(ui.ctx());
    let side = ui.available_width().min(200.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, t.radius_sm, t.field);
    for i in 1..4 {
        let f = i as f32 / 4.0;
        painter.line_segment([pos2(rect.left() + f * side, rect.top()), pos2(rect.left() + f * side, rect.bottom())], Stroke::new(1.0, t.separator));
        painter.line_segment([pos2(rect.left(), rect.top() + f * side), pos2(rect.right(), rect.top() + f * side)], Stroke::new(1.0, t.separator));
    }
    (rect, resp)
}

/// Point-curve editor for one channel: click to add, drag to move, right-click to delete.
fn curve_editor(ui: &mut egui::Ui, curve: &mut Vec<[f32; 2]>, colour: Color32, dirty: &mut bool, state: &mut crate::state::PointCurveState) -> ERect {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = curve_square(ui);
    let side = rect.width();
    let painter = ui.painter_at(rect);
    let to_screen = |x: f32, y: f32| pos2(rect.left() + x / 255.0 * side, rect.bottom() - y / 255.0 * side);
    // The identity handles are presentation until a gesture actually edits them.
    let mut points = if curve.is_empty() { vec![[0.0, 0.0], [255.0, 255.0]] } else { curve.clone() };
    if crate::point_curve::interact(ui, &resp, rect, &mut points, state).changed {
        *curve = points.clone();
        *dirty = true;
    }
    let lut = curve_lut(&points, 256);
    let pts: Vec<Pos2> = lut.iter().enumerate().map(|(i, value)| to_screen(i as f32, value * 255.0)).collect();
    painter.add(Shape::line(pts, Stroke::new(1.5, colour)));
    for (i, c) in points.iter().enumerate() {
        let centre = to_screen(c[0], c[1]);
        if state.selected == Some(i) {
            painter.circle_filled(centre, 3.0, t.accent);
        }
        painter.circle_stroke(centre, 4.0, Stroke::new(1.5, t.accent));
    }
    rect
}

/// The parametric curve, with its three region splits as handles under it.
fn parametric_editor(ui: &mut egui::Ui, p: &mut CameraRaw, dirty: &mut bool) -> ERect {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = curve_square(ui);
    let side = rect.width();
    let painter = ui.painter_at(rect);
    let lut = photocraft_algo::camera_raw::parametric_curve(p, 256);
    let pts: Vec<Pos2> = lut.iter().enumerate().map(|(i, v)| pos2(rect.left() + i as f32 / 255.0 * side, rect.bottom() - v * side)).collect();
    painter.add(Shape::line(pts, Stroke::new(1.5, t.text)));
    for s in p.curve_splits {
        let x = rect.left() + s / 100.0 * side;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, t.text_faint.gamma_multiply(0.5)));
    }
    // The split handles: triangles under the square, dragged along it.
    let (strip, response) = ui.allocate_exact_size(vec2(side, 12.0), Sense::click_and_drag());
    let to_x = |s: f32| strip.left() + s / 100.0 * side;
    if response.drag_started()
        && let Some(at) = response.interact_pointer_pos()
    {
        let nearest = (0..3).min_by(|a, b| (to_x(p.curve_splits[*a]) - at.x).abs().total_cmp(&(to_x(p.curve_splits[*b]) - at.x).abs())).unwrap_or(0);
        ui.ctx().data_mut(|data| data.insert_temp(response.id, nearest));
    }
    if response.dragged()
        && let Some(at) = response.interact_pointer_pos()
    {
        let k = ui.ctx().data(|data| data.get_temp::<usize>(response.id)).unwrap_or(0).min(2);
        let v = ((at.x - strip.left()) / side.max(1.0) * 100.0).round();
        let lo = if k == 0 { 5.0 } else { p.curve_splits[k - 1] + 5.0 };
        let hi = if k == 2 { 95.0 } else { p.curve_splits[k + 1] - 5.0 };
        p.curve_splits[k] = v.clamp(lo, hi.max(lo));
        *dirty = true;
    }
    if response.double_clicked() {
        p.curve_splits = [25.0, 50.0, 75.0];
        *dirty = true;
    }
    for s in p.curve_splits {
        let x = to_x(s);
        ui.painter().add(Shape::convex_polygon(vec![pos2(x, strip.top() + 1.0), pos2(x - 5.0, strip.bottom() - 1.0), pos2(x + 5.0, strip.bottom() - 1.0)], t.text, Stroke::NONE));
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, tl!("Curve region splits")));
    rect
}
