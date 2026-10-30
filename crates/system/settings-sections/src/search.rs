//! What the person chose from the desktop's search on this device and where
//! their windows were ([`settings_store::search_history`]).
//!
//! The desktop's search keeps both while it runs and writes them here
//! through the section's field edits whenever they change, so they outlast
//! the session and search starts again where the person left it.

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// Where the person's windows were, newest first, as the desktop's search
/// keeps them: the section holds them whole and reads nothing inside.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Places(pub serde_json::Value);

tilo_resource::json_model!(Places);

/// The section.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
pub struct SearchHistory {
    /// Which rows were chosen, how often and how recently.
    #[serde(default)]
    pub chosen: String,
    /// Which row was chosen after typing what.
    #[serde(default)]
    pub chosen_for_text: String,
    /// Where the person's windows were.
    #[serde(default)]
    pub places: Places,
}

impl ui::storage::SettingsSchema for SearchHistory {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::search_history::SECTION);
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.search_history", label = "Search history")]
impl SearchHistory {}
