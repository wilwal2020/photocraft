//! Window › Libraries: named libraries of reusable items, as Photoshop's Libraries panel keeps
//! them. An item is saved from one document and used in any other:
//!
//! - **layers**: the selected layers whole (masks, effects, blending, a group's children).
//!   Using one inserts copies above the active layer, centred on the canvas or on a point;
//! - **image**: an image file, or the selected pixels of the active layer (or of the merged
//!   document). Using one adds it as a pixel layer, the same way;
//! - **style**: a layer's effects and blending options. Using one sets them on the selected layers
//!   (Photoshop's Styles panel behaviour: `"add": true` adds to their effects instead);
//! - **color**: a solid colour. Using one makes it the foreground colour.
//!
//! Layer, image and style items are scratch documents, the form the clipboard keeps whole layers
//! in: a document in the source's colour mode, depth and profile holding just the item's layers,
//! cropped to their content, with the patterns they use. They persist as `.pcraft` bundles
//! ([`store`]), next to the preferences on the desktop; headless sessions keep them in memory.
//!
//! Every action is a `library.*` command. Using an item that changes a document is one history
//! step there; editing the libraries themselves (create, rename, delete, add) is panel state, as
//! the Styles and Swatches panels' edits are, and is recorded in actions but not in a document's
//! history.

use std::sync::Arc;

use photocraft_doc::{Document, Layer, LayerId, Size};
use photocraft_geom::Rect;
use photocraft_raster::Rgba8Image;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::presets::styles::StylePreset;
use crate::{EngineError, Result, Session};

pub mod store;
#[cfg(test)]
mod tests;

/// Longest side of an item's thumbnail.
pub const THUMB_SIDE: u32 = 128;
/// Most libraries kept.
pub const MAX_LIBRARIES: usize = 1000;
/// Most items in one library.
pub const MAX_ITEMS: usize = 10_000;
/// Longest library or item name kept.
const MAX_NAME: usize = 255;
/// A layer or image item larger than this on either side keeps the source's canvas instead of
/// being cropped to its content (a layer far off the canvas).
const MAX_CROP_SIDE: u32 = 30_000;

/// What an item holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Layers,
    Image,
    Style,
    Color,
}

impl ItemKind {
    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Layers => "layers",
            ItemKind::Image => "image",
            ItemKind::Style => "style",
            ItemKind::Color => "color",
        }
    }
}

/// One library item.
#[derive(Clone, Debug)]
pub struct Item {
    /// Unique among every library's items, and never reused (it names the item's files).
    pub id: u64,
    pub name: String,
    pub kind: ItemKind,
    /// A colour item's colour (RGB, 0..1, alpha 1).
    pub color: Option<[f32; 4]>,
    /// The thumbnail the panel draws (`None` for colours, and for an item whose thumbnail could
    /// not be read).
    pub thumb: Option<Arc<Rgba8Image>>,
    /// The item's scratch document; `None` for colours, and for a stored item until it is used.
    pub(crate) payload: Option<Arc<Document>>,
}

/// A named library.
#[derive(Clone, Debug)]
pub struct Library {
    pub name: String,
    pub items: Vec<Item>,
}

/// The session's libraries (`Session::libraries`).
#[derive(Default)]
pub struct LibraryState {
    pub libraries: Vec<Library>,
    /// The library new items go to (index into `libraries`).
    pub current: usize,
    /// The next item id.
    pub(crate) next_id: u64,
    /// Bumped on every change (the panel keys its textures on it; the store syncs on it).
    pub rev: u64,
    /// The persistent store (desktop); `None` keeps the libraries in memory.
    pub(crate) store: Option<store::LibraryStore>,
}

