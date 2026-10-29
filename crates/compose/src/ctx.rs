use super::content::empty_in;
use super::fx_cache::{effect_maps, fx_over_budget, layer_identity};
use super::{
    Buffer, Document, Layer, LayerContent, PROXY_THUMBNAIL_PIXELS, Rect, adjust, advanced, effects, pattern, proxy, render_bands, thumbnail_buffer,
    thumbnail_size,
};

/// The composite of an area area-averaged (premultiplied) down to a smaller size band by band,
/// exactly as [`render_reduced`] does it: fed the bands of a render the caller makes anyway, so the
/// merged image a PSD save renders gives its thumbnail too, and one render gives a .pcraft save
/// both its thumbnail and its preview.
pub struct Reducer {
    fw: usize,
    fh: usize,
    w: usize,
    h: usize,
    /// Output pixels made, and the document area they read.
    out: Rect,
    src: Rect,
    b: Rect,
    /// Output column of each source column of `src`, and output row of each document row of `b`.
    cols: Vec<u32>,
    rows: Vec<u32>,
    acc: Vec<[f32; 4]>,
    /// The next source row a band must start at (`None` once bands came out of order).
    next: Option<i32>,
}

impl Reducer {
    /// The reduction of all of `b` (a document's bounds) to `w`×`h` (clamped to `b`'s size), or
    /// `None` when there is nothing to reduce (`b` is empty, or `w`×`h` is its own size).
    pub fn new(b: Rect, w: u32, h: u32) -> Option<Self> {
        Self::over(b, w, h, None).ok()
    }

    /// [`Self::new`] for the output pixels a change to `damage` affects. `Err(Some(out))`: `w`×`h`
    /// is `b`'s own size (output pixels `out` are document pixels); `Err(None)`: nothing to make.
    fn over(b: Rect, w: u32, h: u32, damage: Option<Rect>) -> Result<Self, Option<Rect>> {
        let (fw, fh) = (b.width() as usize, b.height() as usize);
        let (w, h) = (w.clamp(1, b.width().max(1)) as usize, h.clamp(1, b.height().max(1)) as usize);
        let full = Rect::from_xywh(0, 0, w as u32, h as u32);
        if fw == 0 || fh == 0 {
            return Err(None);
        }
        // The output pixel whose span holds document column / row `x` (spans are contiguous: n <= f).
        let cell = |x: usize, f: usize, n: usize| ((x + 1) * n).saturating_sub(1) / f;
        // Output pixels to produce and the document area they cover.
        let out = match damage {
            None => full,
            Some(d) => {
                let d = d.intersect(&b);
                if d.is_empty() {
                    return Err(None);
                }
                let (x0, x1) = ((d.x0 - b.x0) as usize, (d.x1 - b.x0) as usize);
                let (y0, y1) = ((d.y0 - b.y0) as usize, (d.y1 - b.y0) as usize);
                Rect::new(cell(x0, fw, w) as i32, cell(y0, fh, h) as i32, cell(x1 - 1, fw, w) as i32 + 1, cell(y1 - 1, fh, h) as i32 + 1)
            }
        };
        let (ox0, oy0, ow, oh) = (out.x0 as usize, out.y0 as usize, out.width() as usize, out.height() as usize);
        let (sx0, sx1) = (reduce_span(ox0, fw, w).0, reduce_span(ox0 + ow - 1, fw, w).1);
        let (sy0, sy1) = (reduce_span(oy0, fh, h).0, reduce_span(oy0 + oh - 1, fh, h).1);
        let src = Rect::new(b.x0 + sx0 as i32, b.y0 + sy0 as i32, b.x0 + sx1 as i32, b.y0 + sy1 as i32);
        if (w, h) == (fw, fh) {
            return Err(Some(out));
        }
        let map = |f: usize, n: usize| {
            let mut m = vec![0u32; f];
            for t in 0..n {
                let (a, z) = reduce_span(t, f, n);
                m[a..z].fill(t as u32);
            }
            m
        };
        let (cols, rows) = (map(fw, w), map(fh, h));
        let cols = cols[sx0..sx1].to_vec();
        Ok(Self { fw, fh, w, h, out, src, b, cols, rows, acc: vec![[0.0f32; 4]; ow * oh], next: Some(src.y0) })
    }

