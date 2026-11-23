//! The shell's desktop-navigation preferences: the person's
//! (`desktop_controls`) and this device's (`desktop_controls_device`).

use serde::{Deserialize, Serialize};

/// Desktop-navigation preferences consumed directly by the shell.
///
/// This is deliberately separate from editable one-shot chords: Vim mode is
/// a complete navigation layer whose keys must stay internally consistent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopControlsConfig {
    pub vim_mode: bool,
    pub hide_top_bar: bool,
}

pub const DESKTOP_CONTROLS_SECTION: &str = "desktop_controls";
pub const DESKTOP_CONTROLS_DEVICE_SECTION: &str = "desktop_controls_device";

impl DesktopControlsConfig {
    pub fn load() -> Self {
        Self::from_document(&crate::load())
    }

    #[must_use]
    pub fn from_document(document: &serde_json::Map<String, serde_json::Value>) -> Self {
        serde_json::from_value(serde_json::Value::Object(crate::merged_sections(
            document,
            &[DESKTOP_CONTROLS_SECTION, DESKTOP_CONTROLS_DEVICE_SECTION],
        )))
        .unwrap_or_default()
    }
}
