use super::commands::{ASYNC_CHANNEL_THUMB_PIXELS, channel_images, channel_thumb_job};
use super::{DocId, Document, PhotocraftApp, mask_thumbs_ui};

impl PhotocraftApp {
    pub(super) fn prune_thumbs(&mut self) {
        let documents = self.session.documents();
        if self.thumb_documents.len() == documents.len()
            && self.thumb_documents.iter().zip(documents).all(|((id, snapshot), st)| *id == st.doc.id && snapshot.ptr_eq(&std::sync::Arc::downgrade(&st.doc)))
        {
            return;
        }
        // Check snapshots every sync, but walk the layer tree only after it changes. A Weak
        // tracks replacement (including undo and preserved-ID reopen) without retaining pixels.
        if !self.thumbs.is_empty() {
            let mut live = std::collections::HashSet::new();
            let mut pending: Vec<_> = documents.iter().flat_map(|st| &st.doc.layers).collect();
            while let Some(layer) = pending.pop() {
                live.insert((layer.id, mask_thumbs_ui::THUMB_LAYER));
                live.insert((layer.id, mask_thumbs_ui::THUMB_LAYER_BOUNDS));
                if layer.mask.is_some() {
                    live.insert((layer.id, mask_thumbs_ui::THUMB_MASK));
                }
                if layer.vector_mask.is_some() {
                    live.insert((layer.id, mask_thumbs_ui::THUMB_VECTOR));
                }
                if matches!(&layer.content, photocraft_doc::LayerContent::Smart(sm) if sm.filter_mask.is_some()) {
                    live.insert((layer.id, mask_thumbs_ui::THUMB_SMART_MASK));
                }
                if let Some(children) = layer.children() {
                    pending.extend(children);
                }
            }
            // Native files can restore IDs: retain the union across all open documents, rather
            // than deleting an ID just because one document containing it was closed.
            self.thumbs.retain(|key, _| live.contains(key));
            let any =
                |id: &photocraft_doc::LayerId| live.contains(&(*id, mask_thumbs_ui::THUMB_LAYER)) || live.contains(&(*id, mask_thumbs_ui::THUMB_LAYER_BOUNDS));
            self.thumb_frames.retain(|id, _| any(id));
            self.bounds_frames.retain(|id, _| live.contains(&(*id, mask_thumbs_ui::THUMB_LAYER_BOUNDS)));
        }
        self.thumb_documents = documents.iter().map(|st| (st.doc.id, std::sync::Arc::downgrade(&st.doc))).collect();
    }

    /// Cached 64px thumbnail of a pixel-ish layer, letterboxed in a square, and the document
    /// area it frames: the canvas, or with Clip Thumbnails to Layer Bounds the layer's pixels.
    pub fn layer_thumb(&mut self, ctx: &egui::Context, doc: &Document, layer: &photocraft_doc::Layer) -> (egui::TextureId, photocraft_geom::Rect) {
        let clip = self.session.prefs().interface.clip_thumbnails_to_layer_bounds;
        // Key by content, not document revision: COW tiles change pointer only when their pixels
        // change, so unrelated edits (e.g. painting another layer) don't rebuild this thumbnail.
        let rev = layer.surface().map_or(0, surface_fingerprint) ^ (doc.size.width as u64) << 40 ^ u64::from(clip) << 63;
        let key = (layer.id, mask_thumbs_ui::THUMB_LAYER);
        if let Some((r, tex)) = self.thumbs.get(&key)
            && *r == rev
            && let Some(frame) = self.thumb_frames.get(&layer.id)
        {
            return (tex.id(), *frame);
        }
        let canvas = doc.bounds();
        let frame = layer_thumb_frame(layer, canvas, clip);
        let mut px = [0.0f32; 8];
        let img = thumb_image(frame, 64, |x, y| {
            let Some(s) = layer.surface() else { return [0.0; 4] };
            let n = s.channels();
            s.read_pixel(x, y, &mut px[..n]);
            photocraft_raster::to_rgba(&s.format(), &px[..n])
        });
        self.thumb_frames.insert(layer.id, frame);
        (self.store_thumb(ctx, key, rev, img), frame)
    }