impl LibraryState {
    fn changed(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    fn take_id(&mut self) -> u64 {
        let used = self.libraries.iter().flat_map(|l| l.items.iter().map(|i| i.id)).max().map_or(0, |m| m.saturating_add(1));
        let id = self.next_id.max(used).max(1);
        self.next_id = id.saturating_add(1);
        id
    }

    /// The library an item lives in and its index there.
    pub fn locate(&self, id: u64) -> Option<(usize, usize)> {
        self.libraries.iter().enumerate().find_map(|(li, l)| l.items.iter().position(|i| i.id == id).map(|ii| (li, ii)))
    }

    pub fn item(&self, id: u64) -> Option<&Item> {
        self.locate(id).and_then(|(li, ii)| self.libraries.get(li)?.items.get(ii))
    }

    fn unique_library_name(&self, base: &str, except: Option<usize>) -> String {
        let taken = |n: &str| self.libraries.iter().enumerate().any(|(i, l)| Some(i) != except && l.name.eq_ignore_ascii_case(n));
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|k| format!("{base} {k}")).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
    }

    /// Warnings from the store since the last call (the shell shows them in the status bar).
    pub fn take_warnings(&mut self) -> Vec<String> {
        self.store.as_mut().map(store::LibraryStore::take_warnings).unwrap_or_default()
    }

    /// Is a persistent store attached?
    pub fn persistent(&self) -> bool {
        self.store.is_some()
    }
}

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn clean_name(cmd: &str, name: &str) -> Result<String> {
    let n: String = name.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    if n.is_empty() { Err(bad(cmd, "the name is empty")) } else { Ok(n) }
}

fn name_param(cmd: &str, p: &Value) -> Result<Option<String>> {
    match p.get("name") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => clean_name(cmd, s).map(Some),
        Some(_) => Err(bad(cmd, "`name` must be a string")),
    }
}

/// The `library` param (a name, or an index) or the current library.
fn library_index(s: &Session, p: &Value, cmd: &str) -> Result<usize> {
    let st = &s.libraries;
    match p.get("library") {
        None | Some(Value::Null) => {
            if st.libraries.get(st.current).is_some() {
                Ok(st.current)
            } else {
                Err(bad(cmd, "there is no library (create one with library.new)"))
            }
        }
        Some(Value::String(n)) => {
            st.libraries.iter().position(|l| l.name.eq_ignore_ascii_case(n)).ok_or_else(|| bad(cmd, format!("no library \"{n}\" (see library.list)")))
        }
        Some(v) => v
            .as_u64()
            .and_then(|i| usize::try_from(i).ok())
            .filter(|i| *i < st.libraries.len())
            .ok_or_else(|| bad(cmd, "`library` must be a library name or index (see library.list)")),
    }
}

/// The target library for a new item: the named or current one, created ("My Library") when
/// there is none yet.
fn target_library(s: &mut Session, p: &Value, cmd: &str) -> Result<usize> {
    if p.get("library").is_none_or(Value::is_null) && s.libraries.libraries.is_empty() {
        s.libraries.libraries.push(Library { name: "My Library".into(), items: Vec::new() });
        s.libraries.current = 0;
    }
    let li = library_index(s, p, cmd)?;
    if s.libraries.libraries.get(li).is_some_and(|l| l.items.len() >= MAX_ITEMS) {
        return Err(bad(cmd, format!("the library is full ({MAX_ITEMS} items)")));
    }
    Ok(li)
}

fn item_id(s: &Session, p: &Value, cmd: &str) -> Result<u64> {
    let id = match p.get("item") {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(t)) => t.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| bad(cmd, "`item` must be an item id (see library.list)"))?;
    s.libraries.locate(id).map(|_| id).ok_or_else(|| bad(cmd, format!("no library item {id} (see library.list)")))
}

fn hex(c: [f32; 4]) -> String {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", q(c[0]), q(c[1]), q(c[2]))
}

