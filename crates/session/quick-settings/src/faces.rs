//! What each part of the panel shows, read from the device's readings as
//! plain values. The view builds its widgets from these and nothing else,
//! and each part is built again only when its face changes
//! ([`crate::host::Readings::project`]).

use settings_store::hardware::{QuickSettingsConfig, QuickSettingsControl};
use ui::icons::Icon;

use crate::effect::QsEffect;
use crate::i18n::{t, t_args};
use crate::{PowerProfile, QsSection, QuickSettingsState, WifiNetwork};

/// What a slider sets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SliderKind {
    /// The one brightness of a machine whose displays are not listed one by
    /// one.
    Brightness,
    /// One display's, by connector.
    Display(String),
    Volume,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SliderFace {
    pub id: String,
    pub kind: SliderKind,
    /// The accessible name.
    pub name: String,
    /// The display's name over its slider, when there are several.
    pub caption: Option<String>,
    pub level: f64,
    pub available: bool,
}

/// The slider block at the top of the page.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Sliders {
    pub rows: Vec<SliderFace>,
    /// Sound is muted: the speaker shows it and the track is empty.
    pub muted: bool,
    /// There are outputs to choose from: the volume slider has a chevron.
    pub outputs: bool,
}

impl Sliders {
    pub(crate) fn of(state: &QuickSettingsState) -> Self {
        let mut sliders = Vec::new();
        let several = state.displays.len() > 1;
        // A machine with nothing to dim (no backlight, no monitor whose
        // brightness can be set) shows no brightness slider, not a dead one.
        if state.displays.is_empty() && state.can_set_brightness() {
            sliders.push(SliderFace {
                id: "qs-brightness".into(),
                kind: SliderKind::Brightness,
                name: t("qs-brightness"),
                caption: None,
                level: f64::from(state.brightness),
                available: true,
            });
        }
        for display in &state.displays {
            sliders.push(SliderFace {
                id: format!("qs-brightness-{}", display.connector),
                kind: SliderKind::Display(display.connector.clone()),
                name: if several {
                    t_args(
                        "qs-brightness-display",
                        &[("display", display.name.as_str().into())],
                    )
                } else {
                    t("qs-brightness")
                },
                caption: several.then(|| display.name.clone()),
                level: f64::from(display.level),
                available: true,
            });
        }
        let muted = state.volume_muted();
        sliders.push(SliderFace {
            id: "qs-volume".into(),
            kind: SliderKind::Volume,
            name: t("qs-volume"),
            caption: None,
            // A muted output is silent at any level: its track is empty, as
            // the volume readout's is.
            level: if muted {
                0.0
            } else {
                f64::from(state.volume_level())
            },
            available: state.volume_available && state.status.volume.is_some(),
        });
        Self {
            rows: sliders,
            muted,
            outputs: state.volume_available && !state.sound_outputs.is_empty(),
        }
    }
}

/// One tile of the grid.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TileFace {
    pub control: QuickSettingsControl,
    pub icon: Icon,
    pub label: String,
    pub detail: String,
    pub on: bool,
    /// What a press on the tile does; `None` while it can do nothing (a
    /// radio change still under way).
    pub switch: Option<QsEffect>,
    /// The section its chevron opens.
    pub section: Option<QsSection>,
}

/// The tiles this machine shows, in the person's order.
pub(crate) fn tiles(state: &QuickSettingsState) -> Vec<TileFace> {
    visible_controls(&state.arrangement, state)
        .into_iter()
        .map(|control| TileFace {
            control,
            icon: tile_icon(control, state),
            label: tile_label(control, state),
            detail: subline(control, state),
            on: tile_on(control, state),
            switch: tile_switch(control, state),
            section: section_of(control, state),
        })
        .collect()
}

/// The controls of `arrangement` this machine has what they control for.
pub fn visible_controls(
    arrangement: &QuickSettingsConfig,
    state: &QuickSettingsState,
) -> Vec<QuickSettingsControl> {
    arrangement
        .controls
        .iter()
        .copied()
        .filter(|control| available(*control, state))
        .collect()
}

