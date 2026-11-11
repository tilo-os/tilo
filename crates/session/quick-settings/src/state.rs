//! Quick Settings' state: what the panel shows, local to this device.

use connectivity::model::{Bluetooth, Network, Vpn};

use crate::PowerProfile;

/// One monitor's brightness control in Quick Settings.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayBrightness {
    /// DRM connector, e.g. `DP-2` — what the brightness service keys on.
    pub connector: String,
    /// What a person calls the display: its model, as its EDID names it, or
    /// the part it plays — never the connector, which is how the machine
    /// knows it. The row the panel shows it on carries this name.
    pub name: String,
    /// 0.0..=1.0.
    pub level: f32,
}

/// Put the display the person works on (`focused`, its connector) first,
/// keeping the others in their order: Quick Settings shows that display's
/// slider at the top.
pub fn focused_display_first(displays: &mut Vec<DisplayBrightness>, focused: Option<&str>) {
    if let Some(index) =
        focused.and_then(|focused| displays.iter().position(|d| d.connector == focused))
    {
        let display = displays.remove(index);
        displays.insert(0, display);
    }
}

/// A radio change under way, so its tile says "Turning on…" or "Turning
/// off…" until the machine answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioChange {
    Wifi(bool),
    Airplane(bool),
}

/// Local UI state; does not sync to peers — device settings like
/// brightness, volume, and radio toggles are always per-device.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each device-local quick-settings flag maps to a distinct hardware truth or user control and must remain independently renderable"
)]
pub struct QuickSettingsState {
    /// The radios, the links and the networks in reach, as the connectivity
    /// service publishes them.
    pub network: Network,
    /// The Bluetooth adapter and what it knows, as the same service
    /// publishes them.
    pub bluetooth: Bluetooth,
    /// The VPN profiles, as the same service publishes them.
    pub vpn: Vpn,
    /// 0.0..=1.0.
    pub brightness: f32,
    /// False after the session publishes no display whose brightness can be
    /// set. The slider remains visible but honestly disabled.
    pub brightness_available: bool,
    /// Every monitor whose brightness can be set (the laptop panel's
    /// backlight, external monitors over DDC/CI), built-in first. Each gets
    /// its own slider once there is more than one; empty falls back to the
    /// single `brightness` control.
    pub displays: Vec<DisplayBrightness>,
    /// The connector of the display the person works on, when the desktop
    /// spans several: what "brighter" and "dimmer" adjust.
    pub focused_display: Option<String>,
    /// False after the audio service publishes no output.
    pub volume_available: bool,
    /// Battery, link and volume: the one copy the top bar draws too, taken
    /// each frame from the session's readings with no lock
    /// held across a draw; `None` fields until a reading lands.
    pub status: crate::status::SystemStatus,
    /// How many firmware updates the system service's `system-update`
    /// reading offers; `None` when `fwupd` is unavailable.
    pub firmware_updates: Option<usize>,
    pub night_light: bool,
    /// The machine has an accelerometer, so its screen can follow the device
    /// and the rotation-lock tile is offered.
    pub rotation_available: bool,
    /// `display.rotation_lock`: the screen stays put when the device turns.
    pub rotation_lock: bool,
    /// The form for joining a network that broadcasts no name; `None`
    /// except while it is up.
    pub wifi_hidden: Option<crate::HiddenNetworkPrompt>,
    /// SSID currently being joined off-thread. Kept separate from the
    /// password prompt so open networks can show progress without presenting
    /// an irrelevant secret field.
    pub wifi_joining: Option<String>,
    /// The passphrase prompt for a protected network; `None` except
    /// while the user is typing into it.
    pub wifi_password: Option<crate::WifiPasswordPrompt>,
    /// The power profile in force, folded from the `power:sleep` reading.
    /// Defaults to `Balanced` so a host without `power-profiles-daemon`
    /// still renders a sensible label.
    pub power_profile: PowerProfile,
    /// The profiles the power daemon offers on this machine; empty until
    /// read. The power row cycles only through these.
    pub power_profiles: Vec<PowerProfile>,
    /// Apps currently keeping the screen awake (an idle inhibitor held
    /// through Wayland, the portal or `org.freedesktop.ScreenSaver`), named
    /// as they asked. Shown on the power row so a screen that never locks
    /// has a visible reason.
    pub awake_holders: Vec<String>,
    /// Certified personal devices known to the device fabric, read from the
    /// `session:devices` reading. `None` when this session has no device
    /// fabric.
    pub personal_devices: Option<usize>,
    /// Mirror of the persisted Do Not Disturb switch for the tile.
    pub do_not_disturb: bool,
    /// The clipboard history the Clipboard row counts and its section
    /// lists, from one snapshot.
    pub clipboard: crate::ClipboardView,
    /// This machine's sound outputs, as the audio service lists them.
    pub sound_outputs: Vec<crate::SoundOutput>,
    /// The microphone sound records from, as the audio service reads it:
    /// the row it gets shows its level and answers its mute with this.
    pub microphone: Option<crate::SoundInput>,
    /// Every application's stream, as the audio service reads it: each
    /// row shows where it goes, its level and its mute.
    pub sound_streams: Vec<crate::SoundStream>,
    /// The Devices section's rows, in order, as the session publishes them.
    pub device_rows: Vec<crate::hosted::HostedRow>,
    /// Which controls the panel shows, in the person's order.
    pub arrangement: settings_store::hardware::QuickSettingsConfig,
    /// How many notifications the session keeps: the row's count.
    pub notifications: usize,
    /// The person's devices a Bluetooth accessory can move to, read while
    /// an accessory's page shows; `None` without a device fabric.
    pub handoff_targets: Option<Vec<crate::HandoffTarget>>,
    /// What plays, while a player is playing or paused.
    pub now_playing: Option<crate::NowPlaying>,
    /// The session another of the person's devices hands this one, as the
    /// `session:devices` reading publishes it.
    pub offer: Option<crate::MediaOffer>,
    /// Why the last press did not reach the device: the session publishes no
    /// answer to it, or it refused the action. `None` while every press has
    /// landed; the person puts it away with [`crate::QsEffect::DismissRefusal`]
    /// or the next press.
    pub refusal: Option<String>,
}

