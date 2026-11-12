use std::sync::Arc;

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Effect, Layer, LayerMask, Size};
use photocraft_geom::Rect;
use photocraft_raster::Surface;

const W: i32 = 40;
const H: i32 = 20;

fn mask(r: Rect, density: f32) -> LayerMask {
    let mut m = LayerMask { surface: Surface::with_default(PixelFormat::GRAY8, &[0.0]), density, ..LayerMask::reveal_all() };
    m.surface.fill_rect(r, &[1.0]);
    m
}

/// Base; "Two" clipped to it with a first mask (top half) and a second mask (left half, 75 %);
/// "Above" clipped over it; "Shadows" with two drop shadows, the second masked (right half);
/// "Locked" with Lock Selection and an XMP packet of its own.
fn doc() -> Document {
    let mut d = Document::new("Round trip", Size::new(W as u32, H as u32), ColorMode::Rgb, SampleType::U8);
    let fmt = d.pixel_format();
    let filled = |name: &str| {
        let mut l = Layer::raster(name, fmt);
        if let Some(s) = l.surface_mut() {
            s.fill_rect(Rect::new(0, 0, W, H), &[1.0, 0.0, 0.0, 1.0]);
        }
        l
    };
    d.layers.push(filled("Base"));
    let mut two = filled("Two");
    two.clipped = true;
    two.mask = Some(mask(Rect::new(0, 0, W, H / 2), 1.0));
    two.second_mask = Some(mask(Rect::new(0, 0, W / 2, H), 0.75));
    d.layers.push(two);
    let mut above = filled("Above");
    above.clipped = true;
    d.layers.push(above);
    let mut shadows = filled("Shadows");
    shadows.effects.items.push(Effect::default_drop_shadow());
    let mut masked = Effect::default_drop_shadow();
    *masked.mask_mut() = Some(mask(Rect::new(W / 2, 0, W, H), 1.0));
    shadows.effects.items.push(masked);
    d.layers.push(shadows);
    let mut locked = filled("Locked");
    locked.locks.selection = true;
    locked.xmp = Some(Arc::from(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" dc:format=\"mine\"/></rdf:RDF></x:xmpmeta>",
    ));
    d.layers.push(locked);
    d
}

fn bytes(d: &Document) -> Vec<u8> {
    crate::psd_export::document_to_psd(d).unwrap().to_bytes().unwrap()
}

/// What a save in Photoshop does to PhotoCraft's own data: private blocks go, and (when
/// `locks`) the spare lock bit too.
fn through_photoshop(data: &[u8], locks: bool, edit: impl Fn(&mut photocraft_psd::layer::LayerRecord)) -> Vec<u8> {
    let mut f = photocraft_psd::PsdFile::from_bytes(data).unwrap();
    for r in &mut f.layer_info.as_mut().unwrap().layers {
        r.blocks.retain(|b| !b.key.starts_with(b"PC"));
        if locks && let Some(b) = r.blocks.iter_mut().find(|b| &b.key == b"lspf") {
            b.data[0] &= !1;
        }
        edit(r);
    }
    f.to_bytes().unwrap()
}

fn open(data: &[u8]) -> (Document, Vec<String>) {
    let r = crate::import("round.psd", data).unwrap();
    (r.document, r.warnings)
}

fn by_name<'a>(d: &'a Document, name: &str) -> &'a Layer {
    d.layers.iter().find(|l| l.name == name).unwrap()
}

