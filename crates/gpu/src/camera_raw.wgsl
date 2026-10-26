// Camera Raw's develop on the GPU: the passes of `photocraft_algo::camera_raw::gpu::program`.
// Each function mirrors the CPU stage it names in `photocraft_algo::camera_raw` (and `acr`), down
// to the order of operations, so the two agree to float rounding. The op and plane codes
// (`OP_*`, `PL_*`) are declared ahead of this source from the Rust constants.

struct Pass {
    start: u32,
    w: u32,
    h: u32,
    kind: u32,
    x0: i32,
    y0: i32,
    full_w: u32,
    full_h: u32,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
    n: u32,
    f: f32,
    k: f32,
    cx: f32,
    cy: f32,
    diag: f32,
    pw: u32,
    ph: u32,
}

@group(0) @binding(0) var<uniform> P: Pass;
@group(0) @binding(1) var<storage, read> prog: array<u32>;
@group(0) @binding(2) var<storage, read> tab: array<f32>;
@group(0) @binding(3) var<storage, read_write> img: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> pl: array<f32>;
@group(0) @binding(5) var<storage, read> prev: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read> maps: array<f32>;

const NONE: u32 = 0xffffffffu;

// The header (see `gpu::HEADER`).
var<private> G: f32;
var<private> DEC: u32;
var<private> ENC: u32;
var<private> SDEC: u32;
var<private> SENC: u32;
var<private> TO: array<f32, 9>;
var<private> FROM: array<f32, 9>;
var<private> YR: vec3<f32>;

fn arg(i: u32) -> u32 {
    return prog[i];
}
fn argf(i: u32) -> f32 {
    return bitcast<f32>(prog[i]);
}

fn load_header() {
    G = argf(0u);
    DEC = prog[1];
    ENC = prog[2];
    SDEC = prog[3];
    SENC = prog[4];
    for (var i = 0u; i < 9u; i++) {
        TO[i] = argf(5u + i);
        FROM[i] = argf(14u + i);
    }
    YR = vec3<f32>(argf(23u), argf(24u), argf(25u));
}

fn to_pro(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(TO[0] * v.x + TO[1] * v.y + TO[2] * v.z, TO[3] * v.x + TO[4] * v.y + TO[5] * v.z, TO[6] * v.x + TO[7] * v.y + TO[8] * v.z);
}
fn from_pro(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(FROM[0] * v.x + FROM[1] * v.y + FROM[2] * v.z, FROM[3] * v.x + FROM[4] * v.y + FROM[5] * v.z, FROM[6] * v.x + FROM[7] * v.y + FROM[8] * v.z);
}

fn luma(c: vec3<f32>) -> f32 {
    return 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;
}

