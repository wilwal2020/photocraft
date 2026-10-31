use super::*;

fn xform(ppp: f32, physical_size: [f32; 2]) -> ViewXform {
    ViewXform {
        rect: Rect::from_min_size(pos2(40.0 / ppp, 100.0 / ppp), vec2(physical_size[0] / ppp, physical_size[1] / ppp)),
        zoom: 1.0 / ppp,
        center: [1500.0, 1000.0],
        flip: false,
        rotation: 0.0,
        aspect: 1.0,
    }
}

fn assert_pixel_edges(xf: ViewXform, ppp: f32) {
    for [x, y] in [[0.0, 0.0], [1.0, 1.0], [17.0, 29.0], [1500.0, 1000.0]] {
        let p = xf.to_screen(x, y) * ppp;
        assert!((p.x - p.x.round()).abs() < 0.001, "x edge: {p:?}, ppp {ppp}");
        assert!((p.y - p.y.round()).abs() < 0.001, "y edge: {p:?}, ppp {ppp}");
    }
}

#[test]
fn actual_pixels_align_texel_edges_in_odd_viewports_at_every_display_scale() {
    for ppp in [1.0, 1.25, 1.5, 2.0] {
        for size in [[523.0, 470.0], [524.0, 471.0], [523.0, 471.0]] {
            let before = xform(ppp, size);
            let aligned = before.pixel_aligned(ppp);
            assert_pixel_edges(aligned, ppp);
            let delta = (aligned.to_screen(0.0, 0.0) - before.to_screen(0.0, 0.0)) * ppp;
            assert!(delta.x.abs() <= 0.501 && delta.y.abs() <= 0.501);
            let again = aligned.pixel_aligned(ppp);
            assert!((again.to_screen(0.0, 0.0) - aligned.to_screen(0.0, 0.0)).length() < 0.001);
            let back = aligned.to_doc(aligned.to_screen(123.25, 45.75));
            assert!((back[0] - 123.25).abs() < 0.001 && (back[1] - 45.75).abs() < 0.001);
        }
    }
}

#[test]
fn integer_magnification_aligns_flipped_and_aspect_corrected_views() {
    for flip in [false, true] {
        let before = ViewXform {
            rect: Rect::from_min_size(pos2(-20.25, 30.25), vec2(523.0, 471.0)),
            zoom: 2.0,
            center: [1500.125, 999.75],
            flip,
            aspect: 1.5,
            ..xform(1.0, [523.0, 471.0])
        };
        assert_pixel_edges(before.pixel_aligned(1.0), 1.0);
    }
    let even = xform(1.0, [524.0, 470.0]);
    assert_eq!(even.pixel_aligned(1.0).center, even.center);
}

#[test]
fn fractional_zoom_rotation_and_invalid_scales_preserve_the_camera() {
    let original = xform(1.0, [523.0, 471.0]);
    for zoom in [0.25, 0.5, 0.75, 1.25, 1.5, 2.5, 0.0, -1.0, f32::NAN, f32::INFINITY] {
        let xf = ViewXform { zoom, ..original };
        assert_eq!(xf.pixel_aligned(1.0).center, xf.center);
    }
    for rotation in [0.2, 30.0, 90.0, f32::NAN] {
        let xf = ViewXform { rotation, ..original };
        assert_eq!(xf.pixel_aligned(1.0).center, xf.center);
    }
    for ppp in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(original.pixel_aligned(ppp).center, original.center);
    }
    let aspect = ViewXform { aspect: 1.2, ..original };
    assert_eq!(aspect.pixel_aligned(1.0).center, aspect.center);
}

#[test]
fn active_transform_uses_the_same_alignment_without_changing_saved_pan() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", serde_json::json!({"width": 3000, "height": 2000})).unwrap();
    app.sync_views();
    for ppp in [1.0, 1.25, 1.5, 2.0] {
        app.ppp = ppp;
        app.last_canvas_rect = xform(ppp, [523.0, 471.0]).rect;
        app.ui.views[0].center = [1500.0, 1000.0];
        app.ui.views[0].zoom = 1.0;
        let before = app.ui.views[0].center;
        let active = ViewXform::active(&app).unwrap();
        assert_pixel_edges(active, ppp);
        assert_eq!(app.ui.views[0].center, before);
    }
}

#[test]
fn cpu_canvas_paints_its_image_on_physical_pixel_edges() {
    for ppp in [1.0, 1.25, 2.0] {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", serde_json::json!({"width": 64, "height": 64, "background": "white"})).unwrap();
        app.sync_views();
        app.ppp = ppp;
        app.ui.views[0].zoom = 1.0;
        app.ui.views[0].center = [32.0, 32.0];
        app.ui.views[0].fit_pending = false;
        app.ui.views[0].doc_size = [64, 64];
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let tex = ensure_texture(&mut app, &ctx, 0, None).unwrap().0;
        let view = app.ui.views[0].clone();
        let saved_center = view.center;
        let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))), ..Default::default() }, |ui| {
            canvas_view(&mut app, ui, 0, xform(ppp, [523.0, 471.0]).rect, view.clone(), false);
        });
        out.textures_delta.clear();
        let mesh = out
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Mesh(mesh) if mesh.texture_id == tex => Some(mesh),
                _ => None,
            })
            .expect("CPU document image quad");
        assert_eq!(mesh.vertices.len(), 4);
        for vertex in &mesh.vertices {
            let p = vertex.pos * ppp;
            assert!((p.x - p.x.round()).abs() < 0.001 && (p.y - p.y.round()).abs() < 0.001, "ppp {ppp}: {p:?}");
        }
        assert_eq!(app.ui.views[0].center, saved_center);
    }
}

