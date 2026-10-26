//! Camera Raw's GPU develop against the CPU's: every stage, alone and together, on a synthetic
//! image, with the measurements the CPU develop took (so any difference is the GPU's own).

use photocraft_algo::camera_raw::{CameraRaw, DevelopCache, Frame, Wheel, develop_cached, gpu};
use photocraft_algo::histogram::RgbHistogram;
use photocraft_gpu::camera_raw::{CameraRawGpu, Display};

use super::GPU_LOCK;

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
        eprintln!("skipping Camera Raw GPU tests: no adapter");
        return None;
    };
    let limits = adapter.limits();
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_limits: limits, ..Default::default() })).ok()
}

/// Gradients, edges, saturated and pale colours, a little noise.
fn image(w: usize, h: usize, float: bool) -> Vec<[f32; 4]> {
    let mut seed = 0x2545_f491_u32;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32) / (u32::MAX as f32)
    };
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            let band = ((x * 6.0) as usize) % 3;
            let mut c = [x, y, 1.0 - x * y];
            c[band] = (c[band] * 0.4 + 0.6 * (1.0 - y)).min(1.0);
            if (x - 0.5).abs() < 0.1 && (y - 0.5).abs() < 0.2 {
                c = [0.95, 0.9, 0.85];
            }
            let n = (rnd() - 0.5) * 0.06;
            let scale = if float && x > 0.7 { 2.5 } else { 1.0 };
            [((c[0] + n) * scale).max(0.0), ((c[1] + n) * scale).max(0.0), ((c[2] + n) * scale).max(0.0), 1.0]
        })
        .map(|q| if float { q } else { [q[0].min(1.0), q[1].min(1.0), q[2].min(1.0), q[3]] })
        .collect()
}

struct Diff {
    max: f32,
    mean: f32,
    at: usize,
}

fn compare(a: &[[f32; 4]], b: &[[f32; 4]]) -> Diff {
    let (mut max, mut sum, mut at) = (0.0f32, 0.0f64, 0);
    for (i, (p, q)) in a.iter().zip(b).enumerate() {
        for c in 0..4 {
            let d = (p[c] - q[c]).abs();
            let d = if d.is_nan() { f32::INFINITY } else { d };
            if d > max {
                max = d;
                at = i;
            }
            sum += f64::from(d.min(1.0));
        }
    }
    Diff { max, mean: (sum / (a.len() * 4) as f64) as f32, at }
}

/// Develops `p` on both and checks the GPU's result is the CPU's: no pixel more than `max` off,
/// on average under 1/100 of a level.
fn check(gpu_dev: &mut CameraRawGpu, name: &str, p: &CameraRaw, w: usize, h: usize, float: bool, max: f32) {
    let src = image(w, h, float);
    let cpu = develop_cached(&src, w, h, p, float, 1, &mut DevelopCache::default(), false);
    assert!(cpu.exact, "{name}: the CPU develop is a draft");
    let prog = gpu::program(p, w, h, Frame::whole(w, h), &cpu.context, float, true).unwrap_or_else(|e| panic!("{name}: {e}"));
    gpu_dev.set_source(name.len() as u64 ^ (w as u64) << 20 ^ u64::from(float) << 40, &src, w as u32, h as u32, None).unwrap_or_else(|e| panic!("{name}: {e}"));
    gpu_dev.develop(&prog).unwrap_or_else(|e| panic!("{name}: {e}"));
    let out = gpu_dev.read_back().unwrap_or_else(|e| panic!("{name}: {e}"));
    let d = compare(&cpu.px, &out);
    eprintln!("{name}: max {:.2e} mean {:.2e} ({} passes)", d.max, d.mean, prog.passes.len());
    assert!(
        d.max <= max && d.mean < 0.01 / 255.0,
        "{name}: GPU differs from CPU by {} at pixel {} ({:?} vs {:?}), mean {}",
        d.max,
        d.at,
        cpu.px[d.at],
        out[d.at],
        d.mean
    );
}

const LEVEL: f32 = 1.0 / 255.0;

