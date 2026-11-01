//! Duplicate the selected layers and merge the copies (the start of a user's "dupe into group
//! masks" action), through `Session::execute` as the app runs it. Selections: the first `n`
//! visible masked fill layers, every one of them, or the layers named on the command line.
//!
//! ```sh
//! cargo run --release -p photocraft-engine --example bench_dupe_merge -- file.psd [reps] [name …]
//! ```

use std::time::Instant;

use photocraft_doc::{LayerContent, LayerId};
use photocraft_engine::Session;
use serde_json::json;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn run(s: &mut Session, label: &str, ids: &[LayerId], reps: usize) {
    if ids.len() < 2 {
        return;
    }
    let (mut dup, mut merge) = (Vec::new(), Vec::new());
    let mut area = String::new();
    for _ in 0..reps {
        s.execute("layer.select", json!({"layer": ids[0].0})).unwrap();
        for id in &ids[1..] {
            s.execute("layer.select", json!({"layer": id.0, "mode": "add"})).unwrap();
        }
        let t = Instant::now();
        s.execute("layer.duplicate", json!({})).unwrap();
        dup.push(ms(t));
        if std::env::var_os("PAUSE").is_some() {
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
        let t = Instant::now();
        let r = s.execute("layer.mergeLayers", json!({})).unwrap();
        merge.push(ms(t));
        let st = s.active().unwrap();
        let id = LayerId(r["layer"].as_u64().unwrap());
        if let Some(surf) = st.doc.layer(id).and_then(|l| l.surface()) {
            let b = surf.content_bounds();
            area = format!("{}×{} merged, {} tiles", b.width(), b.height(), surf.tile_count());
        }
        s.undo();
        s.undo();
    }
    println!("{label:<44} duplicate {:>7.1} ms   merge {:>7.1} ms   ({area})", median(dup), median(merge));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("give a file");
    let reps: usize = args.get(2).and_then(|r| r.parse().ok()).unwrap_or(3);
    let names: Vec<&String> = args.iter().skip(3).collect();
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let doc = photocraft_io::import(path, &bytes).unwrap_or_else(|e| panic!("{path}: {e}")).document;
    let mut s = Session::new();
    s.add_document(doc, None);
    let doc = s.active().unwrap().doc.clone();
    println!("{} × {}, {} layers", doc.size.width, doc.size.height, doc.walk().len());
    if names.first().is_some_and(|n| *n == "--list") {
        for (p, _, l) in doc.walk() {
            let kind = match &l.content {
                LayerContent::Raster(_) => "pixels",
                LayerContent::Fill(_) => "fill",
                LayerContent::Group(_) => "group",
                LayerContent::Adjustment(_) => "adjustment",
                LayerContent::Text(_) => "type",
                LayerContent::Smart(_) => "smart",
                _ => "other",
            };
            let b = photocraft_compose::layer_bounds(l, doc.bounds());
            let mask =
                l.mask.as_ref().map_or(String::new(), |m| format!(" mask {}x{}", m.surface.content_bounds().width(), m.surface.content_bounds().height()));
            println!(
                "{:indent$}{} [{kind}] {}x{}{mask}{}{}",
                "",
                l.name,
                b.width(),
                b.height(),
                if doc.shown_at(&p) { "" } else { " hidden" },
                if l.clipped { " clipped" } else { "" },
                indent = 2 * p.len().saturating_sub(1)
            );
        }
        return;
    }
    // `--action file.json "name" layer…`: select the layers, then play the action's steps one by
    // one, each timed (the app's actions.play runs the same commands).
    if names.first().is_some_and(|n| *n == "--action") {
        let file = names.get(1).expect("actions.json");
        let action = names.get(2).expect("action name");
        let text = std::fs::read_to_string(file.as_str()).unwrap();
        let all: serde_json::Value = serde_json::from_str(&text).unwrap();
        let list = all.get("actions").and_then(|a| a.as_array()).or_else(|| all.as_array()).expect("actions");
        let steps = list.iter().find(|a| a["name"] == action.as_str()).expect("no such action")["steps"].as_array().unwrap().clone();
        let ids: Vec<LayerId> = names[3..].iter().filter_map(|n| doc.walk().into_iter().find(|(_, _, l)| &&l.name == n).map(|(_, _, l)| l.id)).collect();
        for _ in 0..reps {
            s.execute("layer.select", json!({"layer": ids[0].0})).unwrap();
            for id in &ids[1..] {
                s.execute("layer.select", json!({"layer": id.0, "mode": "add"})).unwrap();
            }
            let before = s.active().unwrap().history.past_len();
            let total = Instant::now();
            let mut line = Vec::new();
            for st in &steps {
                let (id, p) = (st[0].as_str().unwrap(), st[1].clone());
                let t = Instant::now();
                let r = s.execute(id, p);
                line.push(format!("{id} {:.0}{}", ms(t), if r.is_err() { " ERR" } else { "" }));
            }
            println!("{} layers: {:.0} ms: {}", ids.len(), ms(total), line.join(", "));
            while s.active().unwrap().history.past_len() > before {
                s.undo();
            }
        }
        return;
    }
    // `--compose`: the merge's composite alone, for the first n masked fills, and what their
    // masks are (default pixel, density, feather, stored tiles).
    if names.first().is_some_and(|n| *n == "--compose") {
        let fills: Vec<&photocraft_doc::Layer> = doc
            .walk()
            .into_iter()
            .filter(|(p, _, l)| doc.shown_at(p) && matches!(l.content, LayerContent::Fill(_)) && l.mask.is_some())
            .map(|(_, _, l)| l)
            .collect();
        for l in fills.iter().filter(|l| !l.effects.items.is_empty()) {
            println!("  {} effects: {:?}", l.name, l.effects.items.iter().map(|e| format!("{e:?}").chars().take(300).collect::<String>()).collect::<Vec<_>>());
        }
        for l in fills.iter().take(6) {
            let m = l.mask.as_ref().unwrap();
            println!(
                "  {}: mask default {:?} density {} feather {} enabled {} {} tiles, clipped {}, blend {:?}, fx {}, cache {}",
                l.name,
                m.surface.default_pixel(),
                m.density,
                m.feather,
                m.enabled,
                m.surface.tile_count(),
                l.clipped,
                l.blend,
                !l.effects.items.is_empty(),
                l.fill_cache.is_some()
            );
        }
        let fmt = photocraft_color::PixelFormat::new(doc.mode, doc.depth, true);
        let mut last = 0.0;
        for n in 1..=fills.len().min(24) {
            let mut solo = (*doc).clone();
            solo.layers = fills.iter().take(n).map(|l| (*l).clone()).collect();
            let took = (0..3)
                .map(|_| {
                    let t = Instant::now();
                    let _ = photocraft_compose::render_tiled(&solo, doc.bounds(), 256);
                    ms(t)
                })
                .fold(f64::INFINITY, f64::min);
            let l = fills[n - 1];
            let m = l.mask.as_ref().unwrap();
            let kind = match &l.content {
                LayerContent::Fill(photocraft_doc::Fill::Solid(_)) => "solid",
                LayerContent::Fill(photocraft_doc::Fill::Gradient { .. }) => "gradient",
                _ => "pattern",
            };
            println!(
                "  +{:<24} {kind:<8} mask default {:?} {:>4} tiles feather {} clipped {} fx {} blend {:?}: {:>6.0} ms (+{:.0})",
                l.name,
                m.surface.default_pixel(),
                m.surface.tile_count(),
                m.feather,
                l.clipped,
                !l.effects.items.is_empty(),
                l.blend,
                took,
                took - last
            );
            last = took;
        }
        for n in [20, fills.len()] {
            let mut solo = (*doc).clone();
            solo.layers = fills.iter().take(n).map(|l| (*l).clone()).collect();
            for (name, area) in [("canvas", doc.bounds()), ("canvas+17", doc.bounds().inflate(17))] {
                for _ in 0..2 {
                    let t = Instant::now();
                    let _ = photocraft_compose::composite_to_surface(&solo, area, fmt, None);
                    println!("{n:>3} fills over {name}: {:.0} ms", ms(t));
                }
            }
            let mut nofx = solo.clone();
            for l in &mut nofx.layers {
                l.effects.items.clear();
            }
            let t = Instant::now();
            let _ = photocraft_compose::composite_to_surface(&nofx, doc.bounds(), fmt, None);
            println!("{n:>3} fills over canvas without effects: {:.0} ms", ms(t));
        }
        for n in [1, 4, 24, fills.len()] {
            let mut solo = (*doc).clone();
            solo.layers = fills.iter().take(n).map(|l| (*l).clone()).collect();
            for _ in 0..2 {
                let t = Instant::now();
                let _ = photocraft_compose::render_bands(&solo, doc.bounds(), 0, |_b| -> Result<(), ()> { Ok(()) });
                let bands = ms(t);
                let t = Instant::now();
                let _ = photocraft_compose::render_tiled(&solo, doc.bounds(), 256);
                let tiled = ms(t);
                let t = Instant::now();
                let _ = photocraft_compose::composite_to_surface(&solo, doc.bounds(), fmt, None);
                println!("{n:>3} fills: render_bands {bands:.0} ms, render_tiled {tiled:.0} ms, composite_to_surface {:.0} ms", ms(t));
            }
        }
        for n in [1, 5, 10, 20, fills.len()] {
            let mut solo = (*doc).clone();
            solo.layers = fills.iter().take(n).map(|l| (*l).clone()).collect();
            for l in &mut solo.layers {
                l.clipped = false;
            }
            let t = Instant::now();
            let out = photocraft_compose::composite_to_surface(&solo, doc.bounds(), fmt, None);
            println!("{n:>3} fills: composite {:.0} ms ({} tiles)", ms(t), out.tile_count());
        }
        return;
    }
    if !names.is_empty() {
        let ids: Vec<LayerId> = names.iter().filter_map(|n| doc.walk().into_iter().find(|(_, _, l)| &&l.name == n).map(|(_, _, l)| l.id)).collect();
        run(&mut s, "named layers", &ids, reps);
        return;
    }
    // Masked fill layers that show, in one sibling list each (a merge takes siblings' top level).
    let fills: Vec<LayerId> = doc
        .walk()
        .into_iter()
        .filter(|(p, _, l)| doc.shown_at(p) && matches!(l.content, LayerContent::Fill(_)) && l.mask.is_some())
        .map(|(_, _, l)| l.id)
        .collect();
    println!("{} visible masked fill layers", fills.len());
    for n in [2, 5, 10, 20] {
        run(&mut s, &format!("{n} masked fills"), &fills[..n.min(fills.len())], reps);
    }
    run(&mut s, "all masked fills", &fills, reps);
}