fn smoothstep_(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// x^y for x ≥ 0 (0 at 0, as `powf` gives for a positive y).
fn powp(x: f32, y: f32) -> f32 {
    if (x <= 0.0) {
        return 0.0;
    }
    return pow(x, y);
}

fn rem_euclid(a: f32, b: f32) -> f32 {
    let r = a - b * trunc(a / b);
    if (r < 0.0) {
        return r + abs(b);
    }
    return r;
}

// --- Encodings (`Encoding`, `Curves`) -------------------------------------------------------

fn srgb_to_linear(v: f32) -> f32 {
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}
fn linear_to_srgb(v: f32) -> f32 {
    if (v <= 0.0031308) {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}
fn exact_decode(g: f32, v: f32) -> f32 {
    if (g > 0.0) {
        return powp(max(v, 0.0), g);
    }
    return srgb_to_linear(v);
}
fn exact_encode(g: f32, v: f32) -> f32 {
    if (g > 0.0) {
        return powp(max(v, 0.0), 1.0 / g);
    }
    return linear_to_srgb(v);
}
fn decode_with(g: f32, t: u32, v: f32) -> f32 {
    let x = v * 4096.0;
    if (!(x >= 1.0 && v <= 1.0)) {
        return exact_decode(g, v);
    }
    let i = min(u32(x), 4095u);
    let a = tab[t + i];
    return a + (tab[t + i + 1u] - a) * (x - f32(i));
}
fn encode_with(g: f32, t: u32, v: f32) -> f32 {
    if (v >= 0.0 && v <= 16.0) {
        let x = sqrt(v / 16.0) * 8192.0;
        if (x >= 1.0) {
            let i = min(u32(x), 8191u);
            let a = tab[t + i];
            return a + (tab[t + i + 1u] - a) * (x - f32(i));
        }
    }
    return exact_encode(g, v);
}
fn dec(v: f32) -> f32 {
    return decode_with(G, DEC, v);
}
fn enc(v: f32) -> f32 {
    return encode_with(G, ENC, v);
}
fn senc(v: f32) -> f32 {
    return encode_with(0.0, SENC, v);
}
fn dec3(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dec(c.x), dec(c.y), dec(c.z));
}
fn enc3(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(enc(c.x), enc(c.y), enc(c.z));
}

// --- Tone curves (`acr::Light`, `acr::hue_preserving`) ----------------------------------------

fn light(lut: u32, past: f32, v: f32) -> f32 {
    if (v <= 0.0) {
        return tab[lut] + min(v, 0.0);
    }
    if (v >= 1.0) {
        return tab[lut + 4096u] + (v - 1.0) * past;
    }
    let p = sqrt(v) * 4096.0;
    let i = min(u32(p), 4095u);
    let a = tab[lut + i];
    return a + (tab[lut + i + 1u] - a) * (p - f32(i));
}

fn hue_preserve(pro: vec3<f32>, lut: u32, past: f32) -> vec3<f32> {
    let mx = max(max(pro.x, pro.y), pro.z);
    let mn = min(min(pro.x, pro.y), pro.z);
    let fmx = light(lut, past, mx);
    let fmn = light(lut, past, mn);
    if (mx - mn > 1e-9) {
        return vec3<f32>(fmn) + (fmx - fmn) * (pro - vec3<f32>(mn)) / (mx - mn);
    }
    return vec3<f32>(fmx);
}

// --- Colour helpers ---------------------------------------------------------------------------

fn hsv(c: vec3<f32>) -> vec3<f32> {
    let mx = max(max(c.x, c.y), c.z);
    let mn = min(min(c.x, c.y), c.z);
    let d = mx - mn;
    var h = 0.0;
    if (d <= 1e-9) {
        h = 0.0;
    } else if (mx == c.x) {
        h = 60.0 * rem_euclid((c.y - c.z) / d, 6.0);
    } else if (mx == c.y) {
        h = 60.0 * ((c.z - c.x) / d + 2.0);
    } else {
        h = 60.0 * ((c.x - c.y) / d + 4.0);
    }
    var s = 0.0;
    if (mx > 1e-9) {
        s = d / mx;
    }
    return vec3<f32>(h, s, mx);
}

const HSL_BANDS = array<f32, 8>(0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0);

// `camera_raw::band_weights` (hat functions): the weight of band `b` at hue `h`.
fn hat_weight(h_in: f32, b: u32) -> f32 {
    var h = h_in;
    if (!(h >= 0.0 && h < 360.0)) {
        h = rem_euclid(h, 360.0);
    }
    var k = 7u;
    for (var i = 0u; i < 8u; i++) {
        if (HSL_BANDS[i] <= h) {
            k = i;
        }
    }
    let next = (k + 1u) % 8u;
    let span = max(rem_euclid(HSL_BANDS[next] - HSL_BANDS[k], 360.0), 1e-3);
    let t = clamp((h - HSL_BANDS[k]) / span, 0.0, 1.0);
    var w = 0.0;
    if (b == k) {
        w = 1.0 - t;
    }
    if (b == next) {
        w += t;
    }
    return w;
}

fn bil_tab(off: u32, w: u32, h: u32, x: f32, y: f32) -> f32 {
    let xc = clamp(x, 0.0, f32(w - 1u));
    let yc = clamp(y, 0.0, f32(h - 1u));
    let x0 = u32(floor(xc));
    let y0 = u32(floor(yc));
    let x1 = min(x0 + 1u, w - 1u);
    let y1 = min(y0 + 1u, h - 1u);
    let tx = xc - f32(x0);
    let ty = yc - f32(y0);
    let p00 = tab[off + y0 * w + x0];
    let p10 = tab[off + y0 * w + x1];
    let p01 = tab[off + y1 * w + x0];
    let p11 = tab[off + y1 * w + x1];
    let top = p00 + (p10 - p00) * tx;
    let bot = p01 + (p11 - p01) * tx;
    return top + (bot - top) * ty;
}

fn bil_map(off: u32, w: u32, h: u32, x: f32, y: f32) -> f32 {
    let xc = clamp(x, 0.0, f32(w - 1u));
    let yc = clamp(y, 0.0, f32(h - 1u));
    let x0 = u32(floor(xc));
    let y0 = u32(floor(yc));
    let x1 = min(x0 + 1u, w - 1u);
    let y1 = min(y0 + 1u, h - 1u);
    let tx = xc - f32(x0);
    let ty = yc - f32(y0);
    let p00 = maps[off + y0 * w + x0];
    let p10 = maps[off + y0 * w + x1];
    let p01 = maps[off + y1 * w + x0];
    let p11 = maps[off + y1 * w + x1];
    let top = p00 + (p10 - p00) * tx;
    let bot = p01 + (p11 - p01) * tx;
    return top + (bot - top) * ty;
}

fn bil_pl(off: u32, w: u32, h: u32, x: f32, y: f32) -> f32 {
    let xc = clamp(x, 0.0, f32(w - 1u));
    let yc = clamp(y, 0.0, f32(h - 1u));
    let x0 = u32(floor(xc));
    let y0 = u32(floor(yc));
    let x1 = min(x0 + 1u, w - 1u);
    let y1 = min(y0 + 1u, h - 1u);
    let tx = xc - f32(x0);
    let ty = yc - f32(y0);
    let p00 = pl[off + y0 * w + x0];
    let p10 = pl[off + y0 * w + x1];
    let p01 = pl[off + y1 * w + x0];
    let p11 = pl[off + y1 * w + x1];
    let top = p00 + (p10 - p00) * tx;
    let bot = p01 + (p11 - p01) * tx;
    return top + (bot - top) * ty;
}

// --- Vibrance (`acr::vibrance_at`, `acr::vibrance_of`) ----------------------------------------

fn vibrance_at(level: u32, sv: f32, te: f32, hue: f32, kt: u32, tt: u32, ht: u32) -> vec2<f32> {
    let x = clamp(sv * 10.0 - 0.5, 0.0, 9.0);
    let y = clamp(te * 8.0 - 0.5, 0.0, 7.0);
    let x0 = min(u32(x), 8u);
    let y0 = min(u32(y), 6u);
    let fx = x - f32(x0);
    let fy = y - f32(y0);
    let base = level * 80u;
    let i00 = base + x0 * 8u + y0;
    let i10 = base + (x0 + 1u) * 8u + y0;
    let i01 = i00 + 1u;
    let i11 = i10 + 1u;
    let k = tab[kt + i00] * (1.0 - fx) * (1.0 - fy) + tab[kt + i10] * fx * (1.0 - fy) + tab[kt + i01] * (1.0 - fx) * fy + tab[kt + i11] * fx * fy;
    let t = tab[tt + i00] * (1.0 - fx) * (1.0 - fy) + tab[tt + i10] * fx * (1.0 - fy) + tab[tt + i01] * (1.0 - fx) * fy + tab[tt + i11] * fx * fy;
    let hx = rem_euclid(hue / 30.0 - 0.5, 12.0);
    let h0 = min(u32(hx), 11u);
    let hf = tab[ht + level * 12u + h0] * (1.0 - (hx - f32(h0))) + tab[ht + level * 12u + (h0 + 1u) % 12u] * (hx - f32(h0));
    return vec2<f32>(k * hf, t * hf);
}

fn vibrance_of(pro: vec3<f32>, v: f32, kt: u32, tt: u32, ht: u32, lv: u32) -> vec3<f32> {
    let c = clamp(pro, vec3<f32>(0.0), vec3<f32>(1.0));
    let mx = max(max(c.x, c.y), c.z);
    let mn = min(min(c.x, c.y), c.z);
    if (mx <= 1e-6 || mx - mn <= 1e-6) {
        return pro;
    }
    let sv = (mx - mn) / mx;
    let te = senc(mx);
    let hue = hsv(c).x;
    let a = abs(v) * 100.0;
    var first = 4u;
    var step = 1i;
    if (v < 0.0) {
        first = 3u;
        step = -1i;
    }
    var lk = 0.0;
    var lt = 0.0;
    var p0 = 0.0;
    var p1 = 0.0;
    var p2 = 0.0;
    for (var n = 0u; n < 4u; n++) {
        let level = u32(i32(first) + step * i32(n));
        let at = abs(tab[lv + level]);
        let kt1 = vibrance_at(level, sv, te, hue, kt, tt, ht);
        if (at >= a || n == 3u) {
            let f = clamp((a - p0) / max(at - p0, 1e-6), 0.0, 1.0);
            lk = p1 + (kt1.x - p1) * f;
            lt = p2 + (kt1.y - p2) * f;
            break;
        }
        p0 = at;
        p1 = kt1.x;
        p2 = kt1.y;
    }
    let k = exp(lk);
    let tr = exp(lt * min(sv / 0.05, 1.0));
    let top = max(max(pro.x, pro.y), pro.z);
    return max(vec3<f32>(top * tr) - (vec3<f32>(top) - pro) * k, vec3<f32>(0.0));
}

// --- Color Mixer (`acr::hsl`) -----------------------------------------------------------------

fn from_hlc(hue: f32, l: f32, c: f32) -> vec3<f32> {
    let hp = rem_euclid(hue, 360.0) / 60.0;
    let x = c * (1.0 - abs((hp - 2.0 * trunc(hp / 2.0)) - 1.0));
    var rgb = vec3<f32>(c, 0.0, x);
    switch (u32(hp)) {
        case 0u: {
            rgb = vec3<f32>(c, x, 0.0);
        }
        case 1u: {
            rgb = vec3<f32>(x, c, 0.0);
        }
        case 2u: {
            rgb = vec3<f32>(0.0, c, x);
        }
        case 3u: {
            rgb = vec3<f32>(0.0, x, c);
        }
        case 4u: {
            rgb = vec3<f32>(x, 0.0, c);
        }
        default: {}
    }
    let m = l - c / 2.0;
    return rgb + vec3<f32>(m);
}

fn hsl_lum_at(lt: u32, ct: u32, band: u32, v: f32, l: f32, s_in: f32) -> vec2<f32> {
    let x = clamp(l * 12.0 - 0.5, 0.0, 11.0);
    let x0 = min(u32(x), 10u);
    let fx = x - f32(x0);
    let s = clamp(s_in, 0.0, 0.95);
    var y0 = 0u;
    var fy = 0.0;
    if (s < 0.05) {
        y0 = 0u;
        fy = s / 0.05;
    } else {
        let y = s * 10.0 + 0.5;
        y0 = min(u32(y), 9u);
        fy = y - f32(y0);
    }
    var half = 2u;
    var full = 3u;
    if (v < 0.0) {
        half = 1u;
        full = 0u;
    }
    let a = abs(v);
    // [band][level][12][11]
    let hb = (band * 4u + half) * 132u;
    let fb = (band * 4u + full) * 132u;
    let i00 = x0 * 11u + y0;
    let i10 = (x0 + 1u) * 11u + y0;
    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;
    let l50 = tab[lt + hb + i00] * w00 + tab[lt + hb + i10] * w10 + tab[lt + hb + i00 + 1u] * w01 + tab[lt + hb + i10 + 1u] * w11;
    let c50 = tab[ct + hb + i00] * w00 + tab[ct + hb + i10] * w10 + tab[ct + hb + i00 + 1u] * w01 + tab[ct + hb + i10 + 1u] * w11;
    if (a <= 0.5) {
        let f = a / 0.5;
        return vec2<f32>(l50 * f, c50 * f);
    }
    let f = (a - 0.5) / 0.5;
    let l100 = tab[lt + fb + i00] * w00 + tab[lt + fb + i10] * w10 + tab[lt + fb + i00 + 1u] * w01 + tab[lt + fb + i10 + 1u] * w11;
    let c100 = tab[ct + fb + i00] * w00 + tab[ct + fb + i10] * w10 + tab[ct + fb + i00 + 1u] * w01 + tab[ct + fb + i10 + 1u] * w11;
    return vec2<f32>(l50 + (l100 - l50) * f, c50 + (c100 - c50) * f);
}

fn op_hsl(q: vec3<f32>, pc: u32) -> vec3<f32> {
    let wt = arg(pc + 25u);
    let shift = arg(pc + 26u);
    let gain = arg(pc + 27u);
    let lt = arg(pc + 28u);
    let ct = arg(pc + 29u);
    let lin = dec3(q);
    let pro = clamp(to_pro(lin), vec3<f32>(0.0), vec3<f32>(1.0));
    let mx = max(max(pro.x, pro.y), pro.z);
    let mn = min(min(pro.x, pro.y), pro.z);
    let l = (mx + mn) / 2.0;
    let c = mx - mn;
    if (c <= 1e-6) {
        return q;
    }
    let h = hsv(pro).x;
    // `acr::band_weights`: the measured table at 5°.
    var hx = 0.0;
    hx = rem_euclid(h, 360.0) / 5.0;
    let k0 = min(u32(hx), 71u);
    let k1 = (k0 + 1u) % 72u;
    let t = hx - f32(k0);
    var dh = 0.0;
    var ks = 1.0;
    var dl = 0.0;
    var dc = 0.0;
    let rel = c / max(2.0 * min(l, 1.0 - l), 1e-9);
    for (var b = 0u; b < 8u; b++) {
        let w = tab[wt + b * 72u + k0] * (1.0 - t) + tab[wt + b * 72u + k1] * t;
        if (w <= 0.0) {
            continue;
        }
        let hue = argf(pc + 1u + b);
        let sat = argf(pc + 9u + b);
        let lum = argf(pc + 17u + b);
        if (hue != 0.0) {
            dh += w * tab[shift + b * 2u + select(0u, 1u, hue > 0.0)] * abs(hue);
        }
        if (sat != 0.0) {
            if (sat < 0.0) {
                ks += w * sat;
            } else {
                ks += w * (tab[gain + b] * sat);
            }
        }
        if (lum != 0.0) {
            let lc = hsl_lum_at(lt, ct, b, lum, l, rel);
            dl += w * lc.x;
            dc += w * lc.y;
        }
    }
    let ks2 = max(ks, 0.0);
    let lr = exp(dl);
    let cr = exp(dc);
    var l2 = l * lr;
    var c2 = c * ks2 * cr;
    if (ks2 > 1.0) {
        let top = l2 + c * cr / 2.0;
        c2 = min(c2, top);
        l2 = top - c2 / 2.0;
    }
    c2 = min(c2, 2.0 * min(clamp(l2, 0.0, 1.0), 1.0 - clamp(l2, 0.0, 1.0)));
    let out = max(from_hlc(h + dh, l2, c2), vec3<f32>(0.0));
    let top = max(max(max(1.0, lin.x), lin.y), lin.z);
    let back = from_pro(out);
    return enc3(clamp(back, vec3<f32>(0.0), vec3<f32>(top)));
}

// --- Color Grading (`acr::grade`) -------------------------------------------------------------

fn sample65(off: u32, x: f32) -> f32 {
    let p = clamp(x, 0.0, 1.0) * 64.0;
    let i = min(u32(p), 63u);
    let a = tab[off + i];
    return a + (tab[off + i + 1u] - a) * (p - f32(i));
}

fn op_grade(q: vec3<f32>, pc: u32) -> vec3<f32> {
    let lut = arg(pc + 1u);
    let past = argf(pc + 2u);
    let n = arg(pc + 3u);
    let lin = dec3(q);
    var pro = to_pro(lin);
    let level = senc(clamp(dot(YR, pro), 0.0, 1.0));
    if (lut != NONE) {
        pro = hue_preserve(pro, lut, past);
    }
    for (var k = 0u; k < n; k++) {
        let o = pc + 4u + k * 7u;
        let strength = argf(o);
        let dir = vec3<f32>(argf(o + 1u), argf(o + 2u), argf(o + 3u));
        let mask = arg(o + 4u);
        let add = argf(o + 5u);
        let keep = arg(o + 6u);
        let a = strength * sample65(mask, level);
        var g = exp(a * dir);
        if (keep != 0u) {
            let nn = YR.x * g.x + YR.y * g.y + YR.z * g.z;
            if (nn > 1e-6) {
                g = g / nn;
            }
        }
        let base = max(YR.x * pro.x + YR.y * pro.y + YR.z * pro.z, 0.0);
        pro = max(pro * g * (1.0 - add) + (pro + base * (g - vec3<f32>(1.0))) * add, vec3<f32>(0.0));
    }
    return enc3(from_pro(pro));
}

// --- Effects (`acr::VignetteMask`, `acr::VignetteTone`, `vignette`, grain) --------------------

fn vignette_mask(pc: u32, dx: f32, dy: f32) -> f32 {
    let ax = argf(pc + 4u);
    let ay = argf(pc + 5u);
    let circle = argf(pc + 6u);
    let round = argf(pc + 7u);
    let power = argf(pc + 8u);
    let centre = argf(pc + 9u);
    let width = argf(pc + 10u);
    let nx = abs(dx / ax);
    let ny = abs(dy / ay);
    var ellipse = 0.0;
    if (power == 2.0) {
        ellipse = sqrt(nx * nx + ny * ny);
    } else {
        ellipse = powp(powp(nx, power) + powp(ny, power), 1.0 / power);
    }
    let r = ellipse + (sqrt(dx * dx + dy * dy) / circle - ellipse) * round;
    let l = log2(max(r * r, 1e-6));
    let m = 1.0 / (1.0 + exp(-(l - centre) / width));
    if (m >= 0.0 && m <= 1.0) {
        return m;
    }
    return 0.0;
}

fn vignette_tone_at(pc: u32, amount: f32, v: f32) -> f32 {
    let rows = arg(pc + 13u);
    if (rows < 2u) {
        return v;
    }
    var i = rows - 2u;
    for (var k = 0u; k + 1u < rows; k++) {
        if (amount <= argf(pc + 14u + (k + 1u) * 3u)) {
            i = k;
            break;
        }
    }
    let ra = pc + 14u + i * 3u;
    let rb = ra + 3u;
    let a0 = argf(ra);
    let b0 = argf(rb);
    let t = clamp((amount - a0) / max(b0 - a0, 1e-6), 0.0, 1.0);
    return light(arg(ra + 1u), argf(ra + 2u), v) * (1.0 - t) + light(arg(rb + 1u), argf(rb + 2u), v) * t;
}

fn op_vignette(q: vec3<f32>, pc: u32, fx: f32, fy: f32) -> vec3<f32> {
    let style = arg(pc + 1u);
    let amount = argf(pc + 2u);
    let hl = argf(pc + 3u);
    let cx = argf(pc + 11u);
    let cy = argf(pc + 12u);
    let amt = amount / 100.0;
    let m = vignette_mask(pc, fx + 0.5 - cx, fy + 0.5 - cy);
    if (m <= 1e-4) {
        return q;
    }
    let l = clamp(luma(q), 0.0, 1.0);
    if (style == 2u) {
        var goal = 1.0;
        if (amt < 0.0) {
            goal = 0.0;
        }
        let k = m * abs(amt);
        return q + (vec3<f32>(goal) - q) * k;
    }
    if (style == 0u) {
        var protect = 0.0;
        if (amt < 0.0) {
            protect = smoothstep_(0.6, 1.0, l) * hl;
        }
        let a = amount * m * (1.0 - protect);
        let pro = to_pro(dec3(q));
        let mx = max(max(pro.x, pro.y), pro.z);
        let mn = min(min(pro.x, pro.y), pro.z);
        let fmx = vignette_tone_at(pc, a, mx);
        let fmn = vignette_tone_at(pc, a, mn);
        var out = vec3<f32>(fmx);
        if (mx - mn > 1e-9) {
            out = vec3<f32>(fmn) + (fmx - fmn) * (pro - vec3<f32>(mn)) / (mx - mn);
        }
        return enc3(from_pro(out));
    }
    let nl = l * exp2(amt * 2.0 * m);
    return q + vec3<f32>(nl - l);
}

// 64-bit unsigned arithmetic for `photo_util::hash01` (low, high words).
fn mul32(a: u32, b: u32) -> vec2<u32> {
    let a0 = a & 0xffffu;
    let a1 = a >> 16u;
    let b0 = b & 0xffffu;
    let b1 = b >> 16u;
    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let p11 = a1 * b1;
    let mid = (p00 >> 16u) + (p01 & 0xffffu) + (p10 & 0xffffu);
    let lo = (p00 & 0xffffu) | (mid << 16u);
    let hi = p11 + (p01 >> 16u) + (p10 >> 16u) + (mid >> 16u);
    return vec2<u32>(lo, hi);
}
fn mul64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let p = mul32(a.x, b.x);
    return vec2<u32>(p.x, p.y + a.x * b.y + a.y * b.x);
}
fn shr64(a: vec2<u32>, s: u32) -> vec2<u32> {
    if (s >= 32u) {
        return vec2<u32>(a.y >> (s - 32u), 0u);
    }
    return vec2<u32>((a.x >> s) | (a.y << (32u - s)), a.y >> s);
}
fn i64of(v: i32) -> vec2<u32> {
    return vec2<u32>(bitcast<u32>(v), select(0u, 0xffffffffu, v < 0));
}

