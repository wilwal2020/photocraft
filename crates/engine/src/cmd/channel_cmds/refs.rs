use rayon::prelude::*;
use std::sync::{Arc, Mutex, PoisonError};

use super::planes::{color_count, edit_mode, luminosity, read_plane, tone};
use super::view::{color_names, composite_name};
use super::{BlendMode, Document, EngineError, LayerContent, LayerId, Result, Session, Value, blend_channel, from_rgba, sel, to_rgba};
use photocraft_raster::from_rgba_into;

// ---------------------------------------------------------------------------------------------
// Channel references

/// A channel named in params: `"composite"`, a colour name (`"red"`, `"cyan"`, `"lightness"`…),
/// an alpha index (number) or name, `"quickMask"`, `"selection"`, `"transparency"`, `"mask"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChanRef {
    Composite,
    Color(usize),
    Alpha(usize),
    QuickMask,
    Selection,
    Transparency,
    LayerMask,
    /// The layer's vector mask, rasterized (⌘-click its thumbnail, #196).
    VectorMask,
    /// The layer's second pixel mask (`Layer::second_mask`).
    SecondMask,
}

pub fn parse_ref(v: &Value, doc: &Document) -> Option<ChanRef> {
    let mode = edit_mode(doc);
    match v {
        Value::Number(n) => n.as_u64().map(|i| ChanRef::Alpha(i as usize)),
        Value::Object(o) => {
            if let Some(k) = o.get("color").and_then(Value::as_u64) {
                Some(ChanRef::Color(k as usize))
            } else {
                o.get("alpha").and_then(Value::as_u64).map(|i| ChanRef::Alpha(i as usize))
            }
        }
        Value::String(s) => {
            let l = s.to_ascii_lowercase();
            match l.as_str() {
                "composite" | "merged" => return Some(ChanRef::Composite),
                "quickmask" | "quick mask" => return Some(ChanRef::QuickMask),
                "selection" => return Some(ChanRef::Selection),
                "transparency" => return Some(ChanRef::Transparency),
                "mask" | "layermask" => return Some(ChanRef::LayerMask),
                "vectormask" | "vector mask" => return Some(ChanRef::VectorMask),
                "secondmask" | "second mask" => return Some(ChanRef::SecondMask),
                _ => {}
            }
            if l == composite_name(mode).to_ascii_lowercase() {
                return Some(ChanRef::Composite);
            }
            if let Some(k) = color_names(mode).iter().position(|n| n.eq_ignore_ascii_case(&l)) {
                return Some(if color_names(mode).len() == 1 { ChanRef::Composite } else { ChanRef::Color(k) });
            }
            doc.channels.iter().position(|c| c.name == *s).map(ChanRef::Alpha)
        }
        _ => None,
    }
}

pub(super) fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

pub(super) fn doc_index(s: &Session, p: &Value, key: &str) -> Result<usize> {
    match crate::commands::int(p, key).and_then(|v| u64::try_from(v).ok()) {
        Some(i) if (i as usize) < s.documents().len() => Ok(i as usize),
        Some(i) => Err(EngineError::Other(format!("no open document {i}"))),
        None => s.active_index().ok_or(EngineError::NoDocument),
    }
}

/// Alpha channel index from `p[key]` (number or name), checked against the document.
pub(super) fn alpha_index(doc: &Document, p: &Value, key: &str, cmd: &str) -> Result<usize> {
    match p.get(key).and_then(|v| parse_ref(v, doc)) {
        Some(ChanRef::Alpha(i)) if i < doc.channels.len() => Ok(i),
        Some(ChanRef::Alpha(i)) => Err(bad(cmd, format!("no alpha channel {i} (document has {})", doc.channels.len()))),
        _ => Err(bad(cmd, format!("`{key}` must name an alpha channel (index or name)"))),
    }
}

// ---------------------------------------------------------------------------------------------
// Sources (Load Selection, Apply Image, Calculations)

