//! Quick Settings' state: what the panel shows, local to this device.

use crate::{BtDevice, BtNearby, PowerProfile, QsLevel, WifiNetwork};

/// Where clipboard history was summoned to paste: the focused field's caret
/// and the focused window, in desktop coordinates, either unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PasteOrigin {
    pub caret: Option<ui::vello::kurbo::Rect>,
    pub window: Option<ui::vello::kurbo::Rect>,
}

/// One monitor's brightness control in Quick Settings.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayBrightness {
    /// DRM connector, e.g. `DP-2` — what the brightness service keys on.
    pub connector: String,
    /// The monitor's EDID name, e.g. `Dell U2720Q`, else its connector.
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

/// Local UI state; does not sync to peers — device settings like
/// brightness, volume, and radio toggles are always per-device.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each device-local quick-settings flag maps to a distinct hardware truth or user control and must remain independently renderable"
)]
pub struct QuickSettingsState {
    /// 0.0..=1.0.
    pub brightness: f32,
    /// False after the device probe confirms there is no controllable
    /// backlight. The slider remains visible but honestly disabled.
    pub brightness_available: bool,
    /// Every monitor whose brightness can be set (the laptop panel's
    /// backlight, external monitors over DDC/CI), built-in first. Each gets
    /// its own slider once there is more than one; empty falls back to the
    /// single `brightness` control.
    pub displays: Vec<DisplayBrightness>,
    /// The connector of the display the person works on, when the desktop
    /// spans several: what "brighter" and "dimmer" adjust. The overlay host
    /// keeps it current whenever search refreshes.
    pub focused_display: Option<String>,
    /// False after the audio service cannot provide a sink reading.
    pub volume_available: bool,
    pub wifi: bool,
    pub bluetooth: bool,
    /// `true` only when `BlueZ` reported a physical adapter. Keeping this
    /// separate from `bluetooth` prevents an unavailable controller from
    /// looking like a permanently-off switch.
    pub bluetooth_available: bool,
    /// Primary `NetworkManager` Ethernet interface. `None` means no adapter.
    pub wired: Option<os_actions::WiredInfo>,
    /// Battery, link and volume: the one copy the top bar draws too, taken
    /// each frame from the shell's status bridge with no lock
    /// held across a draw; `None` fields until a reading lands.
    pub status: crate::status::SystemStatus,
    /// `None` when fwupd is unavailable, otherwise the offered update count.
    pub firmware_updates: Option<usize>,
    pub radios: os_actions::RadioState,
    pub vpns: Vec<os_actions::VpnConnectionInfo>,
    pub privacy: os_actions::PrivacyStatus,
    pub night_light: bool,
    /// The machine has an accelerometer, so its screen can follow the device
    /// and the rotation-lock tile is offered.
    pub rotation_available: bool,
    /// `display.rotation_lock`: the screen stays put when the device turns.
    pub rotation_lock: bool,
    /// The renderer dispatches on `nav_stack.last()` (always at least
    /// `Root`) — pushing `Wifi`/`Bluetooth` flips the popover to the
    /// drill-in list.
    pub nav_stack: Vec<QsLevel>,
    /// The section open in place on the root page (Wi-Fi's networks,
    /// Bluetooth's devices, a VPN's profiles, the battery); at most one.
    pub section: Option<crate::QsSection>,
    /// Set while clipboard history is open to paste (Super+V): where it was
    /// summoned from, so it opens there instead of under the tray and a
    /// choice is pasted into that field. `None` when history is browsed from
    /// Quick Settings. Cleared whenever Quick Settings opens another way.
    pub paste_origin: Option<PasteOrigin>,
    pub wifi_networks: Vec<WifiNetwork>,
    /// The profiles `NetworkManager` keeps, in reach or not.
    pub saved_networks: Vec<crate::SavedNetwork>,
    /// The form for joining a network that broadcasts no name; `None`
    /// except while it is up.
    pub wifi_hidden: Option<crate::HiddenNetworkPrompt>,
    /// True only while `NetworkManager` discovery is outstanding.
    pub wifi_scanning: bool,
    /// A typed radio mutation is admitted but has not completed.
    pub radio_change_pending: bool,
    /// The radio change under way, so its tile says "Turning on…" or
    /// "Turning off…" until the machine answers.
    pub radio_change: Option<os_actions::RadioChange>,
    /// Why Wi-Fi is not in the state last asked for, until the next change.
    pub wifi_problem: Option<crate::radio::WifiProblem>,
    /// Last discovery failure, kept separate from an honest empty result.
    pub wifi_scan_error: Option<String>,
    /// SSID currently being joined off-thread. Kept separate from the
    /// password prompt so open networks can show progress without presenting
    /// an irrelevant secret field.
    pub wifi_joining: Option<String>,
    pub bt_devices: Vec<BtDevice>,
    /// The Bluetooth devices in reach that are not paired, strongest signal
    /// first.
    pub bt_nearby: Vec<BtNearby>,
    /// `BlueZ` is looking for devices now.
    pub bt_scanning: bool,
    /// The passphrase prompt for a protected network; `None` except
    /// while the user is typing into it.
    pub wifi_password: Option<crate::WifiPasswordPrompt>,
    /// Mirrored from `powerprofilesctl` when the panel opens and
    /// pushed back via `set_power_profile` on cycle. Defaults to
    /// `Balanced` so a host without `power-profiles-daemon` still
    /// renders a sensible label.
    pub power_profile: PowerProfile,
    /// The profiles the power daemon offers on this machine; empty until
    /// read. The power row cycles only through these.
    pub power_profiles: Vec<PowerProfile>,
    /// Apps currently keeping the screen awake (an idle inhibitor held
    /// through Wayland, the portal or `org.freedesktop.ScreenSaver`), named
    /// as they asked. Shown on the power row so a screen that never locks
    /// has a visible reason.
    pub awake_holders: Vec<String>,
    /// Certified personal devices known to the device fabric, read when the
    /// popover opens. `None` when this session has no device fabric.
    pub personal_devices: Option<usize>,
    /// Mirror of the persisted Do Not Disturb switch for the tile.
    pub do_not_disturb: bool,
    /// The clipboard history the Clipboard row counts and its section
    /// lists, from one snapshot.
    pub clipboard: crate::ClipboardView,
    /// Notifications in the history, read when the popover opens.
    pub notification_count: usize,
    /// This machine's sound outputs, as the audio service lists them.
    pub sound_outputs: Vec<crate::SoundOutput>,
    /// The microphone sound records from, as the audio service reads it:
    /// the row it gets shows its level and answers its mute with this.
    pub microphone: Option<crate::SoundInput>,
    /// Every application's stream, as the audio service reads it: each
    /// row shows where it goes, its level and its mute.
    pub sound_streams: Vec<crate::SoundStream>,
    /// The Devices section's rows, in order. The overlay host reads the
    /// device fabric for them; the root page lays them out and paints them.
    pub device_rows: Vec<crate::hosted::HostedRow>,
    /// Which controls the panel shows, in the person's order.
    pub arrangement: settings_store::hardware::QuickSettingsConfig,
    /// The notifications kept since they arrived.
    pub notifications: crate::NotificationsView,
    /// The person's devices a Bluetooth accessory can move to, read while
    /// an accessory's page shows; `None` without a device fabric.
    pub handoff_targets: Option<Vec<crate::HandoffTarget>>,
    /// Which of the device's controls the desktop found it has: an effect
    /// working one it lacks is published unavailable.
    pub available: crate::actions::QsActionAvailability,
    /// What plays, while a player is playing or paused.
    pub now_playing: Option<crate::NowPlaying>,
}

