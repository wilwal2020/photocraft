//! [`spill::set_compression`]: idle tiles kept LZ4-compressed in memory.
//!
//! One test function: the tile manager is process-wide, so its steps must not interleave.

use photocraft_color::PixelFormat;
use photocraft_geom::{Rect, TILE_SIZE};
use photocraft_raster::{Surface, spill};

const TILE: usize = (TILE_SIZE * TILE_SIZE) as usize * 4;

/// A full-tile layer of flat colour with a line of "ink" across it: what colouring PSDs are made of.
fn line_art(tiles: i32, seed: u8) -> Surface {
    let mut s = Surface::new(PixelFormat::RGBA8);
    let side = tiles * TILE_SIZE;
    s.fill_rect(Rect::new(0, 0, side, side), &[0.2, 0.4, f32::from(seed) / 255.0, 1.0]);
    for x in 0..side {
        let y = (x * 7 / 5 + i32::from(seed)) % side;
        s.write_pixel(x, y, &[0.0, 0.0, 0.0, 0.5]);
        s.write_pixel(x, (y + 1) % side, &[0.0, 0.0, 0.0, 1.0]);
    }
    s
}

/// A tile of pseudo-random bytes: LZ4 can't shrink it.
fn noise(seed: u32) -> Surface {
    let mut state = seed.wrapping_mul(2_654_435_761).max(1);
    let mut bytes = vec![0u8; TILE];
    for b in &mut bytes {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *b = state as u8;
    }
    Surface::from_interleaved(PixelFormat::RGBA8, Rect::new(0, 0, TILE_SIZE, TILE_SIZE), &bytes)
}

fn pixels(s: &Surface) -> Vec<u8> {
    s.to_interleaved(s.tile_bounds())
}

fn used() -> usize {
    let st = spill::stats();
    st.resident_bytes + st.packed_bytes
}

#[test]
fn idle_tiles_compress_in_memory_and_read_back_unchanged() {
    spill::configure(None, 0).unwrap();
    spill::set_compression(0);

    // Off by default: nothing is compressed however many tiles there are.
    let mut layers: Vec<Surface> = (0..8).map(|i| line_art(2, i)).collect();
    let originals: Vec<Vec<u8>> = layers.iter().map(pixels).collect();
    spill::settle();
    assert_eq!(spill::stats().packed_tiles, 0);
    let before = used();
    assert!(before >= 8 * 4 * TILE, "{before}");

    // A budget of two tiles: every idle tile is compressed, and tile memory falls far below
    // what the pixels take uncompressed.
    spill::set_compression(2 * TILE);
    spill::settle();
    let st = spill::stats();
    assert!(st.packed_tiles >= 8 * 4 - 2, "{st:?}");
    assert!(used() <= 2 * TILE + 8 * 4 * TILE / 20, "flat colour and line art compress well: {st:?}");
    assert!(st.written_bytes == 0 && st.scratch_bytes == 0, "no scratch disk is involved: {st:?}");

    // Reading expands a tile and gives back exactly the pixels it had.
    for (layer, original) in layers.iter().zip(&originals) {
        assert_eq!(&pixels(layer), original);
    }
    assert!(spill::stats().unpacks > 0);

    // Writing to a compressed tile works like writing to any other, and the change sticks once
    // the tile is compressed again.
    spill::settle();
    layers[3].write_pixel(5, 6, &[1.0, 0.0, 0.0, 1.0]);
    spill::settle();
    assert_eq!(layers[3].pixel(5, 6), vec![1.0, 0.0, 0.0, 1.0]);
    let mut expected = originals[3].clone();
    let i = (6 * (2 * TILE_SIZE as usize) + 5) * 4;
    expected[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
    assert_eq!(pixels(&layers[3]), expected);

    // Undo-style sharing: a clone shares the compressed tiles and reads the same.
    let copy = layers[1].clone();
    assert_eq!(pixels(&copy), originals[1]);
    drop(copy);

    // Dropping tiles releases their compressed copies.
    drop(layers);
    let st = spill::stats();
    assert_eq!((st.packed_bytes, st.packed_tiles), (0, 0), "{st:?}");

    // Tiles of one value are shared from their compressed copies, without expanding them.
    let fill = |v: f32| {
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.fill_rect(Rect::new(0, 0, 3 * TILE_SIZE, 2 * TILE_SIZE), &[v, 0.5, 0.25, 1.0]);
        // One mixed tile among the six.
        s.write_pixel(1, 1, &[0.0, 0.0, 0.0, 1.0]);
        s
    };
    // `fill_rect` shares one tile for the whole rect; separate copies are what a decoder makes.
    let mut flats: Vec<Surface> = (0..3)
        .map(|_| {
            let s = fill(0.75);
            let mut own = Surface::new(PixelFormat::RGBA8);
            own.write_interleaved(s.tile_bounds(), &pixels(&s));
            own
        })
        .collect();
    let originals: Vec<Vec<u8>> = flats.iter().map(pixels).collect();
    spill::settle();
    let st = spill::stats();
    assert!(st.packed_tiles >= 15, "{st:?}");
    let mut interner = photocraft_raster::TileInterner::default();
    let saved: usize = flats.iter_mut().map(|s| s.share_uniform_tiles(&mut interner)).sum();
    assert_eq!(saved, 14 * TILE, "15 uniform tiles become one");
    assert_eq!(interner.len(), 1);
    assert_eq!(spill::stats().unpacks, st.unpacks, "nothing was expanded to find them");
    for (s, original) in flats.iter().zip(&originals) {
        assert_eq!(&pixels(s), original);
    }
    drop((flats, interner));

    // Tiles that don't compress stay as they are, and a sweep that finds nothing to take
    // doesn't rescan for every new tile.
    let noisy: Vec<Surface> = (1..=6).map(noise).collect();
    let originals: Vec<Vec<u8>> = noisy.iter().map(pixels).collect();
    spill::settle();
    let st = spill::stats();
    assert_eq!(st.packed_tiles, 0, "noise must not be kept compressed: {st:?}");
    for (n, original) in noisy.iter().zip(&originals) {
        assert_eq!(&pixels(n), original);
    }
    let more: Vec<Surface> = (7..=9).map(noise).collect();
    assert_eq!(spill::stats().packed_tiles, 0);
    drop((noisy, more));

    // With a scratch disk as well, a compressed copy the clock comes back to goes to disk, and
    // reads back from there. This needs a volume above the scratch reserve (5 % free).
    let dir = std::env::temp_dir().join(format!("photocraft-tile-compression-{}", std::process::id()));
    let layers: Vec<Surface> = (0..4).map(|i| line_art(2, 40 + i)).collect();
    let originals: Vec<Vec<u8>> = layers.iter().map(pixels).collect();
    if spill::configure(Some(&dir), 1).is_ok() && !spill::paused() {
        spill::set_compression(1);
        spill::settle();
        spill::settle();
        let st = spill::stats();
        assert!(st.written_bytes > 0, "compressed copies spill on the clock's next pass: {st:?}");
        assert!(st.written_bytes < (4 * 4 * TILE / 10) as u64, "they are written compressed: {st:?}");
        for (layer, original) in layers.iter().zip(&originals) {
            assert_eq!(&pixels(layer), original);
        }
    } else {
        eprintln!("scratch disk paused ({:?}); skipped the spill part", spill::stats().last_error);
    }
    drop(layers);
    spill::configure(None, 0).unwrap();
    spill::set_compression(0);
    let _ = std::fs::remove_dir_all(&dir);
}
