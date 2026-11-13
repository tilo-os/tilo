//! Code's preferences: they follow the person to every device, and no
//! collaborator sees them.

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeSettings {
    pub language_servers: bool,
    pub format_on_save: bool,
    #[serde(default = "default_editor_zoom")]
    pub editor_zoom: f32,
}

fn default_editor_zoom() -> f32 {
    1.0
}

impl Default for CodeSettings {
    fn default() -> Self {
        Self {
            language_servers: true,
            format_on_save: false,
            editor_zoom: default_editor_zoom(),
        }
    }
}

impl SettingsSchema for CodeSettings {
    const SCOPE: Scope = Scope::Personal;
}
