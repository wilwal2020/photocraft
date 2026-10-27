//! Layer effects on a layer with a second pixel mask (`Layer::second_mask`) on the GPU: the effect
//! shape comes through both masks, as the CPU compositor makes it, and follows edits to either.
//! They used to send the whole canvas to the CPU compositor (user report: lag spikes painting on
//! such a page, ~300 ms a frame at 2106 x 3000).

use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Effect, Layer, LayerMask, Size};
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

fn styled(fmt: PixelFormat) -> Layer {
    let mut l = Layer::raster("styled", fmt);
    l.surface_mut().unwrap().fill_rect(Rect::new(10, 8, 54, 40), &photocraft_raster::from_rgba(&fmt, [0.2, 0.7, 0.3, 1.0]));
    let mut first = LayerMask::reveal_all();
    first.surface.fill_rect(Rect::new(40, 0, 64, 48), &[0.0]);
    l.mask = Some(first);
    let mut second = LayerMask::reveal_all();
    second.surface.fill_rect(Rect::new(0, 0, 22, 48), &[0.4]);
    l.second_mask = Some(second);
    l.effects.items = vec![Effect::default_drop_shadow()];
    l
}

#[test]
fn effects_through_a_second_mask_match_the_cpu_and_follow_its_edits() {
    let _lock = crate::GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut g) = gpu() else { return };
    for depth in [SampleType::U8, SampleType::U16] {
        let mut d = Document::with_background("m", Size::new(64, 48), ColorMode::Rgb, depth, Color::WHITE);
        let fmt = d.pixel_format();
        d.layers.push(styled(fmt));
        check(&mut g, &d, &format!("{depth:?}"));
        // Painting the second mask (the same compositor, its maps cached) reshapes the shadow.
        let l = d.layers.last_mut().unwrap();
        l.second_mask.as_mut().unwrap().surface.fill_rect(Rect::new(24, 20, 36, 30), &[0.0]);
        check(&mut g, &d, &format!("{depth:?} second mask painted"));
        // Turned off, it no longer shapes them.
        d.layers.last_mut().unwrap().second_mask.as_mut().unwrap().enabled = false;
        check(&mut g, &d, &format!("{depth:?} second mask off"));
        d.layers.last_mut().unwrap().second_mask.as_mut().unwrap().enabled = true;
        check(&mut g, &d, &format!("{depth:?} second mask on again"));
    }
}
