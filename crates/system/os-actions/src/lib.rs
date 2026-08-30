//! OS-level action wrappers (brightness, audio, Wi-Fi, Bluetooth, power-profile) shared
//! between the qOS compositor (`shell`) and the standalone Settings app (`apps/settings`).
//! Both needed the same brightnessctl/wpctl/nmcli/bluetoothctl/powerprofilesctl bindings, and
//! the Settings app must not depend on the compositor (they're peers, not parent/child), so
//! the bindings live here as a single source of truth for both.
//!
//! Design notes:
//!
//! - Every helper checks the binary's presence first via [`which`] and returns a clear
//!   `"<binary> not installed"` error otherwise, so the QS panel and Settings app stay
//!   functional on machines missing a daemon (e.g. no `power-profiles-daemon`).
//! - [`std::process::Command`] (sync) is deliberate: both call sites are user-driven (slider
//!   drag, toggle click), so getter calls happen only on rising-edge open — no tokio needed.
//! - Parsers are extracted into private free functions so they're unit-testable against
//!   sample stdout without spawning processes.
//!
//! ## Dispatcher trait
//!
//! Bluetooth is the exception to the CLI wrappers: it uses the persistent
//! `qos-bluez` D-Bus client because discovery and pairing are connection-bound.
//!
//! [`Dispatcher`] is the side-effect interface used by the click handlers. The default
//! production impl ([`RealDispatcher`]) calls the free functions in the per-domain modules;
//! tests substitute a recording impl so the click handler can be asserted on without
//! shelling out. The shell crate re-exports the trait as `QsActions` and the impl as
//! `RealQsActions` for source compatibility.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

pub mod audio;
pub mod battery;
pub mod bluetooth;
pub mod bluez;
pub mod display;
pub mod firmware;
pub mod modem;
pub mod network;
pub mod power;
pub mod printing;
pub mod privacy;
mod process;
pub mod session;
pub mod time;
pub mod tool_intent;
pub mod users;

pub use audio::{
    VolumeState, get_source_state, get_volume, get_volume_state, set_mute, set_volume, toggle_mute,
    toggle_source_mute,
};
pub use battery::{
    BatteryHistory, BatteryHistoryPoint, BatteryInfo, ChargeCapBackend, ChargeCapCapability,
    PowerDevice, PowerSupplyKind, charge_capability, load_battery_history, power_devices,
    set_charge_limit, set_charge_limit_checked,
};
pub use bluetooth::{
    BtDeviceInfo, bt_connect, bt_disconnect, get_bluetooth, list_bt_devices, set_bluetooth,
};
pub use display::{
    AmbientBrightnessController, ambient_lux, get_brightness, has_ambient_light_sensor,
    set_brightness,
};
pub use firmware::{
    FirmwareEvent, FirmwareHistoryEntry, FirmwareInstallOutcome, FirmwareUpdate,
    get_firmware_history, get_firmware_updates, refresh_firmware_metadata, start_firmware_install,
};
pub use modem::{SimInfo, list_mobile_sims, unlock_mobile_sim, unlock_mobile_sim_on};
pub use network::{
    CertificateInfo, ConnectionDiagnostics, ConnectionPolicy, ConnectionPolicyPatch,
    ConnectionProfileSettings, ConnectivityState, EnterpriseEapMethod, EnterpriseWifiConfiguration,
    HOTSPOT_PROFILE, HotspotBand, HotspotClient, HotspotConfiguration, IpConfiguration, LinkKind,
    LinkStatus, MacPolicy, MeteredPolicy, ProxyConfiguration, RadioState, SavedNetworkProfile,
    VpnConnectionInfo, WifiNetworkInfo, WiredInfo, captive_portal_uri, configure_connection_policy,
    configure_connection_policy_patch, configure_hotspot, configure_ipv4, connect_enterprise_wifi,
    connect_wifi, connection_diagnostics, connection_profile_settings, create_mobile_profile,
    forget_network_profile, get_connectivity, get_link_status, get_radio_state, get_wifi,
    get_wired, import_vpn_config, inspect_certificate, list_hotspot_clients,
    list_saved_network_profiles, list_vpn_connections, list_wifi_networks, set_airplane_mode,
    set_hotspot, set_mobile_broadband, set_network_autoconnect, set_vpn_connection, set_wifi,
    set_wired,
};
pub use power::{PowerProfile, available_power_profiles, get_power_profile, set_power_profile};
pub use printing::{DiscoveredPrinter, PrintJob, PrinterQueue, QueueState, Scanner};
pub use privacy::{
    ActiveMediaUse, KillState, MediaKind, PrivacyStatus, active_media_uses, get_privacy_status,
    monitor_media_usage, revoke_media_client,
};
pub use session::{SessionAction, perform_session_action};
pub use time::{
    TimeSyncStatus, get_ntp, get_time_sync_status, list_timezones, now_clock, now_date, set_ntp,
    set_timezone, valid_timezone,
};
pub use tool_intent::{
    BackupsRoute, DisksRoute, FirewallRoute, TOOL_CONTEXT_ENV, TOOL_PROTOCOL_VERSION, ToolIntent,
    ToolReply, ToolRequest, VpnRoute, open_tool, tool_intent_from_env,
};
pub use users::AccountRole;

use std::time::Duration;

use tracing::warn;