fn item_json(i: &Item) -> Value {
    let mut v = json!({"id": i.id, "name": i.name, "kind": i.kind.label()});
    if let Some(c) = i.color {
        v["color"] = json!(hex(c));
    }
    if let Some(t) = &i.thumb {
        v["thumbnail"] = json!([t.width, t.height]);
    }
    v
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

fn has_library(s: &Session) -> std::result::Result<(), String> {
    if s.libraries.libraries.is_empty() { Err("there is no library".into()) } else { Ok(()) }
}

fn has_items(s: &Session) -> std::result::Result<(), String> {
    if s.libraries.libraries.iter().any(|l| !l.items.is_empty()) { Ok(()) } else { Err("the libraries are empty".into()) }
}

fn has_layer(s: &Session) -> std::result::Result<(), String> {
    s.active().and_then(|d| d.active_layer).map(|_| ()).ok_or_else(|| "no active layer".into())
}

// ------------------------------------------------------------------ libraries

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let st = &s.libraries;
    let libs: Vec<Value> = st.libraries.iter().map(|l| json!({"name": l.name, "items": l.items.iter().map(item_json).collect::<Vec<_>>()})).collect();
    let current = st.libraries.get(st.current).map(|l| l.name.clone());
    Ok(json!({"libraries": libs, "current": current, "persistent": st.persistent()}))
}

fn new_library(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.new";
    if s.libraries.libraries.len() >= MAX_LIBRARIES {
        return Err(bad(CMD, format!("there are already {MAX_LIBRARIES} libraries")));
    }
    let base = name_param(CMD, p)?.unwrap_or_else(|| "Library".into());
    let name = s.libraries.unique_library_name(&base, None);
    s.libraries.libraries.push(Library { name: name.clone(), items: Vec::new() });
    s.libraries.current = s.libraries.libraries.len() - 1;
    s.libraries.changed();
    Ok(json!({"name": name}))
}

fn rename_library(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.rename";
    let li = library_index(s, p, CMD)?;
    let base = name_param(CMD, p)?.ok_or_else(|| bad(CMD, "missing `name`"))?;
    let name = s.libraries.unique_library_name(&base, Some(li));
    let lib = s.libraries.libraries.get_mut(li).ok_or_else(|| bad(CMD, "no such library"))?;
    let old = std::mem::replace(&mut lib.name, name.clone());
    s.libraries.changed();
    Ok(json!({"from": old, "name": name}))
}

fn delete_library(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.delete";
    let li = library_index(s, p, CMD)?;
    if li >= s.libraries.libraries.len() {
        return Err(bad(CMD, "no such library"));
    }
    let lib = s.libraries.libraries.remove(li);
    let st = &mut s.libraries;
    if st.current > li || st.current >= st.libraries.len() {
        st.current = st.current.saturating_sub(1);
    }
    st.changed();
    Ok(json!({"deleted": lib.name, "items": lib.items.len()}))
}

fn select_library(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.select";
    if p.get("library").is_none_or(Value::is_null) {
        return Err(bad(CMD, "missing `library`"));
    }
    let li = library_index(s, p, CMD)?;
    s.libraries.current = li;
    s.libraries.changed();
    Ok(json!({"current": s.libraries.libraries.get(li).map(|l| l.name.clone())}))
}

// ------------------------------------------------------------------ adding items

/// Adds an item to library `li` and returns its description.
fn push_item(s: &mut Session, li: usize, name: String, kind: ItemKind, color: Option<[f32; 4]>, payload: Option<Document>) -> Result<Value> {
    let thumb = match (&payload, kind) {
        (Some(doc), ItemKind::Style) => Some(style_thumb(doc)),
        (Some(doc), _) => Some(photocraft_compose::thumbnail(doc, THUMB_SIDE)),
        (None, _) => None,
    };
    let id = s.libraries.take_id();
    let item = Item { id, name, kind, color, thumb: thumb.map(Arc::new), payload: payload.map(Arc::new) };
    let out = item_json(&item);
    let lib = s.libraries.libraries.get_mut(li).ok_or(EngineError::Other("no such library".into()))?;
    lib.items.push(item);
    s.libraries.changed();
    Ok(out)
}

/// The union of the layers' content bounds, or the canvas when they have none of their own
/// (adjustment layers) or are too far apart to crop to.
fn content_rect(doc: &Document) -> Rect {
    let b = doc.layers.iter().filter_map(crate::layer_multi_cmds::layer_bounds).fold(Rect::EMPTY, |a, b| a.union(&b));
    if b.is_empty() || b.width() > MAX_CROP_SIDE || b.height() > MAX_CROP_SIDE { doc.bounds() } else { b }
}

