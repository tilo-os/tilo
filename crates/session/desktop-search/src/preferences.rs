//! Which search sources the person switched off: their own choice, kept in
//! the settings' `search` section, so it holds whichever desktop searches.
//! Source ids are opaque stable strings, so a choice survives a source being
//! absent for a while.

use std::collections::BTreeSet;

/// The section of the person's settings the choices are kept in.
pub const SECTION: &str = "search";
const DISABLED_KEY: &str = "disabled_providers";
/// What every desktop offers whatever the person switched: the actions are
/// how anything is done from search.
const ALWAYS_ON: &str = "actions";
/// The longest source id kept.
const MAX_ID_CHARS: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchPreferences {
    disabled: BTreeSet<String>,
}

fn keepable(id: &str) -> bool {
    id != ALWAYS_ON && !id.is_empty() && id.chars().count() <= MAX_ID_CHARS
}

/// Whether the person may switch `source` off: the actions are how anything
/// is done from search, so they stay whatever is switched.
#[must_use]
pub fn switchable(source: &str) -> bool {
    keepable(source)
}

impl SearchPreferences {
    /// The choices the settings' `search` section holds.
    #[must_use]
    pub fn from_values(values: &serde_json::Map<String, serde_json::Value>) -> Self {
        let disabled = values
            .get(DISABLED_KEY)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .filter(|id| keepable(id))
            .map(str::to_owned)
            .collect();
        Self { disabled }
    }

    /// The choices as the settings' `search` section keeps them.
    #[must_use]
    pub fn to_values(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut values = serde_json::Map::new();
        values.insert(
            DISABLED_KEY.into(),
            serde_json::Value::Array(
                self.disabled
                    .iter()
                    .cloned()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
        values
    }

    #[must_use]
    pub fn enabled(&self, source: &str) -> bool {
        !self.disabled.contains(source)
    }

    pub fn set_enabled(&mut self, source: &str, enabled: bool) {
        if enabled {
            self.disabled.remove(source);
        } else if keepable(source) {
            self.disabled.insert(source.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switched_off_source_stays_off_and_actions_never_go() {
        let mut preferences = SearchPreferences::default();
        preferences.set_enabled("host-applications", false);
        preferences.set_enabled("actions", false);
        preferences.set_enabled("", false);
        assert!(!preferences.enabled("host-applications"));
        assert!(preferences.enabled("actions"));
        let kept = SearchPreferences::from_values(&preferences.to_values());
        assert_eq!(kept, preferences);
        preferences.set_enabled("host-applications", true);
        assert!(preferences.enabled("host-applications"));
    }
}
