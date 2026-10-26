//! The keyboard's preferences: what may change in them ([`SetPreference`]),
//! the effects through which a palette, an agent or a script changes them,
//! and the preference rows the keyboard surface publishes.
//!
//! Preferences are the person's settings ([`OskPreferences`]), not the
//! content of a record: they follow the person to their other devices,
//! carry no history and are never undone.
use serde::{Deserialize, Serialize};
use ui::framework::LiveRow;
use ui::tilo_actions::ActionSpec;
use ui::tilo_resource::{self, Effect};

use crate::dict::Language;
use crate::resource::{KeyboardSize, OskPreferences};

/// The letter layouts every keyboard carries; a preference names one of them.
pub const LETTER_LAYOUTS: [(&str, &str); 2] = [
    ("qwerty", "English (QWERTY)"),
    ("azerty", "French (AZERTY)"),
];

/// The letter layout names a preference may take.
pub use tilo_keyboard_map::touch::LAYOUT_NAMES;

/// The keyboard sizes a preference may take.
pub const KEYBOARD_SIZES: [&str; 3] = KeyboardSize::NAMES;

/// The typing languages a preference may take; `layout` follows the keys.
pub const TYPING_LANGUAGES: [&str; 3] = ["layout", "en", "fr"];

/// One keyboard preference set to a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetPreference {
    /// The letter layout text fields open in.
    Layout(String),
    /// How tall the key rows are.
    Size(String),
    /// Whether the suggestion bar offers words.
    Suggestions(bool),
    /// Whether a finished word may be rewritten to a likely correction.
    Autocorrect(bool),
    /// The language the dictionary and autocorrection use, by code; `layout`
    /// follows the letter layout.
    TypingLanguage(String),
    /// Whether the keyboard adapts to what this person types. Turning it off
    /// also forgets the recent emoji.
    Personalization(bool),
    /// This person's recently picked emoji, most recent first.
    EmojiRecents(Vec<String>),
}

impl SetPreference {
    /// Apply this change to `preferences`.
    ///
    /// # Errors
    /// A layout the keyboard does not carry.
    pub fn apply(&self, preferences: &mut OskPreferences) -> Result<(), String> {
        match self {
            Self::Layout(layout) => {
                if !LAYOUT_NAMES.contains(&layout.as_str()) {
                    return Err(format!("no letter layout named {layout:?}"));
                }
                if preferences.layouts.is_empty() {
                    preferences.layouts =
                        LAYOUT_NAMES.iter().map(|name| (*name).to_owned()).collect();
                }
                if !preferences.layouts.contains(&preferences.layout)
                    && LAYOUT_NAMES.contains(&preferences.layout.as_str())
                {
                    preferences.layouts.push(preferences.layout.clone());
                }
                layout.clone_into(&mut preferences.layout);
                if !preferences.layouts.contains(layout) {
                    preferences.layouts.push(layout.clone());
                }
            }
            Self::Size(name) => {
                preferences.size = KeyboardSize::from_name(name)
                    .ok_or_else(|| format!("no keyboard size named {name:?}"))?;
            }
            Self::Suggestions(enabled) => preferences.suggestions = *enabled,
            Self::Autocorrect(enabled) => preferences.autocorrect = *enabled,
            Self::TypingLanguage(name) => {
                preferences.typing_language = if name == "layout" || name.is_empty() {
                    None
                } else {
                    Language::from_code(name)
                        .ok_or_else(|| format!("no typing language named {name:?}"))?;
                    Some(name.clone())
                };
            }
            Self::Personalization(enabled) => {
                preferences.personalization = *enabled;
                if !enabled {
                    preferences.emoji_recents.clear();
                }
            }
            Self::EmojiRecents(recents) => recents.clone_into(&mut preferences.emoji_recents),
        }
        Ok(())
    }
}

