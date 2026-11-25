//! The `accessibility` section's screen-reader settings, which every toolkit app reads to run
//! its narrator. The whole section, with the pointer, typing, hearing and vision assists
//! Settings edits and the compositor applies, is the `accessibility-settings` crate's model; it
//! reads these through the definitions here, so the two cannot disagree about a default or a
//! range.

use serde::Deserialize;
use serde_json::{Map, Value};
use std::ops::RangeInclusive;

pub const SECTION: &str = "accessibility";

pub const DEVICE_SECTION: &str = "accessibility_device";

/// The leaf that turns the screen reader on.
pub const SCREEN_READER_KEY: &str = "screen_reader";

/// Normalised speech rate, where 100 is the engine default.
pub const DEFAULT_SCREEN_READER_RATE: u16 = 100;
pub const SCREEN_READER_RATE_RANGE: RangeInclusive<u16> = 50..=200;

/// Backend-neutral screen-reader voice/language preset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[serde(rename_all = "snake_case")]
pub enum ScreenReaderVoice {
    #[default]
    System,
    EnglishUs,
    EnglishGb,
    German,
    French,
    Spanish,
}

impl ScreenReaderVoice {
    pub const ALL: &'static [Self] = &[
        Self::System,
        Self::EnglishUs,
        Self::EnglishGb,
        Self::German,
        Self::French,
        Self::Spanish,
    ];
    pub const OPTIONS: &'static [&'static str] = &[
        "System voice",
        "English (US)",
        "English (UK)",
        "German",
        "French",
        "Spanish",
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System voice",
            Self::EnglishUs => "English (US)",
            Self::EnglishGb => "English (UK)",
            Self::German => "German",
            Self::French => "French",
            Self::Spanish => "Spanish",
        }
    }

    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|choice| choice.label() == label)
    }

    #[must_use]
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenReaderSettings {
    pub enabled: bool,
    /// Speech rate multiplier: 1.0 is the engine default.
    pub rate: f32,
    pub voice: ScreenReaderVoice,
}

impl ScreenReaderSettings {
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
            key(section, SCREEN_READER_KEY).unwrap_or(false),
            key(section, "screen_reader_rate").unwrap_or(DEFAULT_SCREEN_READER_RATE),
            key(section, "screen_reader_voice").unwrap_or_default(),
        )
    }

    pub fn new(enabled: bool, rate_percent: u16, voice: ScreenReaderVoice) -> Self {
        Self {
            enabled,
            rate: f32::from(clamped_rate(rate_percent)) / 100.0,
            voice,
        }
    }
}

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
    fn screen_reader_settings_clamp_their_rate_and_name_their_voices() {
        {
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
    }
}
