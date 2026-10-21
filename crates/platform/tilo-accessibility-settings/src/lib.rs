//! The `accessibility` section: the pointer, typing, hearing, vision and
//! screen-reader assists. Settings edits this model and the compositor and
//! the shell read it, so they cannot disagree about a key, its default or its
//! range. Toolkit apps read only the screen-reader keys, through
//! `settings_store::accessibility`, which this model shares its definitions
//! with; keeping the rest here leaves it out of the shared runtime. The
//! vision comforts that theme the whole system (high contrast, reduced
//! motion, text size) live in the `appearance` section instead.
use std::ops::RangeInclusive;

use settings_store::dictation::DictationLanguage;
use settings_store::labelled_choice;
use settings_store::magnifier::MagnifierView;

pub use settings_store::accessibility::{
    DEFAULT_SCREEN_READER_RATE, SECTION, ScreenReaderSettings, ScreenReaderVoice,
};

/// The pointer size without the large cursor, in logical pixels.
pub const DEFAULT_CURSOR_SIZE: u32 = 24;
pub const CURSOR_SIZE_RANGE: RangeInclusive<u32> = DEFAULT_CURSOR_SIZE..=64;

/// Default bounce-keys window: a second press of the same key this soon
/// after its release is treated as a bounce, not a keystroke.
pub const BOUNCE_KEYS_DEFAULT_DELAY_MS: u32 = 300;
pub const BOUNCE_KEYS_DELAY_RANGE: RangeInclusive<u32> = 50..=1_000;

/// Automatic switch-scan periods outside this range mean manual scanning.
pub const SWITCH_SCAN_RANGE_MS: RangeInclusive<u16> = 500..=5_000;

