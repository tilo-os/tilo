// HDR → SDR tone-map compute shader.
//
// Mirrors the CPU implementation in `hdr_tonemap.rs`:
//   P010 planes → Y'CbCr → RGB → PQ EOTF → BT.2020→709 gamut →
//   Hable tone-map → BT.709 OETF → sRGB output.
//
// Inputs are 10-bit-in-16-bit texel storage (R16Unorm for the Y
// plane, Rg16Unorm for the interleaved UV plane). Output is
// Rgba8UnormSrgb — the same format PeerView's existing blit
// consumes.
//
// The compute grid is 8×8 workgroups; each invocation writes one
// output pixel. Chroma is nearest-neighbour subsampled per the 4:2:0
// layout.
//
// The tone-map constants (`source_peak_nits`, `is_pq`, `is_limited`)
// come in as a uniform so a single kernel handles PQ + HLG + SDR
// fallback without dispatch fan-out.

struct TonemapParams {
    // 1.0 / source_peak_nits — precomputed so the shader avoids a
    // divide per pixel. 0.001 = 1000-nit source; 0.00025 = 4000-nit.
    inv_source_peak: f32,
    // 1 if the source transfer is PQ (SMPTE ST 2084), 0 if HLG, 2 if
    // SDR BT.709 (no PQ/HLG unpack, treat as already linear-ish).
    transfer_kind: u32,
    // 1 if colour range is limited (TV) at 10 bit (64..940 luma), 0
    // if full (0..1023).
    range_kind: u32,
    // Reserved padding to keep the uniform buffer at a std140-friendly
    // 16-byte alignment.
    _pad: u32,
};

@group(0) @binding(0) var y_plane: texture_2d<f32>;
@group(0) @binding(1) var uv_plane: texture_2d<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(3) var<uniform> params: TonemapParams;

fn unpack_luma(raw: f32) -> f32 {
    // R16Unorm normalises 0..0xFFFF → 0..1. P010 stores samples in
    // the TOP 10 bits, so raw already equals `sample / 65535 ≈
    // sample_10bit / 1023`.
    let code = raw * 1023.0;
    if (params.range_kind == 1u) {
        return clamp((code - 64.0) / (940.0 - 64.0), 0.0, 1.0);
    }
    return clamp(code / 1023.0, 0.0, 1.0);
}

fn unpack_chroma(raw: f32) -> f32 {
    let code = raw * 1023.0;
    if (params.range_kind == 1u) {
        return clamp((code - 512.0) / (960.0 - 64.0), -0.5, 0.5);
    }
    return clamp(code / 1023.0 - 0.5, -0.5, 0.5);
}

fn bt2020_ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> vec3<f32> {
    let r = y + 1.4746 * cr;
    let b = y + 1.8814 * cb;
    let g = y - 0.16455312684 * cb - 0.57135312684 * cr;
    return vec3<f32>(r, g, b);
}

fn pq_eotf(e: f32) -> f32 {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let e_p = pow(max(e, 0.0), 1.0 / m2);
    let num = max(e_p - c1, 0.0);
    let den = c2 - c3 * e_p;
    return pow(num / den, 1.0 / m1) * 10000.0;
}

fn hlg_eotf(e: f32) -> f32 {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.55991073;
    let ev = max(e, 0.0);
    var linear: f32;
    if (ev <= 0.5) {
        linear = (ev * ev) / 3.0;
    } else {
        linear = (exp((ev - c) / a) + b) / 12.0;
    }
    return linear * 1000.0;
}

fn bt2020_to_bt709(rgb: vec3<f32>) -> vec3<f32> {
    let r = 1.6605 * rgb.r - 0.5876 * rgb.g - 0.0728 * rgb.b;
    let g = -0.1246 * rgb.r + 1.1329 * rgb.g - 0.0083 * rgb.b;
    let b = -0.0182 * rgb.r - 0.1006 * rgb.g + 1.1187 * rgb.b;
    return max(vec3<f32>(r, g, b), vec3<f32>(0.0));
}

fn hable_curve(v: f32) -> f32 {
    let a = 0.15;
    let b = 0.50;
    let c = 0.10;
    let d = 0.20;
    let e = 0.02;
    let f = 0.30;
    return ((v * (a * v + c * b) + d * e) / (v * (a * v + b) + d * f)) - e / f;
}

fn hable(x: f32) -> f32 {
    let w = 11.2;
    return clamp(hable_curve(x) / hable_curve(w), 0.0, 1.0);
}

fn linear_to_srgb(v: f32) -> f32 {
    let vc = clamp(v, 0.0, 1.0);
    if (vc <= 0.0031308) {
        return vc * 12.92;
    }
    return 1.055 * pow(vc, 1.0 / 2.4) - 0.055;
}

@compute @workgroup_size(8, 8, 1)
fn tonemap_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let px = vec2<i32>(i32(gid.x), i32(gid.y));
    let chroma_px = vec2<i32>(i32(gid.x / 2u), i32(gid.y / 2u));

    let y_raw = textureLoad(y_plane, px, 0).r;
    let uv_raw = textureLoad(uv_plane, chroma_px, 0);
    // For Rg16Unorm the two channels are U and V respectively.
    let y = unpack_luma(y_raw);
    let cb = unpack_chroma(uv_raw.r);
    let cr = unpack_chroma(uv_raw.g);

    let rgb_prime = bt2020_ycbcr_to_rgb(y, cb, cr);
    var rgb_linear: vec3<f32>;
    if (params.transfer_kind == 0u) {
        // PQ.
        rgb_linear = vec3<f32>(
            pq_eotf(rgb_prime.r),
            pq_eotf(rgb_prime.g),
            pq_eotf(rgb_prime.b),
        );
    } else if (params.transfer_kind == 1u) {
        // HLG.
        rgb_linear = vec3<f32>(
            hlg_eotf(rgb_prime.r),
            hlg_eotf(rgb_prime.g),
            hlg_eotf(rgb_prime.b),
        );
    } else {
        rgb_linear = rgb_prime;
    }

    let rgb_709 = bt2020_to_bt709(rgb_linear);
    let scaled = rgb_709 * params.inv_source_peak;
    let compressed = vec3<f32>(hable(scaled.r), hable(scaled.g), hable(scaled.b));
    let encoded = vec3<f32>(
        linear_to_srgb(compressed.r),
        linear_to_srgb(compressed.g),
        linear_to_srgb(compressed.b),
    );
    textureStore(output, px, vec4<f32>(encoded, 1.0));
}
