//! Creative profiles (`crs:Look`) in settings files. A preset either carries the profile's tables
//! in its Look (`crs:LookTable` and `crs:RGBTable` name `crs:Table_…` attributes beside them) or,
//! as Camera Raw writes its own profiles into presets, only names it (`crs:Stubbed`): Camera Raw
//! then takes the tables from the profile's file among its settings, and so does PhotoCraft
//! ([`installed`]: Camera Raw's settings folders on this computer, matched by UUID, else by file
//! name).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use photocraft_algo::camera_raw::{Profile, Table};

use super::{CRS_NS, MAX_XMP_BYTES, RDF_NS, number};

/// A `crs:` attribute of `node`.
fn attr<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes().find(|a| a.namespace() == Some(CRS_NS) && a.name() == name).map(|a| a.value())
}

/// The tables `desc` carries (its `LookTable` and `RGBTable`, and `RGBTableAmount`).
fn tables(desc: roxmltree::Node<'_, '_>) -> (Option<Table>, Option<Table>, f32) {
    let table = |key: &str| attr(desc, key).and_then(|k| attr(desc, &format!("Table_{}", k.trim()))).map(Table::new);
    let amount = attr(desc, "RGBTableAmount").and_then(number).unwrap_or(1.0);
    (table("LookTable"), table("RGBTable"), amount)
}

/// The settings description of a `crs:Look` (or of a profile file): the first `rdf:Description`.
fn description<'a, 'i>(node: roxmltree::Node<'a, 'i>) -> Option<roxmltree::Node<'a, 'i>> {
    node.descendants().find(|n| n.is_element() && n.tag_name().namespace() == Some(RDF_NS) && n.tag_name().name() == "Description")
}

/// The profile a `crs:Look` element names, at its Amount, with its tables: its own, else those of
/// the profile file `find(uuid, name)` returns. `None` when no table is found.
pub(super) fn from_look(look: roxmltree::Node<'_, '_>, name: Option<String>, find: &dyn Fn(&str, &str) -> Option<Vec<u8>>) -> Option<Profile> {
    let desc = description(look)?;
    let name = name.unwrap_or_default();
    let uuid = attr(desc, "UUID").unwrap_or("").trim().to_string();
    let amount = attr(desc, "Amount").and_then(number).unwrap_or(1.0).clamp(0.0, 2.0);
    let (mut look_table, mut rgb_table, mut rgb_table_amount) = tables(desc);
    if look_table.is_none() && rgb_table.is_none() {
        (look_table, rgb_table, rgb_table_amount) = profile_file(&find(&uuid, &name)?)?;
    }
    let p = Profile { name, uuid, amount, look_table, rgb_table, rgb_table_amount };
    p.validate().ok()?;
    Some(p)
}

/// The tables of a profile file (`crs:PresetType="Look"`).
pub(super) fn profile_file(bytes: &[u8]) -> Option<(Option<Table>, Option<Table>, f32)> {
    if bytes.len() > MAX_XMP_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?.trim_start_matches('\u{feff}');
    let doc = crate::xml::parse(text, roxmltree::ParsingOptions::default()).ok()?;
    let desc = description(doc.root())?;
    let found = tables(desc);
    (found.0.is_some() || found.1.is_some()).then_some(found)
}

/// A profile file among Camera Raw's settings.
struct Installed {
    path: PathBuf,
    uuid: String,
}

/// Most files looked at, and the deepest folder, when listing the settings folders.
const MAX_FILES: usize = 20_000;
const MAX_DEPTH: usize = 8;
/// How much of a file is read to tell a profile and its UUID (both come first).
const HEAD_BYTES: usize = 4096;

/// Camera Raw's settings folders on this computer: the shared one (Adobe's own profiles) and the
/// user's.
fn roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    if cfg!(windows) {
        out.extend(env("PROGRAMDATA").map(|p| p.join("Adobe").join("CameraRaw").join("Settings")));
        out.extend(env("APPDATA").map(|p| p.join("Adobe").join("CameraRaw").join("Settings")));
    } else if cfg!(target_os = "macos") {
        out.push(PathBuf::from("/Library/Application Support/Adobe/CameraRaw/Settings"));
        out.extend(env("HOME").map(|p| p.join("Library/Application Support/Adobe/CameraRaw/Settings")));
    }
    out
}

/// The `crs:UUID` of a profile file from its first bytes; `None` for anything else.
fn head_uuid(head: &str) -> Option<String> {
    if !head.contains("PresetType=\"Look\"") {
        return None;
    }
    let at = head.find("crs:UUID=\"")? + "crs:UUID=\"".len();
    Some(head[at..].split('"').next()?.trim().to_string())
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<Installed>, seen: &mut usize) {
    use std::io::Read;
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if *seen >= MAX_FILES {
            return;
        }
        *seen += 1;
        let path = e.path();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() && depth < MAX_DEPTH {
            walk(&path, depth + 1, out, seen);
        } else if kind.is_file() && path.extension().is_some_and(|x| x.eq_ignore_ascii_case("xmp")) {
            let mut head = Vec::with_capacity(HEAD_BYTES);
            if std::fs::File::open(&path).and_then(|f| f.take(HEAD_BYTES as u64).read_to_end(&mut head)).is_ok()
                && let Some(uuid) = head_uuid(&String::from_utf8_lossy(&head))
            {
                out.push(Installed { path, uuid });
            }
        }
    }
}

/// The profile files in Camera Raw's settings folders, listed once per run.
fn list() -> &'static [Installed] {
    static LIST: OnceLock<Vec<Installed>> = OnceLock::new();
    LIST.get_or_init(|| {
        let (mut out, mut seen) = (Vec::new(), 0);
        for root in roots() {
            walk(&root, 0, &mut out, &mut seen);
        }
        out
    })
}

/// The installed profile file with `uuid`, else the one named `name` (its file name); its bytes.
pub(crate) fn installed(uuid: &str, name: &str) -> Option<Vec<u8>> {
    let list = list();
    let by_uuid = (!uuid.is_empty()).then(|| list.iter().find(|p| p.uuid.eq_ignore_ascii_case(uuid))).flatten();
    let by_name =
        || (!name.is_empty()).then(|| list.iter().find(|p| p.path.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(name)))).flatten();
    let p = by_uuid.or_else(by_name)?;
    let meta = std::fs::metadata(&p.path).ok()?;
    if meta.len() > MAX_XMP_BYTES as u64 {
        return None;
    }
    std::fs::read(&p.path).ok()
}