/// Crops a scratch document to its layers' content, moving them to the origin.
fn crop_to_content(copies: &mut Document) {
    let r = content_rect(copies);
    if r == copies.bounds() {
        return;
    }
    let mut target = Document::new("", Size::new(r.width().max(1), r.height().max(1)), copies.mode, copies.depth);
    target.icc_profile = copies.icc_profile.clone();
    target.resolution_dpi = copies.resolution_dpi;
    crate::layer_copy_cmds::translate_copies(copies, &target, -r.x0, -r.y0);
    copies.size = target.size;
}

fn add_layers(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.addLayers";
    let d = s.active().ok_or(EngineError::NoDocument)?;
    if !crate::layer_copy_cmds::layered(d.doc.mode) {
        return Err(EngineError::Other(format!("layers can't be saved from a {:?} document", d.doc.mode)));
    }
    let ids: Vec<LayerId> = match p.get("layers") {
        None | Some(Value::Null) => d.selected_layers(),
        Some(v) => v
            .as_array()
            .and_then(|a| a.iter().map(|id| id.as_u64().map(LayerId)).collect::<Option<Vec<_>>>())
            .ok_or_else(|| bad(CMD, "`layers` must be an array of layer ids"))?,
    };
    if let Some(missing) = ids.iter().find(|id| d.doc.layer(**id).is_none()) {
        return Err(EngineError::NoLayer(*missing));
    }
    let ids = crate::layer_multi_cmds::top_level(&d.doc, &ids);
    if ids.is_empty() {
        return Err(bad(CMD, "no layers to save"));
    }
    let mut copies = crate::edit_cmds::scratch_doc(&d.doc);
    for id in &ids {
        let mut l = d.doc.layer(*id).ok_or(EngineError::NoLayer(*id))?.duplicate();
        // A saved Background is an ordinary layer, as a duplicated one is.
        if d.doc.is_background(*id) {
            l.locks = Default::default();
        }
        copies.layers.push(l);
    }
    crate::edit_cmds::copy_linked_sources(&d.doc, &mut copies)?;
    crate::layer_multi_cmds::check_group_depth(&copies, "Add to Library")?;
    let default_name = match copies.layers.as_slice() {
        [one] => one.name.clone(),
        many => format!("{} Layers", many.len()),
    };
    crop_to_content(&mut copies);
    let name = name_param(CMD, p)?.unwrap_or(default_name);
    let li = target_library(s, p, CMD)?;
    push_item(s, li, name, ItemKind::Layers, None, Some(copies))
}

/// A one-layer scratch document holding `surface` (straight alpha), cropped to its content.
fn image_doc(like: &Document, surface: photocraft_raster::Surface, bounds: Rect, name: &str) -> Result<Document> {
    if bounds.is_empty() {
        return Err(EngineError::Other("the image is empty".into()));
    }
    let mut doc = Document::new(name, Size::new(bounds.width(), bounds.height()), like.mode, like.depth);
    doc.icc_profile = like.icc_profile.clone();
    doc.resolution_dpi = like.resolution_dpi;
    let mut layer = Layer::raster(name, surface.format());
    if let Some(surf) = layer.surface_mut() {
        *surf = crate::edit_cmds::shifted(&surface, -bounds.x0, -bounds.y0);
    }
    doc.layers.push(layer);
    Ok(doc)
}

/// An image file's flattened pixels as an image item (`name` is the file's name or path).
fn file_image(name: &str, bytes: &[u8]) -> Result<(Document, String)> {
    let opened = crate::file_cmds::open::import(&crate::file_cmds::paths::file_name(name), bytes)?;
    let fmt = opened.pixel_format();
    let flat = photocraft_compose::flatten_to_surface(&opened, photocraft_color::PixelFormat::new(fmt.mode, fmt.sample, true), None);
    let stem = crate::file_cmds::paths::stem(name);
    Ok((image_doc(&opened, flat, opened.bounds(), &stem)?, stem))
}