fn check_restored(d: &Document, original: &Document) {
    let names: Vec<&str> = d.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Base", "Two", "Above", "Shadows", "Locked"], "the helpers are gone");
    let (a, b) = (by_name(original, "Two"), by_name(d, "Two"));
    assert!(b.clipped && by_name(d, "Above").clipped, "the clipping chain is intact");
    let (m1, m2) = (b.mask.as_ref().unwrap(), b.second_mask.as_ref().expect("the second mask came back"));
    for (x, y) in [(5, 5), (30, 5), (5, 15), (30, 15)] {
        assert!((m1.value(x, y) - a.mask.as_ref().unwrap().value(x, y)).abs() < 0.02, "first at {x},{y}");
        assert!((m2.value(x, y) - a.second_mask.as_ref().unwrap().value(x, y)).abs() < 0.02, "second at {x},{y}");
    }
    assert!((m2.density - 0.75).abs() < 0.01);
    let fx = &by_name(d, "Shadows").effects.items;
    assert_eq!(fx.len(), 2);
    assert!(fx[0].mask().is_none(), "the first shadow has no mask");
    let m = fx[1].mask().expect("the second shadow's mask came back");
    assert!(m.value(30, 10) > 0.9 && m.value(5, 10) < 0.1);
    let locked = by_name(d, "Locked");
    assert!(locked.locks.selection, "Lock Selection");
    let x = locked.xmp.as_deref().unwrap();
    assert!(x.contains("dc:format=\"mine\"") && !x.contains(super::NS), "the layer's own XMP, without PhotoCraft's: {x}");
}

#[test]
fn a_photocraft_psd_brings_everything_back() {
    let d = doc();
    let (back, _) = open(&bytes(&d));
    check_restored(&back, &d);
}

/// A save in Photoshop drops the private blocks and (when the locks were changed there) the
/// spare lock bit: the helpers and the XMP bring the masks and the lock back all the same.
#[test]
fn a_psd_saved_again_by_photoshop_brings_everything_back() {
    let d = doc();
    let (back, warnings) = open(&through_photoshop(&bytes(&d), true, |_| {}));
    check_restored(&back, &d);
    assert!(warnings.iter().all(|w| !w.contains("changed outside")), "{warnings:?}");
}

/// Photoshop sees the layer as PhotoCraft draws it: its layer mask is the two multiplied, and
/// the helpers are hidden and clipped, so they draw nothing and keep the chain.
#[test]
fn photoshop_sees_the_masks_multiplied_and_hidden_clipped_helpers() {
    let f = photocraft_psd::PsdFile::from_bytes(&bytes(&doc())).unwrap();
    let layers = &f.layer_info.as_ref().unwrap().layers;
    let names: Vec<String> = layers.iter().map(|r| r.name()).collect();
    let two = names.iter().position(|n| n == "Two").unwrap();
    assert!(names[two + 1].contains("[PhotoCraft second mask]"), "{names:?}");
    assert_eq!(names[two + 2], "Above");
    for r in layers.iter().filter(|r| r.name().contains("[PhotoCraft")) {
        assert!(!r.is_visible() && r.clipping != 0, "{}: hidden and clipped", r.name());
    }
    assert!(names.iter().any(|n| n.contains("[PhotoCraft Drop Shadow mask]")), "{names:?}");
    let (d, _) = open(&through_photoshop(&bytes(&doc()), false, |r| r.blocks.retain(|b| &b.key != b"shmd")));
    // Without PhotoCraft's XMP (another program's save), the helpers stay as ordinary layers and
    // the layer keeps the product.
    let two = by_name(&d, "Two");
    assert!(two.second_mask.is_none());
    assert!((two.mask.as_ref().unwrap().value(5, 5) - 1.0).abs() < 0.02 && two.mask.as_ref().unwrap().value(30, 5) < 0.3);
}

/// The layer's mask edited in Photoshop: that edit is what the layer keeps, alone, with a warning,
/// rather than the masks from before it.
#[test]
fn a_mask_edited_in_photoshop_is_kept() {
    let d = doc();
    let data = bytes(&d);
    let mut f = photocraft_psd::PsdFile::from_bytes(&through_photoshop(&data, false, |_| {})).unwrap();
    let two = f.layer_info.as_mut().unwrap().layers.iter_mut().find(|r| r.name() == "Two").unwrap();
    let photocraft_psd::layer::MaskData::Mask(m) = &mut two.mask else { panic!("a mask") };
    m.default_color = 255 - m.default_color;
    let (back, warnings) = open(&f.to_bytes().unwrap());
    let two = by_name(&back, "Two");
    assert!(two.second_mask.is_none(), "no second mask over the edit");
    assert!(warnings.iter().any(|w| w.contains("changed outside")), "{warnings:?}");
    assert!(back.layers.iter().all(|l| !l.name.contains("[PhotoCraft")), "the helpers still go");
}
