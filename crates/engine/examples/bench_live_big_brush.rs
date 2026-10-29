//! A large brush's live stroke on a 4000×4000 page, fed as the canvas feeds it: `begin` on the
//! press, then two points per 60 Hz frame, with the canvas holding the shown document between
//! frames (`--held`) or not (user report: 200–800 ms "live stroke" spans; their brush was a
//! 5000 px soft round on a 2106×3000 page: `--size 5000 --hardness 0 --spacing 25 --page 2106x3000`).
//! Pushes of 50 ms or more print where they spent it.
//! Run with `cargo run --release -p photocraft-engine --example bench_live_big_brush -- [--held]
//! [--size PX] [--spacing PCT] [--hardness 0..1] [--page WxH] [--frames N]`.
use photocraft_engine::{Session, brush_cmds::LiveStroke};
use photocraft_paint::StrokePoint;
use serde_json::json;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<f64>().ok());
    let held = args.iter().any(|a| a == "--held");
    let size = arg("--size").unwrap_or(429.0);
    let spacing = arg("--spacing").unwrap_or(1.0) / 100.0;
    let hardness = arg("--hardness").unwrap_or(1.0);
    let frames = arg("--frames").map_or(120, |v| v as usize);
    let (w, h) = args
        .iter()
        .position(|a| a == "--page")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.split_once('x'))
        .and_then(|(w, h)| Some((w.parse::<f64>().ok()?, h.parse::<f64>().ok()?)))
        .unwrap_or((4000.0, 4000.0));
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "depth": 8, "mode": "rgb"}))?;
    s.execute("layer.new.layer", json!({}))?;
    // The user's "hardbrush": hard round, pen pressure on size.
    let params = json!({"points": [[w * 0.1, h * 0.5, 0.5]], "size": size, "hardness": hardness, "spacing": spacing, "seed": 7, "brush": {"pressureSize": true}});
    let t0 = Instant::now();
    let mut live = LiveStroke::begin(&s, &params)?;
    let begin = t0.elapsed().as_secs_f64() * 1000.0;
    // 2000 px/s sampled at 120 Hz, two samples per frame, pressure rising and falling.
    let step = 2000.0 / 120.0;
    let pt = |i: usize| {
        let d = i as f64 * step;
        let mut p = StrokePoint::new(w * 0.1 + d % (w * 0.8), h * 0.5 + h * 0.3 * (d / 700.0).sin(), 0.6 + 0.4 * (d / 300.0).sin() as f32);
        p.time = i as f64 * 1000.0 / 120.0;
        p
    };
    let mut shown = None;
    let mut ms = Vec::new();
    for f in 0..frames {
        let t = Instant::now();
        live.push(&[pt(2 * f + 1), pt(2 * f + 2)])?;
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
        if ms.last().is_some_and(|&m| m >= 50.0) {
            println!("  frame {f}: {:?}", live.stats);
        }
        if held {
            shown = Some(live.doc.clone());
        }
    }
    std::hint::black_box(shown);
    ms.sort_by(f64::total_cmp);
    let at = |q: usize| ms.get((ms.len() * q / 100).min(ms.len().saturating_sub(1))).copied().unwrap_or(0.0);
    println!(
        "{w}x{h}, {size} px, hardness {hardness}, spacing {:.0} %, held {held}: begin {begin:.1} ms, push p50 {:.2} p95 {:.2} max {:.2} ms over {frames} frames",
        spacing * 100.0,
        at(50),
        at(95),
        at(100)
    );
    Ok(())
}
