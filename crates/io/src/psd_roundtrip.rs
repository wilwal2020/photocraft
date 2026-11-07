//! PhotoCraft-only layer settings in a PSD, written so they survive a save in Photoshop.
//!
//! Photoshop drops tagged blocks it doesn't know (such as `PCm2`, which keeps a second mask
//! exactly) but keeps layers, their masks and transparency, and each layer's XMP metadata
//! (checked with `examples/psd_roundtrip_probe.rs` against Photoshop 2026). So:
//!
//! - A **second mask** gets a hidden, clipped helper layer directly above its layer: the helper's
//!   layer mask is the second mask and its transparency is the first mask; its XMP says what it
//!   is and holds the first mask's settings. The layer itself keeps the two masks multiplied
//!   (what Photoshop shows) and the `PCm2` block. Clipped and hidden, the helper keeps any
//!   clipping chain through the layer intact.
//! - An **effect's own mask** gets a hidden, clipped helper whose layer mask is that mask; its
//!   XMP names the effect (kind, and which of that kind).
//! - **Lock Selection** is written into the layer's XMP as well as into the spare `lspf` bit.
//!
//! On opening, `PCm2` wins when it is there (a file PhotoCraft saved last); otherwise a second
//! mask comes back from its helper when the layer's mask is still the two multiplied (else the
//! mask edited in Photoshop is kept alone, and a warning says so). Helpers are removed either way.

use std::sync::Arc;

use photocraft_doc::{Document, Layer, LayerContent, LayerMask};
use photocraft_geom::Rect;
use photocraft_raster::Surface;

/// The XMP namespace PhotoCraft's settings use.
pub const NS: &str = "https://photocraft.app/ns/psd/1.0/";
const PREFIX: &str = "photocraft";
/// Helper roles.
const SECOND: &str = "secondMask";
const FX: &str = "fxMask";
/// Values of a mask more apart than this (0–1) mean it was edited (PSD masks are 8-bit or more).
const SAME: f32 = 2.5 / 255.0;

fn packet(attrs: &[(&str, String)]) -> String {
    let attrs: String = attrs.iter().map(|(k, v)| format!(" {PREFIX}:{k}=\"{}\"", escape(v))).collect();
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:{PREFIX}=\"{NS}\"{attrs}/></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>"
    )
}

fn escape(v: &str) -> String {
    v.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

fn unescape(v: &str) -> String {
    v.replace("&quot;", "\"").replace("&lt;", "<").replace("&amp;", "&")
}

/// The value of `photocraft:{name}` in `xmp`, as an attribute (`name="v"`) or an element
/// (`<photocraft:name>v</photocraft:name>`, as some writers rewrite attributes).
fn attr(xmp: &str, name: &str) -> Option<String> {
    let a = format!("{PREFIX}:{name}=\"");
    if let Some(i) = xmp.find(&a) {
        let rest = &xmp[i + a.len()..];
        return rest.find('"').map(|j| unescape(&rest[..j]));
    }
    let open = format!("<{PREFIX}:{name}>");
    let i = xmp.find(&open)?;
    let rest = &xmp[i + open.len()..];
    rest.find('<').map(|j| unescape(&rest[..j]))
}

fn role(l: &Layer) -> Option<String> {
    l.xmp.as_deref().filter(|x| x.contains(NS)).and_then(|x| attr(x, "role"))
}

fn is_helper(l: &Layer) -> bool {
    role(l).is_some_and(|r| r == SECOND || r == FX)
}

fn flag(b: bool) -> String {
    if b { "True".into() } else { "False".into() }
}

fn num(xmp: &str, name: &str) -> Option<f32> {
    attr(xmp, name).and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite())
}

fn default_of(m: &LayerMask) -> f32 {
    m.surface.default_pixel().first().copied().unwrap_or(1.0)
}

/// A hidden, clipped, empty helper layer above `of`, with `mask` as its layer mask.
fn helper(of: &Layer, doc: &Document, name: String, mask: &LayerMask, attrs: &[(&str, String)]) -> Layer {
    let mut h = Layer::raster(name, doc.pixel_format());
    h.visible = false;
    h.clipped = true;
    h.mask = Some(mask.clone());
    h.xmp = Some(Arc::from(packet(attrs)));
    h.label = of.label;
    h
}

