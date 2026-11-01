//! The whole-desktop colour filter: one colour transform every output's
//! final GPU pass applies to the scene it composited.
//!
//! The transform is a 3×3 matrix and an offset — a colour turning off and an
//! inversion are both of that shape — so the shader is one matrix multiply
//! and the choice of filter is data. The pass runs on the GPU with the
//! output's calibration and HDR encoding ([`crate::backend::drm::color`]);
//! nothing is read back or drawn on the CPU.

/// The filter the person chose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorFilter {
    /// Colours as they are.
    #[default]
    Off,
    /// Every colour turned to its brightness: a grey picture.
    Grayscale,
    /// Every colour turned to its opposite.
    Inverted,
    /// Both.
    GrayscaleInverted,
}

impl ColorFilter {
    /// Every filter, in the order Settings offers them.
    pub const ALL: [Self; 4] = [
        Self::Off,
        Self::Grayscale,
        Self::Inverted,
        Self::GrayscaleInverted,
    ];

    /// Whether the scene is shown as it is.
    pub const fn is_off(self) -> bool {
        matches!(self, Self::Off)
    }

    /// The transform itself: `rgb' = rows · rgb + offset`, each row a
    /// channel of the result.
    pub const fn matrix(self) -> ColorMatrix {
        /// Rec. 709 luminance: the brightness of a colour as the eye sees it.
        const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
        const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        const NONE: [f32; 3] = [0.0, 0.0, 0.0];
        const WHITE: [f32; 3] = [1.0, 1.0, 1.0];
        const INVERT: [[f32; 3]; 3] = [[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]];
        match self {
            Self::Off => ColorMatrix {
                rows: IDENTITY,
                offset: NONE,
            },
            Self::Grayscale => ColorMatrix {
                rows: [LUMA, LUMA, LUMA],
                offset: NONE,
            },
            Self::Inverted => ColorMatrix {
                rows: INVERT,
                offset: WHITE,
            },
            // Grey first, then its opposite: the inversion of the luma.
            Self::GrayscaleInverted => ColorMatrix {
                rows: [
                    [-LUMA[0], -LUMA[1], -LUMA[2]],
                    [-LUMA[0], -LUMA[1], -LUMA[2]],
                    [-LUMA[0], -LUMA[1], -LUMA[2]],
                ],
                offset: WHITE,
            },
        }
    }

    /// The transform applied to `rgb`, each channel `0.0 ..= 1.0`: what the
    /// shader computes.
    pub fn apply(self, rgb: [f32; 3]) -> [f32; 3] {
        let matrix = self.matrix();
        let mut out = matrix.offset;
        for (channel, row) in out.iter_mut().zip(matrix.rows) {
            *channel += row[0] * rgb[0];
            *channel += row[1] * rgb[1];
            *channel += row[2] * rgb[2];
            *channel = channel.clamp(0.0, 1.0);
        }
        out
    }
}

/// A colour transform: a 3×3 matrix and a constant offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorMatrix {
    /// One row per output channel.
    pub rows: [[f32; 3]; 3],
    pub offset: [f32; 3],
}

impl Default for ColorMatrix {
    fn default() -> Self {
        ColorFilter::default().matrix()
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "the fixture colours are exact multiples of the documented coefficients"
)]
mod tests {
    use super::*;

    #[test]
    fn off_leaves_every_colour_alone() {
        for rgb in [[0.0, 0.0, 0.0], [1.0, 0.0, 0.4], [0.25, 0.5, 0.75]] {
            assert_eq!(ColorFilter::Off.apply(rgb), rgb);
        }
        assert_eq!(ColorFilter::Off.matrix(), ColorMatrix::default());
    }

    #[test]
    fn grayscale_turns_every_colour_its_brightness() {
        let grey = ColorFilter::Grayscale.apply([1.0, 0.0, 0.0]);
        assert!((grey[0] - 0.2126).abs() < 1e-6, "{grey:?}");
        assert_eq!(grey[0], grey[1]);
        assert_eq!(grey[1], grey[2]);
        let white = ColorFilter::Grayscale.apply([1.0, 1.0, 1.0]);
        assert!(
            (white[0] - 1.0).abs() < 1e-5,
            "white stays white: {white:?}"
        );
        let black = ColorFilter::Grayscale.apply([0.0, 0.0, 0.0]);
        assert_eq!(black, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn inverted_is_the_opposite_and_stays_in_range() {
        assert_eq!(
            ColorFilter::Inverted.apply([1.0, 0.0, 0.4]),
            [0.0, 1.0, 0.6]
        );
        assert_eq!(
            ColorFilter::Inverted.apply([0.0, 0.0, 0.0]),
            [1.0, 1.0, 1.0]
        );
    }

    #[test]
    fn gray_inverted_is_the_opposite_of_the_grey() {
        let rgb = [0.2, 0.6, 0.9];
        let grey = ColorFilter::Grayscale.apply(rgb);
        let both = ColorFilter::GrayscaleInverted.apply(rgb);
        for channel in 0..3 {
            assert!(
                (both[channel] - (1.0 - grey[channel])).abs() < 1e-5,
                "{both:?} vs {grey:?}"
            );
        }
        assert_eq!(both[0], both[1]);
        assert_eq!(both[1], both[2]);
    }

    #[test]
    fn only_off_shows_the_scene_as_it_is() {
        assert!(ColorFilter::Off.is_off());
        for filter in [
            ColorFilter::Grayscale,
            ColorFilter::Inverted,
            ColorFilter::GrayscaleInverted,
        ] {
            assert!(!filter.is_off(), "{filter:?}");
            assert_ne!(filter.matrix(), ColorMatrix::default(), "{filter:?}");
        }
        assert_eq!(ColorFilter::ALL.len(), 4);
    }
}
