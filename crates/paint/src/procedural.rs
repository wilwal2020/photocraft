//! Procedural (generated, deterministic) texture patterns and sampled tips for the built-in presets.
//! Everything here is original: seeded value noise and simple geometry, no bundled brush files.

use crate::brush::PatternStyle;
use crate::rng::hash2;
use crate::tile::{GrayTile, PatternImage};

#[inline]
fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Tileable value noise with `period` lattice cells over the tile.
fn value_noise(x: f32, y: f32, period: i32, salt: u64) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (smooth(x - x0), smooth(y - y0));
    let (xi, yi) = (x0 as i32, y0 as i32);
    let h = |i: i32, j: i32| hash2(i.rem_euclid(period), j.rem_euclid(period), salt);
    let a = h(xi, yi) + (h(xi + 1, yi) - h(xi, yi)) * tx;
    let b = h(xi, yi + 1) + (h(xi + 1, yi + 1) - h(xi, yi + 1)) * tx;
    a + (b - a) * ty
}

/// Tileable fractal noise in 0..1 (`cells` lattice cells across at the base octave).
fn fbm(x: f32, y: f32, size: f32, cells: i32, octaves: u32, salt: u64) -> f32 {
    let (mut sum, mut amp, mut norm, mut c) = (0.0, 1.0, 0.0, cells);
    for o in 0..octaves {
        sum += value_noise(x / size * c as f32, y / size * c as f32, c, salt.wrapping_add(o as u64 * 7919)) * amp;
        norm += amp;
        amp *= 0.5;
        c *= 2;
    }
    sum / norm
}

/// Generate a seamless pattern tile.
pub fn pattern(style: PatternStyle, size: u32, seed: u32) -> PatternImage {
    let n = size.clamp(8, 1024) as usize;
    let s = n as f32;
    let salt = u64::from(seed) * 0x0100_0000_01B3 + 17;
    let mut data = vec![0.0f32; n * n];
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let v = match style {
                PatternStyle::Noise => fbm(fx, fy, s, 8, 4, salt),
                PatternStyle::Paper => {
                    let base = fbm(fx, fy, s, 16, 4, salt);
                    let grain = hash2(x as i32, y as i32, salt ^ 0x55);
                    ((base - 0.5) * 2.2 + 0.5 + (grain - 0.5) * 0.35).clamp(0.0, 1.0)
                }
                PatternStyle::Canvas => {
                    // Over/under weave: threads every 8 px, alternating which direction is on top.
                    let period = 8.0;
                    let (cx, cy) = ((fx / period).floor() as i32, (fy / period).floor() as i32);
                    let (u, v) = ((fx / period).fract(), (fy / period).fract());
                    let warp = (std::f32::consts::PI * u).sin();
                    let weft = (std::f32::consts::PI * v).sin();
                    let top = if (cx + cy) % 2 == 0 { warp } else { weft };
                    (0.25 + 0.65 * top + (hash2(x as i32, y as i32, salt) - 0.5) * 0.15).clamp(0.0, 1.0)
                }
                PatternStyle::Dots => {
                    let period = 8.0;
                    let (u, v) = ((fx / period).fract() - 0.5, (fy / period).fract() - 0.5);
                    let d = (u * u + v * v).sqrt();
                    1.0 - smooth(((0.35 - d) / 0.1 + 0.5).clamp(0.0, 1.0))
                }
            };
            data[y * n + x] = v;
        }
    }
    PatternImage { width: n, height: n, data }
}

/// A chalk/charcoal tip: a disc with a ragged edge and grainy interior.
pub fn chalk_tip(n: u32, seed: u32) -> GrayTile {
    let salt = u64::from(seed) + 101;
    let c = n as f32 / 2.0;
    GrayTile::from_fn(n, n, |x, y| {
        let (dx, dy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
        let r = (dx * dx + dy * dy).sqrt() / c;
        let ang = dy.atan2(dx);
        let edge = 0.8 + 0.15 * value_noise((ang + std::f32::consts::PI) * 3.0, 0.5, 19, salt);
        let inside = ((edge - r) * 8.0).clamp(0.0, 1.0);
        let grain = hash2(x as i32, y as i32, salt);
        inside * if grain < 0.2 { 0.2 } else { 0.7 + 0.3 * grain }
    })
}

/// Most primitives one generated tip is drawn from, and most (pixel × primitive) visits it costs:
/// every pixel is tested against every primitive, so both counts are the caller's to multiply.
const MAX_PRIMITIVES: u32 = 4096;
const MAX_TIP_WORK: u64 = 1 << 27;

/// `count` limited so that an `n`×`n` tip tests at most [`MAX_TIP_WORK`] pixel/primitive pairs.
fn clamp_primitives(n: u32, count: u32) -> u32 {
    let per_primitive = (u64::from(n) * u64::from(n)).max(1);
    count.min(MAX_PRIMITIVES).min(u32::try_from(MAX_TIP_WORK / per_primitive).unwrap_or(u32::MAX))
}

/// A spatter tip: a cluster of round droplets of different sizes.
pub fn spatter_tip(n: u32, seed: u32, drops: u32) -> GrayTile {
    let drops = clamp_primitives(n, drops);
    let salt = u64::from(seed) + 202;
    let s = n as f32;
    let blobs: Vec<(f32, f32, f32)> = (0..drops)
        .map(|i| {
            let a = hash2(i as i32, 0, salt) * std::f32::consts::TAU;
            let d = hash2(i as i32, 1, salt).sqrt() * 0.38 * s;
            let r = (0.03 + 0.09 * hash2(i as i32, 2, salt).powi(2)) * s;
            (s / 2.0 + a.cos() * d, s / 2.0 + a.sin() * d, r)
        })
        .collect();
    GrayTile::from_fn(n, n, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        blobs.iter().fold(0.0f32, |acc, &(bx, by, r)| {
            let d = ((px - bx).powi(2) + (py - by).powi(2)).sqrt();
            acc.max((r + 0.5 - d).clamp(0.0, 1.0))
        })
    })
}