/// Defines `AccessibilitySettings`, the person's assists, in the calling
/// crate: the shell's plain copy here, and Settings' copy that adds its
/// record attributes (`$record` on the type, `$readonly` on the keys only
/// the screen reader's own controls change). One definition for both keeps
/// them from disagreeing about a key, a default or a range, and Settings'
/// copy keeps the record's generated code inside the one app that edits it.
/// The caller has the field types in scope by their own names
/// (`ColorFilter`, `DictationLanguage`, `MagnifierView`, `OnScreenKeyboard`,
/// `ScreenReaderVoice`, `SwitchKeyPair`), so a record's stored field types
/// read the same in its surface whichever crate defines it.
#[doc(hidden)]
#[macro_export]
macro_rules! accessibility_settings {
    ($(#[$record:meta])* ; $(#[$readonly:meta])*) => {
        /// The person's assists. Reading never fails: a missing or malformed key
        /// keeps its own default without resetting the others.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        $(#[$record])*
        #[serde(from = "::serde_json::Map<::std::string::String, ::serde_json::Value>")]
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
            $(#[$readonly])*
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
            /// Desktop magnification factor; [`OFF`]($crate::__private::OFF) means disabled.
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
            $(#[$readonly])*
            pub screen_reader_rate: u16,
            /// Backend-neutral voice/language preset.
            $(#[$readonly])*
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
                    cursor_size: $crate::DEFAULT_CURSOR_SIZE,
                    screen_reader: false,
                    on_screen_keyboard: $crate::OnScreenKeyboard::Auto,
                    sticky_keys: false,
                    slow_keys: false,
                    bounce_keys: false,
                    bounce_keys_delay_ms: $crate::BOUNCE_KEYS_DEFAULT_DELAY_MS,
                    bounce_keys_alert: false,
                    mouse_keys: false,
                    visual_alerts: false,
                    color_filter: $crate::ColorFilter::None,
                    magnifier_zoom: $crate::__private::OFF,
                    magnifier_view: $crate::__private::MagnifierView::Lens,
                    cursor_crosshair: false,
                    thick_text_caret: false,
                    switch_control: false,
                    switch_key_pair: $crate::SwitchKeyPair::F6F7,
                    switch_scan_ms: 0,
                    screen_reader_rate: $crate::DEFAULT_SCREEN_READER_RATE,
                    screen_reader_voice: $crate::ScreenReaderVoice::System,
                    dictation_language: $crate::__private::DictationLanguage::default(),
                    dictation_literal: false,
                }
            }
        }

        impl ::core::convert::From<::serde_json::Map<::std::string::String, ::serde_json::Value>> for AccessibilitySettings {
            fn from(section: ::serde_json::Map<::std::string::String, ::serde_json::Value>) -> Self {
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
                Self::from_section(&$crate::__private::settings_store::load_section($crate::SECTION))
            }

            pub fn from_section(
            section: &::serde_json::Map<::std::string::String, ::serde_json::Value>,
        ) -> Self {
                /// One reader per value type, shared by every key of that type.
                #[inline(never)]
                fn key<T: for<'de> ::serde::Deserialize<'de>>(
                    section: &::serde_json::Map<::std::string::String, ::serde_json::Value>,
                    name: &str,
                ) -> Option<T> {
                    section
                        .get(name)
                        .and_then(|value| T::deserialize(value).ok())
                }
                let mut settings = Self::default();
                if let Some(value) = key(section, "large_cursor") {
                    settings.large_cursor = value;
                }
                if let Some(value) = key(section, "cursor_size") {
                    settings.cursor_size = value;
                }
                if let Some(value) = key(section, "screen_reader") {
                    settings.screen_reader = value;
                }
                if let Some(value) = key(section, "on_screen_keyboard") {
                    settings.on_screen_keyboard = value;
                }
                if let Some(value) = key(section, "sticky_keys") {
                    settings.sticky_keys = value;
                }
                if let Some(value) = key(section, "slow_keys") {
                    settings.slow_keys = value;
                }
                if let Some(value) = key(section, "bounce_keys") {
                    settings.bounce_keys = value;
                }
                if let Some(value) = key(section, "bounce_keys_delay_ms") {
                    settings.bounce_keys_delay_ms = value;
                }
                if let Some(value) = key(section, "bounce_keys_alert") {
                    settings.bounce_keys_alert = value;
                }
                if let Some(value) = key(section, "mouse_keys") {
                    settings.mouse_keys = value;
                }
                if let Some(value) = key(section, "visual_alerts") {
                    settings.visual_alerts = value;
                }
                if let Some(value) = key(section, "color_filter") {
                    settings.color_filter = value;
                }
                if let Some(value) = key(section, "magnifier_zoom") {
                    settings.magnifier_zoom = value;
                }
                if let Some(value) = key(section, "magnifier_view") {
                    settings.magnifier_view = value;
                }
                if let Some(value) = key(section, "cursor_crosshair") {
                    settings.cursor_crosshair = value;
                }
                if let Some(value) = key(section, "thick_text_caret") {
                    settings.thick_text_caret = value;
                }
                if let Some(value) = key(section, "switch_control") {
                    settings.switch_control = value;
                }
                if let Some(value) = key(section, "switch_key_pair") {
                    settings.switch_key_pair = value;
                }
                if let Some(value) = key(section, "switch_scan_ms") {
                    settings.switch_scan_ms = value;
                }
                if let Some(value) = key(section, "screen_reader_rate") {
                    settings.screen_reader_rate = value;
                }
                if let Some(value) = key(section, "screen_reader_voice") {
                    settings.screen_reader_voice = value;
                }
                if let Some(value) = key(section, "dictation_language") {
                    settings.dictation_language = value;
                }
                if let Some(value) = key(section, "dictation_literal") {
                    settings.dictation_literal = value;
                }
                settings.cursor_size = $crate::__private::clamp(settings.cursor_size, &$crate::__private::CURSOR_SIZE_RANGE);
                settings.bounce_keys_delay_ms =
                    $crate::__private::clamp(settings.bounce_keys_delay_ms, &$crate::__private::BOUNCE_KEYS_DELAY_RANGE);
                settings.magnifier_zoom = settings.magnifier_zoom.clamp($crate::__private::OFF, $crate::__private::MAX_ZOOM);
                settings.screen_reader_rate =
                    $crate::__private::settings_store::accessibility::clamped_rate(settings.screen_reader_rate);
                settings
            }

            /// The pointer size the compositor draws.
            pub fn effective_cursor_size(self) -> u32 {
                if self.large_cursor {
                    self.cursor_size
                } else {
                    $crate::DEFAULT_CURSOR_SIZE
                }
            }

            /// The bounce-keys window, or zero when bounce keys is off.
            pub fn bounce_window(self) -> ::std::time::Duration {
                if self.bounce_keys {
                    ::std::time::Duration::from_millis(u64::from(self.bounce_keys_delay_ms))
                } else {
                    ::std::time::Duration::ZERO
                }
            }

            /// The automatic switch-scan period; `None` scans manually.
            pub fn switch_scan(self) -> Option<::std::time::Duration> {
                $crate::__private::SWITCH_SCAN_RANGE_MS
                    .contains(&self.switch_scan_ms)
                    .then(|| ::std::time::Duration::from_millis(u64::from(self.switch_scan_ms)))
            }

            pub fn magnifier(self) -> $crate::__private::MagnifierState {
                $crate::__private::MagnifierState {
                    zoom: self.magnifier_zoom,
                    view: self.magnifier_view,
                }
            }

            pub fn screen_reader(self) -> $crate::ScreenReaderSettings {
                $crate::ScreenReaderSettings::new(
                    self.screen_reader,
                    self.screen_reader_rate,
                    self.screen_reader_voice,
                )
            }

            /// The offered bounce-keys window closest to `ms`.
            pub fn bounce_delay_label(ms: u32) -> &'static str {
                $crate::__private::closest_label(Self::BOUNCE_DELAYS, ms)
            }

            /// Map a bounce-keys label back to milliseconds, if recognised.
            pub fn bounce_delay_ms(label: &str) -> Option<u32> {
                $crate::__private::labelled(Self::BOUNCE_DELAYS, label)
            }

            /// The offered pointer size closest to `size`.
            pub fn cursor_size_label(size: u32) -> &'static str {
                $crate::__private::closest_label(Self::CURSOR_SIZES, size)
            }
        }

    };
}