/// Side-effect interface for the OS actions. The default production impl
/// ([`RealDispatcher`]) calls into the free functions in the per-domain modules; tests
/// substitute a recording impl so the click handler can be asserted on without shelling out.
/// (Shell call sites historically named this trait `QsActions`; a type-alias preserves it.)
pub trait Dispatcher {
    fn set_brightness(&self, pct: f32) -> Result<(), String>;
    fn set_volume(&self, pct: f32) -> Result<(), String>;
    fn set_wifi(&self, enabled: bool) -> Result<(), String>;
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String>;
    fn set_wired(&self, enabled: bool) -> Result<(), String> {
        let _ = enabled;
        Err("wired networking is not supported by this dispatcher".into())
    }
    fn set_airplane_mode(&self, enabled: bool) -> Result<(), String> {
        network::set_airplane_mode(enabled)
    }
    fn set_hotspot(&self, enabled: bool) -> Result<(), String> {
        network::set_hotspot(enabled)
    }
    fn set_mobile_broadband(&self, enabled: bool) -> Result<(), String> {
        network::set_mobile_broadband(enabled)
    }
    fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
        network::set_vpn_connection(name, enabled)
    }
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String>;
    fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String>;
    /// Toggle the default audio sink mute state. Default impl delegates to
    /// [`audio::toggle_mute`]; has a default body so existing shell-crate impls keep compiling.
    fn toggle_mute(&self) -> Result<(), String> {
        audio::toggle_mute()
    }
    /// Set the sink mute state explicitly. Distinct from [`Self::toggle_mute`] because the
    /// volume-up key needs to *clear* mute regardless of prior state — toggling could silence an
    /// already-audible sink. Default body, same reason as above.
    fn set_mute(&self, muted: bool) -> Result<(), String> {
        audio::set_mute(muted)
    }
}

/// Production impl — every method forwards to the free shell-out helper of the same name.
/// Stateless; cheap to construct (zero-sized struct).
#[derive(Default, Clone, Copy, Debug)]
pub struct RealDispatcher;

impl Dispatcher for RealDispatcher {
    fn set_brightness(&self, pct: f32) -> Result<(), String> {
        display::set_brightness(pct)
    }
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        audio::set_volume(pct)
    }
    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        network::set_wifi(enabled)
    }
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        bluez::set_powered(enabled)
    }
    fn set_wired(&self, enabled: bool) -> Result<(), String> {
        network::set_wired(enabled)
    }
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        network::connect_wifi(ssid, password)
    }
    fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
        power::set_power_profile(profile)
    }
    fn toggle_mute(&self) -> Result<(), String> {
        audio::toggle_mute()
    }
}

/// Wrapper around a `Result<(), String>` that funnels the error into `tracing::warn!` with a
/// stable prefix, so a click-handler stays a one-liner:
///
/// ```ignore
/// log_result("set_brightness", os_actions::set_brightness(value));
/// ```
pub fn log_result(op: &str, result: Result<(), String>) {
    if let Err(e) = result {
        warn!(target: "qos::os_actions", op = op, error = %e, "os action failed");
    }
}

/// Same as [`log_result`] but for getter-shaped results that return a value alongside the error.
pub fn log_get<T>(op: &str, result: Result<T, String>) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(target: "qos::os_actions", op = op, error = %e, "os get failed");
            None
        }
    }
}

/// Deadline enforced on every shell-out in this crate, via [`process::run_capture`]. A tool
/// that exceeds it is killed and reported as a timeout, so a wedged daemon degrades to a
/// stale reading instead of blocking its caller indefinitely.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2_500);

#[cfg(test)]
mod dispatcher_tests {
    use super::*;
    use std::cell::RefCell;

    /// Recording dispatcher used by the dispatcher-level tests. Captures every call into a
    /// `Vec<String>` so the test can assert the click translated into the right shell-out.
    #[derive(Default)]
    struct RecordingDispatcher {
        calls: RefCell<Vec<String>>,
    }

    impl RecordingDispatcher {
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Dispatcher for RecordingDispatcher {
        fn set_brightness(&self, pct: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_brightness({pct:.2})"));
            Ok(())
        }
        fn set_volume(&self, pct: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_volume({pct:.2})"));
            Ok(())
        }
        fn set_wifi(&self, enabled: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("set_wifi({enabled})"));
            Ok(())
        }
        fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_bluetooth({enabled})"));
            Ok(())
        }
        fn set_wired(&self, enabled: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_wired({enabled})"));
            Ok(())
        }
        fn connect_wifi(&self, ssid: &str, _password: Option<&str>) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("connect_wifi({ssid})"));
            Ok(())
        }
        fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_power_profile({})", profile.cli_arg()));
            Ok(())
        }
        fn toggle_mute(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("toggle_mute()".to_string());
            Ok(())
        }
    }

    #[test]
    fn dispatcher_records_calls() {
        let d = RecordingDispatcher::default();
        d.set_brightness(60.0).unwrap();
        d.set_volume(75.0).unwrap();
        d.set_wifi(false).unwrap();
        d.set_bluetooth(true).unwrap();
        d.set_wired(true).unwrap();
        d.connect_wifi("MyNet", Some("hunter2")).unwrap();
        d.set_power_profile(PowerProfile::Performance).unwrap();
        d.toggle_mute().unwrap();

        let calls = d.calls();
        assert_eq!(calls.len(), 8);
        assert_eq!(calls[0], "set_brightness(60.00)");
        assert_eq!(calls[1], "set_volume(75.00)");
        assert_eq!(calls[2], "set_wifi(false)");
        assert_eq!(calls[3], "set_bluetooth(true)");
        assert_eq!(calls[4], "set_wired(true)");
        assert_eq!(calls[5], "connect_wifi(MyNet)");
        assert_eq!(calls[6], "set_power_profile(performance)");
        assert_eq!(calls[7], "toggle_mute()");
    }
}
