//! Landing what the workers read in the panel's readings. Each field lands
//! only if it was actually read, so a missing daemon leaves the previous
//! value rather than blanking it.

use os_actions as qs;

use super::reads::{BluetoothReading, FastReading, FieldReading, WifiJoinResult};
use crate::i18n::{t, t_args};
use crate::{BtDevice, BtNearby, DisplayBrightness, PowerProfile, QuickSettingsState, WifiNetwork};

/// The panel's three-way profile, from the daemon's.
pub(crate) const fn ui_power_profile(profile: qs::PowerProfile) -> PowerProfile {
    match profile {
        qs::PowerProfile::Saver => PowerProfile::Saver,
        qs::PowerProfile::Balanced => PowerProfile::Balanced,
        qs::PowerProfile::Performance => PowerProfile::Performance,
    }
}

/// The daemon's profile, from the panel's.
pub(crate) const fn os_power_profile(profile: PowerProfile) -> qs::PowerProfile {
    match profile {
        PowerProfile::Saver => qs::PowerProfile::Saver,
        PowerProfile::Balanced => qs::PowerProfile::Balanced,
        PowerProfile::Performance => qs::PowerProfile::Performance,
    }
}

/// Everything the panel shows, read on opening.
pub(crate) fn fast(qs: &mut QuickSettingsState, reading: FastReading) {
    let FastReading {
        brightness,
        displays,
        volume_available,
        volume,
        bluetooth,
        bluetooth_available,
        power,
        power_profiles,
        wired,
        battery,
        battery_time,
        radios,
        privacy,
        vpns,
        link,
        available,
    } = reading;
    qs.available = available;
    if let Some(profiles) = power_profiles {
        qs.power_profiles = profiles.into_iter().map(ui_power_profile).collect();
    }
    brightness_levels(qs, brightness, displays);
    if let Some(available) = volume_available {
        qs.volume_available = available;
    }
    if let Some(volume) = volume {
        qs.status.volume = Some(volume);
    }
    if let Some(radios) = radios {
        self::radios(qs, radios);
    }
    if let Some(vpns) = vpns {
        qs.vpns = vpns;
    }
    qs.bluetooth_available = bluetooth_available;
    if let Some(on) = bluetooth {
        qs.bluetooth = on;
    }
    if let Some(wired) = wired {
        qs.wired = Some(wired);
    }
    if let Some(profile) = power {
        qs.power_profile = ui_power_profile(profile);
    }
    if let Some(link) = link {
        qs.status.link = Some(link);
    }
    qs.status.battery = Some(battery);
    qs.status.battery_time = battery_time;
    qs.privacy = privacy;
}

/// The read-back of the one control the panel just wrote.
pub(crate) fn field(qs: &mut QuickSettingsState, reading: FieldReading) {
    match reading {
        FieldReading::Radios(Some(radios)) => self::radios(qs, radios),
        FieldReading::Bluetooth(Some(on)) => qs.bluetooth = on,
        FieldReading::Wired(Some(wired)) => qs.wired = Some(wired),
        FieldReading::Power(Some(profile)) => qs.power_profile = ui_power_profile(profile),
        FieldReading::Vpns(Some(vpns)) => qs.vpns = vpns,
        FieldReading::Brightness(brightness, displays) => {
            brightness_levels(qs, brightness, Some(displays));
        }
        FieldReading::Volume(Some(volume)) => qs.status.volume = Some(volume),
        // Not read: leave the control as it is.
        FieldReading::Radios(None)
        | FieldReading::Bluetooth(None)
        | FieldReading::Wired(None)
        | FieldReading::Power(None)
        | FieldReading::Vpns(None)
        | FieldReading::Volume(None) => {}
    }
}

/// Wi-Fi on or off is taken from the radio snapshot rather than read
/// again; a machine with no adapter leaves the switch alone.
pub(crate) fn radios(qs: &mut QuickSettingsState, radios: qs::RadioState) {
    if let Ok(on) = qs::wifi_from_radios(&radios) {
        qs.wifi = on;
        if on {
            // Whatever kept it off has let go.
            qs.wifi_problem = None;
        }
    }
    qs.radios = radios;
}

