// Copyright 2023 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT OR Unlicense

// Write path segments

#import bump
#import config
#import segment
#import tile

@group(0) @binding(0)
var<storage, read_write> bump: BumpAllocators;

@group(0) @binding(1)
var<storage> seg_counts: array<SegmentCount>;

@group(0) @binding(2)
var<storage> lines: array<LineSoup>;

@group(0) @binding(3)
var<storage> paths: array<Path>;

@group(0) @binding(4)
var<storage> tiles: array<Tile>;

@group(0) @binding(5)
var<storage, read_write> segments: array<Segment>;

#ifdef direct_dispatch
@group(0) @binding(6)
var<storage, read_write> ptcl: array<u32>;
#endif

fn span(a: f32, b: f32) -> u32 {
    return u32(max(ceil(max(a, b)) - floor(min(a, b)), 1.0));
}

// See cpu_shaders/util.rs for explanation of these.
const ONE_MINUS_ULP: f32 = 0.99999994;
const ROBUST_EPSILON: f32 = 2e-7;

#ifdef segment_row_mask
// The fine Area equations are the oracle: this is only a conservative
// nonzero-row superset after clipping, robustness, and orientation handling.
// It intentionally uses interval masks rather than a 16-row producer loop.
fn row_interval_mask(first: i32, last: i32) -> u32 {
    if last < 0 || first >= i32(TILE_HEIGHT) {
        return 0u;
    }
    let lo = u32(max(first, 0));
    let hi = u32(min(last, i32(TILE_HEIGHT) - 1));
    let upper = (1u << (hi + 1u)) - 1u;
    let lower = (1u << lo) - 1u;
    return upper & ~lower;
}

fn conservative_row_mask(p0: vec2<f32>, p1: vec2<f32>, y_edge: f32) -> u32 {
    // Ordered comparisons reject NaN as well as infinities. Coordinates are
    // final tile-local values, so any unexpected range is uncertainty.
    if !(p0.x >= 0.0 && p0.x <= f32(TILE_WIDTH)
        && p0.y >= 0.0 && p0.y <= f32(TILE_HEIGHT)
        && p1.x >= 0.0 && p1.x <= f32(TILE_WIDTH)
        && p1.y >= 0.0 && p1.y <= f32(TILE_HEIGHT)) {
        return 0xffffu;
    }
    let sentinel_edge = y_edge == 1e9;
    if !sentinel_edge && !(y_edge >= 0.0 && y_edge <= f32(TILE_HEIGHT)) {
        return 0xffffu;
    }
    let dy_mask = row_interval_mask(
        i32(floor(min(p0.y, p1.y))) - 1,
        i32(ceil(max(p0.y, p1.y))),
    );
    // Preserve WGSL signed-zero behavior: only an exact nonzero final dx can
    // have an edge contribution. The sentinel is exactly known to contribute
    // zero in native rows 0..15.
    if p1.x - p0.x == 0.0 || sentinel_edge {
        return dy_mask;
    }
    return dy_mask | row_interval_mask(i32(floor(y_edge)) - 1, i32(TILE_HEIGHT) - 1);
}
#endif

