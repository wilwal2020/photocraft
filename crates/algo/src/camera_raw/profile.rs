//! Profiles (`crs:Look`): Camera Raw's creative profiles ("Artistic 04", "Modern 10", …). Each is
//! a pair of colour tables that Camera Raw applies at the profile's Amount:
//!
//! - a **look table**: a hue shift and saturation and value scales over a hue × saturation ×
//!   value grid of linear ProPhoto colours, interpolated as the DNG specification describes for
//!   its `ProfileLookTable` (the value sRGB-encoded for the lookup when the table says so);
//! - an **RGB table**: a 3-D colour lookup table in its own primaries and transfer curve.
//!
//! The tables come in Camera Raw's text encoding: a `crs:Table_…` value holds the table's binary
//! layout, zlib-compressed and written in base 85. `photocraft-io` reads them from the profile's
//! own file on this computer, so PhotoCraft carries no profile data of its own. The layout was read
//! from the files:
//! - Everything is little-endian: the kind (0 look, 1 RGB), the version, then the sizes.
//! - Look samples are three `f32` (hue shift in degrees, saturation scale, value scale), value
//!   outermost, then hue, then saturation. The value encoding follows (0 linear, 1 sRGB).
//! - RGB samples are three `u16` offsets from the identity, red outermost. The primaries follow
//!   (sRGB, Adobe RGB, ProPhoto, P3, Rec. 2020), then the transfer curve (linear, sRGB, 1.8, 2.2,
//!   Rec. 2020), then the gamut handling (clip, extend), then the amount's limits as `f64`.
//!
//! This was checked against Photoshop's result for "Artistic 04" (see the dev log of 2026-11-07).

use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::Encoding;
use super::acr::{self, D65_TO_D50, M3, PROPHOTO_XYZ, Space};
use crate::photo_util::{linear_to_srgb, par_rows, srgb_to_linear};

