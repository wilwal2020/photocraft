// Camera Raw's preview on the GPU, after the develop (`camera_raw.wgsl`): the selection blended
// in, the histogram counted (as `RgbHistogram::from_rgba`), and the screen's bytes written (as
// `CanvasDisplay::rgba8_into` and egui's `Color32::from_rgba_unmultiplied`) into the texture the
// dialog shows.

struct Display {
    w: u32,
    h: u32,
    flags: u32,
    lut_size: u32,
}

const F_ENCODE: u32 = 1u;
const F_LUT: u32 = 2u;
const F_MASK: u32 = 4u;

@group(0) @binding(0) var<uniform> D: Display;
@group(0) @binding(1) var<storage, read> img: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> mask: array<f32>;
@group(0) @binding(4) var<storage, read> codes: array<u32>;
@group(0) @binding(5) var<storage, read> lut: array<f32>;
@group(0) @binding(6) var out: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(7) var<storage, read_write> hist: array<atomic<u32>>;

// Histogram words: 3 × 256 bins, then samples, transparent, invalid, underflow (3), overflow (3),
// shadows (3), highlights (3).
const H_SAMPLES: u32 = 768u;
const H_TRANSPARENT: u32 = 769u;
const H_INVALID: u32 = 770u;
const H_UNDER: u32 = 771u;
const H_OVER: u32 = 774u;
const H_SHADOWS: u32 = 777u;
const H_HIGHLIGHTS: u32 = 780u;
const H_WORDS: u32 = 783u;

var<workgroup> local: array<atomic<u32>, 783>;

fn finite(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

fn count(p: vec4<f32>) {
    if (!finite(p.w)) {
        atomicAdd(&local[H_INVALID], 1u);
        return;
    }
    if (p.w <= 0.0) {
        atomicAdd(&local[H_TRANSPARENT], 1u);
        return;
    }
    if (!(finite(p.x) && finite(p.y) && finite(p.z))) {
        atomicAdd(&local[H_INVALID], 1u);
        return;
    }
    atomicAdd(&local[H_SAMPLES], 1u);
    for (var c = 0u; c < 3u; c++) {
        let v = p[c];
        if (v <= 0.0) {
            atomicAdd(&local[H_SHADOWS + c], 1u);
        }
        if (v >= 1.0) {
            atomicAdd(&local[H_HIGHLIGHTS + c], 1u);
        }
        if (v < 0.0) {
            atomicAdd(&local[H_UNDER + c], 1u);
        } else if (v > 1.0) {
            atomicAdd(&local[H_OVER + c], 1u);
        }
        let bin = u32(clamp(v, 0.0, 1.0) * 255.0 + 0.5);
        atomicAdd(&local[c * 256u + min(bin, 255u)], 1u);
    }
}

fn byte(v: f32) -> u32 {
    return u32(clamp(v, 0.0, 1.0) * 255.0 + 0.5);
}

fn code(v: f32) -> u32 {
    return codes[min(u32(clamp(v, 0.0, 1.0) * 65535.0 + 0.5), 65535u)];
}

// The display transform as a `lut_size`³ table over the bytes (trilinear).
fn transform(rgb: vec3<u32>) -> vec3<u32> {
    let n = D.lut_size;
    let p = vec3<f32>(rgb) / 255.0 * f32(n - 1u);
    let i0 = min(vec3<u32>(floor(p)), vec3<u32>(n - 2u));
    let t = p - vec3<f32>(i0);
    var acc = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k++) {
        let o = vec3<u32>(k & 1u, (k >> 1u) & 1u, (k >> 2u) & 1u);
        let at = i0 + o;
        let wv = select(vec3<f32>(1.0) - t, t, o == vec3<u32>(1u));
        let w = wv.x * wv.y * wv.z;
        let idx = ((at.z * n + at.y) * n + at.x) * 3u;
        acc += w * vec3<f32>(lut[idx], lut[idx + 1u], lut[idx + 2u]);
    }
    return vec3<u32>(clamp(acc * 255.0 + 0.5, vec3<f32>(0.0), vec3<f32>(255.0)));
}

// egui's `mul_frac_round`.
fn mul_frac(v: u32, a: u32) -> u32 {
    let p = v * a + 128u;
    return (p + (p >> 8u)) >> 8u;
}

@compute @workgroup_size(256)
fn display_main(@builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    for (var k = li; k < H_WORDS; k += 256u) {
        atomicStore(&local[k], 0u);
    }
    workgroupBarrier();
    let n = D.w * D.h;
    let stride = nwg.x * 256u;
    for (var i = wg.x * 256u + li; i < n; i += stride) {
        var p = img[i];
        if ((D.flags & F_MASK) != 0u) {
            let o = src[i];
            p = o + (p - o) * mask[i];
        }
        count(p);
        var rgb = vec3<u32>(byte(p.x), byte(p.y), byte(p.z));
        if ((D.flags & F_ENCODE) != 0u) {
            rgb = vec3<u32>(code(p.x), code(p.y), code(p.z));
        }
        let a = byte(p.w);
        if ((D.flags & F_LUT) != 0u) {
            rgb = transform(rgb);
        }
        var o = vec4<u32>(rgb, a);
        if (a == 0u) {
            o = vec4<u32>(0u);
        } else if (a != 255u) {
            o = vec4<u32>(mul_frac(rgb.x, a), mul_frac(rgb.y, a), mul_frac(rgb.z, a), a);
        }
        textureStore(out, vec2<u32>(i % D.w, i / D.w), vec4<f32>(o) / 255.0);
    }
    workgroupBarrier();
    for (var k = li; k < H_WORDS; k += 256u) {
        let v = atomicLoad(&local[k]);
        if (v != 0u) {
            atomicAdd(&hist[k], v);
        }
    }
}
