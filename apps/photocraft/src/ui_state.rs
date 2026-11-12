//! The saved window geometry and egui layout (`ui.ron`, written by eframe's persistence).
//!
//! eframe restores this state while it creates the window, before PhotoCraft's code runs, and
//! some values it does not check abort startup: a zoom factor of zero, NaN or 1e30 fails a
//! `dpi` assertion, and a NaN or huge window size fails surface creation. A truncated or
//! hand-edited file would then crash every launch. [`sanitize`] runs first and removes a file
//! holding such values (the window and panels open at their defaults; nothing else is lost).
//! A file that isn't RON at all is left alone: eframe already ignores it.
//!
//! egui 0.37 also replaced the flat `WindowSettings` (`inner_position_pixels`,
//! `inner_size_points: Option<Vec2>`, …) with `normal_geometry: { outer_position_pixels,
//! scale_factor, inner_size_points }`. eframe reads the file before any application code, so an
//! old value cannot be interpreted lazily: [`sanitize`] rewrites it before eframe loads it and
//! keeps a one-time `ui.ron.pre-0.37` backup. The old panel keys in the egui memory part are
//! untouched (`panels::panel_state_id` migrates them additively), but this window rewrite is
//! one-way: after it, a 0.36 build ignores the window entry and opens at its default geometry.
//! Restoring the backup is the documented rollback.

use std::{collections::HashMap, path::Path};

/// Window positions, sizes and panel rects beyond this many points or pixels are corrupt.
const MAX_COORD: f64 = 1.0e6;
/// egui's zoom factor is the UI scale over the display scale (75% on a 2× display is 0.375,
/// 300% on a 1× display is 3).
const ZOOM: std::ops::RangeInclusive<f64> = 0.1..=10.0;
/// The size [`crate::native_options`] opens a window with when an old save has no usable size.
const DEFAULT_SIZE: f32 = 1440.0;
const DEFAULT_HEIGHT: f32 = 900.0;
/// Kept beside `ui.ron` before the one-way 0.36 → 0.37 window rewrite.
const BACKUP_SUFFIX: &str = "pre-0.37";

/// Remove `path` if it holds state that would crash startup, and migrate an old window entry to
/// the 0.37 schema before eframe reads it. Never fails: errors are logged and leave the file
/// alone (eframe then ignores what it cannot read).
pub fn sanitize(path: Option<&Path>) {
    let Some(path) = path else { return };
    let Ok(text) = std::fs::read_to_string(path) else { return };
    if let Err(why) = check(&text) {
        log::warn!("ignoring saved window layout {}: {why}", path.display());
        if let Err(e) = std::fs::remove_file(path) {
            log::warn!("could not remove {}: {e}", path.display());
        }
        return;
    }
    match migrate_window(text) {
        Ok(Some(text)) => write_migrated(path, &text),
        Ok(None) => {}
        Err(why) => log::warn!("not rewriting the saved window geometry in {}: {why}", path.display()),
    }
}

/// Write the migrated file, keeping a one-time backup. A failed backup aborts the rewrite: the
/// pre-0.37 window geometry is the documented rollback, so never destroy the only copy.
fn write_migrated(path: &Path, text: &str) {
    let backup = backup_path(path);
    if !backup.exists()
        && let Err(e) = std::fs::copy(path, &backup)
    {
        log::warn!("could not back up {} to {}: {e}; leaving it unchanged", path.display(), backup.display());
        return;
    }
    if let Err(e) = std::fs::write(path, text) {
        log::warn!("could not write the migrated window layout to {}: {e}", path.display());
    }
}

fn backup_path(path: &Path) -> std::path::PathBuf {
    let mut name = path.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_else(|| "ui.ron".into());
    name.push(format!(".{BACKUP_SUFFIX}"));
    path.with_file_name(name)
}