/// Native pixel values of a layer or of the merged image over the canvas.
pub(super) fn native_pixels(doc: &Document, layer: Option<LayerId>) -> Result<(Vec<f32>, usize)> {
    let r = doc.bounds();
    let fmt = doc.pixel_format();
    let n = fmt.channels();
    match layer {
        Some(id) => {
            let l = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            let s = l.surface().ok_or_else(|| EngineError::Other(format!("layer \"{}\" has no pixels", l.name)))?;
            if s.format() == fmt {
                return Ok((s.read_region(r), n));
            }
            let raw = s.read_region(r);
            let sf = s.format();
            Ok((raw.chunks_exact(sf.channels()).flat_map(|p| from_rgba(&fmt, to_rgba(&sf, p))).collect(), n))
        }
        None => {
            // Converted in place, in parallel: a value list per pixel (`from_rgba`) cost most of
            // a second on a 7 MP document.
            let buf = merged(doc);
            let mut out = vec![0.0f32; buf.px.len().saturating_mul(n)];
            out.par_chunks_mut(n.max(1)).zip(buf.px.par_iter()).for_each(|(o, p)| {
                from_rgba_into(&fmt, *p, o);
            });
            Ok((out, n))
        }
    }
}

/// The last merged image [`merged`] rendered, and the document it is of.
static MERGED: Mutex<Option<(Document, Arc<photocraft_compose::Buffer>)>> = Mutex::new(None);

/// Documents up to this many pixels keep their merged image for the next [`merged`] (16 bytes a
/// pixel: 768 MiB at the cap).
const MAX_KEPT_MERGED_PIXELS: u64 = 48 << 20;

/// The merged image of `doc` over the canvas. Kept from the last call while the document differs
/// only in its selection: ⌘-clicking channel after channel, or a channel and then Apply Image,
/// renders once (user report: each ⌘-click on a 114-layer document re-rendered it, ~1 s).
/// [`prepare_merged`] renders it ahead on another thread; a call meanwhile waits for that one.
pub(crate) fn merged(doc: &Document) -> Arc<photocraft_compose::Buffer> {
    let mut kept = MERGED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((d, buf)) = kept.as_ref()
        && same_but_selection(d, doc)
    {
        return buf.clone();
    }
    // The old one goes before the new one is rendered: two full-size buffers at once is the peak
    // this avoids.
    *kept = None;
    let buf = Arc::new(photocraft_compose::render(doc, doc.bounds()));
    if doc.size.area() <= MAX_KEPT_MERGED_PIXELS {
        *kept = Some((doc.clone(), buf.clone()));
    }
    buf
}

/// Render `doc`'s merged image now (on a background thread) so the next ⌘-click on a composite
/// or colour channel, Apply Image or Calculations doesn't wait for it. Nothing happens when it is
/// kept already or the document is too big to keep it.
pub fn prepare_merged(doc: &Document) {
    if doc.size.area() <= MAX_KEPT_MERGED_PIXELS {
        merged(doc);
    }
}

/// Whether the merged image of `doc` is kept (rendered and not changed since but in its
/// selection).
pub fn merged_is_ready(doc: &Document) -> bool {
    // A render in progress holds the lock: not ready yet.
    MERGED.try_lock().is_ok_and(|kept| kept.as_ref().is_some_and(|(d, _)| same_but_selection(d, doc)))
}

/// Drop the kept merged image of document `id` (it was closed).
pub fn forget_merged(id: photocraft_doc::DocId) {
    let mut kept = MERGED.lock().unwrap_or_else(PoisonError::into_inner);
    if kept.as_ref().is_some_and(|(d, _)| d.id == id) {
        *kept = None;
    }
}

/// Whether `new` differs from `old` at most in its selection, so its merged image is the same.
/// Tiles compare by pointer first, so this costs little.
fn same_but_selection(old: &Document, new: &Document) -> bool {
    if old.selection == new.selection {
        return old == new;
    }
    let mut o = old.clone();
    o.selection = new.selection.clone();
    o == *new
}

