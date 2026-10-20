//! The few numeric conversions the runtime needs, each in one audited place.

/// Float counters are stored as Automerge counters in millionths.
const FLOAT_COUNTER_SCALE: f64 = 1_000_000.0;

#[allow(
    clippy::cast_precision_loss,
    reason = "counter values are far below 2^53 millionths in practice; the scale is documented"
)]
pub fn counter_to_float(units: i64) -> f64 {
    units as f64 / FLOAT_COUNTER_SCALE
}

/// The integer units of a float delta, or `None` when it is not finite or out of range.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the range is checked on the line above"
)]
pub fn float_to_counter(delta: f64) -> Option<i64> {
    let scaled = (delta * FLOAT_COUNTER_SCALE).round();
    (scaled.is_finite() && scaled.abs() < 9.0e18).then_some(scaled as i64)
}

/// Widen a wasm length or index.
pub fn to_usize(v: i64) -> Option<usize> {
    usize::try_from(v).ok()
}

#[allow(
    clippy::cast_possible_wrap,
    reason = "a collection length never reaches i64::MAX"
)]
pub fn len_to_i64(n: usize) -> i64 {
    n as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_counters_round_trip() {
        assert_eq!(float_to_counter(1.5), Some(1_500_000));
        assert!((counter_to_float(1_500_000) - 1.5).abs() < 1e-9);
        assert_eq!(float_to_counter(f64::NAN), None);
    }
}
