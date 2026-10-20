//! Code's preferences: they follow the person to every device, and no
//! collaborator sees them.

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

/// The editor's preferences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeSettings {
    /// Start a language server for a project folder's files (diagnostics,
    /// completion, hover, go to definition).
    pub language_servers: bool,
    /// Format a file with its language server's formatter each time it is
    /// saved.
    pub format_on_save: bool,
}

impl Default for CodeSettings {
    fn default() -> Self {
        Self {
            language_servers: true,
            format_on_save: false,
        }
    }
}

impl SettingsSchema for CodeSettings {
    const SCOPE: Scope = Scope::Personal;
}
