//! Chromatic Aberration in the compositor: tiles, sub-rectangles and reduced copies agree, masks
//! and clipping work as for any adjustment layer, at every depth.

use super::*;
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::adjust::ChromaticAberration;
use photocraft_doc::{Adjustment, Document, Layer, LayerContent, LayerMask};
use photocraft_geom::Size;

fn ca(c: ChromaticAberration) -> Layer {
    Layer::new("Chromatic Aberration 1", LayerContent::Adjustment(Adjustment::ChromaticAberration(c)))
}

fn strong() -> ChromaticAberration {
    ChromaticAberration { red_shift: 6.0, blue_shift: -4.5, offset_x: 1.5, offset_y: -2.25, softness: 1.5, ..Default::default() }
}

/// A busy picture: diagonal stripes of colour over white, part transparent at the right edge.
fn picture(depth: SampleType, size: Size) -> Document {
    let mut d = Document::new("ca", size, ColorMode::Rgb, depth);
    let fmt = d.pixel_format();
    let mut bg = Layer::raster("paper", fmt);
    bg.surface_mut().unwrap().fill_rect(Rect::new(0, 0, size.width as i32 - 6, size.height as i32), &[1.0, 1.0, 1.0, 1.0]);
    d.layers.push(bg);
    let mut ink = Layer::raster("ink", fmt);
    let s = ink.surface_mut().unwrap();
    for y in 0..size.height as i32 {
        for x in 0..size.width as i32 {
            if (x + 2 * y) % 9 < 3 {
                s.write_pixel(x, y, &[((x * 13) % 255) as f32 / 255.0, ((y * 7) % 255) as f32 / 255.0, 0.2, 1.0]);
            }
        }
    }
    d.layers.push(ink);
    d
}

fn crop(b: &Buffer, r: Rect) -> Vec<[f32; 4]> {
    let mut out = Vec::new();
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            out.push(b.get(x, y));
        }
    }
    out
}

/// Tiles of any size, and any rectangle rendered on its own, match the whole composite: every
/// tile reads the pixels around it (its apron). At 8, 16 and 32 bits.
#[test]
fn tiles_and_sub_rectangles_agree() {
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let mut d = picture(depth, Size::new(70, 52));
        d.layers.push(ca(strong()));
        let whole = render_tiled(&d, d.bounds(), 10_000);
        for tile in [7, 16, 64] {
            assert_eq!(render_tiled(&d, d.bounds(), tile).px, whole.px, "{depth:?}, tile {tile}");
        }
        for r in [Rect::new(10, 9, 30, 21), Rect::new(0, 0, 5, 5), Rect::new(60, 40, 70, 52), Rect::new(33, 17, 34, 18)] {
            assert_eq!(render(&d, r).px, crop(&whole, r), "{depth:?}, {r:?}");
        }
        // It changed the picture (red and blue moved), and only red and blue.
        let mut plain = d.clone();
        plain.layers.pop();
        let before = render(&plain, plain.bounds());
        assert_ne!(before.px, whole.px, "{depth:?}");
        // (An integer document rounds the backdrop to its adjustment levels after an adjustment
        // layer: `composite::adjustment_quantum`.)
        let q = composite::adjustment_quantum(depth).map_or(0.0, |q| 0.5 / q) + 1e-6;
        assert!(before.px.iter().zip(&whole.px).all(|(a, b)| (a[1] - b[1]).abs() <= q && a[3] == b[3]), "{depth:?}: green and alpha stay");
    }
}

/// Two layers, one inside a group (isolated and pass-through): the aprons add up, so tiles still
/// agree; a hidden one asks for none.
#[test]
fn nested_and_stacked_layers_agree_across_tiles() {
    for blend in [BlendMode::Normal, BlendMode::PassThrough] {
        let mut d = picture(SampleType::U16, Size::new(64, 40));
        let ink = d.layers.pop().unwrap();
        let mut g = Layer::group("group", vec![ink, ca(ChromaticAberration { offset_x: 3.0, ..strong() })]);
        g.blend = blend;
        d.layers.push(g);
        d.layers.push(ca(ChromaticAberration { red_shift: -5.0, blue_shift: 5.0, center: [0.2, 0.7], falloff: 0.3, ..Default::default() }));
        let reach = crate::adjust::chromatic::stack_reach(&d.layers);
        assert!(reach > 12, "{reach}");
        let whole = render_tiled(&d, d.bounds(), 10_000);
        for tile in [5, 16, 33] {
            assert_eq!(render_tiled(&d, d.bounds(), tile).px, whole.px, "{blend:?}, tile {tile}");
        }
        d.layers.last_mut().unwrap().visible = false;
        assert!(crate::adjust::chromatic::stack_reach(&d.layers) < reach);
    }
}

