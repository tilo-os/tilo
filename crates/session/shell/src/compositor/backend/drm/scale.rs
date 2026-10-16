//! Shared panel-DPI policy plus compositor surface/output selection.

pub use settings_store::display_scale::{MIN_SCALE, resolve_scale};

/// One output's `(position, size, scale)` in combined coordinate space, as
/// consumed by [`preferred_scale_at`].
pub type OutputGeometry = ((i32, i32), (u32, u32), f64);

/// The scale a surface at global point `(x, y)` should render at, from
/// `(position, size, scale)` triples in combined coordinate space. A surface
/// in a gap between mismatched monitors — or before any output is known —
/// gets the *largest* scale in play: rendering too sharp costs bandwidth,
/// rendering too soft is visible.
pub fn preferred_scale_at(outputs: &[OutputGeometry], x: f64, y: f64) -> f64 {
    for &((ox, oy), (ow, oh), scale) in outputs {
        let (ox, oy) = (f64::from(ox), f64::from(oy));
        if x >= ox && x < ox + f64::from(ow) && y >= oy && y < oy + f64::from(oh) {
            return scale;
        }
    }
    outputs
        .iter()
        .map(|&(_, _, s)| s)
        .fold(f64::NEG_INFINITY, f64::max)
        .max(MIN_SCALE)
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact output scales are the behavior under test"
)]
mod tests {
    use super::*;

    #[test]
    fn surface_scale_follows_the_output_it_is_on() {
        // Phase `a_surface_gets_the_scale_of_the_output_it_is_on`.
        {
            // The laptop panel at 1.5 on the left, an external 1080p monitor
            // at 1.0 to its right. Dragging a window across must change the
            // preferred scale it is told to render at.
            let outputs = [
                ((0, 0), (1920u32, 1080u32), 1.5),
                ((1920, 0), (1920u32, 1080u32), 1.0),
            ];
            assert_eq!(preferred_scale_at(&outputs, 10.0, 10.0), 1.5);
            assert_eq!(preferred_scale_at(&outputs, 1919.0, 500.0), 1.5);
            assert_eq!(preferred_scale_at(&outputs, 1920.0, 500.0), 1.0);
            assert_eq!(preferred_scale_at(&outputs, 3000.0, 500.0), 1.0);
        }
        // Phase `a_surface_off_every_output_gets_the_sharpest_scale`.
        {
            // Mismatched heights leave a gap. A surface there is about to land
            // somewhere; too sharp costs bandwidth, too soft is visible.
            let outputs = [
                ((0, 0), (1920u32, 1080u32), 1.0),
                ((1920, 0), (1280u32, 720u32), 2.0),
            ];
            assert_eq!(preferred_scale_at(&outputs, 2000.0, 900.0), 2.0);
        }
        // Phase `with_no_outputs_at_all_the_scale_is_one`.
        // Before the first modeset there is nothing to ask; 1.0 is the
        // only answer that cannot be wrong by a factor.
        assert_eq!(preferred_scale_at(&[], 0.0, 0.0), 1.0);
    }
}