fn hash01(x: i32, y: i32, seed: vec2<u32>) -> f32 {
    var z = mul64(i64of(x), vec2<u32>(0x7F4A7C15u, 0x9E3779B9u)) ^ mul64(i64of(y), vec2<u32>(0x27D4EB4Fu, 0xC2B2AE3Du)) ^ mul64(seed, vec2<u32>(0x9E3779F9u, 0x165667B1u));
    z = mul64(z ^ shr64(z, 30u), vec2<u32>(0x1CE4E5B9u, 0xBF58476Du));
    z = mul64(z ^ shr64(z, 27u), vec2<u32>(0x133111EBu, 0x94D049BBu));
    z = z ^ shr64(z, 31u);
    return f32(z.y >> 8u) / 16777216.0;
}

fn value_noise(x: f32, y: f32, seed: vec2<u32>) -> f32 {
    let xi = floor(x);
    let yi = floor(y);
    let tx = x - xi;
    let ty = y - yi;
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sy = ty * ty * (3.0 - 2.0 * ty);
    let ix = i32(xi);
    let iy = i32(yi);
    let h00 = hash01(ix, iy, seed);
    let h10 = hash01(ix + 1, iy, seed);
    let h01 = hash01(ix, iy + 1, seed);
    let h11 = hash01(ix + 1, iy + 1, seed);
    let top = h00 + (h10 - h00) * sx;
    let bot = h01 + (h11 - h01) * sx;
    return top + (bot - top) * sy;
}

