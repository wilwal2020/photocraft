//! The Camera Raw dialog on a real device: its preview develops on the GPU and shows what the CPU
//! develop would (see `camera_raw_gpu`).

use serde_json::json;

use super::*;

/// The app on a real device, or `None` without a GPU adapter.
fn gpu_harness() -> Option<egui_kittest::Harness<'static, PhotocraftApp>> {
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(8)
            .wgpu_setup(crate::gpu_canvas::wgpu_setup())
            .build_eframe(move |cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
                if let Some(rs) = cc.wgpu_render_state.as_ref() {
                    app.set_wgpu(rs.clone());
                }
                app
            })
    }));
    match built {
        Ok(h) => Some(h),
        Err(_) => {
            eprintln!("skipping: no GPU adapter");
            None
        }
    }
}

/// Settings that reach every kind of GPU pass: pixel ops, planes, measured maps.
fn settings() -> CameraRaw {
    CameraRaw { exposure: 0.6, contrast: 20.0, shadows: 30.0, clarity: 25.0, vibrance: 30.0, sharpen_amount: 40.0, vignette_amount: -20.0, ..Default::default() }
}

#[test]
fn the_preview_develops_on_the_gpu_and_matches_the_cpu() {
    let Some(mut harness) = gpu_harness() else { return };
    let ctx = harness.ctx.clone();
    let app = harness.state_mut();
    if app.gpu.is_none() {
        eprintln!("skipping: no GPU canvas");
        return;
    }
    app.run("file.new", json!({"width": 300, "height": 200})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    app.run("edit.fill", json!({"color": "#6a8fb0"})).unwrap();
    app.run("select.rect", json!({"x": 30, "y": 20, "width": 200, "height": 120})).unwrap();
    app.run("edit.fill", json!({"color": "#d07040"})).unwrap();
    // A feathered selection: the preview blends the develop in by its coverage.
    app.run("select.rect", json!({"x": 100, "y": 60, "width": 150, "height": 100})).unwrap();
    app.run("select.modify.feather", json!({"radius": 12})).unwrap();
    open(app, &ctx).unwrap();
    let d = app.camera_raw.as_mut().unwrap();
    assert!(d.gpu.is_some(), "a wgpu canvas gives the dialog a GPU preview");
    d.params = settings();
    // The pipelines build on a thread: the CPU develops until they are ready.
    let start = std::time::Instant::now();
    loop {
        d.render(&ctx);
        if d.gpu_shown || start.elapsed() > std::time::Duration::from_secs(60) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(d.gpu_shown, "the GPU preview never took over");
    assert!(!d.draft_shown);
    let (gpu_px, gpu_hist) = (d.processed.clone(), d.after_histogram.clone());
    assert_eq!(d.describe(&Default::default(), &Default::default())["gpu"], true);
    // The same settings on the CPU.
    let gpu = d.gpu.take();
    d.render(&ctx);
    assert!(!d.gpu_shown);
    let (cpu_px, cpu_hist) = (d.processed.clone(), d.after_histogram.clone());
    d.gpu = gpu;
    assert_eq!(gpu_px.len(), cpu_px.len());
    let max = gpu_px.iter().zip(&cpu_px).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0f32, f32::max);
    assert!(max < 1e-3, "GPU and CPU previews differ by {max}");
    assert_eq!(gpu_hist.samples, cpu_hist.samples);
    let moved: u64 = (0..3).map(|c| gpu_hist.channels[c].iter().zip(&cpu_hist.channels[c]).map(|(a, b)| a.abs_diff(*b)).sum::<u64>()).sum();
    assert!(moved <= 8, "histograms differ by {moved} counts");
    // A drag step: shown on the GPU, not read back, a settled frame to follow.
    d.params.exposure = 0.9;
    d.render_as(&ctx, true);
    assert!(d.gpu_shown && d.draft_shown);
    assert_eq!(d.processed, cpu_px, "a drag step keeps the last settled pixels");
    // Drawing the dialog uses the GPU texture. `PHOTOCRAFT_CAMERA_RAW_GPU_SNAPSHOT=<dir>` saves
    // the drawn window there for a look.
    harness.run_steps(3);
    if let Some(dir) = std::env::var_os("PHOTOCRAFT_CAMERA_RAW_GPU_SNAPSHOT").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        harness.render().unwrap().save(dir.join("camera-raw-gpu-dialog.png")).unwrap();
    }
}