impl Default for QuickSettingsState {
    fn default() -> Self {
        Self {
            brightness: 0.6,
            brightness_available: false,
            displays: Vec::new(),
            focused_display: None,
            volume_available: true,
            wifi: true,
            bluetooth: false,
            bluetooth_available: false,
            wired: None,
            status: crate::status::SystemStatus::default(),
            firmware_updates: None,
            radios: os_actions::RadioState::default(),
            vpns: Vec::new(),
            privacy: os_actions::get_privacy_status(),
            night_light: settings_store::hardware::NIGHT_LIGHT.get(),
            rotation_available: os_actions::has_accelerometer(),
            rotation_lock: settings_store::hardware::ROTATION_LOCK.get(),
            nav_stack: vec![QsLevel::Root],
            section: None,
            paste_origin: None,
            // Populated by nmcli/bluetoothctl on the popover's rising
            // edge — no fake content.
            wifi_networks: Vec::new(),
            saved_networks: Vec::new(),
            wifi_hidden: None,
            wifi_scanning: false,
            radio_change_pending: false,
            radio_change: None,
            wifi_problem: None,
            wifi_scan_error: None,
            wifi_joining: None,
            bt_devices: Vec::new(),
            bt_nearby: Vec::new(),
            bt_scanning: false,
            wifi_password: None,
            power_profile: PowerProfile::Balanced,
            power_profiles: Vec::new(),
            awake_holders: Vec::new(),
            personal_devices: None,
            do_not_disturb: false,
            clipboard: crate::ClipboardView::default(),
            notification_count: 0,
            sound_outputs: Vec::new(),
            microphone: None,
            sound_streams: Vec::new(),
            device_rows: Vec::new(),
            arrangement: settings_store::hardware::QuickSettingsConfig::default(),
            notifications: crate::NotificationsView::default(),
            handoff_targets: None,
            available: crate::actions::QsActionAvailability::default(),
            now_playing: None,
        }
    }
}

