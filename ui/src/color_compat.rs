//! Backwards-compat shim for the peniko 0.1 → 0.6 transition.
//!
//! peniko 0.6 (bundled with vello 0.9) re-exports
//! [`color::AlphaColor<color::Srgb>`] as `peniko::Color`. That type holds
//! `[f32; 4]` components, so the old `c.r / c.g / c.b / c.a` u8-field
//! reads stopped compiling. The codebase relies on those reads in dozens
//! of places (theme rgb formatting, color picker round-trips, animation
//! lerps, etc.), and rewriting every call site to `c.to_rgba8().r` —
//! while doable — would mean churning hundreds of lines for no behaviour
//! delta.
//!
//! Instead we expose a small extension trait. Old code that did `c.r`
//! becomes `c.r()`, which the rust compiler can chase via UFCS and which
//! every call site can adopt with a one-character sed. The trait
//! collapses to the same `to_rgba8()` call peniko already exposes.

use vello::peniko::Color;

/// Extension trait adding back the legacy `r / g / b / a` u8 accessors
/// and a couple of constructors that used to live as direct methods on
/// `peniko::Color`.
pub trait ColorCompat {
    /// Red channel in `[0, 255]`.
    fn r(&self) -> u8;
    /// Green channel in `[0, 255]`.
    fn g(&self) -> u8;
    /// Blue channel in `[0, 255]`.
    fn b(&self) -> u8;
    /// Alpha channel in `[0, 255]`.
    fn a(&self) -> u8;

    /// Multiplies the alpha channel by `factor`. Old peniko had
    /// `with_alpha_factor`; peniko 0.6 exposes `multiply_alpha` with the
    /// same semantics.
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

/// Reconstruct a `peniko::Color` from raw u8 channels — the same as
/// `Color::from_rgba8` but expressed as a free function so call sites
/// that historically used `vello::peniko::Color { r, g, b, a }` struct
/// literals can switch to `color_compat::rgba8(r, g, b, a)`.
#[inline]
pub fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, a)
}

/// Old `Color::rgba(r, g, b, a)` constructor — floats in `[0.0, 1.0]`.
/// The new peniko has `Color::new([f32; 4])`; this helper preserves the
/// old `f64`-typed signature so callers don't need to insert `as f32`
/// casts everywhere.
#[inline]
pub fn rgba(r: f64, g: f64, b: f64, a: f64) -> Color {
    Color::new([r as f32, g as f32, b as f32, a as f32])
}

/// Old `Color::rgb(r, g, b)` constructor — floats in `[0.0, 1.0]`,
/// alpha defaults to 1.0. Matches the legacy peniko helper.
#[inline]
pub fn rgb(r: f64, g: f64, b: f64) -> Color {
    Color::new([r as f32, g as f32, b as f32, 1.0])
}
