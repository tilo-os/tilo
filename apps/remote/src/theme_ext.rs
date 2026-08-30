//! Small colour helpers used when synthesizing the offline session
//! surfaces. These derive new shades *from theme tokens* rather than
//! hard-coding literals, so the synthesized wallpaper still tracks the
//! active theme's accent ramp.

use ui::ColorCompat;
use vello::peniko::Color;

/// Component-wise colour tweaks built on top of peniko's `Color`.
pub trait ColorExt {
    /// Darken toward black by `t` in `0.0..=1.0` (0 = unchanged).
    #[allow(dead_code)]
    fn darken(self, t: f32) -> Color;
    /// Lighten toward white by `t` in `0.0..=1.0`.
    #[allow(dead_code)]
    fn lighten(self, t: f32) -> Color;
    /// Replace the alpha channel (0.0..=1.0).
    fn at_alpha(self, a: f32) -> Color;
}

impl ColorExt for Color {
    fn darken(self, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let f = 1.0 - t;
        Color::from_rgba8(
            (f32::from(self.r()) * f) as u8,
            (f32::from(self.g()) * f) as u8,
            (f32::from(self.b()) * f) as u8,
            self.a(),
        )
    }

    fn lighten(self, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let lerp = |c: u8| (f32::from(c) + (255.0 - f32::from(c)) * t) as u8;
        Color::from_rgba8(lerp(self.r()), lerp(self.g()), lerp(self.b()), self.a())
    }

    fn at_alpha(self, a: f32) -> Color {
        Color::from_rgba8(
            self.r(),
            self.g(),
            self.b(),
            (a.clamp(0.0, 1.0) * 255.0) as u8,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darken_moves_toward_black() {
        let c = Color::from_rgb8(200, 100, 50).darken(0.5);
        assert_eq!([c.r(), c.g(), c.b()], [100, 50, 25]);
    }

    #[test]
    fn lighten_moves_toward_white() {
        let c = Color::from_rgb8(0, 0, 0).lighten(0.5);
        assert_eq!([c.r(), c.g(), c.b()], [127, 127, 127]);
    }

    #[test]
    fn at_alpha_sets_channel() {
        let c = Color::from_rgb8(10, 20, 30).at_alpha(0.5);
        assert_eq!(c.a(), 127);
    }
}
