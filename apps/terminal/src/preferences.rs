//! The terminal's preferences: the bell and what a selection does.
//!
//! Both are the person's, not the document's and not a tab's: turning on
//! copy-on-select once should hold for every window opened afterwards, and a
//! bell that nobody hears should not have to be silenced every session. They
//! are kept through the toolkit's settings store, in the app's personal
//! section — the same path every other app's preferences take — so they
//! follow the person to their other devices and never reach a collaborator.
//!
//! A shared, system-owned section (the way `locale` and the clock are
//! shared) would be the right home for "the terminal rings a bell" as a
//! system preference. No such section exists yet; when one is declared by
//! its owner, these keys move there and nothing else changes.

use serde::{Deserialize, Serialize};

/// What the terminal does when a program rings the bell (BEL, `\a`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bell {
    /// Nothing: a bell nobody asked for is noise.
    #[default]
    Off,
    /// The pane washes with the attention colour and fades, and the window
    /// asks for attention. There is no audible bell in the toolkit yet, so
    /// this is what a bell is.
    Visual,
}

impl Bell {
    /// The word the context menu shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Bell off",
            Self::Visual => "Flash on a bell",
        }
    }

    /// The other setting: what choosing the menu row does.
    pub fn toggled(self) -> Self {
        match self {
            Self::Off => Self::Visual,
            Self::Visual => Self::Off,
        }
    }

    /// The choices a settings surface offers, and the one `stored` names.
    pub const ALL: [Self; 2] = [Self::Off, Self::Visual];

    pub fn of_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or_default()
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|bell| *bell == self).unwrap_or(0)
    }
}

/// The terminal's preferences as they are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PreferencesSetting {
    /// Finishing a selection also writes the clipboard, not only the primary
    /// selection.
    pub copy_on_select: bool,
    /// Whether a program may set the clipboard through OSC 52.
    pub allow_osc52_clipboard: bool,
    /// What a bell does.
    pub bell: Bell,
}

impl Default for PreferencesSetting {
    fn default() -> Self {
        Self {
            // The primary selection is always set, so middle-click paste
            // works without this; copying to the clipboard unasked would
            // replace what the person pastes next.
            copy_on_select: false,
            // Anything writing to the PTY — a `cat` of a hostile file, a
            // remote host — could otherwise replace the clipboard.
            allow_osc52_clipboard: false,
            bell: Bell::Off,
        }
    }
}

impl ui::storage::SettingsSchema for PreferencesSetting {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Personal;
}

/// The app id the preferences are kept under.
const TERMINAL_APP_ID: &str = "so.tilo.terminal";

/// The terminal's preferences, read from and written to the settings store
/// the host keeps.
#[derive(Clone, Debug)]
pub struct Preferences {
    settings: Option<ui::storage::Settings<PreferencesSetting>>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self::none()
    }
}

impl Preferences {
    /// The app's preferences in the document its host keeps.
    pub fn discover() -> Self {
        Self {
            settings: Some(ui::storage::Settings::for_app(TERMINAL_APP_ID)),
        }
    }

    /// The preferences in the settings document at `path`; tests use a
    /// scratch profile's.
    pub fn at(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            settings: Some(ui::storage::Settings::for_app(TERMINAL_APP_ID).in_file(path)),
        }
    }

    /// No store: nothing is remembered (every default).
    pub fn none() -> Self {
        Self { settings: None }
    }

    /// What the preferences are now.
    pub fn load(&self) -> PreferencesSetting {
        self.settings
            .as_ref()
            .map(ui::storage::Settings::get)
            .unwrap_or_default()
    }

    /// Keep `setting` for every terminal opened from now on. A store that
    /// cannot be written leaves the preference in the session rather than
    /// failing the gesture that changed it.
    pub fn store(&self, setting: PreferencesSetting) {
        if let Some(settings) = &self.settings {
            let _ = settings.set(&setting);
        }
    }

    /// Flip copy-on-select and keep the new value.
    pub fn set_copy_on_select(&self, on: bool) -> PreferencesSetting {
        let mut setting = self.load();
        setting.copy_on_select = on;
        self.store(setting);
        setting
    }

    /// Flip whether programs may set the clipboard and keep the new value.
    pub fn set_allow_osc52_clipboard(&self, on: bool) -> PreferencesSetting {
        let mut setting = self.load();
        setting.allow_osc52_clipboard = on;
        self.store(setting);
        setting
    }

    /// Choose what a bell does and keep it.
    pub fn set_bell(&self, bell: Bell) -> PreferencesSetting {
        let mut setting = self.load();
        setting.bell = bell;
        self.store(setting);
        setting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stored preferences are read back by another terminal, in the same
    /// settings document — which is the whole point of persisting them.
    #[test]
    fn preferences_survive_a_new_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let first = Preferences::at(&path);
        assert_eq!(first.load(), PreferencesSetting::default());
        first.set_copy_on_select(true);
        first.set_bell(Bell::Visual);

        let second = Preferences::at(&path);
        let stored = second.load();
        assert!(stored.copy_on_select, "copy-on-select is remembered");
        assert_eq!(stored.bell, Bell::Visual, "the bell is remembered");
        assert!(
            !stored.allow_osc52_clipboard,
            "an untouched preference is its default"
        );
    }

    /// With no store the preferences are the defaults and stay in memory: a
    /// test or an embedded terminal never writes the person's settings.
    #[test]
    fn no_store_means_no_writes() {
        let prefs = Preferences::none();
        assert_eq!(prefs.load(), PreferencesSetting::default());
        prefs.set_copy_on_select(true);
        assert!(
            !prefs.load().copy_on_select,
            "nothing was kept, because nothing can be"
        );
    }

    /// The bell's two settings are each other's toggle, and its names are
    /// what the menu shows.
    #[test]
    fn the_bell_toggles_between_off_and_a_flash() {
        assert_eq!(Bell::Off.toggled(), Bell::Visual);
        assert_eq!(Bell::Visual.toggled(), Bell::Off);
        assert_eq!(Bell::of_index(1), Bell::Visual);
        assert_eq!(Bell::Visual.index(), 1);
        assert_eq!(Bell::Visual.label(), "Flash on a bell");
    }
}
