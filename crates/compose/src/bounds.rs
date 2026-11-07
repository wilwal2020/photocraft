//! `Surface::content_bounds` with the per-tile scan cached by copy-on-write tile identity, so
//! layer bounds cost a hash lookup per tile instead of a pixel scan per render tile (the CPU
//! compositor asks for an effect layer's bounds in every tile; the GPU planner every frame).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use photocraft_geom::Rect;
use photocraft_raster::{Surface, Tile};

struct Entry {
    tile: Weak<Tile>,
    /// Content bounds inside the tile (tile-local), for one default pixel.
    default: Box<[u8]>,
    bounds: Option<(u16, u16, u16, u16)>,
}

/// The entries, and the size past which dead ones are dropped (see [`next_sweep`]).
struct Cache {
    entries: HashMap<usize, Entry>,
    sweep_at: usize,
}

const MIN_SWEEP: usize = 1 << 16;

/// The size at which to sweep again after a sweep left `survivors` live entries: twice that (at
/// least [`MIN_SWEEP`]), so a cache full of live tiles isn't swept again on every call.
fn next_sweep(survivors: usize) -> usize {
    survivors.saturating_mul(2).max(MIN_SWEEP)
}

fn cache() -> &'static Mutex<Cache> {
    static C: std::sync::OnceLock<Mutex<Cache>> = std::sync::OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache { entries: HashMap::new(), sweep_at: MIN_SWEEP }))
}

pub(crate) fn purge() {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.entries.clear();
    cache.sweep_at = MIN_SWEEP;
}

/// `Surface::content_bounds`, cached per tile.
pub fn content_bounds(s: &Surface) -> Rect {
    if s.tile_count() == 0 {
        return Rect::EMPTY;
    }
    let fmt = s.format();
    let mut dp = vec![0u8; fmt.bytes_per_pixel()];
    photocraft_raster::encode_pixel(&fmt, &s.default_pixel(), &mut dp);
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.entries.len() > c.sweep_at {
        c.entries.retain(|_, e| e.tile.strong_count() > 0);
        c.sweep_at = next_sweep(c.entries.len());
    }
    let mut out = Rect::EMPTY;
    for (coord, t) in s.tiles() {
        let key = Arc::as_ptr(t) as usize;
        let hit = c.entries.get(&key).filter(|e| e.tile.upgrade().is_some_and(|u| Arc::ptr_eq(&u, t)) && *e.default == *dp).map(|e| e.bounds);
        let b = match hit {
            Some(b) => b,
            None => {
                let generation = photocraft_raster::spill::read_error_generation();
                let b = t.content_bounds(&dp);
                if photocraft_raster::spill::check_since(generation).is_ok() && t.scratch_health().is_ok() {
                    c.entries.insert(key, Entry { tile: Arc::downgrade(t), default: dp.clone().into_boxed_slice(), bounds: b });
                }
                b
            }
        };
        if let Some((x0, y0, x1, y1)) = b {
            let o = coord.rect();
            let r = Rect::new(o.x0 + x0 as i32, o.y0 + y0 as i32, o.x0 + x1 as i32, o.y0 + y1 as i32);
            out = if out.is_empty() { r } else { out.union(&r) };
        }
    }
    out
}

/// [`differing_within`]'s entries: a tile, the tile-local rectangle looked at and the pixel
/// compared against, and what was found there.
struct PartEntry {
    tile: Weak<Tile>,
    default: Box<[u8]>,
    bounds: Option<(u16, u16, u16, u16)>,
}

struct PartCache {
    entries: HashMap<(usize, u64), PartEntry>,
    sweep_at: usize,
}

fn part_cache() -> &'static Mutex<PartCache> {
    static C: std::sync::OnceLock<Mutex<PartCache>> = std::sync::OnceLock::new();
    C.get_or_init(|| Mutex::new(PartCache { entries: HashMap::new(), sweep_at: MIN_SWEEP }))
}

/// Tile-local bounds of the pixels in `part` (tile-local, within the tile) that differ from `dp`.
fn part_bounds(t: &Tile, part: (u16, u16, u16, u16), dp: &[u8]) -> Option<(u16, u16, u16, u16)> {
    let bpp = dp.len();
    if bpp == 0 {
        return None;
    }
    let ts = photocraft_geom::TILE_SIZE as usize;
    let (px0, py0, px1, py1) = (usize::from(part.0), usize::from(part.1), usize::from(part.2), usize::from(part.3));
    let blank = dp.repeat(px1.saturating_sub(px0));
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let bytes = t.bytes();
    for y in py0..py1 {
        let start = (y * ts + px0) * bpp;
        let Some(row) = bytes.get(start..start + blank.len()) else { break };
        if row == blank.as_slice() {
            continue;
        }
        let first = row.chunks_exact(bpp).zip(blank.chunks_exact(bpp)).position(|(a, b)| a != b);
        let last = row.chunks_exact(bpp).zip(blank.chunks_exact(bpp)).rposition(|(a, b)| a != b);
        if let (Some(f), Some(l)) = (first, last) {
            x0 = x0.min(px0 + f);
            x1 = x1.max(px0 + l + 1);
            y0 = y0.min(y);
            y1 = y + 1;
        }
    }
    (x0 != usize::MAX).then_some((x0 as u16, y0 as u16, x1 as u16, y1 as u16))
}

