//! Camera Raw presets on disk: PhotoCraft's own folder, which Create Preset writes to, and the
//! folders Photoshop's Camera Raw keeps presets in (the user's and Adobe's built-in ones), read as
//! they are and never written.
//!
//! A preset is a Camera Raw settings file (`.xmp`) with a name and a group (see
//! [`photocraft_io::camera_raw_xmp::preset_info`]); profiles (`crs:PresetType="Look"`) live in the
//! same folders and are skipped. Folders are walked to [`MAX_DEPTH`] levels, files over
//! [`MAX_PRESET_BYTES`] and anything past [`MAX_PRESETS`] are skipped, and a file that can't be
//! read or isn't a preset is left out: a scan never fails. There is no file system on the web,
//! where a scan lists nothing and writing is an error.

use photocraft_io::camera_raw_xmp::preset_info;
use serde::Serialize;
use std::sync::Arc;

/// Largest preset file read (presets are a few kilobytes; profiles with colour tables, which
/// aren't presets, run to a few hundred).
pub const MAX_PRESET_BYTES: u64 = 1 << 20;
/// Most presets one scan lists.
pub const MAX_PRESETS: usize = 5000;
/// How deep below a preset folder a scan looks (preset packs nest their folders).
pub const MAX_DEPTH: usize = 8;

/// Where a preset was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// PhotoCraft's own presets folder.
    Own,
    /// Photoshop's Camera Raw presets (the user's).
    Photoshop,
    /// Adobe's presets installed with Photoshop.
    Adobe,
}

/// A preset file found by [`scan`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PresetFile {
    pub name: String,
    /// The group it is listed under: its `crs:Group`, else its folder below the preset folder.
    pub group: String,
    pub path: String,
    pub source: Source,
    pub supports_amount: bool,
    /// The file's contents, applied with [`photocraft_io::camera_raw_xmp::load`].
    #[serde(skip)]
    pub bytes: Arc<Vec<u8>>,
}

/// The presets in `dirs` (each with where it is), sorted by source, group and name.
#[cfg(not(target_arch = "wasm32"))]
pub fn scan(dirs: &[(String, Source)]) -> Vec<PresetFile> {
    let mut out = Vec::new();
    for (dir, source) in dirs {
        let root = std::path::Path::new(dir.trim());
        if !dir.trim().is_empty() {
            walk(root, root, *source, 0, &mut out);
        }
    }
    out.sort_by_key(|a| (a.source, a.group.to_lowercase(), a.name.to_lowercase()));
    out
}

/// No file system on the web.
#[cfg(target_arch = "wasm32")]
pub fn scan(_: &[(String, Source)]) -> Vec<PresetFile> {
    Vec::new()
}

#[cfg(not(target_arch = "wasm32"))]
fn walk(root: &std::path::Path, dir: &std::path::Path, source: Source, depth: usize, out: &mut Vec<PresetFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() >= MAX_PRESETS {
            return;
        }
        let Ok(name) = e.file_name().into_string() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.is_dir() {
            if depth < MAX_DEPTH {
                walk(root, &path, source, depth + 1, out);
            }
            continue;
        }
        let Some((stem, ext)) = name.rsplit_once('.') else { continue };
        if !ext.eq_ignore_ascii_case("xmp") || !meta.is_file() || meta.len() > MAX_PRESET_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(info) = preset_info(&bytes) else { continue };
        if info.kind.eq_ignore_ascii_case("look") || !String::from_utf8_lossy(&bytes).contains("HasSettings") {
            continue;
        }
        let folder = path.parent().and_then(|p| p.strip_prefix(root).ok()).and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("").to_string();
        let Some(path) = path.to_str() else { continue };
        out.push(PresetFile {
            name: if info.name.trim().is_empty() { stem.to_string() } else { info.name },
            group: if info.group.trim().is_empty() { folder } else { info.group },
            path: path.to_string(),
            source,
            supports_amount: info.supports_amount,
            bytes: Arc::new(bytes),
        });
    }
}

/// `name` as a file or folder name: characters file systems refuse replaced, at most 100
/// characters, never empty or only dots.
pub fn file_name(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c }).take(100).collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim().to_string();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') { "Preset".to_string() } else { cleaned }
}

