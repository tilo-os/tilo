//! Interpolation between two values of a kind, for blending colours and
//! amounts that are not motion (a chart's colour scale, a theme blend). A
//! value that moves over time does so through `crate::motion`.

/// Interpolation from `self` to `other` at `t` (`0..=1`).
pub trait Lerp {
    #[must_use]
    fn lerp(self, other: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(self, b: f32, t: f32) -> f32 {
        self + (b - self) * t
    }
}

impl Lerp for f64 {
    fn lerp(self, b: f64, t: f32) -> f64 {
        self + (b - self) * f64::from(t)
    }
}

impl Lerp for vello::peniko::Color {
    fn lerp(self, other: Self, t: f32) -> Self {
        use crate::color_compat::ColorCompat as _;
        let lerp_u8 = |a: u8, b: u8, t: f32| -> u8 {
            let a = f32::from(a);
            let b = f32::from(b);
            let v = (a + (b - a) * t).round().clamp(0.0, 255.0);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "v is clamped to 0.0..=255.0 just above, so it always fits u8 exactly"
            )]
            let result = v as u8;
            result
        };
        vello::peniko::Color::from_rgba8(
            lerp_u8(self.r(), other.r(), t),
            lerp_u8(self.g(), other.g(), t),
            lerp_u8(self.b(), other.b(), t),
            lerp_u8(self.a(), other.a(), t),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_amounts_interpolate_per_channel() {
        use vello::peniko::Color;
        let red = Color::from_rgba8(255, 0, 0, 255);
        let blue = Color::from_rgba8(0, 0, 255, 255);
        let mid = <Color as Lerp>::lerp(red, blue, 0.5);
        assert_eq!(mid.to_rgba8().r, 128);
        assert_eq!(mid.to_rgba8().g, 0);
        assert_eq!(mid.to_rgba8().b, 128);
        assert_eq!(mid.to_rgba8().a, 255);
        assert!(((0.0_f64).lerp(10.0, 0.5) - 5.0).abs() < 1e-9);
    }
}