/// `library.addImage` for a file the shell already read (the web's file picker gives bytes, not
/// a path), as File › Place takes picked bytes: `name` is the file's name or path, `p` the
/// command's other params (`name`, `library`).
pub fn add_image_bytes(s: &mut Session, name: &str, bytes: &[u8], p: &Value) -> Result<Value> {
    const CMD: &str = "library.addImage";
    let (doc, default_name) = file_image(name, bytes)?;
    let item_name = name_param(CMD, p)?.unwrap_or(default_name);
    let li = target_library(s, p, CMD)?;
    let out = push_item(s, li, item_name, ItemKind::Image, None, Some(doc))?;
    s.sync_library_store();
    Ok(out)
}

fn add_image(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.addImage";
    let (doc, default_name) = match p.get("path") {
        Some(Value::String(path)) => file_image(path, &crate::file_cmds::paths::read_file(path)?)?,
        Some(Value::Null) | None => {
            let merged = match p.get("merged") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(bad(CMD, "`merged` must be true or false")),
            };
            let d = s.active().ok_or(EngineError::NoDocument)?;
            let canvas = d.doc.bounds();
            let (clip, name) = if merged {
                (crate::edit_cmds::lift(&crate::edit_cmds::merged_surface(&d.doc), d.doc.selection.as_ref(), canvas)?, "Merged".to_string())
            } else {
                let id = d.active_layer.ok_or(EngineError::Other("no active layer".into()))?;
                let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
                let surf = l.surface().ok_or_else(|| EngineError::Other("the active layer has no pixels (save it with library.addLayers)".into()))?;
                (crate::edit_cmds::lift(surf, d.doc.selection.as_ref(), canvas)?, l.name.clone())
            };
            if clip.bounds.is_empty() {
                return Err(EngineError::Other("the selected area is empty".into()));
            }
            (image_doc(&d.doc, clip.surface, clip.bounds, &name)?, name)
        }
        Some(_) => return Err(bad(CMD, "`path` must be a file path")),
    };
    let name = name_param(CMD, p)?.unwrap_or(default_name);
    let li = target_library(s, p, CMD)?;
    push_item(s, li, name, ItemKind::Image, None, Some(doc))
}

