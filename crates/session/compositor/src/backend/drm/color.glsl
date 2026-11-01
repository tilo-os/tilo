//_DEFINES
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
uniform sampler2D lut;
uniform float use_lut;
uniform float pq;
uniform float sdr_white;
uniform vec3 filter_row_r;
uniform vec3 filter_row_g;
uniform vec3 filter_row_b;
uniform vec3 filter_offset;
uniform float use_filter;
// The scene as the layers' weight channels show it, and each layer's plane at
// the buffer's precision. A frame carries at most three layers, as many as
// the recorder hands a frame.
uniform sampler2D lit_tex;
uniform sampler2D plane0;
uniform sampler2D plane1;
uniform sampler2D plane2;
uniform float layer_count;
uniform vec2 scene_size;
uniform vec4 layer_row_x[3];
uniform vec4 layer_row_y[3];
uniform vec4 layer_bounds[3];
uniform vec2 layer_size[3];
uniform float layer_channel[3];
uniform float layer_transfer[3];
uniform vec3 layer_row_r[3];
uniform vec3 layer_row_g[3];
uniform vec3 layer_row_b[3];
const float N = 33.0;

vec3 calibrate(vec3 c) {
    float b = clamp(c.b, 0.0, 1.0) * (N - 1.0);
    float b0 = floor(b);
    float b1 = min(b0 + 1.0, N - 1.0);
    vec2 rg = (clamp(c.rg, 0.0, 1.0) * (N - 1.0) + 0.5) / vec2(N * N, N);
    vec3 lo = texture2D(lut, rg + vec2(b0 / N, 0.0)).rgb;
    vec3 hi = texture2D(lut, rg + vec2(b1 / N, 0.0)).rgb;
    return mix(lo, hi, b - b0);
}

float srgb_eotf(float v) {
    float c = clamp(v, 0.0, 1.0);
    return c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4);
}

float pq_eotf(float e) {
    float ep = pow(clamp(e, 0.0, 1.0), 1.0 / 78.84375);
    float num = max(ep - 0.8359375, 0.0);
    float den = 18.851563 - 18.6875 * ep;
    return pow(num / den, 1.0 / 0.15930176) * 10000.0;
}

float pq_inverse_eotf(float nits) {
    float y = pow(clamp(nits / 10000.0, 0.0, 1.0), 0.1593017578125);
    return pow((0.8359375 + 18.8515625 * y) / (1.0 + 18.6875 * y), 78.84375);
}

float hlg_inverse_oetf(float e) {
    float v = clamp(e, 0.0, 1.0);
    return v <= 0.5 ? v * v / 3.0 : (exp((v - 0.5599107) / 0.17883277) + 0.28466892) / 12.0;
}

// A layer's texel decoded from its own transfer into linear BT.2020 cd/m²:
// the same light the headless output builds on the CPU.
vec3 layer_light(vec3 signal, float transfer, vec3 row_r, vec3 row_g, vec3 row_b) {
    vec3 light;
    if (transfer < 0.5) {
        light = vec3(pq_eotf(signal.r), pq_eotf(signal.g), pq_eotf(signal.b));
    } else if (transfer < 1.5) {
        vec3 scene = vec3(hlg_inverse_oetf(signal.r),
                          hlg_inverse_oetf(signal.g),
                          hlg_inverse_oetf(signal.b));
        float luma = max(dot(vec3(0.2627, 0.6780, 0.0593), scene), 0.000001);
        light = scene * (1000.0 * pow(luma, 0.2) * sdr_white / 203.0);
    } else {
        light = signal * sdr_white;
    }
    return max(vec3(dot(row_r, light), dot(row_g, light), dot(row_b, light)), vec3(0.0));
}

float channel_value(vec3 colour, float channel) {
    if (channel < 0.5) return colour.r;
    if (channel < 1.5) return colour.g;
    return colour.b;
}

vec3 plane_texel(int slot, vec2 uv) {
    if (slot == 0) return texture2D(plane0, uv).rgb;
    if (slot == 1) return texture2D(plane1, uv).rgb;
    return texture2D(plane2, uv).rgb;
}

// The whole-desktop colour filter: a matrix and an offset, so turning
// colour off and inverting it are the same one operation.
vec3 colour_filter(vec3 c) {
    return clamp(vec3(dot(filter_row_r, c),
                      dot(filter_row_g, c),
                      dot(filter_row_b, c)) + filter_offset, 0.0, 1.0);
}

void main() {
    vec3 rgb = texture2D(tex, v_coords).rgb;
    if (use_lut > 0.5) {
        rgb = calibrate(rgb);
    }
    if (use_filter > 0.5) {
        rgb = colour_filter(rgb);
    }
    if (pq > 0.5) {
        // How much of each layer reaches this pixel: its channel's difference
        // between the scene with the layers black and this one, where each was
        // drawn in its channel colour.
        vec2 pixel = v_coords * scene_size;
        vec3 lit = layer_count > 0.5 ? texture2D(lit_tex, v_coords).rgb : rgb;
        float covered = 0.0;
        vec3 light = vec3(0.0);
        for (int slot = 0; slot < 3; slot++) {
            if (float(slot) < layer_count) {
                vec4 bounds = layer_bounds[slot];
                if (pixel.x >= bounds.x && pixel.y >= bounds.y
                    && pixel.x < bounds.z && pixel.y < bounds.w) {
                    float weight = channel_value(lit, layer_channel[slot])
                        - channel_value(rgb, layer_channel[slot]);
                    weight = clamp(weight, 0.0, 1.0);
                    if (weight > 0.0) {
                        vec4 row_x = layer_row_x[slot];
                        vec4 row_y = layer_row_y[slot];
                        vec3 at = vec3(pixel, 1.0);
                        vec2 uv = vec2(dot(row_x.xyz, at), dot(row_y.xyz, at)) / layer_size[slot];
                        if (uv.x >= 0.0 && uv.y >= 0.0 && uv.x < 1.0 && uv.y < 1.0) {
                            light = light + weight * layer_light(plane_texel(slot, uv),
                                                                 layer_transfer[slot],
                                                                 layer_row_r[slot],
                                                                 layer_row_g[slot],
                                                                 layer_row_b[slot]);
                            covered = covered + weight;
                        }
                    }
                }
            }
        }
        // What the layers leave is SDR content at SDR white; their own light is
        // added on top of it.
        float coverage = clamp(1.0 - covered, 0.0, 1.0);
        vec3 colour = min(rgb / max(coverage, 0.000001), vec3(1.0));
        vec3 linear = vec3(srgb_eotf(colour.r), srgb_eotf(colour.g), srgb_eotf(colour.b));
        vec3 bt2020 = vec3(
            dot(vec3(0.627404, 0.329283, 0.043313), linear),
            dot(vec3(0.069097, 0.919540, 0.011362), linear),
            dot(vec3(0.016391, 0.088013, 0.895595), linear));
        vec3 nits = bt2020 * (sdr_white * coverage) + light;
        rgb = vec3(pq_inverse_eotf(nits.r), pq_inverse_eotf(nits.g), pq_inverse_eotf(nits.b));
    }
    gl_FragColor = vec4(rgb, 1.0) * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        gl_FragColor = vec4(0.0, 0.2, 0.0, 0.2) + gl_FragColor * 0.8;
#endif
}