/// A fine flicked spray: `drops` droplets spread over the whole tile, thinning out towards its
/// edge, mostly pin-pricks with now and then a bigger drop.
pub fn fine_spatter_tip(n: u32, seed: u32, drops: u32) -> GrayTile {
    let drops = clamp_primitives(n, drops);
    let salt = u64::from(seed) + 1111;
    let s = n as f32;
    let blobs: Vec<(f32, f32, f32)> = (0..drops)
        .map(|i| {
            let a = hash2(i as i32, 0, salt) * std::f32::consts::TAU;
            let d = hash2(i as i32, 1, salt).powf(0.7) * 0.46 * s;
            let r = (0.004 + 0.022 * hash2(i as i32, 2, salt).powi(5)) * s;
            (s / 2.0 + a.cos() * d, s / 2.0 + a.sin() * d, r)
        })
        .collect();
    GrayTile::from_fn(n, n, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        blobs.iter().fold(0.0f32, |acc, &(bx, by, r)| {
            let d = ((px - bx).powi(2) + (py - by).powi(2)).sqrt();
            acc.max((r + 0.5 - d).clamp(0.0, 1.0))
        })
    })
}

/// A bristle tip: parallel hair streaks of varying density inside an ellipse.
pub fn bristle_tip(n: u32, seed: u32) -> GrayTile {
    let salt = u64::from(seed) + 303;
    let c = n as f32 / 2.0;
    GrayTile::from_fn(n, n, |x, y| {
        let (dx, dy) = ((x as f32 + 0.5 - c) / c, (y as f32 + 0.5 - c) / c);
        let r = (dx * dx + (dy * 1.8).powi(2)).sqrt();
        if r > 1.0 {
            return 0.0;
        }
        let hair = hash2(0, (y as f32 / 1.5) as i32, salt);
        let density = if hair < 0.25 { 0.0 } else { 0.5 + 0.5 * hair };
        density * (1.0 - r.powi(4))
    })
}

/// A charcoal tip: a squat ellipse of broken horizontal streaks with a gritty edge.
pub fn charcoal_tip(n: u32, seed: u32) -> GrayTile {
    let salt = u64::from(seed) + 404;
    let c = n as f32 / 2.0;
    GrayTile::from_fn(n, n, |x, y| {
        let (dx, dy) = ((x as f32 + 0.5 - c) / c, (y as f32 + 0.5 - c) / c);
        let r = (dx * dx + (dy * 1.6).powi(2)).sqrt();
        let edge = 0.85 + 0.15 * value_noise(dx * 6.0 + 9.0, dy * 6.0 + 9.0, 64, salt);
        if r > edge {
            return 0.0;
        }
        // Streaks: per-row density modulated along x so they break up.
        let row = hash2(0, y as i32, salt);
        let along = value_noise(x as f32 / 6.0, y as f32 / 1.5, 1 << 12, salt ^ 0x77);
        let grit = hash2(x as i32, y as i32, salt ^ 0x13);
        let v = (0.35 + 0.65 * row) * (0.4 + 0.6 * along) * if grit < 0.15 { 0.2 } else { 1.0 };
        v * ((edge - r) * 6.0).clamp(0.0, 1.0)
    })
}

/// A leaf: a pointed lens shape with a lighter midrib, stem towards -x.
pub fn leaf_tip(n: u32) -> GrayTile {
    let c = n as f32 / 2.0;
    GrayTile::from_fn(n, n, |x, y| {
        let (u, v) = ((x as f32 + 0.5 - c) / c, (y as f32 + 0.5 - c) / c);
        // Half-width of the blade at u: widest a little behind the middle, pointed at both ends.
        let t = ((u + 1.0) / 2.0).clamp(0.0, 1.0);
        let half = 0.42 * (std::f32::consts::PI * t).sin() * (1.0 - 0.25 * t);
        let inside = ((half - v.abs()) * c).clamp(0.0, 1.0);
        let rib = 1.0 - 0.45 * (1.0 - (v.abs() * c / 1.2).clamp(0.0, 1.0));
        let veins = 1.0 - 0.2 * (1.0 - ((((u * 7.0 - v.abs() * 5.0).fract()) - 0.5).abs() * 8.0).clamp(0.0, 1.0));
        let stem = if u < -0.75 && v.abs() * c < 0.9 { 1.0 } else { 0.0 };
        (inside * rib * veins).max(stem)
    })
}