/// Most channels a pixel has (CMYK and alpha).
const MAX_CHANNELS: usize = 8;

/// The merged image over the canvas, one value per pixel: `f` gets each pixel's native values
/// (colour channels, then alpha when the document has it). In parallel, with no per-pixel
/// allocation, and without the colour planes a [`native_pixels`] split makes.
fn composite_values(doc: &Document, f: impl Fn(&mut [f32]) -> f32 + Sync + Send) -> Vec<f32> {
    let fmt = doc.pixel_format();
    let n = fmt.channels().min(MAX_CHANNELS);
    let buf = merged(doc);
    buf.px
        .par_iter()
        .map_init(
            || [0.0f32; MAX_CHANNELS],
            |tmp, p| {
                from_rgba_into(&fmt, *p, tmp);
                tmp.get_mut(..n).map_or(0.0, &f)
            },
        )
        .collect()
}

/// `"layer"` param of a source: a layer id, a layer name (the topmost of that name, as a loaded
/// Photoshop action refers to it), or `"merged"` (default). An unknown name is an error.
pub(super) fn source_layer(doc: &Document, p: &Value, cmd: &str) -> Result<Option<LayerId>> {
    match p.get("layer") {
        Some(Value::Number(n)) => Ok(n.as_u64().map(LayerId)),
        Some(Value::String(name)) if !name.is_empty() && name != "merged" => doc
            .walk()
            .into_iter()
            .rev()
            .find(|(_, _, l)| l.name == *name)
            .map(|(_, _, l)| Some(l.id))
            .ok_or_else(|| bad(cmd, format!("no source layer named \"{name}\""))),
        _ => Ok(None),
    }
}

/// Planes of channel `r` from document `doc` (`layer` = None: merged). A composite gives one
/// plane per colour channel; everything else gives one plane.
fn ref_planes(doc: &Document, layer: Option<LayerId>, active: Option<LayerId>, r: ChanRef) -> Result<Vec<Vec<f32>>> {
    let area = doc.bounds();
    let colors = color_count(doc);
    Ok(match r {
        ChanRef::Composite | ChanRef::Color(_) => {
            let (px, n) = native_pixels(doc, layer)?;
            let planes: Vec<Vec<f32>> = (0..colors).map(|c| px.chunks_exact(n).map(|p| p[c]).collect()).collect();
            match r {
                ChanRef::Color(k) => {
                    let plane = planes.get(k).ok_or_else(|| EngineError::Other(format!("no colour channel {k}")))?;
                    vec![plane.iter().map(|&v| tone(edit_mode(doc), v)).collect()]
                }
                _ => planes,
            }
        }
        ChanRef::Alpha(i) => {
            let ch = doc.channels.get(i).ok_or_else(|| EngineError::Other(format!("no alpha channel {i}")))?;
            let plane = read_plane(&ch.surface, area);
            // A spot channel stores ink (1 = solid) but reads as its tone, as in Photoshop.
            vec![if ch.spot.is_some() { plane.into_iter().map(|v| 1.0 - v).collect() } else { plane }]
        }
        ChanRef::QuickMask => vec![read_plane(&doc.quick_mask.as_ref().ok_or_else(|| EngineError::Other("not in Quick Mask mode".into()))?.surface, area)],
        ChanRef::Selection => vec![sel::mask_from_surface(doc.selection.as_ref(), area)],
        ChanRef::Transparency => {
            let id = layer.or(active).ok_or_else(|| EngineError::Other("transparency needs a layer".into()))?;
            let l = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            match l.surface() {
                Some(s) => {
                    let mut px = vec![[0.0f32; 4]; area.width() as usize * area.height() as usize];
                    s.read_rgba_into(area, &mut px);
                    vec![px.iter().map(|p| p[3]).collect()]
                }
                // A shape can have no pixel cache yet (for example after loading a document).
                // Use its existing renderer rather than treating it as a canvas-wide fill.
                None => match &l.content {
                    LayerContent::Shape(shape) => vec![
                        photocraft_vector::CompiledShape::new(shape, photocraft_vector::DEFAULT_TOLERANCE, area)
                            .render_rgba(area)
                            .into_iter()
                            .map(|p| p[3])
                            .collect(),
                    ],
                    // Adjustment and fill layers cover the whole canvas.
                    _ => vec![vec![1.0; area.width() as usize * area.height() as usize]],
                },
            }
        }
        ChanRef::LayerMask => {
            let id = layer.or(active).ok_or_else(|| EngineError::Other("layer mask needs a layer".into()))?;
            let l = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            let m = l.mask.as_ref().ok_or_else(|| EngineError::Other(format!("layer \"{}\" has no mask", l.name)))?;
            vec![read_plane(&m.surface, area)]
        }
        ChanRef::SecondMask => {
            let id = layer.or(active).ok_or_else(|| EngineError::Other("second mask needs a layer".into()))?;
            let l = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            let m = l.second_mask.as_ref().ok_or_else(|| EngineError::Other(format!("layer \"{}\" has no second mask", l.name)))?;
            vec![read_plane(&m.surface, area)]
        }
        ChanRef::VectorMask => {
            let id = layer.or(active).ok_or_else(|| EngineError::Other("vector mask needs a layer".into()))?;
            let l = doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            let m = l.vector_mask.as_ref().ok_or_else(|| EngineError::Other(format!("layer \"{}\" has no vector mask", l.name)))?;
            // The path's shape, whether or not the mask is enabled (as Photoshop loads it).
            let shape = photocraft_doc::VectorMask { enabled: true, density: 1.0, feather: 0.0, ..m.clone() };
            vec![photocraft_vector::vector_mask_values(&shape, area)]
        }
    })
}

