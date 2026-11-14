//! OS-level actions (brightness, audio, radios, power) for the compositor and the
//! Settings app, which both drive the same hardware and do not depend on each other. A
//! control with a D-Bus owner goes through it; the rest shell out to a tool, which is
//! checked to exist first so a machine missing one still works.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod backlight_events;
pub mod battery;
pub mod bluez;
pub mod brightness;
pub mod bus_follow;
pub mod codec_pack;
pub mod ddc;
pub mod disk;
pub mod display;
pub mod extension;
pub mod firmware;
pub mod keyboard_backlight;
#[cfg(target_os = "linux")]
mod linux_gamepad_evdev;
#[cfg(target_os = "linux")]
pub mod linux_physical_gamepad;
#[cfg(not(target_os = "linux"))]
#[path = "unsupported_physical_gamepad.rs"]
pub mod linux_physical_gamepad;
pub mod linux_virtual_gamepad;
pub mod modem;
pub mod network;
pub mod network_settings;
pub mod network_wake;
pub mod orientation;
pub mod peripheral_forwarding;
pub mod power;
pub mod printing;
pub mod privacy;
mod process;
pub mod rfkill;
pub mod session;
pub mod time;
pub mod users;
pub mod windows_compat;

pub use battery::{
    BatteryHistory, BatteryHistoryPoint, BatteryInfo, CapacityUnit, ChargeCapBackend,
    ChargeCapCapability, PowerDevice, PowerSupplyKind, charge_capability, load_battery_history,
    power_devices, set_charge_limit, set_charge_limit_checked,
};
pub use brightness::{
    BrightnessKind, BrightnessService, BrightnessTarget, MonitorBrightness, MonitorLevel,
    SOFTWARE_DIM_FLOOR, SessionDisplays, software_luminance, step_together,
};
pub use disk::{DiskCapacity, disk_capacity};
pub use display::{
    AmbientBrightnessController, ambient_lux, get_brightness, has_ambient_light_sensor,
    set_brightness,
};
pub use firmware::{
    FirmwareEvent, FirmwareHistoryEntry, FirmwareInstallOutcome, FirmwareUpdate,
    get_firmware_history, get_firmware_updates, refresh_firmware_metadata, start_firmware_install,
};
pub use keyboard_backlight::{
    KeyboardBacklight, get_keyboard_backlight, has_keyboard_backlight, set_keyboard_backlight,
};
pub use linux_physical_gamepad::{
    LinuxPhysicalGamepadCatalog, LinuxPhysicalGamepadSource, PhysicalGamepadDrain,
    PhysicalGamepadHotplug, PhysicalGamepadId, PhysicalGamepadInfo,
};
pub use linux_virtual_gamepad::{
    LinuxGamepadDevice, LinuxGamepadDeviceProvider, LinuxVirtualGamepadFactory,
    UinputGamepadDeviceProvider,
};
use media::audio::default_node::{self, DefaultNode};
pub use modem::{
    MobileProfile, MobileReading, ModemChange, ModemError, ModemFailure, ModemInfo, ModemState,
    NetworkModes, Registration, SimCard, SimKind, SimLock, Technology, Traffic, UnlockRetries,
    apply_modem_change, read_mobile, read_mobile_profile, set_mobile_apn, set_mobile_data,
    set_mobile_roaming,
};
pub use network::{
    CertificateInfo, ConnectionDiagnostics, ConnectionPolicy, ConnectionPolicyPatch,
    ConnectionProfileSettings, ConnectivityState, EnterpriseEapMethod, EnterpriseWifiConfiguration,
    HOTSPOT_PROFILE, HotspotBand, HotspotClient, HotspotConfiguration, HotspotDetails,
    InterfaceTraffic, IpConfiguration, LinkKind, LinkStatus, MacPolicy, MeteredPolicy,
    ProxyConfiguration, RadioState, SavedNetworkProfile, VpnConnectionInfo, WifiNetworkInfo,
    WifiSecurity, WiredInfo, captive_portal_uri, check_connectivity_now,
    configure_connection_policy, configure_connection_policy_patch, configure_hotspot,
    configure_ipv4, connect_enterprise_wifi, connect_wifi, connection_diagnostics,
    connection_profile_settings, delete_vpn_connection, delete_vpn_connection_uuid,
    disconnect_wifi, forget_network_profile, get_connectivity, get_link_status, get_radio_state,
    get_wifi, get_wired, hotspot_details, import_vpn_config, inspect_certificate,
    link_status_from_state, list_hotspot_clients, list_saved_network_profiles,
    list_vpn_connections, list_wifi_networks, network_service_present,
    portable_wifi_profile_secret, read_interface_traffic, scan_wifi, set_airplane_mode,
    set_hotspot, set_mobile_broadband, set_network_autoconnect, set_vpn_connection,
    set_vpn_connection_uuid, set_wifi, set_wifi_radio, set_wired, wifi_change_outcome,
    wifi_from_radios, wireless_link_quality_pct,
};
pub use orientation::{
    AccelSample, DeviceOrientation, OrientationTracker, RotationControl, has_accelerometer,
};
pub use peripheral_forwarding::{
    AUTHORIZATION_TTL, ForwardedGamepadEvent, ForwardedPeripheralBroker,
    ForwardedPeripheralIdentity, GamepadAxis, GamepadButton, PeripheralAuditEvent,
    PeripheralBrokerError, PeripheralDeviceId, PeripheralLeaseId, PeripheralRevocation,
    VirtualGamepad, VirtualGamepadFactory, VirtualGamepadPlan,
};
pub use power::{PowerProfile, available_power_profiles, get_power_profile, set_power_profile};
pub use printing::{DiscoveredPrinter, PrintJob, PrinterQueue, QueueState};
pub use privacy::{
    ActiveMediaUse, KillState, MediaKind, PrivacyStatus, active_media_uses, get_privacy_status,
    revoke_media_client,
};
pub use runtime_portal::tools::{
    AppEntry, AppKind, AppRemoval, BackupsRoute, ContainerEngine, DisksRoute, DriveEntry,
    FirewallRoute, MAX_LISTED_APPS, MAX_LISTED_DRIVES, MAX_REPLY_BYTES, TOOL_CONTEXT_ENV,
    TOOL_PROTOCOL_VERSION, ToolIntent, ToolReply, ToolRequest, VpnRoute, WorkspaceSettingsRoute,
    invocation_hex, list_drives, open_tool, tool_intent_from_env,
};
pub use session::{
    SessionAction, perform_session_action, request_logout, take_logout_request,
    user_switching_supported,
};
pub use time::{
    TimeSyncStatus, get_ntp, get_time_sync_status, list_timezones, set_ntp, set_timezone,
    valid_timezone,
};
pub use users::AccountRole;