    /// The History panel thumbnail of snapshot `snap` of document `doc` (#2966): its composite,
    /// centred in a square. Thumbnails of `doc`'s snapshots not in `live` are dropped.
    pub fn snapshot_thumb(&mut self, ctx: &egui::Context, doc: DocId, snap: &photocraft_engine::history_cmds::Snapshot, live: &[u64]) -> egui::TextureId {
        self.snapshot_thumbs.retain(|(d, id), _| *d != doc || live.contains(id));
        if let Some(tex) = self.snapshot_thumbs.get(&(doc, snap.id)) {
            return tex.id();
        }
        let comp = photocraft_compose::thumbnail(&snap.doc, 56);
        let (w, h) = (comp.width as usize, comp.height as usize);
        let side = w.max(h);
        let (ox, oy) = ((side - w) / 2, (side - h) / 2);
        let mut px = vec![egui::Color32::TRANSPARENT; side * side];
        for (i, &[r, g, b, a]) in comp.pixels.as_chunks::<4>().0.iter().enumerate() {
            if let Some(out) = px.get_mut((i / w + oy) * side + i % w + ox) {
                *out = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
            }
        }
        let tex = ctx.load_texture(format!("snapshot-{}", snap.id), egui::ColorImage::new([side, side], px), egui::TextureOptions::LINEAR);
        let id = tex.id();
        self.snapshot_thumbs.insert((doc, snap.id), tex);
        id
    }

    /// Cached Layers panel thumbnail of a pixel-ish layer, as Photoshop's default (Layers Panel
    /// Options › Layer Bounds) shows it: just the layer's content bounds on the canvas, at their
    /// own aspect ratio, so a wide strip gets a wide thumbnail and a tall one a tall thumbnail. An
    /// empty layer shows the whole canvas. Returns the texture and its size in pixels (its aspect
    /// is the bounds').
    pub fn layer_bounds_thumb(&mut self, ctx: &egui::Context, doc: &Document, layer: &photocraft_doc::Layer) -> (egui::TextureId, [usize; 2]) {
        let rev = layer.surface().map_or(0, surface_fingerprint) ^ (doc.size.width as u64) << 40 ^ (doc.size.height as u64) << 20;
        let key = (layer.id, mask_thumbs_ui::THUMB_LAYER_BOUNDS);
        if let Some((r, tex)) = self.thumbs.get(&key)
            && *r == rev
            && let Some(bounds) = self.bounds_frames.get(&layer.id)
        {
            self.thumb_frames.insert(layer.id, *bounds);
            return (tex.id(), tex.size());
        }
        let canvas = photocraft_geom::Rect::from_size(doc.size);
        let bounds = layer.surface().map(|s| s.content_bounds().intersect(&canvas)).filter(|r| !r.is_empty()).unwrap_or(canvas);
        self.thumb_frames.insert(layer.id, bounds);
        self.bounds_frames.insert(layer.id, bounds);
        let mut px = [0.0f32; 8];
        let img = bounds_thumb_image(bounds, 64, |x, y| {
            let Some(s) = layer.surface() else { return [0.0; 4] };
            let n = s.channels();
            s.read_pixel(x, y, &mut px[..n]);
            photocraft_raster::to_rgba(&s.format(), &px[..n])
        });
        let size = img.size;
        (self.store_thumb(ctx, key, rev, img), size)
    }

    /// The cached thumbnail of `mask`, the layer mask of layer `id`.
    pub fn mask_thumb(&mut self, ctx: &egui::Context, doc: &Document, id: photocraft_doc::LayerId, mask: &photocraft_doc::LayerMask) -> egui::TextureId {
        self.mask_thumb_kind(ctx, doc, id, mask, mask_thumbs_ui::THUMB_MASK)
    }