#[test]
fn crop_image_movement_keeps_its_continuous_camera_mapping() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.ui.tool = Tool::Crop;
    let xf = xform(1.0, [523.0, 471.0]);
    assert_eq!(xf.pixel_aligned_for(&app, 1.0).center, xf.center);
    app.ui.tool_options.crop_shield.classic_mode = true;
    assert_pixel_edges(xf.pixel_aligned_for(&app, 1.0), 1.0);
}

#[test]
fn animated_active_transform_aligns_the_display_camera_and_preserves_target_pan() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", serde_json::json!({"width": 3000, "height": 2000})).unwrap();
    app.run("prefs.set", serde_json::json!({"values": {"general.animatedZoom": true}})).unwrap();
    app.sync_views();
    for ppp in [1.0, 1.25, 1.5, 2.0] {
        app.ppp = ppp;
        app.last_canvas_rect = xform(ppp, [523.0, 471.0]).rect;
        let v = &mut app.ui.views[0];
        v.zoom = 3.0;
        v.center = [1500.0, 1000.0];
        v.zoom_animating = true;
        v.display_camera = Some((1.0, [1490.25, 990.75]));
        let active = ViewXform::active(&app).unwrap();
        assert_eq!(active.zoom, 1.0 / ppp);
        assert_pixel_edges(active, ppp);
        assert!((active.center[0] - 1490.25).abs() <= 0.501);
        assert!((active.center[1] - 990.75).abs() <= 0.501);
        assert_eq!(app.ui.views[0].center, [1500.0, 1000.0]);
        assert_eq!(app.ui.views[0].display_camera, Some((1.0, [1490.25, 990.75])));
    }
}

/// User report: zooming in the Crop tool's default mode flickered. Its image is painted after the
/// frame's input (the tool moves the camera), and a wheel notch drew it at the new centre but the
/// old zoom for one frame. Zooming about the pointer keeps the document point under it in every
/// frame, the one with the notch included.
#[test]
fn zooming_in_crop_mode_keeps_the_point_under_the_pointer_in_every_frame() {
    for animated in [true, false] {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", serde_json::json!({"width": 200, "height": 100, "background": "white"})).unwrap();
        app.sync_views();
        app.ui.tool = Tool::Crop;
        app.ui.extras.rulers = false;
        app.run("prefs.set", serde_json::json!({"path": "general.animatedZoom", "value": animated})).unwrap();
        app.ui.views[0].zoom = 1.0;
        app.ui.views[0].center = [100.0, 50.0];
        app.ui.views[0].fit_pending = false;
        app.ui.views[0].doc_size = [200, 100];
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let pointer = pos2(350.0, 280.0);
        let mut doc_under = Vec::new();
        for frame in 0..30 {
            let tex = ensure_texture(&mut app, &ctx, 0, None).unwrap().0;
            let alt = if frame == 5 || frame == 6 { egui::Modifiers::ALT } else { egui::Modifiers::NONE };
            let mut events = vec![egui::Event::ModifiersChanged(alt), egui::Event::PointerMoved(pointer)];
            if frame == 5 || frame == 6 {
                events.push(egui::Event::MouseWheel {
                    source: egui::MouseWheelSource::Unknown,
                    unit: egui::MouseWheelUnit::Line,
                    delta: vec2(0.0, 1.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::ALT,
                });
            }
            let input = egui::RawInput { screen_rect: Some(rect), time: Some(f64::from(frame) / 60.0), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                let view = app.ui.views[0].clone();
                let v = canvas_view(&mut app, ui, 0, rect, view, true);
                app.ui.views[0] = v;
            });
            out.textures_delta.clear();
            let Some(mesh) = out.shapes.iter().find_map(|s| match &s.shape {
                egui::Shape::Mesh(m) if m.texture_id == tex && m.vertices.len() == 4 => Some(m.clone()),
                _ => None,
            }) else {
                continue;
            };
            let (lo, hi) = mesh.vertices.iter().fold((pos2(f32::MAX, f32::MAX), pos2(f32::MIN, f32::MIN)), |(lo, hi), v| (lo.min(v.pos), hi.max(v.pos)));
            let (ulo, uhi) = mesh.vertices.iter().fold((pos2(f32::MAX, f32::MAX), pos2(f32::MIN, f32::MIN)), |(lo, hi), v| (lo.min(v.uv), hi.max(v.uv)));
            let at = |p: f32, lo: f32, hi: f32, ulo: f32, uhi: f32, size: f32| (ulo + (p - lo) / (hi - lo) * (uhi - ulo)) * size;
            let x = at(pointer.x, lo.x, hi.x, ulo.x, uhi.x, 200.0);
            let y = at(pointer.y, lo.y, hi.y, ulo.y, uhi.y, 100.0);
            let zoom = (hi.x - lo.x) / ((uhi.x - ulo.x) * 200.0);
            doc_under.push((frame, x, y, zoom));
        }
        let zooms: Vec<f32> = doc_under.iter().map(|d| d.3).collect();
        assert!(zooms.last().unwrap() > &1.15, "the notches zoomed in: {zooms:?}");
        let (_, x0, y0, _) = doc_under[0];
        for &(frame, x, y, zoom) in &doc_under {
            // Within a screen pixel (pixel alignment) at that frame's zoom.
            let tol = 1.0 / zoom + 0.01;
            assert!((x - x0).abs() <= tol && (y - y0).abs() <= tol, "animated {animated}, frame {frame}: ({x}, {y}) vs ({x0}, {y0}), zoom {zoom}");
        }
    }
}
