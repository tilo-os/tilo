/// Snapping moves each edge of the content box by less than one device pixel.
const SNAP_TOLERANCE: f64 = 2.0;

/// The canvas extent in device pixels.
///
/// `device` is `devicePixelContentBoxSize` as physical width and height, `css`
/// the untransformed content box in CSS pixels. The device box is exact and
/// preferred, but Chrome has been measured reporting it in CSS pixels under
/// device-scale-factor emulation (Playwright) while `devicePixelRatio` is 2.
/// Surfaces sized from that render at half resolution and lay out at half
/// the logical width. A device box that disagrees with `css * scale` by more
/// than snapping can explain is not in device pixels.
pub(super) fn physical_size(device: (f64, f64), css: (f64, f64), scale: f64) -> (u32, u32) {
    let expected = (css.0 * scale, css.1 * scale);
    if (device.0 - expected.0).abs() <= SNAP_TOLERANCE
        && (device.1 - expected.1).abs() <= SNAP_TOLERANCE
    {
        (device.0 as u32, device.1 as u32)
    } else {
        (expected.0.round() as u32, expected.1.round() as u32)
    }
}
