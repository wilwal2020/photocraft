//! Where the libraries persist: a directory next to the preferences (desktop:
//! `<config dir>/Libraries`), through the preset store's backends (atomic writes, generated file
//! names only):
//!
//! - `libraries.json`: the manifest. Library order and names, each item's id, name and kind, a
//!   colour item's colour, the current library and the next item id. Renaming, moving or deleting
//!   rewrites only this file;
//! - `item-<id>.pcraft`: a layer, image or style item's scratch document, as a native `.pcraft`
//!   bundle with its thumbnail embedded. Written once when the item is added, deleted with it;
//! - `item-<id>.png`: the item's thumbnail on its own, so opening the store never reads an item's
//!   pixels (the bundle's embedded thumbnail stands in when it is missing).
//!
//! The store opens on a background thread at start ([`open_dir_async`]) and syncs after every
//! command that changed the libraries ([`Session::sync_library_store`]). A manifest that exists
//! but can't be read is left on disk untouched and never overwritten; item files the manifest no
//! longer names (a crash between writes) are deleted at the next sync. Every problem is a warning,
//! never a panic.

use std::collections::HashSet;
use std::sync::Arc;

use photocraft_doc::Document;
use photocraft_raster::Rgba8Image;
use serde::{Deserialize, Serialize};

use super::{Item, ItemKind, Library, LibraryState, MAX_ITEMS, MAX_LIBRARIES};
use crate::Session;
use crate::preset_store::{PresetBackend, missing_file};

/// The manifest.
pub const MANIFEST_FILE: &str = "libraries.json";
/// Largest manifest read or written.
pub const MAX_MANIFEST_BYTES: u64 = 16 << 20;
/// Largest item bundle read or written.
pub const MAX_ITEM_BYTES: u64 = 2 << 30;
/// Largest thumbnail file read.
const MAX_THUMB_BYTES: u64 = 4 << 20;
/// Largest thumbnail side accepted from disk.
const MAX_THUMB_SIDE: u32 = 1024;
const FORMAT: &str = "photocraft-libraries";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    format: String,
    version: u32,
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    current: usize,
    #[serde(default)]
    libraries: Vec<StoredLibrary>,
}

#[derive(Serialize, Deserialize)]
struct StoredLibrary {
    name: String,
    #[serde(default)]
    items: Vec<StoredItem>,
}

#[derive(Serialize, Deserialize)]
struct StoredItem {
    id: u64,
    name: String,
    kind: ItemKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<[f32; 3]>,
}

/// An item's bundle (generated from its id, so an edited manifest can't name another file).
pub fn item_file(id: u64) -> String {
    format!("item-{id}.pcraft")
}

/// An item's thumbnail.
pub fn thumb_file(id: u64) -> String {
    format!("item-{id}.png")
}

/// The id in an item file name.
fn file_id(name: &str) -> Option<u64> {
    let stem = name.strip_prefix("item-")?;
    stem.strip_suffix(".pcraft").or_else(|| stem.strip_suffix(".png"))?.parse().ok()
}

/// The libraries' store attached to a session (see the module docs).
pub struct LibraryStore {
    backend: Box<dyn PresetBackend>,
    /// [`LibraryState::rev`] at the last sync.
    synced_rev: Option<u64>,
    /// Item ids whose bundle is on disk.
    written: HashSet<u64>,
    /// Items whose bundle couldn't be written: not retried, so one failure is one warning.
    failed: HashSet<u64>,
    /// The manifest exists but couldn't be read: it is never written.
    skipped: bool,
    warnings: Vec<String>,
}

/// A loaded store and what it held.
pub struct Opened {
    pub store: LibraryStore,
    pub libraries: Vec<Library>,
    pub current: usize,
    pub next_id: u64,
    pub warnings: Vec<String>,
}

