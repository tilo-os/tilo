//! The colour this person wears everywhere, as a setting: their focus ring,
//! text caret and text selection, the mark their members see on their
//! cursor, and their part of every presence list. Beside it, whether their
//! identity is backed up, and whether the system reminds them while it is not.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// The colour the person wears, or following the one their identity derives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[model(check = IdentityPrefs::valid, check_reads = "colour", defaults)]
pub struct IdentityPrefs {
    /// A presence hue's id (`teal`, `violet`, …); `None` derives it from the
    /// person's identity. Written even when `None` (as `null`), so following
    /// their identity again replaces a colour chosen before.
    #[serde(default)]
    #[model(choices = colours::ids())]
    pub colour: Option<String>,
    /// Whether a notice reminds them, every few days, of a backup of their
    /// identity they left for later. On until they turn it off.
    #[serde(default = "reminding")]
    pub backup_reminders: bool,
    /// Whether their identity is backed up: `Some(false)` once they left the
    /// backup for later, `Some(true)` once one was verified. Setup and the
    /// backup itself write it.
    #[serde(default)]
    #[model(readonly)]
    pub backed_up: Option<bool>,
}

const fn reminding() -> bool {
    true
}

impl Default for IdentityPrefs {
    fn default() -> Self {
        Self {
            colour: None,
            backup_reminders: reminding(),
            backed_up: None,
        }
    }
}

impl IdentityPrefs {
    /// A colour is one of the hues the palette has, or none at all.
    fn valid(&self) -> Result<(), tilo_resource::Refusal> {
        match &self.colour {
            Some(id) if ui::tokens::PresenceHue::from_id(id).is_none() => Err(
                tilo_resource::Refusal::new(format!("`{id}` is not a colour")),
            ),
            _ => Ok(()),
        }
    }
}

/// This person's colour, read from the shared `identity` section.
impl settings_model::SettingsSchema for IdentityPrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::identity::SECTION);
    settings_model::settings_codec!();
}

#[tilo_resource::resource(kind = "settings.identity")]
impl IdentityPrefs {}

/// The offered colours.
pub mod colours {
    /// Every hue the palette has, by id as it is stored.
    #[must_use]
    pub fn ids() -> Vec<&'static str> {
        ui::tokens::PresenceHue::ALL
            .iter()
            .map(|hue| hue.id())
            .collect()
    }

    /// The name a person reads for the stored `id`, or the id itself for one
    /// this build does not know.
    #[must_use]
    pub fn label(id: &str) -> String {
        ui::tokens::PresenceHue::from_id(id)
            .map_or_else(|| id.to_owned(), |hue| hue.label().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_is_one_of_the_twelve_hues_or_following_the_identity() {
        let mut prefs = IdentityPrefs::default();
        assert_eq!(prefs.colour, None);
        prefs.colour = Some("violet".into());
        assert_eq!(prefs.valid(), Ok(()));
        prefs.colour = Some("chartreuse".into());
        assert!(prefs.valid().is_err());
        assert_eq!(colours::ids().len(), ui::tokens::PresenceHue::ALL.len());
    }
}
