//! What the panel shows of the device, and where each part comes from.
//!
//! Every part of the panel is a reading one of the session's services
//! publishes: the sound and the players from the audio service, the radios
//! and the networks from the connectivity service, the charge from the power
//! service, the displays, the notifications, the clipboard and the person's
//! devices from the session itself. [`Readings`] keeps the device as those
//! readings add up to it, and [`land`] folds one reading into it.
//!
//! The readings are landed on a worker, so the readings themselves are shared
//! across threads; a part of the view is built again only when what it reads
//! moved, so a level that changes rebuilds its slider and nothing else.

use std::sync::{Arc, Mutex};

use connectivity::model::{Bluetooth, Network, Vpn};
use serde::Deserialize;
use ui::serde_json;

use crate::PowerProfile;
use crate::session::{
    BATTERY, BLUETOOTH, CLIPBOARD, DEVICES, DISPLAYS, NETWORK, NIGHT_LIGHT, NOTIFICATIONS,
    QUICK_SETTINGS, SLEEP, SOUND, SYSTEM_UPDATE, VPN,
};
use crate::state::QuickSettingsState;

/// The panel's readings, as everything that shows them hands them over.
#[derive(Clone)]
pub struct Feed {
    readings: Readings,
}

impl Feed {
    /// The readings themselves.
    pub(crate) fn readings(&self) -> Readings {
        self.readings.clone()
    }

    /// The device as it reads now.
    pub fn set(&self, state: &QuickSettingsState) {
        self.readings.set(state);
    }

    /// The device as it reads now: what a readout of a level shows after a
    /// key changed it.
    #[must_use]
    pub fn current(&self) -> QuickSettingsState {
        self.readings.current()
    }

    /// Change the readings in place.
    pub fn update(&self, change: impl FnOnce(&mut QuickSettingsState)) {
        self.readings.update(change);
    }
}

type Projection = Box<dyn Fn(&QuickSettingsState) + Send + Sync>;

/// The readings the view shows, and what each part of it reads of them.
#[derive(Clone)]
pub(crate) struct Readings {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    current: QuickSettingsState,
    projections: Vec<Projection>,
}

