//! Camera Raw's develop on the GPU, as Photoshop's Camera Raw draws its preview: runs a
//! [`GpuProgram`] (built by [`photocraft_algo::camera_raw::gpu::program`] from the settings and
//! the image-wide measurements) over pixels kept on the GPU, then writes the preview's bytes into
//! a texture the UI shows and counts its histogram, with nothing read back per step.
//!
//! The pixels (straight RGBA floats) are uploaded once per source; a develop runs its passes
//! (`camera_raw.wgsl`) on storage buffers, and [`CameraRawGpu::present`] blends the selection in,
//! counts the histogram and writes the screen's bytes (`camera_raw_display.wgsl`) as the CPU
//! preview does (`CanvasDisplay::rgba8_into` and egui's premultiplying), so the two show the same.
//! Every call returns `Err` rather than panicking (a validation error, a device lost, a buffer the
//! device can't hold), and the caller falls back to the CPU develop.

use photocraft_algo::camera_raw::gpu::{self as program, GpuPass, GpuProgram};
use photocraft_algo::histogram::RgbHistogram;

const SHADER: &str = include_str!("camera_raw.wgsl");
const DISPLAY_SHADER: &str = include_str!("camera_raw_display.wgsl");

/// Bytes of one pass's uniforms (`Pass` in the shader).
const PASS_BYTES: usize = 80;
/// Words of the histogram (`H_WORDS` in the display shader).
const HIST_WORDS: usize = 783;

/// The develop shader's source with the op and plane codes declared ahead of it.
pub fn shader_source() -> String {
    let mut s = String::new();
    for (name, v) in program::op::ALL {
        s.push_str(&format!("const OP_{name}: u32 = {v}u;\n"));
    }
    for (name, v) in program::plane::ALL {
        s.push_str(&format!("const PL_{name}: u32 = {v}u;\n"));
    }
    s.push_str(SHADER);
    s
}

/// The display shader's source.
pub fn display_shader_source() -> &'static str {
    DISPLAY_SHADER
}

/// How [`CameraRawGpu::present`] turns the developed pixels into the screen's bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Display<'a> {
    /// Linear values → sRGB-encoded 8-bit codes, indexed by the value in 1/65535 steps
    /// (`CanvasDisplay::encode_srgb`); `None`: the values are already the screen's.
    pub encode: Option<&'a [u8]>,
    /// The monitor transform as a table over the bytes: `(n, values)`, `n³` RGB triples in 0…1
    /// (red fastest); `None` when it is the identity.
    pub lut: Option<(u32, &'a [f32])>,
}

fn storage(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn uniform(binding: u32, size: u64, dynamic: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: dynamic, min_binding_size: std::num::NonZeroU64::new(size) },
        count: None,
    }
}

/// A storage buffer that grows to what it is asked to hold.
struct Grow {
    label: &'static str,
    usage: wgpu::BufferUsages,
    buf: Option<wgpu::Buffer>,
}

impl Grow {
    fn new(label: &'static str, usage: wgpu::BufferUsages) -> Self {
        Grow { label, usage, buf: None }
    }
    /// The buffer, at least `bytes` long (and never empty: bindings can't be).
    fn at_least(&mut self, device: &wgpu::Device, bytes: u64) -> &wgpu::Buffer {
        let bytes = bytes.max(16).next_multiple_of(16);
        if self.buf.as_ref().is_none_or(|b| b.size() < bytes) {
            if let Some(old) = self.buf.take() {
                old.destroy();
            }
            self.buf = Some(device.create_buffer(&wgpu::BufferDescriptor { label: Some(self.label), size: bytes, usage: self.usage, mapped_at_creation: false }));
        }
        // Just made when missing.
        #[allow(clippy::expect_used)]
        self.buf.as_ref().expect("the buffer was just made")
    }
}

/// The source pixels on the GPU.
struct Source {
    key: u64,
    w: u32,
    h: u32,
    buf: wgpu::Buffer,
}

