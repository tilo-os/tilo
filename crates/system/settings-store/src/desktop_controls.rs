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
    /// Add Super+Ctrl+h/j/k/l and Super+Ctrl+Shift+h/j/k/l aliases for
    /// window and strip navigation and for carrying the focused group. The ordinary arrow bindings remain
    /// available.
    pub vim_mode: bool,
    /// Hide the desktop's top bar until the pointer reaches for the top
    /// edge: windows take the whole height, and the bar
    /// shows over them while it is used. Off by default, so the bar's clock,
    /// workspace and status stay in view until the person chooses otherwise.
    /// Per device: a small laptop panel and a large desk display want
    /// different answers.
    pub hide_top_bar: bool,
}

/// The person's desktop controls ([`DesktopControlsConfig::vim_mode`]).
pub const DESKTOP_CONTROLS_SECTION: &str = "desktop_controls";
/// This device's desktop controls ([`DesktopControlsConfig::hide_top_bar`]).
pub const DESKTOP_CONTROLS_DEVICE_SECTION: &str = "desktop_controls_device";

impl DesktopControlsConfig {
    pub fn load() -> Self {
        Self::from_document(&crate::load())
    }

    /// The controls `document` holds: the person's and this device's.
    #[must_use]
    pub fn from_document(document: &serde_json::Map<String, serde_json::Value>) -> Self {
        serde_json::from_value(serde_json::Value::Object(crate::merged_sections(
            document,
            &[DESKTOP_CONTROLS_SECTION, DESKTOP_CONTROLS_DEVICE_SECTION],
        )))
        .unwrap_or_default()
    }
}
