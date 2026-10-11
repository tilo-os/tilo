//! The `accessibility` section: the pointer, typing, hearing, vision and
//! screen-reader assists. Settings edits this model and the compositor, the
//! shell and every toolkit app read it, so they cannot disagree about a key,
//! its default or its range. The vision comforts that theme the whole system
//! (high contrast, reduced motion, text size) live in the `appearance`
//! section instead.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::ops::RangeInclusive;
use std::time::Duration;

use crate::dictation::DictationLanguage;
use crate::magnifier::{MAX_ZOOM, MagnifierState, MagnifierView, OFF};

pub const SECTION: &str = "accessibility";

/// The pointer size without the large cursor, in logical pixels.
pub const DEFAULT_CURSOR_SIZE: u32 = 24;
const CURSOR_SIZE_RANGE: RangeInclusive<u32> = DEFAULT_CURSOR_SIZE..=64;

/// Default bounce-keys window: a second press of the same key this soon
/// after its release is treated as a bounce, not a keystroke.
pub const BOUNCE_KEYS_DEFAULT_DELAY_MS: u32 = 300;
const BOUNCE_KEYS_DELAY_RANGE: RangeInclusive<u32> = 50..=1_000;

/// Normalised speech rate, where 100 is the engine default.
pub const DEFAULT_SCREEN_READER_RATE: u16 = 100;
const SCREEN_READER_RATE_RANGE: RangeInclusive<u16> = 50..=200;

/// Automatic switch-scan periods outside this range mean manual scanning.
const SWITCH_SCAN_RANGE_MS: RangeInclusive<u16> = 500..=5_000;

/// The person's assists. Reading never fails: a missing or malformed key
/// keeps its own default without resetting the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Map<String, Value>")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "flat user-facing preferences record loaded from settings.json; each bool is an \
              independently meaningful serialized setting, not a related toggle group"
)]
pub struct AccessibilitySettings {
    /// Enlarge the mouse pointer to [`Self::cursor_size`].
    pub large_cursor: bool,
    /// Pointer size in logical pixels (the compositor's cursor theme size).
    pub cursor_size: u32,
    /// Run the screen reader.
    pub screen_reader: bool,
    /// When the on-screen keyboard runs.
    pub on_screen_keyboard: OnScreenKeyboard,
    /// Latch modifier keys so chords can be typed one key at a time.
    pub sticky_keys: bool,
    /// Require a key to be held briefly before it registers, ignoring
    /// accidental brushes.
    pub slow_keys: bool,
    /// Ignore a second press of the same key soon after its release.
    pub bounce_keys: bool,
    /// The bounce-keys window in milliseconds.
    pub bounce_keys_delay_ms: u32,
    /// Flash the visual alert when bounce keys ignores a press.
    pub bounce_keys_alert: bool,
    /// Move the pointer with the numeric keypad.
    pub mouse_keys: bool,
    /// Flash the screen instead of (or alongside) an audible alert.
    pub visual_alerts: bool,
    /// Whole-desktop colour transform owned by the compositor.
    pub color_filter: ColorFilter,
    /// Desktop magnification factor; [`OFF`] means disabled.
    pub magnifier_zoom: u8,
    /// A lens around the point of interest, or the whole screen magnified.
    pub magnifier_view: MagnifierView,
    /// Paint full-screen guide lines through the hardware pointer.
    pub cursor_crosshair: bool,
    /// Make text insertion carets wider in every Tilo toolkit application.
    pub thick_text_caret: bool,
    /// Turn switch signals into standard scan-next/activate actions.
    pub switch_control: bool,
    /// Which ordinary key pair a two-switch device emits.
    pub switch_key_pair: SwitchKeyPair,
    /// Automatic scan period in milliseconds; zero means manual two-switch scanning.
    pub switch_scan_ms: u16,
    /// Normalised screen-reader speech rate, where 100 is the engine default.
    pub screen_reader_rate: u16,
    /// Backend-neutral voice/language preset.
    pub screen_reader_voice: ScreenReaderVoice,
    /// Personal recognition language; selecting it never grants microphone access.
    pub dictation_language: DictationLanguage,
    /// Preserve recognised speech without removing explicit self-repairs.
    pub dictation_literal: bool,
}

