//! How a wheel's motion is read: sideways travel and the gesture that a
//! trackpad's fingers make. The desktop's window strip and the app under the
//! pointer read a wheel by these one rules.

use winit::event::MouseScrollDelta;

/// A wheel event's sideways travel in logical pixels when horizontal motion
/// dominates, positive for a wheel turned (or fingers scrolling) rightward,
/// which scrolls content toward its later columns; `None` for vertical
/// motion. Winit reports pixels physical, so they are divided by
/// `scale_factor`, and a line is a notch of the wheel's own metric. The
/// desktop's window strip and the app under the pointer read a wheel by this
/// one rule.
pub fn sideways_travel(delta: &MouseScrollDelta, scale_factor: f64) -> Option<f64> {
    let line = line_of_wheel();
    let (dx, dy) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * line, f64::from(*y) * line),
        MouseScrollDelta::PixelDelta(p) => {
            let scale = scale_factor.max(f64::EPSILON);
            (p.x / scale, p.y / scale)
        }
    };
    (dx.abs() > dy.abs()).then_some(-dx)
}

/// Logical pixels one notch of a line-stepping wheel scrolls, the metrics
/// token that weighs a line against a touchpad's pixels.
pub fn line_of_wheel() -> f64 {
    f64::from(crate::tokens::Metrics::BASE.wheel_line)
}

/// A wheel's touch gesture ends when it pauses this long: a trackpad keeps
/// reporting while the fingers move and through the momentum after they
/// lift, and a spun mouse wheel's notches come well inside it.
pub const WHEEL_GESTURE_GAP: crate::time::Duration = crate::time::Duration::from_millis(200);