/// One grayscale plane (composites reduce to luminosity, as in Calculations).
pub(super) fn gray_plane(doc: &Document, layer: Option<LayerId>, active: Option<LayerId>, r: ChanRef) -> Result<Vec<f32>> {
    // The merged image or one of its colour channels (⌘-click on the Channels panel): straight
    // from the composite in one parallel pass, with the same values as the planes below give.
    if layer.is_none() {
        let fmt = doc.pixel_format();
        let colors = color_count(doc);
        match r {
            ChanRef::Color(k) if k < colors => {
                // A CMYK channel reads as its tone, as the planes below give it.
                let mode = edit_mode(doc);
                return Ok(composite_values(doc, |px| tone(mode, px.get(k).copied().unwrap_or(0.0))));
            }
            ChanRef::Composite if colors == 1 => return Ok(composite_values(doc, |px| px.first().copied().unwrap_or(0.0))),
            ChanRef::Composite => {
                return Ok(composite_values(doc, |px| {
                    if fmt.alpha
                        && let Some(a) = px.get_mut(colors)
                    {
                        *a = 1.0;
                    }
                    luminosity(to_rgba(&fmt, px))
                }));
            }
            _ => {}
        }
    }
    let mut planes = ref_planes(doc, layer, active, r)?;
    if planes.len() == 1
        && let Some(only) = planes.pop()
    {
        return Ok(only);
    }
    let fmt = doc.pixel_format();
    let n = planes.first().map_or(0, Vec::len);
    let mut px = vec![0.0f32; fmt.channels()];
    Ok((0..n)
        .map(|i| {
            for (c, p) in planes.iter().enumerate() {
                px[c] = p[i];
            }
            if fmt.alpha {
                px[planes.len()] = 1.0;
            }
            luminosity(to_rgba(&fmt, &px))
        })
        .collect())
}

