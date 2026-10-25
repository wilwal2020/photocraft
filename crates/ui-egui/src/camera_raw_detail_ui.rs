//! Full-resolution view of Camera Raw's preview where the proxy has too few pixels (zoomed in).
//!
//! Only the visible part is developed, with a margin around it and from the image-wide
//! measurements of the proxy's develop ([`photocraft_algo::camera_raw::develop_region`]), so it
//! follows a slider while it moves instead of the proxy's softer pixels (user report: Photoshop
//! keeps full quality while dragging). Work runs off the UI thread, one region at a time, and the
//! latest finished region stays on screen until the next lands, so the view never drops to the
//! proxy's resolution once it has a region.
use egui::{Color32, ColorImage, Context, Pos2, Rect, TextureHandle, TextureOptions, pos2, vec2};
use photocraft_algo::camera_raw::{CameraRaw, Context as Measured, DevelopCache, Frame};
use photocraft_geom::Rect as PixelRect;
use photocraft_raster::Surface;
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, TryRecvError},
};

#[derive(Clone)]
pub(crate) enum Coverage {
    Selection(Surface),
    Mask(photocraft_doc::LayerMask),
    None,
}

/// A developed region: the preview revision it is for, the document pixels it covers, its pixels
/// (straight RGBA, coverage applied) and them as the canvas shows them.
struct Region {
    revision: u64,
    crop: PixelRect,
    px: Vec<[f32; 4]>,
    image: Option<ColorImage>,
}

type RegionResult = Result<Region, String>;
type OverlayKey = (u64, bool, PixelRect, crate::camera_raw_scope_ui::ClippingMode, crate::theme::ThemeKind);

/// Largest region (with its margin and a distorted source) one develop reads.
const MAX_REGION_PIXELS: u64 = 64_000_000;

pub(crate) struct DetailPreview {
    source: Surface,
    area: PixelRect,
    coverage: Coverage,
    float: bool,
    /// The latest finished region (shown until a newer one lands).
    result: Option<Region>,
    pending: Option<(u64, PixelRect, Receiver<RegionResult>)>,
    cancel: Arc<AtomicBool>,
    pub(crate) error: Option<String>,
    failed: Option<(u64, PixelRect)>,
    /// What the texture shows: (revision, original, crop, display key).
    crop: Option<(u64, bool, PixelRect, u64)>,
    texture: Option<TextureHandle>,
    overlay_texture: Option<TextureHandle>,
    overlay_key: Option<OverlayKey>,
    /// The region develops' cache (light and presence stages for the region in view).
    cache: Arc<Mutex<DevelopCache>>,
    pub(crate) ready: bool,
}

