//! Curves a layout reads as its size changes: a responsive change moves at the same shape a motion
//! does. A painter's declared transition runs in `transition.rs`.

/// A blend between two values as a layout's size crosses a range, eased so it is flat at both ends: `0`
/// at or below `from`, `1` at or above `to`, and a smoothstep between.
#[must_use]
pub fn blend_across(value: f64, from: f64, to: f64) -> f64 {
    if to <= from {
        return if value >= to { 1.0 } else { 0.0 };
    }
    let t = ((value - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