    /// The document area whose composite this reduction reads: the bands given to [`Self::add`]
    /// must cover it top to bottom, each as wide as it.
    pub fn source(&self) -> Rect {
        self.src
    }

    /// Adds the next band of the composite of [`Self::source`] (its rows in order after the
    /// band before). A band that isn't the next one spoils the reduction ([`Self::finish`] gives
    /// `None`).
    pub fn add(&mut self, band: &Buffer) {
        let Some(next) = self.next else { return };
        if band.rect.x0 != self.src.x0 || band.rect.x1 != self.src.x1 || band.rect.y0 != next || band.rect.y1 > self.src.y1 || band.rect.is_empty() {
            self.next = None;
            return;
        }
        self.next = Some(band.rect.y1);
        let (b, fh, h, ox0, oy0, ow) = (self.b, self.fh, self.h, self.out.x0 as usize, self.out.y0 as usize, self.out.width() as usize);
        let (by0, by1) = ((band.rect.y0 - b.y0) as usize, (band.rect.y1 - b.y0) as usize);
        let (t0, t1) = (self.rows[by0] as usize, self.rows[by1 - 1] as usize + 1);
        let sw = self.cols.len();
        let cols = &self.cols;
        // Each output row sums its source rows in this band (in order, top to bottom).
        let sum_row = |t: usize, out: &mut [[f32; 4]]| {
            let (a, z) = reduce_span(t, fh, h);
            for y in a.max(by0)..z.min(by1) {
                let src = &band.px[(y - by0) * sw..(y - by0 + 1) * sw];
                for (p, &c) in src.iter().zip(cols) {
                    let o = &mut out[c as usize - ox0];
                    for i in 0..3 {
                        o[i] += p[i] * p[3];
                    }
                    o[3] += p[3];
                }
            }
        };
        let part = &mut self.acc[(t0 - oy0) * ow..(t1 - oy0) * ow];
        #[cfg(not(target_arch = "wasm32"))]
        {
            use rayon::prelude::*;
            part.par_chunks_mut(ow).enumerate().for_each(|(i, out)| sum_row(t0 + i, out));
        }
        #[cfg(target_arch = "wasm32")]
        for (i, out) in part.chunks_mut(ow).enumerate() {
            sum_row(t0 + i, out);
        }
    }

    /// The reduced image (output pixels from (0, 0)), once every row of [`Self::source`] was
    /// added in order; `None` otherwise.
    pub fn finish(self) -> Option<Buffer> {
        if self.next != Some(self.src.y1) {
            return None;
        }
        let (fw, fh, w, h) = (self.fw, self.fh, self.w, self.h);
        let (ox0, oy0, ow) = (self.out.x0 as usize, self.out.y0 as usize, self.out.width() as usize);
        let mut acc = self.acc;
        for (i, p) in acc.iter_mut().enumerate() {
            let ((x0, x1), (y0, y1)) = (reduce_span(ox0 + i % ow, fw, w), reduce_span(oy0 + i / ow, fh, h));
            let n = ((y1 - y0) * (x1 - x0)).max(1) as f32;
            let a = p[3];
            *p = if a > 0.0 { [p[0] / a, p[1] / a, p[2] / a, a / n] } else { [0.0; 4] };
        }
        Some(Buffer { rect: self.out, px: acc })
    }
}

/// Output column / row `t` of `n` covers document columns / rows `[t·f/n, (t+1)·f/n)`.
fn reduce_span(t: usize, f: usize, n: usize) -> (usize, usize) {
    (t * f / n, ((t + 1) * f / n).max(t * f / n + 1).min(f))
}

/// The reduction [`thumbnail_buffer`]`(doc, max_side)` makes from the full-size composite, or
/// `None` when it takes another way (a reduced proxy of a big document, or no reduction at all).
pub fn thumbnail_reducer(doc: &Document, max_side: u32) -> Option<Reducer> {
    let b = doc.bounds();
    let longest = b.width().max(b.height()).max(1);
    let k = longest / max_side.max(1).saturating_mul(2);
    if k >= 2 && doc.size.area() > PROXY_THUMBNAIL_PIXELS && proxy::proxy_faithful(doc) {
        return None;
    }
    let (w, h) = thumbnail_size(doc, max_side);
    Reducer::new(b, w, h)
}

