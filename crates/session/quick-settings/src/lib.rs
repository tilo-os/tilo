#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        reason = "in a test an exact comparison is the assertion, not a hazard"
    )
)]
//! Quick Settings — the top-right status-area popover — as an app: sliders,
//! a grid of tiles and rows, the page each section (Wi-Fi, Bluetooth, VPN,
//! sound, the battery, devices, the clipboard) opens in the controls'
//! place, and the pages of the notifications and one accessory's handoff.
//!
//! The app's resource is the panel ([`QuickSettings`]): which page shows
//! and what is open, changed by its actions. What it does to the device is
//! its effects ([`QsEffect`]), which the session's published resources
//! carry out ([`verbs`]) on what they publish ([`readings`]); a screenshot
//! or a test lends a fixture instead.

// The derives below name the resource crate by its own name.
use ui::tilo_resource;

pub mod actions;
mod device_status;
pub mod effect;
mod faces;
pub mod host;
pub mod hosted;
mod i18n;
mod live;
mod media;
pub(crate) mod readings;
pub mod resource;
pub mod scenarios;
pub(crate) mod session;
mod sound;
mod state;
pub mod status;
mod verbs;
mod view;

pub use effect::QsEffect;
pub use faces::{available as control_available, subline};
// The one reading of what plays, which the desk's widget, the lock
// screen's card and the bar's pill show as this panel does.
pub use resource::{Page, QuickSettings, QuickSettingsApp};
pub use tilo_desktop_surface::now_playing::{
    NowPlaying, Offer as MediaOffer, Output as MediaOutput, Player as MediaPlayer,
};

pub use state::{DisplayBrightness, QuickSettingsState, RadioChange, focused_display_first};
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

/// One of the person's devices a Bluetooth accessory can move to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffTarget {
    pub id: String,
    pub label: String,
    pub online: bool,
    /// This device.
    pub local: bool,
}

/// A part of Quick Settings whose page replaces the controls, with the
/// standard drill motion; Back returns. One is open at a time.
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

/// The microphone sound records from, as the Sound section shows it: its
/// level and its mute are the panel's controls for it.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundInput {
    /// The audio graph's node: what the microphone's actions name.
    pub node: u32,
    /// The microphone's name, as the audio service reads it.
    pub name: String,
    /// Its level from 0 to 1, whether or not muted.
    pub level: f32,
    pub muted: bool,
}

/// One application's stream, as the Sound section lists it: what it plays,
/// where that goes now, how loud.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundStream {
    /// The stream's audio node: what routing it, its level and its mute
    /// name.
    pub node: u32,
    /// The application that owns the stream.
    pub label: String,
    /// What it plays or records, when the application says: the row's own
    /// column.
    pub plays: Option<String>,
    /// Whether it plays (`true`) or records.
    pub playback: bool,
    /// The output (or input) it is linked to now.
    pub target: Option<u32>,
    /// Its level from 0 to 1, whether or not muted.
    pub level: f32,
    pub muted: bool,
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
    /// The name the power service publishes the profile by, and takes it
    /// back as.
    #[must_use]
    pub(crate) const fn published(self) -> &'static str {
        match self {
            PowerProfile::Saver => "power-saver",
            PowerProfile::Balanced => "balanced",
            PowerProfile::Performance => "performance",
        }
    }

    pub fn label(self) -> String {
        crate::i18n::t(match self {
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
