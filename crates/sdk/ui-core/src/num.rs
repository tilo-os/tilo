//! Explicit numeric conversions used at UI/model boundaries.
//!
//! Keeping the necessarily lossy casts here makes their policy consistent:
//! integer narrowing and signed-to-unsigned conversions saturate instead of
//! wrapping, while geometry conversions use Rust's saturating float-to-int
//! semantics. Call sites stay readable without scattering lint exceptions.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "this module is where the toolkit's lossy numeric casts live, each with its saturation policy"
)]

pub fn f32_of_f64(value: f64) -> f32 {
    value.clamp(f64::from(f32::MIN), f64::from(f32::MAX)) as f32
}

pub fn f32_of_i64(value: i64) -> f32 {
    value as f32
}

pub fn f32_of_u32(value: u32) -> f32 {
    value as f32
}

pub fn f64_of_i64(value: i64) -> f64 {
    value as f64
}

pub fn f64_of_u64(value: u64) -> f64 {
    value as f64
}

pub fn f64_of_usize(value: usize) -> f64 {
    value as f64
}

pub fn i32_of_f64(value: f64) -> i32 {
    value as i32
}

pub fn i32_of_usize(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

pub fn i64_of_f32(value: f32) -> i64 {
    value as i64
}

pub fn i64_of_f64(value: f64) -> i64 {
    value as i64
}

pub fn i64_of_usize(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub fn isize_of_usize(value: usize) -> isize {
    isize::try_from(value).unwrap_or(isize::MAX)
}

pub fn u32_of_i64(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(if value < 0 { 0 } else { u32::MAX })
}

pub fn u32_of_f64(value: f64) -> u32 {
    value as u32
}

pub fn u32_of_usize(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

pub fn u8_of_i64(value: i64) -> u8 {
    u8::try_from(value).unwrap_or(if value < 0 { 0 } else { u8::MAX })
}

pub fn u8_of_f64(value: f64) -> u8 {
    value as u8
}

pub fn usize_of_f64(value: f64) -> usize {
    value as usize
}

pub fn usize_of_i32(value: i32) -> usize {
    usize::try_from(value).unwrap_or_default()
}

pub fn usize_of_i64(value: i64) -> usize {
    usize::try_from(value).unwrap_or_default()
}

pub fn usize_of_isize(value: isize) -> usize {
    usize::try_from(value).unwrap_or_default()
}

/// The largest power of ten an `f64` holds exactly.
const EXACT_DECIMAL_PLACES: u32 = 22;
/// From here up an `f64` holds only whole numbers, so rounding a scaled
/// value would change nothing.
const WHOLE_FROM: f64 = 4_503_599_627_370_496.0;

/// How many decimal places `value` is written with at its shortest (`0.25`
/// has two, `3.0` none); `None` when it is not finite or needs more places
/// than [`snap_decimal`] can keep exact.
pub fn decimal_places(value: f64) -> Option<u32> {
    if !value.is_finite() {
        return None;
    }
    let shortest = value.abs().to_string();
    let places = shortest
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    u32::try_from(places)
        .ok()
        .filter(|&places| places <= EXACT_DECIMAL_PLACES)
}

/// `value`, a sum of `operands` or of whole multiples of them, as the
/// decimal those operands make: rounded to the most places any of them is
/// written with. Binary floats drift where decimals do not (`0.1 + 0.2` is
/// `0.30000000000000004`, `7 × 0.1` is `0.7000000000000001`), so a stepper
/// or slider moved in tenths would otherwise land beside the value it shows
/// and miss the end of its range. `value` is returned unchanged when an
/// operand is not a short decimal.
pub fn snap_decimal(value: f64, operands: &[f64]) -> f64 {
    let mut places = 0;
    for &operand in operands {
        let Some(operand_places) = decimal_places(operand) else {
            return value;
        };
        places = places.max(operand_places);
    }
    if places == 0 {
        return value;
    }
    let scale = 10_f64.powi(i32::try_from(places).unwrap_or(i32::MAX));
    let scaled = value * scale;
    if !scaled.is_finite() || scaled.abs() >= WHOLE_FROM {
        return value;
    }
    // Adding zero turns a `-0.0` (a step back onto zero) into `0.0`, which
    // a formatter would otherwise show with a sign.
    scaled.round() / scale + 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_conversions_saturate_at_boundaries_and_keep_ordinary_values() {
        {
            assert_eq!(u8_of_i64(-1), 0);
            assert_eq!(u8_of_i64(999), u8::MAX);
            assert_eq!(u8_of_f64(-1.0), 0);
            assert_eq!(u8_of_f64(999.0), u8::MAX);
            assert_eq!(u32_of_i64(-1), 0);
            assert_eq!(i32_of_usize(usize::MAX), i32::MAX);
            assert_eq!(usize_of_i64(-1), 0);
        }

        {
            assert_eq!(f32_of_f64(12.5), 12.5);
            assert_eq!(i64_of_f32(42.0), 42);
            assert_eq!(usize_of_i32(7), 7);
        }
    }

    #[test]
    fn decimal_steps_land_on_the_decimal_they_make() {
        assert_eq!(snap_decimal(0.1 + 0.2, &[0.1, 0.2]), 0.3);
        assert_eq!(snap_decimal(7.0 * 0.1, &[0.1]), 0.7);
        assert_eq!(snap_decimal(1.234 + 0.1, &[1.234, 0.1]), 1.334);
        assert_eq!(snap_decimal(1e-18 + 1e-18, &[1e-18]), 2e-18);
        assert_eq!(snap_decimal(10.5 + 1.0, &[10.5, 1.0]), 11.5);
        let back_to_zero = snap_decimal(0.1 - 0.1, &[0.1]);
        assert_eq!(back_to_zero.to_bits(), 0.0_f64.to_bits(), "zero, unsigned");
        assert_eq!(
            snap_decimal(-0.1 + 0.05, &[-0.1, 0.05]).to_bits(),
            (-0.05_f64).to_bits()
        );
        let third = 1.0 / 3.0;
        assert_eq!(snap_decimal(third + 1.0, &[third]), third + 1.0);
        assert_eq!(snap_decimal(1e300 + 0.1, &[1e300, 0.1]), 1e300 + 0.1);
        assert!(snap_decimal(f64::NAN, &[0.1]).is_nan());
        assert_eq!(decimal_places(0.25), Some(2));
        assert_eq!(decimal_places(-3.0), Some(0));
        assert_eq!(decimal_places(f64::INFINITY), None);
    }
}