// --- The pixel interpreter --------------------------------------------------------------------

@compute @workgroup_size(16, 16)
fn pixel_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= P.w || gid.y >= P.h) {
        return;
    }
    load_header();
    let i = gid.y * P.w + gid.x;
    let fx = f32(P.x0 + i32(gid.x));
    let fy = f32(P.y0 + i32(gid.y));
    var px = img[i];
    var q = px.xyz;
    var saved = q;
    var pc = P.start;
    for (var guard = 0u; guard < 256u; guard++) {
        let code = prog[pc];
        if (code == OP_END) {
            break;
        }
        switch (code) {
            case OP_CALIBRATE: {
                var lin = dec3(q);
                if (arg(pc + 1u) != 0u) {
                    let m = pc + 2u;
                    lin = vec3<f32>(
                        argf(m) * lin.x + argf(m + 1u) * lin.y + argf(m + 2u) * lin.z,
                        argf(m + 3u) * lin.x + argf(m + 4u) * lin.y + argf(m + 5u) * lin.z,
                        argf(m + 6u) * lin.x + argf(m + 7u) * lin.y + argf(m + 8u) * lin.z,
                    );
                }
                let tint = argf(pc + 11u);
                if (tint != 0.0) {
                    let shade = 1.0 - smoothstep_(0.0, 0.5, senc(clamp(luma(lin), 0.0, 1.0)));
                    let k = tint * shade;
                    lin = vec3<f32>(lin.x * (1.0 + k * 0.5), lin.y * (1.0 - k), lin.z * (1.0 + k * 0.5));
                }
                q = enc3(max(lin, vec3<f32>(0.0)));
                pc += 12u;
            }
            case OP_WHITE_BALANCE: {
                let g = vec3<f32>(argf(pc + 1u), argf(pc + 2u), argf(pc + 3u));
                let pro = to_pro(dec3(q));
                let x = max(pro, vec3<f32>(0.0));
                let out = vec3<f32>(
                    g.x * x.x / powp(1.0 + (g.x - 1.0) * powp(x.x, 1.164), 0.99),
                    g.y * x.y / powp(1.0 + (g.y - 1.0) * powp(x.y, 1.164), 0.99),
                    g.z * x.z / powp(1.0 + (g.z - 1.0) * powp(x.z, 1.164), 0.99),
                );
                q = enc3(max(from_pro(out), vec3<f32>(0.0)));
                pc += 4u;
            }
            case OP_HUE_PRESERVING: {
                let pro = to_pro(dec3(q));
                q = enc3(from_pro(hue_preserve(pro, arg(pc + 1u), argf(pc + 2u))));
                pc += 3u;
            }
            case OP_PER_CHANNEL: {
                var pro = to_pro(dec3(q));
                if (arg(pc + 1u) != NONE) {
                    pro.x = light(arg(pc + 1u), argf(pc + 2u), pro.x);
                }
                if (arg(pc + 3u) != NONE) {
                    pro.y = light(arg(pc + 3u), argf(pc + 4u), pro.y);
                }
                if (arg(pc + 5u) != NONE) {
                    pro.z = light(arg(pc + 5u), argf(pc + 6u), pro.z);
                }
                q = enc3(from_pro(pro));
                pc += 7u;
            }
            case OP_SAVE: {
                saved = q;
                pc += 1u;
            }
            case OP_REFINE: {
                let refine = argf(pc + 1u);
                let u = saved;
                let c = q;
                let l0 = luma(u);
                let l1 = luma(c);
                var scale = 1.0;
                if (l0 > 1e-5) {
                    scale = l1 / l0;
                }
                let kept = (u - vec3<f32>(l0)) * scale;
                q = vec3<f32>(l1) + kept + (c - vec3<f32>(l1) - kept) * refine;
                pc += 2u;
            }
            case OP_VIBRANCE_SATURATION: {
                let v = argf(pc + 1u);
                let s = argf(pc + 2u);
                var pro = to_pro(dec3(q));
                if (v != 0.0) {
                    pro = vibrance_of(pro, v, arg(pc + 3u), arg(pc + 4u), arg(pc + 5u), arg(pc + 6u));
                }
                if (s != 0.0) {
                    let l = YR.x * pro.x + YR.y * pro.y + YR.z * pro.z;
                    pro = max(vec3<f32>(l) + (pro - vec3<f32>(l)) * (1.0 + s), vec3<f32>(0.0));
                }
                q = enc3(from_pro(pro));
                pc += 7u;
            }
            case OP_HSL: {
                q = op_hsl(q, pc);
                pc += 30u;
            }
            case OP_GRAY_MIX: {
                let l = luma(q);
                let hs = hsv(max(q, vec3<f32>(0.0)));
                var d = 0.0;
                if (hs.y > 1e-4) {
                    for (var b = 0u; b < 8u; b++) {
                        d += hat_weight(hs.x, b) * clamp(argf(pc + 1u + b), -100.0, 100.0);
                    }
                }
                q = vec3<f32>(l * exp2(d / 100.0 * 1.2 * hs.y));
                pc += 9u;
            }
            case OP_GRADE: {
                q = op_grade(q, pc);
                pc += 4u + arg(pc + 3u) * 7u;
            }
            case OP_DEHAZE: {
                let air = vec3<f32>(argf(pc + 1u), argf(pc + 2u), argf(pc + 3u));
                let t = arg(pc + 4u);
                let tw = arg(pc + 5u);
                let th = arg(pc + 6u);
                let sx = argf(pc + 7u);
                let sy = argf(pc + 8u);
                let amount = argf(pc + 9u);
                let tv = clamp(bil_map(t, tw, th, (fx + 0.5) * sx - 0.5, (fy + 0.5) * sy - 0.5), 0.1, 1.0);
                let j = (q - air) / tv + air;
                if (amount >= 0.0) {
                    q = q + (j - q) * amount;
                } else {
                    q = q + (air - q) * (-amount) * 0.5 * (1.0 - tv * 0.5);
                }
                pc += 10u;
            }
            case OP_TONE_MAP: {
                let lin = dec3(q);
                let u = log2(max(luma(lin), 0.0) + 1.0 / 65536.0);
                let mw = arg(pc + 3u);
                let mh = arg(pc + 4u);
                let sx = (fx + 0.5) * argf(pc + 5u) - 0.5;
                let sy = (fy + 0.5) * argf(pc + 6u) - 0.5;
                let a = bil_map(arg(pc + 1u), mw, mh, sx, sy);
                let b = bil_map(arg(pc + 2u), mw, mh, sx, sy);
                q = enc3(lin * exp2(a * u + b));
                pc += 7u;
            }
            case OP_LENS_VIGNETTE: {
                let amount = argf(pc + 1u);
                let start = argf(pc + 2u);
                let dx = fx + 0.5 - argf(pc + 3u);
                let dy = fy + 0.5 - argf(pc + 4u);
                let r = sqrt(dx * dx + dy * dy) / argf(pc + 5u);
                let m = smoothstep_(start, 1.0, r);
                if (m > 0.0) {
                    q = enc3(dec3(q) * exp2(amount * 1.5 * m * m));
                }
                pc += 6u;
            }
            case OP_DEFRINGE: {
                let n = arg(pc + 1u);
                let e = smoothstep_(0.02, 0.12, pl[arg(pc + 2u + n * 3u) + i]);
                if (e > 0.0) {
                    let hs = hsv(max(q, vec3<f32>(0.0)));
                    var k = 0.0;
                    for (var f = 0u; f < n; f++) {
                        let o = pc + 2u + f * 3u;
                        let lo = argf(o + 1u);
                        let hi = argf(o + 2u);
                        k = max(k, argf(o) * smoothstep_(lo - 10.0, lo, hs.x) * (1.0 - smoothstep_(hi, hi + 10.0, hs.x)));
                    }
                    k = k * e * smoothstep_(0.05, 0.25, hs.y);
                    let lum = luma(q);
                    q = vec3<f32>(lum) + (q - vec3<f32>(lum)) * (1.0 - clamp(k, 0.0, 1.0));
                }
                pc += 3u + n * 3u;
            }
            case OP_LUMA: {
                pl[arg(pc + 1u) + i] = luma(q);
                pc += 2u;
            }
            case OP_LUMA_PLANES: {
                let l = max(luma(q), 0.0);
                pl[arg(pc + 1u) + i] = l;
                pl[arg(pc + 2u) + i] = min(l, 1.0);
                pc += 3u;
            }
            case OP_TEXTURE_CLARITY: {
                let l0 = pl[arg(pc + 1u) + i];
                let lc = pl[arg(pc + 2u) + i];
                var lv = lc;
                if (arg(pc + 3u) != NONE) {
                    let e = 2.0 * clamp(lv, 0.0, 1.0) - 1.0;
                    let mw = 1.0 - e * e;
                    lv += argf(pc + 5u) / 100.0 * 0.8 * (lc - pl[arg(pc + 3u) + i]) * mw;
                }
                if (arg(pc + 4u) != NONE) {
                    lv += argf(pc + 6u) / 100.0 * 0.9 * clamp(lc - pl[arg(pc + 4u) + i], -0.1, 0.1);
                }
                lv = max(lv, 0.0) + (l0 - lc);
                if (l0 > 1e-5) {
                    q = q * (lv / l0);
                } else {
                    q = q + vec3<f32>(lv - l0);
                }
                pc += 7u;
            }
            case OP_DETAIL_SPLIT: {
                let y = luma(q);
                pl[arg(pc + 1u) + i] = y;
                pl[arg(pc + 2u) + i] = q.z - y;
                pl[arg(pc + 3u) + i] = q.x - y;
                pc += 4u;
            }
            case OP_NOISE_LUMINANCE: {
                let yi = arg(pc + 1u) + i;
                let s = pl[arg(pc + 2u) + i];
                var wide = 0.0;
                if (arg(pc + 3u) != NONE) {
                    wide = argf(pc + 5u) * 0.6 * (s - pl[arg(pc + 3u) + i]);
                }
                pl[yi] = s + (pl[yi] - s) * argf(pc + 4u) + wide;
                pc += 6u;
            }
            case OP_NOISE_COLOR: {
                let ci = arg(pc + 1u) + i;
                let bc = pl[arg(pc + 2u) + i];
                var base = bc;
                if (arg(pc + 3u) != NONE) {
                    base = bc + (pl[arg(pc + 3u) + i] - bc) * argf(pc + 4u) * 0.5;
                }
                pl[ci] = base + (pl[ci] - bc) * argf(pc + 5u);
                pc += 6u;
            }
            case OP_SHARPEN: {
                let yi = arg(pc + 1u) + i;
                let det = argf(pc + 5u);
                let t = argf(pc + 6u);
                let limit = 0.03 + det * 0.3;
                let hp = clamp(pl[yi] - pl[arg(pc + 2u) + i], -limit, limit) * (0.5 + 0.5 * det);
                var m = 1.0;
                if (arg(pc + 3u) != NONE) {
                    m = smoothstep_(t * 0.5, t + 1e-4, pl[arg(pc + 3u) + i]);
                }
                pl[yi] = pl[yi] + argf(pc + 4u) * hp * m;
                pc += 7u;
            }
            case OP_DETAIL_JOIN: {
                let l = pl[arg(pc + 1u) + i];
                let rr = pl[arg(pc + 3u) + i] + l;
                let bb = pl[arg(pc + 2u) + i] + l;
                let g = (l - 0.2126 * rr - 0.0722 * bb) / 0.7152;
                q = vec3<f32>(rr, g, bb);
                pc += 4u;
            }
            case OP_VIGNETTE: {
                q = op_vignette(q, pc, fx, fy);
                pc += 14u + arg(pc + 13u) * 3u;
            }
            case OP_GRAIN: {
                let cell = argf(pc + 1u);
                let rough = argf(pc + 2u);
                let amt = argf(pc + 3u);
                let seed = vec2<u32>(arg(pc + 4u), 0u);
                let seed7 = vec2<u32>(seed.x + 7u, select(0u, 1u, seed.x > 0xfffffff8u));
                let pscale = argf(pc + 5u);
                let gx = fx * pscale / cell;
                let gy = fy * pscale / cell;
                let n = value_noise(gx, gy, seed) * (1.0 - rough) + (value_noise(gx * 2.3, gy * 2.3, seed7) - 0.0) * rough;
                let l = clamp(luma(q), 0.0, 1.0);
                let g = (n - 0.5) * 2.0 * amt * (0.4 + 2.4 * l * (1.0 - l));
                q = q + vec3<f32>(g);
                pc += 6u;
            }
            case OP_CLAMP: {
                q = clamp(q, vec3<f32>(0.0), vec3<f32>(1.0));
                pc += 1u;
            }
            default: {
                break;
            }
        }
    }
    img[i] = vec4<f32>(q, px.w);
}