/// Whether this machine has what `control` controls. Powered off and
/// unavailable are different: an absent adapter shows no tile, a radio
/// that is off shows one that is off.
pub(crate) fn available(control: QuickSettingsControl, state: &QuickSettingsState) -> bool {
    match control {
        // `Wifi` is the durable identifier of the required recovery slot.
        // It renders as Wi-Fi when a radio exists and as a wired-network
        // affordance on Ethernet-only machines; no network hardware means
        // no inert tile.
        QuickSettingsControl::Wifi => {
            state.radios.wifi_available
                || state
                    .wired
                    .as_ref()
                    .is_some_and(|wired| wired.device.is_some())
        }
        QuickSettingsControl::Bluetooth => state.bluetooth_available,
        QuickSettingsControl::NightLight | QuickSettingsControl::DoNotDisturb => true,
        QuickSettingsControl::AirplaneMode => {
            state.radios.wifi_available || state.radios.wwan_available
        }
        QuickSettingsControl::Vpn => !state.vpns.is_empty(),
        QuickSettingsControl::MobileData => state.radios.wwan_available,
        QuickSettingsControl::RotationLock => state.rotation_available,
        QuickSettingsControl::PowerMode => state.power_profiles.contains(&PowerProfile::Saver),
    }
}

/// The section a tile's chevron opens, if it has one.
pub(crate) fn section_of(
    control: QuickSettingsControl,
    state: &QuickSettingsState,
) -> Option<QsSection> {
    match control {
        QuickSettingsControl::Wifi if state.radios.wifi_available => Some(QsSection::Wifi),
        QuickSettingsControl::Bluetooth => Some(QsSection::Bluetooth),
        QuickSettingsControl::Vpn => Some(QsSection::Vpn),
        // The one place the power mode is chosen.
        QuickSettingsControl::PowerMode => Some(QsSection::PowerMode),
        _ => None,
    }
}

/// A control's name, as the arrangement editor lists it.
pub(crate) fn control_label(control: QuickSettingsControl) -> String {
    t(match control {
        QuickSettingsControl::Wifi => "qs-network",
        QuickSettingsControl::Bluetooth => "qs-bluetooth",
        QuickSettingsControl::Vpn => "qs-vpn",
        QuickSettingsControl::NightLight => "qs-night-light",
        QuickSettingsControl::AirplaneMode => "qs-airplane-mode",
        QuickSettingsControl::MobileData => "qs-mobile-data",
        QuickSettingsControl::DoNotDisturb => "qs-do-not-disturb",
        QuickSettingsControl::RotationLock => "qs-rotation-lock",
        QuickSettingsControl::PowerMode => "qs-power-mode",
    })
}

/// A control's stable name in effects and test ids.
pub(crate) fn control_id(control: QuickSettingsControl) -> &'static str {
    match control {
        QuickSettingsControl::Wifi => "wifi",
        QuickSettingsControl::Bluetooth => "bluetooth",
        QuickSettingsControl::Vpn => "vpn",
        QuickSettingsControl::NightLight => "night_light",
        QuickSettingsControl::AirplaneMode => "airplane_mode",
        QuickSettingsControl::MobileData => "mobile_data",
        QuickSettingsControl::DoNotDisturb => "do_not_disturb",
        QuickSettingsControl::RotationLock => "rotation_lock",
        QuickSettingsControl::PowerMode => "power_mode",
    }
}

/// The control a stable name names.
pub(crate) fn control_named(name: &str) -> Option<QuickSettingsControl> {
    QuickSettingsControl::ALL
        .into_iter()
        .find(|control| control_id(*control) == name)
}

fn tile_label(control: QuickSettingsControl, state: &QuickSettingsState) -> String {
    match control {
        QuickSettingsControl::Wifi if !state.radios.wifi_available => t("qs-wired-network"),
        QuickSettingsControl::Wifi => t("qs-wifi"),
        _ => control_label(control),
    }
}

pub(crate) fn tile_icon(control: QuickSettingsControl, state: &QuickSettingsState) -> Icon {
    match control {
        QuickSettingsControl::Wifi if !state.radios.wifi_available => Icon::EthernetPort,
        QuickSettingsControl::Wifi if !state.wifi => Icon::WifiOff,
        QuickSettingsControl::Wifi => Icon::Wifi,
        QuickSettingsControl::Bluetooth => Icon::Bluetooth,
        QuickSettingsControl::Vpn => Icon::Shield,
        QuickSettingsControl::NightLight => Icon::Moon,
        QuickSettingsControl::AirplaneMode => Icon::Plane,
        QuickSettingsControl::MobileData => Icon::Globe,
        QuickSettingsControl::DoNotDisturb => Icon::BellOff,
        QuickSettingsControl::RotationLock => Icon::RotateCw,
        QuickSettingsControl::PowerMode => Icon::Gauge,
    }
}

