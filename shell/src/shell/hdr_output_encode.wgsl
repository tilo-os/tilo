// SDR → HDR output-encode compute shader.
//
// The GPU mirror of `color_pipeline::map_sdr_pixel_to_pq_bt2020`. When an
// output is driven in HDR PQ mode the compositor renders SDR surfaces into
// an 8-bit sRGB intermediate, then this kernel lifts each pixel into the
// output's PQ BT.2020 signal so SDR UI composits *correctly* alongside HDR
// video instead of being clipped:
//
//   sRGB8 -> linear BT.709 -> x203 nits (BT.2408 reference white)
//          -> BT.709->BT.2020 gamut -> PQ (ST 2084) OETF -> RGBA16F output.
//
// Output is `rgba16float`, the scRGB-ish HDR swapchain format; the encoded
// PQ signal is written straight into it (0..1 electrical). A backend
// targeting `rgb10a2unorm` uses the same maths with a 10-bit store.
//
// Compute grid is 8x8; one invocation per output pixel.

struct EncodeParams {
    // BT.2408 diffuse-white luminance SDR 1.0 maps to (203.0 by default).
    reference_white_nits: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var sdr_input: texture_2d<f32>;
@group(0) @binding(1) var hdr_output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> params: EncodeParams;

fn srgb_eotf(v: f32) -> f32 {
    let vc = clamp(v, 0.0, 1.0);
    if (vc <= 0.04045) {
        return vc / 12.92;
    }
    return pow((vc + 0.055) / 1.055, 2.4);
}

// Linear BT.709 -> linear BT.2020 (published forward gamut matrix; matches
// the CPU `bt709_to_bt2020_matrix()` derivation within transcription
// tolerance).
fn bt709_to_bt2020(rgb: vec3<f32>) -> vec3<f32> {
    let r = 0.627404 * rgb.r + 0.329283 * rgb.g + 0.043313 * rgb.b;
    let g = 0.069097 * rgb.r + 0.919540 * rgb.g + 0.011362 * rgb.b;
    let b = 0.016391 * rgb.r + 0.088013 * rgb.g + 0.895595 * rgb.b;
    return vec3<f32>(r, g, b);
}

// Absolute luminance (cd/m^2) -> PQ electrical signal (0..1). Inverse
// ST 2084 EOTF.
fn pq_inverse_eotf(nits: f32) -> f32 {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let y = clamp(nits / 10000.0, 0.0, 1.0);
    let ym = pow(y, m1);
    let num = c1 + c2 * ym;
    let den = 1.0 + c3 * ym;
    return pow(num / den, m2);
}

@compute @workgroup_size(8, 8, 1)
fn encode_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(hdr_output);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let px = vec2<i32>(i32(gid.x), i32(gid.y));
    let src = textureLoad(sdr_input, px, 0);

    let linear = vec3<f32>(
        srgb_eotf(src.r),
        srgb_eotf(src.g),
        srgb_eotf(src.b),
    );
    let nits = bt709_to_bt2020(linear) * params.reference_white_nits;
    let signal = vec3<f32>(
        pq_inverse_eotf(nits.r),
        pq_inverse_eotf(nits.g),
        pq_inverse_eotf(nits.b),
    );
    textureStore(hdr_output, px, vec4<f32>(signal, 1.0));
}
