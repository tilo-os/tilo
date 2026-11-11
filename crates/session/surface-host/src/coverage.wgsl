// What a hosted surface's frame shows, read on the GPU where it was drawn: for every pixel of an
// area, whether anything is drawn there (one bit, for the pointer), and for every 8x8 cell, whether
// it is empty, partly drawn or wholly opaque (for what the compositor draws under it). Only these
// leave the GPU; the pixels stay there and are composited from the frame's dma-buf.
//
// A workgroup reads 32x8 pixels: one mask word per row and four cells.

struct Area {
    // The area's top-left pixel: a multiple of 32 across and of 8 down.
    origin: vec2<u32>,
    // Mask words and cells a row of the area holds.
    words: u32,
    cells: u32,
}

@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var<uniform> area: Area;
@group(0) @binding(2) var<storage, read_write> mask: array<u32>;
@group(0) @binding(3) var<storage, read_write> cells: array<u32>;

var<workgroup> rows: array<atomic<u32>, 8>;
var<workgroup> shows: array<atomic<u32>, 4>;
var<workgroup> clear: array<atomic<u32>, 4>;

@compute @workgroup_size(32, 8)
fn main(
    @builtin(local_invocation_id) local: vec3<u32>,
    @builtin(workgroup_id) group: vec3<u32>,
) {
    let pixel = area.origin + group.xy * vec2<u32>(32u, 8u) + local.xy;
    let size = textureDimensions(frame);
    let cell = local.x / 8u;
    if (pixel.x < size.x && pixel.y < size.y) {
        let alpha = textureLoad(frame, vec2<i32>(pixel), 0).a;
        if (alpha > 0.0) {
            atomicOr(&rows[local.y], 1u << local.x);
            atomicOr(&shows[cell], 1u);
        }
        if (alpha < 1.0) {
            atomicOr(&clear[cell], 1u);
        }
    }
    workgroupBarrier();
    if (local.x == 0u) {
        mask[(group.y * 8u + local.y) * area.words + group.x] = atomicLoad(&rows[local.y]);
    }
    if (local.y == 0u && local.x < 4u) {
        // 0: empty, 1: partly drawn, 2: opaque.
        var held = 1u;
        if (atomicLoad(&shows[local.x]) == 0u) {
            held = 0u;
        } else if (atomicLoad(&clear[local.x]) == 0u) {
            held = 2u;
        }
        cells[group.y * area.cells + group.x * 4u + local.x] = held;
    }
}
