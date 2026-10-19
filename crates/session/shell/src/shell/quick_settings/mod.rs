//! `QuickSettings` — the top-right status-area popover. Owns the headline
//! controls (brightness, volume) and popover navigation (root →
//! WiFi/Bluetooth drill-in via `nav_stack`). `wifi_ssid`/`bluetooth_device`
//! populate the toggle-card sub-line, falling back to "Not connected" when off or unset.

pub mod actions;
pub(crate) mod bluetooth;
pub mod now_playing;
pub mod refresh;
pub(crate) mod root;
pub(crate) mod vpn;
pub(crate) mod wifi;

/// A single frame in the `QuickSettings` navigation stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QsLevel {
    Root,
    Wifi,
    WifiDetails(String),
    Bluetooth,
    BluetoothDetails(String),
    /// Configured `NetworkManager` VPN profiles.
    Vpn,
    /// Session-local notification history owned by shell chrome.
    Notifications,
    /// Private clipboard history, including authenticated same-person copies.
    Clipboard,
    /// Certified personal devices, local audio outputs, and media routes.
    Devices,
}

/// A `WiFi` entry in the drill-in list. `signal` is a strength bucket
/// `[0, 4]` (mapped to bar-count); `secure` shows the lock icon;
/// `connected` flags the active-radio dot. `security` is the scanned
/// authentication mode, which decides whether a passphrase may be shared
/// with the person's other devices without scanning again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal: u8,
    pub secure: bool,
    pub security: os_actions::WifiSecurity,
    pub connected: bool,
}

/// An in-flight "join this protected network" prompt. `connect_wifi(ssid,
/// None)` fails for every protected network not seen before — every
/// network on first boot — and there's nowhere else to type a passphrase
/// on a machine with no network yet, so the chrome asks for it.
///
/// The passphrase itself is never chrome state: the overlay host's secret
/// field holds it while this prompt is up, and lets go of it with the
/// prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiPasswordPrompt {
    pub ssid: String,
    /// Tells this prompt from any other, even for the same network, so
    /// nothing typed into one is ever shown in the next.
    pub id: u64,
    /// Set after a failed attempt, so a wrong passphrase says so
    /// instead of reopening an empty prompt.
    pub error: Option<String>,
    /// True while `nmcli` is associating, so the prompt stays inert
    /// instead of looking unresponsive.
    pub connecting: bool,
}

impl WifiPasswordPrompt {
    pub fn new(ssid: impl Into<String>) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            ssid: ssid.into(),
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            error: None,
            connecting: false,
        }
    }

    /// An empty passphrase is rejected by every WPA network.
    pub fn can_submit(&self, password: &str) -> bool {
        !self.connecting && !password.is_empty()
    }
}

/// A Bluetooth device entry in the drill-in list. `connected` flags the active-radio dot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtDevice {
    pub name: String,
    /// Stable `BlueZ` address used only for local actions and ephemeral
    /// exact-device handoff. It is never replicated in personal-fabric state.
    pub address: String,
    pub battery: u8,
    pub connected: bool,
}

/// What a key does in a drill-in pane's list.
#[derive(Clone, Copy)]
pub(crate) enum PaneKey {
    /// Move focus to the previous row when `true`, the next one otherwise.
    Focus(bool),
    /// Use the focused row.
    Activate,
}

impl PaneKey {
    /// Tab moves along `backwards`, arrows move regardless of it, Enter and
    /// Space activate. Any other key is not the pane's.
    pub(crate) fn of(logical_key: &winit::keyboard::Key, backwards: bool) -> Option<Self> {
        use winit::keyboard::{Key, NamedKey};
        match logical_key {
            Key::Named(NamedKey::Tab) => Some(Self::Focus(backwards)),
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => Some(Self::Focus(true)),
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => Some(Self::Focus(false)),
            Key::Named(NamedKey::Enter | NamedKey::Space) => Some(Self::Activate),
            _ => None,
        }
    }
}

/// The three canonical profiles `powerprofilesctl` exposes on Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerProfile {
    Saver,
    Balanced,
    Performance,
}

impl PowerProfile {
    pub fn label(self) -> String {
        crate::shell::i18n::t(match self {
            PowerProfile::Saver => "qs-power-saver",
            PowerProfile::Balanced => "qs-power-balanced",
            PowerProfile::Performance => "qs-power-performance",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_profile_label_matches_variant() {
        assert_eq!(PowerProfile::Saver.label(), "Power Saver");
        assert_eq!(PowerProfile::Balanced.label(), "Balanced");
        assert_eq!(PowerProfile::Performance.label(), "Performance");
    }
}

pub mod radio;