/// `displays: None` means the monitor list was not read this time, so
/// neither the list nor the slider's availability changes. The display the
/// person works on gets the first slider.
pub(crate) fn brightness_levels(
    qs: &mut QuickSettingsState,
    brightness: Option<f32>,
    displays: Option<Vec<DisplayBrightness>>,
) {
    let brightness = match displays {
        Some(mut displays) => {
            crate::focused_display_first(&mut displays, qs.focused_display.as_deref());
            qs.brightness_available = brightness.is_some() || !displays.is_empty();
            let brightness = brightness.or_else(|| displays.first().map(|display| display.level));
            if qs.displays != displays {
                qs.displays = displays;
            }
            brightness
        }
        None => brightness,
    };
    if let Some(level) = brightness {
        qs.brightness = level;
    }
}

pub(crate) fn wifi_networks(qs: &mut QuickSettingsState, networks: Vec<qs::WifiNetworkInfo>) {
    qs.wifi_networks = networks
        .into_iter()
        .map(|n| WifiNetwork {
            ssid: n.ssid,
            signal: n.signal,
            secure: n.secure,
            security: n.security,
            connected: n.connected,
            known: n.known,
        })
        .collect();
}

pub(crate) fn saved_networks(qs: &mut QuickSettingsState, saved: Vec<qs::SavedNetworkProfile>) {
    qs.saved_networks = saved
        .into_iter()
        .map(|profile| crate::SavedNetwork {
            name: profile.name,
            profile: profile.uuid,
            autoconnect: profile.autoconnect,
        })
        .collect();
}

pub(crate) fn bluetooth(qs: &mut QuickSettingsState, reading: BluetoothReading) {
    if let Some(devices) = reading.paired {
        qs.bt_devices = devices
            .into_iter()
            .map(|d| BtDevice {
                name: d.name,
                address: d.mac,
                battery: d.battery,
                connected: d.connected,
            })
            .collect();
    }
    qs.bt_nearby = reading
        .nearby
        .into_iter()
        .map(|d| BtNearby {
            name: d.name,
            address: d.address,
            signal: d.rssi,
        })
        .collect();
    if let Some(scanning) = reading.scanning {
        qs.bt_scanning = scanning;
    }
}

/// Land a join: the network joined, or why not — in the password prompt
/// when one is up, a prompt asking again for a protected network whose
/// saved password was turned down, else a notification (its title and
/// body, returned).
pub(crate) fn wifi_join(
    qs: &mut QuickSettingsState,
    join: WifiJoinResult,
) -> Option<(String, String)> {
    let WifiJoinResult {
        ssid,
        password,
        result,
    } = join;
    qs.wifi_joining = None;
    match result {
        Ok(()) => {
            qs.wifi_password = None;
            qs.wifi_hidden = None;
            for network in &mut qs.wifi_networks {
                network.connected = network.ssid == ssid;
            }
            qs.wifi = true;
            let detail = if password.is_some() {
                t("host-wifi-connected-saved")
            } else {
                t("host-wifi-connected-open")
            };
            Some((
                t_args(
                    "host-wifi-connected-title",
                    &[("ssid", ssid.as_str().into())],
                ),
                detail,
            ))
        }
        Err(error) => {
            tracing::warn!(target: "tilo::os_actions", op = "connect_wifi", %error, "wifi join failed");
            if let Some(prompt) = qs
                .wifi_password
                .as_mut()
                .filter(|prompt| prompt.ssid == ssid)
            {
                prompt.connecting = false;
                // Said in the prompt, under its field.
                prompt.error = Some(t("host-wifi-join-check-password"));
                None
            } else if let Some(prompt) =
                qs.wifi_hidden.as_mut().filter(|prompt| prompt.ssid == ssid)
            {
                prompt.connecting = false;
                // Said in the form, under its fields.
                prompt.error = Some(t("host-wifi-join-check-password"));
                None
            } else if qs
                .wifi_networks
                .iter()
                .any(|network| network.ssid == ssid && network.secure)
            {
                // A protected network that turned its saved password down
                // asks for it, in place.
                let mut prompt = crate::WifiPasswordPrompt::new(ssid);
                prompt.error = Some(t("host-wifi-join-check-password"));
                qs.wifi_password = Some(prompt);
                None
            } else {
                Some((
                    t_args(
                        "host-wifi-connect-failed-title",
                        &[("ssid", ssid.as_str().into())],
                    ),
                    t("host-wifi-connect-failed-body"),
                ))
            }
        }
    }
}

