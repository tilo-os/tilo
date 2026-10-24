//! Compatibility helpers for the byte-channel accessors and `f64`
//! constructors that `peniko::Color` does not provide.

use vello::peniko::Color;

pub trait ColorCompat {
    fn r(&self) -> u8;
    fn g(&self) -> u8;
    fn b(&self) -> u8;
    fn a(&self) -> u8;

    #[must_use]
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

/// Accepts `f64` components so callers do not need repeated narrowing casts.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "narrowing to f32 is this shim's entire purpose (peniko::Color stores f32); \
              the doc-mandated [0.0, 1.0] range loses nothing meaningful at that precision"
)]
pub fn rgba(r: f64, g: f64, b: f64, a: f64) -> Color {
    Color::new([r as f32, g as f32, b as f32, a as f32])
}

/// Accepts `f64` components and defaults alpha to 1.0.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "narrowing to f32 is this shim's entire purpose (peniko::Color stores f32); \
              the doc-mandated [0.0, 1.0] range loses nothing meaningful at that precision"
)]
pub fn rgb(r: f64, g: f64, b: f64) -> Color {
    Color::new([r as f32, g as f32, b as f32, 1.0])
}
