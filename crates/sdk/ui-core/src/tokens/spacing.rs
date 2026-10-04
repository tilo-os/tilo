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
    /// The scale named by relationship: how far apart two things are
    /// follows from how they relate, so one relationship never carries two
    /// different gaps.
    pub rhythm: Rhythm,
}

/// Gaps named by the relationship between two neighbours (HIG §7b), each a
/// step of the [`Spacing`] scale and scaled with it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rhythm {
    /// The smallest gap two neighbours may have; a control's face inset.
    pub tight: f32,
    /// Controls inside one cluster (a bar's tool buttons, a field and its
    /// toggle).
    pub related: f32,
    /// Siblings of equal weight: clusters on a bar, cards and rows in a
    /// list.
    pub sibling: f32,
    /// Padding inside a card or panel body; the gap between two sections.
    pub section: f32,
    /// The gap between groups of sections, and the content gutter.
    pub group: f32,
}

impl Rhythm {
    /// The rhythm on the unscaled scale: 4 / 8 / 12 / 16 / 24.
    pub const BASE: Self = Self {
        tight: 4.0,
        related: 8.0,
        sibling: 12.0,
        section: 16.0,
        group: 24.0,
    };

    #[must_use]
    fn scaled(&self, factor: f32) -> Self {
        Self {
            tight: self.tight * factor,
            related: self.related * factor,
            sibling: self.sibling * factor,
            section: self.section * factor,
            group: self.group * factor,
        }
    }
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
        rhythm: Rhythm::BASE,
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
            rhythm: self.rhythm.scaled(factor),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rhythm_is_monotone_scale_steps_and_scales_with_the_scale() {
        let base = Spacing::BASE;
        let r = base.rhythm;
        assert_eq!(
            [r.tight, r.related, r.sibling, r.section, r.group],
            [base.s1, base.s2, base.s3, base.s4, base.s6]
        );
        let scaled = base.scaled(1.5);
        assert!((scaled.rhythm.section - base.s4 * 1.5).abs() < 1e-4);
        assert!((scaled.rhythm.tight - scaled.s1).abs() < 1e-4);
        let dense = crate::Theme::dark().with_ui_scale(2.0);
        assert!((dense.tokens.spacing.rhythm.sibling - 24.0).abs() < 1e-4);
    }
}
