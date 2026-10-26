//! The Camera Raw dialog's preview on the GPU, as Photoshop's Camera Raw draws it: the proxy stays
//! on the graphics processor, each change develops it there ([`photocraft_gpu::camera_raw`]) and
//! writes the preview straight into the texture the dialog shows, and the histogram is counted
//! there too. The CPU only takes the image-wide measurements the settings need (Dehaze, Shadows,
//! Highlights, Contrast, Whites, Blacks), on a copy at most [`MEASURE_SIDE`] across while a
//! control moves and on the proxy itself once it is let go, and reads the developed pixels back
//! for the scopes and samplers once a change settles.
//!
//! The pipelines are built once per device on a thread of their own; until they are ready (and
//! wherever the GPU can't: no wgpu canvas, a device that refuses, a proxy too large for its
//! buffers, wasm) the preview develops on the CPU as before. A GPU error drops the GPU preview for
//! the dialog and the frame is developed on the CPU instead. `PHOTOCRAFT_CAMERA_RAW_CPU=1` keeps
//! it on the CPU.

use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui_wgpu::{self, RenderState};
use eframe::wgpu;
use photocraft_algo::camera_raw::{CameraRaw, Context, DevelopCache, Frame, gpu as program};
use photocraft_algo::histogram::RgbHistogram;
use photocraft_engine::display_color::CanvasDisplay;
use photocraft_gpu::camera_raw::{CameraRawGpu, Display};

/// While a control moves, the measurements are taken on a copy of the proxy at most this many
/// pixels across (the Shadows and Highlights filter works at about this size on the proxy too).
pub(crate) const MEASURE_SIDE: usize = 1024;
/// Side of the monitor transform's table.
const LUT_SIZE: u32 = 33;

/// The pipelines of one device, built once and shared by every dialog on it.
struct Engine {
    device: wgpu::Device,
    gpu: Mutex<Option<Result<CameraRawGpu, String>>>,
}

fn engine(rs: &RenderState) -> Arc<Engine> {
    static ENGINE: Mutex<Option<Arc<Engine>>> = Mutex::new(None);
    let mut slot = ENGINE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(e) = slot.as_ref().filter(|e| e.device == rs.device) {
        return e.clone();
    }
    let e = Arc::new(Engine { device: rs.device.clone(), gpu: Mutex::new(None) });
    let (built, device, queue) = (e.clone(), rs.device.clone(), rs.queue.clone());
    let build = move || {
        let gpu = CameraRawGpu::new(&device, &queue);
        if let Err(err) = &gpu {
            log::warn!("Camera Raw stays on the CPU: {err}");
        }
        *built.gpu.lock().unwrap_or_else(PoisonError::into_inner) = Some(gpu);
    };
    if std::thread::Builder::new().name("camera-raw-gpu".into()).spawn(build).is_err() {
        log::warn!("Camera Raw stays on the CPU: no thread to build its GPU pipelines on");
    }
    *slot = Some(e.clone());
    e
}

/// What the worker reaches through the dialog's [`GpuPreview`].
pub(crate) struct Shared {
    engine: Arc<Engine>,
    texture: wgpu::Texture,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The proxy's copy for measurements while a control moves: its source, pixels and size.
    measure: Option<(u64, Vec<[f32; 4]>, usize, usize)>,
    measure_cache: DevelopCache,
    /// The settings (measured part) and measurements last taken, by whether on the copy.
    last: Option<(CameraRaw, bool, Arc<Context>)>,
    /// The monitor transform's table, by the display's key.
    lut: Option<(u64, Vec<f32>)>,
}

/// The dialog's GPU preview: the texture it shows (registered with egui) and what develops into it.
pub(crate) struct GpuPreview {
    shared: Arc<Shared>,
    pub(crate) texture_id: egui::TextureId,
    renderer: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
}

impl Drop for GpuPreview {
    fn drop(&mut self) {
        self.renderer.write().free_texture(&self.texture_id);
    }
}

