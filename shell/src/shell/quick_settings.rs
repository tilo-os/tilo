//! `QuickSettings` — the top-right status-area popover. Owns the headline
//! controls (brightness, volume) and popover navigation (root →
//! WiFi/Bluetooth drill-in via `nav_stack`). `wifi_ssid`/`bluetooth_device`
//! populate the toggle-card sub-line, falling back to "Not connected" when off or unset.

/// A single frame in the `QuickSettings` navigation stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QsLevel {
    Root,
    Wifi,
    WifiDetails(String),
    Bluetooth,
    BluetoothDetails(String),
    /// Configured NetworkManager VPN profiles.
    Vpn,
    /// Session-local notification history owned by shell chrome.
    Notifications,
}

/// A `WiFi` entry in the drill-in list. `signal` is a strength bucket
/// `[0, 4]` (mapped to bar-count); `secure` shows the lock icon;
/// `connected` flags the active-radio dot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal: u8,
    pub secure: bool,
    pub connected: bool,
}

/// An in-flight "join this protected network" prompt. `connect_wifi(ssid,
/// None)` fails for every protected network not seen before — every
/// network on first boot — and there's nowhere else to type a passphrase
/// on a machine with no network yet, so the chrome asks for it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WifiPasswordPrompt {
    pub ssid: String,
    /// Held in the clear only while the prompt is up — cleared on
    /// submit, cancel, or popover close.
    pub password: String,
    pub selection: ui::text_selection::Selection,
    /// Set after a failed attempt, so a wrong passphrase says so
    /// instead of reopening an empty prompt.
    pub error: Option<String>,
    /// True while `nmcli` is associating, so the prompt stays inert
    /// instead of looking unresponsive.
    pub connecting: bool,
}

impl WifiPasswordPrompt {
    pub fn new(ssid: impl Into<String>) -> Self {
        Self {
            ssid: ssid.into(),
            password: String::new(),
            selection: ui::text_selection::Selection::collapsed(0),
            error: None,
            connecting: false,
        }
    }

    /// An empty passphrase is rejected by every WPA network.
    pub fn can_submit(&self) -> bool {
        !self.connecting && !self.password.is_empty()
    }
}

/// A Bluetooth device entry in the drill-in list. `connected` flags the active-radio dot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtDevice {
    pub name: String,
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
    pub fn label(self) -> &'static str {
        match self {
            PowerProfile::Saver => "Power Saver",
            PowerProfile::Balanced => "Balanced",
            PowerProfile::Performance => "Performance",
        }
    }
}

/// Logical state for the `QuickSettings` popover.
pub struct QuickSettings {
    brightness: f32,
    volume: f32,
    pub(crate) nav_stack: Vec<QsLevel>,
    /// Cycled by [`Self::cycle_power_profile`].
    pub power_profile: PowerProfile,
    /// Read by the toggle-card sub-line when `wifi_on` is true.
    pub wifi_ssid: Option<String>,
    pub bluetooth_device: Option<String>,
}

impl QuickSettings {
    /// Rooted at `QsLevel::Root`; brightness/volume at 50%, power profile
    /// `Balanced`. Connection labels start unset rather than naming a
    /// network/headset the machine has never seen.
    pub fn new() -> Self {
        Self {
            brightness: 50.0,
            volume: 50.0,
            nav_stack: vec![QsLevel::Root],
            power_profile: PowerProfile::Balanced,
            wifi_ssid: None,
            bluetooth_device: None,
        }
    }