fn tile_on(control: QuickSettingsControl, state: &QuickSettingsState) -> bool {
    match control {
        QuickSettingsControl::Wifi if !state.radios.wifi_available => {
            state.wired.as_ref().is_some_and(|wired| wired.connected)
        }
        QuickSettingsControl::Wifi => state.wifi,
        QuickSettingsControl::Bluetooth => state.bluetooth,
        QuickSettingsControl::Vpn => state.vpns.iter().any(|vpn| vpn.active),
        QuickSettingsControl::NightLight => state.night_light,
        QuickSettingsControl::AirplaneMode => state.radios.airplane_mode,
        QuickSettingsControl::MobileData => state.radios.wwan_enabled,
        QuickSettingsControl::DoNotDisturb => state.do_not_disturb,
        QuickSettingsControl::RotationLock => state.rotation_lock,
        QuickSettingsControl::PowerMode => state.power_profile == PowerProfile::Saver,
    }
}

/// What a press on `control`'s tile does.
fn tile_switch(control: QuickSettingsControl, state: &QuickSettingsState) -> Option<QsEffect> {
    let on = tile_on(control, state);
    if state.radio_change_pending
        && matches!(
            control,
            QuickSettingsControl::Wifi | QuickSettingsControl::AirplaneMode
        )
    {
        return None;
    }
    Some(match control {
        QuickSettingsControl::Wifi if !state.radios.wifi_available => {
            QsEffect::SetWired { on: !on }
        }
        QuickSettingsControl::Wifi => QsEffect::SetWifi { on: !on },
        QuickSettingsControl::Bluetooth => QsEffect::SetBluetooth { on: !on },
        // On: the active profile goes off. Off: the first comes on; the
        // section lists every profile with its own switch.
        QuickSettingsControl::Vpn => {
            let profile = state
                .vpns
                .iter()
                .find(|profile| profile.active)
                .or_else(|| state.vpns.first())?;
            QsEffect::SetVpn {
                profile: profile.name.clone(),
                on: !profile.active,
            }
        }
        QuickSettingsControl::NightLight => QsEffect::SetNightLight { on: !on },
        QuickSettingsControl::AirplaneMode => QsEffect::SetAirplaneMode { on: !on },
        QuickSettingsControl::MobileData => QsEffect::SetMobileData { on: !on },
        QuickSettingsControl::DoNotDisturb => QsEffect::SetDoNotDisturb { on: !on },
        QuickSettingsControl::RotationLock => QsEffect::SetRotationLock { locked: !on },
        // The saver, or back to balanced from it.
        QuickSettingsControl::PowerMode => {
            let next = if on {
                PowerProfile::Balanced
            } else {
                PowerProfile::Saver
            };
            if !state.power_profiles.contains(&next) {
                return None;
            }
            QsEffect::SetPowerMode { mode: next }
        }
    })
}

/// The network Wi-Fi is on: the observed link when it is Wi-Fi or offline,
/// else (wired, or not observed yet) the last scan's.
fn wifi_subline(state: &QuickSettingsState) -> String {
    match &state.status.link {
        Some(link) if link.kind == os_actions::LinkKind::Wifi => link
            .name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| t("qs-connected")),
        Some(link) if link.kind == os_actions::LinkKind::Offline => t("qs-not-connected"),
        _ => state
            .wifi_networks
            .iter()
            .find(|network| network.connected)
            .map_or_else(|| t("qs-not-connected"), |network| network.ssid.clone()),
    }
}

