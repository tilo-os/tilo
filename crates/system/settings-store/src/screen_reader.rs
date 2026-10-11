//! The screen-reader keys of the `accessibility` section, read the same way
//! by every app and by the desktop narrator so they speak with one voice.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const SECTION: &str = "accessibility";

/// Normalised speech rate, where 100 is the engine default.
pub const DEFAULT_RATE: u16 = 100;
const RATE_RANGE: std::ops::RangeInclusive<u16> = 50..=200;

/// Backend-neutral voice/language preset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    const ALL: [Self; 6] = [
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

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|voice| voice.label() == label)
    }

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

    pub fn from_section(section: &Map<String, Value>) -> Self {
        let rate_pct = section
            .get("screen_reader_rate")
            .and_then(Value::as_u64)
            .and_then(|rate| u16::try_from(rate).ok())
            .unwrap_or(DEFAULT_RATE)
            .clamp(*RATE_RANGE.start(), *RATE_RANGE.end());
        Self {
            enabled: section
                .get("screen_reader")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            rate: f32::from(rate_pct) / 100.0,
            voice: section
                .get("screen_reader_voice")
                .cloned()
                .and_then(|voice| serde_json::from_value(voice).ok())
                .unwrap_or_default(),
        }
    }
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
        for (voice, label) in ScreenReaderVoice::ALL
            .into_iter()
            .zip(ScreenReaderVoice::OPTIONS)
        {
            assert_eq!(voice.label(), *label);
            assert_eq!(ScreenReaderVoice::from_label(label), Some(voice));
        }
    }
}
