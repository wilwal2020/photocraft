//! Which ways of carrying PhotoCraft-only settings survive a save in Photoshop.
//!
//! `write <probe.psd>` writes a document with one layer per carrier: a spare bit of the layer's
//! lock flags, a tag in the layer name, the layer's XMP metadata, a custom `shmd` item, a private
//! tagged block, the document's XMP, and a hidden clipped helper layer holding a clipped layer's
//! two masks (its pixels the second mask, its layer mask the first) between that layer and a layer
//! clipped above it.
//!
//! Open it in Photoshop, save it under another name, then
//! `check <probe.psd> <saved-by-photoshop.psd>` reports what each file holds, side by side.
//!
//! `cargo run --release -p photocraft-io --example psd_roundtrip_probe -- write probe.psd`

use std::sync::Arc;

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerMask, Size};
use photocraft_geom::Rect;
use photocraft_psd::layer::LayerRecord;
use photocraft_raster::Surface;

const W: u32 = 200;
const H: u32 = 120;
const NS: &str = "https://photocraft.app/ns/roundtrip/1.0/";
const TAG: &str = "[PhotoCraft: selection lock]";
const HELPER: &str = "Two masks [PhotoCraft masks]";

fn xmp(what: &str) -> String {
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:photocraft=\"{NS}\" photocraft:probe=\"{what}\" photocraft:selectionLock=\"True\"/></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>"
    )
}

fn filled(name: &str, fmt: PixelFormat, r: Rect, c: [f32; 4]) -> Layer {
    let mut l = Layer::raster(name, fmt);
    if let Some(s) = l.surface_mut() {
        s.fill_rect(r, &c);
    }
    l
}

/// The first mask: the top half shows. The second: the left half shows.
fn masks() -> (LayerMask, LayerMask) {
    let gray = PixelFormat::GRAY8;
    let mut first = LayerMask { surface: Surface::with_default(gray, &[0.0]), ..LayerMask::reveal_all() };
    first.surface.fill_rect(Rect::new(0, 0, W as i32, H as i32 / 2), &[1.0]);
    let mut second = LayerMask { surface: Surface::with_default(gray, &[0.0]), ..LayerMask::reveal_all() };
    second.surface.fill_rect(Rect::new(0, 0, W as i32 / 2, H as i32), &[1.0]);
    (first, second)
}

fn write(path: &str) {
    let mut d = Document::new("Round trip probe", Size::new(W, H), ColorMode::Rgb, SampleType::U8);
    let fmt = d.pixel_format();
    let all = Rect::new(0, 0, W as i32, H as i32);
    d.layers.push(filled("Base", fmt, Rect::new(10, 10, 190, 110), [1.0, 0.0, 0.0, 1.0]));
    // A clipped layer with two masks, written as now (the product, and the private PCm2 block).
    let (first, second) = masks();
    let mut two = filled("Two masks", fmt, all, [0.0, 0.0, 1.0, 1.0]);
    two.clipped = true;
    two.mask = Some(first.clone());
    two.second_mask = Some(second.clone());
    d.layers.push(two);
    // The helper: hidden, clipped, its pixels the second mask as gray, its mask the first.
    let mut helper = filled(HELPER, fmt, all, [0.0, 0.0, 0.0, 1.0]);
    if let Some(s) = helper.surface_mut() {
        s.fill_rect(Rect::new(0, 0, W as i32 / 2, H as i32), &[1.0, 1.0, 1.0, 1.0]);
    }
    helper.mask = Some(first);
    helper.visible = false;
    helper.clipped = true;
    d.layers.push(helper);
    // Clipped above the helper: the chain must still reach the base.
    let mut above = filled("Clipped above", fmt, Rect::new(0, 50, 200, 70), [0.0, 1.0, 0.0, 1.0]);
    above.clipped = true;
    d.layers.push(above);
    let mut lock_bit = filled("Lock bit", fmt, Rect::new(0, 0, 20, 20), [1.0, 1.0, 0.0, 1.0]);
    lock_bit.locks.selection = true;
    d.layers.push(lock_bit);
    d.layers.push(filled(&format!("Name tag {TAG}"), fmt, Rect::new(20, 0, 40, 20), [1.0, 0.0, 1.0, 1.0]));
    let mut layer_xmp = filled("Layer XMP", fmt, Rect::new(40, 0, 60, 20), [0.0, 1.0, 1.0, 1.0]);
    layer_xmp.xmp = Some(Arc::from(xmp("layer")));
    d.layers.push(layer_xmp);
    let mut shmd = filled("Custom shmd", fmt, Rect::new(60, 0, 80, 20), [0.5, 0.5, 0.5, 1.0]);
    let item = photocraft_psd::metadata::MetadataItem::new(*b"PCsl", b"selection lock".to_vec());
    shmd.psd_blocks.push((*b"shmd", Arc::new(photocraft_psd::metadata::write_shmd(&[item]))));
    d.layers.push(shmd);
    let mut private = filled("Private block", fmt, Rect::new(80, 0, 100, 20), [0.2, 0.8, 0.2, 1.0]);
    private.psd_blocks.push((*b"PCpb", Arc::new(b"selection lock\0\0".to_vec())));
    d.layers.push(private);
    d.metadata.xmp = Some(Arc::from(xmp("document")));
    let out = match photocraft_io::export(&d, path, &Default::default()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("export failed: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = std::fs::write(path, &out.bytes) {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    }
    println!("wrote {path} ({} bytes)", out.bytes.len());
}

fn be_u32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(0..4)?.try_into().ok()?))
}