/// Camera Raw's base-85 digits, in value order.
const ALPHABET: &[u8; 85] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-:+=^!/*?`'|()[]{}@%$#";
/// Most bytes a table may inflate to (Adobe's are about 200 KB).
const MAX_TABLE_BYTES: usize = 16 << 20;
/// Most samples a look table may have, and most divisions per axis of an RGB table.
const MAX_LOOK_SAMPLES: usize = 1 << 20;
const MAX_RGB_DIVISIONS: usize = 128;

/// A creative profile and its Amount, as a Camera Raw preset applies it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    /// The profile's name (`crs:Name`), shown in the dialog.
    pub name: String,
    /// The profile's `crs:UUID` (empty when unknown).
    pub uuid: String,
    /// Amount, 0..=2 (Camera Raw's 0–200 %).
    pub amount: f32,
    /// The profile's `crs:LookTable` and `crs:RGBTable`, in Camera Raw's text encoding.
    pub look_table: Option<Table>,
    pub rgb_table: Option<Table>,
    /// The RGB table's own strength (`crs:RGBTableAmount`), which Amount scales.
    pub rgb_table_amount: f32,
}

impl Default for Profile {
    fn default() -> Self {
        Profile { name: String::new(), uuid: String::new(), amount: 1.0, look_table: None, rgb_table: None, rgb_table_amount: 1.0 }
    }
}

impl Profile {
    /// Does the profile change nothing (Amount 0, or no table that reads)?
    pub fn is_neutral(&self) -> bool {
        self.amount.is_nan() || self.amount <= 0.0 || (self.look().is_none() && self.rgb().is_none())
    }
    /// Do its tables read? A profile with a table that doesn't is refused by commands.
    pub fn validate(&self) -> Result<(), String> {
        if !self.amount.is_finite() || !(0.0..=2.0).contains(&self.amount) {
            return Err("profile amount must be in 0..=2".into());
        }
        if !self.rgb_table_amount.is_finite() {
            return Err("profile rgbTableAmount must be a number".into());
        }
        if self.look_table.as_ref().is_some_and(|t| t.look().is_none()) {
            return Err(format!("profile {:?}: its look table doesn't read", self.name));
        }
        if self.rgb_table.as_ref().is_some_and(|t| t.rgb().is_none()) {
            return Err(format!("profile {:?}: its RGB table doesn't read", self.name));
        }
        Ok(())
    }
    fn look(&self) -> Option<&LookTable> {
        self.look_table.as_ref().and_then(Table::look)
    }
    fn rgb(&self) -> Option<&RgbTable> {
        self.rgb_table.as_ref().and_then(Table::rgb)
    }
}

/// One of a profile's tables in Camera Raw's text encoding, decoded on first use. Clones share
/// the text and the decoded table.
#[derive(Clone)]
pub struct Table(Arc<TableData>);

struct TableData {
    text: String,
    hash: u64,
    decoded: OnceLock<Option<Decoded>>,
}

enum Decoded {
    Look(LookTable),
    Rgb(RgbTable),
}

impl Table {
    pub fn new(text: impl Into<String>) -> Self {
        use std::hash::{Hash, Hasher};
        let text = text.into();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        Table(Arc::new(TableData { text, hash: h.finish(), decoded: OnceLock::new() }))
    }
    /// The table as Camera Raw writes it (a `crs:Table_…` value).
    pub fn text(&self) -> &str {
        &self.0.text
    }
    /// Is it a look table (else an RGB table, or unreadable)?
    pub fn is_look(&self) -> bool {
        self.look().is_some()
    }
    fn decoded(&self) -> Option<&Decoded> {
        self.0.decoded.get_or_init(|| decode(&self.0.text)).as_ref()
    }
    fn look(&self) -> Option<&LookTable> {
        match self.decoded()? {
            Decoded::Look(l) => Some(l),
            Decoded::Rgb(_) => None,
        }
    }
    fn rgb(&self) -> Option<&RgbTable> {
        match self.decoded()? {
            Decoded::Rgb(r) => Some(r),
            Decoded::Look(_) => None,
        }
    }
}

impl PartialEq for Table {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || (self.0.hash == other.0.hash && self.0.text == other.0.text)
    }
}

impl std::fmt::Debug for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Table({} chars)", self.0.text.len())
    }
}

impl Serialize for Table {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.text)
    }
}

impl<'de> Deserialize<'de> for Table {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Table::new)
    }
}

/// Base 85 to bytes: each group of five digits (least significant first) is four little-endian
/// bytes; a last group of `n` digits is `n − 1` bytes.
fn base85(text: &str) -> Option<Vec<u8>> {
    let mut value = [u8::MAX; 256];
    for (i, &c) in ALPHABET.iter().enumerate() {
        value[usize::from(c)] = i as u8;
    }
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() / 5 * 4 > MAX_TABLE_BYTES {
        return None;
    }
    let mut out = Vec::with_capacity(digits.len() / 5 * 4 + 4);
    for group in digits.chunks(5) {
        let (mut x, mut m) = (0u64, 1u64);
        for &c in group {
            let d = value[usize::from(c)];
            if d == u8::MAX {
                return None;
            }
            x += u64::from(d) * m;
            m *= 85;
        }
        let n = if group.len() == 5 { 4 } else { group.len() - 1 };
        out.extend_from_slice(&(x as u32).to_le_bytes()[..n]);
    }
    Some(out)
}

/// The table in `text`, or `None` when it doesn't read.
fn decode(text: &str) -> Option<Decoded> {
    use std::io::Read;
    let raw = base85(text)?;
    let size = u32::from_le_bytes(raw.get(..4)?.try_into().ok()?) as usize;
    if size > MAX_TABLE_BYTES {
        return None;
    }
    let mut data = Vec::with_capacity(size);
    flate2::read::ZlibDecoder::new(raw.get(4..)?).take(MAX_TABLE_BYTES as u64 + 1).read_to_end(&mut data).ok()?;
    if data.len() != size {
        return None;
    }
    let mut r = Reader { data: &data, at: 0 };
    match r.u32()? {
        0 => LookTable::read(&mut r).map(Decoded::Look),
        1 => RgbTable::read(&mut r).map(Decoded::Rgb),
        _ => None,
    }
}

/// Little-endian reads from a decoded table.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let b = self.data.get(self.at..self.at.checked_add(N)?)?.try_into().ok()?;
        self.at += N;
        Some(b)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_le_bytes)
    }
    fn f32(&mut self) -> Option<f32> {
        self.take().map(f32::from_le_bytes).filter(|v| v.is_finite())
    }
    fn f64(&mut self) -> Option<f64> {
        self.take().map(f64::from_le_bytes).filter(|v| v.is_finite())
    }
}

/// Hue (0..6), saturation and value of a colour, as the DNG specification computes them.
fn hsv(c: [f32; 3]) -> (f32, f32, f32) {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let gap = mx - mn;
    if gap.is_nan() || mx.is_nan() || gap <= 0.0 || mx <= 0.0 {
        return (0.0, 0.0, mx);
    }
    let h = if c[0] == mx {
        (c[1] - c[2]) / gap
    } else if c[1] == mx {
        2.0 + (c[2] - c[0]) / gap
    } else {
        4.0 + (c[0] - c[1]) / gap
    };
    (if h < 0.0 { h + 6.0 } else { h }, gap / mx, mx)
}

fn from_hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    if s.is_nan() || s <= 0.0 {
        return [v; 3];
    }
    let h = h.rem_euclid(6.0);
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as u32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// A profile's look table (see the module notes).
struct LookTable {
    hues: usize,
    sats: usize,
    vals: usize,
    /// Hue shift (degrees), saturation scale and value scale per sample.
    samples: Vec<[f32; 3]>,
    /// The value is looked up sRGB-encoded.
    srgb: bool,
}

impl LookTable {
    fn read(r: &mut Reader<'_>) -> Option<Self> {
        let _version = r.u32()?;
        let (hues, sats, vals) = (r.u32()? as usize, r.u32()? as usize, r.u32()? as usize);
        let n = hues.checked_mul(sats)?.checked_mul(vals)?;
        if hues == 0 || sats < 2 || vals == 0 || n > MAX_LOOK_SAMPLES {
            return None;
        }
        let samples = (0..n).map(|_| Some([r.f32()?, r.f32()?, r.f32()?])).collect::<Option<Vec<_>>>()?;
        let srgb = r.u32().unwrap_or(0) == 1;
        Some(LookTable { hues, sats, vals, samples, srgb })
    }

    /// The table's change at `(h, s, v)` (hue 0..6, the value as looked up), interpolated.
    fn at(&self, h: f32, s: f32, v: f32) -> [f32; 3] {
        let hs = if self.hues < 2 { 0.0 } else { h * self.hues as f32 / 6.0 };
        let mut h0 = (hs.max(0.0) as usize).min(self.hues - 1);
        let hf = (hs - h0 as f32).clamp(0.0, 1.0);
        let h1 = if h0 >= self.hues - 1 {
            h0 = self.hues - 1;
            0
        } else {
            h0 + 1
        };
        let ss = s.clamp(0.0, 1.0) * (self.sats - 1) as f32;
        let s0 = (ss as usize).min(self.sats - 2);
        let sf = ss - s0 as f32;
        let (v0, v1, vf) = if self.vals < 2 {
            (0, 0, 0.0)
        } else {
            let vs = v.clamp(0.0, 1.0) * (self.vals - 1) as f32;
            let v0 = (vs as usize).min(self.vals - 2);
            (v0, v0 + 1, vs - v0 as f32)
        };
        let at = |v: usize, h: usize, s: usize| self.samples[(v * self.hues + h) * self.sats + s];
        let mix = |a: [f32; 3], b: [f32; 3], t: f32| std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t);
        let plane = |v: usize| {
            let lo = mix(at(v, h0, s0), at(v, h0, s0 + 1), sf);
            let hi = mix(at(v, h1, s0), at(v, h1, s0 + 1), sf);
            mix(lo, hi, hf)
        };
        if self.vals < 2 { plane(0) } else { mix(plane(v0), plane(v1), vf) }
    }

    /// The linear ProPhoto colour `c` through the table. Brighter than 1 keeps its ratio to the
    /// table's result at 1.
    fn map(&self, c: [f32; 3]) -> [f32; 3] {
        let c = c.map(|v| v.max(0.0));
        let (h, s, v) = hsv(c);
        if v.is_nan() || v <= 0.0 {
            return c;
        }
        let over = v.max(1.0);
        let v = v / over;
        let ve = if self.srgb { linear_to_srgb(v) } else { v };
        let [shift, sat, val] = self.at(h, s, ve);
        let ve = (ve * val).clamp(0.0, 1.0);
        let v = if self.srgb { srgb_to_linear(ve) } else { ve };
        from_hsv(h + shift / 60.0, (s * sat).min(1.0), v).map(|x| x * over)
    }
}

/// An RGB table's transfer curve.
#[derive(Clone, Copy)]
enum Curve {
    Linear,
    Srgb,
    Gamma(f32),
    Rec2020,
}

impl Curve {
    fn encode(self, v: f32) -> f32 {
        match self {
            Curve::Linear => v,
            Curve::Srgb => linear_to_srgb(v),
            Curve::Gamma(g) => v.max(0.0).powf(1.0 / g),
            Curve::Rec2020 => {
                if v < 0.018_053_97 {
                    4.5 * v
                } else {
                    1.099_296_8 * v.powf(0.45) - 0.099_296_8
                }
            }
        }
    }
    fn decode(self, v: f32) -> f32 {
        match self {
            Curve::Linear => v,
            Curve::Srgb => srgb_to_linear(v),
            Curve::Gamma(g) => v.max(0.0).powf(g),
            Curve::Rec2020 => {
                if v < 0.081_242_87 {
                    v / 4.5
                } else {
                    ((v + 0.099_296_8) / 1.099_296_8).powf(1.0 / 0.45)
                }
            }
        }
    }
}

/// The RGB → XYZ matrix of primaries `xy` with white `white` (chromaticities).
fn primaries_xyz(xy: [[f32; 2]; 3], white: [f32; 2]) -> Option<M3> {
    let col = |[x, y]: [f32; 2]| [x / y, 1.0, (1.0 - x - y) / y];
    let p: M3 = std::array::from_fn(|r| std::array::from_fn(|c| col(xy[c])[r]));
    let s = acr::apply(&acr::inverse(&p)?, col(white));
    Some(std::array::from_fn(|r| std::array::from_fn(|c| p[r][c] * s[c])))
}

/// Linear ProPhoto to the linear RGB of primaries `code`, and back.
fn table_space(code: u32) -> Option<(M3, M3)> {
    const D65: [f32; 2] = [0.3127, 0.3290];
    let xy = match code {
        0 => [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]],
        1 => [[0.64, 0.33], [0.21, 0.71], [0.15, 0.06]],
        2 => {
            let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
            return Some((id, id));
        }
        3 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]],
        4 => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
        _ => return None,
    };
    let to_pro = acr::mul(&acr::inverse(&PROPHOTO_XYZ)?, &acr::mul(&D65_TO_D50, &primaries_xyz(xy, D65)?));
    Some((acr::inverse(&to_pro)?, to_pro))
}

/// A profile's RGB table (see the module notes).
struct RgbTable {
    div: usize,
    /// Outputs (encoded, 0..1), red outermost.
    samples: Vec<[f32; 3]>,
    to_table: M3,
    from_table: M3,
    curve: Curve,
    /// Colours outside the table keep how far outside they are (else they clip).
    extend: bool,
    min_amount: f32,
    max_amount: f32,
}

impl RgbTable {
    fn read(r: &mut Reader<'_>) -> Option<Self> {
        let _version = r.u32()?;
        let (dims, div) = (r.u32()?, r.u32()? as usize);
        if dims != 3 || !(2..=MAX_RGB_DIVISIONS).contains(&div) {
            return None;
        }
        let nominal = |i: usize| ((i * 0xFFFF + (div - 1) / 2) / (div - 1)) as u32;
        let mut samples = Vec::with_capacity(div * div * div);
        for ri in 0..div {
            for gi in 0..div {
                for bi in 0..div {
                    let mut s = [0.0; 3];
                    for (c, i) in [ri, gi, bi].into_iter().enumerate() {
                        s[c] = ((u32::from(r.u16()?) + nominal(i)) & 0xFFFF) as f32 / 65535.0;
                    }
                    samples.push(s);
                }
            }
        }
        let (to_table, from_table) = table_space(r.u32().unwrap_or(2))?;
        let curve = match r.u32().unwrap_or(0) {
            0 => Curve::Linear,
            1 => Curve::Srgb,
            2 => Curve::Gamma(1.8),
            3 => Curve::Gamma(2.2),
            4 => Curve::Rec2020,
            _ => return None,
        };
        let extend = r.u32().unwrap_or(0) == 1;
        let min_amount = r.f64().unwrap_or(0.0) as f32;
        let max_amount = r.f64().map_or(2.0, |v| v as f32).max(min_amount);
        Some(RgbTable { div, samples, to_table, from_table, curve, extend, min_amount, max_amount })
    }

    /// The table at encoded `e` (0..1 per channel), trilinear.
    fn at(&self, e: [f32; 3]) -> [f32; 3] {
        let n = self.div - 1;
        let mut i0 = [0usize; 3];
        let mut f = [0.0f32; 3];
        for c in 0..3 {
            let p = e[c].clamp(0.0, 1.0) * n as f32;
            i0[c] = (p as usize).min(n - 1);
            f[c] = p - i0[c] as f32;
        }
        let mut out = [0.0f32; 3];
        for corner in 0..8 {
            let d = [corner >> 2 & 1, corner >> 1 & 1, corner & 1];
            let w: f32 = (0..3).map(|c| if d[c] == 1 { f[c] } else { 1.0 - f[c] }).product();
            if w == 0.0 {
                continue;
            }
            let s = self.samples[((i0[0] + d[0]) * self.div + i0[1] + d[1]) * self.div + i0[2] + d[2]];
            for c in 0..3 {
                out[c] += s[c] * w;
            }
        }
        out
    }

    /// The linear ProPhoto colour `c` through the table at `amount` (1: the table's own result).
    fn map(&self, c: [f32; 3], amount: f32) -> [f32; 3] {
        let x = acr::apply(&self.to_table, c);
        let inside = x.map(|v| v.clamp(0.0, 1.0));
        let e = inside.map(|v| self.curve.encode(v));
        let t = self.at(e);
        let out: [f32; 3] = std::array::from_fn(|i| {
            let v = self.curve.decode(e[i] + (t[i] - e[i]) * amount);
            if self.extend { v + (x[i] - inside[i]) } else { v }
        });
        acr::apply(&self.from_table, out)
    }
}

/// Applies `p` to straight RGBA pixels encoded with `enc`: the look table, then the RGB table,
/// on linear ProPhoto. Amount scales both changes (beyond 100 % it extends them).
pub(super) fn apply(px: &mut [[f32; 4]], w: usize, p: &Profile, enc: Encoding) {
    let amount = p.amount.clamp(0.0, 2.0);
    let (look, rgb) = (p.look(), p.rgb());
    if amount.is_nan() || amount <= 0.0 || (look.is_none() && rgb.is_none()) {
        return;
    }
    let rgb = rgb.map(|t| (t, (p.rgb_table_amount * amount).clamp(t.min_amount, t.max_amount)));
    let curves = enc.curves();
    let space = Space::of(enc);
    par_rows(px, w, 1, |_, row| {
        for q in row.iter_mut() {
            let lin = [0, 1, 2].map(|c| curves.decode(q[c]));
            let mut pro = acr::apply(space.matrix_to_prophoto(), lin);
            if let Some(look) = look {
                let o = look.map(pro);
                pro = std::array::from_fn(|i| pro[i] + (o[i] - pro[i]) * amount);
            }
            if let Some((t, a)) = rgb {
                pro = t.map(pro, a);
            }
            let back = acr::apply(space.matrix_from_prophoto(), pro);
            for c in 0..3 {
                q[c] = curves.encode(back[c]);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `bytes` in Camera Raw's encoding: the size, then zlib, in base 85.
    pub(crate) fn encode(bytes: &[u8]) -> String {
        use std::io::Write;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(bytes).unwrap();
        let mut raw = (bytes.len() as u32).to_le_bytes().to_vec();
        raw.extend(z.finish().unwrap());
        let mut out = String::new();
        for group in raw.chunks(4) {
            let mut b = [0u8; 4];
            b[..group.len()].copy_from_slice(group);
            let mut x = u32::from_le_bytes(b);
            for _ in 0..group.len() + 1 {
                out.push(char::from(ALPHABET[(x % 85) as usize]));
                x /= 85;
            }
        }
        out
    }

    /// A look table of `hues × 2 × 2` samples all `(shift, sat, val)`, value linear.
    fn look_bytes(hues: u32, shift: f32, sat: f32, val: f32) -> Vec<u8> {
        let mut b: Vec<u8> = [0u32, 1, hues, 2, 2].iter().flat_map(|v| v.to_le_bytes()).collect();
        for _ in 0..hues * 4 {
            for v in [shift, sat, val] {
                b.extend(v.to_le_bytes());
            }
        }
        b.extend(0u32.to_le_bytes());
        b
    }

    /// An identity RGB table of `div` divisions in ProPhoto, linear, `f` applied to each sample.
    fn rgb_bytes(div: u32, f: impl Fn([f32; 3]) -> [f32; 3]) -> Vec<u8> {
        let mut b: Vec<u8> = [1u32, 1, 3, div].iter().flat_map(|v| v.to_le_bytes()).collect();
        let n = div as usize - 1;
        let nominal = |i: usize| ((i * 0xFFFF + (n) / 2) / n) as u32;
        for r in 0..=n {
            for g in 0..=n {
                for bl in 0..=n {
                    let idx = [r, g, bl];
                    let out = f(idx.map(|i| nominal(i) as f32 / 65535.0));
                    for c in 0..3 {
                        let v = (out[c].clamp(0.0, 1.0) * 65535.0).round() as u32;
                        b.extend(((v.wrapping_sub(nominal(idx[c]))) as u16).to_le_bytes());
                    }
                }
            }
        }
        for v in [2u32, 0, 0] {
            b.extend(v.to_le_bytes());
        }
        for v in [0.0f64, 2.0] {
            b.extend(v.to_le_bytes());
        }
        b
    }

    #[test]
    fn base85_round_trips_and_rejects_strangers() {
        for n in 0..12 {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 37 + 5) as u8).collect();
            let text = encode(&bytes);
            let raw = base85(&text).unwrap();
            assert_eq!(u32::from_le_bytes(raw[..4].try_into().unwrap()) as usize, n);
        }
        assert!(base85("ab~cd").is_none(), "a character outside the alphabet");
        assert!(decode("").is_none() && decode("0000000000").is_none());
    }

    #[test]
    fn a_neutral_look_table_changes_nothing() {
        let p = Profile { name: "n".into(), look_table: Some(Table::new(encode(&look_bytes(6, 0.0, 1.0, 1.0)))), ..Default::default() };
        assert!(p.validate().is_ok() && !p.is_neutral());
        let mut px = vec![[0.8, 0.3, 0.1, 1.0], [0.2, 0.2, 0.2, 1.0], [0.0, 0.5, 1.0, 0.5]];
        let before = px.clone();
        apply(&mut px, 3, &p, Encoding::of(0.0));
        for (a, b) in px.iter().zip(&before) {
            assert!((0..4).all(|c| (a[c] - b[c]).abs() < 1e-4), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn look_table_scales_value_and_amount_scales_the_change() {
        // Value halved everywhere: grey 0.5 (linear) becomes 0.25, at Amount 0.5 halfway.
        let half = Table::new(encode(&look_bytes(6, 0.0, 1.0, 0.5)));
        let lin = |v: f32| linear_to_srgb(v);
        let run = |amount: f32| {
            let p = Profile { look_table: Some(half.clone()), amount, ..Default::default() };
            let g = lin(0.5);
            let mut px = vec![[g, g, g, 1.0]];
            apply(&mut px, 1, &p, Encoding::of(0.0));
            srgb_to_linear(px[0][1])
        };
        assert!((run(1.0) - 0.25).abs() < 1e-3, "{}", run(1.0));
        assert!((run(0.5) - 0.375).abs() < 1e-3, "{}", run(0.5));
        assert!((run(0.0) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn look_table_hue_shift_turns_the_hue() {
        // +120°: ProPhoto red to green.
        let t = LookTable { hues: 6, sats: 2, vals: 1, samples: vec![[120.0, 1.0, 1.0]; 12], srgb: false };
        let o = t.map([0.6, 0.0, 0.0]);
        assert!(o[1] > 0.59 && o[0] < 0.01 && o[2] < 0.01, "{o:?}");
    }

    #[test]
    fn rgb_table_identity_and_inversion() {
        let id = Profile { rgb_table: Some(Table::new(encode(&rgb_bytes(9, |c| c)))), ..Default::default() };
        let inv = Profile { rgb_table: Some(Table::new(encode(&rgb_bytes(17, |c| c.map(|v| 1.0 - v))))), ..Default::default() };
        assert!(id.validate().is_ok() && inv.validate().is_ok());
        let px0 = vec![[0.9, 0.4, 0.1, 1.0], [0.5, 0.5, 0.5, 1.0]];
        let mut px = px0.clone();
        apply(&mut px, 2, &id, Encoding::of(1.8));
        for (a, b) in px.iter().zip(&px0) {
            assert!((0..3).all(|c| (a[c] - b[c]).abs() < 2e-3), "{a:?} vs {b:?}");
        }
        // Inverting in linear ProPhoto: the 1.8-encoded grey 0.5 is linear 0.287, which
        // inverts to 0.713, encoded 0.829.
        let mut g = vec![[0.5, 0.5, 0.5, 1.0]];
        apply(&mut g, 1, &inv, Encoding::of(1.8));
        let want = (1.0 - 0.5f32.powf(1.8)).powf(1.0 / 1.8);
        assert!((g[0][0] - want).abs() < 3e-3, "{:?} vs {want}", g[0]);
    }

    #[test]
    fn unreadable_tables_are_refused_and_neutral() {
        let bad = Profile { name: "x".into(), rgb_table: Some(Table::new("not a table")), ..Default::default() };
        assert!(bad.validate().is_err());
        assert!(bad.is_neutral());
        // A look table offered as the RGB table doesn't read as one.
        let swapped = Profile { rgb_table: Some(Table::new(encode(&look_bytes(6, 0.0, 1.0, 1.0)))), ..Default::default() };
        assert!(swapped.validate().is_err());
    }

    #[test]
    fn serialises_as_the_table_text() {
        let p = Profile { name: "P".into(), look_table: Some(Table::new(encode(&look_bytes(6, 10.0, 1.0, 1.0)))), amount: 0.75, ..Default::default() };
        let j = serde_json::to_value(&p).unwrap();
        assert_eq!(j["lookTable"].as_str(), Some(p.look_table.as_ref().unwrap().text()));
        let back: Profile = serde_json::from_value(j).unwrap();
        assert_eq!(back, p);
    }
}