/// The line under `control`'s tile: its state in words.
#[must_use]
pub fn subline(control: QuickSettingsControl, state: &QuickSettingsState) -> String {
    if state.radio_change_pending
        && matches!(
            control,
            QuickSettingsControl::Wifi | QuickSettingsControl::AirplaneMode
        )
    {
        // The change under way, until the machine answers: never the
        // request shown as if it had happened.
        return match (control, state.radio_change) {
            (QuickSettingsControl::Wifi, Some(os_actions::RadioChange::Wifi(on)))
            | (QuickSettingsControl::AirplaneMode, Some(os_actions::RadioChange::Airplane(on))) => {
                t(if on {
                    "qs-turning-on"
                } else {
                    "qs-turning-off"
                })
            }
            _ => t("display-confirm-applying"),
        };
    }
    let on_off = |on: bool| t(if on { "qs-on" } else { "qs-off" });
    match control {
        QuickSettingsControl::Wifi if !state.radios.wifi_available => state
            .wired
            .as_ref()
            .and_then(|wired| wired.device.as_deref())
            .map_or_else(
                || t("qs-not-connected"),
                |device| {
                    if state.wired.as_ref().is_some_and(|wired| wired.connected) {
                        device.to_owned()
                    } else {
                        t("qs-cable-unplugged")
                    }
                },
            ),
        QuickSettingsControl::Wifi if state.wifi => wifi_subline(state),
        QuickSettingsControl::Wifi
            if state.radios.wifi_hardware_blocked || state.wifi_problem.is_some() =>
        {
            wifi_off_message(state)
        }
        QuickSettingsControl::Bluetooth if state.bluetooth => state
            .bt_devices
            .iter()
            .find(|device| device.connected)
            .map_or_else(|| t("qs-not-connected"), |device| device.name.clone()),
        QuickSettingsControl::Wifi | QuickSettingsControl::Bluetooth => t("qs-off"),
        QuickSettingsControl::Vpn => state
            .vpns
            .iter()
            .find(|vpn| vpn.active)
            .map_or_else(|| t("qs-off"), |vpn| vpn.name.clone()),
        QuickSettingsControl::NightLight => on_off(state.night_light),
        QuickSettingsControl::AirplaneMode => on_off(state.radios.airplane_mode),
        QuickSettingsControl::MobileData => on_off(state.radios.wwan_enabled),
        QuickSettingsControl::DoNotDisturb => t(if state.do_not_disturb {
            "qs-do-not-disturb-on"
        } else {
            "qs-off"
        }),
        QuickSettingsControl::RotationLock => t(if state.rotation_lock {
            "qs-rotation-locked"
        } else {
            "qs-rotation-unlocked"
        }),
        QuickSettingsControl::PowerMode => state.power_profile.label(),
    }
}

/// What the Wi-Fi tile and section say about a radio that is off.
pub(crate) fn wifi_off_message(state: &QuickSettingsState) -> String {
    if state.radios.wifi_hardware_blocked {
        crate::radio::WifiProblem::HardwareSwitch.reason()
    } else if let Some(problem) = state.wifi_problem {
        problem.reason()
    } else {
        t("qs-wifi-off")
    }
}

/// A row under the tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootRow {
    /// The battery (or, without one, the power mode): opens its section.
    Battery,
    Devices,
    Clipboard,
    Notifications,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowFace {
    pub row: RootRow,
    pub icon: Icon,
    pub title: String,
    /// The second line (the battery's), if any.
    pub detail: Option<String>,
    /// The value on the trailing side.
    pub value: Option<String>,
}

impl RowFace {
    /// The section the row opens in place; `None` for Notifications,
    /// which opens its page.
    pub(crate) fn section(&self) -> Option<QsSection> {
        match self.row {
            RootRow::Battery => Some(QsSection::Battery),
            RootRow::Devices => Some(QsSection::Devices),
            RootRow::Clipboard => Some(QsSection::Clipboard),
            RootRow::Notifications => None,
        }
    }

    pub(crate) fn id(&self) -> &'static str {
        match self.row {
            RootRow::Battery => "qs-row-battery",
            RootRow::Devices => "qs-row-devices",
            RootRow::Clipboard => "qs-row-clipboard",
            RootRow::Notifications => "qs-row-notifications",
        }
    }
}