/// What the keyboard does on request: change one of the person's
/// preferences, or forget what it learned.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "osk.keyboard")]
pub enum OskEffect {
    /// Set the letter layout text fields open in.
    #[effect(kind = "read", label = "Set keyboard layout")]
    SetLayout {
        #[param("Letter layout", choices = LAYOUT_NAMES)]
        value: String,
    },
    /// Set the keyboard's key size.
    #[effect(kind = "read", label = "Set keyboard size")]
    SetSize {
        #[param("Key size", choices = KEYBOARD_SIZES)]
        value: String,
    },
    /// Enable or disable the suggestion bar.
    #[effect(kind = "read", label = "Set suggestions")]
    SetSuggestions {
        #[param("Enabled")]
        value: bool,
    },
    /// Enable or disable autocorrection, independently of suggestions.
    #[effect(kind = "read", label = "Set autocorrect")]
    SetAutocorrect {
        #[param("Enabled")]
        value: bool,
    },
    /// Set the typing language independently of the letter layout.
    #[effect(kind = "read", label = "Set typing language")]
    SetTypingLanguage {
        #[param("Typing language", choices = TYPING_LANGUAGES)]
        value: String,
    },
    /// Enable or disable adapting to what this person types.
    #[effect(kind = "read", label = "Set personalization")]
    SetPersonalization {
        #[param("Enabled")]
        value: bool,
    },
    /// Replace this person's recent emoji list.
    #[effect(kind = "read", label = "Set recent emoji")]
    SetEmojiRecents {
        #[param("Recent emoji, most recent first")]
        value: Vec<String>,
    },
    /// Forget what the keyboard learned from typing.
    ///
    /// Learned word rankings, tap positions, kept words and recent emoji;
    /// the personalization preference stays as it is.
    #[effect(row, kind = "read", destructive, label = "Clear learned suggestions")]
    ClearPersonal,
}

impl OskEffect {
    /// The preference change this effect asks for; `None` for one that
    /// changes no preference.
    #[must_use]
    pub fn preference(&self) -> Option<SetPreference> {
        Some(match self {
            Self::SetLayout { value } => SetPreference::Layout(value.clone()),
            Self::SetSize { value } => SetPreference::Size(value.clone()),
            Self::SetSuggestions { value } => SetPreference::Suggestions(*value),
            Self::SetAutocorrect { value } => SetPreference::Autocorrect(*value),
            Self::SetTypingLanguage { value } => SetPreference::TypingLanguage(value.clone()),
            Self::SetPersonalization { value } => SetPreference::Personalization(*value),
            Self::SetEmojiRecents { value } => SetPreference::EmojiRecents(value.clone()),
            Self::ClearPersonal => return None,
        })
    }
}

/// `effect` offered as `id`, labelled `label`.
fn offer(id: &str, label: &str, effect: &OskEffect) -> ActionSpec {
    let mut spec = effect.bound();
    id.clone_into(&mut spec.id);
    label.clone_into(&mut spec.label);
    spec
}

/// `effect` offered as `id`, still asking for its `value`.
fn asking(id: &str, effect: &OskEffect) -> ActionSpec {
    let mut spec = effect.bound_asking(&["value"]);
    id.clone_into(&mut spec.id);
    spec
}