/// Whether the night light shows now: the person's switch, or the day/night
/// phase when it follows the Appearance schedule.
fn night_light_shows() -> bool {
    if !settings_store::hardware::NIGHT_LIGHT_SCHEDULE.get() {
        return settings_store::hardware::NIGHT_LIGHT.get();
    }
    settings_store::day_schedule::Scheduled::from_settings(&settings_store::load())
        .is_night(ui::theme::local_moment())
}

impl Default for QuickSettingsState {
    fn default() -> Self {
        Self {
            network: Network::default(),
            bluetooth: Bluetooth::default(),
            vpn: Vpn::default(),
            brightness: 0.6,
            brightness_available: false,
            displays: Vec::new(),
            focused_display: None,
            volume_available: true,
            status: crate::status::SystemStatus::default(),
            firmware_updates: None,
            night_light: night_light_shows(),
            rotation_available: os_actions::has_accelerometer(),
            rotation_lock: settings_store::hardware::ROTATION_LOCK.get(),
            wifi_hidden: None,
            wifi_joining: None,
            wifi_password: None,
            power_profile: PowerProfile::Balanced,
            power_profiles: Vec::new(),
            awake_holders: Vec::new(),
            personal_devices: None,
            do_not_disturb: false,
            clipboard: crate::ClipboardView::default(),
            sound_outputs: Vec::new(),
            microphone: None,
            sound_streams: Vec::new(),
            device_rows: Vec::new(),
            // As the person left it: the session keeps the arrangement and publishes it, and a
            // panel started before that reading arrives starts where they left it.
            arrangement: settings_store::hardware::QuickSettingsConfig::load(),
            notifications: 0,
            handoff_targets: None,
            now_playing: None,
            offer: None,
            refusal: None,
        }
    }
}

impl QuickSettingsState {
    /// Whether anything on this device can set the brightness: a brightness
    /// the session published, on the one slider or on the displays' own.
    #[must_use]
    pub fn can_set_brightness(&self) -> bool {
        self.brightness_available || !self.displays.is_empty()
    }

    /// Whether this machine has a Wi-Fi radio.
    #[must_use]
    pub fn wifi_available(&self) -> bool {
        self.network.wifi.available
    }

    /// Whether this machine reaches the network over a cable: a machine
    /// without a radio shows the wired link in Wi-Fi's place.
    #[must_use]
    pub fn wired_available(&self) -> bool {
        self.network.wired.is_some()
    }

    /// The radio change the connectivity service has not answered yet, and
    /// the direction it takes the radio from where the reading still stands.
    #[must_use]
    pub fn radio_change(&self) -> Option<RadioChange> {
        let pending = |actions: [&str; 2]| {
            self.network
                .pending
                .iter()
                .any(|id| actions.contains(&id.as_str()))
        };
        if pending(["set-wifi", "toggle-wifi"]) {
            return Some(RadioChange::Wifi(!self.network.wifi.on));
        }
        if pending(["set-airplane-mode", "toggle-airplane-mode"]) {
            return Some(RadioChange::Airplane(!self.network.airplane_mode));
        }
        None
    }

    /// A radio change is under way: its tile takes no second press, and
    /// says so, until the machine answers.
    #[must_use]
    pub fn radio_change_pending(&self) -> bool {
        self.radio_change().is_some()
    }

    /// The service is looking for Wi-Fi networks now.
    #[must_use]
    pub fn wifi_scanning(&self) -> bool {
        self.network.pending.iter().any(|id| id == "scan-wifi")
    }

    /// The saved `NetworkManager` profile for the network named `name`,
    /// when this machine keeps one.
    #[must_use]
    pub fn saved_network(&self, name: &str) -> Option<&connectivity::model::SavedNetwork> {
        self.network.saved.iter().find(|saved| saved.name == name)
    }

    /// A panel with stand-in readings and no device reads, for shots and
    /// tests.
    #[must_use]
    pub fn fixture() -> Self {
        Self {
            // The controls as they come, not as this machine's settings happen to have them: what
            // a screenshot, a test or a panel run on its own shows.
            arrangement: settings_store::hardware::QuickSettingsConfig::default(),
            brightness: 0.6,
            brightness_available: true,
            status: crate::status::SystemStatus {
                volume: Some(media::audio::default_node::VolumeState {
                    level: 0.4,
                    muted: false,
                }),
                ..crate::status::SystemStatus::default()
            },
            ..Self::default()
        }
    }

    /// The panel before anything of the device is read: no levels, no
    /// radios, no controls found yet. A session publishes its readings over
    /// it.
    #[must_use]
    pub fn unread() -> Self {
        Self {
            brightness_available: false,
            volume_available: false,
            ..Self::fixture()
        }
    }
}