fn cases() -> Vec<(&'static str, CameraRaw)> {
    let d = CameraRaw::default;
    vec![
        ("exposure", CameraRaw { exposure: 1.2, ..d() }),
        ("exposure and contrast", CameraRaw { exposure: -0.8, contrast: 40.0, ..d() }),
        ("negative contrast", CameraRaw { contrast: -60.0, ..d() }),
        ("white balance", CameraRaw { temperature: 30.0, tint: -20.0, ..d() }),
        ("calibration", CameraRaw { red_hue: 30.0, blue_saturation: -40.0, green_hue: -15.0, shadow_tint: 20.0, ..d() }),
        ("shadows and highlights", CameraRaw { highlights: -60.0, shadows: 50.0, ..d() }),
        ("highlights up", CameraRaw { highlights: 40.0, ..d() }),
        ("dehaze", CameraRaw { dehaze: 40.0, ..d() }),
        ("haze", CameraRaw { dehaze: -30.0, ..d() }),
        ("whites and blacks", CameraRaw { whites: 40.0, blacks: -30.0, ..d() }),
        ("texture and clarity", CameraRaw { texture: 50.0, clarity: 40.0, ..d() }),
        ("negative clarity", CameraRaw { clarity: -60.0, ..d() }),
        (
            "curves",
            CameraRaw {
                curve_highlights: -30.0,
                curve_darks: 20.0,
                curve_splits: [20.0, 55.0, 80.0],
                point_curve: vec![[0.0, 10.0], [128.0, 140.0], [255.0, 245.0]],
                point_curve_red: vec![[0.0, 0.0], [100.0, 120.0], [255.0, 255.0]],
                point_curve_blue: vec![[0.0, 20.0], [255.0, 230.0]],
                curve_refine_saturation: 60.0,
                ..d()
            },
        ),
        ("vibrance and saturation", CameraRaw { vibrance: 40.0, saturation: -20.0, ..d() }),
        ("vibrance down", CameraRaw { vibrance: -50.0, saturation: 30.0, ..d() }),
        (
            "color mixer",
            CameraRaw {
                hsl_hue: [20.0, -30.0, 0.0, 40.0, 0.0, -60.0, 10.0, 0.0],
                hsl_sat: [-50.0, 30.0, 60.0, 0.0, -100.0, 40.0, 0.0, 20.0],
                hsl_lum: [10.0, 0.0, -40.0, 30.0, 0.0, -44.0, 0.0, 70.0],
                ..d()
            },
        ),
        ("black and white", CameraRaw { monochrome: true, gray_mix: [30.0, -20.0, 10.0, 0.0, 40.0, -50.0, 0.0, 20.0], ..d() }),
        (
            "color grading",
            CameraRaw {
                grade_shadows: Wheel { hue: 220.0, sat: 30.0, lum: -10.0 },
                grade_midtones: Wheel { hue: 40.0, sat: 20.0, lum: 15.0 },
                grade_highlights: Wheel { hue: 50.0, sat: 25.0, lum: 0.0 },
                grade_global: Wheel { hue: 300.0, sat: 10.0, lum: -5.0 },
                grade_blending: 70.0,
                grade_balance: -20.0,
                ..d()
            },
        ),
        (
            "detail",
            CameraRaw {
                sharpen_amount: 60.0,
                sharpen_radius: 1.4,
                sharpen_detail: 40.0,
                sharpen_masking: 30.0,
                noise_luminance: 50.0,
                noise_luminance_contrast: 40.0,
                noise_color: 40.0,
                noise_color_smoothness: 80.0,
                ..d()
            },
        ),
        ("colour noise, narrow", CameraRaw { noise_color: 30.0, noise_color_smoothness: 20.0, ..d() }),
        ("vignette", CameraRaw { vignette_amount: -40.0, vignette_highlights: 50.0, vignette_roundness: 30.0, vignette_feather: 70.0, ..d() }),
        ("vignette, square", CameraRaw { vignette_amount: 30.0, vignette_roundness: -40.0, vignette_midpoint: 30.0, ..d() }),
        ("vignette, colour priority", CameraRaw { vignette_amount: -50.0, vignette_style: "colorPriority".into(), ..d() }),
        ("vignette, paint", CameraRaw { vignette_amount: 30.0, vignette_style: "paintOverlay".into(), ..d() }),
        ("grain", CameraRaw { grain_amount: 50.0, grain_size: 40.0, grain_roughness: 70.0, seed: 4_000_000_000, ..d() }),
        ("optics", CameraRaw { lens_distortion: 30.0, lens_vignette_amount: 40.0, lens_vignette_midpoint: 30.0, defringe_purple_amount: 10.0, defringe_green_amount: 8.0, ..d() }),
        (
            "everything",
            CameraRaw {
                temperature: -15.0,
                exposure: 0.4,
                contrast: 25.0,
                highlights: -40.0,
                shadows: 30.0,
                whites: 15.0,
                blacks: -10.0,
                texture: 20.0,
                clarity: 15.0,
                dehaze: 10.0,
                vibrance: 25.0,
                saturation: 5.0,
                curve_lights: 15.0,
                hsl_sat: [0.0, 0.0, 0.0, 0.0, 0.0, 30.0, 0.0, 0.0],
                grade_shadows: Wheel { hue: 210.0, sat: 15.0, lum: 0.0 },
                sharpen_amount: 40.0,
                noise_luminance: 20.0,
                vignette_amount: -20.0,
                grain_amount: 15.0,
                ..d()
            },
        ),
    ]
}

