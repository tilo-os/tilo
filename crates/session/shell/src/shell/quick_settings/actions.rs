//! Quick Settings' dispatcher aliases and fixture implementation.
//!
//! Hardware reads and writes live in `os-actions`, which is shared with the
//! Settings app. This module retains only the names and unavailable dispatcher
//! that belong to shell composition.

pub use os_actions::{PowerProfile, PrivacyStatus, WiredInfo};

// Historical names; `os-actions` itself uses the neutral `Dispatcher`.
pub use os_actions::Dispatcher as QsActions;
pub use os_actions::RealDispatcher as RealQsActions;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag independently records whether one Quick Settings backend is present"
)]
pub(crate) struct QsActionAvailability {
    pub(crate) brightness: bool,
    pub(crate) volume: bool,
    pub(crate) wifi: bool,
    pub(crate) power_profile: bool,
    pub(crate) vpn: bool,
    pub(crate) bluetooth: bool,
    pub(crate) airplane_mode: bool,
}

impl QsActionAvailability {
    pub(crate) const fn all() -> Self {
        Self {
            brightness: true,
            volume: true,
            wifi: true,
            power_profile: true,
            vpn: true,
            bluetooth: true,
            airplane_mode: true,
        }
    }

    pub(crate) fn detect() -> Self {
        #[cfg(target_os = "linux")]
        {
            let mut availability = Self::from_probe(command_on_path);
            // Tools on PATH are not hardware. A VM ships nmcli, brightnessctl
            // and bluetoothctl with no backlight, Wi-Fi radio or adapter; the
            // actions must then be unavailable rather than a working-looking
            // toggle that fails or no-ops.
            availability.brightness = os_actions::display::has_backlight();
            if let Ok(radios) = os_actions::get_radio_state() {
                availability.wifi = radios.wifi_available;
                availability.airplane_mode = radios.wifi_available || radios.wwan_available;
            } else {
                availability.wifi = false;
                availability.airplane_mode = false;
            }
            availability.bluetooth = os_actions::bluez::has_adapter();
            availability
        }
        #[cfg(not(target_os = "linux"))]
        Self::default()
    }

    #[cfg(any(test, target_os = "linux"))]
    fn from_probe(mut present: impl FnMut(&str) -> bool) -> Self {
        let network_manager = present("nmcli");
        Self {
            brightness: present("brightnessctl"),
            volume: present("wpctl"),
            wifi: network_manager,
            power_profile: present("powerprofilesctl"),
            vpn: network_manager,
            // The radio is BlueZ's; its client ships with the daemon.
            bluetooth: present("bluetoothctl"),
            airplane_mode: network_manager,
        }
    }
}

#[cfg(target_os = "linux")]
fn command_on_path(command: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| {
        std::fs::metadata(directory.join(command))
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}

/// Historical name for [`os_actions::log_result`]; new code should call it directly.
pub fn log_qs_result(op: &str, result: Result<(), String>) {
    os_actions::log_result(op, result);
}

/// No hardware service is attached to an explicit fixture.
pub(crate) struct UnavailableQsActions;

impl QsActions for UnavailableQsActions {
    fn set_brightness(&self, _pct: f32) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_volume(&self, _pct: f32) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_wifi(&self, _enabled: bool) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_bluetooth(&self, _enabled: bool) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn connect_bluetooth(&self, _address: &str) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn disconnect_bluetooth(&self, _address: &str) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_wired(&self, _enabled: bool) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_airplane_mode(&self, _enabled: bool) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_hotspot(&self, _enabled: bool) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_mobile_broadband(&self, _enabled: bool) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_vpn_connection(&self, _name: &str, _enabled: bool) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn connect_wifi(&self, _ssid: &str, _password: Option<&str>) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_power_profile(&self, _profile: PowerProfile) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn toggle_mute(&self) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_mute(&self, _muted: bool) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn brightness(&self) -> Result<f32, String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn keyboard_backlight(&self) -> Result<os_actions::KeyboardBacklight, String> {
        Err("Quick Settings service is unavailable".into())
    }
    fn set_keyboard_backlight(&self, _level: u32) -> Result<(), String> {
        Err("Quick Settings service is unavailable".into())
    }
}

#[cfg(test)]
mod availability_tests {
    use super::QsActionAvailability;

    #[test]
    fn quick_settings_mechanisms_are_probed_independently_and_absent_when_unsupported() {
        fn mechanisms_are_probed_independently_before_discovery() {
            let found =
                QsActionAvailability::from_probe(|command| matches!(command, "wpctl" | "nmcli"));
            assert!(!found.brightness);
            assert!(found.volume);
            assert!(found.wifi);
            assert!(!found.power_profile);
            assert!(found.vpn);
            assert!(!found.bluetooth);
            assert!(found.airplane_mode);
        }

        #[cfg(not(target_os = "linux"))]
        fn unsupported_host_advertises_no_quick_settings_mechanism() {
            assert_eq!(
                QsActionAvailability::detect(),
                QsActionAvailability::default()
            );
        }

        mechanisms_are_probed_independently_before_discovery();
        #[cfg(not(target_os = "linux"))]
        unsupported_host_advertises_no_quick_settings_mechanism();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detect_uses_hardware_not_tools_on_the_path() {
        let found = QsActionAvailability::detect();
        assert_eq!(
            found.brightness,
            os_actions::display::has_backlight(),
            "brightnessctl on PATH must not advertise a backlight this machine does not have"
        );
        let wifi = os_actions::get_radio_state()
            .ok()
            .is_some_and(|radios| radios.wifi_available);
        assert_eq!(found.wifi, wifi);
        assert_eq!(found.bluetooth, os_actions::bluez::has_adapter());
    }
}