/// The rows under the tiles this machine shows.
pub(crate) fn rows(state: &QuickSettingsState) -> Vec<RowFace> {
    let mut rows = Vec::new();
    let battery = state.battery();
    if battery.present || !state.power_profiles.is_empty() || !state.awake_holders.is_empty() {
        let present = battery.present;
        rows.push(RowFace {
            row: RootRow::Battery,
            icon: match battery.state {
                tilo_power_mgmt::BatteryState::Charging if present => Icon::BatteryCharging,
                _ if present => Icon::Battery,
                _ => Icon::Gauge,
            },
            title: t(if present { "qs-battery" } else { "qs-power" }),
            detail: Some(if present {
                battery_time_text(state).unwrap_or_else(|| battery_state_text(state))
            } else {
                state.power_profile.label()
            }),
            value: present.then(|| format!("{}%", battery.percent)),
        });
    }
    rows.push(RowFace {
        row: RootRow::Devices,
        icon: Icon::Monitor,
        title: t("qs-devices"),
        detail: None,
        // No personal devices known: nothing to count, so nothing said.
        value: state
            .personal_devices
            .map(|count| t_args("qs-devices-count", &[("count", count.into())])),
    });
    rows.push(RowFace {
        row: RootRow::Clipboard,
        icon: Icon::ClipboardPaste,
        title: t("qs-clipboard"),
        detail: None,
        value: Some(match state.clipboard.total {
            0 => t("qs-clipboard-empty"),
            count => t_args("qs-clipboard-count", &[("count", count.into())]),
        }),
    });
    rows.push(RowFace {
        row: RootRow::Notifications,
        icon: Icon::Bell,
        title: t("qs-notifications"),
        detail: None,
        value: Some(match state.notifications.rows.len() {
            0 => t("qs-notifications-none"),
            count => t_args("qs-notifications-count", &[("count", count.into())]),
        }),
    });
    rows
}

/// The battery's charge state, in words.
pub(crate) fn battery_state_text(state: &QuickSettingsState) -> String {
    use tilo_power_mgmt::BatteryState;
    if !state.battery().present {
        return t("qs-power-ac");
    }
    t(match state.battery().state {
        BatteryState::Charging => "qs-battery-charging",
        BatteryState::Full => "qs-battery-full",
        BatteryState::PendingCharge => "qs-battery-not-charging",
        _ => "qs-battery-on-battery",
    })
}

/// How long until empty or full, in words, when the battery says.
pub(crate) fn battery_time_text(state: &QuickSettingsState) -> Option<String> {
    use tilo_power_mgmt::BatteryTime;
    if !state.battery().present {
        return None;
    }
    let (key, minutes) = match state.status.battery_time? {
        BatteryTime::ToEmpty(minutes) => ("qs-battery-time-left", minutes),
        BatteryTime::ToFull(minutes) => ("qs-battery-time-to-full", minutes),
    };
    if minutes == 0 {
        return None;
    }
    Some(t_args(
        key,
        &[
            ("hours", (minutes / 60).into()),
            ("minutes", (minutes % 60).into()),
        ],
    ))
}

/// The apps keeping the machine awake, as one sentence, or `None`.
pub(crate) fn awake_sentence(state: &QuickSettingsState) -> Option<String> {
    match state.awake_holders.as_slice() {
        [] => None,
        [app, rest @ ..] if rest.iter().all(|other| other == app) => {
            Some(t_args("qs-awake-app", &[("app", app.as_str().into())]))
        }
        many => Some(t_args("qs-awake-apps", &[("count", many.len().into())])),
    }
}

/// The Wi-Fi networks in the order the section lists them: the one in use,
/// then the ones this machine knows, then by signal, then by name.
pub(crate) fn ordered_networks(networks: &[WifiNetwork]) -> Vec<&WifiNetwork> {
    let mut order: Vec<&WifiNetwork> = networks.iter().collect();
    order.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.known.cmp(&a.known))
            .then(b.signal.cmp(&a.signal))
            .then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase()))
    });
    order
}

/// The power modes the daemon offers, in their natural order.
pub(crate) fn offered_modes(state: &QuickSettingsState) -> Vec<PowerProfile> {
    [
        PowerProfile::Saver,
        PowerProfile::Balanced,
        PowerProfile::Performance,
    ]
    .into_iter()
    .filter(|mode| state.power_profiles.contains(mode))
    .collect()
}

/// A network as the Wi-Fi section lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NetworkFace {
    pub ssid: String,
    pub signal: u8,
    pub secure: bool,
    pub connected: bool,
    /// "Connecting…", "Connected" or "Saved".
    pub value: Option<String>,
    /// What a press does; `None` for the network in use, or while another
    /// is being joined.
    pub press: Option<QsEffect>,
    /// The password prompt opens under it.
    pub asking: Option<crate::WifiPasswordPrompt>,
}