fn name(r: &LayerRecord) -> String {
    if let Some(b) = r.block(b"luni")
        && let Some(n) = be_u32(&b.data)
    {
        let units: Vec<u16> = b.data.get(4..4 + 2 * n as usize).unwrap_or_default().as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        return String::from_utf16_lossy(&units).trim_end_matches('\0').to_string();
    }
    String::from_utf8_lossy(&r.name).into_owned()
}

/// One line per carrier for the file at `path`.
fn report(path: &str) -> Vec<(String, String)> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return vec![("file".into(), format!("{e}"))],
    };
    let mut out = Vec::new();
    let file = match photocraft_psd::PsdFile::from_bytes(&bytes) {
        Ok(f) => f,
        Err(e) => return vec![("file".into(), format!("unreadable: {e}"))],
    };
    let doc_xmp = file.resources.iter().find_map(|r| r.xmp_str()).unwrap_or_default();
    out.push(("document XMP".into(), if doc_xmp.contains(NS) { "kept".into() } else { "gone".into() }));
    let layers = file.layer_info.as_ref().map(|i| i.layers.clone()).unwrap_or_default();
    let find = |pred: &dyn Fn(&str) -> bool| layers.iter().find(|r| pred(&name(r)));
    let lspf = |r: &LayerRecord| r.block(b"lspf").and_then(|b| be_u32(&b.data));
    out.push((
        "lock bit 24".into(),
        match find(&|n| n == "Lock bit").map(lspf) {
            Some(Some(v)) => format!("lspf {v:#010x}: {}", if v & (1 << 24) != 0 { "kept" } else { "gone" }),
            Some(None) => "no lspf block: gone".into(),
            None => "layer missing".into(),
        },
    ));
    out.push(("name tag".into(), if find(&|n| n.ends_with(TAG)).is_some() { "kept".into() } else { "gone".into() }));
    let shmd_of = |r: &LayerRecord| r.block(b"shmd").map(|b| b.data.clone());
    out.push((
        "layer XMP".into(),
        match find(&|n| n == "Layer XMP").map(|r| shmd_of(r).and_then(|d| photocraft_psd::metadata::layer_xmp(&d))) {
            Some(Some(x)) if x.contains(NS) => "kept".into(),
            Some(_) => "gone".into(),
            None => "layer missing".into(),
        },
    ));
    out.push((
        "custom shmd item".into(),
        match find(&|n| n == "Custom shmd").map(|r| shmd_of(r).and_then(|d| photocraft_psd::metadata::parse_shmd(&d).ok())) {
            Some(Some(items)) if items.iter().any(|i| &i.key == b"PCsl") => "kept".into(),
            Some(_) => "gone".into(),
            None => "layer missing".into(),
        },
    ));
    out.push((
        "private block PCpb".into(),
        match find(&|n| n == "Private block") {
            Some(r) if r.block(b"PCpb").is_some() => "kept".into(),
            Some(_) => "gone".into(),
            None => "layer missing".into(),
        },
    ));
    out.push((
        "PCm2 block".into(),
        match find(&|n| n == "Two masks") {
            Some(r) if r.block(b"PCm2").is_some() => "kept".into(),
            Some(_) => "gone".into(),
            None => "layer missing".into(),
        },
    ));
    for n in ["Two masks", HELPER, "Clipped above"] {
        out.push((
            format!("'{n}'"),
            match find(&|x| x == n) {
                Some(r) => format!(
                    "visible {}, clipped {}, has mask {}, lyid {:?}",
                    r.is_visible(),
                    r.clipping != 0,
                    !matches!(r.mask, photocraft_psd::layer::MaskData::None),
                    r.block(b"lyid").and_then(|b| be_u32(&b.data))
                ),
                None => "missing".into(),
            },
        ));
    }
    out.push(("layer order (bottom first)".into(), layers.iter().map(name).filter(|n| !n.starts_with("</")).collect::<Vec<_>>().join(" | ")));
    // The helper's pixels and mask as PhotoCraft reads them: they must still be the two masks.
    match photocraft_io::import(path, &bytes) {
        Ok(r) => {
            let d = r.document;
            let helper = d.walk().into_iter().map(|(_, _, l)| l).find(|l| l.name == HELPER);
            out.push((
                "helper's masks".into(),
                match helper {
                    Some(l) => {
                        let px = |x: i32, y: i32| l.surface().map(|s| s.pixel(x, y).first().copied().unwrap_or(-1.0)).unwrap_or(-1.0);
                        let m = |x: i32, y: i32| l.mask.as_ref().map_or(-1.0, |m| m.value(x, y));
                        let second_ok = px(20, 60) > 0.9 && px(180, 60) < 0.1;
                        let first_ok = m(100, 20) > 0.9 && m(100, 100) < 0.1;
                        format!("second mask (pixels) {}, first mask (layer mask) {}", ok(second_ok), ok(first_ok))
                    }
                    None => "helper missing".into(),
                },
            ));
            let two = d.walk().into_iter().map(|(_, _, l)| l).find(|l| l.name == "Two masks");
            out.push((
                "'Two masks' as PhotoCraft opens it".into(),
                match two {
                    Some(l) => format!("second mask {}, layer mask {}", l.second_mask.is_some(), l.mask.is_some()),
                    None => "missing".into(),
                },
            ));
        }
        Err(e) => out.push(("PhotoCraft import".into(), format!("failed: {e}"))),
    }
    out
}

fn ok(b: bool) -> &'static str {
    if b { "intact" } else { "changed" }
}

fn check(paths: &[String]) {
    let reports: Vec<Vec<(String, String)>> = paths.iter().map(|p| report(p)).collect();
    let Some(first) = reports.first() else { return };
    for (i, (k, _)) in first.iter().enumerate() {
        println!("{k}");
        for (p, r) in paths.iter().zip(&reports) {
            let v = r.get(i).map_or("—", |(_, v)| v.as_str());
            println!("    {:<28} {v}", std::path::Path::new(p).file_name().map_or(p.clone(), |n| n.to_string_lossy().into_owned()));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("write") if args.len() == 2 => write(&args[1]),
        Some("check") if args.len() >= 2 => check(&args[1..]),
        _ => {
            eprintln!("usage: psd_roundtrip_probe write <probe.psd> | check <file.psd> [<file.psd>…]");
            std::process::exit(2);
        }
    }
}
