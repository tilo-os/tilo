//! Total numeric conversions for layout, paint and document metrics.
//!
//! The editor mixes three numeric worlds: `usize` counts and indices from the
//! text buffer, `i64` attribute values from the CRDT document, and `f64`/`f32`
//! pixel geometry. Every conversion between them is narrowing in at least one
//! direction, and `overflow-checks` is off in release, so a bare `as` would
//! wrap silently.
//!
//! These helpers are the single place those conversions happen. Integer
//! narrowing saturates at the destination's bound rather than wrapping; float
//! conversions are the ones that genuinely cannot be expressed losslessly, and
//! each carries the reason it is safe here.

/// A count or index as a document attribute value, saturating at [`i64::MAX`].
#[inline]
pub fn i64_of_usize(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// A count as a signed splice delta, saturating at [`isize::MAX`].
#[inline]
pub fn isize_of_usize(n: usize) -> isize {
    isize::try_from(n).unwrap_or(isize::MAX)
}

/// A count as an `i32`, saturating at [`i32::MAX`].
#[inline]
pub fn i32_of_usize(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// A count as a `u32`, saturating at [`u32::MAX`].
#[inline]
pub fn u32_of_usize(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A document attribute as an index. Negatives clamp to `0`; a value beyond
/// the address space clamps to [`usize::MAX`].
#[inline]
pub fn usize_of_i64(n: i64) -> usize {
    usize::try_from(n.max(0)).unwrap_or(usize::MAX)
}

/// A signed offset as an index; negatives clamp to `0`.
#[inline]
pub fn usize_of_i32(n: i32) -> usize {
    usize::try_from(n.max(0)).unwrap_or(0)
}

/// A signed offset as an index; negatives clamp to `0`.
#[inline]
pub fn usize_of_isize(n: isize) -> usize {
    usize::try_from(n.max(0)).unwrap_or(0)
}

/// A document attribute as an unsigned count, clamped to `0..=u32::MAX`.
#[inline]
pub fn u32_of_i64(n: i64) -> u32 {
    u32::try_from(n.clamp(0, i64::from(u32::MAX))).unwrap_or(u32::MAX)
}

/// A document attribute as a byte-sized channel, clamped to `0..=255`.
#[inline]
pub fn u8_of_i64(n: i64) -> u8 {
    u8::try_from(n.clamp(0, 255)).unwrap_or(u8::MAX)
}

/// A count as a layout coordinate.
#[allow(
    clippy::cast_precision_loss,
    reason = "counts here are character, line, block and page tallies of a \
              single open document, orders of magnitude below 2^53 where f64 \
              is exact"
)]
#[inline]
pub fn f64_of_usize(n: usize) -> f64 {
    n as f64
}

/// A document attribute as a layout coordinate.
#[allow(
    clippy::cast_precision_loss,
    reason = "attribute values are page/row/column metrics and pixel sizes, \
              far below 2^53 where f64 is exact"
)]
#[inline]
pub fn f64_of_i64(n: i64) -> f64 {
    n as f64
}

/// A layout coordinate as a paint coordinate.
#[allow(
    clippy::cast_possible_truncation,
    reason = "vello and taffy take f32; layout coordinates are page pixels, \
              well inside f32's range, and sub-ulp precision is not observable \
              in a rasterised frame"
)]
#[inline]
pub fn f32_of_f64(v: f64) -> f32 {
    v as f32
}

/// A count as a paint coordinate.
#[allow(
    clippy::cast_precision_loss,
    reason = "counts reaching paint are row, column and glyph tallies, far \
              below 2^24 where f32 is exact"
)]
#[inline]
pub fn f32_of_u32(n: u32) -> f32 {
    n as f32
}

/// A count as a paint coordinate.
#[allow(
    clippy::cast_precision_loss,
    reason = "counts reaching paint are row, column and glyph tallies, far \
              below 2^24 where f32 is exact"
)]
#[inline]
pub fn f32_of_usize(n: usize) -> f32 {
    n as f32
}

/// A document attribute as a paint coordinate.
#[allow(
    clippy::cast_precision_loss,
    reason = "attribute values reaching paint are pixel sizes and counts, far \
              below 2^24 where f32 is exact"
)]
#[inline]
pub fn f32_of_i64(n: i64) -> f32 {
    n as f32
}

/// A coordinate as an integer, truncating toward zero.
///
/// Rust's float-to-integer `as` is saturating: out-of-range values clamp to
/// the destination's bounds and `NaN` maps to `0`, so this is total.
#[allow(
    clippy::cast_possible_truncation,
    reason = "float-to-int `as` saturates rather than wrapping; callers round \
              or clamp first where the fractional part matters"
)]
#[inline]
pub fn i64_of_f64(v: f64) -> i64 {
    v as i64
}

/// A paint coordinate as an integer, truncating toward zero. Saturating.
#[allow(
    clippy::cast_possible_truncation,
    reason = "float-to-int `as` saturates rather than wrapping; callers round \
              or clamp first where the fractional part matters"
)]
#[inline]
pub fn i64_of_f32(v: f32) -> i64 {
    v as i64
}

/// A coordinate as an `i32`, truncating toward zero. Saturating.
#[allow(
    clippy::cast_possible_truncation,
    reason = "float-to-int `as` saturates rather than wrapping; callers round \
              or clamp first where the fractional part matters"
)]
#[inline]
pub fn i32_of_f64(v: f64) -> i32 {
    v as i32
}

/// A non-negative coordinate as an index, truncating toward zero.
///
/// Saturating in both directions: negatives and `NaN` map to `0`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "float-to-int `as` saturates rather than wrapping, and the \
              `max(0.0)` makes the sign loss a no-op"
)]
#[inline]
pub fn usize_of_f64(v: f64) -> usize {
    v.max(0.0) as usize
}