/// [`thumbnail_buffer`] for several sizes: those made from the full-size composite share one
/// render of it (a .pcraft save's thumbnail and preview rendered it twice). `progress` gets the
/// share done, 0..=1, per band of that render and as each other size is made.
pub fn thumbnail_buffers(doc: &Document, sides: &[u32], progress: &mut dyn FnMut(f32)) -> Vec<Buffer> {
    let mut reducers: Vec<Option<Reducer>> = sides.iter().map(|&s| thumbnail_reducer(doc, s)).collect();
    let shared = reducers.iter().flatten().count();
    if shared > 1 {
        let b = doc.bounds();
        let rows = b.height().max(1) as f32;
        let ok: Result<(), ()> = render_bands(doc, b, 0, |band| {
            reducers.iter_mut().flatten().for_each(|r| r.add(&band));
            progress((band.rect.y1 - b.y0) as f32 / rows * shared as f32 / sides.len() as f32);
            Ok(())
        });
        debug_assert!(ok.is_ok());
    } else {
        reducers.iter_mut().for_each(|r| *r = None);
    }
    let first = if shared > 1 { shared } else { 0 };
    let mut made = first;
    sides
        .iter()
        .zip(reducers)
        .map(|(&s, r)| match r.and_then(Reducer::finish) {
            Some(b) => b,
            None => {
                let b = thumbnail_buffer(doc, s);
                made += 1;
                progress(made as f32 / sides.len().max(1) as f32);
                b
            }
        })
        .collect()
}

type VectorMaskSlot = std::sync::Arc<std::sync::OnceLock<photocraft_vector::CompiledVectorMask>>;

#[derive(Default)]
pub(crate) struct RenderVectorMasks {
    /// Compiled masks by content. Callers pass temporary clones (see `advanced.rs`), so an
    /// address cannot identify a mask: a dropped clone's address is reused by the next one.
    masks: std::sync::Mutex<Vec<(photocraft_doc::VectorMask, VectorMaskSlot)>>,
}

impl RenderVectorMasks {
    pub(crate) fn values(&self, mask: &photocraft_doc::VectorMask, rect: Rect) -> Vec<f32> {
        let slot = {
            let mut masks = self.masks.lock().unwrap_or_else(|e| e.into_inner());
            match masks.iter().find(|(m, _)| m == mask) {
                Some((_, slot)) => slot.clone(),
                None => {
                    let slot = VectorMaskSlot::default();
                    masks.push((mask.clone(), slot.clone()));
                    slot
                }
            }
        };
        // Compilation is sequential. Rendering can enter Rayon and must happen after
        // initialization, so waiting tiles cannot re-enter a slot still being built.
        let compiled = slot.get_or_init(|| photocraft_vector::CompiledVectorMask::new(mask));
        compiled.render(rect)
    }
}

