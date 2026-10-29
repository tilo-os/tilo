//! How far a wheel event travels, sideways and down.

use ui::winit::event::MouseScrollDelta;

/// Logical pixels a line of sideways wheel travels: the 40 px to the line a
/// trackpad's pixels are weighed against when telling sideways from
/// vertical, so both kinds of input move the strip alike.
const WHEEL_LINE_PX: f64 = 40.0;

/// A wheel event's sideways travel in logical pixels when horizontal motion
/// dominates, positive forward (toward later windows, like Super+Right).
/// Winit's deltas are negative for a wheel turned (or fingers scrolling)
/// rightward, which scrolls content toward its later columns and so moves
/// forward; it reports pixels physical. The shell's overlay host classifies
/// wheel input the same way.
pub(super) fn sideways(delta: MouseScrollDelta, scale_factor: f64) -> Option<f64> {
    let (dx, dy) = match delta {
        MouseScrollDelta::LineDelta(x, y) => {
            (f64::from(x) * WHEEL_LINE_PX, f64::from(y) * WHEEL_LINE_PX)
        }
        MouseScrollDelta::PixelDelta(p) => {
            let scale = scale_factor.max(f64::EPSILON);
            (p.x / scale, p.y / scale)
        }
    };
    (dx.abs() > dy.abs()).then_some(-dx)
}

/// A wheel event's vertical travel in logical pixels, positive for a wheel
/// turned (or fingers scrolling) downward, which winit reports negative.
pub(super) fn vertical(delta: MouseScrollDelta, scale_factor: f64) -> f64 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * WHEEL_LINE_PX,
        MouseScrollDelta::PixelDelta(p) => -p.y / scale_factor.max(f64::EPSILON),
    }
}