impl Readings {
    pub(crate) fn new(initial: QuickSettingsState) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                current: initial,
                projections: Vec::new(),
            })),
        }
    }

    pub(crate) fn feed(&self) -> Feed {
        Feed {
            readings: self.clone(),
        }
    }

    pub(crate) fn current(&self) -> QuickSettingsState {
        self.lock().current.clone()
    }

    pub(crate) fn read<V>(&self, read: impl FnOnce(&QuickSettingsState) -> V) -> V {
        read(&self.lock().current)
    }

    /// Change the readings, and hand every part what it reads of the new
    /// ones when something moved.
    pub(crate) fn update(&self, change: impl FnOnce(&mut QuickSettingsState)) {
        let (state, projections) = {
            let mut inner = self.lock();
            let before = inner.current.clone();
            change(&mut inner.current);
            if inner.current == before {
                return;
            }
            (
                inner.current.clone(),
                std::mem::take(&mut inner.projections),
            )
        };
        for project in &projections {
            project(&state);
        }
        let mut inner = self.lock();
        let added = std::mem::replace(&mut inner.projections, projections);
        inner.projections.extend(added);
    }

    fn set(&self, state: &QuickSettingsState) {
        self.update(|current| current.clone_from(state));
    }

    /// What `read` reads of the readings, kept current: a part keyed on it is
    /// built again when it moves.
    pub(crate) fn project<K>(
        &self,
        read: impl Fn(&QuickSettingsState) -> K + Send + Sync + 'static,
    ) -> ui::LocalState<K>
    where
        K: Clone + PartialEq + Send + Sync + 'static,
    {
        let state = ui::LocalState::new(self.read(&read));
        let writer = state.writer();
        let kept = state.clone();
        self.lock().projections.push(Box::new(move |readings| {
            let value = read(readings);
            if kept.with(|shown| *shown != value) {
                writer.set(value);
            }
        }));
        state
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Fold the reading `text` a service published on `resource` into the
/// readings. Whether it was one of the resources this folds.
pub(crate) fn land(readings: &Readings, resource: &str, text: &str) -> bool {
    match resource {
        NETWORK => parse::<Network>(readings, resource, text, land_network),
        BLUETOOTH => parse::<Bluetooth>(readings, resource, text, land_bluetooth),
        VPN => parse::<Vpn>(readings, resource, text, land_vpn),
        SOUND => parse::<audio::sound::Sound>(readings, resource, text, |state, sound| {
            crate::sound::land(state, sound);
        }),
        BATTERY => parse::<Battery>(readings, resource, text, land_battery),
        SLEEP => parse::<Sleep>(readings, resource, text, land_sleep),
        SYSTEM_UPDATE => parse::<Update>(readings, resource, text, |state, update| {
            state.firmware_updates.clone_from(&update.firmware);
        }),
        DISPLAYS => parse::<Displays>(readings, resource, text, land_displays),
        NOTIFICATIONS => {
            parse::<crate::hosted::NotificationCentre>(readings, resource, text, land_notifications)
        }
        CLIPBOARD => {
            parse::<Vec<crate::hosted::ClipboardCopy>>(readings, resource, text, |state, copies| {
                land_clipboard(state, copies);
            })
        }
        QUICK_SETTINGS => parse::<Arrangement>(readings, resource, text, land_arrangement),
        DEVICES => parse::<crate::hosted::DevicesReading>(readings, resource, text, land_devices),
        NIGHT_LIGHT => parse::<NightLight>(readings, resource, text, land_night_light),
        _ => false,
    }
}

/// Fold one published reading into the panel's, through `fold`.
fn parse<V>(
    readings: &Readings,
    resource: &str,
    text: &str,
    fold: impl FnOnce(&mut QuickSettingsState, &V),
) -> bool
where
    V: serde::de::DeserializeOwned,
{
    match serde_json::from_str::<V>(text) {
        Ok(value) => {
            readings.update(|state| fold(state, &value));
            true
        }
        Err(error) => {
            tracing::warn!(%error, %resource, "the session's reading did not parse");
            false
        }
    }
}

/// The session's night light, as its reading says it is: the switch shows
/// what the session did, so a press that was refused shows as it still is.
#[derive(Deserialize)]
struct NightLight {
    on: bool,
}

fn land_night_light(state: &mut QuickSettingsState, night_light: &NightLight) {
    state.night_light = night_light.on;
}

fn land_network(state: &mut QuickSettingsState, network: &Network) {
    state.status.link.clone_from(&network.link);
    if !network.wifi.on {
        // Nothing is joined while the radio is off: no prompt can be up.
        state.wifi_joining = None;
        state.wifi_password = None;
        state.wifi_hidden = None;
    }
    // A join that was under way is over once the machine says it joined, or
    // says why it did not.
    if let Some(joining) = state.wifi_joining.clone() {
        let joined = network
            .wifi
            .connected()
            .is_some_and(|connected| connected.name == joining);
        if joined || network.problem.is_some() {
            state.wifi_joining = None;
            if let Some(prompt) = &mut state.wifi_hidden {
                prompt.connecting = false;
            }
            if let Some(prompt) = &mut state.wifi_password {
                prompt.connecting = false;
                if !joined {
                    prompt.error.clone_from(&network.problem);
                }
            }
        }
    }
    state.network.clone_from(network);
}

fn land_bluetooth(state: &mut QuickSettingsState, bluetooth: &Bluetooth) {
    state.bluetooth.clone_from(bluetooth);
}

fn land_vpn(state: &mut QuickSettingsState, vpn: &Vpn) {
    state.vpn.clone_from(vpn);
}

/// The pack, and how long it lasts or needs from where it stands.
fn land_battery(state: &mut QuickSettingsState, battery: &Battery) {
    state.status.battery = Some(tilo_power_mgmt::BatterySnapshot {
        present: battery.present,
        percent: battery.percentage,
        state: battery.state,
    });
    state.status.battery_time = battery.minutes_left.map(|minutes| {
        if battery.state.is_discharging() {
            tilo_power_mgmt::BatteryTime::ToEmpty(minutes)
        } else {
            tilo_power_mgmt::BatteryTime::ToFull(minutes)
        }
    });
}

/// What holds the machine awake, and the power profiles it offers.
fn land_sleep(state: &mut QuickSettingsState, sleep: &Sleep) {
    state.awake_holders.clone_from(&sleep.holds);
    state.power_profiles = sleep
        .profiles
        .iter()
        .filter_map(|it| profile_named(it))
        .collect();
    if let Some(profile) = sleep.profile.as_deref().and_then(profile_named) {
        state.power_profile = profile;
    }
}

fn land_displays(state: &mut QuickSettingsState, reading: &Displays) {
    let mut levels = reading
        .displays
        .iter()
        .filter_map(|display| {
            Some(crate::DisplayBrightness {
                connector: display.connector.clone(),
                name: display.name.clone(),
                level: display.brightness?,
            })
        })
        .collect();
    crate::focused_display_first(&mut levels, reading.focused.as_deref());
    let brightness = reading.brightness();
    state.brightness_available = brightness.is_some();
    if let Some(brightness) = brightness {
        state.brightness = brightness;
    }
    state.focused_display.clone_from(&reading.focused);
    state.displays = levels;
}

fn land_notifications(state: &mut QuickSettingsState, centre: &crate::hosted::NotificationCentre) {
    state.notifications = centre.count;
    state.do_not_disturb =
        settings_store::notification_policy::NotificationPolicy::load().do_not_disturb;
}

fn land_clipboard(state: &mut QuickSettingsState, copies: &[crate::hosted::ClipboardCopy]) {
    state.clipboard = crate::hosted::clipboard_view(copies, ui::time::unix_now_ms());
}

/// Which controls the panel shows, in the person's order, as the session keeps them: a control
/// this build does not know stays out of it.
#[derive(Deserialize)]
struct Arrangement {
    controls: Vec<String>,
}

fn land_arrangement(state: &mut QuickSettingsState, arrangement: &Arrangement) {
    state.arrangement.controls = arrangement
        .controls
        .iter()
        .filter_map(|control| crate::faces::control_named(control))
        .collect();
}

fn land_devices(state: &mut QuickSettingsState, reading: &crate::hosted::DevicesReading) {
    state.device_rows = reading.rows();
    state.personal_devices = Some(reading.count());
    state.handoff_targets = Some(reading.targets());
    state.offer.clone_from(&reading.offer);
}

/// The power profile `name` is, as the daemon words it; `None` for a name
/// this panel does not know.
fn profile_named(name: &str) -> Option<PowerProfile> {
    match name.trim().to_ascii_lowercase().as_str() {
        "power-saver" | "saver" => Some(PowerProfile::Saver),
        "balanced" | "balance" => Some(PowerProfile::Balanced),
        "performance" => Some(PowerProfile::Performance),
        _ => None,
    }
}

/// The sleep policy, as the power service publishes it: enough of it to name
/// the profile in force, the ones offered, and what holds the machine awake.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(crate) struct Sleep {
    holds: Vec<String>,
    profile: Option<String>,
    profiles: Vec<String>,
}

/// The system update, as the system service publishes it.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(crate) struct Update {
    firmware: Option<usize>,
}

/// The pack, as the power service publishes it.
#[derive(Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(crate) struct Battery {
    present: bool,
    percentage: u8,
    state: tilo_power_mgmt::BatteryState,
    minutes_left: Option<u32>,
}

impl Default for Battery {
    fn default() -> Self {
        Self {
            present: false,
            percentage: 0,
            state: tilo_power_mgmt::BatteryState::Unknown,
            minutes_left: None,
        }
    }
}

/// The displays, as the session publishes them: enough of each to show its
/// slider.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(crate) struct Displays {
    displays: Vec<Display>,
    focused: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub(crate) struct Display {
    connector: String,
    name: String,
    brightness: Option<f32>,
}

impl Displays {
    /// How bright the display the person works on is, when it can be dimmed,
    /// else the first display that can: the level the panel's one slider
    /// shows.
    fn brightness(&self) -> Option<f32> {
        self.focused
            .as_deref()
            .and_then(|focused| {
                self.displays
                    .iter()
                    .find(|display| display.connector == focused)
            })
            .and_then(|display| display.brightness)
            .or_else(|| self.displays.iter().find_map(|display| display.brightness))
    }
}
