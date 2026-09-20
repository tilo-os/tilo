//! Quick Settings' dispatcher aliases and fixture implementation.
//!
//! Hardware reads and writes live in `os-actions`, which is shared with the
//! Settings app. This module retains only the names and unavailable dispatcher
//! that belong to shell composition.

pub use os_actions::{BatteryInfo, PowerProfile, PrivacyStatus, WiredInfo};

// Historical names; `os-actions` itself uses the neutral `Dispatcher`.
pub use os_actions::Dispatcher as QsActions;
pub use os_actions::RealDispatcher as RealQsActions;

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