fn decode_thumb(bytes: &[u8]) -> Option<Rgba8Image> {
    let img = photocraft_codecs::decode(bytes).ok()?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || w > MAX_THUMB_SIDE || h > MAX_THUMB_SIDE || img.layout() != photocraft_codecs::ChannelLayout::Rgba {
        return None;
    }
    let px = img.as_u8()?;
    (px.len() as u64 == u64::from(w) * u64::from(h) * 4).then(|| Rgba8Image { width: w, height: h, pixels: px.to_vec() })
}

fn encode_thumb(t: &Rgba8Image) -> Option<Vec<u8>> {
    let img = photocraft_codecs::Image::from_u8(t.width, t.height, photocraft_codecs::ChannelLayout::Rgba, t.pixels.clone()).ok()?;
    photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).ok()
}

/// An item's thumbnail: its PNG, or the one its bundle embeds.
fn read_thumb(backend: &dyn PresetBackend, id: u64) -> Option<Rgba8Image> {
    if let Some(t) = backend.read(&thumb_file(id), MAX_THUMB_BYTES).ok().and_then(|b| decode_thumb(&b)) {
        return Some(t);
    }
    let bundle = backend.read(&item_file(id), MAX_ITEM_BYTES).ok()?;
    decode_thumb(&photocraft_format::read_thumbnail(&bundle).ok()??)
}

fn valid_color(c: [f32; 3]) -> Option<[f32; 4]> {
    c.iter().all(|v| v.is_finite()).then(|| [c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0), 1.0])
}

/// Load a store (any thread; the desktop app does this in the background at start).
pub fn open(backend: Box<dyn PresetBackend>) -> Opened {
    let mut warnings = Vec::new();
    let files = backend.list().unwrap_or_else(|e| {
        warnings.push(format!("libraries: {e}"));
        Vec::new()
    });
    let bundles: HashSet<u64> = files.iter().filter(|(n, _)| n.ends_with(".pcraft")).filter_map(|(n, _)| file_id(n)).collect();
    let max_file_id = files.iter().filter_map(|(n, _)| file_id(n)).max().map_or(0, |m| m.saturating_add(1));
    let mut store = LibraryStore { backend, synced_rev: None, written: HashSet::new(), failed: HashSet::new(), skipped: false, warnings: Vec::new() };
    let manifest = match store.backend.read(MANIFEST_FILE, MAX_MANIFEST_BYTES) {
        Ok(bytes) => match serde_json::from_slice::<Manifest>(&bytes) {
            Ok(m) if m.format == FORMAT && m.version == 1 => Some(m),
            Ok(m) => {
                warnings.push(format!("libraries: {MANIFEST_FILE} skipped: unsupported format {} version {}", m.format, m.version));
                None
            }
            Err(e) => {
                warnings.push(format!("libraries: {MANIFEST_FILE} skipped: {e}"));
                None
            }
        },
        Err(e) if missing_file(&e) => Some(Manifest { format: FORMAT.into(), version: 1, next_id: 1, current: 0, libraries: Vec::new() }),
        Err(e) => {
            warnings.push(format!("libraries: {MANIFEST_FILE} skipped: {e}"));
            None
        }
    };
    let Some(manifest) = manifest else {
        // Left untouched (and never written) so a fixed file still loads.
        store.skipped = true;
        return Opened { store, libraries: Vec::new(), current: 0, next_id: max_file_id.max(1), warnings };
    };
    // Every bundle on disk is the store's: the ones the manifest doesn't name are deleted by
    // the first sync (left by a crash between writing a bundle and the manifest).
    store.written = bundles.clone();
    let mut seen = HashSet::new();
    let mut names = HashSet::new();
    let mut libraries = Vec::new();
    for lib in manifest.libraries.into_iter().take(MAX_LIBRARIES) {
        let name: String = lib.name.trim().chars().filter(|c| !c.is_control()).take(255).collect();
        if name.is_empty() || !names.insert(name.to_lowercase()) {
            warnings.push(format!("libraries: a library named \"{}\" was skipped (empty or duplicate name)", lib.name));
            continue;
        }
        let mut items = Vec::new();
        for it in lib.items.into_iter().take(MAX_ITEMS) {
            if !seen.insert(it.id) {
                warnings.push(format!("libraries: item {} (\"{}\") skipped: duplicate id", it.id, it.name));
                continue;
            }
            let item = match it.kind {
                ItemKind::Color => match it.color.and_then(valid_color) {
                    Some(c) => Item { id: it.id, name: it.name, kind: it.kind, color: Some(c), thumb: None, payload: None },
                    None => {
                        warnings.push(format!("libraries: colour \"{}\" skipped: no valid colour", it.name));
                        continue;
                    }
                },
                _ if !bundles.contains(&it.id) => {
                    warnings.push(format!("libraries: \"{}\" skipped: {} is missing", it.name, item_file(it.id)));
                    continue;
                }
                _ => {
                    let thumb = read_thumb(store.backend.as_ref(), it.id).map(Arc::new);
                    Item { id: it.id, name: it.name, kind: it.kind, color: None, thumb, payload: None }
                }
            };
            items.push(item);
        }
        libraries.push(Library { name, items });
    }
    let current = manifest.current.min(libraries.len().saturating_sub(1));
    let max_item = seen.iter().max().map_or(0, |m| m.saturating_add(1));
    let next_id = manifest.next_id.max(max_item).max(max_file_id).max(1);
    Opened { store, libraries, current, next_id, warnings }
}