    /// [`PhotocraftApp::mask_thumb`] cached under thumbnail `kind` (the second mask and Smart
    /// Filter masks have their own), re-rendered only when the mask's pixels or the document size
    /// change.
    pub fn mask_thumb_kind(
        &mut self,
        ctx: &egui::Context,
        doc: &Document,
        id: photocraft_doc::LayerId,
        mask: &photocraft_doc::LayerMask,
        kind: u8,
    ) -> egui::TextureId {
        let rev = surface_fingerprint(&mask.surface) ^ (doc.size.width as u64) << 40 ^ (doc.size.height as u64) << 16;
        let key = (id, kind);
        if let Some((r, tex)) = self.thumbs.get(&key)
            && *r == rev
        {
            return tex.id();
        }
        let mut v = [0.0f32; 1];
        let img = thumb_image(doc.bounds(), 64, |x, y| {
            mask.surface.read_pixel(x, y, &mut v);
            [v[0], v[0], v[0], 1.0]
        });
        self.store_thumb(ctx, key, rev, img)
    }

    pub(crate) fn store_thumb(&mut self, ctx: &egui::Context, key: (photocraft_doc::LayerId, u8), rev: u64, img: egui::ColorImage) -> egui::TextureId {
        match self.thumbs.get_mut(&key) {
            Some((r, tex)) => {
                tex.set(img, egui::TextureOptions::LINEAR);
                *r = rev;
                tex.id()
            }
            None => {
                let tex = ctx.load_texture(format!("thumb-{}-{}", key.0.0, key.1), img, egui::TextureOptions::LINEAR);
                let id = tex.id();
                self.thumbs.insert(key, (rev, tex));
                id
            }
        }
    }
}

