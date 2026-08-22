// Timeline frame compositor.
//
// One dispatch of `cs_layer` composites ONE layer of the timeline stack
// over the accumulation buffer: it samples the layer's source picture
// (or synthesises the representative fill when no source resolved),
// applies the colour grade, runs the stackable effect chain, then blends
// the result over what is already there using the layer's coverage —
// uniform alpha times an optional geometric wipe mask.
//
// Layers are dispatched bottom-to-top and the buffers ping-pong, so a
// dissolve is not a special case: the outgoing clip is simply the layer
// underneath, and the incoming clip's alpha does the mixing.
//
// `cs_present` copies the finished accumulation into the caller's target
// at a pixel offset (the preview canvas rect). Export skips it and reads
// the accumulation back directly.

const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);
const MAX_EFFECTS: i32 = 8;

struct Params {
    // x: frame width, y: frame height, z: has_prev, w: has_texture
    frame: vec4<f32>,
    // x: brightness gain, y: contrast gain, z: saturation gain, w: alpha
    grade: vec4<f32>,
    // xyz: representative palette colour, w: effect count
    palette: vec4<f32>,
    // x: source width, y: source height, z: wipe enabled, w: wipe direction
    src: vec4<f32>,
    // x: wipe progress, y: wipe softness, z/w: present-pass destination offset
    wipe: vec4<f32>,
    // per effect — x: kind, y: amount
    effects: array<vec4<f32>, 8>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var prev_tex: texture_2d<f32>;
@group(0) @binding(2) var src_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var dst: texture_storage_2d<rgba8unorm, write>;

// Matches `preview::Rgb::graded` exactly: brightness scales, contrast
// pivots around mid-grey, saturation lerps toward luma.
fn grade_rgb(c: vec3<f32>, bright: f32, contrast: f32, sat: f32) -> vec3<f32> {
    var x = c * bright;
    x = (x - vec3<f32>(0.5)) * contrast + vec3<f32>(0.5);
    let l = dot(x, LUMA);
    x = vec3<f32>(l) + (x - vec3<f32>(l)) * sat;
    return clamp(x, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Matches `preview::apply_effect`, except Vignette — which here is a real
// spatial falloff from the frame centre instead of the flat dim a single
// representative colour was forced into.
fn apply_effect(c: vec3<f32>, kind: f32, amount: f32, uv: vec2<f32>) -> vec3<f32> {
    let a = clamp(amount, 0.0, 1.0);
    let k = i32(kind + 0.5);
    var o = c;
    if (k == 0) {
        let l = dot(c, LUMA);
        o = mix(c, vec3<f32>(l), a);
    } else if (k == 1) {
        let s = vec3<f32>(
            dot(c, vec3<f32>(0.393, 0.769, 0.189)),
            dot(c, vec3<f32>(0.349, 0.686, 0.168)),
            dot(c, vec3<f32>(0.272, 0.534, 0.131)),
        );
        o = mix(c, s, a);
    } else if (k == 2) {
        o = mix(c, vec3<f32>(1.0) - c, a);
    } else if (k == 3) {
        o = c + vec3<f32>(a * 0.16, a * 0.04, -a * 0.16);
    } else if (k == 4) {
        o = c + vec3<f32>(-a * 0.16, a * 0.04, a * 0.16);
    } else if (k == 5) {
        // Distance to the frame centre, normalised so a corner is 1.
        let d = length(uv - vec2<f32>(0.5)) / 0.70710678;
        o = mix(c, vec3<f32>(0.0), a * smoothstep(0.35, 1.0, d));
    }
    return clamp(o, vec3<f32>(0.0), vec3<f32>(1.0));
}

// The deterministic stand-in drawn when a layer has no source texture —
// hermetic screenshots, and references whose bytes have not hydrated.
// Two bands over the clip's palette colour, matching the CPU reference
// in `preview::render_rgba`.
fn representative(uv: vec2<f32>, base: vec3<f32>) -> vec3<f32> {
    var c = mix(base, vec3<f32>(0.0), 0.18);
    if (uv.y < 0.5) {
        c = mix(c, mix(base, vec3<f32>(1.0), 0.18), 0.55);
    }
    if (uv.y >= 0.62 && uv.y < 0.72) {
        c = mix(c, mix(base, vec3<f32>(0.0), 0.15), 0.6);
    }
    return c;
}

// A travelling soft edge. Returns how much of the incoming layer is
// revealed at `uv`; the edge overshoots both ends by the feather width so
// progress 0 hides everything and progress 1 reveals everything.
fn wipe_mask(uv: vec2<f32>, dir: f32, progress: f32, soft: f32) -> f32 {
    let d = i32(dir + 0.5);
    var axis = uv.x;
    if (d == 1) {
        axis = 1.0 - uv.x;
    } else if (d == 2) {
        axis = uv.y;
    } else if (d == 3) {
        axis = 1.0 - uv.y;
    }
    let s = max(soft, 0.0005);
    let edge = progress * (1.0 + 2.0 * s) - s;
    return 1.0 - smoothstep(edge - s, edge + s, axis);
}

@compute @workgroup_size(8, 8)
fn cs_layer(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = u32(p.frame.x);
    let h = u32(p.frame.y);
    if (gid.x >= w || gid.y >= h) {
        return;
    }
    let px = vec2<i32>(i32(gid.x), i32(gid.y));
    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / vec2<f32>(f32(w), f32(h));

    var base = vec3<f32>(0.0, 0.0, 0.0);
    if (p.frame.z > 0.5) {
        base = textureLoad(prev_tex, px, 0).rgb;
    }

    var cov = clamp(p.grade.w, 0.0, 1.0);
    var col: vec3<f32>;
    if (p.frame.w > 0.5) {
        // Aspect-fit ("contain") the source into the project frame.
        // Outside the fitted rect the layer contributes nothing, so lower
        // layers — or the letterbox black — stay visible.
        let sa = max(p.src.x, 1.0) / max(p.src.y, 1.0);
        let fa = f32(w) / f32(h);
        var occupy = vec2<f32>(1.0, 1.0);
        if (sa > fa) {
            occupy.y = fa / sa;
        } else {
            occupy.x = sa / fa;
        }
        let fit = (uv - vec2<f32>(0.5)) / occupy + vec2<f32>(0.5);
        if (fit.x < 0.0 || fit.x > 1.0 || fit.y < 0.0 || fit.y > 1.0) {
            cov = 0.0;
        }
        col = textureSampleLevel(src_tex, samp, clamp(fit, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
    } else {
        col = representative(uv, p.palette.rgb);
    }

    col = grade_rgb(col, p.grade.x, p.grade.y, p.grade.z);
    let n = min(i32(p.palette.w + 0.5), MAX_EFFECTS);
    for (var i = 0; i < n; i = i + 1) {
        col = apply_effect(col, p.effects[i].x, p.effects[i].y, uv);
    }
    if (p.src.z > 0.5) {
        cov = cov * wipe_mask(uv, p.src.w, p.wipe.x, p.wipe.y);
    }

    textureStore(dst, px, vec4<f32>(mix(base, col, cov), 1.0));
}

@compute @workgroup_size(8, 8)
fn cs_present(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = u32(p.frame.x);
    let h = u32(p.frame.y);
    if (gid.x >= w || gid.y >= h) {
        return;
    }
    let c = textureLoad(prev_tex, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    let off = vec2<i32>(i32(p.wipe.z), i32(p.wipe.w));
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)) + off, c);
}