/// One item of an open section.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    /// A sentence standing for an empty or unavailable list.
    Message(String),
    Network(NetworkFace),
    Accessory(crate::BtDevice),
    /// "Look for devices", or "Stop looking" while the panel is looking.
    LookForDevices {
        looking: bool,
    },
    /// A Bluetooth device in reach, which a press pairs.
    Nearby(crate::BtNearby),
    Profile {
        name: String,
        active: bool,
    },
    Battery {
        percent: u8,
        state: String,
        time: Option<String>,
    },
    Modes {
        offered: Vec<PowerProfile>,
        current: PowerProfile,
    },
    /// The Battery section's row naming the power mode, leading to where
    /// it is chosen: the Power mode tile's section, or the power settings
    /// when that tile is not on the page.
    PowerModeLink {
        mode: String,
        tile: bool,
    },
    Awake(String),
    Output(crate::SoundOutput),
    /// A Devices row, which the desktop describes.
    Device(crate::hosted::HostedRow),
    /// The clipboard's copies, which the section filters as the person
    /// types.
    Copies(Vec<crate::hosted::HostedRow>),
    Settings {
        label: String,
        link: &'static str,
    },
}

/// What `section` holds.
pub(crate) fn section(section: QsSection, state: &QuickSettingsState) -> Vec<Item> {
    match section {
        QsSection::Wifi => wifi(state),
        QsSection::Bluetooth => bluetooth(state),
        QsSection::Vpn => vpn(state),
        QsSection::PowerMode => power_mode(state),
        QsSection::Battery => battery(state),
        QsSection::Sound => sound(state),
        QsSection::Devices => devices(state),
        QsSection::Clipboard => vec![Item::Copies(state.clipboard.rows.clone())],
    }
}

fn wifi(state: &QuickSettingsState) -> Vec<Item> {
    let mut items = Vec::new();
    if state.radio_change_pending {
        let on = matches!(
            state.radio_change,
            Some(os_actions::RadioChange::Wifi(true) | os_actions::RadioChange::Airplane(false))
        );
        items.push(Item::Message(t(if on {
            "qs-turning-on"
        } else {
            "qs-turning-off"
        })));
    } else if !state.wifi {
        // The tile the section opens under turns the radio on.
        items.push(Item::Message(wifi_off_message(state)));
    } else {
        for network in ordered_networks(&state.wifi_networks) {
            let joining = state.wifi_joining.as_deref() == Some(network.ssid.as_str());
            let value = if joining {
                Some(t("qs-connecting"))
            } else if network.connected {
                Some(t("qs-connected"))
            } else if network.known {
                Some(t("qs-wifi-known"))
            } else {
                None
            };
            let press = (!network.connected && state.wifi_joining.is_none()).then(|| {
                if network.secure && !network.known {
                    QsEffect::AskWifiPassword {
                        network: network.ssid.clone(),
                    }
                } else {
                    QsEffect::JoinWifi {
                        network: network.ssid.clone(),
                    }
                }
            });
            items.push(Item::Network(NetworkFace {
                ssid: network.ssid.clone(),
                signal: network.signal,
                secure: network.secure,
                connected: network.connected,
                value,
                press,
                asking: state
                    .wifi_password
                    .clone()
                    .filter(|prompt| prompt.ssid == network.ssid),
            }));
        }
        if state.wifi_networks.is_empty() {
            items.push(Item::Message(t(if state.wifi_scanning {
                "qs-wifi-scanning"
            } else if state.wifi_scan_error.is_some() {
                "qs-wifi-scan-failed"
            } else {
                "qs-wifi-none"
            })));
        }
    }
    items.push(Item::Settings {
        label: t("qs-wifi-settings"),
        link: "settings://network",
    });
    items
}

fn bluetooth(state: &QuickSettingsState) -> Vec<Item> {
    let mut items = Vec::new();
    if state.bluetooth {
        // Connected first, then by name: what is in use is what a person
        // came to change.
        let mut devices = state.bt_devices.clone();
        devices.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        if devices.is_empty() && state.bt_nearby.is_empty() {
            items.push(Item::Message(t("qs-bluetooth-empty")));
        }
        items.extend(devices.into_iter().map(Item::Accessory));
        items.push(Item::LookForDevices {
            looking: state.bt_scanning,
        });
        items.extend(state.bt_nearby.iter().cloned().map(Item::Nearby));
    } else {
        items.push(Item::Message(t("qs-bluetooth-off")));
    }
    items.push(Item::Settings {
        label: t("qs-bluetooth-pair"),
        link: "settings://bluetooth",
    });
    items
}

