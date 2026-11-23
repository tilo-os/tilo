//! How emphatic a component's own motion is, chosen where it is used: a pace over the theme's tokens,
//! never a set of curves of its own.

use crate::tokens::{EffectiveMotion, Motion};

/// The pace a component's motion plays at: [`Self::Standard`] is the theme's, the others multiply the
/// theme's token durations, and [`Self::Still`] holds the component still.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Feel {
    /// The theme's own pace. What a component does when it says nothing.
    #[default]
    Standard,
    /// Quicker: for dense, repetitive work where the eye is already where the change will be (a
    /// checklist being worked through).
    Brisk,
    /// Slower and softer: for a surface a person reads or watches, where the change should not feel
    /// hurried (a long list reordering under them).
    Calm,
    /// No motion at all: every change lands on the next frame.
    Still,
}

impl Feel {
    /// This pace over the motion the person allows and the theme provides.
    #[must_use]
    pub fn over(self, motion: EffectiveMotion) -> EffectiveMotion {
        match self {
            Self::Standard => motion,
            Self::Brisk => motion.with_tokens(motion.tokens().with_duration_scale(0.7)),
            Self::Calm => motion.with_tokens(motion.tokens().with_duration_scale(1.5)),
            Self::Still => motion.with_tokens(Motion {
                still: true,
                ..motion.tokens()
            }),
        }
    }
}

crate::component::choice!(Feel, "Motion", {
    Standard => "standard",
    Brisk => "brisk",
    Calm => "calm",
    Still => "still",
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::Motion;

    #[test]
    fn a_feel_paces_the_themes_tokens_and_keeps_the_persons_setting() {
        let tokens = Motion::default();
        let standard = EffectiveMotion::new(tokens, false);
        assert_eq!(Feel::Standard.over(standard), standard);

        let brisk = Feel::Brisk.over(standard);
        assert!(brisk.base() < standard.base());
        assert_eq!(brisk.replacement_fade(), crate::time::Duration::ZERO);

        let calm = Feel::Calm.over(standard);
        assert!(calm.base() > standard.base());
        assert_eq!(calm.tokens().durations.base_ms, 315);

        let reduced = EffectiveMotion::new(tokens, true);
        assert!(!Feel::Brisk.over(reduced).moves());
        assert!(!Feel::Calm.over(reduced).moves());

        let theme_still = EffectiveMotion::new(
            Motion {
                still: true,
                ..tokens
            },
            false,
        );
        assert!(Feel::Brisk.over(theme_still).still());
        assert!(Feel::Still.over(standard).still());
        assert!(!Feel::Still.over(standard).moves());
    }
}