/// Whether `path` holds a window position that eframe restores, which means
/// `NativeOptions::centered` must be off: eframe applies `centered` after the restored geometry,
/// so it moved the window back to the centre of the main monitor on every launch (#4826).
///
/// Only the 0.37 `normal_geometry.outer_position_pixels` counts: `egui_winit::WindowGeometry`
/// sets it and `initialize_viewport_builder` applies it. A 0.36 flat entry is not restored by
/// 0.37 (`sanitize` rewrites it first), and its macOS `inner_position_pixels` was never an outer
/// position.
pub fn has_saved_position(path: Option<&Path>) -> bool {
    let Some(text) = path.and_then(|p| std::fs::read_to_string(p).ok()) else { return false };
    saved_position(&text)
}

fn saved_position(text: &str) -> bool {
    let Ok(kv) = ron::from_str::<HashMap<String, String>>(text) else { return false };
    let Some(window) = kv.get("window").and_then(|w| ron::from_str::<ron::Value>(w).ok()) else { return false };
    let Some(geometry) = field(&window, "normal_geometry") else { return false };
    matches!(field(geometry, "outer_position_pixels"), Some(ron::Value::Option(Some(_))))
}

/// The 0.36 → 0.37 rewrite of one `ui.ron` text, for the recorded 0.14/0.15 fixtures.
#[cfg(test)]
pub(crate) fn migrate_for_test(text: &str) -> Result<Option<String>, String> {
    migrate_window(text.to_owned())
}

/// [`saved_position`] for the recorded fixtures.
#[cfg(test)]
pub(crate) fn saved_position_for_test(text: &str) -> bool {
    saved_position(text)
}

/// `Err` names the first value that would crash startup.
fn check(text: &str) -> Result<(), String> {
    // eframe stores a map of key → RON string; a file that doesn't parse is ignored by eframe.
    let Ok(kv) = ron::from_str::<HashMap<String, String>>(text) else { return Ok(()) };
    for (key, value) in &kv {
        // eframe also skips an entry that doesn't parse.
        let Some(scanned) = scan(value) else { continue };
        let zoom = scanned.map_err(|e| format!("{key}: {e}"))?;
        if key == "egui"
            && let Some(z) = zoom.filter(|z| !ZOOM.contains(z))
        {
            return Err(format!("egui: zoom factor {z}"));
        }
    }
    Ok(())
}

/// [`floats_sane`] and the egui zoom factor, read straight off one entry's RON text: every float
/// outside nested strings must be finite and within [`MAX_COORD`], nesting no deeper than 64;
/// `Ok` carries the first `zoom_factor` (egui's `options` come first). `None` when the text is not
/// well-formed (eframe skips such an entry).
///
/// Parsing egui's memory into a `ron::Value` instead took seconds at every launch once its list
/// of closed popups' areas had grown (an 842 KB entry: 4 s of a 6 s start; user report,
/// 2026-11-11): RON's untyped parser retries each nested `(…)` several ways.
fn scan(text: &str) -> Option<Result<Option<f64>, String>> {
    let b = text.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let (mut i, mut depth, mut zoom, mut want_zoom) = (0usize, 0usize, None, false);
    while i < b.len() {
        let c = b[i];
        match c {
            b'"' => {
                // A nested string (egui's persisted values hold RON text of their own): skipped,
                // as `floats_sane` never looked inside strings.
                i += 1;
                loop {
                    match b.get(i)? {
                        b'\\' => i += 2,
                        b'"' => break,
                        _ => i += 1,
                    }
                }
                i += 1;
            }
            b'\'' => {
                i += 1;
                loop {
                    match b.get(i)? {
                        b'\\' => i += 2,
                        b'\'' => break,
                        _ => i += 1,
                    }
                }
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..].find("*/")?;
                i += end + 4;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                if depth > 64 {
                    return Some(Err("nested too deeply".into()));
                }
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                i += 1;
            }
            b'0'..=b'9' | b'-' | b'+' | b'.' => {
                let start = i;
                i += 1;
                while i < b.len() && (ident(b[i]) || b[i] == b'.' || ((b[i] == b'-' || b[i] == b'+') && matches!(b[i - 1], b'e' | b'E'))) {
                    i += 1;
                }
                let token = &text[start..i];
                let body = token.trim_start_matches(['-', '+']);
                if body == "inf" || body == "NaN" {
                    return Some(Err(format!("value {token}")));
                }
                let hex = body.starts_with("0x") || body.starts_with("0b") || body.starts_with("0o");
                let float = !hex && (body.contains('.') || body.contains(['e', 'E']));
                if float {
                    let digits: String = token.trim_end_matches("f32").trim_end_matches("f64").chars().filter(|c| *c != '_').collect();
                    if let Ok(f) = digits.parse::<f64>() {
                        if !f.is_finite() || f.abs() > MAX_COORD {
                            return Some(Err(format!("value {f}")));
                        }
                        if want_zoom && zoom.is_none() {
                            zoom = Some(f);
                        }
                    }
                }
                want_zoom = false;
            }
            c if ident(c) => {
                let start = i;
                while i < b.len() && ident(b[i]) {
                    i += 1;
                }
                let word = &text[start..i];
                // A raw string, r"…" or r#"…"#: skipped like any string.
                if word == "r" && matches!(b.get(i), Some(b'"' | b'#')) {
                    let hashes = text[i..].bytes().take_while(|c| *c == b'#').count();
                    let close = format!("\"{}", "#".repeat(hashes));
                    let body = i + hashes + 1;
                    i = body + text.get(body..)?.find(&close)? + close.len();
                    continue;
                }
                if word == "inf" || word == "NaN" {
                    return Some(Err(format!("value {word}")));
                }
                want_zoom = word == "zoom_factor" && zoom.is_none();
            }
            b':' | b',' | b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            _ => {
                want_zoom = false;
                i += 1;
            }
        }
    }
    (depth == 0).then_some(Ok(zoom))
}