    pub fn brightness(&self) -> f32 {
        self.brightness
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn set_brightness(&mut self, value: f32) {
        self.brightness = value.clamp(0.0, 100.0);
    }

    pub fn set_volume(&mut self, value: f32) {
        self.volume = value.clamp(0.0, 100.0);
    }

    /// The currently visible level. Always at least `Root`: `nav_stack`
    /// starts as `[Root]` and [`Self::pop_level`] refuses to pop the last
    /// frame, so this fallback is unreachable rather than load-bearing.
    pub fn current_level(&self) -> &QsLevel {
        const ROOT: QsLevel = QsLevel::Root;
        self.nav_stack.last().unwrap_or(&ROOT)
    }

    pub fn push_level(&mut self, level: QsLevel) {
        self.nav_stack.push(level);
    }

    /// Pop the top level. No-op when only `Root` remains so the popover
    /// can't be emptied.
    pub fn pop_level(&mut self) {
        if self.nav_stack.len() > 1 {
            self.nav_stack.pop();
        }
    }

    /// `Balanced → Performance → Saver → Balanced`, mirroring the Power Profile row's chevron.
    pub fn cycle_power_profile(&mut self) {
        self.power_profile = match self.power_profile {
            PowerProfile::Saver => PowerProfile::Balanced,
            PowerProfile::Balanced => PowerProfile::Performance,
            PowerProfile::Performance => PowerProfile::Saver,
        };
    }

    /// SSID when `wifi_on` and known, else "Not connected".
    pub fn wifi_connection_label(&self, wifi_on: bool) -> &str {
        if wifi_on {
            self.wifi_ssid.as_deref().unwrap_or("Not connected")
        } else {
            "Not connected"
        }
    }

    /// Same fallback rules as [`Self::wifi_connection_label`].
    pub fn bluetooth_connection_label(&self, bluetooth_on: bool) -> &str {
        if bluetooth_on {
            self.bluetooth_device.as_deref().unwrap_or("Not connected")
        } else {
            "Not connected"
        }
    }
}

impl Default for QuickSettings {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_power_profile_cycles_through_three_states() {
        let mut q = QuickSettings::new();
        assert_eq!(q.power_profile, PowerProfile::Balanced);
        q.cycle_power_profile();
        assert_eq!(q.power_profile, PowerProfile::Performance);
        q.cycle_power_profile();
        assert_eq!(q.power_profile, PowerProfile::Saver);
        q.cycle_power_profile();
        assert_eq!(q.power_profile, PowerProfile::Balanced);
    }

    #[test]
    fn new_names_no_connection_it_has_not_seen() {
        let q = QuickSettings::new();
        assert_eq!(
            q.wifi_ssid, None,
            "a fresh popover must not name a network the machine has not joined"
        );
        assert_eq!(
            q.bluetooth_device, None,
            "a fresh popover must not name a device the machine has not paired"
        );
        assert_eq!(q.wifi_connection_label(true), "Not connected");
        assert_eq!(q.bluetooth_connection_label(true), "Not connected");
    }

    #[test]
    fn notification_history_is_a_first_class_drill_in_level() {
        let mut q = QuickSettings::new();
        q.push_level(QsLevel::Notifications);
        assert_eq!(q.current_level(), &QsLevel::Notifications);
        q.pop_level();
        assert_eq!(q.current_level(), &QsLevel::Root);
    }

    #[test]
    fn wifi_connection_label_is_not_connected_when_off() {
        let q = QuickSettings::new();
        assert_eq!(q.wifi_connection_label(false), "Not connected");
    }

    #[test]
    fn wifi_connection_label_returns_ssid_when_on_and_known() {
        let mut q = QuickSettings::new();
        q.wifi_ssid = Some("Home_Network".to_string());
        assert_eq!(q.wifi_connection_label(true), "Home_Network");
    }

    #[test]
    fn wifi_connection_label_falls_back_when_on_but_no_ssid() {
        let mut q = QuickSettings::new();
        q.wifi_ssid = None;
        assert_eq!(q.wifi_connection_label(true), "Not connected");
    }

    #[test]
    fn bluetooth_connection_label_mirrors_wifi_rules() {
        let mut q = QuickSettings::new();
        q.bluetooth_device = Some("AirPods Pro".to_string());
        assert_eq!(q.bluetooth_connection_label(true), "AirPods Pro");
        assert_eq!(q.bluetooth_connection_label(false), "Not connected");
        q.bluetooth_device = None;
        assert_eq!(q.bluetooth_connection_label(true), "Not connected");
    }

    #[test]
    fn power_profile_label_matches_variant() {
        assert_eq!(PowerProfile::Saver.label(), "Power Saver");
        assert_eq!(PowerProfile::Balanced.label(), "Balanced");
        assert_eq!(PowerProfile::Performance.label(), "Performance");
    }

    #[test]
    fn nav_stack_default_is_root_only() {
        let q = QuickSettings::new();
        assert_eq!(q.current_level(), &QsLevel::Root);
        assert_eq!(q.nav_stack.len(), 1);
    }

    #[test]
    fn brightness_volume_clamp() {
        let mut q = QuickSettings::new();
        q.set_brightness(150.0);
        assert!((q.brightness() - 100.0).abs() < f32::EPSILON);
        q.set_volume(-5.0);
        assert!(q.volume().abs() < f32::EPSILON);
    }
}
