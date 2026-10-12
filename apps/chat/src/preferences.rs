//! How this person likes Chat laid out, on every device of theirs and never
//! seen by collaborators: which channel categories they folded away.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

/// The person's own Chat preferences.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPreferences {
    /// The categories folded in the sidebar, by id.
    #[serde(default)]
    pub collapsed_categories: BTreeSet<String>,
}

impl SettingsSchema for ChatPreferences {
    const SCOPE: Scope = Scope::Personal;
}