/// Grass: a tuft of tapered, slightly curved blades rising from the bottom edge.
pub fn grass_tip(n: u32, seed: u32, blades: u32) -> GrayTile {
    let blades = clamp_primitives(n, blades);
    let salt = u64::from(seed) + 505;
    let s = n as f32;
    let bl: Vec<(f32, f32, f32, f32)> = (0..blades)
        .map(|i| {
            let base = (0.3 + 0.4 * hash2(i as i32, 0, salt)) * s;
            let height = (0.55 + 0.42 * hash2(i as i32, 1, salt)) * s;
            let lean = (hash2(i as i32, 2, salt) - 0.5) * 0.9;
            let width = (0.025 + 0.025 * hash2(i as i32, 3, salt)) * s;
            (base, height, lean, width)
        })
        .collect();
    GrayTile::from_fn(n, n, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let up = s - py;
        bl.iter().fold(0.0f32, |acc, &(base, height, lean, width)| {
            if up < 0.0 || up > height {
                return acc;
            }
            let t = up / height;
            let cx = base + lean * height * t * t;
            let w = width * (1.0 - t) + 0.3;
            acc.max((w - (px - cx).abs()).clamp(0.0, 1.0))
        })
    })
}

/// A sea sponge: a disc riddled with holes of different sizes.
pub fn sponge_tip(n: u32, seed: u32) -> GrayTile {
    let salt = u64::from(seed) + 606;
    let c = n as f32 / 2.0;
    GrayTile::from_fn(n, n, |x, y| {
        let (dx, dy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
        let r = (dx * dx + dy * dy).sqrt() / c;
        let edge = 0.78 + 0.2 * value_noise((dy.atan2(dx) + std::f32::consts::PI) * 2.5, 0.5, 16, salt);
        let disc = ((edge - r) * 6.0).clamp(0.0, 1.0);
        let pores = fbm(x as f32, y as f32, n as f32, 6, 3, salt);
        disc * ((pores - 0.42) * 6.0).clamp(0.0, 1.0)
    })
}

/// A four-point sparkle with a soft glow.
pub fn star_tip(n: u32) -> GrayTile {
    let c = n as f32 / 2.0;
    GrayTile::from_fn(n, n, |x, y| {
        let (u, v) = (((x as f32 + 0.5 - c) / c).abs(), ((y as f32 + 0.5 - c) / c).abs());
        let ray = |a: f32, b: f32| ((1.0 - a) * (1.0 - (b / (0.08 * (1.0 - a) + 0.005)).min(1.0))).max(0.0);
        let glow = (1.0 - (u * u + v * v).sqrt() * 2.5).max(0.0).powi(2);
        ray(u, v).max(ray(v, u)).max(glow).min(1.0)
    })
}

/// A rake: a row of separate round bristles of slightly different sizes.
pub fn rake_tip(n: u32, seed: u32, bristles: u32) -> GrayTile {
    let salt = u64::from(seed) + 707;
    let s = n as f32;
    let k = clamp_primitives(n, bristles).max(1);
    let dots: Vec<(f32, f32, f32)> = (0..k)
        .map(|i| {
            let x = s * (i as f32 + 0.5) / k as f32;
            let y = s / 2.0 + (hash2(i as i32, 0, salt) - 0.5) * s * 0.08;
            let r = s / k as f32 * (0.22 + 0.18 * hash2(i as i32, 1, salt));
            (x, y, r)
        })
        .collect();
    GrayTile::from_fn(n, n, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        dots.iter().fold(0.0f32, |acc, &(dx, dy, r)| {
            let d = ((px - dx).powi(2) + (py - dy).powi(2)).sqrt();
            acc.max((r + 0.5 - d).clamp(0.0, 1.0) * 0.9)
        })
    })
}

/// A dry-brush dash: a `w`×`h` upright bar with rounded ends, bristle streaks running along its
/// length and ragged, broken ends. Laid across the stroke (angle on Direction), Roundness then
/// sets each dash's length, since it squashes the tip's height.
pub fn dash_tip(w: u32, h: u32, seed: u32) -> GrayTile {
    let salt = u64::from(seed) + 808;
    let (cw, ch) = (w as f32 / 2.0, h as f32 / 2.0);
    GrayTile::from_fn(w, h, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let (u, v) = ((px - cw) / cw, (py - ch) / ch);
        // Each bristle (column) ends at its own length, so the ends come out ragged.
        let col = hash2(x as i32, 0, salt);
        let end = 0.86 + 0.12 * col;
        // Half width: full along the body, rounding off over the last part of the bristle length,
        // and a little narrower towards the bottom end, where the brush lifts.
        let t = (v.abs() / end).min(1.0);
        let lift = 1.0 - 0.18 * (v + 1.0) / 2.0;
        let half = (1.0 - t.powi(6)).max(0.0).sqrt() * lift * (0.92 + 0.08 * value_noise(py / 9.0, 0.5, 1 << 12, salt ^ 0x55));
        let inside = ((half - u.abs()) * cw).clamp(0.0, 1.0) * ((end - v.abs()) * ch).clamp(0.0, 1.0);
        if inside <= 0.0 {
            return 0.0;
        }
        // Streaks: a few columns run lighter, and near the ends some bristles break off.
        let streak = if col < 0.15 { 0.7 } else { 0.9 + 0.1 * col };
        let broken = v.abs() > 0.6 && hash2(x as i32, (py / 3.0) as i32, salt ^ 0x99) < (v.abs() - 0.6) * 1.2;
        inside * streak * if broken { 0.35 } else { 1.0 }
    })
}