impl Default for AccessibilitySettings {
    fn default() -> Self {
        Self {
            large_cursor: false,
            cursor_size: DEFAULT_CURSOR_SIZE,
            screen_reader: false,
            on_screen_keyboard: OnScreenKeyboard::Auto,
            sticky_keys: false,
            slow_keys: false,
            bounce_keys: false,
            bounce_keys_delay_ms: BOUNCE_KEYS_DEFAULT_DELAY_MS,
            bounce_keys_alert: false,
            mouse_keys: false,
            visual_alerts: false,
            color_filter: ColorFilter::None,
            magnifier_zoom: OFF,
            magnifier_view: MagnifierView::Lens,
            cursor_crosshair: false,
            thick_text_caret: false,
            switch_control: false,
            switch_key_pair: SwitchKeyPair::F6F7,
            switch_scan_ms: 0,
            screen_reader_rate: DEFAULT_SCREEN_READER_RATE,
            screen_reader_voice: ScreenReaderVoice::System,
            dictation_language: DictationLanguage::default(),
            dictation_literal: false,
        }
    }
}

impl From<Map<String, Value>> for AccessibilitySettings {
    fn from(section: Map<String, Value>) -> Self {
        Self::from_section(&section)
    }
}

impl AccessibilitySettings {
    /// The offered bounce-keys windows as `(display label, milliseconds)`.
    pub const BOUNCE_DELAYS: &'static [(&'static str, u32)] = &[
        ("100 ms", 100),
        ("200 ms", 200),
        ("300 ms", 300),
        ("500 ms", 500),
        ("800 ms", 800),
    ];

    /// The offered pointer sizes as `(display label, size px)`.
    pub const CURSOR_SIZES: &'static [(&'static str, u32)] = &[
        ("Default", 24),
        ("Large", 36),
        ("Larger", 48),
        ("Largest", 64),
    ];

    pub fn load() -> Self {
        Self::from_section(&super::load_section(SECTION))
    }

    pub fn from_section(section: &Map<String, Value>) -> Self {
        let mut settings = Self::default();
        macro_rules! read {
            ($($field:ident),* $(,)?) => {$(
                if let Some(value) = section
                    .get(stringify!($field))
                    .and_then(|value| Deserialize::deserialize(value).ok())
                {
                    settings.$field = value;
                }
            )*};
        }
        read!(
            large_cursor,
            cursor_size,
            screen_reader,
            on_screen_keyboard,
            sticky_keys,
            slow_keys,
            bounce_keys,
            bounce_keys_delay_ms,
            bounce_keys_alert,
            mouse_keys,
            visual_alerts,
            color_filter,
            magnifier_zoom,
            magnifier_view,
            cursor_crosshair,
            thick_text_caret,
            switch_control,
            switch_key_pair,
            switch_scan_ms,
            screen_reader_rate,
            screen_reader_voice,
            dictation_language,
            dictation_literal,
        );
        settings.cursor_size = clamp(settings.cursor_size, &CURSOR_SIZE_RANGE);
        settings.bounce_keys_delay_ms =
            clamp(settings.bounce_keys_delay_ms, &BOUNCE_KEYS_DELAY_RANGE);
        settings.magnifier_zoom = settings.magnifier_zoom.clamp(OFF, MAX_ZOOM);
        settings.screen_reader_rate = clamp(settings.screen_reader_rate, &SCREEN_READER_RATE_RANGE);
        settings
    }

    /// The pointer size the compositor draws.
    pub fn effective_cursor_size(self) -> u32 {
        if self.large_cursor {
            self.cursor_size
        } else {
            DEFAULT_CURSOR_SIZE
        }
    }

    /// The bounce-keys window, or zero when bounce keys is off.
    pub fn bounce_window(self) -> Duration {
        if self.bounce_keys {
            Duration::from_millis(u64::from(self.bounce_keys_delay_ms))
        } else {
            Duration::ZERO
        }
    }

    /// The automatic switch-scan period; `None` scans manually.
    pub fn switch_scan(self) -> Option<Duration> {
        SWITCH_SCAN_RANGE_MS
            .contains(&self.switch_scan_ms)
            .then(|| Duration::from_millis(u64::from(self.switch_scan_ms)))
    }

    pub fn magnifier(self) -> MagnifierState {
        MagnifierState {
            zoom: self.magnifier_zoom,
            view: self.magnifier_view,
        }
    }

    pub fn screen_reader(self) -> ScreenReaderSettings {
        ScreenReaderSettings {
            enabled: self.screen_reader,
            rate: f32::from(self.screen_reader_rate) / 100.0,
            voice: self.screen_reader_voice,
        }
    }

    /// The offered bounce-keys window closest to `ms`.
    pub fn bounce_delay_label(ms: u32) -> &'static str {
        closest_label(Self::BOUNCE_DELAYS, ms)
    }

    /// Map a bounce-keys label back to milliseconds, if recognised.
    pub fn bounce_delay_ms(label: &str) -> Option<u32> {
        labelled(Self::BOUNCE_DELAYS, label)
    }

    /// The offered pointer size closest to `size`.
    pub fn cursor_size_label(size: u32) -> &'static str {
        closest_label(Self::CURSOR_SIZES, size)
    }

    /// Map a cursor-size label back to its pixel size, if recognised.
    pub fn cursor_size_px(label: &str) -> Option<u32> {
        labelled(Self::CURSOR_SIZES, label)
    }
}