impl QuickSettingsState {
    /// Whether anything on this device can set the brightness: a backlight
    /// the desktop found, or a brightness it read.
    #[must_use]
    pub fn can_set_brightness(&self) -> bool {
        self.available.brightness || self.brightness_available
    }

    /// A panel with stand-in readings and no device reads, for shots and
    /// tests.
    #[must_use]
    pub fn fixture() -> Self {
        Self {
            brightness: 0.6,
            brightness_available: true,
            displays: Vec::new(),
            focused_display: None,
            volume_available: true,
            wifi: true,
            bluetooth: false,
            bluetooth_available: false,
            wired: None,
            status: crate::status::SystemStatus {
                volume: Some(media::audio::default_node::VolumeState {
                    level: 0.4,
                    muted: false,
                }),
                ..crate::status::SystemStatus::default()
            },
            firmware_updates: None,
            radios: os_actions::RadioState::default(),
            vpns: Vec::new(),
            privacy: os_actions::PrivacyStatus::default(),
            night_light: false,
            rotation_available: false,
            rotation_lock: false,
            nav_stack: vec![QsLevel::Root],
            section: None,
            paste_origin: None,
            // Populated by nmcli/bluetoothctl on the popover's rising
            // edge — no fake content.
            wifi_networks: Vec::new(),
            saved_networks: Vec::new(),
            wifi_hidden: None,
            wifi_scanning: false,
            radio_change_pending: false,
            radio_change: None,
            wifi_problem: None,
            wifi_scan_error: None,
            wifi_joining: None,
            bt_devices: Vec::new(),
            bt_nearby: Vec::new(),
            bt_scanning: false,
            wifi_password: None,
            power_profile: PowerProfile::Balanced,
            power_profiles: Vec::new(),
            awake_holders: Vec::new(),
            personal_devices: None,
            do_not_disturb: false,
            clipboard: crate::ClipboardView::default(),
            notification_count: 0,
            sound_outputs: Vec::new(),
            microphone: None,
            sound_streams: Vec::new(),
            device_rows: Vec::new(),
            arrangement: settings_store::hardware::QuickSettingsConfig::default(),
            notifications: crate::NotificationsView::default(),
            handoff_targets: None,
            available: crate::actions::QsActionAvailability::all(),
            now_playing: None,
        }
    }

    /// The panel before anything of the device is read: no levels, no
    /// radios, no controls found yet. A device host starts here and lands
    /// its readings over it.
    #[must_use]
    pub fn unread() -> Self {
        Self {
            brightness_available: false,
            volume_available: false,
            status: crate::status::SystemStatus::default(),
            available: crate::actions::QsActionAvailability::default(),
            ..Self::fixture()
        }
    }

    /// The renderer's dispatch key. Defaults to `Root` if the stack is
    /// somehow empty (`new()` always seeds it).
    pub fn current_level(&self) -> &QsLevel {
        self.nav_stack.last().unwrap_or(&QsLevel::Root)
    }

    pub fn push_level(&mut self, level: QsLevel) {
        self.nav_stack.push(level);
    }

    /// Replace stale drill-in history with one authoritative destination.
    pub fn show_level(&mut self, level: QsLevel) {
        self.paste_origin = None;
        self.nav_stack.clear();
        self.nav_stack.push(QsLevel::Root);
        if level != QsLevel::Root {
            self.nav_stack.push(level);
        }
    }

    /// Clipboard history opened to paste (Super+V): the Clipboard section
    /// alone, at the caret, and a choice goes into the field.
    #[must_use]
    pub fn pasting(&self) -> bool {
        self.paste_origin.is_some()
            && self.section == Some(crate::QsSection::Clipboard)
            && self.current_level() == &crate::QsLevel::Root
    }

    /// Open `section` in place on the root page, closing any other: one
    /// section is open at a time.
    pub fn open_section(&mut self, section: crate::QsSection) {
        self.section = Some(section);
    }

    /// Open `section`, or close it when it is the one open.
    pub fn toggle_section(&mut self, section: crate::QsSection) {
        self.section = (self.section != Some(section)).then_some(section);
    }

    /// No-op when only `Root` remains so the popover can't be emptied.
    pub fn pop_level(&mut self) {
        if self.nav_stack.len() > 1 {
            self.nav_stack.pop();
        }
    }
}