/// Open a store directory (desktop). Never fails: problems become warnings.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_dir(root: impl Into<std::path::PathBuf>) -> Opened {
    open(Box::new(crate::preset_store::DirBackend::new(root)))
}

/// Open a store directory on a background thread; the receiver yields it once loaded.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_dir_async(root: std::path::PathBuf) -> std::sync::mpsc::Receiver<Opened> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("libraries".into()).spawn(move || {
        let _ = tx.send(open_dir(root));
    });
    if let Err(e) = spawned {
        // No thread: the libraries stay session-only.
        eprintln!("libraries: {e}");
    }
    rx
}

impl LibraryStore {
    /// Warnings from writes since the last call.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// A stored item's scratch document.
    pub(crate) fn load_item(&self, id: u64) -> Result<Document, String> {
        let bytes = self.backend.read(&item_file(id), MAX_ITEM_BYTES)?;
        let opts = photocraft_format::LoadOptions { preserve_ids: false, ..Default::default() };
        photocraft_format::load_from_bytes_with(&bytes, &opts).map_err(|e| format!("library item {id}: {e}"))
    }

    /// Write what changed since the last sync: new items' bundles, then the manifest, then the
    /// removal of items no longer listed.
    fn sync(&mut self, st: &LibraryState) {
        if self.synced_rev == Some(st.rev) {
            return;
        }
        self.synced_rev = Some(st.rev);
        if self.skipped {
            self.warnings.push(format!("Couldn't save the libraries: {MANIFEST_FILE} could not be read, so it was left unchanged"));
            return;
        }
        let items = || st.libraries.iter().flat_map(|l| l.items.iter());
        for item in items() {
            let Some(doc) = item.payload.as_ref().filter(|_| !self.written.contains(&item.id) && !self.failed.contains(&item.id)) else { continue };
            let opts = photocraft_format::SaveOptions { thumbnail: item.thumb.as_deref().cloned(), composite: None };
            let written = photocraft_format::save_to_bytes(doc, &opts).map_err(|e| e.to_string()).and_then(|bytes| {
                if bytes.len() as u64 > MAX_ITEM_BYTES {
                    return Err(format!("larger than {} MB", MAX_ITEM_BYTES >> 20));
                }
                self.backend.write(&item_file(item.id), &bytes)
            });
            match written {
                Ok(()) => {
                    self.written.insert(item.id);
                    // The bundle embeds it too: a thumbnail that fails to write is only slower to load.
                    if let Some(png) = item.thumb.as_deref().and_then(encode_thumb) {
                        let _ = self.backend.write(&thumb_file(item.id), &png);
                    }
                }
                Err(e) => {
                    self.failed.insert(item.id);
                    self.warnings.push(format!("Couldn't save library item \"{}\": {e}", item.name));
                }
            }
        }
        let saved = |i: &Item| i.kind == ItemKind::Color || self.written.contains(&i.id);
        let manifest = Manifest {
            format: FORMAT.into(),
            version: 1,
            next_id: st.next_id,
            current: st.current,
            libraries: st
                .libraries
                .iter()
                .map(|l| StoredLibrary {
                    name: l.name.clone(),
                    items: l
                        .items
                        .iter()
                        .filter(|i| saved(i))
                        .map(|i| StoredItem { id: i.id, name: i.name.clone(), kind: i.kind, color: i.color.map(|c| [c[0], c[1], c[2]]) })
                        .collect(),
                })
                .collect(),
        };
        let bytes = match serde_json::to_vec_pretty(&manifest) {
            Ok(b) if b.len() as u64 <= MAX_MANIFEST_BYTES => b,
            Ok(_) => {
                self.warnings.push("Couldn't save the libraries: the list is too large".into());
                return;
            }
            Err(e) => {
                self.warnings.push(format!("Couldn't save the libraries: {e}"));
                return;
            }
        };
        if let Err(e) = self.backend.write(MANIFEST_FILE, &bytes) {
            self.warnings.push(format!("Couldn't save the libraries: {e}"));
            // Retried after the next change.
            self.synced_rev = None;
            return;
        }
        // Only now that the manifest no longer names them.
        let present: HashSet<u64> = items().map(|i| i.id).collect();
        let gone: Vec<u64> = self.written.iter().copied().filter(|id| !present.contains(id)).collect();
        for id in gone {
            match self.backend.remove(&item_file(id)) {
                Ok(()) => {
                    self.written.remove(&id);
                    let _ = self.backend.remove(&thumb_file(id));
                }
                Err(e) => self.warnings.push(format!("Couldn't delete library item file: {e}")),
            }
        }
    }
}