fn clamp<T: Ord + Copy>(value: T, range: &RangeInclusive<T>) -> T {
    value.clamp(*range.start(), *range.end())
}

fn closest_label(options: &[(&'static str, u32)], value: u32) -> &'static str {
    options
        .iter()
        .min_by_key(|(_, option)| option.abs_diff(value))
        .map_or("", |(label, _)| *label)
}

fn labelled(options: &[(&'static str, u32)], label: &str) -> Option<u32> {
    options
        .iter()
        .find(|(option, _)| *option == label)
        .map(|(_, value)| *value)
}

/// Generates a persisted choice with the labels Settings offers for it.
macro_rules! labelled_choice {
    ($(#[$meta:meta])* $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $label:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            const ALL: &'static [Self] = &[$(Self::$variant),+];
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
    /// The whole-desktop colour transform.
    ColorFilter {
        #[default]
        None => "Off",
        Grayscale => "Grayscale",
        Inverted => "Inverted",
        GrayscaleInverted => "Grayscale + inverted",
    }
);

labelled_choice!(
    /// When the compositor runs the on-screen keyboard. `Auto` follows the
    /// hardware in use: a touchscreen with no physical typing since the last
    /// touch, or a convertible folded into tablet mode.
    OnScreenKeyboard {
        #[default]
        Auto => "Automatic",
        Always => "Always",
        Never => "Never",
    }
);

labelled_choice!(
    /// Which ordinary key pair a two-switch device emits.
    SwitchKeyPair {
        #[default]
        F6F7 => "F6 / F7",
        F9F10 => "F9 / F10",
        Digits12 => "1 / 2",
    }
);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn read(value: Value) -> AccessibilitySettings {
        let Value::Object(section) = value else {
            panic!("not an object")
        };
        AccessibilitySettings::from_section(&section)
    }

    #[test]
    fn a_malformed_key_keeps_only_its_own_default() {
        let settings = read(serde_json::json!({
            "sticky_keys": true,
            "cursor_size": "huge",
            "large_cursor": true,
            "color_filter": "sepia",
            "switch_key_pair": "f9_f10",
            "screen_reader_voice": "klingon",
        }));
        assert!(settings.sticky_keys);
        assert_eq!(settings.effective_cursor_size(), DEFAULT_CURSOR_SIZE);
        assert_eq!(settings.color_filter, ColorFilter::None);
        assert_eq!(settings.switch_key_pair, SwitchKeyPair::F9F10);
        assert_eq!(settings.screen_reader().voice, ScreenReaderVoice::System);
    }

    #[test]
    fn ranges_are_clamped() {
        let settings = read(serde_json::json!({
            "cursor_size": 900,
            "large_cursor": true,
            "bounce_keys": true,
            "bounce_keys_delay_ms": 1,
            "magnifier_zoom": 40,
            "screen_reader_rate": 900,
            "switch_scan_ms": 100,
        }));
        assert_eq!(settings.effective_cursor_size(), 64);
        assert_eq!(settings.bounce_window(), Duration::from_millis(50));
        assert_eq!(settings.magnifier_zoom, MAX_ZOOM);
        assert!((settings.screen_reader().rate - 2.0).abs() < f32::EPSILON);
        assert_eq!(settings.switch_scan(), None);
    }

    #[test]
    fn deserializing_is_as_tolerant_as_reading_a_section() {
        let settings: AccessibilitySettings =
            serde_json::from_value(serde_json::json!({"mouse_keys": true, "slow_keys": 3}))
                .unwrap();
        assert!(settings.mouse_keys);
        assert!(!settings.slow_keys);
        let round_trip: AccessibilitySettings =
            serde_json::from_value(serde_json::to_value(settings).unwrap()).unwrap();
        assert_eq!(round_trip, settings);
    }

    #[test]
    fn labels_round_trip() {
        for voice in ScreenReaderVoice::ALL {
            assert_eq!(ScreenReaderVoice::from_label(voice.label()), Some(*voice));
        }
        assert_eq!(
            AccessibilitySettings::cursor_size_label(40),
            "Large",
            "the closest offered size"
        );
        assert_eq!(AccessibilitySettings::bounce_delay_ms("500 ms"), Some(500));
    }
}