/// A source spec `{document?, layer?, channel, invert?}`, reduced to planes over the canvas of
/// the active document (sizes must match, as in Photoshop).
pub(super) fn source(s: &Session, p: &Value, cmd: &str, gray: bool) -> Result<Vec<Vec<f32>>> {
    let di = doc_index(s, p, "document")?;
    let st = &s.documents()[di];
    let target = s.active().ok_or(EngineError::NoDocument)?;
    if st.doc.size != target.doc.size {
        return Err(bad(cmd, format!("source document \"{}\" is {}×{}; it must match the target size", st.doc.name, st.doc.size.width, st.doc.size.height)));
    }
    let r = p.get("channel").map_or(Some(ChanRef::Composite), |v| parse_ref(v, &st.doc)).ok_or_else(|| bad(cmd, "unknown `channel`"))?;
    let layer = source_layer(&st.doc, p, cmd)?;
    let mut planes = if gray { vec![gray_plane(&st.doc, layer, st.active_layer, r)?] } else { ref_planes(&st.doc, layer, st.active_layer, r)? };
    // Guard every caller (`.remove(0)` / `src[0]`) against an empty source.
    if planes.is_empty() {
        return Err(bad(cmd, "the source has no channels"));
    }
    if p.get("invert").and_then(Value::as_bool).unwrap_or(false) {
        for pl in &mut planes {
            for v in pl.iter_mut() {
                *v = 1.0 - *v;
            }
        }
    }
    Ok(planes)
}

// ---------------------------------------------------------------------------------------------
// Blending (Apply Image / Calculations)

/// Apply Image / Calculations blending: the separable layer modes plus Add and Subtract with
/// scale and offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Blending {
    Mode(BlendMode),
    Add { scale: f32, offset: f32 },
    Subtract { scale: f32, offset: f32 },
}

impl Blending {
    pub fn parse(p: &Value) -> std::result::Result<Self, String> {
        let name = p.get("blending").and_then(Value::as_str).unwrap_or("multiply");
        let scale = p.get("scale").and_then(Value::as_f64).unwrap_or(1.0).clamp(1.0, 2.0) as f32;
        let offset = p.get("offset").and_then(Value::as_f64).unwrap_or(0.0).clamp(-255.0, 255.0) as f32 / 255.0;
        match name.to_ascii_lowercase().as_str() {
            "add" => Ok(Blending::Add { scale, offset }),
            "subtract" => Ok(Blending::Subtract { scale, offset }),
            other => match crate::commands::blend_from_str(other) {
                Some(m) if m.is_separable() && !matches!(m, BlendMode::Dissolve | BlendMode::PassThrough) => Ok(Blending::Mode(m)),
                _ => Err(format!("unsupported blending `{other}`")),
            },
        }
    }

    /// Blend `top` (the source) onto `base` (the target).
    pub fn apply(self, base: f32, top: f32) -> f32 {
        match self {
            Blending::Mode(m) => blend_channel(m, base, top),
            Blending::Add { scale, offset } => (base + top) / scale + offset,
            Blending::Subtract { scale, offset } => (base - top) / scale + offset,
        }
        .clamp(0.0, 1.0)
    }
}

/// A source spec from `p[key]`, or from flat `<prefix>Document` / `<prefix>Layer` /
/// `<prefix>Channel` / `<prefix>Invert` keys (what the generic dialog form sends).
pub(crate) fn source_spec(p: &Value, key: &str, prefix: &str) -> Option<Value> {
    if let Some(v) = p.get(key) {
        return Some(v.clone());
    }
    let mut m = serde_json::Map::new();
    for (k, field) in [("document", "Document"), ("layer", "Layer"), ("channel", "Channel"), ("invert", "Invert")] {
        match p.get(format!("{prefix}{field}")) {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if s.is_empty() || (k == "layer" && s == "merged") => {}
            Some(v) => {
                m.insert(k.into(), v.clone());
            }
        }
    }
    (!m.is_empty()).then_some(Value::Object(m))
}

pub(super) fn mask_plane(s: &Session, p: &Value, cmd: &str) -> Result<Option<Vec<f32>>> {
    match source_spec(p, "mask", "mask") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(None),
        Some(m) if m.get("channel").is_none_or(|c| c == "none") => Ok(None),
        Some(m) => Ok(Some(source(s, &m, cmd, true)?.remove(0))),
    }
}
