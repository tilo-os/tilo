//! What the person is doing at this device's keyboard, as the device publishes it.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_resource::NoEffect;

/// How long the person must stop typing before they count as having paused. Keystroke studies
/// read a gap of two seconds or more as a pause to plan what comes next, and the gaps inside and
/// between words are well under a second: past three seconds, hesitating over a word never makes
/// what steps aside flicker back, and sitting back to read what was written still brings it.
pub const TYPING_PAUSE: Duration = Duration::from_secs(3);

/// What the person is doing at the keyboard, as a view that steps aside for their typing reads it
/// (`device.input`).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Input {
    /// The person is typing: a key that types text reached a window or a field less than
    /// [`TYPING_PAUSE`] ago. Modifiers, shortcuts and navigation keys do not count.
    pub typing: bool,
}

impl Input {
    /// What the keyboard says at `now`, when text was last typed at `typed_at`.
    #[must_use]
    pub fn at(typed_at: Option<std::time::Instant>, now: std::time::Instant) -> Self {
        Self {
            typing: typed_at
                .is_some_and(|at| now.saturating_duration_since(at) < TYPING_PAUSE),
        }
    }

    /// How long from `now` until the person who typed at `typed_at` counts as having paused, while
    /// they are still typing.
    #[must_use]
    pub fn pause_in(
        typed_at: Option<std::time::Instant>,
        now: std::time::Instant,
    ) -> Option<Duration> {
        let at = typed_at?;
        TYPING_PAUSE
            .checked_sub(now.saturating_duration_since(at))
            .filter(|left| !left.is_zero())
    }
}

/// What the keyboard says as a view of another language reads it (`device.input`).
#[must_use]
pub(crate) fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Input, NoEffect>("input", "Input")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Typing holds for the pause after the last typed key, and the pause ends it on time.
    #[test]
    fn typing_lasts_a_pause_after_the_last_key() {
        let typed = Instant::now();
        let later = |ms| typed + Duration::from_millis(ms);
        assert!(!Input::at(None, typed).typing, "nothing typed yet");
        assert!(Input::at(Some(typed), later(2_900)).typing, "within the pause");
        assert!(!Input::at(Some(typed), later(3_000)).typing, "the pause came");
        assert_eq!(
            Input::pause_in(Some(typed), later(1_000)),
            Some(Duration::from_secs(2)),
            "the pause is two seconds away"
        );
        assert_eq!(Input::pause_in(Some(typed), later(3_000)), None);
    }
}
