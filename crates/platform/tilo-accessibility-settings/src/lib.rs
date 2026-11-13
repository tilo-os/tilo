//! The `accessibility` section: pointer, typing, hearing, vision and
//! screen-reader assists, one model for Settings, the compositor and the shell.
//! Toolkit apps read the screen-reader keys through the same definitions.
use std::ops::RangeInclusive;

use settings_store::dictation::DictationLanguage;
use settings_store::labelled_choice;
use settings_store::magnifier::MagnifierView;

pub use settings_store::accessibility::{
    DEFAULT_SCREEN_READER_RATE, SECTION, ScreenReaderSettings, ScreenReaderVoice,
};

pub const DEFAULT_CURSOR_SIZE: u32 = 24;
pub const CURSOR_SIZE_RANGE: RangeInclusive<u32> = DEFAULT_CURSOR_SIZE..=64;

pub const DEFAULT_CURSOR_THEME: &str = "Adwaita";

pub const CURSOR_THEME_MAX_CHARS: usize = 48;

/// A whole name of [`CURSOR_THEME_MAX_CHARS`] characters in its longest
/// UTF-8 form.
const CURSOR_THEME_BYTES: usize = CURSOR_THEME_MAX_CHARS * 4;

/// The name of a pointer theme: the directory it is installed under, never a
/// path. Inline, so a section holding one stays `Copy` for the compositor.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CursorTheme {
    /// Zero-padded; a name holds no control character, so the first zero ends it.
    bytes: [u8; CURSOR_THEME_BYTES],
}

impl CursorTheme {
    /// `name` as a pointer theme, or `None` when it is empty, too long, hidden or
    /// a path.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        let name = name.trim();
        (!name.is_empty()
            && name.chars().count() <= CURSOR_THEME_MAX_CHARS
            && !name.starts_with('.')
            && !name.contains(['/', '\\'])
            && !name.chars().any(char::is_control))
        .then(|| Self::of(name))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        let end = self
            .bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(CURSOR_THEME_BYTES);
        let bytes = self.bytes.get(..end).unwrap_or_default();
        std::str::from_utf8(bytes).unwrap_or_default()
    }

    fn of(name: &str) -> Self {
        let mut bytes = [0; CURSOR_THEME_BYTES];
        if let Some(start) = bytes.get_mut(..name.len()) {
            start.copy_from_slice(name.as_bytes());
        }
        Self { bytes }
    }
}

impl Default for CursorTheme {
    fn default() -> Self {
        Self::of(DEFAULT_CURSOR_THEME)
    }
}

impl std::fmt::Display for CursorTheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Debug for CursorTheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl std::ops::Deref for CursorTheme {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for CursorTheme {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<CursorTheme> for String {
    fn from(theme: CursorTheme) -> Self {
        theme.as_str().to_owned()
    }
}

impl ::serde::Serialize for CursorTheme {
    fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> ::serde::Deserialize<'de> for CursorTheme {
    fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::new(&name).ok_or_else(|| {
            ::serde::de::Error::custom(format!("`{name}` is not the name of a pointer theme"))
        })
    }
}

#[cfg(feature = "model")]
impl tilo_resource::Settable for CursorTheme {}

#[cfg(feature = "model")]
impl tilo_resource::Model for CursorTheme {
    tilo_resource::leaf_paths!();

    fn value_schema() -> ::serde_json::Value {
        ::serde_json::json!({"type": "string"})
    }

    fn field_type(types: &mut tilo_resource::TypeSet) -> tilo_resource::FieldType {
        let _ = types;
        tilo_resource::FieldType::Text
    }

    fn to_json(&self) -> ::serde_json::Value {
        ::serde_json::Value::from(self.as_str())
    }

    fn from_json(value: &::serde_json::Value) -> Result<Self, tilo_resource::ModelError> {
        match value {
            ::serde_json::Value::String(name) => Self::new(name).ok_or_else(|| {
                tilo_resource::ModelError::invalid(format!(
                    "`{name}` is not the name of a pointer theme"
                ))
            }),
            ::serde_json::Value::Null => Ok(Self::default()),
            _ => Err(tilo_resource::ModelError::shape("text")),
        }
    }