impl Session {
    /// Attach a loaded library store: its libraries come first, and libraries made before it
    /// finished loading are merged in (by name; their items get fresh ids). Returns the load
    /// warnings.
    pub fn attach_library_store(&mut self, opened: Opened) -> Vec<String> {
        let Opened { store, libraries, current, next_id, mut warnings } = opened;
        let st = &mut self.libraries;
        let made = std::mem::take(&mut st.libraries);
        let had_current = made.get(st.current).map(|l| l.name.clone());
        st.libraries = libraries;
        st.next_id = st.next_id.max(next_id);
        st.current = current;
        for mut lib in made {
            for item in &mut lib.items {
                item.id = st.take_id();
            }
            match st.libraries.iter().position(|l| l.name.eq_ignore_ascii_case(&lib.name)) {
                Some(i) => {
                    if let Some(have) = st.libraries.get_mut(i) {
                        have.items.extend(lib.items);
                    }
                }
                None if st.libraries.len() < MAX_LIBRARIES => st.libraries.push(lib),
                None => warnings.push(format!("libraries: \"{}\" was not kept ({MAX_LIBRARIES} libraries)", lib.name)),
            }
        }
        if let Some(name) = had_current
            && let Some(i) = st.libraries.iter().position(|l| l.name.eq_ignore_ascii_case(&name))
        {
            st.current = i;
        }
        st.store = Some(store);
        st.changed();
        self.sync_library_store();
        warnings.extend(self.libraries.take_warnings());
        warnings
    }

    /// Write pending library changes to the attached store (no-op without one).
    pub fn sync_library_store(&mut self) {
        if let Some(mut store) = self.libraries.store.take() {
            store.sync(&self.libraries);
            self.libraries.store = Some(store);
        }
    }
}
