//! How available a person says they are, beside their face.

use web_time::Duration;

/// How available a person says they are, beside their face. Emoji-free
/// presets; idle and away also come on their own (no input for
/// [`IDLE_AFTER`], a locked screen).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Availability {
    /// Here and open to being talked to.
    #[default]
    Available,
    /// Here, heads down.
    Focusing,
    /// Not at the computer: chosen, or the screen is locked.
    Away,
    /// No input for a while: the face dims.
    Idle,
}

/// No input for this long and a person shows as idle.
pub const IDLE_AFTER: Duration = Duration::from_secs(5 * 60);

impl Availability {
    /// What others see, from what the person chose and what their machine
    /// knows: a locked screen is away, and a quiet one is idle unless they
    /// chose otherwise.
    #[must_use]
    pub fn shown(chosen: Self, locked: bool, quiet_for: Duration) -> Self {
        if locked {
            Self::Away
        } else if chosen == Self::Available && quiet_for >= IDLE_AFTER {
            Self::Idle
        } else {
            chosen
        }
    }

    /// Whether the person's face dims.
    #[must_use]
    pub const fn dims(self) -> bool {
        matches!(self, Self::Away | Self::Idle)
    }

    /// The stored and wire word.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Focusing => "focusing",
            Self::Away => "away",
            Self::Idle => "idle",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "available" => Some(Self::Available),
            "focusing" => Some(Self::Focusing),
            "away" => Some(Self::Away),
            "idle" => Some(Self::Idle),
            _ => None,
        }
    }
}