/// The helper carrying `l`'s two masks: its layer mask is the second, its alpha the first.
fn second_mask_helper(l: &Layer, doc: &Document) -> Option<Layer> {
    let (first, second) = (l.mask.as_ref()?, l.second_mask.as_ref()?);
    let attrs = [
        ("role", SECOND.to_string()),
        ("firstDefault", default_of(first).to_string()),
        ("firstDensity", first.density.to_string()),
        ("firstFeather", first.feather.to_string()),
        ("firstEnabled", flag(first.enabled)),
        ("firstLinked", flag(first.linked)),
    ];
    let mut h = helper(l, doc, format!("{} [PhotoCraft second mask]", l.name), second, &attrs);
    let area = first.surface.content_bounds().intersect(&doc.bounds());
    if !area.is_empty()
        && let Some(s) = h.surface_mut()
    {
        let n = s.format().channels().max(1);
        let raw = first.surface.read_region(area);
        let mut px = vec![0.0f32; raw.len() * n];
        for (v, p) in raw.iter().zip(px.chunks_exact_mut(n)) {
            if let Some(a) = p.last_mut() {
                *a = v.clamp(0.0, 1.0);
            }
        }
        s.write_region(area, &px);
        s.prune();
    }
    Some(h)
}

/// One helper per effect of `l` with a mask of its own, naming the effect by kind and by which of
/// that kind it is (Photoshop stores a layer's effects kind by kind, so their order changes).
fn effect_mask_helpers(l: &Layer, doc: &Document) -> Vec<Layer> {
    let mut out = Vec::new();
    for (i, e) in l.effects.items.iter().enumerate() {
        let Some(m) = e.mask() else { continue };
        let nth = l.effects.items[..i].iter().filter(|o| o.label() == e.label()).count();
        let attrs = [("role", FX.to_string()), ("effect", e.label().to_string()), ("nth", nth.to_string())];
        out.push(helper(l, doc, format!("{} [PhotoCraft {} mask]", l.name, e.label()), m, &attrs));
    }
    out
}

/// `xmp` (the layer's own packet, if any) with Lock Selection recorded in it.
fn with_selection_lock(xmp: Option<&str>) -> Option<String> {
    match xmp {
        None => Some(packet(&[("packet", "selectionLock".into()), ("selectionLock", "True".into())])),
        Some(x) if attr(x, "selectionLock").is_some() => Some(x.to_string()),
        Some(x) => {
            let at = x.find("<rdf:Description")? + "<rdf:Description".len();
            let ns = if x.contains(NS) { String::new() } else { format!(" xmlns:{PREFIX}=\"{NS}\"") };
            Some(format!("{}{ns} {PREFIX}:selectionLock=\"True\"{}", &x[..at], &x[at..]))
        }
    }
}

/// `xmp` without the Lock Selection PhotoCraft wrote into it: `None` when the packet was only
/// that. Returns whether the lock was recorded.
fn take_selection_lock(xmp: &str) -> (bool, Option<String>) {
    if !xmp.contains(NS) {
        return (false, Some(xmp.to_string()));
    }
    let locked = attr(xmp, "selectionLock").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    if attr(xmp, "packet").as_deref() == Some("selectionLock") {
        return (locked, None);
    }
    let rest = xmp.replace(&format!(" {PREFIX}:selectionLock=\"True\""), "").replace(&format!(" xmlns:{PREFIX}=\"{NS}\""), "");
    (locked, Some(rest))
}

fn needs_work(layers: &[Layer]) -> bool {
    layers
        .iter()
        .any(|l| l.second_mask.is_some() || l.locks.selection || l.effects.items.iter().any(|e| e.mask().is_some()) || l.children().is_some_and(needs_work))
}

/// `doc` ready for PSD export: helper layers, Lock Selection in the XMP, and the second masks
/// folded into the layer masks with their `PCm2` block ([`crate::second_mask_psd::for_export`]).
/// `None` when nothing needs it (export `doc` itself).
pub fn for_export(doc: &Document) -> Option<Document> {
    if !needs_work(&doc.layers) {
        return crate::second_mask_psd::for_export(doc);
    }
    fn walk(layers: &mut Vec<Layer>, doc: &Document) {
        let old = std::mem::take(layers);
        for mut l in old {
            if let LayerContent::Group(g) = &mut l.content {
                walk(&mut g.children, doc);
            }
            let mut helpers: Vec<Layer> = second_mask_helper(&l, doc).into_iter().collect();
            helpers.extend(effect_mask_helpers(&l, doc));
            if l.locks.selection
                && let Some(x) = with_selection_lock(l.xmp.as_deref())
            {
                l.xmp = Some(Arc::from(x));
            }
            layers.push(l);
            layers.extend(helpers);
        }
    }
    let mut out = doc.clone();
    walk(&mut out.layers, doc);
    Some(crate::second_mask_psd::for_export(&out).unwrap_or(out))
}