// --- Plane passes -----------------------------------------------------------------------------

@compute @workgroup_size(16, 16)
fn plane_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= P.pw || gid.y >= P.ph) {
        return;
    }
    let x = gid.x;
    let y = gid.y;
    let w = P.w;
    let h = P.h;
    switch (P.kind) {
        case PL_BOX_H: {
            let r = i32(P.n);
            var acc = 0.0;
            for (var k = -r; k <= r; k++) {
                acc += pl[P.a + y * w + u32(clamp(i32(x) + k, 0, i32(w) - 1))];
            }
            pl[P.b + y * w + x] = acc / f32(2 * r + 1);
        }
        case PL_BOX_V: {
            let r = i32(P.n);
            var acc = 0.0;
            for (var k = -r; k <= r; k++) {
                acc += pl[P.a + u32(clamp(i32(y) + k, 0, i32(h) - 1)) * w + x];
            }
            pl[P.b + y * w + x] = acc / f32(2 * r + 1);
        }
        case PL_BLOCK: {
            let s = P.n;
            let sw = P.pw;
            var sum = 0.0;
            var count = 0.0;
            for (var yy = y * s; yy < min((y + 1u) * s, h); yy++) {
                for (var xx = x * s; xx < min((x + 1u) * s, w); xx++) {
                    sum += pl[P.a + yy * w + xx];
                    count += 1.0;
                }
            }
            pl[P.b + y * sw + x] = sum / max(count, 1.0);
        }
        case PL_SQUARE: {
            let v = pl[P.a + y * w + x];
            pl[P.b + y * w + x] = v * v;
        }
        case PL_GUIDED_AB: {
            let k = y * w + x;
            let mi = pl[P.a + k];
            let mii = pl[P.b + k];
            let a = (mii - mi * mi) / (mii - mi * mi + P.f);
            pl[P.c + k] = a;
            pl[P.d + k] = mi - a * mi;
        }
        case PL_EVAL: {
            let k = y * w + x;
            let s = P.n;
            var av = 0.0;
            var bv = 0.0;
            if (s == 1u) {
                av = pl[P.a + k];
                bv = pl[P.b + k];
            } else {
                let sw = (w + s - 1u) / s;
                let sh = (h + s - 1u) / s;
                let sf = f32(s);
                let fx = (f32(x) + 0.5) / sf - 0.5;
                let fy = (f32(y) + 0.5) / sf - 0.5;
                av = bil_pl(P.a, sw, sh, fx, fy);
                bv = bil_pl(P.b, sw, sh, fx, fy);
            }
            pl[P.d + k] = av * pl[P.c + k] + bv;
        }
        case PL_GRADIENT: {
            let gx = pl[P.a + y * w + min(x + 1u, w - 1u)] - pl[P.a + y * w + select(x - 1u, 0u, x == 0u)];
            let gy = pl[P.a + min(y + 1u, h - 1u) * w + x] - pl[P.a + select(y - 1u, 0u, y == 0u) * w + x];
            pl[P.b + y * w + x] = sqrt(gx * gx + gy * gy);
        }
        default: {}
    }
}

