//! The device's sign-in comforts: what the login screens start with before
//! anyone has signed in (docs/security/sign-in.md §3.4).
//!
//! They are the person's own settings, carried to where no person's
//! settings file is readable: the screen-reader keys of the `accessibility`
//! section ([`crate::accessibility`]), the `appearance` section's language,
//! contrast and text scale ([`crate::appearance`]) and the active keyboard
//! source ([`crate::hardware::KeyboardInputSource`]). `tilo-authd` keeps them
//! in device storage and mirrors them into the volume header as the
//! `tilo-comforts` token, which the initrd reads before the volume opens.
//!
//! That header is unauthenticated: someone holding the disk can write it. So
//! reading is forgiving and clamps every value, as the settings file's own
//! reads do, and the result is preferences only.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::accessibility::{
    DEFAULT_SCREEN_READER_RATE, ScreenReaderSettings, ScreenReaderVoice, clamped_rate,
};
use crate::appearance::{TEXT_SCALE_MAX, TEXT_SCALE_MIN};
use crate::hardware::KeyboardInputSource;

/// Whether a person's changes to these comforts apply to this device's login
/// screens too: a shared machine and a laptop of their own want different
/// answers. On unless they keep the two apart.
pub const APPLY_TO_SIGN_IN: crate::Key<bool> = crate::Key::new(
    crate::accessibility::DEVICE_SECTION,
    "apply_to_sign_in",
    || true,
);

/// The longest language tag kept. BCP 47 tags in use are far shorter.
const MAX_LANGUAGE_LEN: usize = 35;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "Map<String, Value>")]
pub struct SignInComforts {
    pub screen_reader: bool,
    /// Normalised speech rate, where 100 is the engine default.
    pub screen_reader_rate: u16,
    pub screen_reader_voice: ScreenReaderVoice,
    /// The interface language as a BCP 47 tag; `None` follows the system.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The XKB layout typed into the secret field; `None` keeps the
    /// machine's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xkb_layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xkb_variant: Option<String>,
    pub text_scale: f32,
    pub high_contrast: bool,
}

impl Default for SignInComforts {
    fn default() -> Self {
        Self {
            screen_reader: false,
            screen_reader_rate: DEFAULT_SCREEN_READER_RATE,
            screen_reader_voice: ScreenReaderVoice::System,
            language: None,
            xkb_layout: None,
            xkb_variant: None,
            text_scale: 1.0,
            high_contrast: false,
        }
    }
}

impl From<Map<String, Value>> for SignInComforts {
    fn from(map: Map<String, Value>) -> Self {
        Self::from_map(&map)
    }
}

impl SignInComforts {
    /// The comforts in `map`; a missing or malformed field keeps its default
    /// without resetting the others, and every value is clamped.
    pub fn from_map(map: &Map<String, Value>) -> Self {
        fn key<T: for<'de> Deserialize<'de>>(map: &Map<String, Value>, name: &str) -> Option<T> {
            map.get(name).and_then(|value| T::deserialize(value).ok())
        }
        let defaults = Self::default();
        Self {
            screen_reader: key(map, "screen_reader").unwrap_or(defaults.screen_reader),
            screen_reader_rate: key(map, "screen_reader_rate")
                .unwrap_or(defaults.screen_reader_rate),
            screen_reader_voice: key(map, "screen_reader_voice").unwrap_or_default(),
            language: key(map, "language"),
            xkb_layout: key(map, "xkb_layout"),
            xkb_variant: key(map, "xkb_variant"),
            text_scale: key(map, "text_scale").unwrap_or(defaults.text_scale),
            high_contrast: key(map, "high_contrast").unwrap_or(defaults.high_contrast),
        }
        .clamped()
    }

    /// The comforts in a JSON value, as [`Self::from_map`] reads them; anything
    /// but an object is the defaults.
    pub fn from_value(value: &Value) -> Self {
        value.as_object().map(Self::from_map).unwrap_or_default()
    }