impl Drop for DetailPreview {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl DetailPreview {
    pub(crate) fn new(source: Surface, area: PixelRect, coverage: Coverage) -> Self {
        let float = source.format().sample == photocraft_color::SampleType::F32;
        Self {
            source,
            area,
            coverage,
            float,
            result: None,
            pending: None,
            cancel: Arc::new(AtomicBool::new(false)),
            error: None,
            failed: None,
            crop: None,
            texture: None,
            overlay_texture: None,
            overlay_key: None,
            cache: Default::default(),
            ready: false,
        }
    }

    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    fn poll(&mut self) {
        let Some((revision, crop, rx)) = &self.pending else { return };
        let (revision, crop) = (*revision, *crop);
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Camera Raw detail preview worker stopped".into()),
        };
        self.pending = None;
        match result {
            Ok(region) => {
                self.result = Some(region);
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e);
                self.failed = Some((revision, crop));
            }
        }
    }

    /// Starts developing `crop` for preview `revision` (settings `params`, measurements
    /// `context`) unless a develop is running (the next frame asks again).
    #[allow(clippy::too_many_arguments)]
    fn request(&mut self, ctx: &Context, revision: u64, crop: PixelRect, params: &CameraRaw, context: &Arc<Measured>, display: &Arc<photocraft_engine::display_color::CanvasDisplay>) {
        if self.pending.is_some() || self.failed == Some((revision, crop)) {
            return;
        }
        let job = RegionJob {
            source: self.source.clone(),
            area: self.area,
            coverage: self.coverage.clone(),
            float: self.float,
            params: params.clone(),
            context: context.clone(),
            crop,
            revision,
            display: display.clone(),
            cache: self.cache.clone(),
        };
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel::<RegionResult>();
        let work = move || {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run()))
                .unwrap_or_else(|_| Err("Camera Raw detail preview failed; the proxy remains available".into()));
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(result);
                ctx.request_repaint();
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            match std::thread::Builder::new().name("camera-raw-region".into()).spawn(work) {
                Ok(_) => self.pending = Some((revision, crop, rx)),
                Err(e) => {
                    self.error = Some(format!("Cannot start Camera Raw preview: {e}"));
                    self.failed = Some((revision, crop));
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            // No browser worker/executor is available here. Preserve the interactive proxy
            // instead of blocking its UI with full-resolution develops.
            drop(work);
            drop(rx);
            self.error = Some("Full-resolution filtered preview requires a native Camera Raw worker; the web preview keeps the proxy".into());
            self.failed = Some((revision, crop));
        }
    }

    /// Draws the full-resolution view of the visible part of `image` when `needed`: the original
    /// (`before`, or neutral settings) straight from the source, else the latest developed region
    /// (asking for one of `revision` covering the view when it isn't). Only a bounded crop around
    /// what is visible is uploaded; no full-image texture.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint(
        &mut self,
        ui: &egui::Ui,
        image: Rect,
        revision: u64,
        before: bool,
        params: &CameraRaw,
        context: &Arc<Measured>,
        needed: bool,
        display: &Arc<photocraft_engine::display_color::CanvasDisplay>,
    ) -> bool {
        self.ready = false;
        self.poll();
        if !needed {
            return false;
        }
        let Some(visible) = visible_pixels(image, ui.clip_rect(), self.area, ui.input(|i| i.max_texture_side)) else { return false };
        let original = before || params.is_identity();
        // A little margin around the view: panning or zooming in a little keeps what is there (a
        // wider one costs every develop while a slider moves).
        let wide = {
            let max_side = ui.input(|i| i.max_texture_side) as u32;
            let (mx, my) = ((visible.width() / 8) as i32, (visible.height() / 8) as i32);
            let wide = PixelRect::new(visible.x0 - mx, visible.y0 - my, visible.x1 + mx, visible.y1 + my).intersect(&self.area);
            let fits = wide.width() <= max_side && wide.height() <= max_side && wide.width() as u64 * wide.height() as u64 <= 16_777_216;
            if fits { wide } else { visible }
        };
        let key = if original {
            let reuse = self.crop.filter(|(r, o, c, d)| (*r, *o, *d) == (0, true, display.key) && c.contains_rect(&visible));
            let key = reuse.unwrap_or((0, true, wide, display.key));
            if self.crop != Some(key) {
                self.upload(ui, key, crop_image(&self.source, key.2, display));
            }
            key
        } else {
            let current = self.result.as_ref().is_some_and(|r| r.revision == revision && r.crop.contains_rect(&visible));
            if !current {
                self.request(ui.ctx(), revision, wide, params, context, display);
                self.poll();
            }
            // The latest region, even one developed for the settings a step back: it stays sharp
            // while the next one develops.
            let Some(region) = self.result.as_mut() else { return false };
            let key = (region.revision, false, region.crop, display.key);
            if self.crop != Some(key) {
                let image = region.image.take().unwrap_or_else(|| {
                    let (w, px) = (region.crop.width() as usize, &region.px);
                    let crop = region.crop;
                    display_image(crop, display, |r, out| {
                        let start = ((r.y0 - crop.y0).max(0) as usize).saturating_mul(w);
                        if let Some(src) = px.get(start..start.saturating_add(out.len())) {
                            out.copy_from_slice(src);
                        }
                    })
                });
                self.upload(ui, key, image);
            }
            key
        };
        if let Some(texture) = &self.texture {
            let crop = key.2;
            let scale = image.size() / vec2(self.area.width() as f32, self.area.height() as f32);
            let screen = Rect::from_min_size(
                image.min + vec2((crop.x0 - self.area.x0) as f32, (crop.y0 - self.area.y0) as f32) * scale,
                vec2(crop.width() as f32, crop.height() as f32) * scale,
            );
            ui.painter().image(texture.id(), screen, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
            self.ready = original || (key.0 == revision && crop.contains_rect(&visible));
        }
        true
    }

    fn upload(&mut self, ui: &egui::Ui, key: (u64, bool, PixelRect, u64), color: ColorImage) {
        let options = TextureOptions { magnification: egui::TextureFilter::Nearest, ..TextureOptions::LINEAR };
        match &mut self.texture {
            Some(t) => t.set(color, options),
            None => self.texture = Some(ui.ctx().load_texture("cr-detail-preview", color, options)),
        }
        self.crop = Some(key);
    }

    /// The full-resolution pixel at `p` (0–1 across the image) when the view shows it.
    pub(crate) fn sample(&self, p: [f32; 2], revision: u64, before: bool, params: &CameraRaw) -> Option<[f32; 4]> {
        if !self.ready || !p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            return None;
        }
        let x = self.area.x0 + ((p[0] * self.area.width() as f32) as i32).min((self.area.width() - 1) as i32);
        let y = self.area.y0 + ((p[1] * self.area.height() as f32) as i32).min((self.area.height() - 1) as i32);
        if before || params.is_identity() {
            return Some(self.source.rgba(x, y));
        }
        let r = self.result.as_ref().filter(|r| r.revision == revision && r.crop.contains(x, y))?;
        let w = r.crop.width() as usize;
        r.px.get((y - r.crop.y0) as usize * w + (x - r.crop.x0) as usize).copied()
    }

    pub(crate) fn overlay(&mut self, ui: &egui::Ui, image: Rect, revision: u64, mode: crate::camera_raw_scope_ui::ClippingMode) -> bool {
        use crate::camera_raw_scope_ui::ClippingMode;
        if !self.ready {
            return false;
        }
        let Some((r, original, crop, _display_key)) = self.crop else { return false };
        let t = crate::theme::Tokens::get(ui.ctx());
        let key = (r, original, crop, mode, t.kind);
        if self.overlay_key != Some(key) {
            let pixels_of: Vec<[f32; 4]> = if original {
                let mut v = vec![[0.0; 4]; crop.width() as usize * crop.height() as usize];
                self.source.read_rgba_into(crop, &mut v);
                v
            } else if let Some(region) = self.result.as_ref().filter(|g| g.revision == revision && g.crop == crop) {
                region.px.clone()
            } else {
                return false;
            };
            let pixels: Vec<Color32> = pixels_of
                .iter()
                .map(|p| {
                    if p[3] <= 0.0 || !p.iter().all(|v| v.is_finite()) {
                        return Color32::TRANSPARENT;
                    }
                    let (lo, hi) = photocraft_algo::histogram::clipping(*p);
                    match mode {
                        ClippingMode::ShadowChannels => t.histogram_color(7 ^ lo),
                        ClippingMode::HighlightChannels => {
                            if hi == 0 {
                                t.histogram_background()
                            } else {
                                t.histogram_color(hi)
                            }
                        }
                        ClippingMode::Warnings(w) => {
                            if w & 2 != 0 && hi != 0 {
                                t.histogram_color(1)
                            } else if w & 1 != 0 && lo != 0 {
                                t.histogram_color(4)
                            } else {
                                Color32::TRANSPARENT
                            }
                        }
                        ClippingMode::None => Color32::TRANSPARENT,
                    }
                })
                .collect();
            let color = ColorImage::new([crop.width() as usize, crop.height() as usize], pixels);
            match &mut self.overlay_texture {
                Some(t) => t.set(color, TextureOptions::NEAREST),
                None => self.overlay_texture = Some(ui.ctx().load_texture("cr-detail-clipping", color, TextureOptions::NEAREST)),
            }
            self.overlay_key = Some(key);
        }
        if let Some(texture) = &self.overlay_texture {
            let scale = image.size() / vec2(self.area.width() as f32, self.area.height() as f32);
            let screen = Rect::from_min_size(
                image.min + vec2((crop.x0 - self.area.x0) as f32, (crop.y0 - self.area.y0) as f32) * scale,
                vec2(crop.width() as f32, crop.height() as f32) * scale,
            );
            ui.painter().image(texture.id(), screen, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        }
        true
    }
}