#[test]
fn every_stage_on_the_gpu_matches_the_cpu() {
    let _lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((device, queue)) = device() else { return };
    let mut gpu_dev = CameraRawGpu::new(&device, &queue).expect("Camera Raw GPU pipelines");
    for (name, p) in cases() {
        check(&mut gpu_dev, name, &p, 211, 143, false, LEVEL / 10.0);
    }
}

#[test]
fn float_and_power_law_documents_match_the_cpu() {
    let _lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((device, queue)) = device() else { return };
    let mut gpu_dev = CameraRawGpu::new(&device, &queue).expect("Camera Raw GPU pipelines");
    for (name, p) in cases() {
        let pro = CameraRaw { encoding_gamma: 1.8, ..p.clone() };
        check(&mut gpu_dev, &format!("{name} (ProPhoto)"), &pro, 160, 97, false, LEVEL / 10.0);
    }
    for (name, p) in [("float exposure", CameraRaw { exposure: 2.0, contrast: 20.0, ..Default::default() }), ("float everything", cases().pop().map(|c| c.1).unwrap_or_default())] {
        // Values above 1 stay: the GPU must agree there too.
        check(&mut gpu_dev, name, &p, 160, 97, true, LEVEL / 4.0);
    }
}

/// A large image takes the subsampled guided filter for Clarity (as a proxy does).
#[test]
fn clarity_on_a_proxy_sized_image_matches_the_cpu() {
    let _lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((device, queue)) = device() else { return };
    let mut gpu_dev = CameraRawGpu::new(&device, &queue).expect("Camera Raw GPU pipelines");
    let p = CameraRaw { clarity: 50.0, texture: -30.0, shadows: 40.0, dehaze: 20.0, ..Default::default() };
    check(&mut gpu_dev, "clarity, 1200 px", &p, 1200, 800, false, LEVEL / 10.0);
}

/// The preview's bytes and histogram are the CPU preview's (`CanvasDisplay::rgba8_into` without a
/// monitor transform, egui's premultiplying, `RgbHistogram::from_rgba`), with a selection.
#[test]
fn the_preview_texture_and_histogram_match_the_cpu() {
    let _lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((device, queue)) = device() else { return };
    let mut gpu_dev = CameraRawGpu::new(&device, &queue).expect("Camera Raw GPU pipelines");
    let (w, h) = (130usize, 90usize);
    let mut src = image(w, h, false);
    for (i, q) in src.iter_mut().enumerate() {
        q[3] = [1.0, 0.5, 0.0, 0.25][i % 4];
    }
    let mask: Vec<f32> = (0..w * h).map(|i| ((i % w) as f32 / w as f32).min(1.0)).collect();
    let p = CameraRaw { exposure: 0.7, vibrance: 30.0, ..Default::default() };
    let cpu = develop_cached(&src, w, h, &p, false, 1, &mut DevelopCache::default(), false);
    let blended: Vec<[f32; 4]> = cpu.px.iter().zip(&src).zip(&mask).map(|((d, o), m)| std::array::from_fn(|c| o[c] + (d[c] - o[c]) * m)).collect();
    let prog = gpu::program(&p, w, h, Frame::whole(w, h), &cpu.context, false, true).expect("program");
    gpu_dev.set_source(7, &src, w as u32, h as u32, Some(&mask)).expect("source");
    gpu_dev.develop(&prog).expect("develop");
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    gpu_dev.present(&target, Display::default()).expect("present");
    let hist = gpu_dev.histogram(true).expect("histogram");
    let want = RgbHistogram::from_rgba(&blended);
    // A value a hair from a bin's edge may round to either neighbour.
    let moved: u64 = (0..3).map(|c| hist.channels[c].iter().zip(&want.channels[c]).map(|(a, b)| a.abs_diff(*b)).sum::<u64>()).sum();
    assert_eq!((hist.samples, hist.transparent, hist.invalid), (want.samples, want.transparent, want.invalid));
    assert!(moved <= 6, "histogram bins differ by {moved}");
    // The texture's bytes.
    let row = (w as u32 * 4).next_multiple_of(256);
    let buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: u64::from(row) * h as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h as u32) } },
        wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    let data = buf.slice(..).get_mapped_range().expect("mapped");
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let premultiply = |v: u8, a: u8| {
        let p = u16::from(v) * u16::from(a) + 128;
        ((p + (p >> 8)) >> 8) as u8
    };
    let mut off = 0;
    for y in 0..h {
        for x in 0..w {
            let q = blended[y * w + x];
            let a = byte(q[3]);
            let want: [u8; 4] = match a {
                0 => [0; 4],
                255 => [byte(q[0]), byte(q[1]), byte(q[2]), 255],
                a => [premultiply(byte(q[0]), a), premultiply(byte(q[1]), a), premultiply(byte(q[2]), a), a],
            };
            let o = y * row as usize + x * 4;
            let got = [data[o], data[o + 1], data[o + 2], data[o + 3]];
            if want.iter().zip(&got).any(|(a, b)| a.abs_diff(*b) > 1) {
                off += 1;
            }
        }
    }
    assert_eq!(off, 0, "{off} preview pixels differ by more than one level");
}