/// Writes the preset `xmp` named `name` into `group`'s folder under `dir` (created as needed),
/// replacing a preset of that name there. Returns its path.
#[cfg(not(target_arch = "wasm32"))]
pub fn write(dir: &str, group: &str, name: &str, xmp: &str) -> Result<String, String> {
    if dir.trim().is_empty() {
        return Err("no presets folder".into());
    }
    let mut folder = std::path::PathBuf::from(dir.trim());
    if !group.trim().is_empty() {
        folder.push(file_name(group));
    }
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(format!("{}.xmp", file_name(name)));
    photocraft_format::atomic_write(&path, xmp.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    path.to_str().map(str::to_string).ok_or_else(|| "the preset's path isn't text".to_string())
}

#[cfg(target_arch = "wasm32")]
pub fn write(_: &str, _: &str, _: &str, _: &str) -> Result<String, String> {
    Err("presets can't be saved here".into())
}

/// Deletes the preset file at `path`, which must lie inside `dir` (PhotoCraft's own folder:
/// Photoshop's presets are never touched).
#[cfg(not(target_arch = "wasm32"))]
pub fn delete(dir: &str, path: &str) -> Result<(), String> {
    let file = inside(dir, path)?;
    std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(target_arch = "wasm32")]
pub fn delete(_: &str, _: &str) -> Result<(), String> {
    Err("presets can't be deleted here".into())
}

/// Renames the preset file at `path` (inside `dir`): its `crs:Name` and its file name. Returns
/// the new path.
#[cfg(not(target_arch = "wasm32"))]
pub fn rename(dir: &str, path: &str, name: &str) -> Result<String, String> {
    let file = inside(dir, path)?;
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let text = String::from_utf8(bytes).map_err(|_| "the preset isn't text".to_string())?;
    let renamed = with_name(&text, name).ok_or("the preset has no name to change")?;
    let target = file.with_file_name(format!("{}.xmp", file_name(name)));
    photocraft_format::atomic_write(&target, renamed.as_bytes()).map_err(|e| format!("{}: {e}", target.display()))?;
    if target != file {
        std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    target.to_str().map(str::to_string).ok_or_else(|| "the preset's path isn't text".to_string())
}

#[cfg(target_arch = "wasm32")]
pub fn rename(_: &str, _: &str, _: &str) -> Result<String, String> {
    Err("presets can't be renamed here".into())
}

/// `path` resolved, if it is a file inside `dir`.
#[cfg(not(target_arch = "wasm32"))]
fn inside(dir: &str, path: &str) -> Result<std::path::PathBuf, String> {
    let root = std::fs::canonicalize(dir.trim()).map_err(|e| format!("{dir}: {e}"))?;
    let file = std::fs::canonicalize(path).map_err(|e| format!("{path}: {e}"))?;
    if !file.starts_with(&root) || !file.is_file() {
        return Err("only presets in PhotoCraft's presets folder can be changed".into());
    }
    Ok(file)
}

/// `xmp` with its `crs:Name` text replaced by `name` (the layout [`photocraft_io::camera_raw_xmp::save_preset`]
/// writes: the name's first `rdf:li`).
fn with_name(xmp: &str, name: &str) -> Option<String> {
    let start = xmp.find("<crs:Name>")?;
    let li = start + xmp.get(start..)?.find("<rdf:li")?;
    let open = li + xmp.get(li..)?.find('>')? + 1;
    let close = open + xmp.get(open..)?.find("</rdf:li>")?;
    let escaped = name.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    Some(format!("{}{}{}", xmp.get(..open)?, escaped, xmp.get(close..)?))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use photocraft_algo::camera_raw::CameraRaw;
    use photocraft_io::camera_raw_xmp::{Section, load, save_preset};

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("photocraft-cr-presets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn presets_are_found_in_nested_folders_and_profiles_are_skipped() {
        let own = temp("scan-own");
        let ps = temp("scan-ps");
        let p = CameraRaw { contrast: 20.0, ..CameraRaw::default() };
        write(own.to_str().unwrap(), "Mine", "Crisp", &save_preset(&p, "Crisp", "Mine", "1", &[Section::Light])).unwrap();
        let nested = ps.join("Pack").join("Pack");
        std::fs::create_dir_all(&nested).unwrap();
        // A preset without a group is listed under its folder; one without a name by its file.
        let bare = save_preset(&p, "", "", "2", &[Section::Light]);
        std::fs::write(nested.join("Film 01.xmp"), &bare).unwrap();
        std::fs::write(ps.join("Look.xmp"), bare.replace("PresetType=\"Normal\"", "PresetType=\"Look\"")).unwrap();
        std::fs::write(ps.join("broken.xmp"), b"<nope").unwrap();
        std::fs::write(ps.join("Index_1.dat"), b"x").unwrap();
        std::fs::write(ps.join(".hidden.xmp"), &bare).unwrap();
        let found = scan(&[(own.to_str().unwrap().to_string(), Source::Own), (ps.to_str().unwrap().to_string(), Source::Photoshop), ("".into(), Source::Adobe)]);
        let names: Vec<(&str, &str, Source)> = found.iter().map(|f| (f.name.as_str(), f.group.as_str(), f.source)).collect();
        assert_eq!(names, vec![("Crisp", "Mine", Source::Own), ("Film 01", "Pack", Source::Photoshop)]);
        let mut applied = CameraRaw::default();
        load(&mut applied, &found[0].bytes).unwrap();
        assert_eq!(applied.contrast, 20.0);
        assert!(scan(&[("Z:/no/such/folder".into(), Source::Photoshop)]).is_empty());
        let _ = (std::fs::remove_dir_all(own), std::fs::remove_dir_all(ps));
    }

    #[test]
    fn only_own_presets_are_renamed_or_deleted() {
        let own = temp("edit-own");
        let other = temp("edit-other");
        let xmp = save_preset(&CameraRaw::default(), "Old", "G", "3", &[Section::Light]);
        let path = write(own.to_str().unwrap(), "G", "Old", &xmp).unwrap();
        let renamed = rename(own.to_str().unwrap(), &path, "New: <one>").unwrap();
        assert!(renamed.ends_with("New_ _one_.xmp"), "{renamed}");
        let info = preset_info(&std::fs::read(&renamed).unwrap()).unwrap();
        assert_eq!(info.name, "New: <one>");
        assert!(!std::path::Path::new(&path).exists());
        let foreign = other.join("x.xmp");
        std::fs::write(&foreign, &xmp).unwrap();
        assert!(delete(own.to_str().unwrap(), foreign.to_str().unwrap()).is_err(), "outside the own folder");
        assert!(foreign.exists());
        delete(own.to_str().unwrap(), &renamed).unwrap();
        assert!(!std::path::Path::new(&renamed).exists());
        assert!(write("", "G", "x", &xmp).is_err());
        assert_eq!(file_name("  ..  "), "Preset");
        assert_eq!(file_name("a/b\\c"), "a_b_c");
        assert!(with_name("<x/>", "n").is_none());
        let _ = (std::fs::remove_dir_all(own), std::fs::remove_dir_all(other));
    }
}