fn add_style(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.addStyle";
    let id = crate::commands::layer_param(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let l = d.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    // A one-pixel document holding an empty layer with the effects and blending, plus the
    // patterns they name.
    let mut doc = Document::new("", Size::new(1, 1), d.doc.mode, d.doc.depth);
    doc.icc_profile = d.doc.icc_profile.clone();
    let mut holder = Layer::raster(&l.name, doc.pixel_format());
    holder.effects = l.effects.clone();
    holder.effects.psd_raw = None;
    holder.blend = l.blend;
    holder.opacity = l.opacity;
    holder.fill_opacity = l.fill_opacity;
    for (name, pid) in photocraft_io::asl_map::referenced_patterns(&holder.effects.items) {
        if let Some(pat) = photocraft_doc::pattern::find(&d.doc.patterns, &name, &pid) {
            crate::pattern_cmds::ensure_in_doc(&mut doc, pat);
        }
    }
    doc.layers.push(holder);
    let default_name = format!("{} Style", l.name);
    let name = name_param(CMD, p)?.unwrap_or(default_name);
    let li = target_library(s, p, CMD)?;
    push_item(s, li, name, ItemKind::Style, None, Some(doc))
}

fn add_color(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.addColor";
    let fg = s.tools.foreground;
    let c = match p.get("color") {
        None | Some(Value::Null) => fg,
        Some(Value::String(t)) => crate::commands::parse_hex(t).ok_or_else(|| bad(CMD, "`color` must be \"#rrggbb\" or [r, g, b] (0..1)"))?,
        Some(Value::Array(a)) if a.len() == 3 || a.len() == 4 => {
            let mut out = [0.0, 0.0, 0.0, 1.0];
            for (slot, v) in out.iter_mut().zip(a.iter().take(3)) {
                let f = v.as_f64().filter(|f| f.is_finite() && (0.0..=1.0).contains(f)).ok_or_else(|| bad(CMD, "colour components must be numbers 0..1"))?;
                *slot = f as f32;
            }
            out
        }
        Some(_) => return Err(bad(CMD, "`color` must be \"#rrggbb\" or [r, g, b] (0..1)")),
    };
    let c = [c[0], c[1], c[2], 1.0];
    let name = name_param(CMD, p)?.unwrap_or_else(|| hex(c));
    let li = target_library(s, p, CMD)?;
    push_item(s, li, name, ItemKind::Color, Some(c), None)
}

// ------------------------------------------------------------------ editing items

fn rename_item(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.renameItem";
    let id = item_id(s, p, CMD)?;
    let name = name_param(CMD, p)?.ok_or_else(|| bad(CMD, "missing `name`"))?;
    let (li, ii) = s.libraries.locate(id).ok_or_else(|| bad(CMD, "no such item"))?;
    let item = s.libraries.libraries.get_mut(li).and_then(|l| l.items.get_mut(ii)).ok_or_else(|| bad(CMD, "no such item"))?;
    let old = std::mem::replace(&mut item.name, name.clone());
    s.libraries.changed();
    Ok(json!({"item": id, "from": old, "name": name}))
}

fn delete_item(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.deleteItem";
    let id = item_id(s, p, CMD)?;
    let (li, ii) = s.libraries.locate(id).ok_or_else(|| bad(CMD, "no such item"))?;
    let lib = s.libraries.libraries.get_mut(li).ok_or_else(|| bad(CMD, "no such item"))?;
    if ii >= lib.items.len() {
        return Err(bad(CMD, "no such item"));
    }
    let item = lib.items.remove(ii);
    s.libraries.changed();
    Ok(json!({"deleted": id, "name": item.name}))
}

fn move_item(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.moveItem";
    let id = item_id(s, p, CMD)?;
    if p.get("library").is_none_or(Value::is_null) {
        return Err(bad(CMD, "missing `library` (the destination)"));
    }
    let to = library_index(s, p, CMD)?;
    let (li, ii) = s.libraries.locate(id).ok_or_else(|| bad(CMD, "no such item"))?;
    if li == to {
        return Ok(json!({"item": id, "library": s.libraries.libraries.get(to).map(|l| l.name.clone())}));
    }
    if s.libraries.libraries.get(to).is_some_and(|l| l.items.len() >= MAX_ITEMS) {
        return Err(bad(CMD, format!("the library is full ({MAX_ITEMS} items)")));
    }
    let from = s.libraries.libraries.get_mut(li).ok_or_else(|| bad(CMD, "no such item"))?;
    if ii >= from.items.len() {
        return Err(bad(CMD, "no such item"));
    }
    let item = from.items.remove(ii);
    let dest = s.libraries.libraries.get_mut(to).ok_or_else(|| bad(CMD, "no such library"))?;
    dest.items.push(item);
    let name = dest.name.clone();
    s.libraries.changed();
    Ok(json!({"item": id, "library": name}))
}

// ------------------------------------------------------------------ using items

/// The item's scratch document, read from the store the first time a stored item is used.
fn payload(s: &mut Session, id: u64) -> Result<Arc<Document>> {
    let (li, ii) = s.libraries.locate(id).ok_or_else(|| EngineError::Other(format!("no library item {id}")))?;
    if let Some(doc) = s.libraries.libraries.get(li).and_then(|l| l.items.get(ii)).and_then(|i| i.payload.clone()) {
        return Ok(doc);
    }
    let store = s.libraries.store.as_ref().ok_or_else(|| EngineError::Other(format!("library item {id} has no content")))?;
    let doc = Arc::new(store.load_item(id).map_err(EngineError::Other)?);
    if let Some(item) = s.libraries.libraries.get_mut(li).and_then(|l| l.items.get_mut(ii)) {
        item.payload = Some(doc.clone());
    }
    Ok(doc)
}

fn point(p: &Value, cmd: &str) -> Result<Option<[f64; 2]>> {
    match p.get("at") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let xy = v.as_array().filter(|a| a.len() == 2).and_then(|a| Some([a[0].as_f64()?, a[1].as_f64()?]));
            match xy {
                Some(xy) if xy.iter().all(|c| c.is_finite() && c.abs() <= 1e7) => Ok(Some(xy)),
                _ => Err(bad(cmd, "`at` must be [x, y] (pixels)")),
            }
        }
    }
}

