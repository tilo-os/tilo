// An output's ICC calibration, shared by the SDR final pass of a calibrated
// output (`shell::render::orientation::OrientedCopy`).
//
// The GPU mirror of the readback presenter's qcms pass (`paint_one_scanout`,
// `qcms::Transform::apply`): `lut` holds that transform sampled on a grid of
// 8-bit sRGB codes (`shell::render::icc_lut::IccLut`), and `coordinates` maps
// each input code 0..=255 to its position on the grid, so the grid can be
// spaced unevenly. Interpolation is tetrahedral and reads texels with
// `textureLoad`, so the result does not depend on the GPU's filter precision.

// `rgb`: an sRGB colour read from an 8-bit target, each channel k/255.
// Returns the panel's colour for it, each channel in 0..1.
fn icc_lut_apply(lut: texture_3d<f32>, coordinates: texture_1d<f32>, rgb: vec3<f32>) -> vec3<f32> {
    let code = clamp(vec3<i32>(round(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * 255.0)), vec3<i32>(0), vec3<i32>(255));
    let position = vec3<f32>(
        textureLoad(coordinates, code.r, 0).r,
        textureLoad(coordinates, code.g, 0).r,
        textureLoad(coordinates, code.b, 0).r,
    );
    let last_cell = vec3<i32>(textureDimensions(lut)) - vec3<i32>(2);
    let cell = clamp(vec3<i32>(floor(position)), vec3<i32>(0), last_cell);
    let f = position - vec3<f32>(cell);
    // The tetrahedron of the cube that holds `f`: its corners run from the
    // cell's origin to its far corner one axis at a time, largest fraction
    // first, and each corner weighs the difference between consecutive
    // fractions.
    var first = vec3<i32>(1, 0, 0);
    var second = vec3<i32>(1, 1, 0);
    var w = vec4<f32>(1.0 - f.x, f.x - f.y, f.y - f.z, f.z);
    if (f.x >= f.y) {
        if (f.y >= f.z) {
            // x >= y >= z: defaults.
        } else if (f.x >= f.z) {
            second = vec3<i32>(1, 0, 1);
            w = vec4<f32>(1.0 - f.x, f.x - f.z, f.z - f.y, f.y);
        } else {
            first = vec3<i32>(0, 0, 1);
            second = vec3<i32>(1, 0, 1);
            w = vec4<f32>(1.0 - f.z, f.z - f.x, f.x - f.y, f.y);
        }
    } else if (f.z >= f.y) {
        first = vec3<i32>(0, 0, 1);
        second = vec3<i32>(0, 1, 1);
        w = vec4<f32>(1.0 - f.z, f.z - f.y, f.y - f.x, f.x);
    } else if (f.z >= f.x) {
        first = vec3<i32>(0, 1, 0);
        second = vec3<i32>(0, 1, 1);
        w = vec4<f32>(1.0 - f.y, f.y - f.z, f.z - f.x, f.x);
    } else {
        first = vec3<i32>(0, 1, 0);
        second = vec3<i32>(1, 1, 0);
        w = vec4<f32>(1.0 - f.y, f.y - f.x, f.x - f.z, f.z);
    }
    let origin = textureLoad(lut, cell, 0).rgb;
    let a = textureLoad(lut, cell + first, 0).rgb;
    let b = textureLoad(lut, cell + second, 0).rgb;
    let far = textureLoad(lut, cell + vec3<i32>(1), 0).rgb;
    return clamp(w.x * origin + w.y * a + w.z * b + w.w * far, vec3<f32>(0.0), vec3<f32>(1.0));
}