/// `Some(new_text)` when `text` is an old-schema file that should be rewritten; `None` when it
/// is already 0.37-shaped or holds no window entry; `Err` when the old window entry is unusable
/// and must be left untouched.
fn migrate_window(text: String) -> Result<Option<String>, String> {
    let Ok(mut kv) = ron::from_str::<HashMap<String, String>>(&text) else { return Ok(None) };
    let Some(value) = kv.get("window").cloned() else { return Ok(None) };
    let Ok(window) = ron::from_str::<ron::Value>(&value) else { return Ok(None) };
    if field(&window, "normal_geometry").is_some() {
        return Ok(None);
    }
    if field(&window, "inner_size_points").is_none() && field(&window, "inner_position_pixels").is_none() && field(&window, "outer_position_pixels").is_none() {
        return Ok(None);
    }
    let migrated = migrate_window_value(&window)?;
    kv.insert("window".into(), migrated);
    ron::to_string(&kv).map(Some).map_err(|e| format!("cannot write the new window entry: {e}"))
}

/// The RON payload eframe 0.37 deserializes as `egui_winit::WindowSettings`.
///
/// The field order and names mirror `WindowGeometry`; the types are local because
/// `egui_winit::WindowGeometry` keeps its fields private.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq)]
struct PointV37 {
    x: f32,
    y: f32,
}

impl PointV37 {
    fn sane(self) -> Option<Self> {
        let ok = |v: f32| v.is_finite() && (v.abs() as f64) <= MAX_COORD;
        (ok(self.x) && ok(self.y)).then_some(self)
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq)]
struct GeometryV37 {
    outer_position_pixels: Option<PointV37>,
    scale_factor: f32,
    inner_size_points: PointV37,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq)]
struct WindowV37 {
    normal_geometry: GeometryV37,
    fullscreen: bool,
    maximized: bool,
}

fn migrate_window_value(window: &ron::Value) -> Result<String, String> {
    let size = old_size(window)?;
    // The old macOS entry stored an inner (not outer) position in logical points and Linux saved
    // no scale factor, so a faithful restore is only possible for the Windows physical outer
    // position. Everywhere else the window opens centred with its saved size.
    let position = if cfg!(target_os = "windows") { old_position(window, "outer_position_pixels") } else { None };
    let window = WindowV37 {
        normal_geometry: GeometryV37 {
            outer_position_pixels: position,
            // Physical pixels of the old Windows position: 0.37 divides by zoom × scale.
            scale_factor: 1.0,
            inner_size_points: size,
        },
        fullscreen: old_bool(window, "fullscreen"),
        maximized: old_bool(window, "maximized"),
    };
    ron::to_string(&window).map_err(|e| format!("cannot serialize the new window entry: {e}"))
}