/// How far Louder and Quieter move the volume.
pub(crate) const VOLUME_STEP: f64 = 0.05;
/// How far Brighter and Dimmer move the brightness.
pub(crate) const BRIGHTNESS_STEP: f64 = 0.1;

/// A level the panel sets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LevelChange {
    /// The one brightness, `0..=1`.
    Brightness(f64),
    /// One display's brightness, by connector.
    Display(String, f64),
    Volume(f64),
}

/// The volume `step` from the level it is at, as Louder and Quieter ask.
pub(crate) fn volume_step(qs: &QuickSettingsState, step: f64) -> LevelChange {
    LevelChange::Volume((f64::from(qs.volume_level()) + step).clamp(0.0, 1.0))
}

/// The brightness `step` away, as Brighter and Dimmer ask: from the level
/// of the display the person works on, when the desktop spans several that
/// can be dimmed, else from the one brightness.
pub(crate) fn brightness_step(qs: &QuickSettingsState, step: f64) -> LevelChange {
    let moved = |level: f32| (f64::from(level) + step).clamp(0.0, 1.0);
    let focused = qs.focused_display.as_deref().and_then(|focused| {
        qs.displays
            .iter()
            .find(|display| display.connector == focused)
    });
    match focused {
        Some(display) => LevelChange::Display(display.connector.clone(), moved(display.level)),
        None => LevelChange::Brightness(moved(qs.brightness)),
    }
}

/// A level as the readings keep it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a level in 0..=1 kept at f32 precision"
)]
pub(crate) fn level_of(level: f64) -> f32 {
    level.clamp(0.0, 1.0) as f32
}

/// The slider shows where it was let go until the device answers.
pub(crate) fn show_level(qs: &mut QuickSettingsState, change: &LevelChange) {
    match change {
        LevelChange::Brightness(level) => qs.brightness = level_of(*level),
        LevelChange::Display(connector, level) => {
            if let Some(display) = qs
                .displays
                .iter_mut()
                .find(|display| display.connector == *connector)
            {
                display.level = level_of(*level);
            }
        }
        LevelChange::Volume(level) => qs.set_volume_level(level_of(*level)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_move_the_display_in_use_and_stay_in_range() {
        let mut qs = QuickSettingsState::fixture();
        qs.brightness = 0.95;
        assert_eq!(
            brightness_step(&qs, BRIGHTNESS_STEP),
            LevelChange::Brightness(1.0)
        );
        qs.displays = vec![
            DisplayBrightness {
                connector: "eDP-1".into(),
                name: "Built-in display".into(),
                level: 0.5,
            },
            DisplayBrightness {
                connector: "DP-2".into(),
                name: "Dell".into(),
                level: 0.25,
            },
        ];
        qs.focused_display = Some("DP-2".into());
        assert_eq!(
            brightness_step(&qs, -BRIGHTNESS_STEP),
            LevelChange::Display("DP-2".into(), 0.25 - BRIGHTNESS_STEP)
        );
        qs.set_volume_level(0.02);
        assert_eq!(volume_step(&qs, -VOLUME_STEP), LevelChange::Volume(0.0));
    }

    #[test]
    fn a_turned_down_password_asks_again_in_place() {
        let mut qs = QuickSettingsState::fixture();
        qs.wifi_networks = vec![WifiNetwork {
            ssid: "Home".into(),
            signal: 3,
            secure: true,
            security: qs::WifiSecurity::default(),
            connected: false,
            known: true,
        }];
        let told = wifi_join(
            &mut qs,
            WifiJoinResult {
                ssid: "Home".into(),
                password: None,
                result: Err("secrets were required".into()),
            },
        );
        assert_eq!(told, None, "said in the prompt, not a notification");
        let prompt = qs.wifi_password.clone().expect("asked again");
        assert_eq!(prompt.ssid, "Home");
        assert!(prompt.error.is_some());

        let told = wifi_join(
            &mut qs,
            WifiJoinResult {
                ssid: "Home".into(),
                password: Some("secret".into()),
                result: Ok(()),
            },
        );
        assert_eq!(
            told.map(|(title, _)| title).as_deref(),
            Some("Connected to Home")
        );
        assert!(qs.wifi_password.is_none());
        assert!(qs.wifi_networks[0].connected);
    }
}
