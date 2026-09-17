/// The 4 px spacing scale every gap, inset and gutter is drawn from.
///
/// `Copy` so a `const` context can read a step (`Spacing::BASE.s3`) for the
/// handful of chrome constants that must be compile-time values; everything
/// with a `Theme` in hand reads `theme.tokens.spacing.*`, which is already
/// scaled by [`crate::Theme::with_ui_scale`] and by a custom theme's
/// `spacing_px`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spacing {
    pub s0: f32,
    pub s1: f32,
    pub s2: f32,
    pub s3: f32,
    pub s4: f32,
    pub s6: f32,
    pub s8: f32,
    pub s12: f32,
    pub s16: f32,
}

impl Spacing {
    /// Unscaled reference scale: 0 / 4 / 8 / 12 / 16 / 24 / 32 / 48 / 64.
    /// [`Spacing::default`] returns this; a `const` that has to agree with
    /// a token (a shell breakpoint composed from a gutter, say) reads it
    /// directly instead of restating the number.
    pub const BASE: Self = Self {
        s0: 0.0,
        s1: 4.0,
        s2: 8.0,
        s3: 12.0,
        s4: 16.0,
        s6: 24.0,
        s8: 32.0,
        s12: 48.0,
        s16: 64.0,
    };

    /// The step's value on this (possibly scaled) scale.
    #[must_use]
    pub fn step(&self, step: SpacingStep) -> f32 {
        match step {
            SpacingStep::S0 => self.s0,
            SpacingStep::S1 => self.s1,
            SpacingStep::S2 => self.s2,
            SpacingStep::S3 => self.s3,
            SpacingStep::S4 => self.s4,
            SpacingStep::S6 => self.s6,
            SpacingStep::S8 => self.s8,
            SpacingStep::S12 => self.s12,
            SpacingStep::S16 => self.s16,
        }
    }

    /// Every step multiplied by `factor` (`ui_scale`, or a custom theme's
    /// `spacing_px / BASE.s3`). Proportions between steps are preserved, so
    /// a widget that pairs `s2` with `s4` keeps its 1:2 rhythm at any scale.
    #[must_use]
    pub fn scaled(&self, factor: f32) -> Self {
        Self {
            s0: self.s0 * factor,
            s1: self.s1 * factor,
            s2: self.s2 * factor,
            s3: self.s3 * factor,
            s4: self.s4 * factor,
            s6: self.s6 * factor,
            s8: self.s8 * factor,
            s12: self.s12 * factor,
            s16: self.s16 * factor,
        }
    }
}

impl Default for Spacing {
    fn default() -> Self {
        Self::BASE
    }
}

/// A named step on the [`Spacing`] scale, for APIs that resolve a gap at
/// layout time from the `Theme` they are handed (`Spacer::token`) rather than
/// taking an unscaled pixel value from the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpacingStep {
    S0,
    S1,
    S2,
    S3,
    S4,
    S6,
    S8,
    S12,
    S16,
}
