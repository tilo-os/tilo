//! The colour this person wears everywhere, as a setting: their focus ring,
//! text caret and text selection, the mark their collaborators see on their
//! cursor, and their part of every presence list.
//!
//! It is one of the twelve presence hues (`ui::tokens::PresenceHue`), stored
//! as `identity.colour` and read by `PresenceHue::chosen_in`; until they
//! choose, it is derived from their identity, so every host paints them the
//! same without asking. A colour is identity, not theme: it roams with the
//! person's other settings.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// The colour the person wears, or following the one their identity derives.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[model(check = IdentityPrefs::valid, check_reads = "colour", defaults)]
pub struct IdentityPrefs {
    /// A presence hue's id (`teal`, `violet`, …); `None` derives it from the
    /// person's identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[model(choices = colours::ids())]
    pub colour: Option<String>,
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
impl ui::storage::SettingsSchema for IdentityPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared(settings_store::identity::SECTION);
    ui::settings_codec!();
}

#[tilo_resource::resource(kind = "settings.identity", label = "Your colour")]
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
        assert_eq!(colours::label("violet"), "Violet");
        prefs.colour = Some("chartreuse".into());
        assert!(prefs.valid().is_err());
        assert_eq!(colours::label("chartreuse"), "chartreuse");
        assert_eq!(colours::ids().len(), ui::tokens::PresenceHue::ALL.len());
    }
}