/// Rendering context shared down the tree.
pub(crate) struct Ctx<'a> {
    /// Document canvas: fill layers and gradients are laid out relative to it, never to the render rect.
    pub(crate) canvas: Rect,
    /// Tone transfer used by adjustments that work in linear light.
    pub(crate) transfer: adjust::Transfer,
    /// Global light for layer effects.
    pub(crate) light: photocraft_doc::GlobalLight,
    /// Prepared document patterns, shared across all tiles and bands of this render call.
    pub(crate) patterns: &'a pattern::PreparedPatterns<'a>,
    /// The document's colour mode (channel restrictions name its channels).
    pub(crate) mode: photocraft_color::ColorMode,
    pub(crate) depth: photocraft_color::SampleType,
    /// At most 1 MiB of tone tables, initialized once and shared by this render's tiles and bands.
    pub(crate) tone_luts: Vec<(&'a photocraft_doc::Adjustment, std::sync::OnceLock<[Vec<f32>; 4]>)>,
    pub(crate) vector_masks: RenderVectorMasks,
    /// Effect maps used by this render, by cache key: built before the parallel tiles (see
    /// [`prepare_effects`]) and held for the whole render (or band, for maps clipped to it: the
    /// `bool`), so eviction can't force a rebuild.
    pub(crate) fx_maps: std::sync::Mutex<std::collections::HashMap<u64, (std::sync::Arc<effects::FxMaps>, bool)>>,
    /// The rectangle being rendered (a band of [`render_bands`]); see [`effect_region`].
    pub(crate) fx_clip: std::sync::Mutex<Option<Rect>>,
    /// The whole-region effect maps of this render's layers wouldn't fit in the effect cache
    /// together ([`fx_over_budget`]), so local ones are clipped to the render rect (#8330).
    pub(crate) fx_over_budget: bool,
    /// The document being rendered (deep knockouts reveal its Background); `None` for a lone layer.
    pub(crate) doc: Option<&'a Document>,
    /// Effect-map identities of this render's document layers (see [`LayerKeys`]).
    pub(crate) layer_keys: LayerKeys,
}

/// Memo of [`layer_identity`] for the layers of the document being rendered. The identity depends
/// on the layer alone, not on the tile, so it is computed once per layer per render instead of
/// once per tile. Only layers that sit in the document tree are memoised by address: the document
/// is borrowed for the whole render, so no other object can live at such an address. Temporary
/// copies (`advanced::effects_source`) are never in the tree and always take the full path.
#[derive(Default)]
pub(crate) struct LayerKeys {
    /// Addresses of every layer in the document tree, collected on first use.
    in_doc: std::sync::OnceLock<std::collections::HashSet<usize>>,
    ids: std::sync::Mutex<std::collections::HashMap<usize, u64>>,
    /// Identities computed (tests count them).
    #[cfg(test)]
    pub(crate) built: std::sync::atomic::AtomicUsize,
}

impl LayerKeys {
    pub(crate) fn identity(&self, layer: &Layer, doc: Option<&Document>) -> u64 {
        let addr = std::ptr::from_ref(layer) as usize;
        let in_doc = doc.is_some_and(|d| self.in_doc.get_or_init(|| doc_layer_addresses(d)).contains(&addr));
        if in_doc {
            let mut ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
            // Computing an identity only hashes immutable layer data; it does not run Rayon
            // work or re-enter this memo. Keep the miss and insertion atomic across callers.
            *ids.entry(addr).or_insert_with(|| self.compute(layer))
        } else {
            self.compute(layer)
        }
    }

    fn compute(&self, layer: &Layer) -> u64 {
        use std::hash::Hasher;
        #[cfg(test)]
        self.built.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        layer_identity(layer, &mut h, &mut |_| {});
        h.finish()
    }
}

/// Addresses of all layers of `doc` (groups' children included).
fn doc_layer_addresses(doc: &Document) -> std::collections::HashSet<usize> {
    let mut out = std::collections::HashSet::new();
    let mut todo: Vec<&Layer> = doc.layers.iter().collect();
    while let Some(l) = todo.pop() {
        out.insert(std::ptr::from_ref(l) as usize);
        if let LayerContent::Group(g) = &l.content {
            todo.extend(g.children.iter());
        }
    }
    out
}

impl<'a> Ctx<'a> {
    pub(crate) fn for_doc(doc: &'a Document, patterns: &'a pattern::PreparedPatterns<'a>) -> Self {
        Self {
            doc: Some(doc),
            canvas: doc.bounds(),
            transfer: adjust::Transfer::for_document(doc.mode, doc.depth),
            light: doc.global_light,
            patterns,
            mode: doc.mode,
            depth: doc.depth,
            vector_masks: RenderVectorMasks::default(),
            fx_maps: Default::default(),
            fx_clip: Default::default(),
            fx_over_budget: fx_over_budget(&doc.layers, doc.bounds()),
            layer_keys: Default::default(),
            tone_luts: prepare_tones(&doc.layers, 0),
        }
    }