/// Milliseconds per develop and preview at a 2048 px proxy's size (`--ignored --nocapture`).
#[test]
#[ignore]
fn camera_raw_gpu_timings() {
    let _lock = GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((device, queue)) = device() else { return };
    let mut gpu_dev = CameraRawGpu::new(&device, &queue).expect("Camera Raw GPU pipelines");
    let (w, h) = (2048usize, 1365usize);
    let src = image(w, h, false);
    gpu_dev.set_source(1, &src, w as u32, h as u32, None).expect("source");
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let d = CameraRaw::default;
    let mut all = cases();
    all.push(("clarity", CameraRaw { clarity: 50.0, ..d() }));
    for (name, p) in all {
        let p = CameraRaw { pixel_scale: 2048.0 / 6000.0, ..p };
        let cpu_start = std::time::Instant::now();
        let context = gpu::measure(&src, w, h, &p, 1, &mut DevelopCache::default(), false);
        let measure_ms = cpu_start.elapsed().as_secs_f64() * 1e3;
        let (mut best, mut build) = (f64::MAX, f64::MAX);
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let prog = gpu::program(&p, w, h, Frame::whole(w, h), &context, false, true).expect("program");
            build = build.min(t.elapsed().as_secs_f64() * 1e3);
            gpu_dev.develop(&prog).expect("develop");
            gpu_dev.present(&target, Display::default()).expect("present");
            gpu_dev.histogram(true).expect("histogram");
            best = best.min(t.elapsed().as_secs_f64() * 1e3);
        }
        eprintln!("{name:28} step {best:6.2} ms, of it building the program {build:5.2} ms   (CPU measurements on the proxy {measure_ms:6.1} ms)");
    }
}

/// What a drag of a measured control costs on the CPU per step: the measurements on the 1024 px
/// copy (draft), against the whole CPU develop of the 2048 px proxy as a drag ran it before
/// (`--ignored --nocapture`).
#[test]
#[ignore]
fn camera_raw_measured_drag_timings() {
    let (w, h) = (2048usize, 1365usize);
    let src = image(w, h, false);
    let (sw, sh) = (1024usize, 683usize);
    let small: Vec<[f32; 4]> = (0..sw * sh).map(|i| src[(i / sw * 2).min(h - 1) * w + (i % sw) * 2]).collect();
    let d = CameraRaw::default;
    let steps = |v: f32| -> [CameraRaw; 5] {
        [
            CameraRaw { shadows: v, ..d() },
            CameraRaw { highlights: -v, ..d() },
            CameraRaw { contrast: v, ..d() },
            CameraRaw { whites: v, blacks: -v / 2.0, ..d() },
            CameraRaw { dehaze: v / 2.0, ..d() },
        ]
    };
    let names = ["Shadows", "Highlights", "Contrast", "Whites/Blacks", "Dehaze"];
    for (k, name) in names.iter().enumerate() {
        let (mut cache, mut cpu_cache) = (DevelopCache::default(), DevelopCache::default());
        let (mut gpu_ms, mut cpu_ms) = (0.0, 0.0);
        let n = 8;
        for s in 0..=n {
            let p = steps(20.0 + s as f32 * 3.0)[k].clone();
            let q = CameraRaw { pixel_scale: 1024.0 / 6000.0, ..p.clone() };
            let t = std::time::Instant::now();
            let _ = gpu::measure(&small, sw, sh, &q, 3, &mut cache, true);
            let a = t.elapsed().as_secs_f64() * 1e3;
            let t = std::time::Instant::now();
            let _ = develop_cached(&src, w, h, &CameraRaw { pixel_scale: 2048.0 / 6000.0, ..p }, false, 4, &mut cpu_cache, true);
            let b = t.elapsed().as_secs_f64() * 1e3;
            if s > 0 {
                gpu_ms += a / n as f64;
                cpu_ms += b / n as f64;
            }
        }
        eprintln!("{:14} drag step: measurements on the copy {gpu_ms:6.1} ms (+ ~1.5 ms GPU), CPU develop before {cpu_ms:6.1} ms", name);
    }
}
