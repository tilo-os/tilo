//! The keyboard's preferences: what may change them ([`SetPreference`]), the
//! effects a palette, an agent or a script takes, and the rows the keyboard
//! publishes. They are the person's settings ([`OskPreferences`]), not a resource's
//! content: they follow the person to their devices and are never undone.
use serde::{Deserialize, Serialize};
use ui::framework::LiveRow;
use ui::tilo_resource::{self, Effect};

use crate::dict::Language;
use crate::resource::{KeyboardSize, OskPreferences};

pub const LETTER_LAYOUTS: [(&str, &str); 2] = [
    ("qwerty", "English (QWERTY)"),
    ("azerty", "French (AZERTY)"),
];

pub use tilo_keyboard_map::touch::LAYOUT_NAMES;

pub const KEYBOARD_SIZES: [&str; 3] = KeyboardSize::NAMES;

pub const TYPING_LANGUAGES: [&str; 3] = ["layout", "en", "fr"];

/// The letter layout text fields open in, as a row of this person's
/// preferences.
#[derive(tilo_resource::Row)]
#[row(kind = "osk.layout", label = "Keyboard layout", effects = OskEffect)]
pub struct OskLayout;

/// How tall the key rows are.
#[derive(tilo_resource::Row)]
#[row(kind = "osk.size", label = "Keyboard size", effects = OskEffect)]
pub struct OskSize;

/// The language the dictionary and autocorrection use.
#[derive(tilo_resource::Row)]
#[row(kind = "osk.language", label = "Typing language", effects = OskEffect)]
pub struct OskLanguage;

/// Whether a finished word may be corrected.
#[derive(tilo_resource::Row)]
#[row(kind = "osk.autocorrect", label = "Autocorrect", effects = OskEffect)]
pub struct OskAutocorrect;

/// The suggestion bar and what the keyboard learns.
#[derive(tilo_resource::Row)]
#[row(kind = "osk.suggestions", label = "Suggestions and privacy", effects = OskEffect)]
pub struct OskSuggestions;

/// This person's recently picked emoji.
#[derive(tilo_resource::Row)]
#[row(kind = "osk.emoji", label = "Recent emoji")]
pub struct OskEmoji;

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

/// Change a preference, or forget what the keyboard learned.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "osk.keyboard")]
pub enum OskEffect {
    /// Set the letter layout text fields open in.
    #[effect(kind = "read")]
    SetLayout {
        #[param("Letter layout", choices = LAYOUT_NAMES)]
        value: String,
    },
    /// Set the keyboard's key size.
    #[effect(kind = "read")]
    SetSize {
        #[param("Key size", choices = KEYBOARD_SIZES)]
        value: String,
    },
    /// Enable or disable the suggestion bar.
    #[effect(kind = "read")]
    SetSuggestions {
        #[param("Enabled")]
        value: bool,
    },
    /// Enable or disable autocorrection, independently of suggestions.
    #[effect(kind = "read")]
    SetAutocorrect {
        #[param("Enabled")]
        value: bool,
    },
    /// Set the typing language independently of the letter layout.
    #[effect(kind = "read")]
    SetTypingLanguage {
        #[param("Typing language", choices = TYPING_LANGUAGES)]
        value: String,
    },
    /// Enable or disable adapting to what this person types.
    #[effect(kind = "read")]
    SetPersonalization {
        #[param("Enabled")]
        value: bool,
    },
    /// Replace this person's recent emoji list.
    #[effect(kind = "read")]
    SetEmojiRecents {
        #[param("Recent emoji, most recent first")]
        value: Vec<String>,
    },
    /// Forget what the keyboard learned from typing.
    ///
    /// Learned word rankings, tap positions, kept words and recent emoji;
    /// the personalization preference stays as it is.
    #[effect(row, on = OskSuggestions, kind = "read", destructive)]
    ClearPersonal,
    /// Use the QWERTY letter layout.
    #[effect(row, on = OskLayout, kind = "read")]
    Qwerty,
    /// Use the AZERTY letter layout.
    #[effect(row, on = OskLayout, kind = "read")]
    Azerty,
    /// Use the compact keyboard.
    #[effect(row, on = OskSize, kind = "read")]
    Compact,
    /// Use the standard keyboard.
    #[effect(row, on = OskSize, kind = "read")]
    Standard,
    /// Use the large keyboard.
    #[effect(row, on = OskSize, kind = "read")]
    Large,
    /// Match the key layout.
    #[effect(row, on = OskLanguage, kind = "read")]
    MatchLayout,
    /// Type in English.
    #[effect(row, on = OskLanguage, kind = "read")]
    English,
    /// Type in French.
    #[effect(row, on = OskLanguage, kind = "read")]
    French,
    /// Correct words.
    #[effect(row, on = OskAutocorrect, kind = "read")]
    EnableAutocorrect,
    /// Never correct words.
    #[effect(row, on = OskAutocorrect, kind = "read")]
    DisableAutocorrect,
    /// Turn on suggestions.
    #[effect(row, on = OskSuggestions, kind = "read")]
    EnableSuggestions,
    /// Turn off suggestions.
    #[effect(row, on = OskSuggestions, kind = "read")]
    DisableSuggestions,
    /// Turn on personalized suggestions.
    #[effect(row, on = OskSuggestions, kind = "read")]
    EnablePersonalization,
    /// Turn off personalized suggestions.
    #[effect(row, on = OskSuggestions, kind = "read")]
    DisablePersonalization,
}

impl OskEffect {
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
            Self::Qwerty => SetPreference::Layout("qwerty".into()),
            Self::Azerty => SetPreference::Layout("azerty".into()),
            Self::Compact => SetPreference::Size("compact".into()),
            Self::Standard => SetPreference::Size("standard".into()),
            Self::Large => SetPreference::Size("large".into()),
            Self::MatchLayout => SetPreference::TypingLanguage("layout".into()),
            Self::English => SetPreference::TypingLanguage("en".into()),
            Self::French => SetPreference::TypingLanguage("fr".into()),
            Self::EnableAutocorrect => SetPreference::Autocorrect(true),
            Self::DisableAutocorrect => SetPreference::Autocorrect(false),
            Self::EnableSuggestions => SetPreference::Suggestions(true),
            Self::DisableSuggestions => SetPreference::Suggestions(false),
            Self::EnablePersonalization => SetPreference::Personalization(true),
            Self::DisablePersonalization => SetPreference::Personalization(false),
        })
    }
}

/// The layout, the suggestions and the recent emoji as they stand, each with
/// the changes it offers.
pub(crate) fn preference_rows(preferences: &OskPreferences) -> Vec<LiveRow> {
    let language_name = preferences
        .typing_language
        .as_deref()
        .and_then(Language::from_code)
        .map_or("Match layout", Language::code);
    vec![
        LiveRow::typed::<OskLayout>(
            "osk.keyboard.layout",
            format!("Keyboard layout ({})", preferences.layout.to_uppercase()),
        ),
        LiveRow::typed::<OskSize>(
            "osk.keyboard.size",
            format!("Keyboard size ({})", preferences.size.name()),
        ),
        LiveRow::typed::<OskLanguage>(
            "osk.keyboard.language",
            format!("Typing language ({language_name})"),
        ),
        LiveRow::typed::<OskAutocorrect>("osk.keyboard.autocorrect", "Autocorrect"),
        LiveRow::typed::<OskSuggestions>("osk.keyboard.suggestions", "Suggestions and privacy"),
        LiveRow::typed::<OskEmoji>("osk.keyboard.emoji", "Recent emoji"),
    ]
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
