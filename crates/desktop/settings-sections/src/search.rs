//! What the person chose from the desktop's search on this device and where
//! their windows were ([`settings_store::search_history`]).

use serde::{Deserialize, Serialize};
use ui::tilo_resource;

/// Where the person's windows were, newest first, as the desktop's search
/// keeps them: the section holds them whole and reads nothing inside. Search
/// writes them as the list they are, and a type that declares no shape has its
/// setter taken for an object, so the list is declared here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Places(pub serde_json::Value);

tilo_resource::json_model!(Places => {"type": "array"}, Any);

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

impl settings_model::SettingsSchema for SearchHistory {
    const SCOPE: settings_model::Scope =
        settings_model::Scope::Shared(settings_store::search_history::SECTION);
    settings_model::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "settings.search_history")]
impl SearchHistory {}