/// The style a style item holds.
fn style_of(doc: &Document, name: &str) -> Option<StylePreset> {
    let l = doc.layers.first()?;
    Some(StylePreset {
        name: name.to_string(),
        effects: l.effects.items.clone(),
        blend: Some(l.blend),
        fill_opacity: Some(l.fill_opacity),
        opacity: Some(l.opacity),
    })
}

fn style_thumb(doc: &Document) -> Rgba8Image {
    let style = style_of(doc, "").unwrap_or(StylePreset { name: String::new(), effects: Vec::new(), blend: None, fill_opacity: None, opacity: None });
    let px = crate::presets::styles::swatch(&style, &doc.patterns, THUMB_SIDE);
    Rgba8Image { width: THUMB_SIDE, height: THUMB_SIDE, pixels: px }
}

fn use_item(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "library.use";
    let id = item_id(s, p, CMD)?;
    let item = s.libraries.item(id).cloned().ok_or_else(|| bad(CMD, "no such item"))?;
    match item.kind {
        ItemKind::Color => {
            let c = item.color.ok_or_else(|| EngineError::Other("the colour item has no colour".into()))?;
            s.tools.foreground = [c[0], c[1], c[2], 1.0];
            s.tools.grey_for_mask();
            Ok(json!({"item": id, "foreground": hex(c)}))
        }
        ItemKind::Style => {
            let add = match p.get("add") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(bad(CMD, "`add` must be true or false")),
            };
            let ids = crate::layer_multi_cmds::selected(s);
            if ids.is_empty() {
                return Err(bad(CMD, "no target layers"));
            }
            let doc = payload(s, id)?;
            let style = style_of(&doc, &item.name).ok_or_else(|| EngineError::Other("the style item is empty".into()))?;
            let pats = doc.patterns.clone();
            s.edit(&format!("Apply Style: {}", item.name), |doc, _| {
                for pat in &pats {
                    crate::pattern_cmds::ensure_in_doc(doc, pat);
                }
                let mode = doc.mode;
                let effects: Vec<_> = style.effects.iter().cloned().map(|e| e.in_mode(mode)).collect();
                for lid in &ids {
                    crate::extra_cmds::background_to_layer(doc, *lid);
                    let l = doc.layer_mut(*lid).ok_or(EngineError::NoLayer(*lid))?;
                    crate::presets::styles::apply_to_layer(l, &style, &effects, add);
                }
                Ok(())
            })?;
            Ok(json!({"item": id, "layers": ids.iter().map(|l| l.0).collect::<Vec<_>>()}))
        }
        ItemKind::Layers | ItemKind::Image => {
            let at = point(p, CMD)?;
            let dest = s.active().ok_or(EngineError::NoDocument)?;
            if !crate::layer_copy_cmds::layered(dest.doc.mode) {
                return Err(EngineError::Other(format!("library items can't be placed in a {:?} document", dest.doc.mode)));
            }
            let canvas = dest.doc.bounds();
            let doc = payload(s, id)?;
            let content = content_rect(&doc);
            let target = at.unwrap_or([(f64::from(canvas.x0) + f64::from(canvas.x1)) / 2.0, (f64::from(canvas.y0) + f64::from(canvas.y1)) / 2.0]);
            let mid = [(f64::from(content.x0) + f64::from(content.x1)) / 2.0, (f64::from(content.y0) + f64::from(content.y1)) / 2.0];
            let (dx, dy) = ((target[0] - mid[0]).round() as i32, (target[1] - mid[1]).round() as i32);
            let mut copies = (*doc).clone();
            if item.kind == ItemKind::Image
                && let [l] = copies.layers.as_mut_slice()
            {
                l.name = item.name.clone();
            }
            let top = crate::edit_cmds::insert_layers(s, &copies, dx, dy, &format!("Place {}", item.name), false)?;
            Ok(json!({"item": id, "layer": top.0, "offset": [dx, dy]}))
        }
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "library.list",
            label: "Libraries",
            menu: &[],
            shortcut: None,
            params: r#"{} → {libraries:[{name,items:[{id,name,kind:"layers|image|style|color",color?,thumbnail?:[w,h]}]}],current,persistent}"#,
            enabled: always,
            run: list,
            journal: false,
        },
        CommandSpec {
            id: "library.new",
            label: "New Library",
            menu: &[],
            shortcut: None,
            params: r#"{"name":str="Library"} → {name} (made unique; becomes the current library)"#,
            enabled: always,
            run: new_library,
            journal: true,
        },
        CommandSpec {
            id: "library.rename",
            label: "Rename Library",
            menu: &[],
            shortcut: None,
            params: r#"{"library":name|index?=current,"name":str} → {from,name}"#,
            enabled: has_library,
            run: rename_library,
            journal: true,
        },
        CommandSpec {
            id: "library.delete",
            label: "Delete Library",
            menu: &[],
            shortcut: None,
            params: r#"{"library":name|index?=current} → {deleted,items}"#,
            enabled: has_library,
            run: delete_library,
            journal: true,
        },
        CommandSpec {
            id: "library.select",
            label: "Select Library",
            menu: &[],
            shortcut: None,
            params: r#"{"library":name|index} → {current} (where new items go)"#,
            enabled: has_library,
            run: select_library,
            journal: true,
        },
        CommandSpec {
            id: "library.addLayers",
            label: "Add Layers to Library",
            menu: &[],
            shortcut: None,
            params: r#"{"layers":[ids]?=selected,"name":str?,"library":name|index?=current} → {id,name,kind} (the layers whole: masks, effects, a group's children)"#,
            enabled: has_layer,
            run: add_layers,
            journal: true,
        },
        CommandSpec {
            id: "library.addImage",
            label: "Add Image to Library",
            menu: &[],
            shortcut: None,
            params: r#"{"path":str? (an image file; default: the selected pixels of the active layer),"merged":bool=false (the selected pixels of every visible layer),"name":str?,"library":name|index?=current} → {id,name,kind}"#,
            enabled: always,
            run: add_image,
            journal: true,
        },
        CommandSpec {
            id: "library.addStyle",
            label: "Add Layer Style to Library",
            menu: &[],
            shortcut: None,
            params: r#"{"layer":id?=active,"name":str?,"library":name|index?=current} → {id,name,kind} (the layer's effects and blending options)"#,
            enabled: has_layer,
            run: add_style,
            journal: true,
        },
        CommandSpec {
            id: "library.addColor",
            label: "Add Color to Library",
            menu: &[],
            shortcut: None,
            params: r##"{"color":"#rrggbb"|[r,g,b] (0..1)?=foreground,"name":str?,"library":name|index?=current} → {id,name,kind,color}"##,
            enabled: always,
            run: add_color,
            journal: true,
        },
        CommandSpec {
            id: "library.renameItem",
            label: "Rename Library Item",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id,"name":str} → {item,from,name}"#,
            enabled: has_items,
            run: rename_item,
            journal: true,
        },
        CommandSpec {
            id: "library.deleteItem",
            label: "Delete Library Item",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id} → {deleted,name}"#,
            enabled: has_items,
            run: delete_item,
            journal: true,
        },
        CommandSpec {
            id: "library.moveItem",
            label: "Move Library Item",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id,"library":name|index} → {item,library}"#,
            enabled: has_items,
            run: move_item,
            journal: true,
        },
        CommandSpec {
            id: "library.use",
            label: "Use Library Item",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id,"at":[x,y]? (layers/image: centre here; default the canvas centre),"add":bool=false (style: add to the effects)} → layers/image: {item,layer,offset} (one history step); style: {item,layers} (applied to the selected layers); color: {item,foreground}"#,
            enabled: has_items,
            run: use_item,
            journal: true,
        },
    ]
}
