//! Which layer effect makes painting on its layer slow: a document of thin strokes (as a line
//! drawing has), one effect kind at a time, timed on the GPU compositor for a full refresh and for
//! a brush dab (the damage rect grown by the effect's reach, as the canvas refreshes it).
//!
//! ```sh
//! cargo run --release -p photocraft-ui-egui --example fx_kinds_bench -- [--size 3000x3000] [--reps 5] [--cpu] [--masked]
//! ```

use std::time::Instant;

use eframe::wgpu;
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer};
use photocraft_geom::{Rect, Size};

const KINDS: &[&str] =
    &["dropShadow", "innerShadow", "outerGlow", "innerGlow", "bevelEmboss", "satin", "colorOverlay", "gradientOverlay", "patternOverlay", "stroke"];

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

/// Concentric U-shaped strokes, 6 px wide, over most of the canvas.
fn strokes(w: u32, h: u32) -> Document {
    let mut d = Document::new("strokes", Size::new(w, h), ColorMode::Rgb, SampleType::U8);
    let mut bg = Layer::raster("bg", d.pixel_format());
    if let Some(s) = bg.surface_mut() {
        s.fill_rect(Rect::new(0, 0, w as i32, h as i32), &[0.0, 0.0, 0.0, 1.0]);
    }
    d.layers.push(bg);
    let mut l = Layer::raster("lines", d.pixel_format());
    if let Some(s) = l.surface_mut() {
        let (cx, cy) = (w as f64 / 2.0, h as f64 * 0.55);
        for k in 0..6 {
            let r = w as f64 * 0.18 + k as f64 * 30.0;
            for step in 0..4000 {
                let a = std::f64::consts::PI * step as f64 / 4000.0;
                let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
                s.fill_rect(Rect::new(x as i32 - 3, y as i32 - 3, x as i32 + 3, y as i32 + 3), &[1.0, 1.0, 1.0, 1.0]);
            }
            for y in (h as f64 * 0.1) as i32..cy as i32 {
                for x in [cx - r, cx + r] {
                    s.fill_rect(Rect::new(x as i32 - 3, y, x as i32 + 3, y + 1), &[1.0, 1.0, 1.0, 1.0]);
                }
            }
        }
    }
    d.layers.push(l);
    d
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (w, h) = arg(&args, "--size").and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))).unwrap_or((3000, 3000));
    let reps: usize = arg(&args, "--reps").and_then(|v| v.parse().ok()).unwrap_or(5);
    let cpu = args.iter().any(|a| a == "--cpu");
    // `--masked`: each effect gets its own mask (the left half of the canvas), as effect masks do.
    let masked = args.iter().any(|a| a == "--masked");
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) =
        block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }))
    else {
        eprintln!("no GPU adapter");
        return;
    };
    let limits = photocraft_ui_egui::gpu_canvas::device_limits(&adapter);
    let Ok((device, queue)) = block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_limits: limits, ..Default::default() })) else {
        eprintln!("no device");
        return;
    };
    println!("{} — {w}x{h}, {reps} dabs each", adapter.get_info().name);
    let base = strokes(w, h);
    let full = base.bounds();
    for kinds in KINDS.iter().map(|k| vec![*k]).chain(std::iter::once(KINDS.to_vec())) {
        let mut doc = base.clone();
        let mut comp = photocraft_gpu::Compositor::new(&device);
        if let Some(l) = doc.layers.last_mut() {
            for k in &kinds {
                let p = photocraft_engine::layer_style::effect_defaults(k);
                if let Some(mut e) = photocraft_engine::layer_style::effect_from_params(k, &p) {
                    if masked {
                        let mut m = photocraft_doc::LayerMask::hide_all();
                        m.surface.fill_rect(Rect::new(0, 0, w as i32 / 2, h as i32), &[1.0]);
                        *e.mask_mut() = Some(m);
                    }
                    l.effects.items.push(e);
                }
            }
        }
        let margin = doc.layers.last().map_or(0, photocraft_compose::effects::margin);
        let time = |comp: &mut photocraft_gpu::Compositor, doc: &Document, r: Rect| -> Result<f64, String> {
            let t = Instant::now();
            comp.render(&device, &queue, doc, r, |_, _| {}).map_err(|e| e.to_string())?;
            let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
            Ok(t.elapsed().as_secs_f64() * 1000.0)
        };
        let cold = time(&mut comp, &doc, full);
        let warm = time(&mut comp, &doc, full);
        let mut dabs = Vec::new();
        let mut cpu_dabs = Vec::new();
        let mut err = None;
        for i in 0..reps {
            let (x, y) = (w as i32 / 2 - (w as f64 * 0.18) as i32 + 3, h as i32 / 3 + i as i32 * 90);
            let r = Rect::new(x - 16, y - 16, x + 16, y + 16);
            if let Some(s) = doc.layers.last_mut().and_then(Layer::surface_mut) {
                s.fill_rect(r, &[0.8, 0.8, 0.8, 1.0]);
            }
            let region = r.inflate(margin).intersect(&full);
            match time(&mut comp, &doc, region) {
                Ok(v) => dabs.push(v),
                Err(e) => err = Some(e),
            }
            if cpu {
                let t = Instant::now();
                std::hint::black_box(photocraft_compose::render(&doc, region));
                cpu_dabs.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        dabs.sort_by(f64::total_cmp);
        cpu_dabs.sort_by(f64::total_cmp);
        let med = |v: &[f64]| v.get(v.len() / 2).copied().unwrap_or(f64::NAN);
        let name = if kinds.len() == 1 { kinds[0].to_string() } else { "all ten".into() };
        match (cold, warm, err) {
            (Ok(c), Ok(wm), None) => println!(
                "{name:<16} reach {margin:>4} px   full cold {c:7.1} ms  warm {wm:6.1} ms   dab median {:6.1} ms  max {:6.1} ms{}",
                med(&dabs),
                dabs.last().copied().unwrap_or(f64::NAN),
                if cpu { format!("   cpu dab {:7.1} ms", med(&cpu_dabs)) } else { String::new() }
            ),
            (c, wm, e) => println!("{name:<16} unsupported: {:?} {:?} {:?}", c.err(), wm.err(), e),
        }
    }
}
