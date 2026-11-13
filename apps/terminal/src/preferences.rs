//! The terminal's preferences: the bell and what a selection does.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bell {
    #[default]
    Off,
    Visual,
}

impl Bell {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Bell off",
            Self::Visual => "Flash on a bell",
        }
    }

    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            Self::Off => Self::Visual,
            Self::Visual => Self::Off,
        }
    }

    pub const ALL: [Self; 2] = [Self::Off, Self::Visual];

    pub fn of_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or_default()
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|bell| *bell == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PreferencesSetting {
    /// Finishing a selection also writes the clipboard, not only the primary
    /// selection.
    pub copy_on_select: bool,
    pub allow_osc52_clipboard: bool,
    pub bell: Bell,
}

impl Default for PreferencesSetting {
    fn default() -> Self {
        Self {
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

const TERMINAL_APP_ID: &str = "so.tilo.terminal";

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
    pub fn discover() -> Self {
        Self {
            settings: Some(ui::storage::Settings::for_app(TERMINAL_APP_ID)),
        }
    }

    pub fn at(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            settings: Some(ui::storage::Settings::for_app(TERMINAL_APP_ID).in_profile(path)),
        }
    }

    pub fn none() -> Self {
        Self { settings: None }
    }

    pub fn load(&self) -> PreferencesSetting {
        self.settings
            .as_ref()
            .map(ui::storage::Settings::get)
            .unwrap_or_default()
    }

    pub fn store(&self, setting: PreferencesSetting) {
        if let Some(settings) = &self.settings {
            let _ = settings.set(&setting);
        }
    }

    pub fn set_copy_on_select(&self, on: bool) -> PreferencesSetting {
        let mut setting = self.load();
        setting.copy_on_select = on;
        self.store(setting);
        setting
    }

    pub fn set_allow_osc52_clipboard(&self, on: bool) -> PreferencesSetting {
        let mut setting = self.load();
        setting.allow_osc52_clipboard = on;
        self.store(setting);
        setting
    }

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

    #[test]
    fn preferences_survive_a_new_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
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

    #[test]
    fn the_bell_toggles_between_off_and_a_flash() {
        assert_eq!(Bell::Off.toggled(), Bell::Visual);
        assert_eq!(Bell::Visual.toggled(), Bell::Off);
        assert_eq!(Bell::of_index(1), Bell::Visual);
        assert_eq!(Bell::Visual.index(), 1);
        assert_eq!(Bell::Visual.label(), "Flash on a bell");
    }
}