    /// Start rendering `rect`: effect maps clipped to the previous rectangle are let go.
    pub(crate) fn set_fx_clip(&self, rect: Rect) {
        let mut clip = self.fx_clip.lock().unwrap_or_else(|e| e.into_inner());
        if *clip != Some(rect) {
            *clip = Some(rect);
            self.fx_maps.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, (_, clipped)| !*clipped);
        }
    }

    pub(crate) fn apply_adjustment(&self, adj: &photocraft_doc::Adjustment, buf: &mut Buffer) {
        if let photocraft_doc::Adjustment::Levels { space, .. } | photocraft_doc::Adjustment::Curves { space, .. } = adj
            && !matches!(adj, photocraft_doc::Adjustment::Levels { space: photocraft_doc::adjust::ToneSpace::Rgb, .. } if self.depth == photocraft_color::SampleType::F32)
            && let Some((_, luts)) = self.tone_luts.iter().find(|(a, _)| std::ptr::eq(*a, adj))
        {
            let identity = adjust::float_tone_identity(adj, Some(self.depth));
            if identity == [true; 3] {
                return;
            }
            let extrapolate = adjust::float_tone_extrapolates(adj, Some(self.depth), self.mode);
            adjust::apply_tone(buf, *space, luts.get_or_init(|| adjust::tone_luts_depth(adj, Some(self.depth))), identity, extrapolate);
        } else {
            adjust::apply_in(adj, buf, self.transfer, Some(self.depth), self.mode);
        }
    }
}

pub(crate) fn prepare_tones(layers: &[Layer], depth: u32) -> Vec<(&photocraft_doc::Adjustment, std::sync::OnceLock<[Vec<f32>; 4]>)> {
    let mut out = Vec::new();
    for layer in layers.iter().filter(|l| l.visible) {
        if out.len() == 16 {
            break;
        }
        match &layer.content {
            LayerContent::Adjustment(a @ (photocraft_doc::Adjustment::Levels { .. } | photocraft_doc::Adjustment::Curves { .. })) => {
                out.push((a, Default::default()))
            }
            LayerContent::Group(g) if depth < PREPARE_DEPTH => out.extend(prepare_tones(&g.children, depth + 1).into_iter().take(16 - out.len())),
            _ => {}
        }
    }
    out
}

/// Deepest group nesting preparation walks (deeper layers prepare on demand).
pub(crate) const PREPARE_DEPTH: u32 = 64;

/// Build the effect maps of every visible layer in `layers` (groups included) that can reach
/// `rect` before tiles render in parallel, layers concurrently on native targets, each build
/// inside `setting` (the tiles' thread-local colour setting). Tiles then only read finished maps:
/// none builds one while others need it (#276), and none duplicates another's build. Builds never
/// wait on each other (`cached_effect_maps`), so building them in parallel can't deadlock.
pub(crate) fn prepare_effects(layers: &[Layer], rect: Rect, cx: &Ctx, setting: impl Fn(&mut dyn FnMut()) + Sync) {
    let mut todo = Vec::new();
    effect_layers(layers, rect, 0, &mut todo);
    let build = |l: &std::borrow::Cow<Layer>| {
        setting(&mut || {
            let _ = effect_maps(l, rect, cx);
        });
    };
    #[cfg(not(target_arch = "wasm32"))]
    if todo.len() > 1 {
        use rayon::prelude::*;
        todo.par_iter().for_each(build);
        return;
    }
    todo.iter().for_each(build);
}

/// The visible layers of `layers` (groups included) with effects that can reach `rect`.
pub(crate) fn effect_layers<'l>(layers: &'l [Layer], rect: Rect, depth: u32, out: &mut Vec<std::borrow::Cow<'l, Layer>>) {
    let mut base_visible = true;
    for l in layers {
        if !l.clipped {
            base_visible = l.visible;
        }
        // A clipping group is drawn only when its base is.
        if !l.visible || !base_visible {
            continue;
        }
        if effects::has_effects(l) && !empty_in(l, rect) {
            // A mask that hides the effects is left out of the shape they are built from.
            out.push(advanced::effects_source(l));
        }
        if let LayerContent::Group(g) = &l.content
            && depth < PREPARE_DEPTH
        {
            effect_layers(&g.children, rect, depth + 1, out);
        }
    }
}
