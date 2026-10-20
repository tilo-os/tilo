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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_conversions_saturate_at_boundaries_and_keep_ordinary_values() {
        // Case `integer_boundaries_saturate_instead_of_wrapping`.
        {
            assert_eq!(u8_of_i64(-1), 0);
            assert_eq!(u8_of_i64(999), u8::MAX);
            assert_eq!(u8_of_f64(-1.0), 0);
            assert_eq!(u8_of_f64(999.0), u8::MAX);
            assert_eq!(u32_of_i64(-1), 0);
            assert_eq!(i32_of_usize(usize::MAX), i32::MAX);
            assert_eq!(usize_of_i64(-1), 0);
        }

        // Case `ordinary_ui_values_keep_their_value`.
        {
            assert_eq!(f32_of_f64(12.5), 12.5);
            assert_eq!(i64_of_f32(42.0), 42);
            assert_eq!(usize_of_i32(7), 7);
        }
    }
}