/// A dry flat-brush tip for washes: a short bar (`w` along the stroke, `h` across it, for a tip
/// turned with the stroke direction) whose bristles run along the stroke. They differ in how
/// much paint they hold (soft streaks, a few thin lighter lines, no clean gaps), each ends at
/// its own length, and towards the sides they thin out and break up, as a half-dry brush does.
pub fn dry_wash_tip(w: u32, h: u32, seed: u32) -> GrayTile {
    let salt = u64::from(seed) + 909;
    let (cw, ch) = (w as f32 / 2.0, h as f32 / 2.0);
    GrayTile::from_fn(w, h, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let (u, v) = ((px - cw) / cw, (py - ch) / ch);
        // One row of the tile is one bristle (or a few, for a big tile).
        let row = (py / (h as f32 / 96.0).max(1.0)) as i32;
        let r = hash2(row, 0, salt);
        // Paint held: bundles of bristles (broad and fine noise across the brush), a few thin
        // lighter hairs.
        let bundles = 0.55 * value_noise(py / h as f32 * 7.0, 0.5, 1 << 12, salt ^ 0x11) + 0.45 * value_noise(py / h as f32 * 23.0, 0.5, 1 << 12, salt ^ 0x22);
        let mut held = 0.3 + 0.7 * bundles;
        if r < 0.18 {
            held *= 0.4;
        }
        // The sides: bristles thin out over the outer fifth and some there hold almost nothing.
        let side = ((1.0 - v.abs()) / 0.2).clamp(0.0, 1.0);
        let dry = v.abs() > 0.75 && hash2(row, 7, salt) < (v.abs() - 0.75) * 2.4;
        if dry {
            held *= 0.25;
        }
        // Along the stroke: each bristle's own length, soft at its ends so dabs run together.
        // Shorter towards the sides, so the ends come out rounded rather than square.
        let len = (0.75 + 0.25 * hash2(row, 3, salt)) * (1.0 - 0.45 * v.abs().powi(3));
        let along = smooth(((len - u.abs()) / 0.45).clamp(0.0, 1.0));
        (held * smooth(side) * along).clamp(0.0, 1.0)
    })
}

/// A square flat-brush tip for washes (`n` × `n`; bristles run along the tile's x axis, along the
/// stroke for a tip turned with the stroke direction). The paint varies in soft bundles across
/// the brush; the sides fade evenly over the outer part (so strokes that turn or go back and
/// forth keep clean sides instead of a stack of single-hair lines); at the ends single hairs run
/// on past the body in a dry, spiky fringe.
pub fn dry_wash_square_tip(n: u32, seed: u32) -> GrayTile {
    dry_wash_edged_tip(n, seed, 0.2)
}

/// [`dry_wash_square_tip`] with its sides fading over `side_fade` of the half-width (0.2 soft,
/// about 0.06 a crisp edge where a stroke's shape reads clearly against the paper).
pub fn dry_wash_edged_tip(n: u32, seed: u32, side_fade: f32) -> GrayTile {
    let side_fade = side_fade.clamp(0.01, 1.0);
    let salt = u64::from(seed) + 1212;
    let c = n as f32 / 2.0;
    GrayTile::from_fn(n, n, |x, y| {
        let (u, v) = ((x as f32 + 0.5 - c) / c, (y as f32 + 0.5 - c) / c);
        let t = (v + 1.0) / 2.0;
        // Paint held across the brush: broad to fine bundles (streaks), no lone dry hairs.
        let bundles = 0.45 * value_noise(t * 5.0, 0.5, 1 << 12, salt ^ 0x11)
            + 0.3 * value_noise(t * 14.0, 0.5, 1 << 12, salt ^ 0x22)
            + 0.25 * value_noise(t * 37.0, 0.5, 1 << 12, salt ^ 0x44);
        let hair = (y as f32 * 48.0 / n as f32) as i32;
        // Single hairs leave faint lines through the body.
        let held = (0.25 + 0.75 * smooth(((bundles - 0.15) / 0.7).clamp(0.0, 1.0))) * (0.88 + 0.12 * hash2(hair, 9, salt));
        // The sides fade evenly over their outer `side_fade`. At the ends the body stops on a gently
        // wavy line and single hairs run on past it, each its own length, tapering to a point:
        // the dry fringe a flat brush leaves where it lifts off.
        let side = smooth(((1.0 - v.abs()) / side_fade).clamp(0.0, 1.0));
        let body_end = 0.66 + 0.05 * value_noise(t * 11.0, 0.5, 1 << 12, salt ^ 0x33);
        let body = smooth(((body_end - u.abs()) / 0.05).clamp(0.0, 1.0));
        let reach = 0.32 * hash2(hair, 7, salt) * (0.55 + 0.45 * value_noise(t * 9.0, 0.5, 1 << 12, salt ^ 0x66));
        let past = (u.abs() - body_end + 0.03).max(0.0);
        let spike = if reach > 0.02 { (1.0 - past / reach).clamp(0.0, 1.0).powf(0.6) } else { 0.0 };
        held * side * body.max(spike * (0.75 + 0.25 * hash2(hair, 8, salt)))
    })
}