fn old_size(window: &ron::Value) -> Result<PointV37, String> {
    let value = field(window, "inner_size_points");
    let Some(ron::Value::Option(Some(size))) = value else {
        // A missing size is the app's own opening size, not corrupt data.
        return Ok(PointV37 { x: DEFAULT_SIZE, y: DEFAULT_HEIGHT });
    };
    let point = point_value(size).ok_or_else(|| "inner_size_points is not a finite (x, y) point".to_string())?;
    let point = point.sane().ok_or_else(|| "inner_size_points is not finite or is absurdly large".to_string())?;
    if point.x < 1.0 || point.y < 1.0 {
        return Err("inner_size_points has a zero or negative side".into());
    }
    Ok(point)
}

/// One dimension of an old `Option<Pos2>` field, rejecting corrupt values rather than guessing.
fn old_position(window: &ron::Value, key: &str) -> Option<PointV37> {
    match field(window, key) {
        Some(ron::Value::Option(Some(p))) => point_value(p).and_then(PointV37::sane),
        // `None`, missing, and malformed all mean "no position"; corrupt positions elsewhere are
        // caught by `check`, which removes a file with non-finite values before this runs.
        _ => None,
    }
}

fn old_bool(window: &ron::Value, key: &str) -> bool {
    matches!(field(window, key), Some(ron::Value::Bool(true)))
}

fn point_value(v: &ron::Value) -> Option<PointV37> {
    let x = field(v, "x").and_then(as_f32)?;
    let y = field(v, "y").and_then(as_f32)?;
    Some(PointV37 { x, y })
}

/// Every float finite and within [`MAX_COORD`]; depth-limited (the file is untrusted).
/// The reference walk [`scan`] reproduces without building a `ron::Value` (tests compare them).
#[cfg(test)]
fn floats_sane(v: &ron::Value, depth: usize) -> Result<(), String> {
    if depth > 64 {
        return Err("nested too deeply".into());
    }
    match v {
        ron::Value::Number(_) => match as_f64(v) {
            Some(f) if !f.is_finite() || f.abs() > MAX_COORD => Err(format!("value {f}")),
            _ => Ok(()),
        },
        ron::Value::Option(Some(inner)) => floats_sane(inner, depth + 1),
        ron::Value::Seq(items) => items.iter().try_for_each(|i| floats_sane(i, depth + 1)),
        ron::Value::Map(m) => m.iter().try_for_each(|(k, v)| floats_sane(k, depth + 1).and_then(|()| floats_sane(v, depth + 1))),
        _ => Ok(()),
    }
}

/// A float (integers such as egui's hashed ids are not coordinates).
fn as_f64(v: &ron::Value) -> Option<f64> {
    match v {
        ron::Value::Number(ron::value::Number::F32(f)) => Some(f64::from(f.get())),
        ron::Value::Number(ron::value::Number::F64(f)) => Some(f.get()),
        _ => None,
    }
}

fn as_f32(v: &ron::Value) -> Option<f32> {
    as_f64(v).and_then(|f| {
        let f = f as f32;
        f.is_finite().then_some(f)
    })
}

