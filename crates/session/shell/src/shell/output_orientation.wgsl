// Output orientation, shared by every final pass that writes a scanout target.
//
// An output's surface holds its part of the scene upright (scene orientation);
// its scanout target holds the physical KMS mode, which is the surface turned
// by the output's transform. Each target pixel reads the surface pixel it
// shows. The GPU mirror of `write_subrect_transformed` in
// `shell/render/paint.rs`, which does the same on the CPU for the readback
// presenter: a pure permutation of pixels, so both produce identical bytes.
//
// `orientation`: 0 normal, 1 rotated 90°, 2 rotated 180°, 3 rotated 270°
// (`shell::render::orientation::shader_code`).

fn oriented_source_pixel(scanout: vec2<i32>, surface: vec2<i32>, orientation: u32) -> vec2<i32> {
    switch orientation {
        case 1u: {
            return vec2<i32>(scanout.y, surface.y - 1 - scanout.x);
        }
        case 2u: {
            return vec2<i32>(surface.x - 1 - scanout.x, surface.y - 1 - scanout.y);
        }
        case 3u: {
            return vec2<i32>(surface.x - 1 - scanout.y, scanout.x);
        }
        default: {
            return scanout;
        }
    }
}
