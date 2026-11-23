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
use power::{Battery, Sleep};
use serde::Deserialize;
use tilo_system_resources::update::Update;
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

    pub(crate) fn read<V>(&self, read: impl FnOnce(&QuickSettingsState) -> V) -> V {
        read(&self.lock().current)
    }

    /// Forget the parts the page built before: it is built again, so its
    /// parts project what they read anew, and nothing of the page before
    /// stays to be handed readings it no longer shows.
    pub(crate) fn built_again(&self) {
        self.lock().projections.clear();
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
            state.firmware_updates = update.firmware;
        }),
        DISPLAYS => parse::<DisplaysReading>(readings, resource, text, land_displays),
        NOTIFICATIONS => {
            parse::<crate::hosted::NotificationCentre>(readings, resource, text, land_notifications)
        }
        CLIPBOARD => {
            parse::<Vec<crate::hosted::ClipboardCopy>>(readings, resource, text, |state, copies| {
                land_clipboard(state, copies);
            })
        }
        QUICK_SETTINGS => parse::<Controls>(readings, resource, text, land_controls),
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

fn land_displays(state: &mut QuickSettingsState, reading: &DisplaysReading) {
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
    state.brightness_available = reading.level.is_some();
    if let Some(brightness) = reading.level {
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
struct Controls {
    controls: Vec<String>,
}

fn land_controls(state: &mut QuickSettingsState, config: &Controls) {
    state.config.controls = config
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

/// The displays, as the session publishes them: enough of each to show its
/// slider. The publisher's `Reading` lives in `tilo-session`, which depends on
/// this crate.
#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct DisplaysReading {
    displays: Vec<Display>,
    focused: Option<String>,
    level: Option<f32>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct Display {
    connector: String,
    name: String,
    brightness: Option<f32>,
}

#[cfg(test)]
mod tests {
    use audio::sound::{Device, Sound};
    use connectivity::model::{Link, LinkKind, VpnProfile, Wifi};
    use tilo_power_mgmt::{BatterySnapshot, BatteryState, BatteryTime, PowerAction};

    use super::*;

    fn published(value: &impl serde::Serialize) -> String {
        tilo_service::reading(value).to_string()
    }

    fn isolated_settings(
        doc: serde_json::Map<String, serde_json::Value>,
    ) -> settings_store::SourceScope {
        settings_store::scoped_source(Some(Arc::new(settings_store::HostDocument::new(
            doc,
            |_| Ok(()),
        ))))
    }

    #[test]
    fn a_published_battery_reading_lands_with_its_minutes() {
        let _settings = isolated_settings(serde_json::Map::new());
        let readings = Readings::new(QuickSettingsState::unread());
        let discharging = Battery {
            present: true,
            percentage: 76,
            state: BatteryState::Discharging,
            on_battery: true,
            minutes_left: Some(214),
            charge_limit: None,
            notice: None,
        };
        let wire = tilo_service::reading(&discharging);
        assert_eq!(wire["minutes_left"], serde_json::json!(214));
        assert_eq!(wire["on_battery"], serde_json::json!(true));
        assert!(land(&readings, BATTERY, &wire.to_string()));
        readings.read(|state| {
            assert_eq!(
                state.status.battery,
                Some(BatterySnapshot {
                    present: true,
                    percent: 76,
                    state: BatteryState::Discharging,
                })
            );
            assert_eq!(state.status.battery_time, Some(BatteryTime::ToEmpty(214)));
            assert!(
                crate::faces::battery_time_text(state).is_some(),
                "the panel formats the remaining minutes"
            );
        });

        let charging = Battery {
            present: true,
            percentage: 80,
            state: BatteryState::Charging,
            on_battery: false,
            minutes_left: Some(45),
            charge_limit: None,
            notice: None,
        };
        assert!(land(&readings, BATTERY, &published(&charging)));
        readings.read(|state| {
            assert_eq!(state.status.battery_time, Some(BatteryTime::ToFull(45)));
        });

        let unknown_rate = Battery {
            minutes_left: None,
            ..charging
        };
        assert!(land(&readings, BATTERY, &published(&unknown_rate)));
        readings.read(|state| {
            assert_eq!(state.status.battery_time, None);
        });
    }

    #[test]
    fn a_published_sleep_reading_lands_its_holds_and_profiles() {
        let _settings = isolated_settings(serde_json::Map::new());
        let readings = Readings::new(QuickSettingsState::unread());
        let sleep = Sleep {
            blank_minutes: Some(10),
            lock_minutes: Some(5),
            suspend_minutes: None,
            lock_on_sleep: true,
            lid_close: PowerAction::Lock,
            power_key: PowerAction::Suspend,
            held_awake: true,
            holds: vec!["Video Player".into()],
            profile: Some("power-saver".into()),
            profiles: vec![
                "power-saver".into(),
                "balanced".into(),
                "performance".into(),
            ],
        };
        assert!(land(&readings, SLEEP, &published(&sleep)));
        readings.read(|state| {
            assert_eq!(state.awake_holders, ["Video Player"]);
            assert_eq!(state.power_profile, PowerProfile::Saver);
            assert_eq!(
                state.power_profiles,
                [
                    PowerProfile::Saver,
                    PowerProfile::Balanced,
                    PowerProfile::Performance,
                ]
            );
        });
    }

    #[test]
    fn a_published_update_reading_lands_its_firmware_count() {
        let _settings = isolated_settings(serde_json::Map::new());
        let readings = Readings::new(QuickSettingsState::unread());
        let update = Update {
            firmware: Some(3),
            restart_needed: true,
            desktop_pending: true,
            ..Update::default()
        };
        assert!(land(&readings, SYSTEM_UPDATE, &published(&update)));
        readings.read(|state| {
            assert_eq!(state.firmware_updates, Some(3));
        });

        let none = Update::default();
        assert!(land(&readings, SYSTEM_UPDATE, &published(&none)));
        readings.read(|state| {
            assert_eq!(state.firmware_updates, None);
        });
    }

    #[test]
    fn published_connectivity_sound_and_hosted_readings_round_trip() {
        let mut doc = serde_json::Map::new();
        doc.insert(
            settings_store::notification_policy::SECTION.into(),
            serde_json::json!({ "do_not_disturb": true }),
        );
        let _settings = isolated_settings(doc);
        let readings = Readings::new(QuickSettingsState::unread());

        let network = Network {
            wifi: Wifi {
                available: true,
                on: true,
                ..Wifi::default()
            },
            link: Some(Link {
                kind: LinkKind::Wifi,
                name: Some("Home".into()),
                signal: 4,
            }),
            ..Network::default()
        };
        assert!(land(&readings, NETWORK, &published(&network)));

        let bluetooth = Bluetooth {
            available: true,
            on: true,
            discovering: true,
            ..Bluetooth::default()
        };
        assert!(land(&readings, BLUETOOTH, &published(&bluetooth)));

        let vpn = Vpn {
            profiles: vec![VpnProfile {
                name: "Work".into(),
                id: "work-vpn".into(),
                kind: "wireguard".into(),
                connected: true,
            }],
            ..Vpn::default()
        };
        assert!(land(&readings, VPN, &published(&vpn)));

        let sound = Sound {
            output: Some(40),
            volume: Some(0.55),
            muted: false,
            outputs: vec![Device {
                node: 40,
                name: "node-40".into(),
                label: "Speakers".into(),
                chosen: true,
                volume: Some(0.55),
                muted: false,
                channels: 2,
                codec: None,
            }],
            ..Sound::default()
        };
        assert!(land(&readings, SOUND, &published(&sound)));

        let centre = crate::hosted::NotificationCentre {
            count: 4,
            ..crate::hosted::NotificationCentre::default()
        };
        assert!(land(&readings, NOTIFICATIONS, &published(&centre)));

        let copies = vec![crate::hosted::ClipboardCopy {
            key: "c1".into(),
            mime: "text/plain;charset=utf-8".into(),
            text: Some("hello".into()),
            created_ms: 1_000,
            pinned: true,
            from: Some("laptop".into()),
        }];
        assert!(land(&readings, CLIPBOARD, &published(&copies)));

        let devices = crate::hosted::DevicesReading {
            devices: vec![crate::hosted::PersonalDevice {
                id: "phone".into(),
                label: "Phone".into(),
                online: true,
                nearby: false,
                local: false,
                display: false,
                wakeable: false,
                audio_output: false,
            }],
            ..crate::hosted::DevicesReading::default()
        };
        assert!(land(&readings, DEVICES, &published(&devices)));

        readings.read(|state| {
            assert_eq!(state.network, network);
            assert_eq!(state.status.link, network.link);
            assert_eq!(state.bluetooth, bluetooth);
            assert_eq!(state.vpn, vpn);
            assert!(state.volume_available);
            assert_eq!(state.volume_level(), 0.55);
            assert_eq!(state.notifications, 4);
            assert!(state.do_not_disturb);
            assert_eq!(state.clipboard.total, 1);
            assert_eq!(state.personal_devices, Some(1));
        });
    }
}
