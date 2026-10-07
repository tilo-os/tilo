//! The reader's own keys: the one-key bindings a person chooses for reader
//! commands. They follow the person to every device they sign in on, and a
//! collaborator's keys never change anyone else's reader.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::framework::Setting;
use ui::storage::{Scope, SettingsSchema};

/// At most this many bindings are kept.
const MAX_BINDINGS: usize = 64;

/// The bindings as stored: reader command id to its key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReaderKeySettings {
    #[serde(default)]
    shortcuts: BTreeMap<String, String>,
}

impl SettingsSchema for ReaderKeySettings {
    const SCOPE: Scope = Scope::Personal;
}

/// The reader's key bindings, kept in the person's settings when the reader
/// has them, in memory otherwise.
#[derive(Clone, Default)]
pub(crate) struct ReaderKeys {
    stored: Option<Setting<ReaderKeySettings>>,
    unsaved: ReaderKeySettings,
}

impl ReaderKeys {
    /// The bindings in `stored`, followed while the reader lives: a key the
    /// person chose on another device applies here on the next frame.
    pub(crate) fn following(stored: Setting<ReaderKeySettings>) -> Self {
        Self {
            stored: Some(stored),
            unsaved: ReaderKeySettings::default(),
        }
    }

    fn current(&self) -> ReaderKeySettings {
        self.stored
            .as_ref()
            .map_or_else(|| self.unsaved.clone(), Setting::get)
    }

    /// The key bound to reader command `action`, else `default`.
    pub(crate) fn shortcut(&self, action: &str, default: &str) -> String {
        self.current()
            .shortcuts
            .get(action)
            .filter(|key| valid_shortcut(key) && !reserved_reader_key(key))
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    /// Bind `key` to reader command `action`. A key another command has, a
    /// reserved key or one that is not a single printable character is
    /// refused, so pressing a key always has one predictable meaning.
    pub(crate) fn set_shortcut(&mut self, action: &str, key: &str) -> bool {
        let mut keys = self.current();
        if !valid_shortcut(key)
            || reserved_reader_key(key)
            || keys
                .shortcuts
                .iter()
                .any(|(other, bound)| other != action && bound.eq_ignore_ascii_case(key))
            || (keys.shortcuts.len() >= MAX_BINDINGS && !keys.shortcuts.contains_key(action))
        {
            return false;
        }
        keys.shortcuts
            .insert(action.to_string(), key.to_lowercase());
        match &self.stored {
            Some(stored) => {
                if let Err(error) = stored.set(keys) {
                    tracing::warn!(%error, "ebook: couldn't keep the reader's keys");
                    return false;
                }
            }
            None => self.unsaved = keys,
        }
        true
    }
}

/// These reader commands deliberately remain fixed. They are either global
/// mode controls or selection-sensitive actions, so allowing a configurable
/// binding to shadow them would make the keyboard contract ambiguous.
pub(crate) fn reserved_reader_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    matches!(
        lowered.as_str(),
        "t" | "n" | "f" | "c" | "+" | "=" | "-" | "_" | "−" | "[" | "]" | "1" | "2" | "3" | "4"
    )
}

fn valid_shortcut(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(key) = chars.next() else {
        return false;
    };
    chars.next().is_none() && !key.is_whitespace() && !key.is_control()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys are validated, unique and never shadow a reserved command.
    #[test]
    fn reader_keys_are_validated_and_unique() {
        let mut keys = ReaderKeys::default();
        assert_eq!(keys.shortcut("next-page", "."), ".");
        assert!(keys.set_shortcut("next-page", "J"));
        assert_eq!(keys.shortcut("next-page", "."), "j");
        assert!(
            !keys.set_shortcut("bookmark", "j"),
            "another command has it"
        );
        assert!(!keys.set_shortcut("bookmark", "t"), "reserved");
        assert!(!keys.set_shortcut("bookmark", "−"), "reserved");
        assert!(!keys.set_shortcut("bookmark", "\n"), "not printable");
        assert!(!keys.set_shortcut("bookmark", "ab"), "not one key");
        assert!(keys.set_shortcut("bookmark", "x"));
        assert_eq!(keys.shortcut("bookmark", "b"), "x");
    }

    /// The keys a person chooses are their own settings, which roam to
    /// their other devices.
    #[test]
    fn reader_keys_are_personal_settings() {
        assert_eq!(ReaderKeySettings::SCOPE, Scope::Personal);
    }
}