/// Everything one region develop reads, owned, so it runs off the UI thread.
struct RegionJob {
    source: Surface,
    area: PixelRect,
    coverage: Coverage,
    float: bool,
    params: CameraRaw,
    context: Arc<Measured>,
    crop: PixelRect,
    revision: u64,
    display: Arc<photocraft_engine::display_color::CanvasDisplay>,
    cache: Arc<Mutex<DevelopCache>>,
}

impl RegionJob {
    fn run(self) -> RegionResult {
        let (fw, fh) = (self.area.width() as usize, self.area.height() as usize);
        let mut p = self.params.clone();
        p.pixel_scale = 1.0;
        let m = photocraft_algo::camera_raw::region_margin(&p, fw, fh) as i32;
        let crop = self.crop.intersect(&self.area);
        let region = PixelRect::new(crop.x0 - m, crop.y0 - m, crop.x1 + m, crop.y1 + m).intersect(&self.area);
        if crop.is_empty() || region.width() as u64 * region.height() as u64 > MAX_REGION_PIXELS {
            return Err("the Camera Raw region is too large to develop".into());
        }
        let input = read_input(&self.source, self.area, region, &p)?;
        let (bw, bh) = (region.width() as usize, region.height() as usize);
        let frame = Frame { x0: i64::from(region.x0 - self.area.x0), y0: i64::from(region.y0 - self.area.y0), full_w: fw, full_h: fh };
        // The cache's light and presence stages hold for this region and these measurements.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (region.x0, region.y0, region.x1, region.y1, Arc::as_ptr(&self.context) as usize).hash(&mut h);
            h.finish()
        };
        let developed = {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            let inner = ((crop.x0 - region.x0) as usize, (crop.y0 - region.y0) as usize, crop.width() as usize, crop.height() as usize);
            photocraft_algo::camera_raw::develop_region(&input, bw, bh, frame, &p, self.float, &self.context, key, &mut cache, inner)
        };
        // Cut to the crop; alpha and what the selection or filter mask leaves stay the original's.
        let (cw, ch) = (crop.width() as usize, crop.height() as usize);
        let mut original = vec![[0.0f32; 4]; cw * ch];
        self.source.read_rgba_into(crop, &mut original);
        let coverage = coverage_of(&self.coverage, crop);
        let mut px = Vec::with_capacity(cw * ch);
        for y in 0..ch {
            let row = y * cw;
            for x in 0..cw {
                let o = original[y * cw + x];
                let n = developed.get(row + x).copied().unwrap_or(o);
                let k = coverage.as_ref().map_or(1.0, |c| c[y * cw + x]);
                px.push([o[0] + (n[0] - o[0]) * k, o[1] + (n[1] - o[1]) * k, o[2] + (n[2] - o[2]) * k, o[3]]);
            }
        }
        let image = display_image(crop, &self.display, |r, out| {
            let start = ((r.y0 - crop.y0).max(0) as usize).saturating_mul(cw);
            if let Some(src) = px.get(start..start.saturating_add(out.len())) {
                out.copy_from_slice(src);
            }
        });
        Ok(Region { revision: self.revision, crop, px, image: Some(image) })
    }
}

