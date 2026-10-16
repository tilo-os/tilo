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
// The finished accumulation is read back directly; there is no separate
// present pass.

const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);
const MAX_EFFECTS: i32 = 8;

// `sign` returns 0 at 0, which would erase the direction of an axis whose
// scale underflows. This keeps a direction either way.
fn sign_nz(v: f32) -> f32 {
    if (v < 0.0) {
        return -1.0;
    }
    return 1.0;
}

struct Params {
    // x: frame width, y: frame height, z: has_prev, w: has_texture
    frame: vec4<f32>,
    // x: brightness gain, y: contrast gain, z: saturation gain, w: alpha
    grade: vec4<f32>,
    // xyz: representative palette colour, w: effect count
    palette: vec4<f32>,
    // x: source width, y: source height, z: wipe enabled, w: wipe direction
    src: vec4<f32>,
    // x: wipe progress, y: wipe softness, z/w unused
    wipe: vec4<f32>,
    // x: scale x, y: scale y, z: position x, w: position y
    // Position is in frame widths / heights; scale multiplies the
    // aspect-fit size. A negative scale mirrors that axis.
    xform_a: vec4<f32>,
    // x: rotation in radians (clockwise), y/z: anchor in source uv,
    // w: 1 when the source texture's own alpha is meaningful
    xform_b: vec4<f32>,
    // The visible source rect after cropping: x/y left,top — z/w right,bottom
    crop: vec4<f32>,
    // Colour key — x: active, y: tolerance, z: softness, w: spill
    key_a: vec4<f32>,
    // xyz: the key colour in RGB, w: unused
    key_b: vec4<f32>,
    // Shape mask — x: shape (0 off, 1 rect, 2 ellipse),
    // y/z: centre offset from the frame centre, w: rotation in radians
    mask_a: vec4<f32>,
    // x/y: full extent as a fraction of the frame, z: feather, w: invert
    mask_b: vec4<f32>,
    // Tone curve — x: 1 when this layer has a baked table, y: which row of
    // the LUT texture is this layer's. z/w unused.
    lut: vec4<f32>,
    // per effect — x: kind, y: amount
    effects: array<vec4<f32>, 8>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var prev_tex: texture_2d<f32>;
@group(0) @binding(2) var src_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var dst: texture_storage_2d<rgba8unorm, write>;
// One row per layer, 256 entries across. A curve and a 1D LUT are the same
// thing here: whatever filled the row, this reads it the same way.
@group(0) @binding(5) var lut_tex: texture_2d<f32>;

// Matches `preview::Rgb::graded` exactly: brightness scales, contrast
// pivots around mid-grey, saturation lerps toward luma.
fn grade_rgb(c: vec3<f32>, bright: f32, contrast: f32, sat: f32) -> vec3<f32> {
    var x = c * bright;
    x = (x - vec3<f32>(0.5)) * contrast + vec3<f32>(0.5);
    let l = dot(x, LUMA);
    x = vec3<f32>(l) + (x - vec3<f32>(l)) * sat;
    return clamp(x, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Push a colour through this layer's baked tone curve.
//
// The table already has the curve's `mix` folded into it (see
// `curve::Curve::bake`), so there is no blend term here — a curve at half
// strength is a table that is half way to the identity ramp.
//
// Between two entries the lookup interpolates rather than snapping to the
// nearer one: 256 entries over a smooth gradient would otherwise band
// visibly, and the CPU mirror in `Curve::apply` is continuous.
fn apply_lut(c: vec3<f32>) -> vec3<f32> {
    if (p.lut.x < 0.5) {
        return c;
    }
    let row = i32(p.lut.y + 0.5);
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)) * 255.0;
    let lo = vec3<i32>(floor(x));
    let hi = min(lo + vec3<i32>(1), vec3<i32>(255));
    let f = x - floor(x);
    let r = mix(textureLoad(lut_tex, vec2<i32>(lo.x, row), 0).r,
                textureLoad(lut_tex, vec2<i32>(hi.x, row), 0).r, f.x);
    let g = mix(textureLoad(lut_tex, vec2<i32>(lo.y, row), 0).g,
                textureLoad(lut_tex, vec2<i32>(hi.y, row), 0).g, f.y);
    let b = mix(textureLoad(lut_tex, vec2<i32>(lo.z, row), 0).b,
                textureLoad(lut_tex, vec2<i32>(hi.z, row), 0).b, f.z);
    return vec3<f32>(r, g, b);
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

// BT.709 chroma of an RGB triple, each component in -0.5..0.5. Mirrors
// `ChromaKey::chroma` on the host, which is what makes the CPU matte and
// this one the same matte.
fn chroma(c: vec3<f32>) -> vec2<f32> {
    let y = dot(c, LUMA);
    return vec2<f32>((c.b - y) / 1.8556, (c.r - y) / 1.5748);
}

// Chroma normalised by brightness. Plain Cb/Cr scale with exposure, so a
// shadowed patch of one green screen would sit far from a lit patch of the
// same cloth; dividing by luma removes that term and lets one tolerance
// cover the whole screen. Below `DARK_FLOOR` the division stops, because
// the chroma left in near-black is noise and amplifying it would key it.
// Mirrors `ChromaKey::norm_chroma` on the host.
const DARK_FLOOR: f32 = 0.06;

fn norm_chroma(c: vec3<f32>) -> vec2<f32> {
    return chroma(c) / max(dot(c, LUMA), DARK_FLOOR);
}

// How much of `c` survives the colour key: 0 removed, 1 kept. Distance is
// measured in normalised chroma only, so the lit and the shadowed parts of
// one green screen key identically — which an RGB distance cannot do.
fn key_cover(c: vec3<f32>) -> f32 {
    if (p.key_a.x < 0.5) {
        return 1.0;
    }
    let d = length(norm_chroma(c) - norm_chroma(p.key_b.rgb));
    let lo = max(p.key_a.y, 0.0);
    let hi = lo + max(p.key_a.z, 0.0);
    if (d <= lo) {
        return 0.0;
    }
    if (d >= hi) {
        return 1.0;
    }
    let u = (d - lo) / (hi - lo);
    return u * u * (3.0 - 2.0 * u);
}

// Pull the key colour's chroma back out of what survived, leaving its
// brightness alone: the green bounced onto a subject standing near the
// screen goes, and the subject's exposure does not move.
fn despill(c: vec3<f32>) -> vec3<f32> {
    let amount = clamp(p.key_a.w, 0.0, 1.0);
    if (p.key_a.x < 0.5 || amount <= 1e-6) {
        return c;
    }
    let k = chroma(p.key_b.rgb);
    let klen = length(k);
    if (klen < 1e-6) {
        return c;
    }
    let u = k / klen;
    let pc = chroma(c);
    // Only the component pointing *at* the key is spill; chroma leaning
    // the other way is left alone.
    let along = dot(pc, u);
    if (along <= 0.0) {
        return c;
    }
    let q = pc - amount * along * u;
    let y = dot(c, LUMA);
    let r = y + 1.5748 * q.y;
    let b = y + 1.8556 * q.x;
    let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

// How much of the frame point `uv` the shape mask keeps. The mask lives in
// frame space, so it composes with the layer transform rather than being
// carried by it — a layer can move behind a stationary hole.
fn mask_cover(uv: vec2<f32>, fa: f32) -> f32 {
    let shape = i32(p.mask_a.x + 0.5);
    if (shape == 0) {
        return 1.0;
    }
    let d = uv - vec2<f32>(0.5) - p.mask_a.yz;
    // Rotate in aspect-corrected space, or a rotated shape comes out sheared.
    let a = vec2<f32>(d.x * fa, d.y);
    let ang = -p.mask_a.w;
    let ca = cos(ang);
    let sn = sin(ang);
    let r = vec2<f32>(a.x * ca - a.y * sn, a.x * sn + a.y * ca);
    let half_ = vec2<f32>(max(abs(p.mask_b.x) * 0.5 * fa, 1e-6),
                          max(abs(p.mask_b.y) * 0.5, 1e-6));
    let q = r / half_;
    // A feather of zero would make the edge a hard step, which aliases; the
    // floor keeps it one narrow band wide instead.
    let f = clamp(p.mask_b.z, 1e-3, 1.0);
    var inside = 0.0;
    if (shape == 1) {
        inside = (1.0 - smoothstep(1.0 - f, 1.0, abs(q.x)))
               * (1.0 - smoothstep(1.0 - f, 1.0, abs(q.y)));
    } else {
        inside = 1.0 - smoothstep(1.0 - f, 1.0, length(q));
    }
    if (p.mask_b.w > 0.5) {
        return 1.0 - inside;
    }
    return inside;
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
    let fa = f32(w) / f32(h);

    // Aspect-fit ("contain") the source into the project frame. That is the
    // placement at the identity transform, and everything the transform
    // does is relative to it. With no source texture there is no aspect to
    // fit, so the layer's stand-in fill covers the frame — but it is still
    // placed by the transform, so a picture-in-picture reads as one even
    // before its bytes arrive.
    var occupy = vec2<f32>(1.0, 1.0);
    if (p.frame.w > 0.5) {
        let sa = max(p.src.x, 1.0) / max(p.src.y, 1.0);
        if (sa > fa) {
            occupy.y = fa / sa;
        } else {
            occupy.x = sa / fa;
        }
    }

    // Invert the transform to find which source texel lands on this pixel.
    // Forward it reads: move the anchor to the origin, scale, rotate,
    // offset by pos — so undoing it runs the other way.
    var q = uv - vec2<f32>(0.5) - p.xform_a.zw;
    // Rotate in aspect-corrected space, or a square would come out a
    // parallelogram on a non-square frame.
    let ang = -p.xform_b.x;
    let ca = cos(ang);
    let sn = sin(ang);
    let qa = vec2<f32>(q.x * fa, q.y);
    let qr = vec2<f32>(qa.x * ca - qa.y * sn, qa.x * sn + qa.y * ca);
    q = vec2<f32>(qr.x / fa, qr.y);

    // A scale of zero collapses the layer to nothing: there is no texel to
    // sample, so it simply contributes none.
    let denom = occupy * p.xform_a.xy;
    if (abs(denom.x) < 1e-6 || abs(denom.y) < 1e-6) {
        cov = 0.0;
    }
    let fit = q / vec2<f32>(sign_nz(denom.x) * max(abs(denom.x), 1e-6),
                            sign_nz(denom.y) * max(abs(denom.y), 1e-6))
        + p.xform_b.yz;

    // Outside the source, or outside the crop rect, the layer contributes
    // nothing — so lower layers, or the letterbox black, stay visible.
    if (fit.x < p.crop.x || fit.x > p.crop.z || fit.y < p.crop.y || fit.y > p.crop.w) {
        cov = 0.0;
    }

    var col: vec3<f32>;
    if (p.frame.w > 0.5) {
        let texel = textureSampleLevel(
            src_tex, samp, clamp(fit, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);
        col = texel.rgb;
        // Only sources that actually carry alpha (a title, an image with
        // transparency) are cut by it. Decoded video whose alpha channel is
        // left at zero would otherwise vanish entirely.
        if (p.xform_b.w > 0.5) {
            cov = cov * clamp(texel.a, 0.0, 1.0);
        }
    } else {
        col = representative(clamp(fit, vec2<f32>(0.0), vec2<f32>(1.0)), p.palette.rgb);
    }

    // The keyer runs on the source picture, before anything is graded onto
    // it: that is the order a keyer sits in on a real desk, and it is why
    // grading a keyed clip afterwards cannot re-open the matte.
    cov = cov * key_cover(col);
    col = despill(col);

    col = grade_rgb(col, p.grade.x, p.grade.y, p.grade.z);
    // The curve is the fine half of the same grade, so it runs with it —
    // after the three coarse axes and before the look effects, which are
    // applied to a graded picture rather than being part of the grade.
    col = apply_lut(col);
    let n = min(i32(p.palette.w + 0.5), MAX_EFFECTS);
    for (var i = 0; i < n; i = i + 1) {
        col = apply_effect(col, p.effects[i].x, p.effects[i].y, uv);
    }
    // The shape mask is the last thing to cut coverage before the
    // transition's own: it limits where this layer lands in the frame,
    // whatever the layer's picture turned out to be.
    cov = cov * mask_cover(uv, fa);
    if (p.src.z > 0.5) {
        cov = cov * wipe_mask(uv, p.src.w, p.wipe.x, p.wipe.y);
    }

    textureStore(dst, px, vec4<f32>(mix(base, col, cov), 1.0));
}