impl GpuPreview {
    /// A GPU preview for a `pw × ph` proxy on `rs`'s device, or `None` where there can't be one.
    pub(crate) fn new(rs: &RenderState, pw: usize, ph: usize) -> Option<Self> {
        if cfg!(target_arch = "wasm32") || std::env::var("PHOTOCRAFT_CAMERA_RAW_CPU").is_ok_and(|v| v.trim() == "1") {
            return None;
        }
        let (w, h) = (u32::try_from(pw).ok()?, u32::try_from(ph).ok()?);
        let max = rs.device.limits().max_texture_dimension_2d;
        if w == 0 || h == 0 || w > max || h > max {
            return None;
        }
        let texture = rs.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("camera-raw-gpu-preview"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let texture_id = rs.renderer.write().register_native_texture(&rs.device, &view, wgpu::FilterMode::Linear);
        Some(GpuPreview { shared: Arc::new(Shared { engine: engine(rs), texture, state: Mutex::new(State::default()) }), texture_id, renderer: rs.renderer.clone() })
    }

    pub(crate) fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }
}

/// One develop on the GPU.
pub(crate) struct GpuFrame {
    pub(crate) histogram: RgbHistogram,
    pub(crate) context: Arc<Context>,
    /// The developed pixels, when asked for.
    pub(crate) px: Option<Vec<[f32; 4]>>,
}

/// What one develop reads.
pub(crate) struct Job<'a> {
    pub(crate) input: &'a [[f32; 4]],
    pub(crate) source: u64,
    pub(crate) size: (usize, usize),
    /// The settings, with the proxy's pixel scale.
    pub(crate) params: &'a CameraRaw,
    pub(crate) float: bool,
    pub(crate) selection: Option<&'a [f32]>,
    pub(crate) display: &'a CanvasDisplay,
    /// The proxy's develop cache (the measurements once a change settles go through it).
    pub(crate) cache: &'a Mutex<DevelopCache>,
    /// A control is moving: measure on the small copy, don't read the pixels back.
    pub(crate) draft: bool,
}

impl Shared {
    /// Develops `job` into the preview texture. `Ok(None)` while the pipelines are being built
    /// (or when the device refused them, or the proxy is too large for it): the caller develops
    /// on the CPU. `Err` when the GPU failed.
    pub(crate) fn develop(&self, job: &Job) -> Result<Option<GpuFrame>, String> {
        let mut slot = self.engine.gpu.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(Ok(gpu)) = slot.as_mut() else { return Ok(None) };
        let (pw, ph) = job.size;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let p = job.params;
        let context = self.measure(&mut state, job);
        let prog = program::program(p, pw, ph, Frame::whole(pw, ph), &context, job.float, true)?;
        if !gpu.fits(prog.width, prog.height, prog.plane_floats) {
            return Ok(None);
        }
        gpu.set_source(job.source, job.input, prog.width, prog.height, job.selection)?;
        gpu.develop(&prog)?;
        let encode = job.display.encode_srgb.then(photocraft_engine::display_color::srgb_encode_table);
        let lut = match &job.display.transform {
            Some(t) => {
                if state.lut.as_ref().is_none_or(|(key, _)| *key != job.display.key) {
                    state.lut = Some((job.display.key, lut(t)));
                }
                state.lut.as_ref().map(|(_, v)| (LUT_SIZE, v.as_slice()))
            }
            None => None,
        };
        gpu.present(&self.texture, Display { encode, lut })?;
        let histogram = gpu.histogram(true).ok_or("Camera Raw GPU: the histogram wasn't counted")?;
        let px = if job.draft {
            None
        } else {
            let mut px = gpu.read_back()?;
            // As the CPU preview keeps them: the selection blended in (the texture has it too).
            if let Some(mask) = job.selection {
                for ((out, original), m) in px.iter_mut().zip(job.input).zip(mask) {
                    for c in 0..4 {
                        out[c] = original[c] + (out[c] - original[c]) * m;
                    }
                }
            }
            Some(px)
        };
        Ok(Some(GpuFrame { histogram, context, px }))
    }