/// Camera Raw's GPU develop and preview (see the module docs).
pub struct CameraRawGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pixel: wgpu::ComputePipeline,
    plane: wgpu::ComputePipeline,
    distort: wgpu::ComputePipeline,
    display_layout: wgpu::BindGroupLayout,
    display: wgpu::ComputePipeline,
    source: Option<Source>,
    /// The selection's coverage per source pixel, uploaded with it.
    mask: Option<wgpu::Buffer>,
    img: Grow,
    prev: Grow,
    planes: Grow,
    ops: Grow,
    table: Grow,
    maps: Grow,
    /// The measurements whose maps are in `maps`.
    maps_of: Option<std::sync::Weak<photocraft_algo::camera_raw::Context>>,
    uniforms: Grow,
    codes: Grow,
    lut: Grow,
    /// The display's code table and LUT last uploaded (pointer and length: a new table is new).
    display_keys: (usize, usize),
    hist: wgpu::Buffer,
    hist_read: wgpu::Buffer,
    /// A histogram read is under way (mapped when `true` is stored).
    hist_pending: Option<std::sync::Arc<std::sync::atomic::AtomicU8>>,
    /// Whether the last develop's result is in `img` (not just the source).
    developed: bool,
}

/// Runs `f` inside wgpu's error scopes: an error it raises is an `Err`, not a panic.
fn scoped<T>(device: &wgpu::Device, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let (result, error) = crate::first_error(device, f);
    match error {
        Some(e) => Err(format!("Camera Raw on the GPU failed: {e}")),
        None => result,
    }
}

