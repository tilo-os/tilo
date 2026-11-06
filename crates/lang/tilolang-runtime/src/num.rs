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

/// The most places `round`, `floor` and `ceil` keep: a Float holds no more than seventeen
/// digits, so a longer scale would only add its own error.
const MAX_PLACES: i64 = 15;

/// The scale a number of places is kept at.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the places are clamped to 0..=15 first"
)]
fn scale(places: i64) -> f64 {
    10f64.powi(places.clamp(0, MAX_PLACES) as i32)
}

/// `x` rounded to `places` decimals, halves away from zero.
pub fn round_to(x: f64, places: i64) -> f64 {
    let s = scale(places);
    (x * s).round() / s
}

/// `x` rounded down to `places` decimals.
pub fn floor_to(x: f64, places: i64) -> f64 {
    let s = scale(places);
    (x * s).floor() / s
}

/// `x` rounded up to `places` decimals.
pub fn ceil_to(x: f64, places: i64) -> f64 {
    let s = scale(places);
    (x * s).ceil() / s
}

/// The whole number nearest `x`, halves away from zero, saturating past the ends of `Int` and
/// answering 0 for no number at all.
#[allow(
    clippy::cast_possible_truncation,
    reason = "`as i64` from a float saturates and answers 0 for NaN: no trap"
)]
pub fn round_whole(x: f64) -> i64 {
    x.round() as i64
}

/// The whole number at or under `x`, as [`round_whole`] reads one.
#[allow(
    clippy::cast_possible_truncation,
    reason = "`as i64` from a float saturates and answers 0 for NaN: no trap"
)]
pub fn floor_whole(x: f64) -> i64 {
    x.floor() as i64
}

/// The whole number at or over `x`, as [`round_whole`] reads one.
#[allow(
    clippy::cast_possible_truncation,
    reason = "`as i64` from a float saturates and answers 0 for NaN: no trap"
)]
pub fn ceil_whole(x: f64) -> i64 {
    x.ceil() as i64
}

/// A whole number as a float. Past 2^53 the nearest float is the one a sum of them is kept in
/// anyway.
#[allow(
    clippy::cast_precision_loss,
    reason = "an average past 2^53 items is not a number a person reads"
)]
pub fn int_to_float(n: i64) -> f64 {
    n as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "each case is a value the rounding must give exactly, not one near another"
    )]
    fn rounding_keeps_the_places_it_is_given() {
        assert_eq!(round_to(2.5, 0), 3.0);
        assert_eq!(round_to(-2.5, 0), -3.0, "halves go away from zero");
        assert_eq!(round_to(1.2345, 2), 1.23);
        assert_eq!(round_to(1.2355, 3), 1.236);
        assert_eq!(floor_to(1.239, 2), 1.23);
        assert_eq!(ceil_to(1.231, 2), 1.24);
        assert_eq!(
            round_to(1.5, 99),
            1.5,
            "the places stop at what a double holds"
        );
        assert_eq!(round_whole(2.5), 3);
        assert_eq!(floor_whole(-2.5), -3);
        assert_eq!(ceil_whole(-2.5), -2);
        assert_eq!(round_whole(f64::NAN), 0);
    }

    #[test]
    fn float_counters_round_trip() {
        assert_eq!(float_to_counter(1.5), Some(1_500_000));
        assert!((counter_to_float(1_500_000) - 1.5).abs() < 1e-9);
        assert_eq!(float_to_counter(f64::NAN), None);
    }
}