    /// The measurements `job`'s settings need: none, the small copy's while a control moves, the
    /// proxy's once it settles; the last ones again when the settings they depend on are the same.
    fn measure(&self, state: &mut State, job: &Job) -> Arc<Context> {
        let p = job.params;
        if !program::measures(p) {
            return Arc::default();
        }
        let (pw, ph) = job.size;
        let small = job.draft && pw.max(ph) > MEASURE_SIDE;
        let key = measured_key(p);
        if let Some((k, s, c)) = &state.last
            && *k == key
            && *s == small
        {
            return c.clone();
        }
        let context = if small {
            if state.measure.as_ref().is_none_or(|m| m.0 != job.source) {
                let (px, mw, mh) = shrink(job.input, pw, ph, pw.max(ph).div_ceil(MEASURE_SIDE));
                state.measure = Some((job.source, px, mw, mh));
            }
            match &state.measure {
                Some((source, px, mw, mh)) => {
                    let q = CameraRaw { pixel_scale: p.pixel_scale * *mw as f32 / pw.max(1) as f32, ..p.clone() };
                    program::measure(px, *mw, *mh, &q, *source, &mut state.measure_cache, true)
                }
                None => Arc::default(),
            }
        } else {
            let mut cache = job.cache.lock().unwrap_or_else(PoisonError::into_inner);
            program::measure(job.input, pw, ph, p, job.source, &mut cache, false)
        };
        state.last = Some((key, small, context.clone()));
        context
    }
}

/// The settings the measurements depend on (everything up to Whites and Blacks).
fn measured_key(p: &CameraRaw) -> CameraRaw {
    CameraRaw {
        curve_highlights: 0.0,
        curve_lights: 0.0,
        curve_darks: 0.0,
        curve_shadows: 0.0,
        point_curve: Vec::new(),
        point_curve_red: Vec::new(),
        point_curve_green: Vec::new(),
        point_curve_blue: Vec::new(),
        vibrance: 0.0,
        saturation: 0.0,
        hsl_hue: [0.0; 8],
        hsl_sat: [0.0; 8],
        hsl_lum: [0.0; 8],
        monochrome: false,
        gray_mix: [0.0; 8],
        grade_shadows: Default::default(),
        grade_midtones: Default::default(),
        grade_highlights: Default::default(),
        grade_global: Default::default(),
        sharpen_amount: 0.0,
        noise_luminance: 0.0,
        noise_color: 0.0,
        grain_amount: 0.0,
        vignette_amount: 0.0,
        vignette_style: String::new(),
        ..p.clone()
    }
}

/// `px` (`w × h`) averaged over `f × f` blocks.
fn shrink(px: &[[f32; 4]], w: usize, h: usize, f: usize) -> (Vec<[f32; 4]>, usize, usize) {
    let f = f.max(1);
    let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
    let mut out = vec![[0.0f32; 4]; sw * sh];
    for (i, o) in out.iter_mut().enumerate() {
        let (bx, by) = (i % sw, i / sw);
        let mut sum = [0.0f32; 4];
        let mut n = 0.0f32;
        for y in by * f..((by + 1) * f).min(h) {
            for q in px.get(y * w + bx * f..y * w + ((bx + 1) * f).min(w)).unwrap_or(&[]) {
                for c in 0..4 {
                    sum[c] += q[c];
                }
                n += 1.0;
            }
        }
        *o = sum.map(|v| v / n.max(1.0));
    }
    (out, sw, sh)
}

/// The monitor transform as a [`LUT_SIZE`]³ table over the screen bytes (red fastest).
fn lut(t: &photocraft_cms::Transform) -> Vec<f32> {
    let n = LUT_SIZE as usize;
    let mut out = Vec::with_capacity(n * n * n * 3);
    let mut rgb = [0.0f32; 3];
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let input = [r, g, b].map(|v| v as f32 / (n - 1) as f32);
                t.eval(&input, &mut rgb);
                out.extend(rgb.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }));
            }
        }
    }
    out
}
