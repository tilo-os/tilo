#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        reason = "in a test an exact comparison is the assertion, not a hazard"
    )
)]
//! Quick Settings — the top-right status-area popover — as an app: sliders,
//! a grid of tiles and rows whose sections (Wi-Fi, Bluetooth, VPN, sound,
//! the battery, devices, the clipboard) open in place, and the pages that
//! stay pages (the notifications, one accessory's handoff).
//!
//! The app's resource is the panel ([`QuickSettings`]): which page shows
//! and what is open, changed by its actions. What it does to the device is
//! its effects ([`QsEffect`]). The desktop that shows it lends it a
//! [`host::PanelHost`] that reads the device into [`QuickSettingsState`]
//! and carries the effects out; a screenshot or a test lends a fixture.

// The derives below name the resource crate by its own name.
use ui::tilo_resource;

pub mod actions;
pub mod device;
mod device_status;
pub mod effect;
mod faces;
pub mod host;
pub mod hosted;
mod i18n;
mod now_playing;
mod prepare;
pub mod resource;
pub mod scenarios;
mod state;
pub mod status;
mod view;

pub use effect::QsEffect;
pub use faces::subline;
pub use now_playing::{MediaOutput, MediaPlayer, NowPlaying};
pub use prepare::prepare;
pub use resource::{Page, QuickSettings, QuickSettingsApp};

pub use state::{DisplayBrightness, PasteOrigin, QuickSettingsState, focused_display_first};
pub use status::SystemStatus;

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

/// What was pressed on a row of the Devices section.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ui::tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum RowPart {
    /// The row itself.
    Row,
    /// The one action the row names (stop, cancel, try again).
    Action,
    /// Allow, on a request.
    Allow,
    /// Decline, on a request.
    Decline,
    /// Drive the device with this machine's pointer.
    Control,
}

/// The notifications kept since they arrived, as the panel lists them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotificationsView {
    /// Newest first.
    pub rows: Vec<NotificationRow>,
    /// Why fresh notifications are being held (Do Not Disturb, presenting,
    /// a focus schedule); `None` while they show.
    pub quiet: Option<String>,
    /// The last Clear all can still be undone.
    pub can_undo: bool,
}

/// One kept notification.
#[derive(Clone, Debug, PartialEq)]
pub struct NotificationRow {
    pub id: u64,
    /// The app group it is listed under, by its stable key.
    pub group: String,
    /// The app, as the group is named.
    pub app: String,
    pub title: String,
    pub body: String,
    /// How long ago it arrived.
    pub age: String,
    /// The sender's choices, as `(key, label)`.
    pub actions: Vec<(String, String)>,
    /// The agent it can be handed to, where one is live.
    pub agent: Option<String>,
    /// Choosing it opens something (its sender, or its app); otherwise it
    /// is read in full in the panel.
    pub opens: bool,
}

/// One of the person's devices a Bluetooth accessory can move to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffTarget {
    pub id: String,
    pub label: String,
    pub online: bool,
    /// This device.
    pub local: bool,
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
#[derive(
    ui::tilo_resource::Model,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    ui::tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum QsSection {
    /// The networks in range, the one in use first.
    #[default]
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
    /// Certified personal devices, local audio outputs and media routes,
    /// as the desktop describes them ([`QuickSettingsState::device_rows`]).
    ///
    /// [`QuickSettingsState::device_rows`]: crate::QuickSettingsState::device_rows
    Devices,
    /// Private clipboard history, including authenticated same-person
    /// copies.
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

/// A network profile `NetworkManager` keeps, so the machine joins it again
/// without a password typed and without being asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedNetwork {
    pub name: String,
    /// The profile's `NetworkManager` UUID: what forgetting it, or turning
    /// its autoconnect off, names.
    pub profile: String,
    /// It joins by itself whenever it is in reach.
    pub autoconnect: bool,
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

/// The form for joining a network that broadcasts no name: only its name
/// and its password find it. Neither is chrome state: the fields hold them
/// while the form is up, and the machine saves them with the profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HiddenNetworkPrompt {
    /// Tells this form from any other, so nothing typed into one is ever
    /// shown in the next.
    pub id: u64,
    /// The name being joined, once the machine is.
    pub ssid: String,
    /// Set after a failed attempt, so a wrong password says so instead of
    /// the form reopening empty.
    pub error: Option<String>,
    /// True while the machine is associating, so the form stays inert
    /// instead of looking unresponsive.
    pub connecting: bool,
}

impl HiddenNetworkPrompt {
    #[must_use]
    pub fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ssid: String::new(),
            error: None,
            connecting: false,
        }
    }
}

impl Default for HiddenNetworkPrompt {
    fn default() -> Self {
        Self::new()
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

/// A Bluetooth device in reach that is not paired yet, as the Bluetooth
/// section lists it while it looks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtNearby {
    pub name: String,
    /// `BlueZ`'s address: what pairing it names.
    pub address: String,
    /// Signal while it is in reach, in dBm.
    pub signal: Option<i16>,
}

/// The three canonical profiles `powerprofilesctl` exposes on Linux.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ui::tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum PowerProfile {
    Saver,
    #[default]
    Balanced,
    Performance,
}

impl PowerProfile {
    pub fn label(self) -> String {
        crate::i18n::t(match self {
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
    state: &mut crate::QuickSettingsState,
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
        let mut state = crate::QuickSettingsState::default();
        assert!(follow_brightness_levels(
            &mut state,
            &[level("eDP-1", 0.7, Backlight)]
        ));
        assert!(
            (state.brightness - 0.7).abs() < f32::EPSILON,
            "the one slider"
        );

        state.displays = vec![
            crate::DisplayBrightness {
                connector: "eDP-1".into(),
                name: "Built-in display".into(),
                level: 0.7,
            },
            crate::DisplayBrightness {
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
