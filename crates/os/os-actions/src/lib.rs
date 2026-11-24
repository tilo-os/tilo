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
pub use modem::{
    Availability, CodeAnswer, MobileProfile, MobileReading, ModemChange, ModemError, ModemFailure,
    ModemInfo, ModemState, NetworkModes, Operator, Registration, SimCard, SimKind, SimLock,
    Technology, Traffic, UnlockRetries, answer_code, apply_modem_change, cancel_code, read_mobile,
    read_mobile_profile, scan_operators, send_code, set_mobile_apn, set_mobile_data,
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
    set_hotspot, set_mobile_broadband, set_network_autoconnect, set_open_wifi_autoconnect,
    set_vpn_connection, set_vpn_connection_uuid, set_wifi, set_wifi_radio, set_wired,
    wifi_change_outcome, wifi_from_radios, wireless_link_quality_pct,
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

use tracing::warn;

/// The system bus this session may ask, or the reason it may not: the machine's daemons are its
/// own only when it owns the machine ([`tilo_paths::machine`]). Every call here that reaches the
/// system bus goes through this, so a session that runs inside another desktop reaches no daemon of
/// it and answers as if none were installed.
pub(crate) async fn system_bus() -> Result<zbus::Connection, String> {
    if !tilo_paths::machine::owns_the_machine() {
        return Err(
            "this session reaches no machine: its daemons belong to the desktop it runs in".into(),
        );
    }
    zbus::Connection::system()
        .await
        .map_err(|error| format!("cannot reach the system bus: {error}"))
}

/// Drives one system-service call to completion for a synchronous caller: the runtime thread polls the
/// bus while the caller waits.
pub(crate) fn block_on<T>(
    call: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tilo_rt::try_app()
        .and_then(|runtime| runtime.block_on(call))
        .map_err(|error| error.to_string())?
}

/// A change to the radios that is checked afterwards: a switch may hold a radio off although the
/// change was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioChange {
    Wifi(bool),
    Airplane(bool),
}

impl RadioChange {
    /// Makes the change and reads the radios back.
    pub fn apply(self) -> Result<RadioState, String> {
        match self {
            Self::Wifi(enabled) => network::set_wifi_radio(enabled),
            Self::Airplane(enabled) => {
                network::set_airplane_mode(enabled)?;
                network::get_radio_state()
            }
        }
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
pub use bounded_command::command::DEFAULT_TIMEOUT;
/// The typed CUPS vocabulary, published here as it was: the print engine links `tilo-printing`
/// itself and takes none of this crate's device control with it.
pub use tilo_printing::printing;
pub use tilo_printing::printing::{DiscoveredPrinter, PrintJob, PrinterQueue, QueueState};

/// What the live update applied of a staged image, and what still waits (the state
/// `tilo-extension live:system` leaves for the desktop). Settings and the session's system
/// service both read it.
pub const LIVE_UPDATE_STATE: &str = "/run/tilo/live-update/state.json";
