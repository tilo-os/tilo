//! `QuickSettings` — the top-right status-area popover: sliders, a grid of
//! tiles and rows whose sections (Wi-Fi, Bluetooth, VPN, sound, the
//! battery, devices, the clipboard) open in place on the root page, and the
//! drill-in pages that stay pages (notification history, one accessory's
//! handoff) through `nav_stack`.

pub mod actions;
pub mod hosted;
pub mod now_playing;
pub mod refresh;
pub(crate) mod root;
pub(crate) mod sections;

/// A single frame in the `QuickSettings` navigation stack. Everything else
/// opens in place on the root page ([`QsSection`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QsLevel {
    Root,
    /// One accessory's exact-device handoff to the person's other devices.
    BluetoothDetails(String),
    /// Session-local notification history owned by shell chrome.
    Notifications,
}

/// A part of Quick Settings that opens in place on the root page, below
/// the control it belongs to, with the standard disclosure motion. One is
/// open at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QsSection {
    /// The networks in range, the one in use first.
    Wifi,
    /// Paired accessories.
    Bluetooth,
    /// `NetworkManager`'s VPN profiles.
    Vpn,
    /// Charge, the power mode and what keeps the machine awake.
    Battery,
    /// Where sound plays: this machine's outputs.
    Sound,
    /// Certified personal devices, local audio outputs and media routes.
    /// The overlay host paints its rows ([`QuickSettingsState::device_rows`]).
    ///
    /// [`QuickSettingsState::device_rows`]: crate::shell::chrome_state::QuickSettingsState::device_rows
    Devices,
    /// Private clipboard history, including authenticated same-person
    /// copies. The overlay host paints its rows.
    Clipboard,
}

impl QsSection {
    /// Every section, in the order the page is laid out.
    pub const ALL: [Self; 7] = [
        Self::Sound,
        Self::Wifi,
        Self::Bluetooth,
        Self::Vpn,
        Self::Battery,
        Self::Devices,
        Self::Clipboard,
    ];
}

/// One of this machine's sound outputs (speakers, headphones, a display's
/// speakers), as the Sound section lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundOutput {
    /// The audio graph's node: what routing to it names.
    pub node: u32,
    pub name: String,
    /// Sound plays here now.
    pub current: bool,
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
    /// This machine has joined it before: it connects without asking.
    pub known: bool,
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
