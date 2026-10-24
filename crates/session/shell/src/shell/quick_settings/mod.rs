//! `QuickSettings` — the top-right status-area popover: sliders, a grid of
//! tiles and rows whose sections (Wi-Fi, Bluetooth, VPN, sound, the
//! battery, devices, the clipboard) open in place on the root page, and the
//! drill-in pages that stay pages (notification history, one accessory's
//! handoff) through `nav_stack`.

pub mod actions;
mod device_status;
pub mod hosted;
pub mod now_playing;
pub mod refresh;
pub(crate) mod root;
pub(crate) mod sections;

/// The clipboard history as Quick Settings shows it: the Clipboard row's
/// count and the section's copies, read together from one snapshot of the
/// history so the row can never say "Empty" over a list of copies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipboardView {
    /// Every copy in the history: the row's count.
    pub total: usize,
    /// The line over the copies: how many there are, or what is being
    /// searched for (typing while the section is open searches).
    pub caption: String,
    /// The copies the section lists (the latest, or the matches).
    pub rows: Vec<hosted::HostedRow>,
}

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
    /// The power modes the daemon offers, under the Power mode tile: the
    /// one place the mode is chosen.
    PowerMode,
    /// Charge, what keeps the machine awake, and a link to the power mode.
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
    pub const ALL: [Self; 8] = [
        Self::Sound,
        Self::Wifi,
        Self::Bluetooth,
        Self::Vpn,
        Self::PowerMode,
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
    /// How tall the host's prompt is, in logical pixels, as it last laid
    /// it out; 0 until it has. The section leaves this much room.
    pub height: u32,
}

impl WifiPasswordPrompt {
    pub fn new(ssid: impl Into<String>) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            ssid: ssid.into(),
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            error: None,
            connecting: false,
            height: 0,
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

/// Take the brightness service's levels into the sliders, by connector, as
/// the service announces a change, whoever made it. Without per-display
/// sliders the built-in panel's level is the one slider's. Returns whether
/// a slider moved.
pub fn follow_brightness_levels(
    state: &mut crate::shell::chrome_state::QuickSettingsState,
    levels: &[os_actions::MonitorLevel],
) -> bool {
    let before = (state.brightness.to_bits(), state.displays.clone());
    for display in &mut state.displays {
        if let Some(level) = levels.iter().find(|l| l.connector == display.connector) {
            display.level = level.level;
        }
    }
    let first = match state.displays.first() {
        Some(display) => Some(display.level),
        None => levels
            .iter()
            .find(|l| l.kind == os_actions::BrightnessKind::Backlight)
            .map(|l| l.level),
    };
    if let Some(level) = first {
        state.brightness = level;
    }
    before != (state.brightness.to_bits(), state.displays.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(
        connector: &str,
        level: f32,
        kind: os_actions::BrightnessKind,
    ) -> os_actions::MonitorLevel {
        os_actions::MonitorLevel {
            connector: connector.into(),
            level,
            kind,
        }
    }

    #[test]
    fn a_backlight_change_moves_its_slider_and_nothing_else() {
        use os_actions::BrightnessKind::{Backlight, Ddc};
        let mut state = crate::shell::chrome_state::QuickSettingsState::default();
        assert!(follow_brightness_levels(
            &mut state,
            &[level("eDP-1", 0.7, Backlight)]
        ));
        assert!(
            (state.brightness - 0.7).abs() < f32::EPSILON,
            "the one slider"
        );

        state.displays = vec![
            crate::shell::chrome_state::DisplayBrightness {
                connector: "eDP-1".into(),
                name: "Built-in display".into(),
                level: 0.7,
            },
            crate::shell::chrome_state::DisplayBrightness {
                connector: "DP-2".into(),
                name: "Dell U2720Q".into(),
                level: 0.5,
            },
        ];
        let levels = [level("eDP-1", 0.25, Backlight), level("DP-2", 0.5, Ddc)];
        assert!(follow_brightness_levels(&mut state, &levels));
        assert!((state.displays[0].level - 0.25).abs() < f32::EPSILON);
        assert!((state.displays[1].level - 0.5).abs() < f32::EPSILON);
        assert!((state.brightness - 0.25).abs() < f32::EPSILON);
        assert!(
            !follow_brightness_levels(&mut state, &levels),
            "already shown"
        );
    }

    #[test]
    fn power_profile_label_matches_variant() {
        assert_eq!(PowerProfile::Saver.label(), "Power Saver");
        assert_eq!(PowerProfile::Balanced.label(), "Balanced");
        assert_eq!(PowerProfile::Performance.label(), "Performance");
    }
}

pub mod radio;
