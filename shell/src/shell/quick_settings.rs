//! `QuickSettings` — the top-right status-area popover.
//!
//! Owns the headline controls (brightness, volume) and the popover
//! navigation state (root → WiFi/Bluetooth drill-in via `nav_stack`).
//!
//! State model:
//! - `brightness`, `volume`: `f32` in `[0.0, 100.0]`. Setters clamp.
//! - `nav_stack`: starts with `vec![QsLevel::Root]`; `push_level`/`pop_level`
//!   drive WiFi/BT drill-in.
//! - `power_profile`: cycles Saver → Balanced → Performance → Saver via
//!   [`QuickSettings::cycle_power_profile`].
//! - `wifi_ssid` / `bluetooth_device`: optional connection identifiers used
//!   to populate the toggle-card sub-line. When the toggle is off (or the
//!   field is `None`) the sub-line falls back to `"Not connected"`.

/// A single frame in the QuickSettings navigation stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QsLevel {
    Root,
    Wifi,
    WifiDetails(String),
    Bluetooth,
    BluetoothDetails(String),
}

/// A single WiFi entry rendered in the drill-in list. `signal` is a
/// signal-strength bucket in `[0, 4]` (mapped to bar-count in the
/// renderer); `secure` flips the lock-icon trailing affordance;
/// `connected` flags the active-radio dot. Mock data until real
/// scanning lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal: u8,
    pub secure: bool,
    pub connected: bool,
}

/// An in-flight "join this protected network" prompt.
///
/// A plain `connect_wifi(ssid, None)` works for an open network and for
/// one whose profile NetworkManager already holds, and fails for every
/// protected network the machine has not seen before — which is every
/// network on first boot. There is nowhere else to type a passphrase:
/// Settings doesn't help a machine with no network yet to install
/// anything from. So the chrome asks for it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WifiPasswordPrompt {
    /// The network being joined.
    pub ssid: String,
    /// What has been typed. Held in the clear only for as long as the
    /// prompt is up: it is cleared on submit, on cancel, and whenever
    /// the popover closes.
    pub password: String,
    /// Caret + selection into [`Self::password`].
    pub selection: ui::text_selection::Selection,
    /// Set after a failed attempt, so a wrong passphrase says so instead
    /// of silently reopening an empty prompt.
    pub error: Option<String>,
    /// True while `nmcli` is associating. The prompt stays up and inert
    /// so the user is not left wondering whether the click registered.
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

    /// Whether the Connect button is live. An empty passphrase would be
    /// rejected by every WPA network, so offering it is a wasted round
    /// trip and a confusing failure.
    pub fn can_submit(&self) -> bool {
        !self.connecting && !self.password.is_empty()
    }
}

/// A single Bluetooth device entry rendered in the drill-in list.
/// `battery` is a percentage in `[0, 100]`; `connected` flags the
/// active-radio dot. Mock data until real pairing lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtDevice {
    pub name: String,
    pub battery: u8,
    pub connected: bool,
}

/// Power-management profile shown in the Power Profile row at the bottom
/// of the QuickSettings popover — the three canonical profiles
/// `powerprofilesctl` exposes on Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerProfile {
    Saver,
    Balanced,
    Performance,
}

impl PowerProfile {
    /// Human-readable label rendered in the popover row.
    pub fn label(self) -> &'static str {
        match self {
            PowerProfile::Saver => "Power Saver",
            PowerProfile::Balanced => "Balanced",
            PowerProfile::Performance => "Performance",
        }
    }
}

/// Logical state for the QuickSettings popover.
pub struct QuickSettings {
    brightness: f32,
    volume: f32,
    pub(crate) nav_stack: Vec<QsLevel>,
    /// Active power profile. Cycled by [`Self::cycle_power_profile`].
    pub power_profile: PowerProfile,
    /// SSID of the currently connected Wi-Fi network, if any. The toggle
    /// card sub-line reads this when `wifi_on` is true.
    pub wifi_ssid: Option<String>,
    /// Display name of the currently paired Bluetooth device, if any.
    pub bluetooth_device: Option<String>,
}

impl QuickSettings {
    /// Construct a popover rooted at `QsLevel::Root`. Brightness and volume
    /// initialise to 50%; the power profile defaults to `Balanced`.
    ///
    /// Both connection labels start unset, and read "Not connected" until
    /// something actually reports a connection. Seeding them kept the
    /// sub-lines from rendering blank by naming a network and a headset
    /// the machine had never seen.
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

    /// Set brightness, clamped to `[0.0, 100.0]`.
    pub fn set_brightness(&mut self, value: f32) {
        self.brightness = value.clamp(0.0, 100.0);
    }

    /// Set volume, clamped to `[0.0, 100.0]`.
    pub fn set_volume(&mut self, value: f32) {
        self.volume = value.clamp(0.0, 100.0);
    }

    /// The currently visible level. Always at least `Root`.
    pub fn current_level(&self) -> &QsLevel {
        self.nav_stack
            .last()
            .expect("nav_stack always has at least Root")
    }

    /// Push a new level onto the nav stack (drill into WiFi/BT details).
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

    /// Cycle the power profile in the order
    /// `Balanced → Performance → Saver → Balanced`.
    ///
    /// Mirrors the chevron on the Power Profile row — each click advances
    /// to the next profile and wraps around at the end.
    pub fn cycle_power_profile(&mut self) {
        self.power_profile = match self.power_profile {
            PowerProfile::Saver => PowerProfile::Balanced,
            PowerProfile::Balanced => PowerProfile::Performance,
            PowerProfile::Performance => PowerProfile::Saver,
        };
    }

    /// Sub-line text rendered under the "Wi-Fi" label on the toggle card.
    ///
    /// Returns the SSID when `wifi_on` is true and an SSID is known; falls
    /// back to `"Not connected"` otherwise so the sub-line never goes
    /// blank.
    pub fn wifi_connection_label(&self, wifi_on: bool) -> &str {
        if wifi_on {
            self.wifi_ssid.as_deref().unwrap_or("Not connected")
        } else {
            "Not connected"
        }
    }

    /// Sub-line text rendered under the "Bluetooth" label on the toggle
    /// card. Same fallback rules as [`Self::wifi_connection_label`].
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
        assert_eq!(q.brightness(), 100.0);
        q.set_volume(-5.0);
        assert_eq!(q.volume(), 0.0);
    }
}