fn vpn(state: &QuickSettingsState) -> Vec<Item> {
    let mut items: Vec<Item> = state
        .vpns
        .iter()
        .map(|profile| Item::Profile {
            name: profile.name.clone(),
            active: profile.active,
        })
        .collect();
    if items.is_empty() {
        items.push(Item::Message(t("qs-vpn-empty")));
    }
    items.push(Item::Settings {
        label: t("qs-vpn-settings"),
        link: "settings://network",
    });
    items
}

fn battery(state: &QuickSettingsState) -> Vec<Item> {
    let mut items = Vec::new();
    if state.battery().present {
        items.push(Item::Battery {
            percent: state.battery().percent.min(100),
            state: battery_state_text(state),
            time: battery_time_text(state),
        });
    }
    // The mode is chosen in one place, the Power mode tile's section; here
    // it is named, and the row leads there.
    if !offered_modes(state).is_empty() {
        items.push(Item::PowerModeLink {
            mode: state.power_profile.label(),
            tile: visible_controls(&state.arrangement, state)
                .contains(&QuickSettingsControl::PowerMode),
        });
    }
    if let Some(awake) = awake_sentence(state) {
        items.push(Item::Awake(awake));
    }
    items.push(Item::Settings {
        label: t(if state.battery().present {
            "qs-battery-settings"
        } else {
            "qs-power-settings"
        }),
        link: "settings://power",
    });
    items
}

fn power_mode(state: &QuickSettingsState) -> Vec<Item> {
    let offered = offered_modes(state);
    let mut items = Vec::new();
    if offered.is_empty() {
        items.push(Item::Message(t("qs-power-mode-unavailable")));
    } else {
        items.push(Item::Modes {
            offered,
            current: state.power_profile,
        });
    }
    items.push(Item::Settings {
        label: t("qs-power-settings"),
        link: "settings://power",
    });
    items
}

fn sound(state: &QuickSettingsState) -> Vec<Item> {
    let mut items: Vec<Item> = state
        .sound_outputs
        .iter()
        .cloned()
        .map(Item::Output)
        .collect();
    items.push(Item::Settings {
        label: t("qs-sound-settings"),
        link: "settings://sound",
    });
    items
}

fn devices(state: &QuickSettingsState) -> Vec<Item> {
    let mut items = Vec::new();
    if state.personal_devices.is_none() && state.device_rows.is_empty() {
        items.push(Item::Message(t("qs-devices-no-identity")));
    } else if state.device_rows.is_empty() {
        items.push(Item::Message(t("qs-devices-empty")));
    }
    items.extend(state.device_rows.iter().cloned().map(Item::Device));
    items.push(Item::Settings {
        label: t("qs-devices-settings"),
        link: "settings://devices_sync",
    });
    items
}

/// The copies matching `query` (every copy for an empty one): its words
/// in the copy's text or where it came from, in any case.
pub(crate) fn matching<'a>(
    copies: &'a [crate::hosted::HostedRow],
    query: &str,
) -> Vec<&'a crate::hosted::HostedRow> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    copies
        .iter()
        .filter(|copy| {
            let haystack = format!("{} {}", copy.title, copy.detail).to_lowercase();
            words.iter().all(|word| haystack.contains(word))
        })
        .collect()
}