use std::time::Duration;

use tracing::warn;

/// Drives one system-service call to completion for a synchronous caller: the runtime thread polls the
/// bus while the caller waits.
pub(crate) fn block_on<T>(
    call: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tilo_rt::try_app()
        .and_then(|runtime| runtime.block_on(call))
        .map_err(|error| error.to_string())?
}

/// A radio operation prepared on the owning session thread and run by its worker; it carries no
/// borrowed dispatcher or credential.
pub type RadioWork = Box<dyn FnOnce() -> Result<Option<RadioState>, String> + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioChange {
    Wifi(bool),
    Airplane(bool),
}

impl RadioChange {
    pub fn execute(self, dispatcher: &dyn Dispatcher) -> Result<(), String> {
        match self {
            Self::Wifi(enabled) => dispatcher.set_wifi(enabled),
            Self::Airplane(enabled) => dispatcher.set_airplane_mode(enabled),
        }
    }

    pub fn run(self, dispatcher: &dyn Dispatcher) -> Result<Option<RadioState>, String> {
        dispatcher.apply_radio_change(self)
    }

    /// Whether `radios`, read after this change, show it happened; otherwise why not.
    pub fn outcome(self, radios: &RadioState) -> Result<(), String> {
        match self {
            Self::Wifi(enabled) => network::wifi_change_outcome(enabled, radios),
            Self::Airplane(enabled) if radios.airplane_mode == enabled => Ok(()),
            Self::Airplane(true) => Err("a radio stayed on".into()),
            Self::Airplane(false) => network::wifi_change_outcome(true, radios),
        }
    }
}

/// The side effects of the OS actions: the production impl forwards to this crate's functions, tests
/// record what was asked.
pub trait Dispatcher {
    fn set_brightness(&self, pct: f32) -> Result<(), String>;
    /// Sets one monitor, named by its DRM connector (`eDP-1`, `DP-2`), to `pct` percent; applied in the
    /// background, returns once queued.
    fn set_display_brightness(&self, connector: &str, pct: f32) -> Result<(), String> {
        if brightness::BrightnessService::global().set(connector, pct) {
            Ok(())
        } else {
            Err(format!("{connector} has no brightness control"))
        }
    }
    fn set_volume(&self, pct: f32) -> Result<(), String>;
    fn set_wifi(&self, enabled: bool) -> Result<(), String>;
    /// Prepares an owned radio operation without starting it; an impl that only changes radios
    /// synchronously returns `None`.
    fn prepare_radio_change(&self, _change: RadioChange) -> Option<RadioWork> {
        None
    }
    /// Makes a radio change and reads the radios back; `None` when the impl cannot read them, and its
    /// caller does.
    fn apply_radio_change(&self, change: RadioChange) -> Result<Option<RadioState>, String> {
        match change {
            RadioChange::Wifi(enabled) => self.set_wifi(enabled),
            RadioChange::Airplane(enabled) => self.set_airplane_mode(enabled),
        }
        .map(|()| None)
    }