/// A seamless watercolour granulation pattern (`n` × `n`, a texture tile): pigment settled in
/// blotches of a few sizes across the paper plus a fine speckle, with a steep enough contrast that
/// a wash shows mottling inside a flat shape. Paper-white (1) where the paint holds, darker where
/// it thins out.
pub fn granulation_tile(n: u32, seed: u32) -> GrayTile {
    let salt = u64::from(seed) * 0x0100_0000_01B3 + 2323;
    let s = n as f32;
    GrayTile::from_fn(n, n, |x, y| {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let blotch = fbm(fx, fy, s, 5, 3, salt);
        let mottle = fbm(fx, fy, s, 22, 2, salt ^ 0x77);
        let speck = hash2(x as i32, y as i32, salt ^ 0x99);
        let v = 0.5 * blotch + 0.32 * mottle + 0.18 * speck;
        ((v - 0.5) * 2.6 + 0.62).clamp(0.0, 1.0)
    })
}

/// A dry bristle-clump tip (`w` × `h`): two slanted, overlapping clumps of short bristle marks,
/// dense in the middle of each clump and breaking up into scattered specks towards their edges,
/// with a sparse trail off one corner. Sparse on its own (a few tenths of the tile is paint);
/// laid down at close spacing the marks drag into a body with split, streaky edges.
pub fn bristle_clump_tip(w: u32, h: u32, seed: u32) -> GrayTile {
    let salt = u64::from(seed) + 1414;
    let (fw, fh) = (w as f32, h as f32);
    // Clumps: centre, half-size (as fractions of the tile) and density.
    let clumps = [(0.27f32, 0.33f32, 0.15f32, 0.28f32, 0.85f32), (0.67, 0.57, 0.24, 0.33, 1.0), (0.16, 0.66, 0.1, 0.13, 0.4)];
    GrayTile::from_fn(w, h, |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let (u, v) = (px / fw, py / fh);
        // Density: the strongest clump here, each a slanted, rounded box with a soft edge.
        let dens = clumps.iter().fold(0.0f32, |m, &(cx, cy, ax, ay, k)| {
            let dy = (v - cy) / ay;
            let dx = (u - cx + 0.3 * (v - cy)) / ax;
            let r = (dx.abs().powi(4) + dy.abs().powi(4)).powf(0.25);
            m.max(k * smooth(((1.15 - r) / 0.45).clamp(0.0, 1.0)))
        });
        if dens <= 0.0 {
            return 0.0;
        }
        // Bristle marks: fine grain stretched upright (short strokes) and a finer speckle, soft-valued.
        let marks = 0.6 * value_noise(px / 0.9, py / 3.2, 1 << 12, salt) + 0.4 * value_noise(px / 0.6, py / 1.2, 1 << 12, salt ^ 0x77);
        let thresh = 1.0 - 0.56 * dens;
        smooth(((marks - thresh) / 0.12).clamp(0.0, 1.0))
    })
}

/// A squarish-round tip of opaque paint that piles up at its edges, for painted highlights (`n` × `n`; the
/// tile's x axis runs along the stroke for a tip turned with the stroke direction). Bands along
/// both sides hold the most paint and the middle a thinner, streaky load, so a stroke's two edges
/// build up to a bright bead while its middle stays a little see-through.
pub fn gouache_rim_tip(n: u32, seed: u32) -> GrayTile {
    let salt = u64::from(seed) + 1313;
    let c = n as f32 / 2.0;
    GrayTile::from_fn(n, n, |x, y| {
        let (px, py) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
        // Squarish (a superellipse) rather than a circle: the rows near the sides are nearly as
        // long as the middle ones, so a stroke's sides get about as many dabs as its middle and
        // the side beads stay brighter (a circle's short side rows cancel them out; a full
        // square makes boxy blobs).
        let d = (px.abs().powf(2.6) + py.abs().powf(2.6)).powf(1.0 / 2.6) / c;
        let a = py.atan2(px) / std::f32::consts::TAU + 0.5;
        let edge = 0.9 + 0.08 * value_noise(a * 7.0, 0.5, 7, salt);
        let inside = ((edge - d) * c).clamp(0.0, 1.0);
        if inside <= 0.0 {
            return 0.0;
        }
        // Across the brush: the thin middle load, streaked along the stroke.
        let v = py / c;
        let t = (v + 1.0) / 2.0;
        let streak = 0.5 * value_noise(t * 13.0, 0.5, 1 << 12, salt ^ 0x51) + 0.5 * value_noise(t * 37.0, 0.5, 1 << 12, salt ^ 0x52);
        let middle = 0.18 + 0.36 * smooth(streak);
        // The beads, along both sides only: a ring round the tip would stack into arcs inside
        // the stroke.
        let bead = smooth(((v.abs() - 0.72) / 0.14).clamp(0.0, 1.0));
        inside * (middle + (1.0 - middle) * bead)
    })
}