/// The source's pixels over `region` of the image `area`, moved by Manual Distortion when it is
/// set (read from the bounds of where the region's pixels come from).
fn read_input(source: &Surface, area: PixelRect, region: PixelRect, p: &CameraRaw) -> Result<Vec<[f32; 4]>, String> {
    let (fw, fh) = (area.width() as usize, area.height() as usize);
    let (bw, bh) = (region.width() as usize, region.height() as usize);
    let at = |x: i32, y: i32| photocraft_algo::camera_raw::distortion_source(p, fw, fh, (x - area.x0) as f32, (y - area.y0) as f32);
    if at(region.x0, region.y0).is_none() {
        let mut px = vec![[0.0f32; 4]; bw * bh];
        source.read_rgba_into(region, &mut px);
        return Ok(px);
    }
    // The radial model is monotone along rays, so the region's border bounds where it reads from
    // (its centre too, should the region hold the image's centre).
    let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let step = 8usize;
    let mut take = |x: i32, y: i32| {
        if let Some((sx, sy)) = at(x, y) {
            b = (b.0.min(sx), b.1.min(sy), b.2.max(sx), b.3.max(sy));
        }
    };
    for x in (region.x0..region.x1).step_by(step).chain([region.x1 - 1]) {
        take(x, region.y0);
        take(x, region.y1 - 1);
    }
    for y in (region.y0..region.y1).step_by(step).chain([region.y1 - 1]) {
        take(region.x0, y);
        take(region.x1 - 1, y);
    }
    take((region.x0 + region.x1) / 2, (region.y0 + region.y1) / 2);
    let from = PixelRect::new(area.x0 + b.0.floor() as i32 - 2, area.y0 + b.1.floor() as i32 - 2, area.x0 + b.2.ceil() as i32 + 3, area.y0 + b.3.ceil() as i32 + 3)
        .union(&region)
        .intersect(&area);
    if from.width() as u64 * from.height() as u64 > MAX_REGION_PIXELS {
        return Err("the Camera Raw region is too large to develop".into());
    }
    let (sw, sh) = (from.width() as usize, from.height() as usize);
    let mut src = vec![[0.0f32; 4]; sw * sh];
    source.read_rgba_into(from, &mut src);
    let (ox, oy) = ((from.x0 - area.x0) as f32, (from.y0 - area.y0) as f32);
    let moved = |i: usize| {
        let (x, y) = (region.x0 + (i % bw) as i32, region.y0 + (i / bw) as i32);
        let (sx, sy) = at(x, y).unwrap_or(((x - area.x0) as f32, (y - area.y0) as f32));
        photocraft_algo::camera_raw::sample_rgba(&src, sw, sh, sx - ox, sy - oy)
    };
    #[cfg(not(target_arch = "wasm32"))]
    let px = {
        use rayon::prelude::*;
        (0..bw * bh).into_par_iter().map(moved).collect()
    };
    #[cfg(target_arch = "wasm32")]
    let px = (0..bw * bh).map(moved).collect();
    Ok(px)
}

