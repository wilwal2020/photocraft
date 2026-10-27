//! Effect Noise (drop and inner shadows, outer and inner glows) on the GPU: the same
//! document-coordinate speckle as the CPU maps (`compose::effects::noise`). It used to send the
//! whole canvas to the CPU compositor (user report: a 7 s frame on a 2106 x 3000 page).

use photocraft_color::{BlendMode, Color, ColorMode, SampleType};
use photocraft_doc::{Contour, Document, Effect, FxCommon, FxPaint, Glow, GlowSource, GlowTechnique, Layer, Size};
use photocraft_geom::Rect;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue, photocraft_gpu::Compositor)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    // Exact comparison needs 32-bit float targets (see parity.rs); skip on adapters without them.
    if photocraft_gpu::Compositor::preferred_acc_format(&adapter) != wgpu::TextureFormat::Rgba32Float {
        return None;
    }
    let comp = photocraft_gpu::Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).ok()?;
    Some((device, queue, comp))
}

fn check(g: &mut (wgpu::Device, wgpu::Queue, photocraft_gpu::Compositor), d: &Document, what: &str) {
    let cpu = photocraft_compose::flatten(d);
    let out = photocraft_gpu::render_to_vec(&mut g.2, &g.0, &g.1, d, d.bounds()).unwrap_or_else(|e| panic!("{what}: {e}"));
    let worst = cpu
        .px
        .iter()
        .zip(&out)
        .map(|(c, o)| (0..4).map(|k| if k == 3 { (c[3] - o[3]).abs() } else { (c[k] * c[3] - o[k] * o[3]).abs() }).fold(0.0f32, f32::max))
        .fold(0.0f32, f32::max);
    assert!(worst <= 1.0 / 255.0 + 1e-6, "{what}: max diff {:.2}/255", worst * 255.0);
}

fn glow(technique: GlowTechnique, source: GlowSource, noise: f32) -> Glow {
    Glow {
        common: FxCommon::new(BlendMode::Screen, 0.8),
        paint: FxPaint::Color(Color::rgb(1.0, 0.9, 0.2)),
        technique,
        spread: 0.1,
        size: 7.0,
        contour: Contour::Linear,
        anti_alias: false,
        range: 0.5,
        jitter: 0.0,
        noise,
        source,
    }
}

#[test]
fn effect_noise_matches_the_cpu() {
    let _lock = crate::GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut g) = gpu() else { return };
    let shadow = |noise: f32, inner: bool| match Effect::default_drop_shadow() {
        Effect::DropShadow(mut s) => {
            s.noise = noise;
            s.size = 9.0;
            if inner { Effect::InnerShadow(s) } else { Effect::DropShadow(s) }
        }
        e => e,
    };
    let sets: Vec<(&str, Vec<Effect>)> = vec![
        ("drop shadow", vec![shadow(0.5, false)]),
        ("inner shadow", vec![shadow(0.3, true)]),
        ("outer glow", vec![Effect::OuterGlow(glow(GlowTechnique::Softer, GlowSource::Edge, 0.4))]),
        ("inner glow, precise, centre", vec![Effect::InnerGlow(glow(GlowTechnique::Precise, GlowSource::Center, 0.6))]),
        ("all", vec![shadow(1.0, false), Effect::InnerGlow(glow(GlowTechnique::Softer, GlowSource::Edge, 0.25))]),
    ];
    for depth in [SampleType::U8, SampleType::U16] {
        for (what, effects) in &sets {
            let mut d = Document::with_background("n", Size::new(72, 56), ColorMode::Rgb, depth, Color::WHITE);
            let fmt = d.pixel_format();
            let mut l = Layer::raster("styled", fmt);
            l.surface_mut().unwrap().fill_rect(Rect::new(14, 12, 52, 40), &photocraft_raster::from_rgba(&fmt, [0.2, 0.5, 0.8, 1.0]));
            l.effects.items = effects.clone();
            d.layers.push(l);
            check(&mut g, &d, &format!("{depth:?} {what}"));
        }
    }
}
