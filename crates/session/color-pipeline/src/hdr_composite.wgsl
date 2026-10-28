// HDR output composite: the SDR desktop plus HDR layers, in linear light.
//
// The GPU mirror of `hdr_surface::composite_hdr_pixel` for a PQ output.
// Inputs, all at the output's size in scene orientation:
//
//   sdr_input    - vello's render with every HDR layer image black
//   weight_input - the same scene with each layer image in its channel colour
//
// Their difference in a layer's channel is how much of that layer reaches the
// pixel, after clipping, rounded corners, opacity and everything drawn over
// it. Each covering layer's plane is sampled at native precision, decoded
// from its own description (PQ, HLG, linear) into linear BT.2020 cd/m², and
// added at that weight. What the layers leave is SDR content, linearised as
// its own colour and placed at SDR white. The sum is encoded to PQ once.

struct Layer {
    // Scene pixel -> plane texel: x' = dot(row_x.xyz, (x, y, 1)).
    row_x: vec4<f32>,
    row_y: vec4<f32>,
    // Scene pixels the entry may cover: x0, y0, x1, y1.
    bounds: vec4<f32>,
    // Rows of the linear signal-primaries -> BT.2020 matrix.
    to_bt2020_r: vec4<f32>,
    to_bt2020_g: vec4<f32>,
    to_bt2020_b: vec4<f32>,
    // Plane size in texels.
    size: vec2<f32>,
    // 0 red, 1 green, 2 blue: the weight channel, and the plane binding.
    channel: u32,
    // 0 PQ, 1 HLG, 2 linear (scRGB).
    transfer: u32,
};

struct Params {
    sdr_white_nits: f32,
    layer_count: u32,
    // `shell::render::orientation::shader_code` of the output's transform.
    orientation: u32,
    _pad1: u32,
    layers: array<Layer, 4>,
};

@group(0) @binding(0) var sdr_input: texture_2d<f32>;
@group(0) @binding(1) var weight_input: texture_2d<f32>;
@group(0) @binding(2) var plane0: texture_2d<f32>;
@group(0) @binding(3) var plane1: texture_2d<f32>;
@group(0) @binding(4) var plane2: texture_2d<f32>;
@group(0) @binding(5) var plane_sampler: sampler;
@group(0) @binding(6) var<uniform> params: Params;

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

fn bt709_to_bt2020(rgb: vec3<f32>) -> vec3<f32> {
    let r = 0.627404 * rgb.r + 0.329283 * rgb.g + 0.043313 * rgb.b;
    let g = 0.069097 * rgb.r + 0.919540 * rgb.g + 0.011362 * rgb.b;
    let b = 0.016391 * rgb.r + 0.088013 * rgb.g + 0.895595 * rgb.b;
    return vec3<f32>(r, g, b);
}

const PQ_M1: f32 = 0.1593017578125;
const PQ_M2: f32 = 78.84375;
const PQ_C1: f32 = 0.8359375;
const PQ_C2: f32 = 18.8515625;
const PQ_C3: f32 = 18.6875;

fn pq_eotf(e: f32) -> f32 {
    let ep = pow(clamp(e, 0.0, 1.0), 1.0 / PQ_M2);
    let num = max(ep - PQ_C1, 0.0);
    let den = PQ_C2 - PQ_C3 * ep;
    return pow(num / den, 1.0 / PQ_M1) * 10000.0;
}

fn pq_inverse_eotf(nits: f32) -> f32 {
    let y = clamp(nits / 10000.0, 0.0, 1.0);
    let ym = pow(y, PQ_M1);
    return pow((PQ_C1 + PQ_C2 * ym) / (1.0 + PQ_C3 * ym), PQ_M2);
}

fn hlg_inverse_oetf(e: f32) -> f32 {
    let ec = clamp(e, 0.0, 1.0);
    if (ec <= 0.5) {
        return ec * ec / 3.0;
    }
    return (exp((ec - 0.55991073) / 0.17883277) + 0.28466892) / 12.0;
}