// One invocation for each tile that is to be written.
// Total number of invocations = bump.seg_counts
@compute @workgroup_size(256)
fn main(
    @builtin(global_invocation_id) global_id: vec3<u32>,
#ifdef direct_dispatch
    @builtin(num_workgroups) num_workgroups: vec3<u32>,
#endif
) {
#ifdef direct_dispatch
    // This dispatch is always non-empty. Preserve path_tiling_setup's fine
    // failure signal even when there are no segments to iterate.
    if atomicLoad(&bump.failed) != 0u {
        if global_id.x == 0u {
            ptcl[0] = ~0u;
        }
        return;
    }
#endif
    let n_segments = atomicLoad(&bump.seg_counts);
#ifdef direct_dispatch
    let dispatch_stride = num_workgroups.x * 256u;
    for (var seg_ix = global_id.x; seg_ix < n_segments; seg_ix += dispatch_stride) {
        let seg_count = seg_counts[seg_ix];
#else
    if global_id.x < n_segments {
        let seg_count = seg_counts[global_id.x];
#endif
        let line = lines[seg_count.line_ix];
        let counts = seg_count.counts;
        let seg_within_slice = counts >> 16u;
        let seg_within_line = counts & 0xffffu;

        // coarse rasterization logic
        let is_down = line.p1.y >= line.p0.y;
        var xy0 = select(line.p1, line.p0, is_down);
        var xy1 = select(line.p0, line.p1, is_down);
        let s0 = xy0 * TILE_SCALE;
        let s1 = xy1 * TILE_SCALE;
        let count_x = span(s0.x, s1.x) - 1u;
        let count = count_x + span(s0.y, s1.y);
        let dx = abs(s1.x - s0.x);
        let dy = s1.y - s0.y;
        // Division by zero can't happen because zero-length lines
        // have already been discarded in the path_count stage.
        let idxdy = 1.0 / (dx + dy);
        var a = dx * idxdy;
        let is_positive_slope = s1.x >= s0.x;
        let x_sign = select(-1.0, 1.0, is_positive_slope);
        let xt0 = floor(s0.x * x_sign);
        let c = s0.x * x_sign - xt0;
        let y0i = floor(s0.y);
        let ytop = select(y0i + 1.0, ceil(s0.y), s0.y == s1.y);
        let b = min((dy * c + dx * (ytop - s0.y)) * idxdy, ONE_MINUS_ULP);
        let robust_err = floor(a * (f32(count) - 1.0) + b) - f32(count_x);
        if robust_err != 0.0 {
            a -= ROBUST_EPSILON * sign(robust_err);
        }
        let x0i = i32(xt0 * x_sign + 0.5 * (x_sign - 1.0));
        let z = floor(a * f32(seg_within_line) + b);
        let x = x0i + i32(x_sign * z);
        let y = i32(y0i + f32(seg_within_line) - z);

        let path = paths[line.path_ix];
        let bbox = vec4<i32>(path.bbox);
        let stride = bbox.z - bbox.x;
        let tile_ix = i32(path.tiles) + (y - bbox.y) * stride + x - bbox.x;
        let tile = tiles[tile_ix];
        let seg_start = ~tile.segment_count_or_ix;
        if i32(seg_start) < 0 {
#ifdef direct_dispatch
            continue;
#else
            return;
#endif
        }
        let tile_xy = vec2(f32(x) * f32(TILE_WIDTH), f32(y) * f32(TILE_HEIGHT));
        let tile_xy1 = tile_xy + vec2(f32(TILE_WIDTH), f32(TILE_HEIGHT));

        if seg_within_line > 0u {
            let z_prev = floor(a * (f32(seg_within_line) - 1.0) + b);
            if z == z_prev {
                // Top edge is clipped
                var xt = xy0.x + (xy1.x - xy0.x) * (tile_xy.y - xy0.y) / (xy1.y - xy0.y);
                // TODO: we want to switch to tile-relative coordinates
                xt = clamp(xt, tile_xy.x + 1e-3, tile_xy1.x);
                xy0 = vec2(xt, tile_xy.y);
            } else {
                // If is_positive_slope, left edge is clipped, otherwise right
                let x_clip = select(tile_xy1.x, tile_xy.x, is_positive_slope);
                var yt = xy0.y + (xy1.y - xy0.y) * (x_clip - xy0.x) / (xy1.x - xy0.x);
                yt = clamp(yt, tile_xy.y + 1e-3, tile_xy1.y);
                xy0 = vec2(x_clip, yt);
            }
        }
        if seg_within_line < count - 1u {
            let z_next = floor(a * (f32(seg_within_line) + 1.0) + b);
            if z == z_next {
                // Bottom edge is clipped
                var xt = xy0.x + (xy1.x - xy0.x) * (tile_xy1.y - xy0.y) / (xy1.y - xy0.y);
                xt = clamp(xt, tile_xy.x + 1e-3, tile_xy1.x);
                xy1 = vec2(xt, tile_xy1.y);
            } else {
                // If is_positive_slope, right edge is clipped, otherwise left
                let x_clip = select(tile_xy.x, tile_xy1.x, is_positive_slope);
                var yt = xy0.y + (xy1.y - xy0.y) * (x_clip - xy0.x) / (xy1.x - xy0.x);
                yt = clamp(yt, tile_xy.y + 1e-3, tile_xy1.y);
                xy1 = vec2(x_clip, yt);
            }
        }
        var y_edge = 1e9;
        // Apply numerical robustness logic
        var p0 = xy0 - tile_xy;
        var p1 = xy1 - tile_xy;
        // When we move to f16, this will be f16::MIN_POSITIVE
        let EPSILON = 1e-6;
        if p0.x == 0.0 {
            if p1.x == 0.0 {
                p0.x = EPSILON;
                if p0.y == 0.0 {
                    // Entire tile
                    p1.x = EPSILON;
                    p1.y = f32(TILE_HEIGHT);
                } else {
                    // Make segment disappear
                    p1.x = 2.0 * EPSILON;
                    p1.y = p0.y;
                }
            } else if p0.y == 0.0 {
                p0.x = EPSILON;
            } else {
                y_edge = p0.y;
            }
        } else if p1.x == 0.0 {
            if p1.y == 0.0 {
                p1.x = EPSILON;
            } else {
                y_edge = p1.y;
            }
        }
        // Hacky approach to numerical robustness in fine.
        // This just makes sure there are no vertical lines aligned to
        // the pixel grid internal to the tile. It's faster to do this
        // logic here rather than in fine, but at some point we might
        // rework it.
        if p0.x == floor(p0.x) && p0.x != 0.0 {
            p0.x -= EPSILON;
        }
        if p1.x == floor(p1.x) && p1.x != 0.0 {
            p1.x -= EPSILON;
        }
        if !is_down {
            let tmp = p0;
            p0 = p1;
            p1 = tmp;
        }
#ifdef segment_row_mask
        let segment = Segment(p0, p1, y_edge, conservative_row_mask(p0, p1, y_edge));
#else
        let segment = Segment(p0, p1, y_edge);
#endif
        segments[seg_start + seg_within_slice] = segment;
    }
}