impl CameraRawGpu {
    /// Builds the pipelines on `device`. `Err` when the device can't (a shader its compiler
    /// refuses), so the caller develops on the CPU.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, String> {
        scoped(device, || {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("pc_camera_raw"), source: wgpu::ShaderSource::Wgsl(shader_source().into()) });
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("pc_camera_raw"),
                entries: &[uniform(0, PASS_BYTES as u64, true), storage(1, true), storage(2, true), storage(3, false), storage(4, false), storage(5, true), storage(6, true)],
            });
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("pc_camera_raw"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
            let pipeline = |entry: &str| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&pl),
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    cache: None,
                })
            };
            let (pixel, plane, distort) = (pipeline("pixel_main"), pipeline("plane_main"), pipeline("distort_main"));
            let dmodule = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("pc_camera_raw_display"), source: wgpu::ShaderSource::Wgsl(DISPLAY_SHADER.into()) });
            let display_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("pc_camera_raw_display"),
                entries: &[
                    uniform(0, 16, false),
                    storage(1, true),
                    storage(2, true),
                    storage(3, true),
                    storage(4, true),
                    storage(5, true),
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                    storage(7, false),
                ],
            });
            let dpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("pc_camera_raw_display"), bind_group_layouts: &[Some(&display_layout)], immediate_size: 0 });
            let display = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("display_main"),
                layout: Some(&dpl),
                module: &dmodule,
                entry_point: Some("display_main"),
                compilation_options: Default::default(),
                cache: None,
            });
            let storage_usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
            let hist_bytes = (HIST_WORDS * 4) as u64;
            let hist = device.create_buffer(&wgpu::BufferDescriptor { label: Some("pc_camera_raw_hist"), size: hist_bytes, usage: storage_usage, mapped_at_creation: false });
            let hist_read = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pc_camera_raw_hist_read"),
                size: hist_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            Ok(CameraRawGpu {
                device: device.clone(),
                queue: queue.clone(),
                layout,
                pixel,
                plane,
                distort,
                display_layout,
                display,
                source: None,
                mask: None,
                img: Grow::new("pc_camera_raw_img", storage_usage),
                prev: Grow::new("pc_camera_raw_prev", storage_usage),
                planes: Grow::new("pc_camera_raw_planes", storage_usage),
                ops: Grow::new("pc_camera_raw_ops", storage_usage),
                table: Grow::new("pc_camera_raw_table", storage_usage),
                maps: Grow::new("pc_camera_raw_maps", storage_usage),
                maps_of: None,
                uniforms: Grow::new("pc_camera_raw_uniforms", wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST),
                codes: Grow::new("pc_camera_raw_codes", storage_usage),
                lut: Grow::new("pc_camera_raw_lut", storage_usage),
                display_keys: (0, 0),
                hist,
                hist_read,
                hist_pending: None,
                developed: false,
            })
        })
    }

    /// The largest storage buffer the device binds, in bytes.
    fn max_binding(&self) -> u64 {
        self.device.limits().max_storage_buffer_binding_size
    }

    /// Whether `w × h` pixels with `plane_floats` floats of planes fit the device's buffers.
    pub fn fits(&self, w: u32, h: u32, plane_floats: usize) -> bool {
        let px = u64::from(w) * u64::from(h);
        let max = self.max_binding();
        px * 16 <= max && (plane_floats as u64) * 4 <= max && w <= self.device.limits().max_texture_dimension_2d && h <= self.device.limits().max_texture_dimension_2d
    }

    /// Uploads the source pixels (`w × h` straight RGBA) and the selection's coverage (one float
    /// per pixel, or `None`), unless `key` names the ones already there.
    pub fn set_source(&mut self, key: u64, px: &[[f32; 4]], w: u32, h: u32, mask: Option<&[f32]>) -> Result<(), String> {
        if self.source.as_ref().is_some_and(|s| s.key == key && (s.w, s.h) == (w, h)) {
            return Ok(());
        }
        let n = (w as usize).checked_mul(h as usize).ok_or("image too large")?;
        if px.len() != n || n == 0 || mask.is_some_and(|m| m.len() != n) {
            return Err("Camera Raw GPU source: size mismatch".into());
        }
        if !self.fits(w, h, 0) {
            return Err("Camera Raw GPU source: too large for the device".into());
        }
        let device = self.device.clone();
        scoped(&device, || {
            let bytes = (n * 16) as u64;
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pc_camera_raw_src"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buf, 0, &as_bytes(px));
            if let Some(old) = self.source.replace(Source { key, w, h, buf }) {
                old.buf.destroy();
            }
            if let Some(old) = self.mask.take() {
                old.destroy();
            }
            if let Some(m) = mask {
                let mb = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("pc_camera_raw_mask"),
                    size: (n * 4) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.queue.write_buffer(&mb, 0, &as_bytes(m));
                self.mask = Some(mb);
            }
            self.developed = false;
            Ok(())
        })
    }

    /// Runs `prog` on the source: afterwards the develop is on the GPU, for [`Self::present`] and
    /// [`Self::read_back`]. An empty program (an identity) leaves the source as it is.
    pub fn develop(&mut self, prog: &GpuProgram) -> Result<(), String> {
        let Some(src) = &self.source else { return Err("Camera Raw GPU: no source".into()) };
        let (w, h) = (src.w, src.h);
        if (prog.width, prog.height) != (w, h) {
            return Err("Camera Raw GPU: the program is for another size".into());
        }
        if !self.fits(w, h, prog.plane_floats) {
            return Err("Camera Raw GPU: too large for the device".into());
        }
        if prog.is_empty() {
            // An identity: the preview shows the source.
            self.developed = false;
            return Ok(());
        }
        let device = self.device.clone();
        let queue = self.queue.clone();
        scoped(&device, || {
            let n = u64::from(w) * u64::from(h);
            let img_bytes = n * 16;
            let src_buf = self.source.as_ref().map(|s| s.buf.clone()).ok_or("no source")?;
            let img = self.img.at_least(&device, img_bytes).clone();
            let prev = self.prev.at_least(&device, if prog.passes.iter().any(|p| matches!(p, GpuPass::Distort { .. })) { img_bytes } else { 16 }).clone();
            let planes = self.planes.at_least(&device, (prog.plane_floats as u64) * 4).clone();
            let ops = self.ops.at_least(&device, (prog.ops.len() as u64) * 4).clone();
            let table = self.table.at_least(&device, (prog.table.len() as u64) * 4).clone();
            let align = (PASS_BYTES as u64).next_multiple_of(u64::from(device.limits().min_uniform_buffer_offset_alignment).max(1));
            let uniforms = self.uniforms.at_least(&device, align * prog.passes.len().max(1) as u64).clone();
            let map_data = match &prog.maps {
                Some(c) if !self.maps_of.as_ref().is_some_and(|w| std::ptr::eq(w.as_ptr(), std::sync::Arc::as_ptr(c))) => Some(program::map_data(c)),
                _ => None,
            };
            let maps = self.maps.at_least(&device, map_data.as_ref().map_or(16, |m| (m.len() as u64) * 4)).clone();
            if let (Some(data), Some(c)) = (&map_data, &prog.maps) {
                if !data.is_empty() {
                    queue.write_buffer(&maps, 0, &as_bytes(data));
                }
                self.maps_of = Some(std::sync::Arc::downgrade(c));
            }
            queue.write_buffer(&ops, 0, &as_bytes(&prog.ops));
            if !prog.table.is_empty() {
                queue.write_buffer(&table, 0, &as_bytes(&prog.table));
            }
            let mut words = vec![0u8; (align as usize) * prog.passes.len().max(1)];
            for (k, pass) in prog.passes.iter().enumerate() {
                let u = pass_uniform(prog, pass);
                let at = k * align as usize;
                words[at..at + PASS_BYTES].copy_from_slice(&as_bytes(&u));
            }
            queue.write_buffer(&uniforms, 0, &words);
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pc_camera_raw"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &uniforms, offset: 0, size: std::num::NonZeroU64::new(PASS_BYTES as u64) }),
                    },
                    wgpu::BindGroupEntry { binding: 1, resource: ops.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: table.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: img.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: planes.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 5, resource: prev.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: maps.as_entire_binding() },
                ],
            });
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_camera_raw") });
            enc.copy_buffer_to_buffer(&src_buf, 0, &img, 0, img_bytes);
            for (k, pass) in prog.passes.iter().enumerate() {
                let offset = (k as u64 * align) as u32;
                let (pipeline, gw, gh) = match pass {
                    GpuPass::Pixel { .. } => (&self.pixel, w, h),
                    GpuPass::Distort { .. } => {
                        enc.copy_buffer_to_buffer(&img, 0, &prev, 0, img_bytes);
                        (&self.distort, w, h)
                    }
                    GpuPass::Plane { kind, w: pw, h: ph, n, .. } => {
                        let (ow, oh) = plane_out(*kind, *pw, *ph, *n);
                        (&self.plane, ow, oh)
                    }
                };
                let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("pc_camera_raw"), timestamp_writes: None });
                cp.set_pipeline(pipeline);
                cp.set_bind_group(0, &bind, &[offset]);
                cp.dispatch_workgroups(gw.div_ceil(16), gh.div_ceil(16), 1);
            }
            queue.submit([enc.finish()]);
            Ok(())
        })?;
        self.developed = true;
        Ok(())
    }

    /// Writes the develop (or the source, before one) into `target` (an `Rgba8Unorm` texture of
    /// the source's size with storage use) as the screen's bytes, with the selection blended in,
    /// and starts counting its histogram (read with [`Self::histogram`]).
    pub fn present(&mut self, target: &wgpu::Texture, display: Display) -> Result<(), String> {
        let Some(src) = &self.source else { return Err("Camera Raw GPU: no source".into()) };
        let (w, h) = (src.w, src.h);
        if (target.width(), target.height()) != (w, h) || target.format() != wgpu::TextureFormat::Rgba8Unorm {
            return Err("Camera Raw GPU: the preview texture has another size or format".into());
        }
        let device = self.device.clone();
        let queue = self.queue.clone();
        scoped(&device, || {
            let src_buf = self.source.as_ref().map(|s| s.buf.clone()).ok_or("no source")?;
            let img = if self.developed { self.img.buf.clone().ok_or("no develop")? } else { src_buf.clone() };
            let codes = self.codes.at_least(&device, 65536 * 4).clone();
            let lut = self.lut.at_least(&device, display.lut.map_or(16, |(_, v)| (v.len() * 4) as u64)).clone();
            let key = (display.encode.map_or(0, |t| t.as_ptr() as usize), display.lut.map_or(0, |(n, v)| v.as_ptr() as usize ^ (n as usize) << 1 ^ v.len()));
            if key != self.display_keys {
                if let Some(t) = display.encode {
                    let words: Vec<u32> = (0..65536).map(|i| u32::from(t.get(i).copied().unwrap_or(255))).collect();
                    queue.write_buffer(&codes, 0, &as_bytes(&words));
                }
                if let Some((_, v)) = display.lut {
                    queue.write_buffer(&lut, 0, &as_bytes(v));
                }
                self.display_keys = key;
            }
            let lut_n = display.lut.map_or(0, |(n, v)| if n >= 2 && v.len() >= (n as usize).pow(3) * 3 { n } else { 0 });
            let flags = u32::from(display.encode.is_some()) | if lut_n >= 2 { 2 } else { 0 } | if self.mask.is_some() { 4 } else { 0 };
            let uni = device.create_buffer(&wgpu::BufferDescriptor { label: Some("pc_camera_raw_display"), size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            queue.write_buffer(&uni, 0, &as_bytes(&[w, h, flags, lut_n]));
            let mask = self.mask.clone().unwrap_or_else(|| codes.clone());
            let view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pc_camera_raw_display"),
                layout: &self.display_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uni.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: img.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: src_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: mask.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: codes.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 5, resource: lut.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 7, resource: self.hist.as_entire_binding() },
                ],
            });
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_camera_raw_display") });
            enc.clear_buffer(&self.hist, 0, None);
            {
                let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("pc_camera_raw_display"), timestamp_writes: None });
                cp.set_pipeline(&self.display);
                cp.set_bind_group(0, &bind, &[]);
                let groups = (u64::from(w) * u64::from(h)).div_ceil(256).clamp(1, 512) as u32;
                cp.dispatch_workgroups(groups, 1, 1);
            }
            // A read still mapped (or mapping) can't be copied into: this frame's histogram is
            // then skipped, the last one stays.
            if self.hist_pending.is_none() {
                enc.copy_buffer_to_buffer(&self.hist, 0, &self.hist_read, 0, (HIST_WORDS * 4) as u64);
            }
            queue.submit([enc.finish()]);
            if self.hist_pending.is_none() {
                let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
                let done = state.clone();
                self.hist_read.slice(..).map_async(wgpu::MapMode::Read, move |r| done.store(if r.is_ok() { 1 } else { 2 }, std::sync::atomic::Ordering::Release));
                self.hist_pending = Some(state);
            }
            Ok(())
        })
    }

    /// The histogram of the last [`Self::present`], once the GPU has counted it: with `wait`,
    /// blocks until it has. `None` while it is still being counted (or when it failed).
    pub fn histogram(&mut self, wait: bool) -> Option<RgbHistogram> {
        let state = self.hist_pending.clone()?;
        let poll = if wait { wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(5)) } } else { wgpu::PollType::Poll };
        let _ = self.device.poll(poll);
        match state.load(std::sync::atomic::Ordering::Acquire) {
            0 => None,
            1 => {
                let h = {
                    let data = self.hist_read.slice(..).get_mapped_range().ok()?;
                    let words: Vec<u32> = data.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect();
                    histogram_of(&words)
                };
                self.hist_read.unmap();
                self.hist_pending = None;
                h
            }
            _ => {
                self.hist_pending = None;
                None
            }
        }
    }

    /// The developed pixels (or the source, before a develop), read back: blocks until the GPU
    /// is done. For tests and the colour samplers.
    pub fn read_back(&self) -> Result<Vec<[f32; 4]>, String> {
        let Some(src) = &self.source else { return Err("Camera Raw GPU: no source".into()) };
        let n = (src.w as usize) * (src.h as usize);
        let from = if self.developed { self.img.buf.clone().ok_or("no develop")? } else { src.buf.clone() };
        let bytes = (n * 16) as u64;
        let device = self.device.clone();
        let staging = scoped(&device, || {
            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pc_camera_raw_read"),
                size: bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_camera_raw_read") });
            enc.copy_buffer_to_buffer(&from, 0, &staging, 0, bytes);
            self.queue.submit([enc.finish()]);
            Ok(staging)
        })?;
        let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
        let done = state.clone();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| done.store(if r.is_ok() { 1 } else { 2 }, std::sync::atomic::Ordering::Release));
        let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) });
        if state.load(std::sync::atomic::Ordering::Acquire) != 1 {
            return Err("Camera Raw GPU: read back failed".into());
        }
        let out = {
            let data = staging.slice(..).get_mapped_range().map_err(|e| format!("Camera Raw GPU: read back failed: {e:?}"))?;
            data.as_chunks::<16>()
                .0
                .iter()
                .map(|p| std::array::from_fn(|c| f32::from_le_bytes([p[c * 4], p[c * 4 + 1], p[c * 4 + 2], p[c * 4 + 3]])))
                .collect()
        };
        staging.unmap();
        staging.destroy();
        Ok(out)
    }
}