accessibility_settings!(;);

#[doc(hidden)]
pub mod __private {
    pub use crate::{BOUNCE_KEYS_DELAY_RANGE, CURSOR_SIZE_RANGE, SWITCH_SCAN_RANGE_MS};
    pub use settings_store;
    pub use settings_store::dictation::DictationLanguage;
    pub use settings_store::magnifier::{MAX_ZOOM, MagnifierState, MagnifierView, OFF};

    pub fn clamp<T: Ord + Copy>(value: T, range: &std::ops::RangeInclusive<T>) -> T {
        value.clamp(*range.start(), *range.end())
    }

    pub fn closest_label(options: &[(&'static str, u32)], value: u32) -> &'static str {
        options
            .iter()
            .min_by_key(|(_, option)| option.abs_diff(value))
            .map_or("", |(label, _)| *label)
    }

    pub fn labelled(options: &[(&'static str, u32)], label: &str) -> Option<u32> {
        options
            .iter()
            .find(|(option, _)| *option == label)
            .map(|(_, value)| *value)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use settings_store::magnifier::MAX_ZOOM;
    use std::time::Duration;

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
        for filter in ColorFilter::ALL {
            assert_eq!(ColorFilter::from_label(filter.label()), Some(*filter));
        }
        assert_eq!(
            AccessibilitySettings::cursor_size_label(40),
            "Large",
            "the closest offered size"
        );
        assert_eq!(AccessibilitySettings::bounce_delay_ms("500 ms"), Some(500));
    }
}