    fn load(
        doc: &tilo_resource::Doc,
        value: &tilo_resource::__private::Value<'_>,
        id: &tilo_resource::__private::ObjId,
    ) -> Result<Self, tilo_resource::ModelError> {
        let name = <String as tilo_resource::Model>::load(doc, value, id)?;
        Ok(Self::new(&name).unwrap_or_default())
    }

    fn missing() -> Result<Self, tilo_resource::ModelError> {
        Ok(Self::default())
    }

    fn store(
        &self,
        doc: &mut tilo_resource::Doc,
        slot: tilo_resource::Slot<'_>,
    ) -> Result<(), tilo_resource::ModelError> {
        <String as tilo_resource::Model>::store(&self.as_str().to_owned(), doc, slot)
    }

    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }

    fn store_changed(
        &self,
        before: &Self,
        doc: &mut tilo_resource::Doc,
        slot: tilo_resource::Slot<'_>,
        dirty: &tilo_resource::Delta,
    ) -> Result<(), tilo_resource::ModelError> {
        <String as tilo_resource::Model>::store_changed(
            &self.as_str().to_owned(),
            &before.as_str().to_owned(),
            doc,
            slot,
            dirty,
        )
    }

    fn edit(
        doc: &mut tilo_resource::Doc,
        slot: tilo_resource::Slot<'_>,
        path: &[tilo_resource::Segment],
        op: &tilo_resource::EditOp,
        _cx: &mut tilo_resource::ActionCx,
    ) -> Result<(), tilo_resource::ModelError> {
        tilo_resource::edit_whole::<Self>(doc, slot, path, op)
    }

    fn collect_text(&self, out: &mut Vec<String>) {
        <String as tilo_resource::Model>::collect_text(&self.as_str().to_owned(), out);
    }
}

pub const BOUNCE_KEYS_DEFAULT_DELAY_MS: u32 = 300;
pub const BOUNCE_KEYS_DELAY_RANGE: RangeInclusive<u32> = 50..=1_000;

pub const SWITCH_SCAN_RANGE_MS: RangeInclusive<u16> = 500..=5_000;

