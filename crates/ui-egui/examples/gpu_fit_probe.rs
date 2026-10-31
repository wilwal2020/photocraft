//! Which render path each file takes when opened in order, as in the desktop app: the GPU canvas
//! warmed up, then each file opened and shown for a few frames. Prints the layer fit
//! (`perf.gpu_layer_fit`: working set against the layer budget), the last refresh kind and the
//! document's CPU reason, if any.
//!
//! ```sh
//! cargo run --release -p photocraft-ui-egui --example gpu_fit_probe -- [--prefs preferences.json] a.psd b.psd …
//! ```

use egui_kittest::Harness;
use photocraft_ui_egui::{PhotocraftApp, Services};

fn mb(b: u64) -> f64 {
    b as f64 / f64::from(1u32 << 20)
}

/// Per surface: the compositor's estimate (its tile bounds' area) against what its stored tiles
/// cover per 2048² page cell, and how many stored tiles are just the surface's default.
fn layers(doc: &photocraft_doc::Document) {
    const PAGE: i32 = 2048;
    let grid = doc.bounds();
    let (mut est_all, mut cell_all, mut nondefault_all) = (0u64, 0u64, 0u64);
    let mut rows = Vec::new();
    for (_, _, l) in doc.walk() {
        let mut surfaces: Vec<(&str, &photocraft_raster::Surface, u64)> = Vec::new();
        // The compositor's texture kinds (`TexKind::for_surface`): RGBA8 or RGBA16F pixels, R8,
        // R16F or R32F masks.
        use photocraft_color::SampleType;
        if let Some(s) = l.surface() {
            surfaces.push(("pixels", s, if s.format().sample == SampleType::U8 { 4 } else { 8 }));
        }
        if let Some(c) = &l.fill_cache {
            let kind = match &c.fill {
                photocraft_doc::Fill::Solid(_) => "fill-cache solid",
                photocraft_doc::Fill::Gradient { .. } => "fill-cache gradient",
                _ => "fill-cache pattern",
            };
            let current = matches!(&l.content, photocraft_doc::LayerContent::Fill(f) if *f == c.fill);
            if current {
                surfaces.push((kind, &c.surface, if c.surface.format().sample == SampleType::U8 { 4 } else { 8 }));
            }
        }
        if let Some(m) = &l.mask {
            let f = m.surface.format();
            surfaces.push((
                "mask",
                &m.surface,
                if f == photocraft_color::PixelFormat::GRAY8 {
                    1
                } else if f.sample == SampleType::F32 {
                    4
                } else {
                    2
                },
            ));
        }
        for (role, s, bpp) in surfaces {
            let b = s.tile_bounds().intersect(&grid);
            let est = b.width() as u64 * b.height() as u64 * bpp;
            let mut cells: std::collections::HashMap<(i32, i32), photocraft_geom::Rect> = Default::default();
            let mut nondefault: std::collections::HashMap<(i32, i32), photocraft_geom::Rect> = Default::default();
            let default = s.default_bytes();
            for (c, t) in s.tiles() {
                let r = c.rect().intersect(&grid);
                if r.is_empty() {
                    continue;
                }
                let key = (r.x0.div_euclid(PAGE), r.y0.div_euclid(PAGE));
                let e = cells.entry(key).or_insert(r);
                *e = e.union(&r);
                let uniform = t.bytes().chunks(default.len().max(1)).all(|px| px == default);
                if !uniform {
                    let e = nondefault.entry(key).or_insert(r);
                    *e = e.union(&r);
                }
            }
            let area =
                |m: &std::collections::HashMap<(i32, i32), photocraft_geom::Rect>| m.values().map(|r| r.width() as u64 * r.height() as u64 * bpp).sum::<u64>();
            let (cell, nd) = (area(&cells), area(&nondefault));
            est_all += est;
            cell_all += cell;
            nondefault_all += nd;
            rows.push((
                est,
                format!(
                    "{:<28} {role:<19} est {:>7.1} MB  cells {:>7.1} MB  non-default {:>7.1} MB  ({} tiles)",
                    l.name.chars().take(28).collect::<String>(),
                    mb(est),
                    mb(cell),
                    mb(nd),
                    s.tile_count()
                ),
            ));
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    for (_, r) in rows.iter().take(25) {
        println!("  {r}");
    }
    println!("  total: est {:.0} MB, per-cell tile bounds {:.0} MB, non-default tiles only {:.0} MB", mb(est_all), mb(cell_all), mb(nondefault_all));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--layers") {
        for path in &args[1..] {
            let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
            let doc = photocraft_io::import(path, &bytes).unwrap_or_else(|e| panic!("{path}: {e}")).document;
            println!("{path}: {:?} {:?}", doc.mode, doc.depth);
            layers(&doc);
        }
        return;
    }
    let prefs =
        args.iter().position(|a| a == "--prefs").and_then(|i| args.get(i + 1)).map(|p| std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}")));
    let files: Vec<&String> = args.iter().enumerate().filter(|(i, a)| *a != "--prefs" && (*i == 0 || args[i - 1] != "--prefs")).map(|(_, a)| a).collect();
    let mut h: Harness<'static, PhotocraftApp> = Harness::builder()
        .with_size(egui::vec2(1920.0, 1080.0))
        .with_pixels_per_point(1.0)
        .wgpu_setup(photocraft_ui_egui::gpu_canvas::wgpu_setup())
        .build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut session = photocraft_engine::Session::new();
            if let Some(text) = prefs.as_deref() {
                let _ = session.load_prefs_json(text);
            }
            let mut app = PhotocraftApp::new(session, Services::default());
            if let Some(rs) = cc.wgpu_render_state.as_ref() {
                app.set_wgpu(rs.clone());
                app.warm_up_gpu();
            }
            app
        });
    for _ in 0..3 {
        h.step();
    }
    std::thread::sleep(std::time::Duration::from_secs(3));
    for path in files {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let doc = photocraft_io::import(path, &bytes).unwrap_or_else(|e| panic!("{path}: {e}")).document;
        let (w, hh, n) = (doc.size.width, doc.size.height, doc.walk().len());
        let id = doc.id.0;
        h.state_mut().session.open_document(doc, Some(path.clone()));
        h.state_mut().sync_views();
        for _ in 0..30 {
            h.step();
        }
        let app = h.state();
        let fit =
            app.perf.gpu_layer_fit.map_or_else(|| "none".to_string(), |f| format!("doc {} needs {:.0} MB of {:.0} MB", f.doc, mb(f.needed), mb(f.budget)));
        println!("{path}: {w}x{hh}, {n} layers (doc {id}); fit {fit}; last refresh {}; reason {:?}", app.perf.last_refresh, app.render_path.current(id));
    }
}