    /// Every value within what the login screens offer: the speech rate in
    /// its range, the text scale in the theme's band, a language that is a
    /// plain tag and a layout that is a plain XKB name. What fails is
    /// dropped to its default, not repaired.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.screen_reader_rate = clamped_rate(self.screen_reader_rate);
        self.text_scale = if self.text_scale.is_finite() {
            self.text_scale.clamp(TEXT_SCALE_MIN, TEXT_SCALE_MAX)
        } else {
            1.0
        };
        self.language = self.language.filter(|tag| plain_language_tag(tag));
        let source = self.xkb_layout.as_deref().and_then(|layout| {
            KeyboardInputSource::parse(&match self.xkb_variant.as_deref() {
                Some(variant) => format!("{layout}:{variant}"),
                None => layout.to_owned(),
            })
        });
        // A layout with a variant that does not parse loses both: half of a
        // keyboard source is a different keyboard.
        (self.xkb_layout, self.xkb_variant) = match source {
            Some(source) => (Some(source.layout), source.variant),
            None => (None, None),
        };
        self
    }

    /// The comforts with the keyboard layout of the active source `id`
    /// (`layout` or `layout:variant`, as the `input` section stores it). An
    /// input-method engine or anything else that is not an XKB layout leaves
    /// the layout unset.
    #[must_use]
    pub fn with_keyboard_source(mut self, id: &str) -> Self {
        let source = KeyboardInputSource::parse(id);
        self.xkb_layout = source.as_ref().map(|source| source.layout.clone());
        self.xkb_variant = source.and_then(|source| source.variant);
        self
    }

    /// What changed from `before` to `after`, as the fields to change: an
    /// unset language or layout is `null`, and the layout and its variant go
    /// together. Applied with [`Self::with_changes`], a person who makes the
    /// text larger leaves the screen reader someone turned on at the login
    /// screen on.
    pub fn changes(before: &Self, after: &Self) -> Map<String, Value> {
        let fields = |comforts: &Self| match serde_json::to_value(comforts) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        let (before, after) = (fields(before), fields(after));
        let field = |map: &Map<String, Value>, name: &str| map.get(name).cloned();
        let mut changes = Map::new();
        for name in [
            "screen_reader",
            "screen_reader_rate",
            "screen_reader_voice",
            "language",
            "text_scale",
            "high_contrast",
        ] {
            if field(&before, name) != field(&after, name) {
                changes.insert(name.into(), field(&after, name).unwrap_or(Value::Null));
            }
        }
        let layout = ["xkb_layout", "xkb_variant"];
        if layout
            .iter()
            .any(|name| field(&before, name) != field(&after, name))
        {
            for name in layout {
                changes.insert(name.into(), field(&after, name).unwrap_or(Value::Null));
            }
        }
        changes
    }

    /// These comforts with the fields `changes` names replaced, clamped as
    /// [`Self::from_map`] reads them.
    #[must_use]
    pub fn with_changes(&self, changes: &Map<String, Value>) -> Self {
        let Ok(Value::Object(mut fields)) = serde_json::to_value(self) else {
            return self.clone();
        };
        fields.extend(changes.iter().map(|(k, v)| (k.clone(), v.clone())));
        Self::from_map(&fields)
    }

    /// What the screen reader does with these comforts.
    pub fn screen_reader_settings(&self) -> ScreenReaderSettings {
        ScreenReaderSettings::new(
            self.screen_reader,
            self.screen_reader_rate,
            self.screen_reader_voice,
        )
    }
}

/// Letters, digits and `-`, starting with a letter: `de`, `de-DE`, `zh-Hant-TW`.
fn plain_language_tag(tag: &str) -> bool {
    tag.len() <= MAX_LANGUAGE_LEN
        && tag.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !tag.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_value_is_clamped_and_a_malformed_one_keeps_its_default() {
        let comforts = SignInComforts::from_value(&json!({
            "screen_reader": true,
            "screen_reader_rate": 60000,
            "screen_reader_voice": "klingon",
            "language": "../../etc/passwd",
            "xkb_layout": "de",
            "xkb_variant": "nodeadkeys,us",
            "text_scale": 1e30,
            "high_contrast": "yes",
        }));
        assert_eq!(
            comforts,
            SignInComforts {
                screen_reader: true,
                screen_reader_rate: 200,
                screen_reader_voice: ScreenReaderVoice::System,
                language: None,
                xkb_layout: None,
                xkb_variant: None,
                text_scale: TEXT_SCALE_MAX,
                high_contrast: false,
            }
        );
        assert_eq!(
            SignInComforts::from_value(&json!({"text_scale": -3.0, "screen_reader_rate": 1})),
            SignInComforts {
                text_scale: TEXT_SCALE_MIN,
                screen_reader_rate: 50,
                ..SignInComforts::default()
            }
        );
        assert_eq!(
            SignInComforts::from_value(&json!("not an object")),
            SignInComforts::default()
        );
    }

    #[test]
    fn only_what_changed_is_carried_over() {
        let at_login_screen = SignInComforts {
            screen_reader: true,
            ..SignInComforts::default()
        }
        .with_keyboard_source("fr");
        let before = SignInComforts::default().with_keyboard_source("de:nodeadkeys");
        let larger = SignInComforts {
            text_scale: 1.5,
            ..before.clone()
        };
        let changes = SignInComforts::changes(&before, &larger);
        assert_eq!(
            changes,
            json!({"text_scale": 1.5}).as_object().cloned().unwrap()
        );
        assert_eq!(
            at_login_screen.with_changes(&changes),
            SignInComforts {
                text_scale: 1.5,
                ..at_login_screen.clone()
            }
        );
        let us = before.clone().with_keyboard_source("us");
        let changes = SignInComforts::changes(&before, &us);
        assert_eq!(
            changes,
            json!({"xkb_layout": "us", "xkb_variant": null})
                .as_object()
                .cloned()
                .unwrap()
        );
        assert_eq!(
            at_login_screen.with_changes(&changes),
            SignInComforts {
                screen_reader: true,
                ..us.clone()
            }
        );
        assert!(SignInComforts::changes(&us, &us).is_empty());
    }

    #[test]
    fn plain_values_round_trip_in_the_token_shape() {
        let comforts = SignInComforts {
            screen_reader: true,
            screen_reader_rate: 140,
            screen_reader_voice: ScreenReaderVoice::German,
            language: Some("de-DE".into()),
            ..SignInComforts::default()
        }
        .with_keyboard_source("de:nodeadkeys");
        let value = serde_json::to_value(&comforts).unwrap();
        assert_eq!(
            value,
            json!({
                "screen_reader": true,
                "screen_reader_rate": 140,
                "screen_reader_voice": "german",
                "language": "de-DE",
                "xkb_layout": "de",
                "xkb_variant": "nodeadkeys",
                "text_scale": 1.0,
                "high_contrast": false,
            })
        );
        assert_eq!(SignInComforts::from_value(&value), comforts);
        assert_eq!(
            comforts.screen_reader_settings().voice.engine_voice(),
            Some("de")
        );
        assert_eq!(
            SignInComforts::default()
                .with_keyboard_source("ime/rime")
                .xkb_layout,
            None
        );
    }
}
