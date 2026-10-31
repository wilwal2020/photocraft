//! Load Visible Pixels (a PhotoCraft addition, user request): with Ctrl+Space held (⌘Space on the
//! Mac), a click on a layer's row, one of its mask thumbnails or the canvas selects only where
//! that layer (or that mask) shows in the composite. What is above covers it as far as it is
//! opaque there; layers with Lock Selection (their own or a group's) count as invisible, and
//! adjustment layers cover nothing. The clicked layer counts as shown, its groups too.
//!
//! The compositor measures it, so clipping, group masks and opacities, knockout and effects above
//! all count as they draw: the document is rendered twice with the layer painted white and then
//! black (a layer of that colour clipped to it, or for a clipped layer a Color Overlay; for a mask
//! a full canvas of that colour through the mask), and how far the composite changes between the two at a pixel is how much of the
//! layer shows there. Everything else is the same in both renders and cancels out, so what is
//! below the layer (but its clipping base) is left out of them: the renders cost what the layer
//! and the layers above it do (user request: make it faster).

use photocraft_algo::selection::{self as sel, SelectionMode};
use photocraft_color::{BlendMode, Color};
use photocraft_doc::{Document, Effect, Effects, FxCommon, LayerContent, LayerId};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const ID: &str = "select.loadVisible";

/// What a click measures: the layer itself or one of its masks alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Layer,
    Mask,
    SecondMask,
    VectorMask,
}

impl Source {
    fn parse(s: &str) -> Option<Source> {
        Some(match s {
            "layer" => Source::Layer,
            "mask" => Source::Mask,
            "secondMask" => Source::SecondMask,
            "vectorMask" => Source::VectorMask,
            _ => return None,
        })
    }
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: ID.into(), msg: msg.into() }
}

/// `doc` with layer `id` painted `shade` (0 black, 1 white) as [`Source`] says, and the layers
/// that must not cover it hidden. `None` when the layer is gone or lacks the mask.
fn probe(doc: &Document, id: LayerId, source: Source, shade: f32) -> Option<Document> {
    let mut d = doc.clone();
    let path = d.path_of(id)?;
    // The clicked layer and its groups show; Lock Selection layers and adjustments elsewhere don't,
    // nor does anything below it but its clipping base.
    let ancestors: Vec<LayerId> = (1..path.len()).filter_map(|n| d.layer_at(&path[..n]).map(|l| l.id)).collect();
    let base = clip_base(&d, &path);
    let hide: Vec<LayerId> = d
        .walk()
        .iter()
        .filter(|(_, _, l)| l.id != id && !ancestors.contains(&l.id) && Some(l.id) != base)
        .filter(|(p, _, l)| *p < path || matches!(l.content, LayerContent::Adjustment(_)) || doc.effective_locks(l.id).selection)
        .map(|(_, _, l)| l.id)
        .collect();
    for h in hide.into_iter().chain(ancestors.iter().copied()) {
        if let Some(l) = d.layer_mut(h) {
            l.visible = ancestors.contains(&h);
        }
    }
    let fmt = d.pixel_format();
    let mode = fmt.mode;
    let l = d.layer_mut(id)?;
    l.visible = true;
    l.blend = BlendMode::Normal;
    l.opacity = 1.0;
    l.fill_opacity = 1.0;
    l.fill_cache = None;
    let paint = Surface::with_default(fmt, &from_rgba(&fmt, [shade, shade, shade, 1.0]));
    if source == Source::Layer {
        if l.clipped {
            // Clipped itself: what is clipped above it still covers an overlay of its own.
            let overlay = Effect::ColorOverlay { common: FxCommon::new(BlendMode::Normal, 1.0), color: Color::rgb(shade, shade, shade).in_mode(mode) };
            l.effects = Effects { enabled: true, items: vec![overlay], psd_raw: None, reference: l.effects.reference };
            return Some(d);
        }
        // A clipping base's own effects draw over the layers clipped to it, so the colour is a
        // layer clipped to it, first of its clipping group, and its effects are off.
        l.effects = Effects::default();
        let mut coat = photocraft_doc::Layer::new("probe", LayerContent::Raster(paint));
        coat.clipped = true;
        let (&last, parent) = path.split_last()?;
        let siblings = if parent.is_empty() { &mut d.layers } else { d.layer_at_mut(parent)?.children_mut()? };
        siblings.insert(last + 1, coat);
        return Some(d);
    }
    // A mask alone: an opaque canvas of the shade, shown through only that mask.
    let (mask, second, vector) = (l.mask.take(), l.second_mask.take(), l.vector_mask.take());
    match source {
        Source::Mask => l.mask = Some(mask?),
        Source::SecondMask => l.mask = Some(second?),
        _ => l.vector_mask = Some(vector?),
    }
    if let Some(m) = l.mask.as_mut() {
        m.enabled = true;
    }
    if let Some(v) = l.vector_mask.as_mut() {
        v.enabled = true;
    }
    l.content = LayerContent::Raster(paint);
    l.effects = Effects::default();
    Some(d)
}

/// The layer the one at `path` is clipped to: the nearest unclipped one below it in its group.
fn clip_base(doc: &Document, path: &[usize]) -> Option<LayerId> {
    let (&last, parent) = path.split_last()?;
    if !doc.layer_at(path)?.clipped {
        return None;
    }
    (0..last).rev().find_map(|i| {
        let mut p = parent.to_vec();
        p.push(i);
        doc.layer_at(&p).filter(|l| !l.clipped).map(|l| l.id)
    })
}

