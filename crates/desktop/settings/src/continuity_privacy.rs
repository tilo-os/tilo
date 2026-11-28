//! The facets of the person's `continuity` settings: what may leave or
//! control this machine. Settings writes them and the shell enforces them,
//! both by these keys.

/// Whether a switch the person never set is on: the master switch, every
/// facet and automatic display input alike. Continuity runs only between the
/// person's own certified devices, so it is on until they turn it off.
pub const ON_UNLESS_SET: bool = true;

/// A separately revocable Continuity facet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[serde(rename_all = "snake_case")]
pub enum ContinuityCapability {
    Clipboard,
    Notifications,
    WifiSettings,
    Media,
    DisplayInput,
    FileTransfer,
    RealtimeSessions,
    TypedGamepad,
    AppContinuations,
}

impl ContinuityCapability {
    pub const ALL: [Self; 9] = [
        Self::Clipboard,
        Self::Notifications,
        Self::WifiSettings,
        Self::Media,
        Self::DisplayInput,
        Self::FileTransfer,
        Self::RealtimeSessions,
        Self::TypedGamepad,
        Self::AppContinuations,
    ];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Clipboard => "clipboard",
            Self::Notifications => "notifications",
            Self::WifiSettings => "wifi_settings",
            Self::Media => "media",
            Self::DisplayInput => "display_input",
            Self::FileTransfer => "file_transfer",
            Self::RealtimeSessions => "realtime_sessions",
            Self::TypedGamepad => "typed_gamepad",
            Self::AppContinuations => "app_continuations",
        }
    }

    /// Whether this device's person lets this facet run now: the master switch and the facet,
    /// each on unless they turned it off, as the device's settings say. What every part of the
    /// device that carries a facet between their devices reads before it does.
    #[must_use]
    pub fn allowed(self) -> bool {
        self.allowed_in(&crate::load())
    }

    /// Whether `document`, the device's settings, lets this facet run.
    #[must_use]
    pub fn allowed_in(self, document: &serde_json::Map<String, serde_json::Value>) -> bool {
        let on = |name: &str| match document.get("continuity") {
            None => ON_UNLESS_SET,
            Some(serde_json::Value::Object(section)) => section
                .get(name)
                .map_or(ON_UNLESS_SET, |value| value.as_bool() == Some(true)),
            Some(_) => false,
        };
        on("enabled") && on(self.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A facet runs under the master switch, each on until the person turns it off, and a
    /// settings document that says something else of either holds it off.
    #[test]
    fn a_facet_runs_while_it_and_the_master_switch_are_on() {
        let document = |value: serde_json::Value| value.as_object().cloned().unwrap();
        let gamepads = ContinuityCapability::TypedGamepad;
        assert!(gamepads.allowed_in(&serde_json::Map::new()));
        assert!(gamepads.allowed_in(&document(serde_json::json!({"continuity": {}}))));
        assert!(!gamepads.allowed_in(&document(
            serde_json::json!({"continuity": {"typed_gamepad": false}})
        )));
        assert!(!gamepads.allowed_in(&document(
            serde_json::json!({"continuity": {"enabled": false, "typed_gamepad": true}})
        )));
        assert!(!gamepads.allowed_in(&document(serde_json::json!({"continuity": "off"}))));
        assert!(ContinuityCapability::Media.allowed_in(&document(
            serde_json::json!({"continuity": {"typed_gamepad": false}})
        )));
    }
}