/// The preference rows the keyboard publishes beside its resource: the
/// layout, the suggestions and the recent emoji as they stand, each with the
/// changes it offers.
pub(crate) fn preference_rows(preferences: &OskPreferences) -> Vec<LiveRow> {
    let mut layout = LiveRow::new(
        "osk.keyboard.layout",
        "osk.layout",
        format!("Keyboard layout ({})", preferences.layout.to_uppercase()),
    )
    .operation(asking(
        "set",
        &OskEffect::SetLayout {
            value: preferences.layout.clone(),
        },
    ));
    for (id, label) in LETTER_LAYOUTS {
        layout = layout.operation(offer(
            id,
            label,
            &OskEffect::SetLayout {
                value: id.to_owned(),
            },
        ));
    }
    let mut size = LiveRow::new(
        "osk.keyboard.size",
        "osk.size",
        format!("Keyboard size ({})", preferences.size.name()),
    )
    .operation(asking(
        "set",
        &OskEffect::SetSize {
            value: preferences.size.name().to_owned(),
        },
    ));
    for size_name in KeyboardSize::ALL {
        size = size.operation(offer(
            size_name.name(),
            size_name.name(),
            &OskEffect::SetSize {
                value: size_name.name().to_owned(),
            },
        ));
    }
    let language_name = preferences
        .typing_language
        .as_deref()
        .and_then(Language::from_code)
        .map_or("Match layout", Language::code);
    let language = LiveRow::new(
        "osk.keyboard.language",
        "osk.language",
        format!("Typing language ({language_name})"),
    )
    .operation(asking(
        "set",
        &OskEffect::SetTypingLanguage {
            value: preferences
                .typing_language
                .clone()
                .unwrap_or_else(|| "layout".to_owned()),
        },
    ))
    .operation(offer(
        "match-layout",
        "Match the key layout",
        &OskEffect::SetTypingLanguage {
            value: "layout".to_owned(),
        },
    ))
    .operation(offer(
        "english",
        "English",
        &OskEffect::SetTypingLanguage {
            value: "en".to_owned(),
        },
    ))
    .operation(offer(
        "french",
        "French",
        &OskEffect::SetTypingLanguage {
            value: "fr".to_owned(),
        },
    ));
    let autocorrect = LiveRow::new("osk.keyboard.autocorrect", "osk.autocorrect", "Autocorrect")
        .operation(asking(
            "set",
            &OskEffect::SetAutocorrect {
                value: preferences.autocorrect,
            },
        ))
        .operation(offer(
            "enable",
            "Correct words",
            &OskEffect::SetAutocorrect { value: true },
        ))
        .operation(offer(
            "disable",
            "Never correct words",
            &OskEffect::SetAutocorrect { value: false },
        ));
    let suggestions = LiveRow::new(
        "osk.keyboard.suggestions",
        "osk.suggestions",
        "Suggestions and privacy",
    )
    .operation(asking(
        "set",
        &OskEffect::SetSuggestions {
            value: preferences.suggestions,
        },
    ))
    .operation(asking(
        "set-personal",
        &OskEffect::SetPersonalization {
            value: preferences.personalization,
        },
    ))
    .operation(offer(
        "enable",
        "Turn on suggestions",
        &OskEffect::SetSuggestions { value: true },
    ))
    .operation(offer(
        "disable",
        "Turn off suggestions",
        &OskEffect::SetSuggestions { value: false },
    ))
    .operation(offer(
        "enable-personal",
        "Turn on personalized suggestions",
        &OskEffect::SetPersonalization { value: true },
    ))
    .operation(offer(
        "disable-personal",
        "Turn off personalized suggestions",
        &OskEffect::SetPersonalization { value: false },
    ))
    .operation(OskEffect::ClearPersonal.bound());
    let emoji = LiveRow::new("osk.keyboard.emoji", "osk.emoji", "Recent emoji").operation(asking(
        "set-recents",
        &OskEffect::SetEmojiRecents {
            value: preferences.emoji_recents.clone(),
        },
    ));
    vec![layout, size, language, autocorrect, suggestions, emoji]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_action_preserves_legacy_choices_and_explicit_single_source() {
        let mut preferences = OskPreferences {
            layout: "azerty".into(),
            suggestions: false,
            ..Default::default()
        };
        assert_eq!(
            SetPreference::Layout("azerty".into()).apply(&mut preferences),
            Ok(())
        );
        assert_eq!(preferences.layouts, ["qwerty", "azerty"]);
        assert_eq!(preferences.layout, "azerty");
        assert!(!preferences.suggestions);
        preferences.layouts = vec!["azerty".into()];
        assert_eq!(
            SetPreference::Layout("azerty".into()).apply(&mut preferences),
            Ok(())
        );
        assert_eq!(preferences.layouts, ["azerty"]);
        assert_eq!(
            SetPreference::Layout("qwerty".into()).apply(&mut preferences),
            Ok(())
        );
        assert_eq!(preferences.layouts, ["azerty", "qwerty"]);
    }
}