/// How dark the composite is at each pixel, premultiplied (a transparent pixel counts as black).
fn tone(doc: &Document, rect: Rect) -> Vec<f32> {
    photocraft_compose::render(doc, rect).px.iter().map(|p| photocraft_color::convert::rgb_to_gray([p[0], p[1], p[2]]) * p[3]).collect()
}

/// The change between white and black probes of an opaque layer alone: 1 in RGB, less where the
/// document's colours don't reach pure white and black (CMYK's black ink is not RGB black).
fn full_change(doc: &Document) -> f32 {
    let mut d = doc.clone();
    let fmt = d.pixel_format();
    let mut l = photocraft_doc::Layer::raster("probe", fmt);
    l.content = LayerContent::Raster(Surface::with_default(fmt, &from_rgba(&fmt, [0.5, 0.5, 0.5, 1.0])));
    let id = l.id;
    d.layers = vec![l];
    let px = Rect::new(0, 0, 1, 1);
    let at = |shade: f32| probe(&d, id, Source::Layer, shade).map_or(0.0, |p| tone(&p, px).first().copied().unwrap_or(0.0));
    let k = at(1.0) - at(0.0);
    if k.is_finite() && k > 1e-3 { k } else { 1.0 }
}

/// How much of `source` of layer `id` shows at each pixel of the canvas (row-major, 0..1).
pub fn visible_plane(doc: &Document, id: LayerId, source: Source) -> Option<Vec<f32>> {
    let canvas = doc.bounds();
    let (white, black) = (probe(doc, id, source, 1.0)?, probe(doc, id, source, 0.0)?);
    // A layer shows only inside its own extent; a mask anywhere on the canvas.
    let area = match source {
        Source::Layer => white.layer(id).map_or(canvas, |l| photocraft_compose::layer_bounds(l, canvas)).intersect(&canvas),
        _ => canvas,
    };
    let mut out = vec![0.0f32; canvas.width() as usize * canvas.height() as usize];
    if area.is_empty() {
        return Some(out);
    }
    let k = full_change(doc);
    let (w, b) = (tone(&white, area), tone(&black, area));
    let (aw, cw) = (area.width() as usize, canvas.width() as usize);
    for (i, (w, b)) in w.iter().zip(&b).enumerate() {
        let (x, y) = (area.x0 as usize - canvas.x0 as usize + i % aw, area.y0 as usize - canvas.y0 as usize + i / aw);
        let v = (w - b) / k;
        out[y * cw + x] = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    }
    Some(out)
}

/// The layer a Ctrl+Space click at (x, y) on the canvas measures: the topmost visible one with
/// pixels there, Lock Selection layers passed through.
pub fn layer_at(doc: &Document, x: i32, y: i32) -> Option<LayerId> {
    crate::pick_cmds::layers_at(doc, x, y).into_iter().find(|id| !doc.effective_locks(*id).selection && doc.layer(*id).is_some_and(|l| l.artboard().is_none()))
}

fn load_visible(s: &mut Session, p: &Value) -> Result<Value> {
    let mode = match p.get("operation").and_then(Value::as_str).unwrap_or("new") {
        "new" | "replace" => SelectionMode::Replace,
        "add" => SelectionMode::Add,
        "subtract" => SelectionMode::Subtract,
        "intersect" => SelectionMode::Intersect,
        other => return Err(bad(format!("unknown operation `{other}`"))),
    };
    let source = match p.get("source") {
        None | Some(Value::Null) => Source::Layer,
        Some(v) => v.as_str().and_then(Source::parse).ok_or_else(|| bad(format!("`source` = {v} must be \"layer\", \"mask\", \"secondMask\" or \"vectorMask\"")))?,
    };
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let id = match (p.get("layer").and_then(Value::as_u64), p.get("x").and_then(Value::as_f64), p.get("y").and_then(Value::as_f64)) {
        (Some(id), _, _) => Some(LayerId(id)).filter(|id| doc.layer(*id).is_some()).ok_or_else(|| bad(format!("no layer {id}")))?,
        (None, Some(x), Some(y)) if x.is_finite() && y.is_finite() => match layer_at(doc, x.floor() as i32, y.floor() as i32) {
            Some(id) => id,
            // Nothing there: nothing to load, and the selection stays.
            None => return Ok(json!({ "layer": null, "selected": doc.selection.is_some() })),
        },
        _ => return Err(bad("give `layer`, or the canvas point `x`, `y`")),
    };
    let plane = visible_plane(doc, id, source).ok_or_else(|| bad("the layer has no such mask"))?;
    let selected = s.edit("Load Visible Pixels", |doc, _| {
        doc.selection = sel::combine(doc.selection.as_ref(), &plane, doc.bounds(), mode);
        Ok(doc.selection.is_some())
    })?;
    Ok(json!({ "layer": id.0, "selected": selected }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: "Load Visible Pixels",
        menu: &[],
        shortcut: None,
        params: r##"{"layer":id?,"x":px?,"y":px? (instead of `layer`: the topmost visible layer with pixels at that canvas point, Lock Selection layers passed through),"source":"layer"|"mask"|"secondMask"|"vectorMask"="layer","operation":"new|add|subtract|intersect"="new"} → {layer,selected} (selects where the layer, or that mask alone, shows in the composite: layers above cover it as far as they are opaque, Lock Selection layers count as invisible, adjustments cover nothing)"##,
        enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document open".into()),
        run: load_visible,
        journal: true,
    }]
}

#[cfg(test)]
mod tests;
