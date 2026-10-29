//! Large round brushes on a 4000×4000 RGBA8 layer, fed frame by frame as the canvas feeds a live
//! stroke: each frame's `push` (dabs: rasterise and accumulate) and `composite` (the dirty
//! coverage tiles onto the layer), with the pixels each stage touched, so the per-pixel cost of
//! each stage shows (run with `RAYON_NUM_THREADS=1` for single-core numbers).
//!
//! ```sh
//! cargo run --release -p photocraft-paint --example big_brush_bench -- [--frames N] [--size PX] [--spacing PCT]
//! ```

use std::time::Instant;

use photocraft_color::PixelFormat;
use photocraft_geom::Rect;
use photocraft_paint::{BrushSettings, StrokePoint, StrokeRenderer};
use photocraft_raster::Surface;

fn run(name: &str, fmt: PixelFormat, brush: &BrushSettings, frames: usize) {
    let (w, h) = (4000, 4000);
    let mut target = Surface::new(fmt);
    target.fill_rect(Rect::new(0, 0, w, h), &photocraft_raster::from_rgba(&fmt, [0.9, 0.85, 0.8, 1.0]));
    let pre = target.clone();
    let mut r = StrokeRenderer::new(brush, Some(target.format()), 1.0);
    // 2000 px/s sampled at 120 Hz, two samples per 60 Hz frame.
    let step = 2000.0 / 120.0;
    let pt = |i: usize| {
        let s = i as f64 * step;
        let x = 400.0 + s % 3200.0;
        let y = 2000.0 + 1200.0 * (s / 700.0).sin();
        let mut p = StrokePoint::new(x, y, 1.0);
        p.time = i as f64 * 1000.0 / 120.0;
        p
    };
    let (mut push_ms, mut comp_ms) = (Vec::new(), Vec::new());
    let mut comp_px = 0u64;
    for f in 0..frames {
        let pts = [pt(2 * f), pt(2 * f + 1)];
        let t0 = Instant::now();
        r.push(&pts);
        let t1 = Instant::now();
        let dmg = r.composite(&pre, &mut target, None, false, false);
        let t2 = Instant::now();
        push_ms.push((t1 - t0).as_secs_f64() * 1000.0);
        comp_ms.push((t2 - t1).as_secs_f64() * 1000.0);
        comp_px += u64::from(dmg.width()) * u64::from(dmg.height());
    }
    let stat = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let sum: f64 = v.iter().sum();
        (v[v.len() / 2], v[(v.len() * 95 / 100).min(v.len() - 1)], sum)
    };
    let (p50, p95, sp) = stat(&mut push_ms);
    let (c50, c95, sc) = stat(&mut comp_ms);
    println!(
        "{name:<42} push p50 {p50:7.2} p95 {p95:7.2} | composite p50 {c50:7.2} p95 {c95:7.2} ms | {:>4} dabs | {:.1} ns/composited px | {:.0} ms total for {:.0} ms of stroke",
        r.dab_count(),
        sc * 1e6 / comp_px.max(1) as f64,
        sp + sc,
        frames as f64 * 1000.0 / 60.0,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<f32>().ok());
    let frames = arg("--frames").map_or(90, |v| v as usize);
    let sizes: Vec<f32> = arg("--size").map_or_else(|| vec![300.0, 1000.0, 2500.0], |s| vec![s]);
    let base = BrushSettings { pressure_size: false, seed: 7, ..Default::default() };
    // The tip spacing, % of the size (the default brush's 10 %; the user's hard brush is 1 %).
    let spacing = arg("--spacing").map_or(base.spacing, |v| v / 100.0);
    for fmt in [PixelFormat::RGBA8, PixelFormat::RGBA16] {
        for &size in &sizes {
            for (kind, hardness) in [("soft", 0.0), ("hard", 1.0)] {
                let b = BrushSettings { size, hardness, spacing, ..base.clone() };
                run(&format!("{:?} round {kind} {size} px, spacing {:.0} %", fmt.sample, spacing * 100.0), fmt, &b, frames);
            }
        }
    }
}