/// The output size of a plane op over a `w × h` plane (`n`: the op's count).
fn plane_out(kind: u32, w: u32, h: u32, n: u32) -> (u32, u32) {
    if kind == program::plane::BLOCK && n > 1 { (w.div_ceil(n), h.div_ceil(n)) } else { (w, h) }
}

/// The `Pass` uniform of one pass, as 20 words.
fn pass_uniform(prog: &GpuProgram, pass: &GpuPass) -> [u32; 20] {
    let mut u = [0u32; 20];
    u[1] = prog.width;
    u[2] = prog.height;
    u[4] = prog.frame_x0 as u32;
    u[5] = prog.frame_y0 as u32;
    u[6] = prog.full_w;
    u[7] = prog.full_h;
    match pass {
        GpuPass::Pixel { start } => u[0] = *start,
        GpuPass::Distort { k, cx, cy, diag } => {
            u[14] = k.to_bits();
            u[15] = cx.to_bits();
            u[16] = cy.to_bits();
            u[17] = diag.to_bits();
        }
        GpuPass::Plane { kind, a, b, c, d, w, h, n, f } => {
            let (ow, oh) = plane_out(*kind, *w, *h, *n);
            u[1] = *w;
            u[2] = *h;
            u[3] = *kind;
            u[8] = *a;
            u[9] = *b;
            u[10] = *c;
            u[11] = *d;
            u[12] = *n;
            u[13] = f.to_bits();
            u[18] = ow;
            u[19] = oh;
        }
    }
    u
}