/// The bounds within `area` of `s`'s pixels that differ from `pixel` (normalised channel values,
/// encoded in `s`'s format). Where `s` has no tile, every pixel reads as its default: those count
/// as differing unless the default is `pixel`. Each tile's part is scanned once and remembered by
/// tile identity, like [`content_bounds`], so asking every frame (a transform box's frame) costs
/// a lookup per tile.
pub fn differing_within(s: &Surface, area: Rect, pixel: &[f32]) -> Rect {
    use photocraft_geom::{TILE_SIZE, TileCoord};
    if area.is_empty() {
        return Rect::EMPTY;
    }
    let fmt = s.format();
    let mut dp = vec![0u8; fmt.bytes_per_pixel()];
    photocraft_raster::encode_pixel(&fmt, pixel, &mut dp);
    let mut default = vec![0u8; fmt.bytes_per_pixel()];
    photocraft_raster::encode_pixel(&fmt, &s.default_pixel(), &mut default);
    let untiled_differs = default != dp;
    let (a, b) = (TileCoord::containing(area.x0, area.y0), TileCoord::containing(area.x1 - 1, area.y1 - 1));
    let mut c = part_cache().lock().unwrap_or_else(|e| e.into_inner());
    if c.entries.len() > c.sweep_at {
        c.entries.retain(|_, e| e.tile.strong_count() > 0);
        c.sweep_at = next_sweep(c.entries.len());
    }
    let mut out = Rect::EMPTY;
    for ty in a.ty..=b.ty {
        for tx in a.tx..=b.tx {
            let coord = TileCoord::new(tx, ty);
            let o = coord.rect();
            let inside = o.intersect(&area);
            if inside.is_empty() {
                continue;
            }
            let r = match s.tile(coord) {
                None if untiled_differs => inside,
                None => continue,
                Some(t) => {
                    let local = |v: i32, base: i32| (v - base).clamp(0, TILE_SIZE) as u16;
                    let part = (local(inside.x0, o.x0), local(inside.y0, o.y0), local(inside.x1, o.x0), local(inside.y1, o.y0));
                    let packed = u64::from(part.0) | u64::from(part.1) << 16 | u64::from(part.2) << 32 | u64::from(part.3) << 48;
                    let key = (Arc::as_ptr(t) as usize, packed);
                    let hit = c.entries.get(&key).filter(|e| e.tile.upgrade().is_some_and(|u| Arc::ptr_eq(&u, t)) && *e.default == *dp).map(|e| e.bounds);
                    let found = match hit {
                        Some(found) => found,
                        None => {
                            let found = part_bounds(t, part, &dp);
                            c.entries.insert(key, PartEntry { tile: Arc::downgrade(t), default: dp.clone().into_boxed_slice(), bounds: found });
                            found
                        }
                    };
                    match found {
                        Some((x0, y0, x1, y1)) => Rect::new(o.x0 + i32::from(x0), o.y0 + i32::from(y0), o.x0 + i32::from(x1), o.y0 + i32::from(y1)),
                        None => continue,
                    }
                }
            };
            out = if out.is_empty() { r } else { out.union(&r) };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn differing_within_looks_only_inside_the_area() {
        // A mask white past a 64 x 64 canvas, black on it but for a white square.
        let mut m = Surface::with_default(photocraft_color::PixelFormat::GRAY8, &[1.0]);
        m.fill_rect(Rect::new(0, 0, 64, 64), &[0.0]);
        m.fill_rect(Rect::new(16, 16, 48, 48), &[1.0]);
        let canvas = Rect::new(0, 0, 64, 64);
        assert_eq!(differing_within(&m, canvas, &[0.0]), Rect::new(16, 16, 48, 48));
        assert_eq!(differing_within(&m, canvas, &[0.0]), Rect::new(16, 16, 48, 48), "cached");
        // Past the painted tile the default (white) differs from black.
        assert_eq!(differing_within(&m, Rect::new(0, 0, 300, 64), &[0.0]), Rect::new(16, 0, 300, 64));
        // Nothing differs from the default where nothing is painted.
        let blank = Surface::with_default(photocraft_color::PixelFormat::GRAY8, &[1.0]);
        assert_eq!(differing_within(&blank, canvas, &[1.0]), Rect::EMPTY);
        assert_eq!(differing_within(&blank, canvas, &[0.0]), canvas);
        assert_eq!(differing_within(&blank, Rect::EMPTY, &[0.0]), Rect::EMPTY);
    }

    #[test]
    fn sweeps_again_only_past_twice_the_survivors() {
        // A cache of live tiles past the minimum isn't swept on every call (it used to be).
        assert_eq!(next_sweep(0), MIN_SWEEP);
        assert_eq!(next_sweep(MIN_SWEEP / 2), MIN_SWEEP);
        assert_eq!(next_sweep(MIN_SWEEP), 2 * MIN_SWEEP);
        assert_eq!(next_sweep(usize::MAX), usize::MAX);
    }

    #[test]
    fn a_shared_tile_reads_the_same_bounds_at_every_coordinate() {
        let mut s = Surface::new(photocraft_color::PixelFormat::RGBA8);
        s.fill_rect(Rect::new(-300, 10, 1300, 900), &[0.2, 0.4, 0.6, 1.0]);
        assert_eq!(content_bounds(&s), Rect::new(-300, 10, 1300, 900));
        assert_eq!(content_bounds(&s), s.content_bounds(), "cached");
    }
}