/// The selection's or filter mask's coverage over `crop` (row-major), or `None` for all of it.
fn coverage_of(coverage: &Coverage, crop: PixelRect) -> Option<Vec<f32>> {
    match coverage {
        Coverage::None => None,
        Coverage::Selection(mask) => {
            let v = mask.read_region(crop);
            let n = mask.channels().max(1);
            Some(v.chunks(n).map(|c| c.first().copied().unwrap_or(0.0).clamp(0.0, 1.0)).collect())
        }
        Coverage::Mask(mask) => Some((crop.y0..crop.y1).flat_map(|y| (crop.x0..crop.x1).map(move |x| (x, y))).map(|(x, y)| mask.value(x, y).clamp(0.0, 1.0)).collect()),
    }
}

fn visible_pixels(image: Rect, viewport: Rect, area: PixelRect, max_side: usize) -> Option<PixelRect> {
    let visible = image.intersect(viewport);
    if !image.is_finite() || image.width() <= 0.0 || image.height() <= 0.0 || !visible.is_positive() || area.is_empty() {
        return None;
    }
    let scale = vec2(area.width() as f32, area.height() as f32) / image.size();
    let lo = (visible.min - image.min) * scale;
    let hi = (visible.max - image.min) * scale;
    let crop = PixelRect::new(
        area.x0.saturating_add((lo.x.floor() as i32).saturating_sub(1)),
        area.y0.saturating_add((lo.y.floor() as i32).saturating_sub(1)),
        area.x0.saturating_add((hi.x.ceil() as i32).saturating_add(1)),
        area.y0.saturating_add((hi.y.ceil() as i32).saturating_add(1)),
    )
    .intersect(&area);
    // Proxy is sufficient below its native scale; a larger intermediate should never be
    // allocated if an unusually large viewport exceeds the device's texture limit.
    (crop.width() as usize <= max_side && crop.height() as usize <= max_side && crop.width() as u64 * crop.height() as u64 <= 16_777_216).then_some(crop)
}

/// Pixels per band [`display_image`] reads and colour-converts at once: at least `photocraft-cms`'s
/// parallel threshold (16K pixels), and small enough that a band's float scratch is 1 MiB.
const CROP_BAND_PIXELS: usize = 64 * 1024;

/// The visible pixels of `surface`, as the canvas shows them.
fn crop_image(surface: &Surface, area: PixelRect, display: &photocraft_engine::display_color::CanvasDisplay) -> ColorImage {
    display_image(area, display, |rect, out| surface.read_rgba_into(rect, out))
}

