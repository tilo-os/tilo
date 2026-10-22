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

    /// The facet's switch in the `continuity` section.
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
}
