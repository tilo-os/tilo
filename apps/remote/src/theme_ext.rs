//! Small colour helpers used when synthesizing the offline session
//! surfaces. These derive new shades *from theme tokens* rather than
//! hard-coding literals, so the synthesized wallpaper still tracks the
//! active theme's accent ramp.

use ui::ColorCompat;
use vello::peniko::Color;

/// Component-wise colour tweaks built on top of peniko's `Color`.
pub trait ColorExt {
    /// Replace the alpha channel (0.0..=1.0).
    fn at_alpha(self, a: f32) -> Color;
}

impl ColorExt for Color {
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
    fn at_alpha_sets_channel() {
        let c = Color::from_rgb8(10, 20, 30).at_alpha(0.5);
        assert_eq!(c.a(), 127);
    }
}
