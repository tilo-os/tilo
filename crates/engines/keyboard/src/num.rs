//! The few numeric conversions the keyboard needs at its boundaries.
//!
//! Rust has no checked or infallible conversion from an `f64` to an `f32`, and
//! none from a float to an integer, so the lossy ones the engine cannot avoid
//! live here, each bounded by what the keyboard measures: key geometry and tap
//! positions in logical pixels, probabilities, frequencies and scores in
//! `0..=1`, tap offsets in key widths, and counts of points, words, keys and
//! grid cells in the thousands. `crates/ui/ui-core/src/num.rs` keeps the same
//! policy for the toolkit's boundaries; this crate may not depend on the UI, so
//! it keeps its own.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "the values narrowed here are geometry in logical pixels, probabilities and scores in 0..=1, and counts far below 2^24"
)]

/// `value` as an `f32`: the narrowing a probability, score, key offset or pixel
/// coordinate takes when it reaches a type that stores an `f32`.
pub fn f32_of_f64(value: f64) -> f32 {
    value as f32
}

/// A dictionary or bigram weight, or a counter, as an `f32`. An `f32` holds
/// every integer below 2^24 exactly and rounds the rest; the bundled tables
/// stay below 2^16 and any host's table satisfies the same bound that an
/// `f32` score implies.
pub fn f32_of_u32(value: u32) -> f32 {
    value as f32
}

/// A count of points, words, keys or cells as an `f32`, through the same `u32`
/// bound as [`f64_of_count`].
pub fn f32_of_count(count: usize) -> f32 {
    f32_of_u32(u32::try_from(count).unwrap_or(u32::MAX))
}

/// A count of points, words, keys or cells as an `f64`. `u32::try_from` states
/// the bound: a count above 2^32 cannot come from a path, a word list or a
/// screen, and saturating there keeps the conversion total.
pub fn f64_of_count(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// A grid dimension divided by a step, as a whole number of cells. A float to
/// integer `as` saturates and maps NaN to zero, so a nonsensical area asks for
/// no cells rather than a huge number; callers reject non-positive steps and
/// areas before dividing.
pub fn usize_of_f64(value: f64) -> usize {
    value as usize
}