/// Defines `AccessibilitySettings` in the calling crate: the shell's plain copy
/// here, Settings' copy with its record attributes. One definition keeps both
/// from disagreeing about a key, a default or a range.
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
            pub large_cursor: bool,
            pub cursor_size: u32,
            pub cursor_theme: $crate::CursorTheme,
            $(#[$readonly])*
            pub screen_reader: bool,
            /// When the on-screen keyboard shows: with no hardware keyboard,
            /// always, or never.
            pub on_screen_keyboard: OnScreenKeyboard,
            /// Latch modifier keys so chords can be typed one key at a time.
            pub sticky_keys: bool,
            pub slow_keys: bool,
            pub bounce_keys: bool,
            pub bounce_keys_delay_ms: u32,
            pub bounce_keys_alert: bool,
            pub mouse_keys: bool,
            /// Flash the screen instead of (or alongside) an audible alert.
            pub visual_alerts: bool,
            pub color_filter: ColorFilter,
            pub magnifier_zoom: u8,
            pub magnifier_view: MagnifierView,
            $(#[$readonly])*
            pub magnifier_last_zoom: u8,
            pub cursor_crosshair: bool,
            pub thick_text_caret: bool,
            pub switch_control: bool,
            pub switch_key_pair: SwitchKeyPair,
            /// Automatic scan period in milliseconds; zero means manual two-switch scanning.
            pub switch_scan_ms: u16,
            /// Normalised screen-reader speech rate, where 100 is the engine default.
            pub screen_reader_rate: u16,
            pub screen_reader_voice: ScreenReaderVoice,
            /// Personal recognition language; selecting it never grants microphone access.
            pub dictation_language: DictationLanguage,
            pub dictation_literal: bool,
        }

        impl Default for AccessibilitySettings {
            fn default() -> Self {
                Self {
                    large_cursor: false,
                    cursor_size: $crate::DEFAULT_CURSOR_SIZE,
                    cursor_theme: $crate::CursorTheme::default(),
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
                    magnifier_last_zoom: $crate::__private::DEFAULT_ZOOM,
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
                if let Some(value) = key(section, "cursor_theme") {
                    settings.cursor_theme = value;
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
                if let Some(value) = key(section, $crate::__private::LAST_ZOOM_KEY) {
                    settings.magnifier_last_zoom = value;
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
                settings.magnifier_last_zoom = $crate::__private::remembered_zoom(settings.magnifier_last_zoom);
                settings.screen_reader_rate =
                    $crate::__private::settings_store::accessibility::clamped_rate(settings.screen_reader_rate);
                settings
            }

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

            pub fn switch_scan(self) -> Option<::std::time::Duration> {
                $crate::__private::SWITCH_SCAN_RANGE_MS
                    .contains(&self.switch_scan_ms)
                    .then(|| ::std::time::Duration::from_millis(u64::from(self.switch_scan_ms)))
            }

            pub fn step_magnifier(&mut self, step: $crate::__private::MagnifierStep) {
                let (zoom, remembered) = $crate::__private::step_zoom(
                    self.magnifier_zoom,
                    self.magnifier_last_zoom,
                    step,
                );
                self.magnifier_zoom = zoom;
                self.magnifier_last_zoom = remembered;
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

            pub fn bounce_delay_label(ms: u32) -> &'static str {
                $crate::__private::closest_label(Self::BOUNCE_DELAYS, ms)
            }

            pub fn bounce_delay_ms(label: &str) -> Option<u32> {
                $crate::__private::labelled(Self::BOUNCE_DELAYS, label)
            }

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
    pub use settings_store::magnifier::{
        DEFAULT_ZOOM, LAST_ZOOM_KEY, MAX_ZOOM, MagnifierState, MagnifierStep, MagnifierView, OFF,
        remembered_zoom, step as step_zoom,
    };

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
    ColorFilter {
        #[default]
        None => "Off",
        Grayscale => "Grayscale",
        Inverted => "Inverted",
        GrayscaleInverted => "Grayscale + inverted",
    }
);

labelled_choice!(
    OnScreenKeyboard {
        #[default]
        Auto => "Automatic",
        Always => "Always",
        Never => "Never",
    }
);

labelled_choice!(
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
    fn a_theme_is_a_name_and_anything_else_keeps_the_default() {
        assert_eq!(DEFAULT_CURSOR_THEME, "Adwaita");
        assert_eq!(
            read(serde_json::json!({})).cursor_theme.as_str(),
            DEFAULT_CURSOR_THEME
        );
        assert_eq!(
            read(serde_json::json!({"cursor_theme": " Bibata-Modern-Ice "}))
                .cursor_theme
                .as_str(),
            "Bibata-Modern-Ice",
            "the spaces around a name are not part of it"
        );
        for theme in [
            serde_json::json!(""),
            serde_json::json!("   "),
            serde_json::json!("/usr/share/icons/Adwaita"),
            serde_json::json!("../../../etc/passwd"),
            serde_json::json!(".hidden"),
            serde_json::json!("x".repeat(CURSOR_THEME_MAX_CHARS + 1)),
            serde_json::json!(42),
        ] {
            assert_eq!(
                read(serde_json::json!({"cursor_theme": theme}))
                    .cursor_theme
                    .as_str(),
                DEFAULT_CURSOR_THEME,
                "{theme} is not a theme's name"
            );
        }
    }

    #[test]
    fn a_theme_name_holds_its_text_and_nothing_else() {
        let theme = CursorTheme::new("Bibata-Modern-Ice").expect("a name");
        assert_eq!(theme.as_str(), "Bibata-Modern-Ice");
        assert_eq!(String::from(theme), "Bibata-Modern-Ice");
        assert_eq!(
            theme,
            CursorTheme::new(" Bibata-Modern-Ice").expect("a name")
        );
        assert!(CursorTheme::new("two/parts").is_none());
        assert!(CursorTheme::new("").is_none());
        assert_eq!(CursorTheme::default().as_str(), DEFAULT_CURSOR_THEME);
        let long = "ü".repeat(CURSOR_THEME_MAX_CHARS);
        assert_eq!(
            CursorTheme::new(&long).expect("64 characters").as_str(),
            long,
            "a name may be non-ASCII, as long as it is not too long"
        );
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