/// `area` as the canvas shows it, with `read` filling each band's float pixels. Converted in
/// bands of rows, in parallel off wasm, not one row or one whole copy at a time: a row is below
/// `photocraft-cms`'s parallel threshold, so a per-row conversion ran the display transform (any
/// document profile other than the monitor's, such as an Adobe RGB photo on sRGB) on one core of
/// the UI thread for every edit (#4682).
pub(crate) fn display_image(
    area: PixelRect,
    display: &photocraft_engine::display_color::CanvasDisplay,
    read: impl Fn(PixelRect, &mut [[f32; 4]]) + Sync,
) -> ColorImage {
    let (width, height) = (area.width() as usize, area.height() as usize);
    let mut pixels = vec![Color32::TRANSPARENT; width * height];
    if width == 0 {
        return ColorImage::new([width, height], pixels);
    }
    let band_rows = (CROP_BAND_PIXELS / width).max(1);
    let band = |index: usize, out: &mut [Color32]| {
        let y0 = area.y0.saturating_add((index * band_rows) as i32);
        let rect = PixelRect::new(area.x0, y0, area.x1, y0.saturating_add((out.len() / width) as i32));
        let mut buf = photocraft_compose::Buffer { rect, px: vec![[0.0; 4]; out.len()] };
        read(rect, &mut buf.px);
        let img = display.to_rgba8(&buf);
        for (o, &[r, g, b, a]) in out.iter_mut().zip(img.pixels.as_chunks::<4>().0) {
            *o = Color32::from_rgba_unmultiplied(r, g, b, a);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        pixels.par_chunks_mut(band_rows * width).enumerate().for_each(|(i, out)| band(i, out));
    }
    #[cfg(target_arch = "wasm32")]
    for (i, out) in pixels.chunks_mut(band_rows * width).enumerate() {
        band(i, out);
    }
    ColorImage::new([width, height], pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::PixelFormat;
    /// The measurements a develop of the whole of `area` takes (the dialog takes them on its proxy).
    fn measured(source: &Surface, area: PixelRect, params: &CameraRaw) -> Arc<Measured> {
        let (w, h) = (area.width() as usize, area.height() as usize);
        let mut px = vec![[0.0f32; 4]; w * h];
        source.read_rgba_into(area, &mut px);
        photocraft_algo::camera_raw::develop_cached(&px, w, h, params, false, 1, &mut DevelopCache::default(), false).context
    }

    fn display() -> Arc<photocraft_engine::display_color::CanvasDisplay> {
        let doc = photocraft_doc::Document::new("t", photocraft_geom::Size::new(1, 1), photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::U8);
        photocraft_engine::color_cmds::ColorState::default().canvas_display(&doc).unwrap()
    }

    fn job(source: &Surface, area: PixelRect, crop: PixelRect, params: &CameraRaw, coverage: Coverage) -> RegionJob {
        RegionJob {
            source: source.clone(),
            area,
            coverage,
            float: source.format().sample == photocraft_color::SampleType::F32,
            params: params.clone(),
            context: measured(source, area, params),
            crop,
            revision: 1,
            display: display(),
            cache: Default::default(),
        }
    }

    /// A region develops as the engine's whole-image Camera Raw does there, and keeps what the
    /// selection leaves out.
    #[test]
    fn a_region_develops_as_the_engine_does_and_respects_the_selection() {
        let area = PixelRect::new(10, -5, 74, 43);
        let mut source = Surface::new(PixelFormat::RGBA8);
        let (w, h) = (area.width() as usize, area.height() as usize);
        let mut values = vec![0.0; w * h * 4];
        for (i, p) in values.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            p.copy_from_slice(&[if i % 2 == 0 { 0.1 } else { 0.8 }, 0.2 + 0.5 * x as f32 / w as f32, 0.6 - 0.4 * y as f32 / h as f32, 1.0]);
        }
        source.write_region(area, &values);
        let params = CameraRaw {
            exposure: 0.5,
            texture: 15.0,
            clarity: 20.0,
            shadows: 40.0,
            whites: 10.0,
            dehaze: 15.0,
            grain_amount: 30.0,
            vignette_amount: -40.0,
            lens_distortion: 30.0,
            ..Default::default()
        };
        let expected = photocraft_engine::lens_cmds::camera_raw_surface(&source, area, &params);
        let crop = PixelRect::new(30, 5, 60, 30);
        let region = job(&source, area, crop, &params, Coverage::None).run().unwrap();
        assert_eq!(region.crop, crop);
        let mut worst = 0.0f32;
        for (i, got) in region.px.iter().enumerate() {
            let (x, y) = (crop.x0 + (i % crop.width() as usize) as i32, crop.y0 + (i / crop.width() as usize) as i32);
            let want = expected.rgba(x, y);
            worst = (0..4).map(|c| (got[c] - want[c]).abs()).fold(worst, f32::max);
        }
        // 8-bit storage of the engine's result rounds by half a level.
        assert!(worst <= 1.0 / 255.0 + 1e-4, "region vs engine: {worst}");
        let none = Surface::with_default(PixelFormat::GRAY8, &[0.0]);
        let kept = job(&source, area, crop, &params, Coverage::Selection(none)).run().unwrap();
        assert!(kept.px.iter().enumerate().all(|(i, p)| {
            let (x, y) = (crop.x0 + (i % crop.width() as usize) as i32, crop.y0 + (i / crop.width() as usize) as i32);
            *p == source.rgba(x, y)
        }));
    }

    /// The latest region stays on screen while a newer one develops; a failed region isn't asked
    /// for again, and a worker that stopped is an error.
    #[test]
    fn regions_replace_each_other_and_failures_are_kept() {
        let source = Surface::with_default(PixelFormat::RGBA8, &[0.2, 0.4, 0.6, 1.0]);
        let mut preview = DetailPreview::new(source.clone(), PixelRect::new(0, 0, 16, 16), Coverage::None);
        let crop = PixelRect::new(0, 0, 8, 8);
        let region = |revision| Region { revision, crop, px: vec![[0.5; 4]; 64], image: None };
        let (tx, rx) = std::sync::mpsc::channel();
        preview.pending = Some((1, crop, rx));
        tx.send(Ok(region(1))).unwrap();
        preview.poll();
        assert_eq!(preview.result.as_ref().map(|r| r.revision), Some(1));
        let (tx, rx) = std::sync::mpsc::channel();
        preview.pending = Some((2, crop, rx));
        tx.send(Err("failed".into())).unwrap();
        preview.poll();
        assert_eq!(preview.failed, Some((2, crop)));
        assert_eq!(preview.result.as_ref().map(|r| r.revision), Some(1), "the older region stays");
        preview.request(&Context::default(), 2, crop, &CameraRaw { exposure: 1.0, ..Default::default() }, &Arc::default(), &display());
        assert!(!preview.pending(), "not asked for again");
        let (tx, rx) = std::sync::mpsc::channel::<RegionResult>();
        preview.pending = Some((3, crop, rx));
        drop(tx);
        preview.poll();
        assert!(preview.error.is_some() && !preview.pending());
    }

    #[test]
    fn full_resolution_sampling_and_regions_keep_fine_detail_at_every_supported_depth() {
        use photocraft_color::{ColorMode, PixelFormat, SampleType};
        for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
            for mode in [ColorMode::Rgb, ColorMode::Grayscale] {
                let format = PixelFormat { mode, sample: depth, alpha: true };
                let area = PixelRect::new(-12, 8, 28, 38);
                let mut source = Surface::new(format);
                let channels = format.channels();
                let mut pixels = vec![0.0; 40 * 30 * channels];
                for (i, pixel) in pixels.chunks_exact_mut(channels).enumerate() {
                    pixel.fill(if i % 2 == 0 { 0.1 } else { 0.9 });
                    pixel[channels - 1] = 1.0;
                }
                source.write_region(area, &pixels);
                let params = CameraRaw { exposure: 0.5, sharpen_amount: 20.0, grain_amount: 30.0, ..Default::default() };
                let region = job(&source, area, area, &params, Coverage::None).run().unwrap();
                let mut preview = DetailPreview::new(source, area, Coverage::None);
                preview.result = Some(region);
                preview.ready = true;
                let dark = preview.sample([0.0, 0.0], 1, false, &params).unwrap();
                let light = preview.sample([1.0 / 40.0, 0.0], 1, false, &params).unwrap();
                assert!(light[0] - dark[0] > 0.5, "native alternating pixels survive: {mode:?}/{depth:?}");
                assert!(preview.sample([f32::NAN, 0.0], 1, false, &params).is_none());
                assert!(preview.sample([0.5, 0.5], 2, false, &params).is_none());
            }
        }
    }

    /// A huge, sparse document develops only what is in view.
    #[test]
    fn a_huge_image_develops_only_the_region_in_view() {
        let mut source = Surface::new(PixelFormat::RGBA8);
        source.write_region(PixelRect::new(4000, 4000, 4010, 4010), &[0.5; 400]);
        let area = PixelRect::new(0, 0, 8001, 8000);
        let params = CameraRaw { exposure: 1.0, ..Default::default() };
        let mut j = job(&source, PixelRect::new(3990, 3990, 4020, 4020), PixelRect::new(3995, 3995, 4015, 4015), &params, Coverage::None);
        j.area = area;
        let r = j.run().unwrap();
        assert_eq!(r.px.len(), 400);
        let whole = RegionJob { crop: area, ..job(&source, PixelRect::new(0, 0, 8, 8), area, &params, Coverage::None) };
        assert!(RegionJob { area, ..whole }.run().is_err(), "more than a region's worth of pixels is refused");
    }

    #[test]
    fn refined_crop_uses_linear_document_display_encoding() {
        let area = PixelRect::new(0, 0, 1, 1);
        let mut source =
            Surface::new(photocraft_color::PixelFormat { mode: photocraft_color::ColorMode::Rgb, sample: photocraft_color::SampleType::F32, alpha: true });
        source.write_region(area, &[0.25, 0.25, 0.25, 1.0]);
        let mut doc = photocraft_doc::Document::new("test", photocraft_geom::Size::new(1, 1), photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::F32);
        doc.icc_profile = Some(photocraft_cms::Builtin::LinearSrgb.profile().to_bytes());
        let mut color = photocraft_engine::color_cmds::ColorState::default();
        color.settings.monitor_profile = "srgb".into();
        let display = color.canvas_display(&doc).unwrap();
        let image = crop_image(&source, area, &display);
        assert!(image.pixels[0].r() > 130, "linear 0.25 should be shown as encoded sRGB");
    }

    #[test]
    fn zoom_uploads_only_visible_pixels_even_on_a_huge_offset_image() {
        let area = PixelRect::new(-200, 10, 15800, 10010);
        let image = Rect::from_min_size(pos2(-7800.0, -4800.0), vec2(16000.0, 10000.0));
        let visible = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let crop = visible_pixels(image, visible, area, 4096).unwrap();
        assert_eq!((crop.width(), crop.height()), (802, 602));
        assert!(visible_pixels(image, Rect::from_min_size(Pos2::ZERO, vec2(8000.0, 6000.0)), area, 4096).is_none());
        assert!(visible_pixels(Rect::NOTHING, visible, area, 4096).is_none());
    }

    #[test]
    fn banded_crop_matches_one_conversion_of_the_whole_area() {
        // Several bands, a ragged last one and an area off the image origin, through a real
        // transform (an Adobe RGB document on an sRGB monitor) and without one.
        let area = PixelRect::new(-3, 5, 1021, 5 + 2 * (CROP_BAND_PIXELS / 1024) as i32 + 7);
        let mut source = Surface::new(PixelFormat { mode: photocraft_color::ColorMode::Rgb, sample: photocraft_color::SampleType::U8, alpha: true });
        let n = (area.width() * area.height()) as usize;
        let px: Vec<f32> = (0..n).flat_map(|i| [(i % 251) as f32 / 255.0, (i % 241) as f32 / 255.0, (i % 239) as f32 / 255.0, (i % 7) as f32 / 6.0]).collect();
        source.write_region(area, &px);
        for profile in [photocraft_cms::Builtin::AdobeRgbCompat, photocraft_cms::Builtin::Srgb] {
            let mut doc =
                photocraft_doc::Document::new("test", photocraft_geom::Size::new(1024, 1024), photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::U8);
            doc.icc_profile = Some(profile.profile().to_bytes());
            let mut color = photocraft_engine::color_cmds::ColorState::default();
            color.settings.monitor_profile = "srgb".into();
            let display = color.canvas_display(&doc).unwrap();
            assert_eq!(display.transform.is_some(), profile == photocraft_cms::Builtin::AdobeRgbCompat);
            let mut whole = photocraft_compose::Buffer { rect: area, px: vec![[0.0; 4]; n] };
            source.read_rgba_into(area, &mut whole.px);
            let expected = display.to_rgba8(&whole);
            let image = crop_image(&source, area, &display);
            assert_eq!(image.size, [area.width() as usize, area.height() as usize]);
            let expected: Vec<Color32> = expected.pixels.as_chunks::<4>().0.iter().map(|&[r, g, b, a]| Color32::from_rgba_unmultiplied(r, g, b, a)).collect();
            assert_eq!(image.pixels, expected, "{profile:?}");
        }
        assert_eq!(
            crop_image(
                &source,
                PixelRect::new(0, 0, 0, 4),
                &photocraft_engine::color_cmds::ColorState::default()
                    .canvas_display(&photocraft_doc::Document::new(
                        "t",
                        photocraft_geom::Size::new(1, 1),
                        photocraft_doc::ColorMode::Rgb,
                        photocraft_doc::SampleType::U8
                    ))
                    .unwrap()
            )
            .size,
            [0, 4]
        );
    }
}