/// Cheap identity of a surface's pixels: its default (untouched) pixel, tile coordinates and
/// `Arc` pointers. The default pixel matters: inverting or filling a tile-less mask only changes
/// it (#2117).
pub fn surface_fingerprint(s: &photocraft_raster::Surface) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ s.tile_count() as u64;
    for &b in s.default_bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    for (c, t) in s.tiles() {
        let p = std::sync::Arc::as_ptr(t) as usize as u64;
        h = (h ^ p ^ ((c.tx as u64) << 32 | c.ty as u32 as u64)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Thumbnail of document area `r` at its own aspect ratio, `side` pixels along its longer edge
/// (at least one pixel along the other), sampling `f(x, y)` in document space.
fn bounds_thumb_image(r: photocraft_geom::Rect, side: usize, mut f: impl FnMut(i32, i32) -> [f32; 4]) -> egui::ColorImage {
    let (w, h) = (r.width().max(1) as f32, r.height().max(1) as f32);
    let scale = w.max(h) / side.max(1) as f32;
    let (tw, th) = (((w / scale).round() as usize).clamp(1, side.max(1)), ((h / scale).round() as usize).clamp(1, side.max(1)));
    let mut px = vec![egui::Color32::TRANSPARENT; tw * th];
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    for ty in 0..th {
        for tx in 0..tw {
            let dx = (r.x0 as f32 + (tx as f32 + 0.5) * scale).min(r.x1 as f32 - 1.0);
            let dy = (r.y0 as f32 + (ty as f32 + 0.5) * scale).min(r.y1 as f32 - 1.0);
            let c = f(dx as i32, dy as i32);
            if let Some(p) = px.get_mut(ty * tw + tx) {
                *p = egui::Color32::from_rgba_unmultiplied(q(c[0]), q(c[1]), q(c[2]), q(c[3]));
            }
        }
    }
    egui::ColorImage::new([tw, th], px)
}

/// The document area a layer thumbnail frames: the canvas, or with `clip` the layer's pixels on
/// it (the canvas again when it has none there, so an empty layer still shows the page shape).
pub(crate) fn layer_thumb_frame(layer: &photocraft_doc::Layer, canvas: photocraft_geom::Rect, clip: bool) -> photocraft_geom::Rect {
    if !clip {
        return canvas;
    }
    let bounds = layer.surface().map_or(photocraft_geom::Rect::EMPTY, photocraft_compose::bounds::content_bounds).intersect(&canvas);
    if bounds.is_empty() { canvas } else { bounds }
}

/// Square thumbnail of the document area `frame`, letterboxed, sampling `f(x, y)` in document
/// space.
fn thumb_image(frame: photocraft_geom::Rect, side: usize, mut f: impl FnMut(i32, i32) -> [f32; 4]) -> egui::ColorImage {
    let (w, h) = (frame.width().max(1) as f32, frame.height().max(1) as f32);
    let scale = w.max(h) / side as f32;
    let (ox, oy) = ((side as f32 - w / scale) / 2.0, (side as f32 - h / scale) / 2.0);
    let mut px = vec![egui::Color32::TRANSPARENT; side * side];
    for ty in 0..side {
        for tx in 0..side {
            let dx = (tx as f32 - ox + 0.5) * scale;
            let dy = (ty as f32 - oy + 0.5) * scale;
            if dx < 0.0 || dy < 0.0 || dx >= w || dy >= h {
                continue;
            }
            let c = f(frame.x0.saturating_add(dx as i32), frame.y0.saturating_add(dy as i32));
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            px[ty * side + tx] = egui::Color32::from_rgba_unmultiplied(q(c[0]), q(c[1]), q(c[2]), q(c[3]));
        }
    }
    egui::ColorImage::new([side, side], px)
}

impl PhotocraftApp {
    /// Cached `Surface::content_bounds` keyed by an id and the surface's tile identity.
    pub fn cached_bounds(&mut self, key: u64, surface: &photocraft_raster::Surface) -> photocraft_geom::Rect {
        let rev = surface_fingerprint(surface);
        if let Some((r, b)) = self.bounds_cache.get(&key)
            && *r == rev
        {
            return *b;
        }
        let b = surface.content_bounds();
        if self.bounds_cache.len() > 256 {
            self.bounds_cache.clear();
        }
        self.bounds_cache.insert(key, (rev, b));
        b
    }

    /// The box Properties › Transform shows for `layer` (X, Y, W, H): its pixels' bounds, but a
    /// soft shape's path and stroke, whose Density and Feather render past them (#5846), as its
    /// transform frame does.
    pub fn layer_frame_bounds(&mut self, layer: &photocraft_doc::Layer, surface: &photocraft_raster::Surface) -> photocraft_geom::Rect {
        match &layer.content {
            photocraft_doc::LayerContent::Shape(sh) if sh.has_soft_mask() => {
                // Cached per rendering, like the pixel bounds: the frame renders strips of the shape.
                let (key, rev) = (layer.id.0 | 1 << 63, surface_fingerprint(surface));
                if let Some((r, b)) = self.bounds_cache.get(&key)
                    && *r == rev
                {
                    return *b;
                }
                let canvas = self.session.active().map_or_else(|| surface.content_bounds(), |st| st.doc.bounds());
                let b = photocraft_compose::soft_shape_frame(sh, surface.format(), canvas).unwrap_or(photocraft_geom::Rect::EMPTY);
                if self.bounds_cache.len() > 256 {
                    self.bounds_cache.clear();
                }
                self.bounds_cache.insert(key, (rev, b));
                b
            }
            _ => self.cached_bounds(layer.id.0, surface),
        }
    }
}

impl PhotocraftApp {
    /// Channels panel thumbnails of the active document snapshot: the composite,
    /// each colour channel (when there is more than one), each alpha channel, then the Quick Mask.
    pub fn channel_thumbs(&mut self, ctx: &egui::Context) -> Vec<egui::TextureId> {
        let Some(st) = self.session.active() else {
            self.channel_thumbs = None;
            return Vec::new();
        };
        let (id, doc) = (st.doc.id, st.doc.clone());
        // View-only commands bump revision without changing pixels. Keeping a Weak pins allocation
        // identity against address reuse without retaining the document's pixel data.
        let snapshot = std::sync::Arc::downgrade(&doc);
        let show_color = self.session.prefs().interface.show_channels_in_color;
        // A step that changes only the selection (a marquee, Deselect) makes a new snapshot with
        // the same pixels: keep the thumbnails, a full composite at 56 px costs ~0.7 s on a
        // layered 20 MP document (#4518).
        let current = match &mut self.channel_thumbs {
            Some((d, old, in_color, _)) if *d == id && *in_color == show_color => {
                let same = old.ptr_eq(&snapshot) || old.upgrade().is_some_and(|prev| same_but_selection(&prev, &doc));
                if same {
                    *old = snapshot.clone();
                }
                same
            }
            _ => false,
        };
        if !current {
            // A big document builds them on a background thread (user report: switching to the
            // Channels panel after an edit froze for its composite); until they are ready the
            // panel keeps showing the document's previous thumbnails.
            let built = if cfg!(target_arch = "wasm32") || doc.size.area() <= ASYNC_CHANNEL_THUMB_PIXELS {
                Some(channel_images(&doc, show_color))
            } else {
                channel_thumb_job(ctx, id, &doc, show_color)
            };
            let Some(texs) = built else {
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
                return match &self.channel_thumbs {
                    Some((d, _, _, handles)) if *d == id => handles.iter().map(|t| t.id()).collect(),
                    _ => Vec::new(),
                };
            };
            // Update the textures already uploaded rather than allocating new ones.
            let mut handles = match self.channel_thumbs.take() {
                Some((d, _, _, h)) if d == id => h,
                _ => Vec::new(),
            };
            handles.truncate(texs.len());
            for (i, (name, image)) in texs.into_iter().enumerate() {
                match handles.get_mut(i) {
                    Some(h) => h.set(image, egui::TextureOptions::LINEAR),
                    None => handles.push(ctx.load_texture(name, image, egui::TextureOptions::LINEAR)),
                }
            }
            self.channel_thumbs = Some((id, snapshot, show_color, handles));
        }
        self.channel_thumbs.as_ref().map(|(_, _, _, t)| t.iter().map(|t| t.id()).collect()).unwrap_or_default()
    }
}

/// Whether `new` differs from `old` at most in its selection, so every composite and channel
/// thumbnail of it is unchanged. Tiles compare by pointer first, so this costs little.
pub(super) fn same_but_selection(old: &Document, new: &Document) -> bool {
    if old.selection == new.selection {
        return old == new;
    }
    let mut o = old.clone();
    o.selection = new.selection.clone();
    o == *new
}

/// A channel thumbnail pixel whose lightness is `byte` (255 = white: full light, no ink).
/// Preferences ▸ Interface ▸ Show Channels in Color tints RGB channels from black to their
/// primary and CMYK channels from white to their ink, as Photoshop does; otherwise grey.
pub(super) fn channel_tint(mode: photocraft_doc::ColorMode, k: usize, byte: u8, in_color: bool) -> egui::Color32 {
    use photocraft_doc::ColorMode::{Cmyk, Rgb};
    if !in_color {
        return egui::Color32::from_gray(byte);
    }
    match (mode, k) {
        (Rgb, 0) => egui::Color32::from_rgb(byte, 0, 0),
        (Rgb, 1) => egui::Color32::from_rgb(0, byte, 0),
        (Rgb, 2) => egui::Color32::from_rgb(0, 0, byte),
        (Cmyk, 0) => egui::Color32::from_rgb(byte, 255, 255),
        (Cmyk, 1) => egui::Color32::from_rgb(255, byte, 255),
        (Cmyk, 2) => egui::Color32::from_rgb(255, 255, byte),
        _ => egui::Color32::from_gray(byte),
    }
}

#[cfg(test)]
mod channel_tint_tests {
    use super::channel_tint;
    use egui::Color32;
    use photocraft_doc::ColorMode::{Cmyk, Grayscale, Rgb};

    #[test]
    fn channels_in_color_tint_rgb_from_black_and_cmyk_inks_from_white() {
        assert_eq!(channel_tint(Rgb, 0, 255, true), Color32::from_rgb(255, 0, 0));
        assert_eq!(channel_tint(Rgb, 2, 0, true), Color32::BLACK);
        // CMYK: no ink is white, full ink is the ink's colour.
        assert_eq!(channel_tint(Cmyk, 0, 255, true), Color32::WHITE);
        assert_eq!(channel_tint(Cmyk, 0, 0, true), Color32::from_rgb(0, 255, 255));
        assert_eq!(channel_tint(Cmyk, 1, 0, true), Color32::from_rgb(255, 0, 255));
        assert_eq!(channel_tint(Cmyk, 2, 0, true), Color32::from_rgb(255, 255, 0));
        assert_eq!(channel_tint(Cmyk, 3, 0, true), Color32::BLACK);
        assert_eq!(channel_tint(Grayscale, 0, 77, true), Color32::from_gray(77));
        assert_eq!(channel_tint(Rgb, 0, 77, false), Color32::from_gray(77));
    }
}
