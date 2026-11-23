//! Byte-channel accessors on `peniko::Color` that it does not provide.

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
