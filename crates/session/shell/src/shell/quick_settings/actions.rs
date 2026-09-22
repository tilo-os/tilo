//! Quick Settings' dispatcher aliases and fixture implementation.
//!
//! Hardware reads and writes live in `os-actions`, which is shared with the
//! Settings app. This module retains only the names and unavailable dispatcher
//! that belong to shell composition.

pub use os_actions::{BatteryInfo, PowerProfile, PrivacyStatus, WiredInfo};

// Historical names; `os-actions` itself uses the neutral `Dispatcher`.
pub use os_actions::Dispatcher as QsActions;
pub use os_actions::RealDispatcher as RealQsActions;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct QsActionAvailability {
    pub(crate) brightness: bool,
    pub(crate) volume: bool,
    pub(crate) wifi: bool,
    pub(crate) power_profile: bool,
    pub(crate) vpn: bool,
}

impl QsActionAvailability {
    pub(crate) const fn all() -> Self {
        Self {
            brightness: true,
            volume: true,
            wifi: true,
            power_profile: true,
            vpn: true,
        }
    }

    pub(crate) fn detect() -> Self {
        #[cfg(target_os = "linux")]
        {
            let mut availability = Self::from_probe(command_on_path);
            // The production brightness path writes the kernel backlight class
            // directly and only falls back to `brightnessctl` on development
            // hosts, so the binary alone is not its capability source.
            availability.brightness |= os_actions::display::has_backlight();
            return availability;
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
}

#[cfg(test)]
mod availability_tests {
    use super::QsActionAvailability;

    #[test]
    fn mechanisms_are_probed_independently_before_discovery() {
        let found =
            QsActionAvailability::from_probe(|command| matches!(command, "wpctl" | "nmcli"));
        assert!(!found.brightness);
        assert!(found.volume);
        assert!(found.wifi);
        assert!(!found.power_profile);
        assert!(found.vpn);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_host_advertises_no_quick_settings_mechanism() {
        assert_eq!(
            QsActionAvailability::detect(),
            QsActionAvailability::default()
        );
    }
}