fn histogram_of(words: &[u32]) -> Option<RgbHistogram> {
    if words.len() < HIST_WORDS {
        return None;
    }
    let mut h = RgbHistogram::default();
    for c in 0..3 {
        for b in 0..256 {
            h.channels[c][b] = u64::from(words[c * 256 + b]);
        }
        h.underflow[c] = u64::from(words[771 + c]);
        h.overflow[c] = u64::from(words[774 + c]);
        h.shadows[c] = u64::from(words[777 + c]);
        h.highlights[c] = u64::from(words[780 + c]);
    }
    h.samples = u64::from(words[768]);
    h.transparent = u64::from(words[769]);
    h.invalid = u64::from(words[770]);
    Some(h)
}

/// The bytes of plain numbers (`f32`, `u32`, `[f32; 4]`), little-endian as wgpu takes them.
fn as_bytes<T: Plain>(v: &[T]) -> Vec<u8> {
    let mut out = Vec::with_capacity(std::mem::size_of_val(v));
    for x in v {
        x.push(&mut out);
    }
    out
}

trait Plain {
    fn push(&self, out: &mut Vec<u8>);
}

impl Plain for f32 {
    fn push(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
}
impl Plain for u32 {
    fn push(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_le_bytes());
    }
}
impl Plain for [f32; 4] {
    fn push(&self, out: &mut Vec<u8>) {
        for v in self {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
}
