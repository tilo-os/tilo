//! The `accessibility` section's screen-reader keys, which every toolkit app
//! reads to run its narrator. The whole section, with the pointer, typing,
//! hearing and vision assists Settings edits and the compositor applies, is
//! the `accessibility-settings` crate's model; it reads these keys through
//! the definitions here, so the two cannot disagree about a default or a range.
use serde::Deserialize;
use serde_json::{Map, Value};
use std::ops::RangeInclusive;

pub const SECTION: &str = "accessibility";

/// Normalised speech rate, where 100 is the engine default.
pub const DEFAULT_SCREEN_READER_RATE: u16 = 100;
/// The speech rates reading clamps to.
pub const SCREEN_READER_RATE_RANGE: RangeInclusive<u16> = 50..=200;

/// Generates a persisted choice with the labels Settings offers for it.
#[doc(hidden)]
#[macro_export]
macro_rules! labelled_choice {
    ($(#[$meta:meta])* $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $label:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        #[cfg_attr(feature = "model", derive(tilo_resource::Model))]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const OPTIONS: &'static [&'static str] = &[$($label),+];

            pub fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }

            pub fn from_label(label: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|choice| choice.label() == label)
            }
        }
    };
}

labelled_choice!(
    /// Backend-neutral screen-reader voice/language preset.
    ScreenReaderVoice {
        #[default]
        System => "System voice",
        EnglishUs => "English (US)",
        EnglishGb => "English (UK)",
        German => "German",
        French => "French",
        Spanish => "Spanish",
    }
);

impl ScreenReaderVoice {
    /// The speech engine's voice id; `None` keeps the engine's own default.
    pub fn engine_voice(self) -> Option<&'static str> {
        match self {
            Self::System => None,
            Self::EnglishUs => Some("en-us"),
            Self::EnglishGb => Some("en-gb"),
            Self::German => Some("de"),
            Self::French => Some("fr"),
            Self::Spanish => Some("es"),
        }
    }
}

/// What the screen reader should do, as the person configured it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenReaderSettings {
    pub enabled: bool,
    /// Speech rate multiplier: 1.0 is the engine default.
    pub rate: f32,
    pub voice: ScreenReaderVoice,
}

impl ScreenReaderSettings {
    pub fn load() -> Self {
        Self::from_section(&super::load_section(SECTION))
    }

    /// The screen-reader keys of `section`; a missing or malformed key keeps
    /// its own default.
    pub fn from_section(section: &Map<String, Value>) -> Self {
        fn key<T: for<'de> Deserialize<'de>>(
            section: &Map<String, Value>,
            name: &str,
        ) -> Option<T> {
            section
                .get(name)
                .and_then(|value| T::deserialize(value).ok())
        }
        Self::new(
            key(section, "screen_reader").unwrap_or(false),
            key(section, "screen_reader_rate").unwrap_or(DEFAULT_SCREEN_READER_RATE),
            key(section, "screen_reader_voice").unwrap_or_default(),
        )
    }

    /// The settings for a stored rate percentage, clamped to the offered range.
    pub fn new(enabled: bool, rate_percent: u16, voice: ScreenReaderVoice) -> Self {
        Self {
            enabled,
            rate: f32::from(clamped_rate(rate_percent)) / 100.0,
            voice,
        }
    }
}

/// `rate_percent` within [`SCREEN_READER_RATE_RANGE`].
pub fn clamped_rate(rate_percent: u16) -> u16 {
    rate_percent.clamp(
        *SCREEN_READER_RATE_RANGE.start(),
        *SCREEN_READER_RATE_RANGE.end(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            panic!("not an object")
        };
        map
    }

    #[test]
    fn rate_is_clamped_and_unknown_voices_fall_back_to_the_system_voice() {
        let settings = ScreenReaderSettings::from_section(&section(serde_json::json!({
            "screen_reader": true,
            "screen_reader_rate": 900,
            "screen_reader_voice": "klingon",
        })));
        assert!(settings.enabled);
        assert!((settings.rate - 2.0).abs() < f32::EPSILON);
        assert_eq!(settings.voice, ScreenReaderVoice::System);
        assert_eq!(settings.voice.engine_voice(), None);

        let settings = ScreenReaderSettings::from_section(&section(serde_json::json!({
            "screen_reader_rate": 65,
            "screen_reader_voice": "english_gb",
        })));
        assert!(!settings.enabled);
        assert!((settings.rate - 0.65).abs() < f32::EPSILON);
        assert_eq!(settings.voice.engine_voice(), Some("en-gb"));
    }

    #[test]
    fn labels_round_trip() {
        for voice in ScreenReaderVoice::ALL {
            assert_eq!(ScreenReaderVoice::from_label(voice.label()), Some(*voice));
        }
    }
}
