//! Backwards-compat shim for the peniko 0.1 → 0.6 transition: `peniko::Color`
//! now holds `[f32; 4]` components, so the old `c.r`/`c.g`/`c.b`/`c.a` u8
//! field reads stopped compiling. This trait keeps those call sites as a
//! one-character `c.r()` sed rather than churning them to `c.to_rgba8().r`.

use vello::peniko::Color;

pub trait ColorCompat {
    fn r(&self) -> u8;
    fn g(&self) -> u8;
    fn b(&self) -> u8;
    fn a(&self) -> u8;

    /// Old peniko had `with_alpha_factor`; 0.6's `multiply_alpha` has the same semantics.
    fn with_alpha_factor(self, factor: f32) -> Self;
}

impl ColorCompat for Color {
    fn r(&self) -> u8 {
        self.to_rgba8().r
    }
    fn g(&self) -> u8 {
        self.to_rgba8().g
    }
    fn b(&self) -> u8 {
        self.to_rgba8().b
    }
    fn a(&self) -> u8 {
        self.to_rgba8().a
    }
    fn with_alpha_factor(self, factor: f32) -> Self {
        self.multiply_alpha(factor)
    }
}

/// Free-function equivalent of the old `vello::peniko::Color { r, g, b, a }` struct literal.
#[inline]
pub fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, a)
}

/// Old `Color::rgba(r, g, b, a)` constructor — floats in `[0.0, 1.0]`.
/// Preserves the old `f64`-typed signature so callers don't need `as f32` casts everywhere.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "narrowing to f32 is this shim's entire purpose (peniko::Color stores f32); \
              the doc-mandated [0.0, 1.0] range loses nothing meaningful at that precision"
)]
pub fn rgba(r: f64, g: f64, b: f64, a: f64) -> Color {
    Color::new([r as f32, g as f32, b as f32, a as f32])
}

/// Old `Color::rgb(r, g, b)` constructor — floats in `[0.0, 1.0]`, alpha defaults to 1.0.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "narrowing to f32 is this shim's entire purpose (peniko::Color stores f32); \
              the doc-mandated [0.0, 1.0] range loses nothing meaningful at that precision"
)]
pub fn rgb(r: f64, g: f64, b: f64) -> Color {
    Color::new([r as f32, g as f32, b as f32, 1.0])
}
