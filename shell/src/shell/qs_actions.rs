//! Re-export shim for the shared `os-actions` crate — the
//! brightnessctl/wpctl/nmcli/bluetoothctl/powerprofilesctl bindings live
//! there so the standalone Settings app can drive the same hardware
//! without depending on the compositor. Keeps the historical
//! `qs_actions::*` names so existing call-sites compile without rename
//! churn; new code should import `os_actions::*` directly.

pub use os_actions::{
    BatteryInfo, BtDeviceInfo, DEFAULT_TIMEOUT, FirmwareUpdate, PowerProfile, PrivacyStatus,
    RadioState, VpnConnectionInfo, WifiNetworkInfo, WiredInfo, connect_wifi, get_bluetooth,
    get_brightness, get_firmware_updates, get_power_profile, get_privacy_status, get_radio_state,
    get_volume, get_wifi, get_wired, list_bt_devices, list_vpn_connections, list_wifi_networks,
    log_get, log_result, set_airplane_mode, set_bluetooth, set_brightness, set_hotspot,
    set_mobile_broadband, set_mute, set_power_profile, set_volume, set_vpn_connection, set_wifi,
    set_wired, toggle_mute,
};

// Historical names; `os-actions` itself uses the neutral `Dispatcher`.
pub use os_actions::Dispatcher as QsActions;
pub use os_actions::RealDispatcher as RealQsActions;

/// Historical name for [`os_actions::log_result`]; new code should call it directly.
pub fn log_qs_result(op: &str, result: Result<(), String>) {
    os_actions::log_result(op, result);
}

/// Same shim for the getter-shaped logger.
pub fn log_qs_get<T>(op: &str, result: Result<T, String>) -> Option<T> {
    os_actions::log_get(op, result)
}
