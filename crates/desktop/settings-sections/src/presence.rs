//! What the person shares of what they are doing, and what the people in each workspace see of
//! them: the switches their own card offers, kept as the `presence` section so every device of
//! theirs reads the same choice.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// How what they are doing is shared, and what each workspace shows of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[model(check = PresencePrefs::valid, check_reads = "sharing visibility", defaults)]
pub struct PresencePrefs {
    /// `automatic` (their device writes what they are doing for them), `manual` (only what they
    /// write themselves) or `off` (nothing).
    #[serde(default = "shared_automatically")]
    #[model(choices = sharing::ids())]
    pub sharing: String,
    /// What the people in each workspace see of them, by the workspace's id: `where` (their face,
    /// their window and their pointer), `here` (their face alone) or `nothing`. A workspace the
    /// person never chose for is not listed, and reads as `where`.
    #[serde(default)]
    pub visibility: BTreeMap<String, String>,
}

/// The default for a person who never chose: their device may write what they are doing.
fn shared_automatically() -> String {
    sharing::AUTOMATIC.to_owned()
}

impl Default for PresencePrefs {
    fn default() -> Self {
        Self {
            sharing: shared_automatically(),
            visibility: BTreeMap::new(),
        }
    }
}

impl PresencePrefs {
    /// The sharing is one the card offers, and every workspace's sight is one of the three.
    fn valid(&self) -> Result<(), tilo_resource::Refusal> {
        let stored = &self.sharing;
        if sharing::from_id(stored).is_none() {
            return Err(tilo_resource::Refusal::new(format!(
                "`{stored}` is not one of {}",
                sharing::IDS.join(", ")
            )));
        }
        for (workspace, sight) in &self.visibility {
            if ui::PresenceVisibility::from_id(sight).is_none() {
                return Err(tilo_resource::Refusal::new(format!(
                    "`{sight}` is not what a workspace sees of a person ({workspace})"
                )));
            }
        }
        Ok(())
    }
}

/// This person's presence choices, read from the shared `presence` section.
impl settings_model::SettingsSchema for PresencePrefs {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::presence::SECTION);
    settings_model::settings_codec!();
}

#[tilo_resource::resource(kind = "settings.presence")]
impl PresencePrefs {}

/// The ways of sharing what you are doing, as the card offers them.
pub mod sharing {
    pub const AUTOMATIC: &str = "automatic";
    pub const MANUAL: &str = "manual";
    pub const OFF: &str = "off";
    pub const IDS: [&str; 3] = [AUTOMATIC, MANUAL, OFF];

    /// The modes the card offers, as the setting they are stored as.
    #[must_use]
    pub fn ids() -> Vec<&'static str> {
        IDS.to_vec()
    }

    /// The mode a stored `id` names, if this build knows it.
    #[must_use]
    pub fn from_id(id: &str) -> Option<ui::StatusSharing> {
        ui::StatusSharing::from_word(id)
    }

    /// What the mode is stored as.
    #[must_use]
    pub const fn id(mode: ui::StatusSharing) -> &'static str {
        mode.word()
    }
}