fn field<'a>(v: &'a ron::Value, name: &str) -> Option<&'a ron::Value> {
    match v {
        ron::Value::Map(m) => m.get(&ron::Value::String(name.into())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_HEIGHT, DEFAULT_SIZE, WindowV37, backup_path, check, migrate_window, saved_position};
    use std::{collections::HashMap, path::PathBuf};

    fn file(window: &str, zoom: &str) -> String {
        let egui = format!("(options:(zoom_factor:{zoom},max_passes:2),data:([(18446744073709551615,(ron:\"(x:1.0)\"))]))");
        format!("{{\"window\":{window:?},\"egui\":{egui:?}}}")
    }

    const WINDOW: &str = "(inner_position_pixels:Some((x:616.0,y:430.0)),fullscreen:false,inner_size_points:Some((x:1100.0,y:760.0)))";
    const WINDOW_WITH_OUTER: &str = "(inner_position_pixels:Some((x:2048.0,y:1265.0)),outer_position_pixels:Some((x:2048.0,y:1228.0)),fullscreen:false,maximized:false,inner_size_points:Some((x:880.0,y:480.0)))";

    /// Read the window payload back as eframe 0.37 would.
    fn window_of(text: &str) -> WindowV37 {
        let kv: HashMap<String, String> = ron::from_str(text).expect("the migrated file is a RON map");
        ron::from_str(kv.get("window").expect("a window entry")).expect("0.37 WindowSettings shape")
    }

    fn migrated(window: &str) -> String {
        migrate_window(file(window, "1.0")).expect("migratable").expect("rewritten")
    }

    #[test]
    fn a_saved_layout_is_kept() {
        assert_eq!(check(&file(WINDOW, "1.0")), Ok(()));
        assert_eq!(check(&file(WINDOW, "0.375")), Ok(()));
    }

    #[test]
    fn values_that_abort_startup_are_rejected() {
        // Each of these aborted the app at launch before the check (dpi / wgpu assertions).
        for zoom in ["0.0", "NaN", "1e30", "-1.0"] {
            assert!(check(&file(WINDOW, zoom)).is_err(), "zoom {zoom}");
        }
        for size in ["(x:NaN,y:760.0)", "(x:1100.0,y:1e30)", "(x:inf,y:1.0)"] {
            let w = WINDOW.replace("(x:1100.0,y:760.0)", size);
            assert!(check(&file(&w, "1.0")).is_err(), "size {size}");
        }
    }

    #[test]
    fn an_old_size_and_maximized_state_survive() {
        let text = migrated(WINDOW_WITH_OUTER);
        let window = window_of(&text);
        assert_eq!(window.normal_geometry.inner_size_points.x, 880.0);
        assert_eq!(window.normal_geometry.inner_size_points.y, 480.0);
        assert!(!window.fullscreen);
        assert!(!window.maximized);
        let maximized = migrated(&WINDOW.replace("fullscreen:false", "fullscreen:true,maximized:true"));
        assert!(window_of(&maximized).fullscreen && window_of(&maximized).maximized);
    }

    #[test]
    fn a_missing_size_uses_the_default_opening_size() {
        // A window entry with a position but no size migrates and keeps the app's opening size.
        let window = window_of(&migrated("(inner_position_pixels:Some((x:616.0,y:430.0)),fullscreen:false,maximized:false)"));
        assert_eq!(window.normal_geometry.inner_size_points.x, DEFAULT_SIZE);
        assert_eq!(window.normal_geometry.inner_size_points.y, DEFAULT_HEIGHT);
        // A window entry with no geometry fields at all is left alone: it is not a 0.36 window.
        assert_eq!(migrate_window(file("(fullscreen:false,maximized:false)", "1.0")), Ok(None));
        // So is a file with no window-like fields at all.
        assert_eq!(migrate_window(file("(something:1)", "1.0")), Ok(None));
    }

    #[test]
    fn the_old_position_is_kept_only_on_windows() {
        let text = migrated(WINDOW_WITH_OUTER);
        let pos = window_of(&text).normal_geometry.outer_position_pixels;
        if cfg!(target_os = "windows") {
            let pos = pos.expect("the Windows physical outer position survives");
            assert_eq!((pos.x, pos.y), (2048.0, 1228.0));
            assert_eq!(window_of(&text).normal_geometry.scale_factor, 1.0);
        } else {
            assert!(pos.is_none(), "an old macOS/Linux position is not a restorable outer position");
        }
    }

    #[test]
    fn a_corrupt_old_size_is_not_rewritten() {
        let bad = WINDOW.replace("(x:1100.0,y:760.0)", "(x:1100.0,y:0.0)");
        assert!(migrate_window(file(&bad, "1.0")).is_err());
        let huge = WINDOW.replace("(x:1100.0,y:760.0)", "(x:1100.0,y:9000000.0)");
        assert!(migrate_window(file(&huge, "1.0")).is_err());
    }

    #[test]
    fn an_already_migrated_file_is_left_alone() {
        let text = migrated(WINDOW);
        assert_eq!(migrate_window(text), Ok(None));
        // A 0.36 build accepts the entry, but its missing flat geometry defaults to None.
        // It cannot recover position or size from the nested 0.37 geometry.
        let kv: HashMap<String, String> = ron::from_str(&migrated(WINDOW)).unwrap();
        let old: OldWindow = ron::from_str(kv.get("window").unwrap()).unwrap();
        assert!(old.inner_position_pixels.is_none() && old.outer_position_pixels.is_none());
        assert!(old.inner_size_points.is_none(), "documented rollback loss of saved geometry");
        // Restoring the original backup restores both the old position and size.
        let restored: OldWindow = ron::from_str(WINDOW).unwrap();
        let position = restored.inner_position_pixels.unwrap();
        let size = restored.inner_size_points.unwrap();
        assert_eq!((position.x, position.y), (616.0, 430.0));
        assert_eq!((size.x, size.y), (1100.0, 760.0));
    }

    #[test]
    fn a_saved_window_position_is_detected() {
        // A 0.36 flat entry is not what eframe 0.37 restores (it would open at the OS default);
        // `sanitize` rewrites it before eframe runs, and the migrated nested value is detected.
        assert!(!saved_position(&file(WINDOW_WITH_OUTER, "1.0")), "an unmigrated flat entry is not a restorable position");
        // An old macOS inner position is not an outer position either.
        assert!(!saved_position(&file(WINDOW, "1.0")));
        let no_position =
            "(normal_geometry:(outer_position_pixels:None,scale_factor:1.0,inner_size_points:(x:880.0,y:480.0)),fullscreen:false,maximized:false)";
        assert!(!saved_position(&file(no_position, "1.0")));
        // The migrated nested geometry is what a 0.37 save holds (on Windows only, from the
        // migration; a later 0.37 save on macOS/Linux has one too and eframe restores it).
        let text = migrated(WINDOW_WITH_OUTER);
        assert_eq!(saved_position(&text), cfg!(target_os = "windows"));
        assert!(!saved_position(&format!("{{\"egui\":{:?}}}", "()")), "no window entry");
        for text in ["", "not ron {{{", "{\"window\":\"(((\"}"] {
            assert!(!saved_position(text), "{text:?}");
        }
        assert!(!super::has_saved_position(None));
        assert!(!super::has_saved_position(Some(&std::env::temp_dir().join("photocraft-missing-ui.ron"))));
    }

    #[test]
    fn the_text_scan_agrees_with_the_value_walk() {
        use super::{floats_sane, scan};
        // Floats inside nested strings were never checked (egui's persisted values are RON text).
        assert_eq!(scan("(data:([(1,(ron:\"(x:1e30)\"))]))"), Some(Ok(None)));
        // Integers (egui's hashed ids) are not coordinates; hex and suffixes are read as such.
        assert_eq!(scan("(id:18446744073709551615,h:0xFF,z:(zoom_factor:1.5))"), Some(Ok(Some(1.5))));
        assert_eq!(scan("(zoom_factor:2.0,later:(zoom_factor:0.0))"), Some(Ok(Some(2.0))), "the first one, options'");
        assert!(scan("(a:-inf)").unwrap().is_err() && scan("(a:-1.5e7)").unwrap().is_err() && scan("(a:2.5e5)").unwrap().is_ok());
        assert_eq!(scan("(a:r#\"NaN ) ( \"#,b:'(')"), Some(Ok(None)), "raw strings and chars are skipped");
        assert_eq!(scan("((("), None, "unbalanced");
        assert_eq!(scan("(a:\"open)"), None, "unterminated string");
        assert!(scan(&("(".repeat(70) + &")".repeat(70))).unwrap().is_err(), "nested too deeply");
        // Agrees with the full parse on a realistic egui entry.
        let egui = "(options:(theme_preference:System,zoom_factor:1.0,tessellation_options:(feathering:true,feathering_size_in_pixels:1.0)),data:([(442875985498941510,(type_id:(4020315281066545825),ron:\"(x:1621.0,y:502.0)\",generation:1))]),areas:{((18446744073709551615)):(areas:{(10191671437090508800):(pivot_pos:Some((x:2310.0,y:721.0)),pivot:((Min,Min)),interactable:false)})})";
        let v: ron::Value = ron::from_str(egui).unwrap();
        assert_eq!(floats_sane(&v, 0), Ok(()));
        assert_eq!(scan(egui), Some(Ok(Some(1.0))));
    }

    #[test]
    fn a_big_egui_memory_checks_quickly() {
        // 3000 closed popups' areas, as a user's ui.ron had grown to (842 KB; 4 s to parse untyped).
        let areas: Vec<String> = (0..3000u64).map(|i| format!("({}):(pivot_pos:Some((x:{}.5,y:721.0)),pivot:((Min,Min)),interactable:false)", i * 7919, i)).collect();
        let egui = format!("(options:(zoom_factor:1.0),areas:{{((18446744073709551615)):(areas:{{{}}})}})", areas.join(","));
        let text = format!("{{\"egui\":{egui:?}}}");
        let t = std::time::Instant::now();
        assert_eq!(check(&text), Ok(()));
        assert!(t.elapsed() < std::time::Duration::from_millis(500), "{:?}", t.elapsed());
    }

    #[test]
    fn unreadable_files_are_left_to_eframe() {
        // eframe ignores files and entries it can't parse, so they can't crash it.
        for text in ["", "not ron {{{", "{\"window\":\"(((\"}", "[1,2,3]"] {
            assert_eq!(check(text), Ok(()), "{text:?}");
        }
        let deep = format!("{{\"egui\":{:?}}}", "[".repeat(500) + &"]".repeat(500));
        let _ = check(&deep);
    }

    #[test]
    fn sanitize_removes_only_a_bad_file_and_migrates_a_good_one() {
        let dir = std::env::temp_dir().join(format!("photocraft-ui-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.ron");
        let bad = dir.join("bad.ron");
        std::fs::write(&good, file(WINDOW, "1.0")).unwrap();
        std::fs::write(&bad, file(WINDOW, "0.0")).unwrap();
        super::sanitize(Some(&good));
        super::sanitize(Some(&bad));
        super::sanitize(Some(&dir.join("missing.ron")));
        super::sanitize(None);
        assert!(!bad.exists());
        assert!(good.exists());
        // Migrated in place, with the one-time pre-0.37 backup beside it.
        let text = std::fs::read_to_string(&good).unwrap();
        assert!(window_of(&text).normal_geometry.inner_size_points.x == 1100.0);
        let backup = backup_path(&good);
        assert!(backup.exists(), "the pre-0.37 layout is kept for rollback");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), file(WINDOW, "1.0"));
        // A second run leaves both untouched.
        super::sanitize(Some(&good));
        assert_eq!(std::fs::read_to_string(&good).unwrap(), text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_backup_is_not_overwritten_by_a_later_migration() {
        let dir = std::env::temp_dir().join(format!("photocraft-ui-state-backup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ui.ron");
        std::fs::write(&path, file(WINDOW, "1.0")).unwrap();
        super::sanitize(Some(&path));
        let backup = backup_path(&path);
        let original = std::fs::read_to_string(&backup).unwrap();
        // Pretend a 0.36 run wrote its old schema again; the backup must stay the first one.
        std::fs::write(&path, file(&WINDOW.replace("1100.0", "1111.0"), "1.0")).unwrap();
        super::sanitize(Some(&path));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
        assert!(std::fs::read_to_string(&path).unwrap().contains("1111"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The 0.36 flat schema, kept to prove the new nested value is a one-way migration.
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    #[allow(dead_code)]
    struct OldWindow {
        inner_position_pixels: Option<Point>,
        outer_position_pixels: Option<Point>,
        fullscreen: bool,
        maximized: bool,
        inner_size_points: Option<Point>,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Point {
        x: f32,
        y: f32,
    }

    #[test]
    fn the_backup_path_sits_beside_ui_ron() {
        assert_eq!(backup_path(&PathBuf::from("/tmp/x/ui.ron")), PathBuf::from("/tmp/x/ui.ron.pre-0.37"));
    }
}
