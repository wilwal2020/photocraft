//! Camera Raw slider steps on a dialog-sized proxy (2048 × 1365): the time `develop_cached` takes
//! when one control moves, as the dialog's preview runs it while a slider is dragged.
use photocraft_algo::camera_raw::{CameraRaw, DevelopCache, develop_cached};

fn main() {
    let (w, h) = (2048usize, 1365usize);
    // Smooth hue and tone sweeps with some texture, like a photo's mix of flat and busy areas.
    let src: Vec<[f32; 4]> = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            let t = ((x * 37.0).sin() * (y * 23.0).cos()) * 0.05;
            [(0.5 + 0.45 * (x * 6.0).sin() + t).clamp(0.0, 1.0), (0.5 + 0.45 * (y * 5.0 + 1.0).sin()).clamp(0.0, 1.0), (x * 0.6 + y * 0.4).clamp(0.0, 1.0), 1.0]
        })
        .collect();
    let reps = 12;
    type Set = fn(&mut CameraRaw, f32);
    let cases: [(&str, Set); 6] = [
        ("exposure", |p, v| p.exposure = v * 0.5),
        ("temperature", |p, v| p.temperature = v * 20.0),
        ("contrast", |p, v| p.contrast = v * 30.0),
        ("vibrance", |p, v| p.vibrance = v * 30.0),
        ("color mixer", |p, v| p.hsl_lum[5] = -v * 30.0),
        ("grading", |p, v| p.grade_shadows.sat = v * 30.0),
    ];
    for (name, set) in cases {
        let mut cache = DevelopCache::default();
        let mut p = CameraRaw { saturation: 5.0, ..Default::default() };
        let mut times = Vec::new();
        for i in 0..reps + 2 {
            set(&mut p, if i % 2 == 0 { 0.5 } else { 1.0 });
            let t = std::time::Instant::now();
            let d = develop_cached(&src, w, h, &p, false, 1, &mut cache, true);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            cache.recycle(d.px);
            if i >= 2 {
                times.push(ms);
            }
        }
        times.sort_by(f64::total_cmp);
        // The drafts against the exact develop of the same settings.
        let draft = develop_cached(&src, w, h, &p, false, 1, &mut cache, true).px;
        let exact = develop_cached(&src, w, h, &p, false, 1, &mut DevelopCache::default(), false).px;
        let (mut sum, mut max, mut at) = (0.0f64, 0.0f32, 0usize);
        for (i, (a, b)) in draft.iter().zip(&exact).enumerate() {
            for c in 0..3 {
                let d = (a[c] - b[c]).abs() * 255.0;
                sum += f64::from(d);
                if d > max {
                    (max, at) = (d, i);
                }
            }
        }
        if std::env::var_os("WORST").is_some() {
            println!("  worst at {at}: src {:?} draft {:?} exact {:?}", src[at], draft[at], exact[at]);
        }
        println!("{name:<12} median {:6.2} ms  max {:6.2} ms   draft vs exact: mean {:.3} max {:.2} levels", times[times.len() / 2], times[times.len() - 1], sum / (exact.len() * 3) as f64, max);
    }
}