/// After import (and after [`crate::second_mask_psd::after_import`] restored the `PCm2` masks):
/// masks back from helper layers, which are removed, and Lock Selection from the XMP.
pub fn after_import(doc: &mut Document, warnings: &mut Vec<String>) {
    fn walk(layers: &mut Vec<Layer>, canvas: Rect, warnings: &mut Vec<String>) {
        for l in layers.iter_mut() {
            if let LayerContent::Group(g) = &mut l.content {
                walk(&mut g.children, canvas, warnings);
            }
            if let Some(x) = l.xmp.as_deref().filter(|x| x.contains(NS)).map(str::to_string)
                && !is_helper(l)
            {
                let (locked, rest) = take_selection_lock(&x);
                l.locks.selection |= locked;
                l.xmp = rest.map(Arc::from);
            }
        }
        // Layers are bottom first: a helper belongs to the nearest layer below it that isn't one.
        let mut i = 0;
        while i < layers.len() {
            if !is_helper(&layers[i]) {
                i += 1;
                continue;
            }
            let h = layers.remove(i);
            let Some(target) = layers[..i].iter_mut().rev().find(|l| !is_helper(l)) else {
                warnings.push(format!("\"{}\" (a PhotoCraft helper layer) has no layer below it; it was left out", h.name));
                continue;
            };
            match role(&h).as_deref() {
                Some(SECOND) => restore_second(target, &h, canvas, warnings),
                Some(FX) => restore_effect_mask(target, &h, warnings),
                _ => {}
            }
        }
    }
    let canvas = doc.bounds();
    walk(&mut doc.layers, canvas, warnings);
}

/// The first mask carried by a second-mask helper (its alpha), with its settings from the XMP.
fn first_from_helper(h: &Layer, xmp: &str, like: &LayerMask, canvas: Rect) -> LayerMask {
    let fmt = like.surface.format();
    let mut surface = Surface::with_default(fmt, &[num(xmp, "firstDefault").unwrap_or(1.0).clamp(0.0, 1.0)]);
    if let Some(s) = h.surface() {
        let area = s.content_bounds().intersect(&canvas);
        if !area.is_empty() {
            let n = s.format().channels().max(1);
            let px = s.read_region(area);
            let alpha: Vec<f32> = px.chunks_exact(n).map(|p| p.last().copied().unwrap_or(0.0)).collect();
            surface.write_region(area, &alpha);
            surface.prune();
        }
    }
    let on = |k: &str, d: bool| attr(xmp, k).map_or(d, |v| v.eq_ignore_ascii_case("true"));
    LayerMask {
        surface,
        enabled: on("firstEnabled", true),
        linked: on("firstLinked", true),
        density: num(xmp, "firstDensity").unwrap_or(1.0).clamp(0.0, 1.0),
        feather: num(xmp, "firstFeather").unwrap_or(0.0).clamp(0.0, 1000.0),
    }
}

/// Whether masks `a` and `b` read the same (within [`SAME`]) over the canvas.
fn same_values(a: &LayerMask, b: &LayerMask, canvas: Rect) -> bool {
    let area = a.surface.content_bounds().union(&b.surface.content_bounds()).intersect(&canvas);
    if (a.value(canvas.x1 + 10, canvas.y1 + 10) - b.value(canvas.x1 + 10, canvas.y1 + 10)).abs() > SAME && area != canvas {
        // Their defaults differ and some of the canvas reads the default.
        return false;
    }
    if area.is_empty() {
        return true;
    }
    let (mut va, mut vb) = (Vec::new(), Vec::new());
    a.values_into(area, &mut va);
    b.values_into(area, &mut vb);
    va.iter().zip(&vb).all(|(x, y)| (x - y).abs() <= SAME)
}

fn restore_second(target: &mut Layer, h: &Layer, canvas: Rect, warnings: &mut Vec<String>) {
    if target.second_mask.is_some() {
        // `PCm2` brought both masks back exactly.
        return;
    }
    let (Some(xmp), Some(second), Some(file)) = (h.xmp.as_deref(), h.mask.as_ref(), target.mask.as_ref()) else { return };
    let first = first_from_helper(h, xmp, file, canvas);
    let expected = if second.enabled { crate::second_mask_psd::multiplied(&first, second, canvas) } else { first.clone() };
    if same_values(&expected, file, canvas) {
        target.mask = Some(first);
        target.second_mask = Some(second.clone());
    } else {
        warnings.push(format!(
            "layer \"{}\": its mask was changed outside PhotoCraft, so it is kept as the layer's only mask (its second mask is folded into it)",
            target.name
        ));
    }
}

fn restore_effect_mask(target: &mut Layer, h: &Layer, warnings: &mut Vec<String>) {
    let (Some(xmp), Some(mask)) = (h.xmp.as_deref(), h.mask.as_ref()) else { return };
    let (Some(kind), nth) = (attr(xmp, "effect"), attr(xmp, "nth").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0)) else { return };
    match target.effects.items.iter_mut().filter(|e| e.label() == kind).nth(nth) {
        Some(e) => *e.mask_mut() = Some(mask.clone()),
        None => warnings.push(format!("layer \"{}\": its {kind} effect is gone, so that effect's mask was left out", target.name)),
    }
}

#[cfg(test)]
mod tests;
