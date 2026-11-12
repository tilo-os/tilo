//! What each part of the panel shows, read from the device's readings as
//! plain values. The view builds its widgets from these and nothing else,
//! and each part is built again only when its face changes
//! ([`crate::readings::Readings::project`]).

use connectivity::model::{Accessory, LinkKind, Nearby, SavedNetwork, WifiNetwork};
use settings_store::hardware::{QuickSettingsConfig, QuickSettingsControl};
use ui::icons::Icon;
use ui::widget::primitives::Fact;

use crate::effect::QsEffect;
use crate::i18n::{t, t_args};
use crate::{PowerProfile, QsSection, QuickSettingsState, RadioChange};

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
    /// What the slider sets, when it is one of several: the display's name,
    /// or the screen and port the sound plays through, each its own fact.
    pub device: Vec<Fact>,
    /// The level as the person reads it, or that it is muted.
    pub value: String,
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
                device: Vec::new(),
                value: percent(f64::from(state.brightness)),
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
                device: if several {
                    vec![Fact::text(display.name.clone())]
                } else {
                    Vec::new()
                },
                value: percent(f64::from(display.level)),
                level: f64::from(display.level),
                available: true,
            });
        }
        let muted = state.volume_muted();
        let level = f64::from(state.volume_level());
        // A session without an audio server has no output to set: the row
        // stays, saying there is no audio device, and the sound's own
        // service turns it back on when the server appears.
        let available = state.volume_available && state.status.volume.is_some();
        sliders.push(SliderFace {
            id: "qs-volume".into(),
            kind: SliderKind::Volume,
            name: t("qs-volume"),
            device: if available {
                state
                    .sound_outputs
                    .iter()
                    .find(|output| output.current)
                    .map_or_else(Vec::new, |output| crate::sound::output_facts(&output.name))
            } else {
                vec![Fact::text(t("qs-sound-no-device"))]
            },
            value: if !available {
                String::new()
            } else if muted {
                t("qs-muted")
            } else {
                percent(level)
            },
            // A muted output is silent at any level: its track is empty, as
            // the volume readout's is.
            level: if muted { 0.0 } else { level },
            available,
        });
        Self {
            rows: sliders,
            muted,
            outputs: available && !state.sound_outputs.is_empty(),
        }
    }
}

/// A level from 0 to 1 as the person reads it.
fn percent(level: f64) -> String {
    t_args(
        "qs-percent",
        &[("percent", format!("{:.0}", level * 100.0).into())],
    )
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
        .map(|control| tile(control, state))
        .collect()
}

/// `control`'s tile.
fn tile(control: QuickSettingsControl, state: &QuickSettingsState) -> TileFace {
    TileFace {
        control,
        icon: tile_icon(control, state),
        label: tile_label(control, state),
        detail: subline(control, state),
        on: tile_on(control, state),
        switch: tile_switch(control, state),
        section: section_of(control, state),
    }
}