fn sample_plane(slot: u32, uv: vec2<f32>) -> vec3<f32> {
    switch slot {
        case 0u: {
            return textureSampleLevel(plane0, plane_sampler, uv, 0.0).rgb;
        }
        case 1u: {
            return textureSampleLevel(plane1, plane_sampler, uv, 0.0).rgb;
        }
        default: {
            return textureSampleLevel(plane2, plane_sampler, uv, 0.0).rgb;
        }
    }
}

// Normalised signal -> linear BT.2020 cd/m², as `signal_to_output_nits`.
fn decode(signal: vec3<f32>, layer: Layer) -> vec3<f32> {
    var light: vec3<f32>;
    switch layer.transfer {
        case 0u: {
            light = vec3<f32>(pq_eotf(signal.r), pq_eotf(signal.g), pq_eotf(signal.b));
        }
        case 1u: {
            let scene = vec3<f32>(
                hlg_inverse_oetf(signal.r),
                hlg_inverse_oetf(signal.g),
                hlg_inverse_oetf(signal.b),
            );
            let luma = max(dot(vec3<f32>(0.2627, 0.6780, 0.0593), scene), 1e-6);
            // BT.2100 OOTF at 1000 cd/m², HLG reference white at SDR white.
            let gain = 1000.0 * pow(luma, 0.2) * (params.sdr_white_nits / 203.0);
            light = scene * gain;
        }
        default: {
            light = signal * params.sdr_white_nits;
        }
    }
    let converted = vec3<f32>(
        dot(layer.to_bt2020_r.xyz, light),
        dot(layer.to_bt2020_g.xyz, light),
        dot(layer.to_bt2020_b.xyz, light),
    );
    return max(converted, vec3<f32>(0.0));
}

fn channel_of(v: vec4<f32>, channel: u32) -> f32 {
    switch channel {
        case 0u: {
            return v.r;
        }
        case 1u: {
            return v.g;
        }
        default: {
            return v.b;
        }
    }
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // The inputs and the layers are in scene orientation; the target is the
    // output's scanout, turned by its orientation (`oriented_source_pixel`,
    // output_orientation.wgsl, concatenated in front of this file).
    let surface = vec2<i32>(textureDimensions(sdr_input));
    let px = oriented_source_pixel(vec2<i32>(in.position.xy), surface, params.orientation);
    let centre = vec2<f32>(px) + vec2<f32>(0.5);
    let first = textureLoad(sdr_input, px, 0);
    let second = textureLoad(weight_input, px, 0);
    var covered = 0.0;
    var hdr = vec3<f32>(0.0);
    let count = min(params.layer_count, 4u);
    for (var i = 0u; i < count; i = i + 1u) {
        let layer = params.layers[i];
        if (centre.x < layer.bounds.x || centre.y < layer.bounds.y
            || centre.x >= layer.bounds.z || centre.y >= layer.bounds.w) {
            continue;
        }
        let weight = clamp(
            channel_of(second, layer.channel) - channel_of(first, layer.channel),
            0.0,
            1.0,
        );
        if (weight <= 0.0) {
            continue;
        }
        let p = vec3<f32>(centre, 1.0);
        let texel = vec2<f32>(dot(layer.row_x.xyz, p), dot(layer.row_y.xyz, p));
        let signal = sample_plane(layer.channel, texel / layer.size);
        hdr = hdr + weight * decode(signal, layer);
        covered = covered + weight;
    }
    let coverage = clamp(1.0 - covered, 0.0, 1.0);
    var sdr = vec3<f32>(0.0);
    if (coverage > 0.0) {
        let colour = min(first.rgb / coverage, vec3<f32>(1.0));
        sdr = coverage * vec3<f32>(srgb_eotf(colour.r), srgb_eotf(colour.g), srgb_eotf(colour.b));
    }
    let nits = bt709_to_bt2020(sdr) * params.sdr_white_nits + hdr;
    return vec4<f32>(
        pq_inverse_eotf(nits.r),
        pq_inverse_eotf(nits.g),
        pq_inverse_eotf(nits.b),
        1.0,
    );
}
