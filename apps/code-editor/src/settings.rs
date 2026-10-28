//! Code's preferences: they follow the person to every device, and no
//! collaborator sees them.

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

/// The editor's preferences.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeSettings {
    /// Start a language server for a project folder's files (diagnostics,
    /// completion, hover, go to definition).
    pub language_servers: bool,
    /// Format a file with its language server's formatter each time it is
    /// saved.
    pub format_on_save: bool,
    /// The editor's text zoom as a factor of the toolkit's default code size
    /// (1.0 is the theme's own size). Kept per person, so a size chosen once
    /// follows them to the next window and device without changing the
    /// window's chrome.
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