/// The radios whose section's page has their switch in its header, as their tiles say: the
/// page is where the person turns the radio on to see what it reaches, whether or not its tile is
/// on the controls page.
pub(crate) fn section_switches(state: &QuickSettingsState) -> Vec<TileFace> {
    [
        QuickSettingsControl::Wifi,
        QuickSettingsControl::Bluetooth,
        QuickSettingsControl::Vpn,
    ]
    .into_iter()
    .filter(|control| available(*control, state))
    .map(|control| tile(control, state))
    .filter(|face| face.section.is_some())
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
pub fn available(control: QuickSettingsControl, state: &QuickSettingsState) -> bool {
    match control {
        // `Wifi` is the durable identifier of the required recovery slot.
        // It renders as Wi-Fi when a radio exists and as a wired-network
        // affordance on Ethernet-only machines; no network hardware means
        // no inert tile.
        QuickSettingsControl::Wifi => state.wifi_available() || state.wired_available(),
        QuickSettingsControl::Bluetooth => state.bluetooth.available,
        QuickSettingsControl::NightLight | QuickSettingsControl::DoNotDisturb => true,
        QuickSettingsControl::AirplaneMode => {
            state.network.wifi.available || state.network.mobile.available
        }
        QuickSettingsControl::Vpn => !state.vpn.profiles.is_empty(),
        QuickSettingsControl::MobileData => state.network.mobile.available,
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
        // The wired link is listed here too, on a machine without a radio.
        QuickSettingsControl::Wifi if state.wifi_available() || state.wired_available() => {
            Some(QsSection::Wifi)
        }
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
pub fn control_id(control: QuickSettingsControl) -> &'static str {
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
pub fn control_named(name: &str) -> Option<QuickSettingsControl> {
    QuickSettingsControl::ALL
        .into_iter()
        .find(|control| control_id(*control) == name)
}

fn tile_label(control: QuickSettingsControl, state: &QuickSettingsState) -> String {
    match control {
        QuickSettingsControl::Wifi if !state.wifi_available() => t("qs-wired-network"),
        QuickSettingsControl::Wifi => t("qs-wifi"),
        _ => control_label(control),
    }
}

pub(crate) fn tile_icon(control: QuickSettingsControl, state: &QuickSettingsState) -> Icon {
    match control {
        QuickSettingsControl::Wifi if !state.wifi_available() => Icon::EthernetPort,
        QuickSettingsControl::Wifi if !state.network.wifi.on => Icon::WifiOff,
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
        QuickSettingsControl::Wifi if !state.wifi_available() => state
            .network
            .wired
            .as_ref()
            .is_some_and(|wired| wired.connected),
        QuickSettingsControl::Wifi => state.network.wifi.on,
        QuickSettingsControl::Bluetooth => state.bluetooth.on,
        QuickSettingsControl::Vpn => state.vpn.profiles.iter().any(|vpn| vpn.connected),
        QuickSettingsControl::NightLight => state.night_light,
        QuickSettingsControl::AirplaneMode => state.network.airplane_mode,
        QuickSettingsControl::MobileData => state.network.mobile.on,
        QuickSettingsControl::DoNotDisturb => state.do_not_disturb,
        QuickSettingsControl::RotationLock => state.rotation_lock,
        QuickSettingsControl::PowerMode => state.power_profile == PowerProfile::Saver,
    }
}

/// What a press on `control`'s tile does.
fn tile_switch(control: QuickSettingsControl, state: &QuickSettingsState) -> Option<QsEffect> {
    let on = tile_on(control, state);
    if state.radio_change_pending()
        && matches!(
            control,
            QuickSettingsControl::Wifi | QuickSettingsControl::AirplaneMode
        )
    {
        return None;
    }
    Some(match control {
        QuickSettingsControl::Wifi if !state.wifi_available() => QsEffect::SetWired { on: !on },
        QuickSettingsControl::Wifi => QsEffect::SetWifi { on: !on },
        QuickSettingsControl::Bluetooth => QsEffect::SetBluetooth { on: !on },
        // On: the active profile goes off. Off: the first comes on; the
        // section lists every profile with its own switch.
        QuickSettingsControl::Vpn => {
            let profile = state
                .vpn
                .profiles
                .iter()
                .find(|profile| profile.connected)
                .or_else(|| state.vpn.profiles.first())?;
            QsEffect::SetVpn {
                profile: profile.id.clone(),
                on: !profile.connected,
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
/// else (wired, or not observed yet) the one the radio reports joined.
fn wifi_subline(state: &QuickSettingsState) -> String {
    match &state.status.link {
        Some(link) if link.kind == LinkKind::Wifi => link
            .name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| t("qs-connected")),
        Some(link) if link.kind == LinkKind::Offline => t("qs-not-connected"),
        _ => state
            .network
            .wifi
            .connected()
            .map_or_else(|| t("qs-not-connected"), |network| network.name.clone()),
    }
}

/// The line under `control`'s tile: its state in words.
#[must_use]
pub fn subline(control: QuickSettingsControl, state: &QuickSettingsState) -> String {
    if state.radio_change_pending()
        && matches!(
            control,
            QuickSettingsControl::Wifi | QuickSettingsControl::AirplaneMode
        )
    {
        // The change under way, until the machine answers: never the
        // request shown as if it had happened.
        return match (control, state.radio_change()) {
            (QuickSettingsControl::Wifi, Some(RadioChange::Wifi(on)))
            | (QuickSettingsControl::AirplaneMode, Some(RadioChange::Airplane(on))) => t(if on {
                "qs-turning-on"
            } else {
                "qs-turning-off"
            }),
            _ => t("display-confirm-applying"),
        };
    }
    let on_off = |on: bool| t(if on { "qs-on" } else { "qs-off" });
    match control {
        QuickSettingsControl::Wifi if !state.wifi_available() => state
            .network
            .wired
            .as_ref()
            .and_then(|wired| wired.device.as_deref())
            .map_or_else(
                || t("qs-not-connected"),
                |device| {
                    if state
                        .network
                        .wired
                        .as_ref()
                        .is_some_and(|wired| wired.connected)
                    {
                        device.to_owned()
                    } else {
                        t("qs-cable-unplugged")
                    }
                },
            ),
        QuickSettingsControl::Wifi if state.network.wifi.on => wifi_subline(state),
        QuickSettingsControl::Wifi
            if state.network.wifi.hardware_blocked || state.network.problem.is_some() =>
        {
            wifi_off_message(state)
        }
        QuickSettingsControl::Bluetooth if state.bluetooth.on => state
            .bluetooth
            .accessories
            .iter()
            .find(|device| device.connected)
            .map_or_else(|| t("qs-not-connected"), |device| device.name.clone()),
        QuickSettingsControl::Wifi | QuickSettingsControl::Bluetooth => t("qs-off"),
        QuickSettingsControl::Vpn => state
            .vpn
            .profiles
            .iter()
            .find(|vpn| vpn.connected)
            .map_or_else(|| t("qs-off"), |vpn| vpn.name.clone()),
        QuickSettingsControl::NightLight => on_off(state.night_light),
        QuickSettingsControl::AirplaneMode => on_off(state.network.airplane_mode),
        QuickSettingsControl::MobileData => on_off(state.network.mobile.on),
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
    if state.network.wifi.hardware_blocked {
        t("qs-wifi-problem-hardware-switch")
    } else if let Some(problem) = &state.network.problem {
        problem.clone()
    } else {
        t("qs-wifi-off")
    }
}

/// A row under the tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootRow {
    /// The battery (or, without one, what the machine is doing with it):
    /// opens its section.
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
    /// The section whose page the row opens; `None` for Notifications,
    /// which opens the shell's centre.
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
    let present = battery.present;
    // The charge, and what the machine is doing to make it last. The mode
    // it runs in is the power tile above, which is where it is chosen and
    // where all of them are listed.
    if present || !state.awake_holders.is_empty() {
        rows.push(RowFace {
            row: RootRow::Battery,
            icon: match battery.state {
                tilo_power_mgmt::BatteryState::Charging => Icon::BatteryCharging,
                _ if present => Icon::Battery,
                _ => Icon::Gauge,
            },
            title: t(if present { "qs-battery" } else { "qs-power" }),
            detail: Some(if present {
                battery_time_text(state).unwrap_or_else(|| battery_state_text(state))
            } else {
                awake_sentence(state).unwrap_or_else(|| battery_state_text(state))
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
        value: Some(match state.notifications {
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
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
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
    pub name: String,
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
    /// The saved profile this network is, when the machine joins it by
    /// itself.
    pub profile: Option<String>,
}

/// One item of an open section.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    /// A sentence standing for an empty or unavailable list.
    Message(String),
    Network(NetworkFace),
    /// A saved network this machine cannot see now; it can still be
    /// forgotten, or kept from joining by itself.
    Saved(SavedNetwork),
    /// The way into a network that broadcasts no name: the row that asks
    /// for its name and password, or the form itself once it is up.
    Hidden {
        prompt: Option<crate::HiddenNetworkPrompt>,
    },
    /// The wired link, when this machine has an ethernet device.
    Wired {
        connected: bool,
        /// The cable's address while it is up, else the device's name.
        detail: Option<String>,
    },
    Accessory(Accessory),
    /// "Look for devices", or "Stop looking" while the panel is looking.
    LookForDevices {
        looking: bool,
    },
    /// A Bluetooth device in reach, which a press pairs.
    Nearby(Nearby),
    Profile {
        name: String,
        id: String,
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
    /// The microphone that records, with its own level and mute.
    Microphone(crate::SoundInput),
    /// One application's stream, and the outputs it can move to.
    Stream(StreamFace),
    /// A Devices row, as the session publishes it.
    Device(crate::hosted::HostedRow),
    /// The clipboard's copies, which the section filters as the person
    /// types.
    Copies(Vec<crate::hosted::HostedRow>),
    Settings {
        label: String,
        link: &'static str,
    },
}

/// One application's stream as the Sound section lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StreamFace {
    pub stream: crate::SoundStream,
    /// Where it goes now, in words: the output it plays on or the
    /// microphone it records from.
    pub goes: String,
    /// The outputs it can move to, the one it plays on marked: empty for a
    /// stream that records.
    pub outputs: Vec<crate::SoundOutput>,
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
    if let Some(wired) = &state.network.wired {
        items.push(Item::Wired {
            connected: wired.connected,
            detail: if wired.connected {
                wired.ipv4.clone().or_else(|| wired.device.clone())
            } else {
                wired.device.clone()
            },
        });
    }
    if !state.wifi_available() {
        // A machine on a cable: there is no radio to switch.
    } else if let Some(change) = state.radio_change() {
        let on = matches!(
            change,
            RadioChange::Wifi(true) | RadioChange::Airplane(false)
        );
        items.push(Item::Message(t(if on {
            "qs-turning-on"
        } else {
            "qs-turning-off"
        })));
    } else if !state.network.wifi.on {
        // The tile the section opens under turns the radio on.
        items.push(Item::Message(wifi_off_message(state)));
    } else {
        items.extend(wifi_on(state));
    }
    items.push(Item::Settings {
        label: t(if state.wifi_available() {
            "qs-wifi-settings"
        } else {
            "qs-network-settings"
        }),
        link: "settings://network",
    });
    items
}

/// What the section lists while the radio is on: the networks in reach, the
/// saved ones this machine cannot see, and the way to join one whose name is
/// hidden.
fn wifi_on(state: &QuickSettingsState) -> Vec<Item> {
    let mut items = Vec::new();
    for network in ordered_networks(&state.network.wifi.networks) {
        let joining = state.wifi_joining.as_deref() == Some(network.name.as_str());
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
                    network: network.name.clone(),
                }
            } else {
                QsEffect::JoinWifi {
                    network: network.name.clone(),
                }
            }
        });
        items.push(Item::Network(NetworkFace {
            name: network.name.clone(),
            signal: network.signal,
            secure: network.secure,
            connected: network.connected,
            value,
            press,
            asking: state
                .wifi_password
                .clone()
                .filter(|prompt| prompt.ssid == network.name),
            profile: state
                .saved_network(&network.name)
                .map(|saved| saved.id.clone()),
        }));
    }
    if state.network.wifi.networks.is_empty() {
        items.push(Item::Message(t(if state.wifi_scanning() {
            "qs-wifi-scanning"
        } else if state.network.problem.is_some() {
            "qs-wifi-scan-failed"
        } else {
            "qs-wifi-none"
        })));
    }
    // Saved networks this machine cannot see: the only place they can be
    // forgotten from, or kept from joining by themselves.
    let mut unseen: Vec<SavedNetwork> = state
        .network
        .saved
        .iter()
        .filter(|saved| {
            !state
                .network
                .wifi
                .networks
                .iter()
                .any(|network| network.name == saved.name)
        })
        .cloned()
        .collect();
    unseen.sort_by_key(|saved| saved.name.to_lowercase());
    items.extend(unseen.into_iter().map(Item::Saved));
    items.push(Item::Hidden {
        prompt: state.wifi_hidden.clone(),
    });
    items
}

fn bluetooth(state: &QuickSettingsState) -> Vec<Item> {
    let mut items = Vec::new();
    if state.bluetooth.on {
        // Connected first, then by name: what is in use is what a person
        // came to change.
        let mut devices = state.bluetooth.accessories.clone();
        devices.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        if devices.is_empty() && state.bluetooth.nearby.is_empty() {
            items.push(Item::Message(t("qs-bluetooth-empty")));
        }
        items.extend(devices.into_iter().map(Item::Accessory));
        items.push(Item::LookForDevices {
            looking: state.bluetooth.discovering,
        });
        items.extend(state.bluetooth.nearby.iter().cloned().map(Item::Nearby));
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
        .vpn
        .profiles
        .iter()
        .map(|profile| Item::Profile {
            name: profile.name.clone(),
            id: profile.id.clone(),
            active: profile.connected,
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
    let mut items: Vec<Item> = Vec::new();
    // Nothing to list and nothing that could record: this session has no
    // audio graph, as a check's desktop in a container has none.
    if !state.volume_available && state.microphone.is_none() && state.sound_streams.is_empty() {
        items.push(Item::Message(t("qs-sound-no-device")));
    }
    // The microphone first and the outputs next, so where sound goes stays
    // put however many applications come and go below them.
    if let Some(microphone) = &state.microphone {
        items.push(Item::Microphone(microphone.clone()));
    }
    items.extend(state.sound_outputs.iter().cloned().map(Item::Output));
    items.extend(
        state
            .sound_streams
            .iter()
            .map(|stream| Item::Stream(stream_face(state, stream))),
    );
    items.push(Item::Settings {
        label: t("qs-sound-settings"),
        link: "settings://sound",
    });
    items
}

/// One application's stream: where it goes now, and the outputs it can
/// move to when it plays.
fn stream_face(state: &QuickSettingsState, stream: &crate::SoundStream) -> StreamFace {
    StreamFace {
        stream: stream.clone(),
        goes: stream_destination(state, stream),
        outputs: if stream.playback {
            state
                .sound_outputs
                .iter()
                .map(|output| crate::SoundOutput {
                    node: output.node,
                    name: output.name.clone(),
                    current: stream.target == Some(output.node),
                })
                .collect()
        } else {
            Vec::new()
        },
    }
}

/// Where a stream goes, in words: the output it plays on, the microphone it
/// records from, or the default it follows.
fn stream_destination(state: &QuickSettingsState, stream: &crate::SoundStream) -> String {
    if stream.playback {
        let output = stream
            .target
            .and_then(|target| {
                state
                    .sound_outputs
                    .iter()
                    .find(|output| output.node == target)
            })
            .map_or_else(|| t("qs-default-output"), |output| output.name.clone());
        t_args("qs-stream-plays-on", &[("output", output.into())])
    } else {
        let input = stream
            .target
            .and_then(|target| {
                state
                    .microphone
                    .as_ref()
                    .filter(|microphone| microphone.node == target)
            })
            .map_or_else(
                || t("qs-default-input"),
                |microphone| microphone.name.clone(),
            );
        t_args("qs-stream-records-from", &[("input", input.into())])
    }
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
    use crate::{DisplayBrightness, SoundInput, SoundOutput, SoundStream};

    /// The words of a slider's device line, in the order it says them.
    fn said(facts: &[Fact]) -> Vec<&str> {
        facts.iter().map(|fact| fact.text.as_str()).collect()
    }

    fn network(name: &str, signal: u8, connected: bool, known: bool) -> WifiNetwork {
        WifiNetwork {
            name: name.into(),
            signal,
            secure: true,
            connected,
            known,
        }
    }

    #[test]
    fn no_brightness_slider_without_anything_to_dim() {
        let mut state = QuickSettingsState::fixture();
        state.brightness_available = false;
        let ids: Vec<String> = Sliders::of(&state)
            .rows
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, ["qs-volume"]);
        state.brightness_available = true;
        let ids: Vec<String> = Sliders::of(&state)
            .rows
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(ids, ["qs-brightness", "qs-volume"]);
    }

    /// A session with no audio server: the row stays and says there is no
    /// audio device, inert until the sound service reads an output again.
    #[test]
    fn the_volume_row_says_there_is_no_audio_device_until_one_appears() {
        let mut state = QuickSettingsState::fixture();
        state.volume_available = false;
        state.status.volume = None;
        state.sound_outputs.clear();
        let sliders = Sliders::of(&state);
        let volume = sliders
            .rows
            .iter()
            .find(|row| row.id == "qs-volume")
            .expect("the row stands without an audio server");
        assert!(!volume.available, "there is nothing to set");
        assert!(volume.value.is_empty(), "and no level to show");
        assert_eq!(said(&volume.device), vec![t("qs-sound-no-device")]);
        assert!(!sliders.outputs, "there is no output to choose");

        // The server came: the same row reads the output it landed.
        state.volume_available = true;
        state.status.volume = Some(media::audio::default_node::VolumeState {
            level: 0.4,
            muted: false,
        });
        state.sound_outputs = vec![SoundOutput {
            node: 41,
            name: "Speakers".into(),
            current: true,
        }];
        let sliders = Sliders::of(&state);
        let volume = sliders
            .rows
            .iter()
            .find(|row| row.id == "qs-volume")
            .expect("an output to set");
        assert!(volume.available);
        assert_eq!(said(&volume.device), ["Speakers"]);
        assert_eq!(volume.value, "40%");
        assert!(sliders.outputs);
    }

    #[test]
    fn the_sound_section_says_so_when_there_is_no_audio_device() {
        let mut state = QuickSettingsState::fixture();
        state.volume_available = false;
        state.status.volume = None;
        state.microphone = None;
        state.sound_outputs.clear();
        state.sound_streams.clear();
        let items = sound(&state);
        assert!(
            matches!(items.first(), Some(Item::Message(message)) if message == &t("qs-sound-no-device")),
            "the section names the missing audio device"
        );
        assert!(
            matches!(items.last(), Some(Item::Settings { .. })),
            "and still leads to the sound settings"
        );
    }

    #[test]
    fn each_slider_names_what_it_sets_and_the_value_it_stands_at() {
        let mut state = QuickSettingsState::fixture();
        state.sound_outputs = vec![
            SoundOutput {
                node: 41,
                name: "Speakers".into(),
                current: true,
            },
            SoundOutput {
                node: 52,
                name: "Studio headphones".into(),
                current: false,
            },
        ];
        let sliders = Sliders::of(&state);
        let brightness = sliders
            .rows
            .iter()
            .find(|row| row.id == "qs-brightness")
            .expect("a screen to dim");
        assert_eq!(brightness.value, "60%");
        assert!(
            brightness.device.is_empty(),
            "one screen needs no name of its own"
        );
        let volume = sliders
            .rows
            .iter()
            .find(|row| row.id == "qs-volume")
            .expect("an output to set");
        assert_eq!(
            said(&volume.device),
            ["Speakers"],
            "it names where the sound plays"
        );
        assert_eq!(volume.value, "40%");
        assert!((volume.level - 0.4).abs() < 0.001);
    }

    /// Two displays are two rows, each named as a person knows the display —
    /// its model, or the part it plays — and never by its connector.
    #[test]
    fn each_display_has_its_own_row_named_as_a_person_knows_it() {
        let mut state = QuickSettingsState::fixture();
        state.displays = vec![
            DisplayBrightness {
                connector: "eDP-1".into(),
                name: "Built-in display".into(),
                level: 0.7,
            },
            DisplayBrightness {
                connector: "HDMI-A-1".into(),
                name: "LG ULTRAGEAR+".into(),
                level: 0.3,
            },
        ];
        let rows: Vec<SliderFace> = Sliders::of(&state)
            .rows
            .into_iter()
            .filter(|row| matches!(row.kind, SliderKind::Display(_)))
            .collect();
        let names: Vec<String> = rows.iter().map(|row| row.name.clone()).collect();
        assert_eq!(
            names,
            [
                t_args(
                    "qs-brightness-display",
                    &[("display", "Built-in display".into())]
                ),
                t_args(
                    "qs-brightness-display",
                    &[("display", "LG ULTRAGEAR+".into())]
                ),
            ],
            "each row says the display it dims"
        );
        let devices: Vec<Vec<Fact>> = rows.iter().map(|row| row.device.clone()).collect();
        assert_eq!(
            devices,
            [
                vec![Fact::text("Built-in display")],
                vec![Fact::text("LG ULTRAGEAR+")]
            ],
            "and names it on its own line"
        );
        let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        assert_eq!(
            ids,
            ["qs-brightness-eDP-1", "qs-brightness-HDMI-A-1"],
            "the connector is what the row's control addresses, not its name"
        );
    }

    #[test]
    fn a_muted_output_says_so_and_stands_empty() {
        let mut state = QuickSettingsState::fixture();
        state
            .status
            .volume
            .as_mut()
            .expect("a default output")
            .muted = true;
        let volume = Sliders::of(&state)
            .rows
            .into_iter()
            .find(|row| row.id == "qs-volume")
            .expect("an output to set");
        assert_eq!(volume.value, "Muted");
        assert!(
            volume.level.abs() < 0.001,
            "a muted output is silent at any level"
        );
    }

    #[test]
    fn the_charge_stands_above_the_rest_and_the_mode_has_no_row_of_its_own() {
        use tilo_power_mgmt::{BatterySnapshot, BatteryState, BatteryTime};
        let mut state = QuickSettingsState::fixture();
        state.status.battery = Some(BatterySnapshot {
            present: true,
            percent: 76,
            state: BatteryState::Discharging,
        });
        state.status.battery_time = Some(BatteryTime::ToEmpty(214));
        state.power_profiles = vec![PowerProfile::Saver, PowerProfile::Balanced];
        let faces = rows(&state);
        assert_eq!(faces[0].row, RootRow::Battery);
        assert_eq!(faces[0].value.as_deref(), Some("76%"));
        assert_eq!(faces[0].detail.as_deref(), Some("3 h 34 min left"));
        assert!(
            faces
                .iter()
                .all(|face| face.section() != Some(QsSection::PowerMode)),
            "the mode is switched on its tile, which opens the modes' page: no row says it twice"
        );
        assert!(
            tiles(&state)
                .iter()
                .any(|tile| tile.control == QuickSettingsControl::PowerMode
                    && tile.section == Some(QsSection::PowerMode)),
            "the tile is where the mode is chosen and where its page opens"
        );
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
            .map(|network| network.name.as_str())
            .collect();
        assert_eq!(
            order,
            ["Home", "Weak known", "Strong stranger", "another", "Beta"]
        );
    }

    #[test]
    fn a_protected_network_never_seen_asks_for_its_password_and_a_known_one_joins() {
        let mut state = QuickSettingsState::fixture();
        state.network.wifi.available = true;
        state.network.wifi.on = true;
        state.network.wifi.networks = vec![
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
        state.network.wifi.available = true;
        state.network.wifi.on = false;
        state.network.pending = vec!["set-wifi".into()];
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

    #[test]
    fn the_sound_section_keeps_the_microphone_and_the_outputs_above_the_streams() {
        let mut state = QuickSettingsState::fixture();
        state.microphone = Some(SoundInput {
            node: 50,
            name: "Microphone".into(),
            level: 0.5,
            muted: false,
        });
        state.sound_outputs = vec![
            SoundOutput {
                node: 40,
                name: "Speakers".into(),
                current: true,
            },
            SoundOutput {
                node: 41,
                name: "Headphones".into(),
                current: false,
            },
        ];
        state.sound_streams = vec![
            SoundStream {
                node: 60,
                label: "Music".into(),
                plays: Some("Clair de lune".into()),
                playback: true,
                target: Some(41),
                level: 0.3,
                muted: false,
            },
            SoundStream {
                node: 61,
                label: "Browser".into(),
                plays: Some("Recording".into()),
                playback: false,
                target: Some(50),
                level: 0.8,
                muted: false,
            },
        ];

        let listed: Vec<String> = sound(&state)
            .into_iter()
            .map(|item| match item {
                Item::Microphone(microphone) => format!("input:{}", microphone.node),
                Item::Output(output) => format!("output:{}", output.node),
                Item::Stream(stream) => format!("stream:{}:{}", stream.stream.node, stream.goes),
                Item::Settings { .. } => "settings".to_owned(),
                _ => "other".to_owned(),
            })
            .collect();
        assert_eq!(
            listed,
            [
                "input:50",
                "output:40",
                "output:41",
                "stream:60:Playing on Headphones",
                "stream:61:Recording from Microphone",
                "settings",
            ],
            "the microphone, the outputs, then every stream and the way to the settings"
        );

        let Item::Stream(playing) = &sound(&state)[3] else {
            panic!("the playing stream's row");
        };
        assert_eq!(
            playing
                .outputs
                .iter()
                .map(|output| (output.name.as_str(), output.current))
                .collect::<Vec<_>>(),
            [("Speakers", false), ("Headphones", true)],
            "the output it plays on is the one marked"
        );
        let Item::Stream(recording) = &sound(&state)[4] else {
            panic!("the recording stream's row");
        };
        assert!(
            recording.outputs.is_empty(),
            "a stream that records does not move between outputs"
        );
    }
}