/// A layer mask limits it like any adjustment, opacity fades it, and clipped it changes only its
/// base.
#[test]
fn mask_opacity_and_clipping() {
    let mut d = picture(SampleType::U8, Size::new(48, 24));
    let plain = render(&d, d.bounds());
    let mut layer = ca(ChromaticAberration { offset_x: 2.0, ..strong() });
    let mut mask = photocraft_raster::Surface::new(PixelFormat::GRAY8);
    mask.fill_rect(Rect::new(24, 0, 48, 24), &[1.0]);
    layer.mask = Some(LayerMask { surface: mask, enabled: true, linked: true, density: 1.0, feather: 0.0 });
    d.layers.push(layer);
    let out = render(&d, d.bounds());
    let left = Rect::new(0, 0, 24, 24);
    assert_eq!(crop(&out, left), crop(&plain, left), "the mask hides it on the left");
    assert_ne!(crop(&out, Rect::new(24, 0, 48, 24)), crop(&plain, Rect::new(24, 0, 48, 24)));

    // Half opacity lands halfway (within rounding) between the two.
    d.layers.last_mut().unwrap().mask = None;
    let full = render(&d, d.bounds());
    d.layers.last_mut().unwrap().opacity = 0.5;
    let half = render(&d, d.bounds());
    for ((h, f), p) in half.px.iter().zip(&full.px).zip(&plain.px) {
        for c in 0..3 {
            assert!((h[c] - (f[c] + p[c]) / 2.0).abs() <= 1.0 / 255.0 + 1e-6, "{h:?} {f:?} {p:?}");
        }
    }

    // Clipped to the ink, the paper beside the stripes is left as it is.
    let mut d = picture(SampleType::U8, Size::new(48, 24));
    let mut layer = ca(ChromaticAberration { offset_x: 2.0, ..strong() });
    layer.clipped = true;
    d.layers.push(layer);
    let out = render(&d, d.bounds());
    for (i, (o, p)) in out.px.iter().zip(&plain.px).enumerate() {
        let (x, y) = ((i % 48) as i32, (i / 48) as i32);
        if (x + 2 * y) % 9 >= 3 {
            assert_eq!(o, p, "paper at ({x}, {y})");
        }
    }
    assert_ne!(out.px, plain.px);
}

/// A grayscale document has no colour to fringe: the layer changes nothing.
#[test]
fn grayscale_documents_are_left_alone() {
    let mut d = Document::new("gray", Size::new(20, 10), ColorMode::Grayscale, SampleType::U8);
    let mut l = Layer::raster("ink", d.pixel_format());
    l.surface_mut().unwrap().fill_rect(Rect::new(5, 0, 8, 10), &[0.3, 1.0]);
    d.layers.push(l);
    let plain = render(&d, d.bounds());
    d.layers.push(ca(strong()));
    assert_eq!(render(&d, d.bounds()).px, plain.px);
}

/// A reduced copy of the document shrinks the shifts with it.
#[test]
fn proxies_scale_the_shifts() {
    let mut d = picture(SampleType::U8, Size::new(64, 64));
    d.layers.push(ca(strong()));
    let p = proxy::proxy_document(&d, 4);
    let Some(LayerContent::Adjustment(Adjustment::ChromaticAberration(c))) = p.layers.last().map(|l| &l.content) else { panic!() };
    assert_eq!((c.red_shift, c.offset_x, c.softness), (1.5, 0.375, 0.375));
    assert!(render(&p, p.bounds()).px.iter().flatten().all(|v| v.is_finite()));
    // A single layer rendered on its own (a group's thumbnail) agrees across rectangles too.
    let g = Layer::group("group", d.layers.clone());
    let whole = render_layer_in(&d, &g, d.bounds());
    let r = Rect::new(20, 30, 40, 41);
    assert_eq!(render_layer_in(&d, &g, r).px, crop(&whole, r));
}