// --- Manual Distortion (`distortion_source`, `sample_rgba`) ----------------------------------

@compute @workgroup_size(16, 16)
fn distort_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= P.w || gid.y >= P.h) {
        return;
    }
    let w = P.w;
    let h = P.h;
    let fx = f32(P.x0 + i32(gid.x));
    let fy = f32(P.y0 + i32(gid.y));
    let dx = (fx + 0.5 - P.cx) / P.diag;
    let dy = (fy + 0.5 - P.cy) / P.diag;
    let f = 1.0 - P.k * (dx * dx + dy * dy);
    let sx = clamp(P.cx + dx * f * P.diag - 0.5 - f32(P.x0), 0.0, f32(w - 1u));
    let sy = clamp(P.cy + dy * f * P.diag - 0.5 - f32(P.y0), 0.0, f32(h - 1u));
    let x0 = u32(sx);
    let y0 = u32(sy);
    let x1 = min(x0 + 1u, w - 1u);
    let y1 = min(y0 + 1u, h - 1u);
    let tx = sx - f32(x0);
    let ty = sy - f32(y0);
    let a = prev[y0 * w + x0];
    let b = prev[y0 * w + x1];
    let c = prev[y1 * w + x0];
    let d = prev[y1 * w + x1];
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    img[gid.y * w + gid.x] = top + (bottom - top) * ty;
}