/// The line over the clipboard's copies: what was searched for and how many
/// match, or how many there are.
pub(crate) fn copies_caption(query: &str, matches: usize) -> String {
    let query = query.trim();
    if !query.is_empty() && matches == 0 {
        t_args("qs-clipboard-no-matches", &[("query", query.into())])
    } else if !query.is_empty() {
        t_args(
            "qs-clipboard-search-matches",
            &[("query", query.into()), ("count", matches.into())],
        )
    } else if matches == 0 {
        String::new()
    } else {
        t_args("qs-clipboard-search-count", &[("count", matches.into())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(ssid: &str, signal: u8, connected: bool, known: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            signal,
            secure: true,
            security: os_actions::WifiSecurity::Wpa2,
            connected,
            known,
        }
    }

    #[test]
    fn no_brightness_slider_without_anything_to_dim() {
        let mut state = QuickSettingsState::fixture();
        state.brightness_available = false;
        state.available.brightness = false;
        let ids: Vec<String> = Sliders::of(&state)
            .rows
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, ["qs-volume"]);
        state.available.brightness = true;
        let ids: Vec<String> = Sliders::of(&state)
            .rows
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, ["qs-brightness", "qs-volume"]);
    }

    #[test]
    fn networks_list_the_one_in_use_then_known_ones_then_by_signal() {
        let networks = vec![
            network("Strong stranger", 4, false, false),
            network("Weak known", 1, false, true),
            network("Home", 3, true, true),
            network("another", 2, false, false),
            network("Beta", 2, false, false),
        ];
        let order: Vec<&str> = ordered_networks(&networks)
            .into_iter()
            .map(|network| network.ssid.as_str())
            .collect();
        assert_eq!(
            order,
            ["Home", "Weak known", "Strong stranger", "another", "Beta"]
        );
    }

    #[test]
    fn a_protected_network_never_seen_asks_for_its_password_and_a_known_one_joins() {
        let mut state = QuickSettingsState::fixture();
        state.radios.wifi_available = true;
        state.wifi_networks = vec![
            network("Home", 3, false, true),
            network("Cafe", 2, false, false),
        ];
        let presses: Vec<Option<QsEffect>> = wifi(&state)
            .into_iter()
            .filter_map(|item| match item {
                Item::Network(face) => Some(face.press),
                _ => None,
            })
            .collect();
        assert_eq!(
            presses,
            [
                Some(QsEffect::JoinWifi {
                    network: "Home".into()
                }),
                Some(QsEffect::AskWifiPassword {
                    network: "Cafe".into()
                }),
            ]
        );
    }

    #[test]
    fn a_radio_change_under_way_says_so_and_takes_no_second_press() {
        let mut state = QuickSettingsState::fixture();
        state.radios.wifi_available = true;
        state.radio_change_pending = true;
        state.radio_change = Some(os_actions::RadioChange::Wifi(true));
        assert_eq!(subline(QuickSettingsControl::Wifi, &state), "Turning on…");
        let wifi = tiles(&state)
            .into_iter()
            .find(|tile| tile.control == QuickSettingsControl::Wifi)
            .expect("the Wi-Fi tile");
        assert_eq!(wifi.switch, None);
    }

    #[test]
    fn the_power_mode_tile_switches_between_the_saver_and_balanced() {
        let mut state = QuickSettingsState::fixture();
        state.power_profiles = vec![PowerProfile::Saver, PowerProfile::Balanced];
        let tile = |state: &QuickSettingsState| {
            tiles(state)
                .into_iter()
                .find(|tile| tile.control == QuickSettingsControl::PowerMode)
                .and_then(|tile| tile.switch)
        };
        assert_eq!(
            tile(&state),
            Some(QsEffect::SetPowerMode {
                mode: PowerProfile::Saver
            })
        );
        state.power_profile = PowerProfile::Saver;
        assert_eq!(
            tile(&state),
            Some(QsEffect::SetPowerMode {
                mode: PowerProfile::Balanced
            })
        );
    }

    #[test]
    fn copies_match_every_word_in_their_text_or_source() {
        let copy = |title: &str, detail: &str| crate::hosted::HostedRow {
            key: title.into(),
            title: title.into(),
            detail: detail.into(),
            icon: Icon::FileText,
            selected: false,
            enabled: true,
            actions: crate::hosted::HostedActions::None,
        };
        let copies = vec![
            copy("Meeting at noon", "Copied on Laptop"),
            copy("Grocery list", "Copied on Phone"),
        ];
        let titles = |query: &str| -> Vec<String> {
            matching(&copies, query)
                .into_iter()
                .map(|copy| copy.title.clone())
                .collect()
        };
        assert_eq!(titles(""), ["Meeting at noon", "Grocery list"]);
        assert_eq!(titles("phone"), ["Grocery list"]);
        assert_eq!(titles("noon laptop"), ["Meeting at noon"]);
        assert!(titles("nothing").is_empty());
        assert_eq!(copies_caption("phone", 1), "One copy matches “phone”");
    }
}