/// A loaded round brush of gouache, for painted highlights: an `n`-pixel round tip with a
/// slightly wobbly outline whose bristles (rows of the tile, running along its x axis: along the
/// stroke, for a tip turned with the stroke direction) hold very different amounts of paint, in
/// bundles, with thinner paint towards the rim and dry, broken hairs at the sides. At low flow a
/// stroke keeps those streaks (thin drags with the colour beneath showing through); pressed
/// hard, the dabs build up to thick paint.
pub fn gouache_tip(n: u32, seed: u32) -> GrayTile {
    let salt = u64::from(seed) + 1010;
    let c = n as f32 / 2.0;
    GrayTile::from_fn(n, n, |x, y| {
        let (px, py) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
        let d = (px * px + py * py).sqrt() / c;
        // Outline: a few gentle bumps around the circle.
        let a = py.atan2(px) / std::f32::consts::TAU + 0.5;
        let edge = 0.88 + 0.1 * value_noise(a * 7.0, 0.5, 7, salt);
        // A pixel of anti-aliasing at the brush's own size.
        let inside = ((edge - d) * c).clamp(0.0, 1.0);
        if inside <= 0.0 {
            return 0.0;
        }
        // Thinner paint over the outer quarter of the radius.
        let rim = 1.0 - 0.45 * smooth(((d / edge - 0.72) / 0.28).clamp(0.0, 1.0));
        // Bristles: bundles (broad and fine noise across the brush) and single hairs.
        let v = (py / c + 1.0) / 2.0;
        let row = (v * 48.0) as i32;
        let bundles =
            0.5 * value_noise(v * 7.0, 0.5, 1 << 12, salt ^ 0x31) + 0.3 * value_noise(v * 19.0, 0.5, 1 << 12, salt ^ 0x47) + 0.2 * hash2(row, 1, salt);
        let mut held = 0.25 + 0.75 * smooth(((bundles - 0.2) / 0.6).clamp(0.0, 1.0));
        // A few hairs carry next to nothing: the gaps a real brush leaves.
        if hash2(row, 9, salt) < 0.14 {
            held *= 0.12;
        }
        // Towards the sides more and more hairs run dry.
        let side = (py / c).abs();
        if side > 0.65 && hash2(row, 5, salt) < (side - 0.65) * 1.8 {
            held *= 0.45;
        }
        inside * rim * held
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dash_tip_is_an_upright_bar() {
        let t = dash_tip(40, 128, 1);
        assert!(t.is_valid());
        assert_eq!((t.width, t.height), (40, 128));
        // Paint down the middle column from end to end, none at the corners.
        assert!(t.get(20, 64) > 0.5 && t.get(20, 20) > 0.3 && t.get(20, 108) > 0.3);
        assert_eq!(t.get(0, 0), 0.0);
        assert_eq!(t.get(39, 127), 0.0);
        assert_eq!(t.to_f32(), dash_tip(40, 128, 1).to_f32(), "deterministic");
    }

    /// The user's back-and-forth Dry Wash strokes showed stacks of hard arcs along the edges:
    /// the old tip's dry single hairs at its sides and rounded ends. The square tip streaks
    /// inside but fades its sides evenly; its ends got a dry hair fringe on the user's request.
    #[test]
    fn dry_wash_square_tip_streaks_with_even_sides_and_fringed_ends() {
        let t = dry_wash_square_tip(96, 5);
        assert!(t.is_valid());
        // Across the brush (down the middle column): streaks inside, no gap.
        let col: Vec<f32> = (20..76).map(|y| t.get(48, y)).collect();
        let (lo, hi) = col.iter().fold((1.0f32, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
        assert!(lo > 0.15 && hi - lo > 0.3, "streaks {lo}..{hi}");
        // Every column's sides fade out evenly: from the outer fifth to the edge, each row holds
        // no more than the row inside it (no lone hairs standing out).
        for x in (20..76).step_by(5) {
            for y in 0..9 {
                assert!(t.get(x, y) <= t.get(x, y + 1) + 1e-3, "top side at ({x}, {y})");
                assert!(t.get(x, 95 - y) <= t.get(x, 94 - y) + 1e-3, "bottom side at ({x}, {})", 95 - y);
            }
        }
        // The ends: the body stops a little way in, and past it single hairs run on to different
        // lengths (the dry fringe of the user's sample): at three quarters of the way out, some
        // rows still carry paint and some are bare.
        for x in [12u32, 83] {
            let rows: Vec<f32> = (30..66).map(|y| t.get(x, y) / t.get(48, y).max(1e-3)).collect();
            let wet = rows.iter().filter(|&&r| r > 0.4).count();
            let bare = rows.iter().filter(|&&r| r < 0.1).count();
            assert!(wet >= 4 && bare >= 4, "no fringe at x = {x}: {wet} wet, {bare} bare rows");
        }
        assert!((30..66).all(|y| t.get(30, y) > 0.8 * t.get(48, y)), "the body ends too early");
        assert_eq!(t.to_f32(), dry_wash_square_tip(96, 5).to_f32(), "deterministic");
    }

    /// The Dry Wash Grain tip (user request after comparing their colouring with the reference
    /// pages, 2026-11-08): the same square tip with its sides fading over far fewer rows, so a
    /// shadow shape keeps a crisp edge.
    #[test]
    fn dry_wash_edged_tip_fades_its_sides_in_fewer_rows() {
        assert_eq!(dry_wash_edged_tip(96, 5, 0.2).to_f32(), dry_wash_square_tip(96, 5).to_f32());
        // The outer rows (3..9 of 96): the soft tip is still fading there, the crisp one is at
        // full paint; the very edge row is near bare on both.
        let (soft, crisp) = (dry_wash_square_tip(96, 5), dry_wash_edged_tip(96, 5, 0.06));
        let sum = |t: &GrayTile, ys: std::ops::Range<u32>| {
            (20..76).flat_map(|x| ys.clone().map(move |y| (x, y))).map(|(x, y)| t.get(x, y) + t.get(x, 95 - y)).sum::<f32>()
        };
        assert!(sum(&crisp, 3..9) > 1.3 * sum(&soft, 3..9), "crisp {} vs soft {}", sum(&crisp, 3..9), sum(&soft, 3..9));
        assert!(sum(&crisp, 0..1) < 0.2 * sum(&crisp, 20..21), "the crisp tip still ends at its sides");
        assert!(dry_wash_edged_tip(96, 5, 0.06).is_valid());
    }

    /// Granulation mottles in blotches (neighbours alike, far pixels not), with enough dark
    /// patches to show inside a wash but mostly paper-white.
    #[test]
    fn granulation_tile_mottles_in_blotches() {
        let t = granulation_tile(256, 4);
        assert!(t.is_valid());
        let v = t.to_f32();
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let dark = v.iter().filter(|&&x| x < 0.4).count() as f32 / v.len() as f32;
        assert!((0.45..0.8).contains(&mean), "mean {mean}");
        assert!((0.05..0.45).contains(&dark), "dark share {dark}");
        let near: f32 = (0..255).flat_map(|y| (0..255).map(move |x| (x, y))).map(|(x, y)| (t.get(x, y) - t.get(x + 1, y)).abs()).sum::<f32>() / (255.0 * 255.0);
        let far: f32 =
            (0..255).flat_map(|y| (0..255).map(move |x| (x, y))).map(|(x, y)| (t.get(x, y) - t.get((x + 128) % 256, (y + 97) % 256)).abs()).sum::<f32>()
                / (255.0 * 255.0);
        assert!(near < 0.6 * far, "blotchy: neighbours differ by {near}, far pixels by {far}");
        // Seamless: the wrap-around step is no bigger than a step inside the tile.
        let wrap: f32 = (0..256).map(|y| (t.get(255, y) - t.get(0, y)).abs()).sum::<f32>() / 256.0;
        assert!(wrap < 2.0 * near, "seam {wrap} vs {near}");
    }

    #[test]
    fn dry_wash_tip_is_a_streaky_bar_without_gaps() {
        let t = dry_wash_tip(32, 128, 5);
        assert!(t.is_valid());
        assert_eq!((t.width, t.height), (32, 128));
        // Down the middle column (across the stroke): paint everywhere inside, none past the
        // ends, and bristles holding clearly different amounts.
        let col: Vec<f32> = (16..112).map(|y| t.get(16, y)).collect();
        let (lo, hi) = col.iter().fold((1.0f32, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
        assert!(lo > 0.05, "a clean gap across the brush: {lo}");
        assert!(hi - lo > 0.4, "streaks {lo}..{hi}");
        assert!([t.get(16, 0), t.get(16, 127), t.get(0, 64), t.get(31, 64)].iter().all(|&v| v < 0.01), "paint past the ends");
        assert_eq!(t.to_f32(), dry_wash_tip(32, 128, 5).to_f32(), "deterministic");
    }

    #[test]
    fn gouache_tip_has_bristles_gaps_and_a_thinner_rim() {
        let t = gouache_tip(96, 7);
        assert!(t.is_valid());
        // Across the middle (down the column, across the stroke): bristles holding clearly
        // different amounts.
        let mid: Vec<f32> = (30..66).map(|y| t.get(48, y)).collect();
        // A few hairs carry next to nothing (the gaps of a real brush), most carry plenty.
        let gaps = mid.iter().filter(|&&v| v < 0.15).count() as f32 / mid.len() as f32;
        assert!((0.02..0.35).contains(&gaps), "gap rows {gaps}");
        let held = mid.iter().filter(|&&v| v > 0.5).count() as f32 / mid.len() as f32;
        assert!(held > 0.4, "too little paint: {held}");
        // Thinner paint in the ring just inside the outline than over the middle disc.
        let ring_mean = |r0: f32, r1: f32| {
            let v: Vec<f32> = (0..96 * 96)
                .map(|i| (i % 96, i / 96))
                .filter(|&(x, y)| ((x as f32 - 47.5).hypot(y as f32 - 47.5) / 48.0 - (r0 + r1) / 2.0).abs() < (r1 - r0) / 2.0)
                .map(|(x, y)| t.get(x, y))
                .collect();
            v.iter().sum::<f32>() / v.len() as f32
        };
        let (inner, rim) = (ring_mean(0.0, 0.5), ring_mean(0.76, 0.86));
        assert!(rim < inner - 0.1, "rim {rim} vs middle {inner}");
        assert!([t.get(0, 0), t.get(95, 95), t.get(48, 0), t.get(0, 48)].iter().all(|&v| v == 0.0), "paint outside the round");
        assert_eq!(t.to_f32(), gouache_tip(96, 7).to_f32(), "deterministic");
    }

    #[test]
    fn gouache_rim_tip_holds_most_paint_along_its_sides() {
        let t = gouache_rim_tip(96, 7);
        assert!(t.is_valid());
        // Down the middle column (across the stroke): bright beads at both sides, a thinner,
        // streaky middle.
        let col: Vec<f32> = (0..96).map(|y| t.get(48, y)).collect();
        let middle = &col[30..66];
        let (lo, hi) = middle.iter().fold((1.0f32, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
        assert!(lo > 0.1 && hi < 0.65 && hi - lo > 0.15, "middle {lo}..{hi}");
        let bead = |r: std::ops::Range<usize>| col[r].iter().fold(0.0f32, |m, &v| m.max(v));
        assert!(bead(3..12) > 0.95 && bead(84..93) > 0.95, "side beads {} {}", bead(3..12), bead(84..93));
        assert!([t.get(0, 0), t.get(95, 95), t.get(48, 0), t.get(0, 48)].iter().all(|&v| v == 0.0), "paint outside the round");
        assert_eq!(t.to_f32(), gouache_rim_tip(96, 7).to_f32(), "deterministic");
    }

    #[test]
    fn bristle_clump_tip_is_two_sparse_clumps_of_marks() {
        let t = bristle_clump_tip(224, 184, 3);
        assert!(t.is_valid());
        let v = t.to_f32();
        let frac = v.iter().filter(|&&a| a > 0.5).count() as f32 / v.len() as f32;
        assert!((0.05..0.25).contains(&frac), "paint fraction {frac}");
        // Two clumps: paint in both the left and the right third, a thin gap between them.
        let col = |x0: u32, x1: u32| (x0..x1).flat_map(|x| (0..184).map(move |y| (x, y))).filter(|&(x, y)| t.get(x, y) > 0.5).count();
        assert!(col(20, 80) > 200 && col(150, 210) > 400, "clumps {} {}", col(20, 80), col(150, 210));
        // Grainy, not solid: inside the big clump many pixels stay bare.
        let inner: Vec<f32> = (130..170).flat_map(|x| (80..120).map(move |y| (x, y))).map(|(x, y)| t.get(x, y)).collect();
        let bare = inner.iter().filter(|&&a| a < 0.2).count() as f32 / inner.len() as f32;
        assert!((0.2..0.9).contains(&bare), "bare share inside a clump {bare}");
        assert_eq!(v, bristle_clump_tip(224, 184, 3).to_f32(), "deterministic");
    }

    #[test]
    fn fine_spatter_tip_is_mostly_pin_pricks() {
        let t = fine_spatter_tip(256, 5, 70);
        assert!(t.is_valid());
        // Sparse: a few percent of the tile is paint, spread right across it.
        let v = t.to_f32();
        let painted = v.iter().filter(|&&a| a > 0.5).count() as f32 / v.len() as f32;
        assert!((0.003..0.05).contains(&painted), "coverage {painted}");
        let quarters = [(0, 0), (128, 0), (0, 128), (128, 128)].map(|(x0, y0)| (y0..y0 + 128).any(|y| (x0..x0 + 128).any(|x| t.get(x, y) > 0.5)));
        assert!(quarters.iter().all(|&q| q), "drops in every quarter: {quarters:?}");
        assert!(fine_spatter_tip(1024, 1, u32::MAX).width == 1024);
    }

    #[test]
    fn tips_with_absurd_primitive_counts_stay_cheap() {
        // 4 billion drops over a 1024² tip would be 4·10¹⁵ pixel tests.
        let t = std::time::Instant::now();
        assert_eq!(spatter_tip(1024, 1, u32::MAX).width, 1024);
        assert_eq!(grass_tip(1024, 1, u32::MAX).height, 1024);
        assert_eq!(rake_tip(1024, 1, u32::MAX).width, 1024);
        assert!(t.elapsed().as_secs() < 60, "{:?}", t.elapsed());
        assert!(clamp_primitives(1024, u32::MAX) <= MAX_PRIMITIVES);
        assert!(clamp_primitives(60_000, 100) < 100, "a huge tip gets fewer primitives");
        assert_eq!(clamp_primitives(0, 7), 7);
    }

    #[test]
    fn patterns_are_seamless_and_in_range() {
        for st in [PatternStyle::Noise, PatternStyle::Paper, PatternStyle::Canvas, PatternStyle::Dots] {
            let p = pattern(st, 64, 3);
            assert!(p.data.iter().all(|v| (0.0..=1.0).contains(v)), "{st:?}");
            let var = p.data.iter().map(|v| (v - 0.5).abs()).sum::<f32>() / p.data.len() as f32;
            assert!(var > 0.02, "{st:?} is flat");
            // Deterministic.
            assert_eq!(p.data, pattern(st, 64, 3).data);
        }
        // Noise tiles: left and right columns are neighbours.
        let p = pattern(PatternStyle::Noise, 64, 1);
        let seam: f32 = (0..64).map(|y| (p.data[y * 64] - p.data[y * 64 + 63]).abs()).sum::<f32>() / 64.0;
        assert!(seam < 0.08, "{seam}");
    }

    #[test]
    fn tips_have_paint() {
        for t in [
            chalk_tip(48, 1),
            spatter_tip(48, 1, 12),
            bristle_tip(48, 1),
            charcoal_tip(48, 1),
            leaf_tip(48),
            grass_tip(48, 1, 9),
            sponge_tip(48, 1),
            star_tip(48),
            rake_tip(48, 1, 7),
        ] {
            assert!(t.is_valid());
            let sum: f32 = t.to_f32().iter().sum();
            assert!(sum > 48.0, "{sum}");
            assert_eq!(t.get(0, 0), 0.0);
        }
    }
}