    fn set_bluetooth(&self, enabled: bool) -> Result<(), String>;
    fn connect_bluetooth(&self, address: &str) -> Result<(), String> {
        bluez::connect_address(address)
    }
    fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
        bluez::disconnect_address(address)
    }
    fn scan_bluetooth(&self, on: bool) -> Result<(), String> {
        if on {
            bluez::start_scan()
        } else {
            bluez::stop_scan()
        }
    }
    /// Pairs the device at `address`, trusting and connecting it; what its agent asks to compare is put
    /// to the person.
    fn pair_bluetooth(&self, address: &str) -> Result<(), String> {
        bluez::pair_handoff(address)
    }
    fn forget_bluetooth(&self, address: &str) -> Result<(), String> {
        bluez::forget_address(address)
    }
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
    fn connect_hidden_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        let _ = (ssid, password);
        Err("joining a network with a hidden name is not supported by this dispatcher".into())
    }
    fn forget_network(&self, profile: &str) -> Result<(), String> {
        let _ = profile;
        Err("forgetting saved networks is not supported by this dispatcher".into())
    }
    fn set_network_autoconnect(&self, profile: &str, on: bool) -> Result<(), String> {
        let _ = (profile, on);
        Err("saved network autoconnect is not supported by this dispatcher".into())
    }
    fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String>;
    fn toggle_mute(&self) -> Result<(), String> {
        default_node::toggle_mute(DefaultNode::Sink)
    }
    /// Sets the sink mute state explicitly: the volume-up key must clear mute whatever it was, and a
    /// toggle could silence an audible sink.
    fn set_mute(&self, muted: bool) -> Result<(), String> {
        default_node::set_mute(DefaultNode::Sink, muted)
    }
    fn brightness(&self) -> Result<f32, String> {
        display::get_brightness()
    }
    fn keyboard_backlight(&self) -> Result<KeyboardBacklight, String> {
        keyboard_backlight::get_keyboard_backlight()
    }
    fn set_keyboard_backlight(&self, level: u32) -> Result<(), String> {
        keyboard_backlight::set_keyboard_backlight(level)
    }
}

/// The production dispatcher: every method forwards to this crate's function of the same name.
#[derive(Default, Clone, Copy, Debug)]
pub struct RealDispatcher;

impl Dispatcher for RealDispatcher {
    fn prepare_radio_change(&self, change: RadioChange) -> Option<RadioWork> {
        Some(Box::new(move || Self.apply_radio_change(change)))
    }
    fn apply_radio_change(&self, change: RadioChange) -> Result<Option<RadioState>, String> {
        match change {
            RadioChange::Wifi(enabled) => network::set_wifi_radio(enabled).map(Some),
            RadioChange::Airplane(enabled) => {
                network::set_airplane_mode(enabled)?;
                network::get_radio_state().map(Some)
            }
        }
    }

    fn set_brightness(&self, pct: f32) -> Result<(), String> {
        display::set_brightness(pct)
    }
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        default_node::set_volume(DefaultNode::Sink, pct)
    }
    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        network::set_wifi(enabled)
    }
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        bluez::set_powered(enabled)
    }
    fn connect_bluetooth(&self, address: &str) -> Result<(), String> {
        bluez::connect_address(address)
    }
    fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
        bluez::disconnect_address(address)
    }
    fn set_wired(&self, enabled: bool) -> Result<(), String> {
        network::set_wired(enabled)
    }
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        network::connect_wifi(ssid, password)
    }
    fn connect_hidden_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        network_settings::connect_hidden_wifi(ssid, password)
    }
    fn forget_network(&self, profile: &str) -> Result<(), String> {
        network::forget_network_profile(profile)
    }
    fn set_network_autoconnect(&self, profile: &str, on: bool) -> Result<(), String> {
        network::set_network_autoconnect(profile, on)
    }
    fn set_power_profile(&self, profile: PowerProfile) -> Result<(), String> {
        power::set_power_profile(profile)
    }
    fn toggle_mute(&self) -> Result<(), String> {
        default_node::toggle_mute(DefaultNode::Sink)
    }
}

/// Runs `result`, logging its error at warning level with `op` as prefix.
pub fn log_result(op: &str, result: Result<(), String>) {
    if let Err(e) = result {
        warn!(target: "tilo::os_actions", op = op, error = %e, "os action failed");
    }
}

pub fn log_get<T>(op: &str, result: Result<T, String>) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(target: "tilo::os_actions", op = op, error = %e, "os get failed");
            None
        }
    }
}

/// Applies to every shell-out in this crate: a tool that exceeds it is killed and reported as a
/// timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2_500);

#[cfg(test)]
mod dispatcher_tests {
    use super::*;
    use std::cell::RefCell;

    /// Records every call, so a test asserts what a click sent.
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
