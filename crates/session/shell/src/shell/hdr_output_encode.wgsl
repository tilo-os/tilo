// SDR → HDR output encode.
//
// The GPU mirror of `color_pipeline::map_sdr_pixel_to_pq_bt2020_at`. When an
// output is driven in HDR PQ mode vello still renders the desktop into its
// 8-bit sRGB target; this pass lifts each pixel into the output's PQ BT.2020
// signal, placing SDR white at the user's SDR content brightness, so SDR UI
// sits correctly beside HDR content instead of being stretched to the peak:
//
//   sRGB8 -> linear BT.709 -> BT.709->BT.2020 gamut -> x SDR white (cd/m^2)
//          -> PQ (ST 2084) inverse EOTF -> 10-bit or half-float scanout
//
// A render pass rather than a compute pass: the scanout target is an
// imported dma-buf with colour-attachment usage only. One triangle covers the
// target; each fragment reads the source pixel under it.

struct EncodeParams {
    // Luminance SDR white (sRGB 1.0) maps to; 203 is the BT.2408 reference.
    sdr_white_nits: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var sdr_input: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: EncodeParams;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

fn srgb_eotf(v: f32) -> f32 {
    let vc = clamp(v, 0.0, 1.0);
    if (vc <= 0.04045) {
        return vc / 12.92;
    }
    return pow((vc + 0.055) / 1.055, 2.4);
}

// Linear BT.709 -> linear BT.2020 (the published forward gamut matrix;
// matches the CPU `bt709_to_bt2020_matrix()` derivation).
fn bt709_to_bt2020(rgb: vec3<f32>) -> vec3<f32> {
    let r = 0.627404 * rgb.r + 0.329283 * rgb.g + 0.043313 * rgb.b;
    let g = 0.069097 * rgb.r + 0.919540 * rgb.g + 0.011362 * rgb.b;
    let b = 0.016391 * rgb.r + 0.088013 * rgb.g + 0.895595 * rgb.b;
    return vec3<f32>(r, g, b);
}

// Absolute luminance (cd/m^2) -> PQ electrical signal (0..1).
fn pq_inverse_eotf(nits: f32) -> f32 {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let y = clamp(nits / 10000.0, 0.0, 1.0);
    let ym = pow(y, m1);
    return pow((c1 + c2 * ym) / (1.0 + c3 * ym), m2);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.position.xy);
    let src = textureLoad(sdr_input, px, 0);
    let linear = vec3<f32>(srgb_eotf(src.r), srgb_eotf(src.g), srgb_eotf(src.b));
    let nits = bt709_to_bt2020(linear) * params.sdr_white_nits;
    let signal = vec3<f32>(
        pq_inverse_eotf(nits.r),
        pq_inverse_eotf(nits.g),
        pq_inverse_eotf(nits.b),
    );
    return vec4<f32>(signal, 1.0);
}
