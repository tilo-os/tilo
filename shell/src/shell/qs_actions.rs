//! Re-export shim for the shared `os-actions` crate.
//!
//! The brightnessctl/wpctl/nmcli/bluetoothctl/powerprofilesctl bindings
//! used to live here. They moved to `os-actions` so the standalone
//! Settings app can drive the same hardware without depending on the
//! compositor — see `os-actions/src/lib.rs` for the design notes.
//!
//! This module continues to expose the historical names
//! (`qs_actions::set_brightness`, `qs_actions::QsActions`,
//! `qs_actions::RealQsActions`) so existing shell call-sites and the
//! `shell/tests/qs_actions_click_routing.rs` integration test keep
//! compiling without rename churn. New code should import from
//! `os_actions::*` directly.

// Re-export the free shell-out helpers.
pub use os_actions::{
    connect_wifi, get_bluetooth, get_brightness, get_power_profile, get_volume, get_wifi,
    list_bt_devices, list_wifi_networks, log_get, log_result, set_bluetooth, set_brightness,
    set_mute, set_power_profile, set_volume, set_wifi, toggle_mute, BtDeviceInfo, PowerProfile,
    WifiNetworkInfo, DEFAULT_TIMEOUT,
};

// The trait carries the historical `QsActions` name in the shell crate
// (the standalone `os-actions` crate uses the more neutral `Dispatcher`
// name). The production impl is re-aliased as `RealQsActions` for the
// same reason.
pub use os_actions::Dispatcher as QsActions;
pub use os_actions::RealDispatcher as RealQsActions;

/// Shim around [`os_actions::log_result`] kept under the historical
/// `log_qs_result` name. Existing shell call-sites use this in the
/// quick-settings click handlers; new code should call `log_result`
/// directly.
pub fn log_qs_result(op: &str, result: Result<(), String>) {
    os_actions::log_result(op, result);
}

/// Same shim for the getter-shaped logger.
pub fn log_qs_get<T>(op: &str, result: Result<T, String>) -> Option<T> {
    os_actions::log_get(op, result)
}
