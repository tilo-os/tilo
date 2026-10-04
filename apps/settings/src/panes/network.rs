#![allow(
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    reason = "network commands are closed typed values and remain beside their validation and dispatch"
)]

//! Network pane — Wi-Fi radio toggle, scanned network list, refresh
//! button, connection-status row.
//!
//! Plumbs `os_actions::network::{get_wifi, set_wifi,
//! list_wifi_networks, connect_wifi}`. Every `NetworkManager` query runs
//! on the app runtime's blocking pool, never on the UI thread: the pane
//! appears in a pending state, reads one snapshot on construction and
//! another when the user clicks `Refresh` (or a connection changes), and
//! applies each when it arrives through the wake.
//
// CRDT keys:
//   `network.wifi_enabled` — bool, wifi toggle
//   `network.refresh`      — bool, click sets true; pane resets on
//                             sync after rescan
//   `network.connect.<ssid>` — bool, click sets true; pane resets on
//                             sync after dispatch

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use ui::doc::{read_bool, read_str_or_text};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use os_actions::{
    ConnectivityState, Dispatcher, EnterpriseWifiConfiguration, HotspotBand, HotspotConfiguration,
    IpConfiguration, SavedNetworkProfile, ToolIntent, VpnRoute, WifiNetworkInfo, WiredInfo,
};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, QrWidget, Row, Spacer, Text, TextInput, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use zeroize::{Zeroize, Zeroizing};

use crate::dispatcher::dispatcher_or_real;
use crate::i18n::{t, t_args};
use crate::widgets::{
    El, caption, card, card_with_action, choice_row, field_row, rows, setting_row, value_row,
};

fn connectivity_shown(state: ConnectivityState) -> String {
    t(match state {
        ConnectivityState::Unknown => "net-connectivity-unknown",
        ConnectivityState::Offline => "net-connectivity-offline",
        ConnectivityState::Portal => "net-connectivity-portal",
        ConnectivityState::Limited => "net-connectivity-limited",
        ConnectivityState::Full => "net-connectivity-full",
    })
}

fn band_shown(id: &str) -> String {
    t(match id {
        "2.4 GHz" => "net-band-24",
        "5 GHz" => "net-band-5",
        _ => "net-band-auto",
    })
}

fn band_from_label(label: &str) -> HotspotBand {
    if label == "2.4 GHz" || label == t("net-band-24") {
        HotspotBand::TwoPointFourGhz
    } else if label == "5 GHz" || label == t("net-band-5") {
        HotspotBand::FiveGhz
    } else {
        HotspotBand::Automatic
    }
}

fn metered_id(policy: os_actions::MeteredPolicy) -> &'static str {
    match policy {
        os_actions::MeteredPolicy::Automatic => "Automatic",
        os_actions::MeteredPolicy::Metered => "Metered",
        os_actions::MeteredPolicy::Unmetered => "Unmetered",
    }
}

fn metered_shown(id: &str) -> String {
    t(match id {
        "Metered" => "net-metered-yes",
        "Unmetered" => "net-metered-no",
        _ => "net-metered-auto",
    })
}

fn metered_from_label(label: &str) -> Option<os_actions::MeteredPolicy> {
    if label == "Metered" || label == t("net-metered-yes") {
        Some(os_actions::MeteredPolicy::Metered)
    } else if label == "Unmetered" || label == t("net-metered-no") {
        Some(os_actions::MeteredPolicy::Unmetered)
    } else if label == "Automatic" || label == t("net-metered-auto") {
        Some(os_actions::MeteredPolicy::Automatic)
    } else {
        None
    }
}

fn mac_id(policy: os_actions::MacPolicy) -> &'static str {
    match policy {
        os_actions::MacPolicy::Stable => "Stable per network",
        os_actions::MacPolicy::Random => "Random each connection",
        os_actions::MacPolicy::Permanent => "Permanent hardware address",
    }
}

fn mac_shown(id: &str) -> String {
    t(match id {
        "Random each connection" => "net-mac-random",
        "Permanent hardware address" => "net-mac-permanent",
        _ => "net-mac-stable",
    })
}

fn mac_from_label(label: &str) -> Option<os_actions::MacPolicy> {
    if label == "Random each connection" || label == t("net-mac-random") {
        Some(os_actions::MacPolicy::Random)
    } else if label == "Permanent hardware address" || label == t("net-mac-permanent") {
        Some(os_actions::MacPolicy::Permanent)
    } else if label == "Stable per network" || label == t("net-mac-stable") {
        Some(os_actions::MacPolicy::Stable)
    } else {
        None
    }
}

fn radio_shown(radio: Radio) -> String {
    t(match radio {
        Radio::Airplane => "net-radio-airplane",
        Radio::Hotspot => "net-radio-hotspot",
        Radio::Mobile => "net-radio-mobile",
    })
}

pub const WIFI_ENABLED_KEY: &str = "network.wifi_enabled";
pub const WIRED_ENABLED_KEY: &str = "network.wired_enabled";
pub const REFRESH_KEY: &str = "network.refresh";
pub const AIRPLANE_KEY: &str = "network.airplane_mode";
pub const HOTSPOT_KEY: &str = "network.hotspot";
pub const MOBILE_KEY: &str = "network.mobile_broadband";
pub const HOTSPOT_APPLY_KEY: &str = "network.hotspot.apply";
pub const HOTSPOT_QR_KEY: &str = "network.hotspot.qr";
pub const HOTSPOT_SSID_KEY: &str = "network.hotspot.ssid";
pub const HOTSPOT_BAND_KEY: &str = "network.hotspot.band";
pub const IPV4_AUTOMATIC_KEY: &str = "network.ipv4.automatic";
pub const IPV4_ADDRESS_KEY: &str = "network.ipv4.address";
pub const IPV4_GATEWAY_KEY: &str = "network.ipv4.gateway";
pub const IPV4_DNS_KEY: &str = "network.ipv4.dns";
pub const IPV4_APPLY_KEY: &str = "network.ipv4.apply";
pub const MOBILE_PROFILE_KEY: &str = "network.mobile.profile";
pub const MOBILE_APN_KEY: &str = "network.mobile.apn";
pub const MOBILE_CREATE_KEY: &str = "network.mobile.create";
pub const MOBILE_UNLOCK_KEY: &str = "network.mobile.unlock";
pub const ENTERPRISE_PROFILE_KEY: &str = "network.enterprise.profile";
pub const ENTERPRISE_SSID_KEY: &str = "network.enterprise.ssid";
pub const ENTERPRISE_IDENTITY_KEY: &str = "network.enterprise.identity";
pub const ENTERPRISE_ANONYMOUS_KEY: &str = "network.enterprise.anonymous";
pub const ENTERPRISE_CA_KEY: &str = "network.enterprise.ca";
pub const ENTERPRISE_CONNECT_KEY: &str = "network.enterprise.connect";
/// Opens or closes the task-focused enterprise network setup surface.
pub const ENTERPRISE_EDITOR_KEY: &str = "network.enterprise.editor";
pub const ENTERPRISE_CA_CHOOSE_KEY: &str = "network.enterprise.ca.choose";
pub const ENTERPRISE_METHOD_KEY: &str = "network.enterprise.method";
pub const ENTERPRISE_PHASE2_KEY: &str = "network.enterprise.phase2";
pub const ENTERPRISE_CLIENT_CERT_KEY: &str = "network.enterprise.client_cert";
pub const ENTERPRISE_PRIVATE_KEY: &str = "network.enterprise.private_key";
pub const MOBILE_SIM_KEY: &str = "network.mobile.sim";
pub const DIAGNOSTICS_KEY: &str = "network.diagnostics.run";
pub const ADVANCED_KEY: &str = "network.advanced";
pub const ADVANCED_PROFILE_KEY: &str = "network.advanced.profile";
pub const WIRED_PROFILE_KEY: &str = "network.ipv4.profile";
pub const CAPTIVE_PORTAL_KEY: &str = "network.captive_portal.open";
const MOBILE_PIN_BUFFER: &str = "__mobile_pin";
const ENTERPRISE_PASSWORD_BUFFER: &str = "__enterprise_password";
/// CRDT key holding the SSID whose inline password prompt is expanded
/// (empty string = none). Kept in the document, not a pane field, so the
/// expansion survives the host's structure rebuilds (which reconstruct the
/// pane from scratch).
pub const EXPANDED_KEY: &str = "network.expanded";

/// Build the per-SSID "connect" CRDT key. A click on a network's primary
/// button writes this `true`; the pane reads it in `sync` and either
/// connects directly (open network) or expands an inline password prompt
/// (secure network).
fn connect_key(ssid: &str) -> String {
    format!("network.connect.{ssid}")
}

/// Per-SSID "join with password" CRDT key — written `true` by the Join
/// button in the inline password prompt.
fn join_key(ssid: &str) -> String {
    format!("network.join.{ssid}")
}

fn profile_autoconnect_key(uuid: &str) -> String {
    format!("network.profile.autoconnect.{uuid}")
}

fn profile_forget_key(uuid: &str) -> String {
    format!("network.profile.forget.{uuid}")
}

fn profile_share_key(uuid: &str) -> String {
    format!("network.profile.share.{uuid}")
}

fn ipv4_key(uuid: &str, field: &str) -> String {
    format!("network.ipv4.{field}.{uuid}")
}
fn profile_label(profile: &SavedNetworkProfile) -> String {
    let short_uuid: String = profile.uuid.chars().take(8).collect();
    format!("{} ({})", profile.name, short_uuid)
}

fn load_profile_settings(
    profiles: &[SavedNetworkProfile],
) -> HashMap<String, os_actions::ConnectionProfileSettings> {
    profiles
        .iter()
        .filter_map(|p| {
            os_actions::connection_profile_settings(&p.uuid)
                .ok()
                .map(|s| (p.uuid.clone(), s))
        })
        .collect()
}

/// The word for a 0..=4 bar signal level.
fn wifi_signal_label(signal: u8) -> String {
    t(match signal {
        0 | 1 => "net-signal-weak",
        2 => "net-signal-fair",
        3 => "net-signal-good",
        _ => "net-signal-strong",
    })
}

/// Network-list provider — the prod impl shells out to nmcli, tests
/// supply a stub closure. Boxed so the pane holds it as a trait object;
/// the box satisfies `Send + Sync` because the closure captures nothing
/// (or only thread-safe state).
type NetworkLister = Arc<dyn Fn() -> Result<Vec<WifiNetworkInfo>, String> + Send + Sync>;

/// A configured VPN / `WireGuard` connection and whether it is currently up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VpnInfo {
    pub name: String,
    pub active: bool,
}

/// VPN-list provider — prod shells out to `nmcli`, tests supply a closure.
type VpnLister = Arc<dyn Fn() -> Vec<VpnInfo> + Send + Sync>;
type ToolOpener = Arc<dyn Fn(ToolIntent) -> Result<(), String> + Send + Sync>;

#[derive(Debug)]
enum NetworkJob {
    Wifi(String),
    ShareProfile,
    MobileProfile,
    SimUnlock,
    EnterpriseWifi,
}

enum NetworkJobSuccess {
    Connected { continuity_notice: Option<String> },
    PasswordRequired { continuity_warning: Option<String> },
    ProfileShared { ssid: String },
    OperationCompleted { notice: String },
}

fn portable_security(
    security: os_actions::WifiSecurity,
) -> Option<ui::collaboration::PortableWifiSecurity> {
    match security {
        os_actions::WifiSecurity::Wpa2 => Some(ui::collaboration::PortableWifiSecurity::Wpa2),
        os_actions::WifiSecurity::Wpa3 => Some(ui::collaboration::PortableWifiSecurity::Wpa3),
        _ => None,
    }
}

trait WifiContinuityClient: Send + Sync {
    fn command(
        &self,
        command: ui::collaboration::WifiContinuityCommand,
    ) -> Result<ui::collaboration::WifiContinuityReply, String>;
}

struct ProductionWifiContinuityClient;

impl WifiContinuityClient for ProductionWifiContinuityClient {
    fn command(
        &self,
        command: ui::collaboration::WifiContinuityCommand,
    ) -> Result<ui::collaboration::WifiContinuityReply, String> {
        ui::collaboration::wifi_continuity_command(command)
    }
}

struct EmptyWifiContinuityClient;

impl WifiContinuityClient for EmptyWifiContinuityClient {
    fn command(
        &self,
        command: ui::collaboration::WifiContinuityCommand,
    ) -> Result<ui::collaboration::WifiContinuityReply, String> {
        match command {
            ui::collaboration::WifiContinuityCommand::Lookup { .. } => {
                Ok(ui::collaboration::WifiContinuityReply::NotFound)
            }
            _ => Ok(ui::collaboration::WifiContinuityReply::Rejected {
                reason: "not configured in this test".into(),
            }),
        }
    }
}

fn initial_secret_buffers() -> HashMap<String, Arc<Mutex<String>>> {
    [MOBILE_PIN_BUFFER, ENTERPRISE_PASSWORD_BUFFER]
        .into_iter()
        .map(|key| (key.to_string(), Arc::new(Mutex::new(String::new()))))
        .collect()
}

/// Read the configured VPN connections from `NetworkManager`. Two `nmcli`
/// calls: every connection (to find the VPN/WireGuard ones) and the active
/// set (to mark which are up). Best-effort — an empty vec when `nmcli` is
/// missing.
fn read_vpn_connections() -> Vec<VpnInfo> {
    os_actions::list_vpn_connections()
        .unwrap_or_default()
        .into_iter()
        .map(|v| VpnInfo {
            name: v.name,
            active: v.active,
        })
        .collect()
}

/// Everything the pane shows from `NetworkManager`, read in one pass on the
/// app runtime's blocking pool. `None` means the value was not read and the
/// pane keeps what it has.
struct NetworkSnapshot {
    networks: Result<Vec<WifiNetworkInfo>, String>,
    /// Read only for the first snapshot, which seeds the pane's controls.
    wifi_enabled: Option<bool>,
    /// Read only for the first snapshot.
    sims: Option<Vec<os_actions::SimInfo>>,
    wired: Option<WiredInfo>,
    radios: Option<os_actions::RadioState>,
    connectivity: Option<ConnectivityState>,
    profiles: Option<Vec<SavedNetworkProfile>>,
    profile_settings: HashMap<String, os_actions::ConnectionProfileSettings>,
    hotspot_clients: Option<Vec<os_actions::HotspotClient>>,
    vpns: Option<Vec<VpnInfo>>,
}

impl NetworkSnapshot {
    /// Query `NetworkManager`. Every call here may block on `nmcli` or the
    /// system bus, so this never runs on the UI thread.
    fn read(lister: &NetworkLister, vpn_lister: &VpnLister, initial: bool) -> Self {
        let profiles = os_actions::list_saved_network_profiles().unwrap_or_default();
        Self {
            networks: lister(),
            wifi_enabled: initial.then(|| os_actions::get_wifi().unwrap_or(true)),
            sims: initial.then(|| os_actions::list_mobile_sims().unwrap_or_default()),
            wired: os_actions::get_wired().ok(),
            radios: os_actions::get_radio_state().ok(),
            connectivity: Some(os_actions::get_connectivity().unwrap_or_default()),
            profile_settings: load_profile_settings(&profiles),
            profiles: Some(profiles),
            hotspot_clients: Some(os_actions::list_hotspot_clients().unwrap_or_default()),
            vpns: Some(vpn_lister()),
        }
    }

    /// A query that stopped before reporting anything.
    fn failed(error: String) -> Self {
        Self {
            networks: Err(error),
            wifi_enabled: None,
            sims: None,
            wired: None,
            radios: None,
            connectivity: None,
            profiles: None,
            profile_settings: HashMap::new(),
            hotspot_clients: None,
            vpns: None,
        }
    }
}

/// A radio the pane switches through the dispatcher.
#[derive(Clone, Copy, Debug)]
enum Radio {
    Airplane,
    Hotspot,
    Mobile,
}

impl Radio {
    const fn key(self) -> &'static str {
        match self {
            Self::Airplane => AIRPLANE_KEY,
            Self::Hotspot => HOTSPOT_KEY,
            Self::Mobile => MOBILE_KEY,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Airplane => "airplane",
            Self::Hotspot => "hotspot",
            Self::Mobile => "mobile",
        }
    }

    const fn operation(self, enabled: bool) -> crate::actions::NetworkOperation {
        match self {
            Self::Airplane => crate::actions::NetworkOperation::SetAirplaneMode { enabled },
            Self::Hotspot => crate::actions::NetworkOperation::SetHotspot { enabled },
            Self::Mobile => crate::actions::NetworkOperation::SetMobileBroadband { enabled },
        }
    }
}

/// A finished change and its result, or why it stopped before reporting.
type ActionOutcome = Result<(NetworkAction, Result<(), String>), String>;

/// A `NetworkManager` change requested from the pane. Every one may block
/// on `nmcli` or the system bus for seconds, so they run on the app
/// runtime's blocking pool, one at a time in the order they were asked for.
enum NetworkAction {
    Wifi {
        enabled: bool,
    },
    Wired {
        enabled: bool,
    },
    Radio {
        radio: Radio,
        enabled: bool,
    },
    Vpn {
        name: String,
        enabled: bool,
    },
    Autoconnect {
        uuid: String,
        name: String,
        enabled: bool,
    },
    Forget {
        uuid: String,
        name: String,
    },
    Hotspot {
        config: HotspotConfiguration,
    },
    Ipv4 {
        uuid: String,
        name: String,
        config: IpConfiguration,
    },
    Policy {
        uuid: String,
        name: String,
        patch: os_actions::ConnectionPolicyPatch,
    },
}

impl NetworkAction {
    /// Carry out the change. Never called on the UI thread.
    fn run(&self, dispatcher: &dyn Dispatcher) -> Result<(), String> {
        match self {
            Self::Wifi { enabled } => dispatcher.set_wifi(*enabled),
            Self::Wired { enabled } => dispatcher.set_wired(*enabled),
            Self::Radio { radio, enabled } => match radio {
                Radio::Airplane => dispatcher.set_airplane_mode(*enabled),
                Radio::Hotspot => dispatcher.set_hotspot(*enabled),
                Radio::Mobile => dispatcher.set_mobile_broadband(*enabled),
            },
            Self::Vpn { name, enabled } => dispatcher.set_vpn_connection(name, *enabled),
            Self::Autoconnect { uuid, enabled, .. } => {
                os_actions::set_network_autoconnect(uuid, *enabled)
            }
            Self::Forget { uuid, .. } => os_actions::forget_network_profile(uuid),
            Self::Hotspot { config } => os_actions::configure_hotspot(config),
            Self::Ipv4 { uuid, config, .. } => os_actions::configure_ipv4(uuid, config),
            Self::Policy { uuid, patch, .. } => {
                os_actions::configure_connection_policy_patch(uuid, patch)
            }
        }
    }
}

impl Drop for NetworkAction {
    fn drop(&mut self) {
        if let Self::Hotspot { config } = self {
            config.password.zeroize();
        }
    }
}

/// Parse the two `nmcli -t` outputs into [`VpnInfo`]s. `all` lines are
/// `NAME:TYPE`; a connection is a VPN when its type is `vpn` or
/// `wireguard`. `active` lines are bare `NAME`s of up connections.
pub fn parse_vpn_connections(all: &str, active: &str) -> Vec<VpnInfo> {
    let active_names: Vec<&str> = active
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut out = Vec::new();
    for line in all.lines() {
        // NAME may itself contain no colon in practice; split on the LAST
        // colon so a name is preserved even if it had one (nmcli escapes it,
        // but be defensive).
        let Some((name, ty)) = line.rsplit_once(':') else {
            continue;
        };
        let ty = ty.trim();
        if ty != "vpn" && ty != "wireguard" {
            continue;
        }
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let active = active_names.contains(&name.as_str());
        out.push(VpnInfo { name, active });
    }
    out
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent UI/lifecycle flag (advanced-open, seed-pending, QR-visible, structure-dirty), not a combinatorial state machine"
)]
pub struct NetworkPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    lister: NetworkLister,
    /// Cached current network list — refreshed at construction and on
    /// `Refresh` button press.
    networks: Vec<WifiNetworkInfo>,
    /// Cached wired-connection snapshot, sampled at construction and on
    /// refresh.
    wired: WiredInfo,
    radios: os_actions::RadioState,
    connectivity: ConnectivityState,
    profiles: Vec<SavedNetworkProfile>,
    profile_settings: HashMap<String, os_actions::ConnectionProfileSettings>,
    selected_wifi_profile: Option<String>,
    selected_wired_profile: Option<String>,
    sims: Vec<os_actions::SimInfo>,
    diagnostics: Option<os_actions::ConnectionDiagnostics>,
    advanced_open: bool,
    enterprise_editor_open: bool,
    /// Configured VPN connections (read-only status), sampled at
    /// construction and on refresh.
    vpns: Vec<VpnInfo>,
    vpn_lister: VpnLister,
    tool_opener: ToolOpener,
    initial_wifi_enabled: bool,
    last_wifi_enabled: bool,
    /// Shared only with the Settings operation host; it is process-local and
    /// never written to the CRDT alongside the radio latches below.
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    command_state: ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    local_doc: AutoCommit,
    isolate_document: bool,
    require_operation_token: bool,
    needs_seed: bool,
    /// Track which `network.join.<ssid>` keys we already dispatched
    /// for, so a single button press doesn't re-fire on every frame.
    dispatched_connects: Vec<String>,
    connection_tx: std::sync::mpsc::Sender<(NetworkJob, Result<NetworkJobSuccess, String>)>,
    connection_rx: std::sync::mpsc::Receiver<(NetworkJob, Result<NetworkJobSuccess, String>)>,
    wifi_continuity: Arc<dyn WifiContinuityClient>,
    connection_notice: Option<String>,
    ca_picker_rx: Option<ui::portal::PortalChannel>,
    /// Opening the network sign-in page, run on the app runtime.
    captive_portal: ui::LatestRequest<Result<(), String>>,
    /// A diagnostics read from `NetworkManager`, run on the app runtime.
    diagnostics_request: ui::LatestRequest<Result<os_actions::ConnectionDiagnostics, String>>,
    /// The `NetworkManager` snapshot being read on the app runtime.
    snapshot_request: ui::LatestRequest<NetworkSnapshot>,
    /// False until the first snapshot arrives. Until then the pane shows a
    /// pending view and acts on none of its controls.
    loaded: bool,
    /// A Refresh operation waiting for the next snapshot.
    refresh_pending: bool,
    /// Requested changes not yet started, oldest first.
    actions: std::collections::VecDeque<NetworkAction>,
    /// The change running on the app runtime, with its outcome.
    action_request: ui::LatestRequest<ActionOutcome>,
    hotspot_password: Arc<Mutex<String>>,
    hotspot_ssid: String,
    hotspot_band: String,
    hotspot_qr_visible: bool,
    hotspot_clients: Vec<os_actions::HotspotClient>,
    /// Passwords never enter the Settings CRDT. Each expanded row gets a
    /// process-local buffer which is zeroed immediately after dispatch.
    secret_buffers: HashMap<String, Arc<Mutex<String>>>,
    /// The secure SSID whose inline password prompt is currently expanded,
    /// if any. Mirrors [`EXPANDED_KEY`] in the document; clicking "Connect"
    /// on a secure network opens it; clicking it again, "Join", or a
    /// successful connect closes it.
    expanded_ssid: Option<String>,
    /// Set when `expanded_ssid` changed this `sync`, so the host re-runs
    /// `build_layout` and the inline password prompt appears/disappears.
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for NetworkPane {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NetworkPane {
    fn drop(&mut self) {
        if let Ok(mut password) = self.hotspot_password.lock() {
            password.zeroize();
        }
        for buffer in self.secret_buffers.values() {
            if let Ok(mut secret) = buffer.lock() {
                secret.zeroize();
            }
        }
    }
}

impl NetworkPane {
    /// The production pane. It appears at once in a pending state; the
    /// `NetworkManager` snapshot is read on the app runtime and applied when
    /// it arrives through the wake.
    pub fn new() -> Self {
        let mut pane = Self::from_parts(
            dispatcher_or_real(None),
            Arc::new(os_actions::list_wifi_networks),
            Arc::new(read_vpn_connections),
            Arc::new(ProductionWifiContinuityClient),
            true,
            Vec::new(),
        );
        pane.begin_initial_load();
        pane
    }

    /// Test-only constructor that pins the dispatcher, the wifi-state,
    /// and the network-list source. The list is captured into the
    /// pane's cache immediately.
    pub fn with_dispatcher(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        initial_wifi_enabled: bool,
        networks: Vec<WifiNetworkInfo>,
    ) -> Self {
        let snapshot = networks.clone();
        Self::from_parts(
            dispatcher,
            Arc::new(move || Ok(snapshot.clone())),
            Arc::new(Vec::<VpnInfo>::new),
            Arc::new(EmptyWifiContinuityClient),
            initial_wifi_enabled,
            networks,
        )
    }

    /// `--shots` helper: a pane with a fixed network list and the real
    /// dispatcher (clicks would shell out, but the render never clicks).
    pub fn new_demo(networks: Vec<WifiNetworkInfo>) -> Self {
        let snapshot = networks.clone();
        let mut pane = Self::from_parts(
            dispatcher_or_real(None),
            Arc::new(move || Ok(snapshot.clone())),
            Arc::new(Vec::<VpnInfo>::new),
            Arc::new(ProductionWifiContinuityClient),
            true,
            networks,
        );
        pane.connectivity = ConnectivityState::Full;
        pane
    }

    /// A loaded pane showing `networks` and nothing else read from the host.
    fn from_parts(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        lister: NetworkLister,
        vpn_lister: VpnLister,
        wifi_continuity: Arc<dyn WifiContinuityClient>,
        initial_wifi_enabled: bool,
        networks: Vec<WifiNetworkInfo>,
    ) -> Self {
        let (connection_tx, connection_rx) = std::sync::mpsc::channel();
        Self {
            dispatcher,
            lister,
            networks,
            wired: WiredInfo::default(),
            radios: os_actions::RadioState::default(),
            connectivity: ConnectivityState::Unknown,
            profiles: Vec::new(),
            profile_settings: HashMap::new(),
            selected_wifi_profile: None,
            selected_wired_profile: None,
            sims: Vec::new(),
            diagnostics: None,
            advanced_open: false,
            enterprise_editor_open: false,
            vpns: Vec::new(),
            vpn_lister,
            tool_opener: Arc::new(os_actions::open_tool),
            initial_wifi_enabled,
            last_wifi_enabled: initial_wifi_enabled,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            command_state: ui::LocalState::new(None),
            local_doc: AutoCommit::new(),
            isolate_document: false,
            require_operation_token: false,
            needs_seed: true,
            dispatched_connects: Vec::new(),
            connection_tx,
            connection_rx,
            wifi_continuity,
            connection_notice: None,
            ca_picker_rx: None,
            captive_portal: ui::LatestRequest::new(),
            diagnostics_request: ui::LatestRequest::new(),
            snapshot_request: ui::LatestRequest::new(),
            loaded: true,
            refresh_pending: false,
            actions: std::collections::VecDeque::new(),
            action_request: ui::LatestRequest::new(),
            hotspot_password: Arc::new(Mutex::new(String::new())),
            hotspot_ssid: "Tilo Hotspot".into(),
            hotspot_band: "Automatic".into(),
            hotspot_qr_visible: false,
            hotspot_clients: Vec::new(),
            secret_buffers: initial_secret_buffers(),
            expanded_ssid: None,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Show the pending state and start reading the first snapshot.
    fn begin_initial_load(&mut self) {
        self.loaded = false;
        if !self.start_snapshot() {
            self.connection_notice = Some(t("net-notice-unavailable"));
        }
    }

    /// Start reading a fresh `NetworkManager` snapshot on the app runtime.
    /// A newer read supersedes an older one still running. Returns `false`
    /// when the owning window is closing.
    fn start_snapshot(&mut self) -> bool {
        let lister = Arc::clone(&self.lister);
        let vpn_lister = Arc::clone(&self.vpn_lister);
        let initial = !self.loaded;
        let started = self.snapshot_request.spawn(async move {
            ui::rt::app()
                .spawn_blocking(move || NetworkSnapshot::read(&lister, &vpn_lister, initial))
                .await
                .unwrap_or_else(|error| {
                    NetworkSnapshot::failed(format!("the network query stopped: {error}"))
                })
        });
        self.structure_dirty = true;
        started
    }

    fn sync_snapshot_result(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(snapshot) = self.snapshot_request.try_take() {
            self.apply_snapshot(state, root, snapshot);
        }
    }

    /// Adopt a snapshot. The first one only fills the pane's fields; the
    /// seed that follows writes them into the document. Later ones also
    /// update the document keys whose controls mirror host state, so a
    /// stale key never reads as a request to change that state.
    fn apply_snapshot(&mut self, state: &mut AutoCommit, root: &ObjId, snapshot: NetworkSnapshot) {
        let initial = !self.loaded;
        let outcome = match snapshot.networks {
            Ok(networks) => {
                self.networks = networks;
                Ok(t("net-notice-refreshed"))
            }
            Err(error) => {
                warn!(target: "tilo::settings", op = "list_wifi_networks", %error, "network scan failed");
                Err(t_args("net-notice-refresh-err", &[("error", error.into())]))
            }
        };
        if std::mem::take(&mut self.refresh_pending) {
            self.finish_local_operation(crate::actions::NetworkOperation::Refresh, outcome);
        }
        if self.loaded && self.actions_busy() {
            // Read before a queued change landed; the snapshot taken once
            // the queue drains reports the host state.
            self.structure_dirty = true;
            return;
        }
        if let Some(enabled) = snapshot.wifi_enabled {
            self.initial_wifi_enabled = enabled;
            self.last_wifi_enabled = enabled;
        }
        if let Some(sims) = snapshot.sims {
            self.sims = sims;
        }
        if let Some(wired) = snapshot.wired {
            self.wired = wired;
            if !initial {
                let _ = state.put(root, WIRED_ENABLED_KEY, self.wired.connected);
            }
        }
        if let Some(radios) = snapshot.radios {
            self.radios = radios;
            if !initial {
                for (key, value) in [
                    (AIRPLANE_KEY, self.radios.airplane_mode),
                    (HOTSPOT_KEY, self.radios.hotspot_active),
                    (MOBILE_KEY, self.radios.wwan_enabled),
                ] {
                    let _ = state.put(root, key, value);
                }
            }
        }
        if let Some(connectivity) = snapshot.connectivity {
            self.connectivity = connectivity;
        }
        if let Some(clients) = snapshot.hotspot_clients {
            self.hotspot_clients = clients;
        }
        if let Some(profiles) = snapshot.profiles {
            self.profiles = profiles;
            self.profile_settings = snapshot.profile_settings;
            self.select_available_profiles();
        }
        if let Some(vpns) = snapshot.vpns {
            self.vpns = vpns;
            if !initial {
                for vpn in &self.vpns {
                    let _ = state.put(root, Self::vpn_key(&vpn.name), vpn.active);
                }
            }
        }
        self.loaded = true;
        self.structure_dirty = true;
    }

    const fn radio(&self, radio: Radio) -> bool {
        match radio {
            Radio::Airplane => self.radios.airplane_mode,
            Radio::Hotspot => self.radios.hotspot_active,
            Radio::Mobile => self.radios.wwan_enabled,
        }
    }

    const fn set_radio(&mut self, radio: Radio, enabled: bool) {
        match radio {
            Radio::Airplane => self.radios.airplane_mode = enabled,
            Radio::Hotspot => self.radios.hotspot_active = enabled,
            Radio::Mobile => self.radios.wwan_enabled = enabled,
        }
    }

    /// Whether a requested change is still queued or running.
    fn actions_busy(&self) -> bool {
        !self.actions.is_empty() || self.action_request.is_pending()
    }

    /// Queue a change behind the ones already requested. The pane shows the
    /// requested state at once and reverts it if the change fails.
    fn queue_action(&mut self, action: NetworkAction) {
        self.actions.push_back(action);
        self.start_next_action();
        self.structure_dirty = true;
    }

    /// Start the oldest queued change unless one is already running.
    fn start_next_action(&mut self) {
        if self.action_request.is_pending() {
            return;
        }
        let Some(action) = self.actions.pop_front() else {
            return;
        };
        let dispatcher = Arc::clone(&self.dispatcher);
        let started = self.action_request.spawn(async move {
            ui::rt::app()
                .spawn_blocking(move || {
                    let result = action.run(&*dispatcher);
                    (action, result)
                })
                .await
                .map_err(|error| format!("the network change stopped: {error}"))
        });
        if !started {
            // The window is closing; nothing is left to report to.
            self.actions.clear();
        }
    }

    /// Apply finished changes in order, start the next one, and re-read
    /// `NetworkManager` once the queue has drained.
    fn sync_action_results(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let mut finished = false;
        while let Some(done) = self.action_request.try_take() {
            match done {
                Ok((action, result)) => self.finish_action(state, root, &action, result),
                Err(error) => {
                    warn!(target: "tilo::settings", op = "network_action", %error, "network change stopped");
                    self.connection_notice = Some(t_args(
                        "net-notice-stopped",
                        &[("error", error.clone().into())],
                    ));
                }
            }
            finished = true;
            self.start_next_action();
        }
        if finished && !self.actions_busy() {
            self.start_snapshot();
        }
        if finished {
            self.structure_dirty = true;
        }
    }

    /// Report one finished change, reverting what the pane showed if it
    /// failed.
    fn finish_action(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        action: &NetworkAction,
        result: Result<(), String>,
    ) {
        match action {
            NetworkAction::Wifi { .. }
            | NetworkAction::Wired { .. }
            | NetworkAction::Radio { .. } => self.finish_switch(state, root, action, result),
            _ => self.finish_setting(state, root, action, result),
        }
    }

    /// Report a finished Wi-Fi, wired or radio switch and its operation.
    fn finish_switch(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        action: &NetworkAction,
        result: Result<(), String>,
    ) {
        match (action, result) {
            (NetworkAction::Wifi { enabled }, Ok(())) => self.finish_local_operation(
                crate::actions::NetworkOperation::SetWifiEnabled { enabled: *enabled },
                Ok(if *enabled {
                    t("net-notice-wifi-on")
                } else {
                    t("net-notice-wifi-off")
                }),
            ),
            (NetworkAction::Wifi { enabled }, Err(e)) => {
                self.last_wifi_enabled = !enabled;
                let _ = state.put(root, WIFI_ENABLED_KEY, !enabled);
                self.finish_local_operation(
                    crate::actions::NetworkOperation::SetWifiEnabled { enabled: *enabled },
                    Err(t_args(
                        "net-notice-wifi-err",
                        &[("error", e.clone().into())],
                    )),
                );
                warn!(target: "tilo::settings", op = "set_wifi", error = %e, "network pane dispatch failed");
            }
            (NetworkAction::Wired { enabled }, Ok(())) => self.finish_local_operation(
                crate::actions::NetworkOperation::SetWiredEnabled { enabled: *enabled },
                Ok(if *enabled {
                    t("net-notice-wired-on")
                } else {
                    t("net-notice-wired-off")
                }),
            ),
            (NetworkAction::Wired { enabled }, Err(e)) => {
                warn!(target: "tilo::settings", op = "set_wired", error = %e, "network pane dispatch failed");
                self.wired.connected = !enabled;
                let _ = state.put(root, WIRED_ENABLED_KEY, !enabled);
                self.finish_local_operation(
                    crate::actions::NetworkOperation::SetWiredEnabled { enabled: *enabled },
                    Err(t_args("net-notice-wired-err", &[("error", e.into())])),
                );
            }
            (NetworkAction::Radio { radio, enabled }, Ok(())) => self.finish_local_operation(
                radio.operation(*enabled),
                Ok(t_args(
                    if *enabled {
                        "net-notice-radio-on"
                    } else {
                        "net-notice-radio-off"
                    },
                    &[("radio", radio_shown(*radio).into())],
                )),
            ),
            (NetworkAction::Radio { radio, enabled }, Err(e)) => {
                warn!(target: "tilo::settings", op = radio.label(), error = %e, "radio action failed");
                self.set_radio(*radio, !enabled);
                let _ = state.put(root, radio.key(), !enabled);
                self.finish_local_operation(
                    radio.operation(*enabled),
                    Err(t_args(
                        "net-notice-radio-err",
                        &[("radio", radio_shown(*radio).into()), ("error", e.into())],
                    )),
                );
            }
            _ => {}
        }
    }

    /// Report a finished VPN, profile, hotspot or policy change.
    fn finish_setting(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        action: &NetworkAction,
        result: Result<(), String>,
    ) {
        match (action, result) {
            (NetworkAction::Vpn { name, enabled }, Err(e)) => {
                warn!(target: "tilo::settings", op = "set_vpn_connection", profile = %name, error = %e, "VPN action failed");
                if let Some(vpn) = self.vpns.iter_mut().find(|vpn| &vpn.name == name) {
                    vpn.active = !enabled;
                }
                let _ = state.put(root, Self::vpn_key(name), !enabled);
            }
            (
                NetworkAction::Autoconnect {
                    uuid,
                    name,
                    enabled,
                },
                Err(error),
            ) => {
                self.connection_notice = Some(t_args(
                    "net-notice-update-err",
                    &[("name", name.as_str().into()), ("error", error.into())],
                ));
                if let Some(saved) = self.profiles.iter_mut().find(|p| &p.uuid == uuid) {
                    saved.autoconnect = !enabled;
                }
                let _ = state.put(root, profile_autoconnect_key(uuid), !enabled);
            }
            (NetworkAction::Forget { uuid, name }, Ok(())) => {
                self.profiles.retain(|p| &p.uuid != uuid);
                self.connection_notice = Some(t_args(
                    "net-notice-forgot",
                    &[("name", name.as_str().into())],
                ));
            }
            (NetworkAction::Forget { name, .. }, Err(error)) => {
                self.connection_notice = Some(t_args(
                    "net-notice-forget-err",
                    &[("name", name.as_str().into()), ("error", error.into())],
                ));
            }
            (NetworkAction::Hotspot { .. }, Ok(())) => {
                self.radios.hotspot_active = true;
                let _ = state.put(root, HOTSPOT_KEY, true);
                self.connection_notice = Some(t("net-notice-hotspot-on"));
            }
            (NetworkAction::Hotspot { .. }, Err(error)) => {
                self.connection_notice =
                    Some(t_args("net-notice-hotspot-err", &[("error", error.into())]));
            }
            (NetworkAction::Ipv4 { name, .. }, Ok(())) => {
                self.connection_notice = Some(t_args(
                    "net-notice-ip-ok",
                    &[("name", name.as_str().into())],
                ));
            }
            (NetworkAction::Ipv4 { .. }, Err(error)) => {
                self.connection_notice =
                    Some(t_args("net-notice-ip-err", &[("error", error.into())]));
            }
            (NetworkAction::Policy { name, .. }, Ok(())) => {
                self.connection_notice = Some(t_args(
                    "net-notice-policy-ok",
                    &[("name", name.as_str().into())],
                ));
            }
            (NetworkAction::Policy { .. }, Err(e)) => {
                self.connection_notice =
                    Some(t_args("net-notice-policy-err", &[("error", e.into())]));
            }
            // Enabling a VPN or autoconnect needs no report; switches are
            // reported by `finish_switch`.
            _ => {}
        }
    }

    /// Keep each profile selector on a profile that still exists.
    fn select_available_profiles(&mut self) {
        if !self
            .profiles
            .iter()
            .any(|p| Some(&p.uuid) == self.selected_wifi_profile.as_ref())
        {
            self.selected_wifi_profile = self
                .profiles
                .iter()
                .find(|p| p.kind == "802-11-wireless")
                .map(|p| p.uuid.clone());
        }
        if !self
            .profiles
            .iter()
            .any(|p| Some(&p.uuid) == self.selected_wired_profile.as_ref())
        {
            self.selected_wired_profile = self
                .profiles
                .iter()
                .find(|p| p.kind == "802-3-ethernet")
                .map(|p| p.uuid.clone());
        }
    }

    /// Test/`--shots` helper: pin the wired snapshot so the Wired card
    /// renders deterministic content.
    #[must_use]
    pub fn with_wired(mut self, wired: WiredInfo) -> Self {
        self.wired = wired;
        self
    }

    /// Test/`--shots` helper: pin the VPN list so the VPN card renders
    /// deterministic content. Also pins the refresh lister to the same set.
    #[must_use]
    pub fn with_vpns(mut self, vpns: Vec<VpnInfo>) -> Self {
        let snapshot = vpns.clone();
        self.vpn_lister = Arc::new(move || snapshot.clone());
        self.vpns = vpns;
        self
    }

    #[cfg(test)]
    fn with_tool_opener(mut self, opener: ToolOpener) -> Self {
        self.tool_opener = opener;
        self
    }

    pub fn vpns(&self) -> &[VpnInfo] {
        &self.vpns
    }

    /// Attach the host-owned lifecycle for typed device-local operations.
    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self.require_operation_token = true;
        self
    }

    #[must_use]
    pub fn with_command_state(
        mut self,
        commands: &ui::LocalState<Option<crate::actions::LocalPaneCommand>>,
    ) -> Self {
        self.command_state = commands.clone();
        self.isolate_document = true;
        self
    }

    fn execute_local_command(
        &mut self,
        local: &mut AutoCommit,
        operation: crate::actions::NetworkOperation,
    ) {
        let root = automerge::ROOT;
        use crate::actions::NetworkOperation as Op;
        match &operation {
            Op::Refresh => {
                let _ = local.put(&root, REFRESH_KEY, true);
            }
            Op::SetWifiEnabled { enabled } => {
                let _ = local.put(&root, WIFI_ENABLED_KEY, *enabled);
            }
            Op::SetWiredEnabled { enabled } => {
                let _ = local.put(&root, WIRED_ENABLED_KEY, *enabled);
            }
            Op::SetAirplaneMode { enabled } => {
                let _ = local.put(&root, AIRPLANE_KEY, *enabled);
            }
            Op::SetHotspot { enabled } => {
                let _ = local.put(&root, HOTSPOT_KEY, *enabled);
            }
            Op::SetMobileBroadband { enabled } => {
                let _ = local.put(&root, MOBILE_KEY, *enabled);
            }
            Op::RunDiagnostics => {
                let _ = local.put(&root, DIAGNOSTICS_KEY, true);
            }
        }
        self.sync_wifi_and_wired_toggles(local, &root);
        self.sync_radio_toggles(local, &root);
        self.sync_refresh(local, &root);
        self.sync_diagnostics(local, &root);
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "callers construct the exact typed request whose token is completed"
    )]
    fn finish_local_operation(
        &self,
        operation: crate::actions::NetworkOperation,
        result: Result<String, String>,
    ) {
        let token = crate::actions::local_operation_token_for(
            crate::actions::NETWORK_OPERATION_TYPE,
            &operation,
        );
        if !matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: running } if running == token
        ) {
            return;
        }
        self.operation_status.set(match result {
            Ok(message) => crate::actions::LocalOperationStatus::Succeeded { token, message },
            Err(message) => crate::actions::LocalOperationStatus::Failed { token, message },
        });
    }

    fn operation_is_running(&self, operation: &crate::actions::NetworkOperation) -> bool {
        if !self.require_operation_token {
            return true;
        }
        let token = crate::actions::local_operation_token_for(
            crate::actions::NETWORK_OPERATION_TYPE,
            operation,
        );
        matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: running } if running == token
        )
    }

    fn vpn_key(name: &str) -> String {
        format!("network.vpn.{name}")
    }

    fn vpn_studio_key(name: &str) -> String {
        format!("network.vpn.studio.{name}")
    }

    const VPN_STUDIO_OVERVIEW_KEY: &'static str = "network.vpn.studio.open";

    /// Rows for the VPN card use the same `NetworkManager` profiles and
    /// action boundary as quick settings.
    fn vpn_rows(&self) -> Vec<El> {
        if self.vpns.is_empty() {
            return vec![
                caption(&t("net-vpn-empty")),
                Box::new(
                    Row::new()
                        .push(Box::new(Spacer::new()))
                        .push(Box::new(Button::new(
                            "settings.network.vpn.studio.open",
                            &t("net-open-vpn-studio"),
                            Some(Self::VPN_STUDIO_OVERVIEW_KEY),
                        ))),
                ),
            ];
        }
        let mut out: Vec<El> = Vec::new();
        for v in &self.vpns {
            let id = format!("settings.network.vpn.{}", v.name);
            let key = Self::vpn_key(&v.name);
            out.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(&v.name)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(Button::new(
                        &format!("settings.network.vpn.studio.{}", v.name),
                        &t("net-open-vpn-studio"),
                        Some(&Self::vpn_studio_key(&v.name)),
                    )))
                    .push(Box::new(Toggle::new(&id, &key))),
            ));
        }
        out
    }

    /// Test/`--shots` helper: pre-expand a network's inline password prompt
    /// so the deterministic render shows it. Also forces the radio "on" so
    /// the network list renders.
    #[must_use]
    pub fn with_expanded(mut self, ssid: &str) -> Self {
        self.expanded_ssid = Some(ssid.to_string());
        self.secret_buffers
            .insert(ssid.to_string(), Arc::new(Mutex::new(String::new())));
        self.last_wifi_enabled = true;
        self.initial_wifi_enabled = true;
        self
    }

    /// Cached network list. Tests read this to assert the panes
    /// rendered the right rows.
    pub fn networks(&self) -> &[WifiNetworkInfo] {
        &self.networks
    }

    fn connection_status_text(&self) -> String {
        if !self.last_wifi_enabled {
            return t("net-wifi-off");
        }
        match self.networks.iter().find(|n| n.connected) {
            Some(n) => t_args("net-connected-to", &[("ssid", n.ssid.as_str().into())]),
            None => t("net-not-connected"),
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        if !self.loaded {
            let mut pending = vec![caption(&t("net-pending"))];
            if let Some(notice) = &self.connection_notice {
                pending.push(caption(notice));
            }
            return crate::widgets::cards(vec![card(&t("net-pending-title"), rows(pending))]);
        }
        let mut cards = vec![
            self.internet_access_card(),
            self.wifi_card(),
            self.enterprise_card(),
            self.wired_card(),
            self.profile_card(),
            card(&t("net-diag-title"), rows(self.diagnostic_rows())),
            card(&t("net-vpn"), rows(self.vpn_rows())),
            Self::airplane_card(),
            self.hotspot_card(),
        ];
        if self.radios.wwan_available {
            cards.push(card_with_action(
                &t("net-mobile"),
                Box::new(Toggle::new("settings.network.mobile", MOBILE_KEY)),
                rows(self.mobile_rows()),
            ));
        }
        crate::widgets::cards(cards)
    }

    /// Connectivity summary card: `NetworkManager`'s captive-portal-aware
    /// state, plus a sign-in shortcut when a portal is blocking the route.
    fn internet_access_card(&self) -> El {
        let connectivity = t("net-connectivity");
        let connectivity_value = connectivity_shown(self.connectivity);
        let mut content = vec![value_row(&connectivity, &connectivity_value)];
        if self.connectivity == ConnectivityState::Portal {
            content.push(caption(&t("net-portal-caption")));
            content.push(Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.captive-portal",
                        &t("net-open-signin"),
                        Some(CAPTIVE_PORTAL_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ));
        }
        card(&t("net-internet"), rows(content))
    }

    /// Wi-Fi card: header carries the toggle; body is status + scanned-network
    /// rows + a Refresh action.
    fn wifi_card(&self) -> El {
        let wifi_toggle =
            Toggle::new("settings.network.wifi", WIFI_ENABLED_KEY).with_id("network.wifi");
        let wifi_toggle = match crate::actions::network_bool_binding("wifi") {
            Some(binding) => wifi_toggle.on_action(binding),
            None => wifi_toggle.with_enabled(false),
        };
        let wifi_toggle: El = Box::new(wifi_toggle);

        let mut wifi_rows: Vec<El> = Vec::new();
        let status_label = t("net-status");
        let status_value = self.connection_status_text();
        wifi_rows.push(value_row(&status_label, &status_value));
        if let Some(notice) = &self.connection_notice {
            wifi_rows.push(caption(notice));
        }

        if self.last_wifi_enabled {
            for net in &self.networks {
                wifi_rows.push(self.network_row(net));
                // Inline password prompt expands directly beneath the row
                // whose "Connect" was pressed (secure networks only).
                if self.expanded_ssid.as_deref() == Some(net.ssid.as_str()) {
                    wifi_rows.push(self.password_prompt(&net.ssid));
                }
            }
        }
        if self.actions_busy() {
            wifi_rows.push(caption(&t("net-applying")));
        } else if self.snapshot_request.is_pending() {
            wifi_rows.push(caption(&t("net-refreshing")));
        }
        wifi_rows.push(Box::new(Row::new().push(Box::new(Spacer::new())).push(
            Box::new(Button::new(
                "settings.network.refresh",
                &t("net-refresh"),
                Some(REFRESH_KEY),
            )),
        )));
        card_with_action(&t("net-wifi"), wifi_toggle, rows(wifi_rows))
    }

    /// Wired card (real nmcli snapshot). `NetworkManager` applies the switch
    /// to this interface only; Wi-Fi and VPN stay untouched.
    fn wired_card(&self) -> El {
        if self.wired.device.is_some() {
            card_with_action(
                &t("net-wired"),
                Box::new(
                    Toggle::new("settings.network.wired", WIRED_ENABLED_KEY)
                        .with_id("network.wired"),
                ),
                rows(self.wired_rows()),
            )
        } else {
            card(&t("net-wired"), rows(self.wired_rows()))
        }
    }

    fn airplane_card() -> El {
        card_with_action(
            &t("net-airplane"),
            Box::new(Toggle::new("settings.network.airplane", AIRPLANE_KEY)),
            rows(vec![caption(&t("net-airplane-caption"))]),
        )
    }

    /// Rows for the hotspot card: SSID/password/band configuration, the
    /// apply/QR actions, and (when active) the QR payload and connected
    /// device list.
    fn hotspot_rows(&self) -> Vec<El> {
        let hotspot_secret = self
            .hotspot_password
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        let mut hotspot_rows: Vec<El> = vec![
            {
                let status = t("net-status");
                let value = if self.radios.hotspot_active {
                    t("net-hotspot-active")
                } else {
                    t("net-hotspot-off")
                };
                value_row(&status, &value)
            },
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.hotspot.ssid")
                    .with_on_change_path(HOTSPOT_SSID_KEY)
                    .with_placeholder(&t("net-hotspot-name")),
            ),
            Box::new(
                TextInput::local()
                    .with_id("network.hotspot.password")
                    .with_external_buffer(Arc::clone(&self.hotspot_password))
                    .with_initial_value(&hotspot_secret)
                    .with_secret(true)
                    .with_placeholder(&t("net-hotspot-password")),
            ),
            choice_row(
                &t("net-band"),
                Box::new(
                    Dropdown::new(
                        ["Automatic", "2.4 GHz", "5 GHz"]
                            .into_iter()
                            .map(band_shown)
                            .collect(),
                        HOTSPOT_BAND_KEY,
                    )
                    .with_id("network.hotspot.band")
                    .with_placeholder(&band_shown(&self.hotspot_band)),
                ),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.hotspot.apply",
                        &t("net-apply-start"),
                        Some(HOTSPOT_APPLY_KEY),
                    )))
                    .push(Box::new(Button::new(
                        "settings.network.hotspot.qr",
                        &if self.hotspot_qr_visible {
                            t("net-hide-qr")
                        } else {
                            t("net-share-qr")
                        },
                        Some(HOTSPOT_QR_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ),
        ];
        if self.hotspot_qr_visible {
            let config = self.hotspot_config_from_values(&self.hotspot_ssid, &self.hotspot_band);
            if let Ok(payload) = config.and_then(|config| config.qr_payload()) {
                hotspot_rows.push(Box::new(QrWidget::new(payload.into_bytes())));
                hotspot_rows.push(caption(&t("net-qr-caption")));
            }
        }
        if self.radios.hotspot_active {
            let clients = t("net-hotspot-clients");
            hotspot_rows.push(value_row(&clients, &self.hotspot_clients.len().to_string()));
            for client in &self.hotspot_clients {
                let signal = client
                    .signal_dbm
                    .map_or_else(|| t("net-signal-unknown"), |dbm| format!("{dbm} dBm"));
                hotspot_rows.push(value_row(&client.mac, &signal));
            }
        }
        hotspot_rows
    }

    fn hotspot_card(&self) -> El {
        card_with_action(
            &t("net-hotspot"),
            Box::new(Toggle::new("settings.network.hotspot", HOTSPOT_KEY)),
            rows(self.hotspot_rows()),
        )
    }

    fn profile_card(&self) -> El {
        card_with_action(
            &t("net-profiles"),
            Box::new(Button::new(
                "settings.network.advanced",
                &if self.advanced_open {
                    t("net-hide-policy")
                } else {
                    t("net-show-policy")
                },
                Some(ADVANCED_KEY),
            )),
            rows(self.profile_rows()),
        )
    }

    fn profile_rows(&self) -> Vec<El> {
        let wifi_profiles: Vec<&SavedNetworkProfile> = self
            .profiles
            .iter()
            .filter(|profile| profile.kind == "802-11-wireless")
            .collect();
        if wifi_profiles.is_empty() {
            return vec![caption(&t("net-no-saved-wifi"))];
        }
        let mut rows_out: Vec<El> = Vec::new();
        for profile in &wifi_profiles {
            rows_out.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(&profile.name)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(Button::new(
                        &format!("settings.network.profile.share.{}", profile.uuid),
                        &t("net-share-devices"),
                        Some(&profile_share_key(&profile.uuid)),
                    )))
                    .push(Box::new(Button::new(
                        &format!("settings.network.profile.forget.{}", profile.uuid),
                        &t("net-forget"),
                        Some(&profile_forget_key(&profile.uuid)),
                    )))
                    .push(Box::new(
                        Toggle::new(
                            &format!("settings.network.profile.autoconnect.{}", profile.uuid),
                            &profile_autoconnect_key(&profile.uuid),
                        )
                        .with_id(&format!("network.profile.autoconnect.{}", profile.uuid)),
                    )),
            ));
        }
        self.push_advanced_profile_rows(&wifi_profiles, &mut rows_out);
        rows_out
    }

    /// Appends the Wi-Fi profile policy editor rows (metered/MAC/proxy
    /// policy) when the Connection policy panel is open and a profile is selected.
    fn push_advanced_profile_rows(
        &self,
        wifi_profiles: &[&SavedNetworkProfile],
        rows_out: &mut Vec<El>,
    ) {
        if self.advanced_open
            && let Some(profile) = self
                .selected_wifi_profile
                .as_ref()
                .and_then(|uuid| wifi_profiles.iter().find(|p| &p.uuid == uuid).copied())
                .or_else(|| wifi_profiles.first().copied())
        {
            if wifi_profiles.len() > 1 {
                rows_out.push(choice_row(
                    &t("net-edit-profile"),
                    Box::new(
                        Dropdown::new(
                            wifi_profiles.iter().map(|p| profile_label(p)).collect(),
                            ADVANCED_PROFILE_KEY,
                        )
                        .with_placeholder(&profile_label(profile)),
                    ),
                ));
            }
            rows_out.push(caption(&t_args(
                "net-policy-for",
                &[("name", profile.name.as_str().into())],
            )));
            let Some(_settings) = self.profile_settings.get(&profile.uuid) else {
                rows_out.push(caption(&t("net-policy-unread")));
                return;
            };
            rows_out.push(choice_row(
                &t("net-metered"),
                Box::new(
                    Dropdown::new(
                        ["Automatic", "Metered", "Unmetered"]
                            .into_iter()
                            .map(metered_shown)
                            .collect(),
                        &format!("network.profile.metered.{}", profile.uuid),
                    )
                    .with_placeholder(&metered_shown("Automatic")),
                ),
            ));
            rows_out.push(choice_row(
                &t("net-mac"),
                Box::new(
                    Dropdown::new(
                        [
                            "Stable per network",
                            "Random each connection",
                            "Permanent hardware address",
                        ]
                        .into_iter()
                        .map(mac_shown)
                        .collect(),
                        &format!("network.profile.mac.{}", profile.uuid),
                    )
                    .with_placeholder(&mac_shown("Stable per network")),
                ),
            ));
            rows_out.push(field_row(
                &t("net-proxy-url"),
                Box::new(
                    TextInput::new(Vec::new())
                        .with_on_change_path(&format!("network.profile.proxy.{}", profile.uuid))
                        .with_placeholder("https://example.org/proxy.pac"),
                ),
            ));
            rows_out.push(Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        &format!("settings.network.profile.policy.{}", profile.uuid),
                        &t("net-apply-policy"),
                        Some(&format!("network.profile.policy.{}", profile.uuid)),
                    )))
                    .push(Box::new(Spacer::new())),
            ));
        }
    }

    fn diagnostic_rows(&self) -> Vec<El> {
        let mut out: Vec<El> = vec![Box::new(
            Row::new()
                .push(Box::new(Button::new(
                    "settings.network.diagnostics",
                    &t("net-run-diagnostics"),
                    Some(DIAGNOSTICS_KEY),
                )))
                .push(Box::new(Spacer::new())),
        )];
        if let Some(d) = &self.diagnostics {
            out.push(value_row("NetworkManager", &d.network_manager_state));
            out.push(value_row(
                &t("net-diag-internet"),
                &connectivity_shown(d.connectivity),
            ));
            if let Some(v) = &d.device {
                out.push(value_row(&t("net-diag-device"), v));
            }
            if let Some(v) = &d.address {
                out.push(value_row(&t("net-diag-address"), v));
            }
            if let Some(v) = &d.gateway {
                out.push(value_row(&t("net-diag-gateway"), v));
            }
            let dns = if d.dns.is_empty() {
                t("net-diag-dns-none")
            } else {
                d.dns.join(", ")
            };
            out.push(value_row(&t("net-diag-dns"), &dns));
            for recommendation in &d.recommendations {
                out.push(caption(recommendation));
            }
        }
        out
    }

    fn secret_field(&self, key: &str, id: &str, placeholder: &str) -> El {
        // `key` is always one of the fixed buffers seeded by
        // `initial_secret_buffers`; fall back to a fresh, unshared buffer
        // rather than panicking if that invariant is ever violated.
        let buffer = self
            .secret_buffers
            .get(key)
            .cloned()
            .unwrap_or_else(|| Arc::new(Mutex::new(String::new())));
        let initial = buffer.lock().map(|value| value.clone()).unwrap_or_default();
        Box::new(
            TextInput::local()
                .with_id(id)
                .with_external_buffer(buffer)
                .with_initial_value(&initial)
                .with_secret(true)
                .with_placeholder(placeholder),
        )
    }

    fn mobile_rows(&self) -> Vec<El> {
        let mut rows = vec![
            caption(&t("net-mobile-caption")),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.mobile.profile")
                    .with_on_change_path(MOBILE_PROFILE_KEY)
                    .with_placeholder(&t("net-profile-name")),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.mobile.apn")
                    .with_on_change_path(MOBILE_APN_KEY)
                    .with_placeholder(&t("net-apn")),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.mobile.create",
                        &t("net-create-profile"),
                        Some(MOBILE_CREATE_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ),
        ];
        if let Some(first_sim) = self.sims.first() {
            rows.push(choice_row(
                &t("net-sim"),
                Box::new(
                    Dropdown::new(
                        self.sims.iter().map(|s| s.label.clone()).collect(),
                        MOBILE_SIM_KEY,
                    )
                    .with_placeholder(&first_sim.label),
                ),
            ));
        }
        rows.extend([
            self.secret_field(MOBILE_PIN_BUFFER, "network.mobile.pin", &t("net-sim-pin")),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.mobile.unlock",
                        &t("net-unlock-sim"),
                        Some(MOBILE_UNLOCK_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ) as El,
        ]);
        rows
    }

    fn enterprise_rows(&self) -> Vec<El> {
        vec![
            choice_row(
                &t("net-method"),
                Box::new(
                    Dropdown::new(
                        vec!["PEAP".into(), "TTLS".into(), "TLS".into()],
                        ENTERPRISE_METHOD_KEY,
                    )
                    .with_placeholder("PEAP"),
                ),
            ),
            choice_row(
                &t("net-inner-auth"),
                Box::new(
                    Dropdown::new(
                        vec!["MSCHAPv2".into(), "PAP".into(), "CHAP".into()],
                        ENTERPRISE_PHASE2_KEY,
                    )
                    .with_placeholder("MSCHAPv2"),
                ),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.profile")
                    .with_on_change_path(ENTERPRISE_PROFILE_KEY)
                    .with_placeholder(&t("net-profile-name")),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.client_cert")
                    .with_on_change_path(ENTERPRISE_CLIENT_CERT_KEY)
                    .with_placeholder(&t("net-client-cert")),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.private_key")
                    .with_on_change_path(ENTERPRISE_PRIVATE_KEY)
                    .with_placeholder(&t("net-private-key")),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.ssid")
                    .with_on_change_path(ENTERPRISE_SSID_KEY)
                    .with_placeholder(&t("net-ssid")),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.identity")
                    .with_on_change_path(ENTERPRISE_IDENTITY_KEY)
                    .with_placeholder(&t("net-identity")),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.anonymous")
                    .with_on_change_path(ENTERPRISE_ANONYMOUS_KEY)
                    .with_placeholder(&t("net-anonymous")),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.ca")
                    .with_on_change_path(ENTERPRISE_CA_KEY)
                    .with_placeholder(&t("net-ca-path")),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.enterprise.ca.choose",
                        &t("net-choose-ca"),
                        Some(ENTERPRISE_CA_CHOOSE_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ),
            self.secret_field(
                ENTERPRISE_PASSWORD_BUFFER,
                "network.enterprise.password",
                &t("net-password"),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.enterprise.connect",
                        &t("net-save-connect"),
                        Some(ENTERPRISE_CONNECT_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ),
        ]
    }

    /// Keep specialist 802.1X credentials out of the ordinary connection
    /// view. The affordance remains visible, while the form appears only when
    /// someone has chosen to add that kind of network.
    fn enterprise_card(&self) -> El {
        let mut content = Vec::new();
        if self.enterprise_editor_open {
            content.extend(self.enterprise_rows());
        }
        content.push(Box::new(
            Row::new()
                .push(Box::new(Button::new(
                    "settings.network.enterprise.editor",
                    &if self.enterprise_editor_open {
                        t("net-close-enterprise")
                    } else {
                        t("net-setup-enterprise")
                    },
                    Some(ENTERPRISE_EDITOR_KEY),
                )))
                .push(Box::new(Spacer::new())),
        ));
        card(&t("net-enterprise"), rows(content))
    }

    fn hotspot_config_from_values(
        &self,
        ssid: &str,
        band: &str,
    ) -> Result<HotspotConfiguration, String> {
        let password = self
            .hotspot_password
            .lock()
            .map_err(|_| "hotspot password buffer is unavailable")?
            .clone();
        let band = band_from_label(band);
        let config = HotspotConfiguration {
            ssid: ssid.to_string(),
            password,
            band,
        };
        config.validate()?;
        Ok(config)
    }

    /// One scanned-network row: the Wi-Fi icon and name on the left, what the
    /// network offers (its protection and signal) and a Connect / Connected
    /// button on the right.
    fn network_row(&self, net: &WifiNetworkInfo) -> El {
        let detail = t_args(
            if net.secure {
                "net-wifi-detail-secured"
            } else {
                "net-wifi-detail-open"
            },
            &[("signal", wifi_signal_label(net.signal).into())],
        );
        let key = connect_key(&net.ssid);
        let cta_id = format!("settings.network.connect.{}", net.ssid);
        let cta_label = if net.connected {
            t("net-connected")
        } else if self.expanded_ssid.as_deref() == Some(net.ssid.as_str()) {
            t("net-cancel")
        } else {
            t("net-connect")
        };
        crate::widgets::icon_row(
            ui::icons::Icon::Wifi,
            &net.ssid,
            &detail,
            Box::new(Button::new(&cta_id, &cta_label, Some(&key))),
        )
    }

    /// The inline password prompt shown beneath a secure network when its
    /// "Connect" is pressed: a masked, process-local field plus a Join
    /// command bit. The password itself never enters the Automerge document.
    fn password_prompt(&self, ssid: &str) -> El {
        let jk = join_key(ssid);
        let field_id = format!("settings.network.password.{ssid}");
        let join_id = format!("settings.network.join.{ssid}");
        let buffer = self
            .secret_buffers
            .get(ssid)
            .cloned()
            .unwrap_or_else(|| Arc::new(Mutex::new(String::new())));
        let field = TextInput::local()
            .with_id(&field_id)
            .with_external_buffer(buffer)
            .with_secret(true)
            .with_placeholder(&t("net-password"));
        Box::new(
            Row::new()
                .push(Box::new(field))
                .push(Box::new(Spacer::new()))
                .push(Box::new(Button::new(&join_id, &t("net-join"), Some(&jk)))),
        )
    }

    /// Rows for the Wired card, derived from the live `nmcli` snapshot.
    fn wired_rows(&self) -> Vec<El> {
        if !self.wired.connected {
            return vec![
                value_row(&t("net-ethernet"), &t("net-not-connected")),
                caption(&t("net-wired-empty")),
            ];
        }
        let mut out = Vec::new();
        let status = match &self.wired.speed {
            Some(s) => t_args("net-connected-speed", &[("speed", s.as_str().into())]),
            None => t("net-connected"),
        };
        out.push(value_row(&t("net-ethernet"), &status));
        if let Some(dev) = &self.wired.device {
            out.push(value_row(&t("net-interface"), dev));
        }
        out.push(value_row(
            &t("net-ipv4"),
            self.wired.ipv4.as_deref().unwrap_or("—"),
        ));
        let wired_profiles: Vec<_> = self
            .profiles
            .iter()
            .filter(|p| p.kind == "802-3-ethernet")
            .collect();
        if let Some(profile) = self
            .selected_wired_profile
            .as_ref()
            .and_then(|uuid| wired_profiles.iter().find(|p| &p.uuid == uuid).copied())
            .or_else(|| wired_profiles.first().copied())
        {
            if wired_profiles.len() > 1 {
                out.push(choice_row(
                    &t("net-edit-profile"),
                    Box::new(
                        Dropdown::new(
                            wired_profiles.iter().map(|p| profile_label(p)).collect(),
                            WIRED_PROFILE_KEY,
                        )
                        .with_placeholder(&profile_label(profile)),
                    ),
                ));
            }
            let Some(_settings) = self
                .profile_settings
                .get(&profile.uuid)
                .and_then(|s| s.ipv4.as_ref())
            else {
                out.push(caption(&t("net-ipv4-unsupported")));
                return out;
            };
            let automatic_key = ipv4_key(&profile.uuid, "automatic");
            let address_key = ipv4_key(&profile.uuid, "address");
            let gateway_key = ipv4_key(&profile.uuid, "gateway");
            let dns_key = ipv4_key(&profile.uuid, "dns");
            let apply_key = ipv4_key(&profile.uuid, "apply");
            out.extend([
                setting_row(
                    &t("net-auto-ipv4"),
                    Box::new(Toggle::new(
                        "settings.network.ipv4.automatic",
                        &automatic_key,
                    )),
                ),
                Box::new(
                    TextInput::new(Vec::new())
                        .with_id("network.ipv4.address")
                        .with_on_change_path(&address_key)
                        .with_placeholder(&t("net-address-prefix")),
                ),
                Box::new(
                    TextInput::new(Vec::new())
                        .with_id("network.ipv4.gateway")
                        .with_on_change_path(&gateway_key)
                        .with_placeholder(&t("net-gateway")),
                ),
                Box::new(
                    TextInput::new(Vec::new())
                        .with_id("network.ipv4.dns")
                        .with_on_change_path(&dns_key)
                        .with_placeholder(&t("net-dns-servers")),
                ),
                Box::new(
                    Row::new()
                        .push(Box::new(Button::new(
                            "settings.network.ipv4.apply",
                            &t("net-apply-ip"),
                            Some(&apply_key),
                        )))
                        .push(Box::new(Spacer::new())),
                ),
            ]);
        }
        out
    }

    /// Update which network's password prompt is expanded, mirroring it
    /// into the document and flagging a structure rebuild so the prompt
    /// appears/disappears on the next frame.
    fn set_expanded(&mut self, state: &mut AutoCommit, root: &ObjId, ssid: Option<String>) {
        if self.expanded_ssid == ssid {
            return;
        }
        let _ = state.put(root, EXPANDED_KEY, ssid.clone().unwrap_or_default());
        if let Some(s) = &ssid {
            self.secret_buffers
                .entry(s.clone())
                .or_insert_with(|| Arc::new(Mutex::new(String::new())));
        }
        let old_to_clear = self
            .expanded_ssid
            .clone()
            .filter(|old| ssid.as_ref() != Some(old));
        if let Some(old) = old_to_clear
            && let Some(buffer) = self.secret_buffers.remove(&old)
            && let Ok(mut secret) = buffer.lock()
        {
            secret.zeroize();
        }
        self.expanded_ssid = ssid;
        self.structure_dirty = true;
    }

    /// Dispatch a Wi-Fi connection, deduplicated per SSID so a single
    /// button press doesn't re-fire on every frame.
    fn do_connect(
        &mut self,
        ssid: &str,
        security: os_actions::WifiSecurity,
        password: Option<&str>,
        lookup_shared: bool,
    ) {
        if self.dispatched_connects.iter().any(|s| s == ssid) {
            return;
        }
        self.dispatched_connects.push(ssid.to_string());
        self.connection_notice = Some(t_args("net-notice-connecting", &[("ssid", ssid.into())]));
        let dispatcher = Arc::clone(&self.dispatcher);
        let tx = self.connection_tx.clone();
        let ssid = ssid.to_string();
        let password = password.map(|value| Zeroizing::new(value.to_string()));
        let continuity = Arc::clone(&self.wifi_continuity);
        std::thread::spawn(move || {
            let portable = portable_security(security);
            let result = if lookup_shared {
                let Some(mode) = portable else {
                    let _ = tx.send((
                        NetworkJob::Wifi(ssid),
                        Ok(NetworkJobSuccess::PasswordRequired {
                            continuity_warning: None,
                        }),
                    ));
                    return;
                };
                match continuity.command(ui::collaboration::WifiContinuityCommand::Lookup {
                    ssid: ssid.clone(),
                    security: mode,
                }) {
                    Ok(ui::collaboration::WifiContinuityReply::Password { password }) => {
                        let password = Zeroizing::new(password);
                        dispatcher
                            .connect_wifi(&ssid, Some(password.as_str()))
                            .map(|()| NetworkJobSuccess::Connected {
                                continuity_notice: Some(t("net-notice-used-sealed")),
                            })
                    }
                    Ok(ui::collaboration::WifiContinuityReply::NotFound) => {
                        Ok(NetworkJobSuccess::PasswordRequired {
                            continuity_warning: None,
                        })
                    }
                    Ok(ui::collaboration::WifiContinuityReply::Rejected { reason }) => {
                        Ok(NetworkJobSuccess::PasswordRequired {
                            continuity_warning: Some(reason),
                        })
                    }
                    Ok(_) => Ok(NetworkJobSuccess::PasswordRequired {
                        continuity_warning: Some(
                            "the Wi-Fi continuity service returned an invalid lookup reply".into(),
                        ),
                    }),
                    Err(error) => Ok(NetworkJobSuccess::PasswordRequired {
                        continuity_warning: Some(error),
                    }),
                }
            } else {
                dispatcher
                    .connect_wifi(&ssid, password.as_deref().map(String::as_str))
                    .map(|()| {
                        let continuity_notice = password.as_ref().and_then(|password| {
                            let mode = portable?;
                            match continuity.command(
                                ui::collaboration::WifiContinuityCommand::Share {
                                    ssid: ssid.clone(),
                                    security: mode,
                                    password: password.to_string(),
                                },
                            ) {
                                Ok(ui::collaboration::WifiContinuityReply::Shared { .. }) => {
                                    Some(t("net-notice-password-sealed"))
                                }
                                Ok(ui::collaboration::WifiContinuityReply::Rejected { reason }) => {
                                    Some(t_args(
                                        "net-notice-share-err",
                                        &[("error", reason.into())],
                                    ))
                                }
                                Ok(_) => Some(t("net-notice-share-invalid")),
                                Err(error) => {
                                    Some(t_args("net-notice-share-err", &[("error", error.into())]))
                                }
                            }
                        });
                        NetworkJobSuccess::Connected { continuity_notice }
                    })
            };
            let _ = tx.send((NetworkJob::Wifi(ssid), result));
        });
    }

    /// Consume the CA-certificate file-picker result, if the background
    /// chooser thread has replied.
    fn sync_ca_picker_result(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(receiver) = &self.ca_picker_rx
            && let Ok(result) = receiver.try_recv()
        {
            match result {
                ui::portal::PortalOutcome::Opened(path) => {
                    let path = path.display().to_string();
                    let object = crate::ensure_text_object(state, root, ENTERPRISE_CA_KEY);
                    let old_len =
                        state.text(&object).map_or(0, |text| text.chars().count()) as isize;
                    let _ = state.splice_text(&object, 0, old_len, &path);
                    self.connection_notice = Some("CA certificate selected.".into());
                }
                ui::portal::PortalOutcome::Cancelled => {}
                ui::portal::PortalOutcome::Failed(error) => {
                    self.connection_notice =
                        Some(t_args("net-notice-cert-err", &[("error", error.into())]));
                }
            }
            self.ca_picker_rx = None;
            self.structure_dirty = true;
        }
    }

    /// Drain completed background connection/profile jobs and report their
    /// outcome via `connection_notice`.
    fn sync_connection_jobs(&mut self, state: &mut AutoCommit, root: &ObjId) {
        while let Ok((job, result)) = self.connection_rx.try_recv() {
            let subject = match &job {
                NetworkJob::Wifi(ssid) => ssid.clone(),
                NetworkJob::ShareProfile => t("net-subject-saved-wifi"),
                NetworkJob::MobileProfile => t("net-subject-mobile"),
                NetworkJob::SimUnlock => t("net-subject-sim"),
                NetworkJob::EnterpriseWifi => t("net-subject-enterprise"),
            };
            if let NetworkJob::Wifi(ssid) = &job {
                self.dispatched_connects.retain(|pending| pending != ssid);
            }
            match result {
                Ok(NetworkJobSuccess::Connected { continuity_notice }) => {
                    self.connection_notice = Some(continuity_notice.map_or_else(
                        || {
                            t_args(
                                "net-notice-connected",
                                &[("subject", subject.as_str().into())],
                            )
                        },
                        |notice| {
                            t_args(
                                "net-notice-connected-extra",
                                &[
                                    ("subject", subject.as_str().into()),
                                    ("notice", notice.into()),
                                ],
                            )
                        },
                    ));
                    // Re-read the list and saved profiles off the UI thread.
                    self.start_snapshot();
                    if matches!(job, NetworkJob::Wifi(_)) {
                        self.set_expanded(state, root, None);
                    }
                }
                Ok(NetworkJobSuccess::PasswordRequired { continuity_warning }) => {
                    if let NetworkJob::Wifi(ssid) = &job {
                        self.set_expanded(state, root, Some(ssid.clone()));
                        self.connection_notice = continuity_warning.map_or_else(
                            || {
                                Some(t_args(
                                    "net-notice-enter-password",
                                    &[("ssid", ssid.as_str().into())],
                                ))
                            },
                            |warning| {
                                Some(t_args(
                                    "net-notice-enter-password-warn",
                                    &[("ssid", ssid.as_str().into()), ("warning", warning.into())],
                                ))
                            },
                        );
                    }
                }
                Ok(NetworkJobSuccess::ProfileShared { ssid }) => {
                    self.connection_notice = Some(t_args(
                        "net-notice-sealed-profile",
                        &[("ssid", ssid.as_str().into())],
                    ));
                }
                Ok(NetworkJobSuccess::OperationCompleted { notice }) => {
                    self.connection_notice = Some(notice);
                }
                Err(error) => {
                    self.connection_notice = Some(t_args(
                        "net-notice-configure-err",
                        &[
                            ("subject", subject.as_str().into()),
                            ("error", error.clone().into()),
                        ],
                    ));
                    warn!(target: "tilo::settings", op = "network_job", %subject, %error, "network pane operation failed");
                }
            }
            self.structure_dirty = true;
        }
    }

    /// One-time seed of every CRDT key this pane owns, run on first `sync`
    /// (and again after a structure rebuild reconstructs the pane). Only
    /// fills keys the document doesn't already carry, so a rebuild never
    /// clobbers a value the user already changed.
    fn seed_if_needed(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !self.needs_seed {
            return;
        }
        self.seed_radio_and_vpn_keys(state, root);
        self.seed_hotspot_and_profile_selection(state, root);
        self.seed_text_and_command_keys(state, root);
        self.seed_profile_settings(state, root);
        let _ = state.put(root, REFRESH_KEY, automerge::ScalarValue::Boolean(false));
        // Seed the expansion mirror from the field so a pre-expanded
        // pane (tests / `--shots`) keeps its prompt open through the
        // first sync (which would otherwise read the empty doc as "no
        // prompt" and collapse it). Only when nothing's there yet.
        if state.get(root, EXPANDED_KEY).ok().flatten().is_none() {
            let _ = state.put(
                root,
                EXPANDED_KEY,
                self.expanded_ssid.clone().unwrap_or_default(),
            );
        }
        self.needs_seed = false;
    }

    fn seed_radio_and_vpn_keys(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if state.get(root, WIFI_ENABLED_KEY).ok().flatten().is_none() {
            let _ = state.put(
                root,
                WIFI_ENABLED_KEY,
                automerge::ScalarValue::Boolean(self.initial_wifi_enabled),
            );
        } else if let Ok(Some((Value::Scalar(s), _))) = state.get(root, WIFI_ENABLED_KEY)
            && let automerge::ScalarValue::Boolean(b) = s.as_ref()
        {
            // Adopt the existing value so the dispatcher-change guard
            // doesn't re-fire `set_wifi` on the first post-rebuild sync.
            self.last_wifi_enabled = *b;
        }
        if state.get(root, WIRED_ENABLED_KEY).ok().flatten().is_none() {
            let _ = state.put(
                root,
                WIRED_ENABLED_KEY,
                automerge::ScalarValue::Boolean(self.wired.connected),
            );
        }
        for (key, value) in [
            (AIRPLANE_KEY, self.radios.airplane_mode),
            (HOTSPOT_KEY, self.radios.hotspot_active),
            (MOBILE_KEY, self.radios.wwan_enabled),
        ] {
            if state.get(root, key).ok().flatten().is_none() {
                let _ = state.put(root, key, value);
            }
        }
        for vpn in &self.vpns {
            if state
                .get(root, Self::vpn_key(&vpn.name))
                .ok()
                .flatten()
                .is_none()
            {
                let _ = state.put(root, Self::vpn_key(&vpn.name), vpn.active);
            }
            let _ = state.put(root, Self::vpn_studio_key(&vpn.name), false);
        }
    }

    fn seed_hotspot_and_profile_selection(&self, state: &mut AutoCommit, root: &ObjId) {
        let _ = state.put(root, Self::VPN_STUDIO_OVERVIEW_KEY, false);
        let _ = state.put(root, HOTSPOT_APPLY_KEY, false);
        let _ = state.put(root, HOTSPOT_QR_KEY, false);
        let _ = state.put(root, HOTSPOT_SSID_KEY, self.hotspot_ssid.as_str());
        let _ = crate::ensure_text_object(state, root, HOTSPOT_SSID_KEY);
        let _ = state.put(root, HOTSPOT_BAND_KEY, self.hotspot_band.as_str());
        if let Some(uuid) = &self.selected_wifi_profile
            && let Some(profile) = self.profiles.iter().find(|p| &p.uuid == uuid)
            && state
                .get(root, ADVANCED_PROFILE_KEY)
                .ok()
                .flatten()
                .is_none()
        {
            let _ = state.put(root, ADVANCED_PROFILE_KEY, profile_label(profile));
        }
        if let Some(uuid) = &self.selected_wired_profile
            && let Some(profile) = self.profiles.iter().find(|p| &p.uuid == uuid)
            && state.get(root, WIRED_PROFILE_KEY).ok().flatten().is_none()
        {
            let _ = state.put(root, WIRED_PROFILE_KEY, profile_label(profile));
        }
    }

    fn seed_text_and_command_keys(&self, state: &mut AutoCommit, root: &ObjId) {
        for key in [
            MOBILE_PROFILE_KEY,
            MOBILE_APN_KEY,
            ENTERPRISE_PROFILE_KEY,
            ENTERPRISE_SSID_KEY,
            ENTERPRISE_IDENTITY_KEY,
            ENTERPRISE_ANONYMOUS_KEY,
            ENTERPRISE_CA_KEY,
            ENTERPRISE_CLIENT_CERT_KEY,
            ENTERPRISE_PRIVATE_KEY,
        ] {
            let _ = crate::ensure_text_object(state, root, key);
        }
        for key in [
            MOBILE_CREATE_KEY,
            MOBILE_UNLOCK_KEY,
            ENTERPRISE_CONNECT_KEY,
            ENTERPRISE_EDITOR_KEY,
            ENTERPRISE_CA_CHOOSE_KEY,
            CAPTIVE_PORTAL_KEY,
            DIAGNOSTICS_KEY,
            ADVANCED_KEY,
        ] {
            let _ = state.put(root, key, false);
        }
        let _ = state.put(root, ENTERPRISE_METHOD_KEY, "PEAP");
        let _ = state.put(root, ENTERPRISE_PHASE2_KEY, "MSCHAPv2");
        if let Some(sim) = self.sims.first() {
            let _ = state.put(root, MOBILE_SIM_KEY, sim.label.as_str());
        }
    }

    fn seed_profile_settings(&self, state: &mut AutoCommit, root: &ObjId) {
        for profile in &self.profiles {
            let _ = state.put(
                root,
                profile_autoconnect_key(&profile.uuid),
                profile.autoconnect,
            );
            let _ = state.put(root, profile_forget_key(&profile.uuid), false);
            let _ = state.put(root, profile_share_key(&profile.uuid), false);
            let Some(settings) = self.profile_settings.get(&profile.uuid) else {
                continue;
            };
            if let Some(value) = settings.metered {
                let label = metered_id(value);
                let key = format!("network.profile.metered.{}", profile.uuid);
                if state.get(root, &key).ok().flatten().is_none() {
                    let _ = state.put(root, &key, label);
                }
            }
            if let Some(value) = settings.mac {
                let label = mac_id(value);
                let key = format!("network.profile.mac.{}", profile.uuid);
                if state.get(root, &key).ok().flatten().is_none() {
                    let _ = state.put(root, &key, label);
                }
            }
            if let Some(proxy) = &settings.proxy {
                let key = format!("network.profile.proxy.{}", profile.uuid);
                if state.get(root, &key).ok().flatten().is_none() {
                    let _ = state.put(root, &key, proxy.automatic_url.as_deref().unwrap_or(""));
                }
                let _ = crate::ensure_text_object(state, root, &key);
            }
            if let Some(ipv4) = &settings.ipv4 {
                let automatic_key = ipv4_key(&profile.uuid, "automatic");
                if state.get(root, &automatic_key).ok().flatten().is_none() {
                    let _ = state.put(root, &automatic_key, ipv4.automatic);
                }
                for (field, value) in [
                    ("address", ipv4.addresses.join(", ")),
                    ("gateway", ipv4.gateway.clone().unwrap_or_default()),
                    ("dns", ipv4.dns.join(", ")),
                ] {
                    let key = ipv4_key(&profile.uuid, field);
                    if state.get(root, &key).ok().flatten().is_none() {
                        let _ = state.put(root, &key, value);
                    }
                    let _ = crate::ensure_text_object(state, root, &key);
                }
                let _ = state.put(root, ipv4_key(&profile.uuid, "apply"), false);
            }
        }
    }

    fn sync_wifi_and_wired_toggles(&mut self, state: &mut AutoCommit, root: &ObjId) {
        // Wi-Fi toggle.
        if let Ok(Some((val, _))) = state.get(root, WIFI_ENABLED_KEY)
            && let Value::Scalar(s) = val
            && let automerge::ScalarValue::Boolean(b) = s.as_ref()
            && *b != self.last_wifi_enabled
        {
            let enabled = *b;
            let operation = crate::actions::NetworkOperation::SetWifiEnabled { enabled };
            if !self.operation_is_running(&operation) {
                let _ = state.put(root, WIFI_ENABLED_KEY, self.last_wifi_enabled);
                return;
            }
            // Shown at once; `finish_action` reverts it if the change fails.
            self.last_wifi_enabled = enabled;
            self.queue_action(NetworkAction::Wifi { enabled });
        }

        // Wired interface toggle. Missing hardware has no switch, and this
        // never uses NetworkManager's global networking radio.
        if let Ok(Some((Value::Scalar(s), _))) = state.get(root, WIRED_ENABLED_KEY)
            && let automerge::ScalarValue::Boolean(enabled) = s.as_ref()
            && *enabled != self.wired.connected
            && self.wired.device.is_some()
        {
            let enabled = *enabled;
            let operation = crate::actions::NetworkOperation::SetWiredEnabled { enabled };
            if !self.operation_is_running(&operation) {
                let _ = state.put(root, WIRED_ENABLED_KEY, self.wired.connected);
                return;
            }
            self.wired.connected = enabled;
            self.queue_action(NetworkAction::Wired { enabled });
        }
    }

    fn sync_radio_toggles(&mut self, state: &AutoCommit, root: &ObjId) {
        for radio in [Radio::Airplane, Radio::Hotspot, Radio::Mobile] {
            let Some(enabled) = read_bool(state, root, radio.key()) else {
                continue;
            };
            let previous = self.radio(radio);
            if enabled == previous {
                continue;
            }
            self.set_radio(radio, enabled);
            self.queue_action(NetworkAction::Radio { radio, enabled });
        }
    }

    fn sync_vpn_toggles(&mut self, state: &AutoCommit, root: &ObjId) {
        let mut changed = Vec::new();
        for vpn in &mut self.vpns {
            let enabled = read_bool(state, root, Self::vpn_key(&vpn.name)).unwrap_or(false);
            if enabled != vpn.active {
                vpn.active = enabled;
                changed.push(NetworkAction::Vpn {
                    name: vpn.name.clone(),
                    enabled,
                });
            }
        }
        for action in changed {
            self.queue_action(action);
        }
    }

    fn sync_profile_autoconnect_and_forget(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let profile_snapshot = self.profiles.clone();
        for profile in profile_snapshot {
            let auto_key = profile_autoconnect_key(&profile.uuid);
            if let Some(enabled) = read_bool(state, root, &auto_key)
                && enabled != profile.autoconnect
            {
                if let Some(saved) = self.profiles.iter_mut().find(|p| p.uuid == profile.uuid) {
                    saved.autoconnect = enabled;
                }
                self.queue_action(NetworkAction::Autoconnect {
                    uuid: profile.uuid.clone(),
                    name: profile.name.clone(),
                    enabled,
                });
            }
            let forget_key = profile_forget_key(&profile.uuid);
            if read_bool(state, root, &forget_key).unwrap_or(false) {
                let _ = state.put(root, &forget_key, false);
                self.queue_action(NetworkAction::Forget {
                    uuid: profile.uuid.clone(),
                    name: profile.name.clone(),
                });
            }
        }
    }

    fn sync_profile_shares(&mut self, state: &mut AutoCommit, root: &ObjId) {
        for profile in self
            .profiles
            .iter()
            .filter(|profile| profile.kind == "802-11-wireless")
        {
            let key = profile_share_key(&profile.uuid);
            if !read_bool(state, root, &key).unwrap_or(false) {
                continue;
            }
            let _ = state.put(root, key, false);
            self.connection_notice = Some(format!("Sealing {} for your devices…", profile.name));
            let continuity = Arc::clone(&self.wifi_continuity);
            let tx = self.connection_tx.clone();
            let uuid = profile.uuid.clone();
            std::thread::spawn(move || {
                let result = continuity
                    .command(
                        ui::collaboration::WifiContinuityCommand::ShareSavedProfile {
                            profile_uuid: uuid.clone(),
                        },
                    )
                    .and_then(|reply| match reply {
                        ui::collaboration::WifiContinuityReply::Shared { ssid } => {
                            Ok(NetworkJobSuccess::ProfileShared { ssid })
                        }
                        ui::collaboration::WifiContinuityReply::Rejected { reason } => Err(reason),
                        _ => Err(
                            "the Wi-Fi continuity service returned an invalid profile reply".into(),
                        ),
                    });
                let _ = tx.send((NetworkJob::ShareProfile, result));
            });
        }
    }

    fn sync_hotspot_apply(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, HOTSPOT_APPLY_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, HOTSPOT_APPLY_KEY, false);
        self.hotspot_ssid = read_str_or_text(state, root, HOTSPOT_SSID_KEY)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Tilo Hotspot".into());
        self.hotspot_band =
            read_str_or_text(state, root, HOTSPOT_BAND_KEY).unwrap_or_else(|| "Automatic".into());
        match self.hotspot_config_from_values(&self.hotspot_ssid, &self.hotspot_band) {
            Ok(config) => {
                self.connection_notice = Some(t("net-notice-hotspot-starting"));
                self.queue_action(NetworkAction::Hotspot { config });
            }
            Err(error) => self.connection_notice = Some(error),
        }
        self.structure_dirty = true;
    }

    fn sync_hotspot_qr(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, HOTSPOT_QR_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, HOTSPOT_QR_KEY, false);
        self.hotspot_ssid = read_str_or_text(state, root, HOTSPOT_SSID_KEY)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Tilo Hotspot".into());
        self.hotspot_band =
            read_str_or_text(state, root, HOTSPOT_BAND_KEY).unwrap_or_else(|| "Automatic".into());
        match self.hotspot_config_from_values(&self.hotspot_ssid, &self.hotspot_band) {
            Ok(_) => self.hotspot_qr_visible = !self.hotspot_qr_visible,
            Err(error) => self.connection_notice = Some(error),
        }
        self.structure_dirty = true;
    }

    fn sync_profile_selectors(&mut self, state: &AutoCommit, root: &ObjId) {
        for selector in [ADVANCED_PROFILE_KEY, WIRED_PROFILE_KEY] {
            if let Some(label) = read_str_or_text(state, root, selector)
                && let Some(profile) = self.profiles.iter().find(|p| profile_label(p) == label)
            {
                let selected = if selector == ADVANCED_PROFILE_KEY {
                    &mut self.selected_wifi_profile
                } else {
                    &mut self.selected_wired_profile
                };
                if selected.as_deref() != Some(&profile.uuid) {
                    *selected = Some(profile.uuid.clone());
                    self.structure_dirty = true;
                }
            }
        }
    }

    fn sync_ipv4_apply(&mut self, state: &mut AutoCommit, root: &ObjId) {
        for profile in self
            .profiles
            .clone()
            .into_iter()
            .filter(|p| p.kind == "802-3-ethernet")
        {
            let apply_key = ipv4_key(&profile.uuid, "apply");
            if !read_bool(state, root, &apply_key).unwrap_or(false) {
                continue;
            }
            let _ = state.put(root, &apply_key, false);
            if self
                .profile_settings
                .get(&profile.uuid)
                .is_none_or(|s| s.ipv4.is_none())
            {
                self.connection_notice = Some(
                    "IP settings were not applied because the profile could not be read safely."
                        .into(),
                );
                continue;
            }
            let config = IpConfiguration {
                automatic: read_bool(state, root, ipv4_key(&profile.uuid, "automatic"))
                    .unwrap_or(false),
                addresses: read_str_or_text(state, root, ipv4_key(&profile.uuid, "address"))
                    .into_iter()
                    .flat_map(|value| {
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                            .take(17)
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .collect(),
                gateway: read_str_or_text(state, root, ipv4_key(&profile.uuid, "gateway"))
                    .filter(|v| !v.trim().is_empty()),
                dns: read_str_or_text(state, root, ipv4_key(&profile.uuid, "dns"))
                    .into_iter()
                    .flat_map(|value| {
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                            .take(17)
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .collect(),
            };
            self.connection_notice = Some(format!("Applying IP settings to {}…", profile.name));
            self.queue_action(NetworkAction::Ipv4 {
                uuid: profile.uuid.clone(),
                name: profile.name.clone(),
                config,
            });
        }
    }

    fn sync_mobile_create(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, MOBILE_CREATE_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, MOBILE_CREATE_KEY, false);
        let name = read_str_or_text(state, root, MOBILE_PROFILE_KEY).unwrap_or_default();
        let apn = read_str_or_text(state, root, MOBILE_APN_KEY).unwrap_or_default();
        let tx = self.connection_tx.clone();
        self.connection_notice = Some("Creating mobile profile…".into());
        std::thread::spawn(move || {
            let result = os_actions::create_mobile_profile(&name, &apn);
            let _ = tx.send((
                NetworkJob::MobileProfile,
                result.map(|()| NetworkJobSuccess::OperationCompleted {
                    notice: "Mobile profile created.".into(),
                }),
            ));
        });
    }

    fn sync_advanced_toggle(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, ADVANCED_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, ADVANCED_KEY, false);
        self.advanced_open = !self.advanced_open;
        if self.advanced_open {
            for profile in &self.profiles {
                if profile.kind == "802-11-wireless" {
                    let _ = crate::ensure_text_object(
                        state,
                        root,
                        &format!("network.profile.proxy.{}", profile.uuid),
                    );
                }
            }
        }
        self.structure_dirty = true;
    }

    fn sync_enterprise_editor(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, ENTERPRISE_EDITOR_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, ENTERPRISE_EDITOR_KEY, false);
        self.enterprise_editor_open = !self.enterprise_editor_open;
        self.structure_dirty = true;
    }

    fn sync_diagnostics(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, DIAGNOSTICS_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, DIAGNOSTICS_KEY, false);
        // The read is a system-bus round trip; it runs on the app runtime's
        // blocking pool and reports through `sync_diagnostics_result`.
        let started = self.diagnostics_request.spawn(async {
            ui::rt::app()
                .spawn_blocking(os_actions::connection_diagnostics)
                .await
                .unwrap_or_else(|error| Err(format!("the diagnostics task stopped: {error}")))
        });
        if !started {
            self.finish_diagnostics(Err("the settings window is closing".into()));
        }
    }

    fn sync_diagnostics_result(&mut self) {
        if let Some(result) = self.diagnostics_request.try_take() {
            self.finish_diagnostics(result);
        }
    }

    fn finish_diagnostics(&mut self, result: Result<os_actions::ConnectionDiagnostics, String>) {
        match result {
            Ok(d) => {
                self.diagnostics = Some(d);
                self.finish_local_operation(
                    crate::actions::NetworkOperation::RunDiagnostics,
                    Ok("Network diagnostics completed.".into()),
                );
            }
            Err(e) => {
                self.connection_notice = Some(format!("Diagnostics unavailable: {e}"));
                self.finish_local_operation(
                    crate::actions::NetworkOperation::RunDiagnostics,
                    Err(format!("Diagnostics unavailable: {e}")),
                );
            }
        }
        self.structure_dirty = true;
    }

    fn sync_profile_policy(&mut self, state: &mut AutoCommit, root: &ObjId) {
        for profile in self.profiles.clone() {
            let apply = format!("network.profile.policy.{}", profile.uuid);
            if !read_bool(state, root, &apply).unwrap_or(false) {
                continue;
            }
            let _ = state.put(root, &apply, false);
            let Some(original) = self.profile_settings.get(&profile.uuid).cloned() else {
                self.connection_notice = Some(t("net-notice-policy-unread"));
                continue;
            };
            let requested_metered = match read_str_or_text(
                state,
                root,
                format!("network.profile.metered.{}", profile.uuid),
            )
            .as_deref()
            {
                Some(label) => metered_from_label(label)
                    .unwrap_or_else(|| original.metered.unwrap_or_default()),
                _ => original.metered.unwrap_or_default(),
            };
            let requested_mac = match read_str_or_text(
                state,
                root,
                format!("network.profile.mac.{}", profile.uuid),
            )
            .as_deref()
            {
                Some(label) => mac_from_label(label).or(original.mac),
                _ => original.mac,
            };
            let requested_proxy_url = read_str_or_text(
                state,
                root,
                format!("network.profile.proxy.{}", profile.uuid),
            )
            .filter(|s| !s.trim().is_empty());
            let requested_proxy =
                original
                    .proxy
                    .as_ref()
                    .map(|old| os_actions::ProxyConfiguration {
                        automatic_url: requested_proxy_url,
                        browser_only: old.browser_only,
                    });
            let patch = os_actions::ConnectionPolicyPatch {
                metered: original
                    .metered
                    .filter(|old| *old != requested_metered)
                    .map(|_| requested_metered),
                mac: match (original.mac, requested_mac) {
                    (Some(old), Some(new)) if old != new => Some(new),
                    _ => None,
                },
                proxy: match (&original.proxy, requested_proxy) {
                    (Some(old), Some(new)) if old != &new => Some(new),
                    _ => None,
                },
            };
            self.connection_notice = Some(format!("Applying advanced policy to {}…", profile.name));
            self.queue_action(NetworkAction::Policy {
                uuid: profile.uuid.clone(),
                name: profile.name.clone(),
                patch,
            });
        }
    }

    fn sync_mobile_unlock(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, MOBILE_UNLOCK_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, MOBILE_UNLOCK_KEY, false);
        let mut pin = self
            .secret_buffers
            .get(MOBILE_PIN_BUFFER)
            .and_then(|buffer| buffer.lock().ok().map(|value| value.clone()))
            .unwrap_or_default();
        if let Some(buffer) = self.secret_buffers.get(MOBILE_PIN_BUFFER)
            && let Ok(mut value) = buffer.lock()
        {
            value.zeroize();
        }
        let pin = Zeroizing::new(std::mem::take(&mut pin));
        let tx = self.connection_tx.clone();
        let selected = read_str_or_text(state, root, MOBILE_SIM_KEY)
            .and_then(|label| {
                self.sims
                    .iter()
                    .find(|s| s.label == label)
                    .map(|s| s.path.clone())
            })
            .or_else(|| self.sims.first().map(|s| s.path.clone()));
        self.connection_notice = Some("Unlocking SIM…".into());
        std::thread::spawn(move || {
            let result = selected
                .ok_or_else(|| "No SIM is available".to_string())
                .and_then(|sim| os_actions::unlock_mobile_sim_on(&sim, &pin));
            let _ = tx.send((
                NetworkJob::SimUnlock,
                result.map(|()| NetworkJobSuccess::OperationCompleted {
                    notice: "The SIM card is unlocked.".into(),
                }),
            ));
        });
        self.structure_dirty = true;
    }

    fn sync_enterprise_connect(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, ENTERPRISE_CONNECT_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, ENTERPRISE_CONNECT_KEY, false);
        let config = EnterpriseWifiConfiguration {
            profile_name: read_str_or_text(state, root, ENTERPRISE_PROFILE_KEY).unwrap_or_default(),
            ssid: read_str_or_text(state, root, ENTERPRISE_SSID_KEY).unwrap_or_default(),
            identity: read_str_or_text(state, root, ENTERPRISE_IDENTITY_KEY).unwrap_or_default(),
            anonymous_identity: read_str_or_text(state, root, ENTERPRISE_ANONYMOUS_KEY)
                .filter(|value| !value.trim().is_empty()),
            ca_certificate: read_str_or_text(state, root, ENTERPRISE_CA_KEY).unwrap_or_default(),
            method: match read_str_or_text(state, root, ENTERPRISE_METHOD_KEY).as_deref() {
                Some("TTLS") => os_actions::EnterpriseEapMethod::Ttls,
                Some("TLS") => os_actions::EnterpriseEapMethod::Tls,
                _ => os_actions::EnterpriseEapMethod::Peap,
            },
            phase2_auth: read_str_or_text(state, root, ENTERPRISE_PHASE2_KEY)
                .map(|v| v.to_ascii_lowercase()),
            client_certificate: read_str_or_text(state, root, ENTERPRISE_CLIENT_CERT_KEY)
                .filter(|v| !v.trim().is_empty()),
            private_key: read_str_or_text(state, root, ENTERPRISE_PRIVATE_KEY)
                .filter(|v| !v.trim().is_empty()),
        };
        let mut password = self
            .secret_buffers
            .get(ENTERPRISE_PASSWORD_BUFFER)
            .and_then(|buffer| buffer.lock().ok().map(|value| value.clone()))
            .unwrap_or_default();
        if let Some(buffer) = self.secret_buffers.get(ENTERPRISE_PASSWORD_BUFFER)
            && let Ok(mut value) = buffer.lock()
        {
            value.zeroize();
        }
        let password = Zeroizing::new(std::mem::take(&mut password));
        let tx = self.connection_tx.clone();
        self.connection_notice = Some("Connecting to enterprise Wi-Fi…".into());
        std::thread::spawn(move || {
            let result = os_actions::connect_enterprise_wifi(&config, &password);
            let _ = tx.send((
                NetworkJob::EnterpriseWifi,
                result.map(|()| NetworkJobSuccess::OperationCompleted {
                    notice: "Enterprise Wi-Fi connected.".into(),
                }),
            ));
        });
        self.structure_dirty = true;
    }

    fn sync_enterprise_ca_choose(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, ENTERPRISE_CA_CHOOSE_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, ENTERPRISE_CA_CHOOSE_KEY, false);
        if self.ca_picker_rx.is_none() {
            self.ca_picker_rx = Some(
                ui::portal::PickRequest::open("Choose organization CA certificate", "Choose")
                    .thread_name("settings-ca-picker")
                    .send(),
            );
        }
    }

    fn sync_captive_portal(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if !read_bool(state, root, CAPTIVE_PORTAL_KEY).unwrap_or(false) {
            return;
        }
        let _ = state.put(root, CAPTIVE_PORTAL_KEY, false);
        // Both steps block (a system-bus lookup, then the desktop portal), so
        // they run on a worker of the app runtime and report through
        // `sync_captive_portal_result`. The opener is resolved here: a scoped
        // host is only visible on this thread.
        let opener = ui::host::current().opener;
        let started = self.captive_portal.spawn(async move {
            ui::rt::app()
                .spawn_blocking(move || {
                    let uri = os_actions::captive_portal_uri()?;
                    ui::portal::open_uri_with(&*opener, &uri).map_err(|error| {
                        format!("could not open browser through the desktop portal: {error}")
                    })
                })
                .await
                .unwrap_or_else(|error| Err(format!("the sign-in task stopped: {error}")))
        });
        if !started {
            self.report_captive_portal(Err("the settings window is closing".into()));
        }
    }

    fn sync_captive_portal_result(&mut self) {
        if let Some(result) = self.captive_portal.try_take() {
            self.report_captive_portal(result);
        }
    }

    fn report_captive_portal(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.connection_notice = Some(format!("Could not open network sign-in: {error}"));
            self.structure_dirty = true;
        }
    }

    /// Contextual handoff: Settings owns daily status/toggles; VPN Studio
    /// owns profile editing. Reset each command bit before requesting the
    /// shell so a failed launch cannot loop every frame.
    fn sync_vpn_studio_handoff(&self, state: &mut AutoCommit, root: &ObjId) {
        let requested_profiles: Vec<String> = self
            .vpns
            .iter()
            .filter(|vpn| read_bool(state, root, Self::vpn_studio_key(&vpn.name)).unwrap_or(false))
            .map(|vpn| vpn.name.clone())
            .collect();
        for profile in requested_profiles {
            let _ = state.put(root, Self::vpn_studio_key(&profile), false);
            if let Err(e) = (self.tool_opener)(ToolIntent::Vpn {
                route: VpnRoute::Profile,
                profile: Some(profile.clone()),
            }) {
                warn!(target: "tilo::settings", profile = %profile, error = %e, "could not open VPN Studio");
            }
        }
        if read_bool(state, root, Self::VPN_STUDIO_OVERVIEW_KEY).unwrap_or(false) {
            let _ = state.put(root, Self::VPN_STUDIO_OVERVIEW_KEY, false);
            if let Err(e) = (self.tool_opener)(ToolIntent::Vpn {
                route: VpnRoute::Overview,
                profile: None,
            }) {
                warn!(target: "tilo::settings", error = %e, "could not open VPN Studio");
            }
        }
    }

    /// Refresh button — when the button writes `true`, rescan and clear the
    /// bit so subsequent frames don't loop.
    fn sync_refresh(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let refresh_pressed = matches!(
            state.get(root, REFRESH_KEY),
            Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
        );
        if !refresh_pressed {
            return;
        }
        let _ = state.put(root, REFRESH_KEY, automerge::ScalarValue::Boolean(false));
        // The rescan runs on the app runtime; `apply_snapshot` finishes the
        // Refresh operation when it arrives.
        self.refresh_pending = true;
        if !self.start_snapshot() {
            self.refresh_pending = false;
            self.finish_local_operation(
                crate::actions::NetworkOperation::Refresh,
                Err("Could not refresh network list: the settings window is closing".into()),
            );
        }
    }

    /// Restore the expanded-prompt SSID from the document (so it survives
    /// structure rebuilds), then drive the per-network "Connect" and "Join"
    /// buttons.
    fn sync_network_connect_actions(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let doc_expanded = read_str_or_text(state, root, EXPANDED_KEY).filter(|s| !s.is_empty());
        if doc_expanded != self.expanded_ssid {
            self.expanded_ssid = doc_expanded;
        }

        // Snapshot the network attributes we need without holding a borrow
        // across the mutating `state` calls below.
        let nets: Vec<(String, bool, os_actions::WifiSecurity)> = self
            .networks
            .iter()
            .map(|n| (n.ssid.clone(), n.secure, n.security))
            .collect();

        // Per-network "Connect" buttons. Open networks connect immediately;
        // secure networks toggle the inline password prompt instead.
        for (ssid, secure, security) in &nets {
            let key = connect_key(ssid);
            let pressed = read_bool(state, root, &key).unwrap_or(false);
            if !pressed {
                continue;
            }
            let _ = state.put(root, key.as_str(), automerge::ScalarValue::Boolean(false));
            if *secure {
                if self.expanded_ssid.as_deref() == Some(ssid.as_str()) {
                    self.set_expanded(state, root, None);
                } else {
                    self.do_connect(ssid, *security, None, true);
                }
            } else {
                // Open network — connect with no password.
                self.do_connect(ssid, *security, None, false);
                self.set_expanded(state, root, None);
            }
        }

        // Per-network "Join" buttons in the expanded password prompt:
        // connect with the typed password, then collapse the prompt.
        for (ssid, _secure, security) in &nets {
            let jk = join_key(ssid);
            if !read_bool(state, root, &jk).unwrap_or(false) {
                continue;
            }
            let _ = state.put(root, jk.as_str(), automerge::ScalarValue::Boolean(false));
            let mut pw = self
                .secret_buffers
                .get(ssid)
                .and_then(|buffer| buffer.lock().ok().map(|value| value.clone()))
                .unwrap_or_default();
            let password = (!pw.is_empty()).then_some(pw.as_str());
            self.do_connect(ssid, *security, password, false);
            pw.zeroize();
            if let Some(buffer) = self.secret_buffers.get(ssid)
                && let Ok(mut value) = buffer.lock()
            {
                value.zeroize();
            }
            self.set_expanded(state, root, None);
        }
    }
    /// Seed and act on the pane's controls. Runs only once the first
    /// snapshot has arrived, so no control is compared against state the
    /// pane has not read yet.
    fn sync_controls(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.seed_if_needed(state, root);
        if let Some(crate::actions::LocalPaneCommand::Network(operation)) = self.command_state.get()
        {
            self.command_state.set(None);
            self.execute_local_command(state, operation);
        }
        self.sync_wifi_and_wired_toggles(state, root);
        self.sync_radio_toggles(state, root);
        self.sync_vpn_toggles(state, root);
        self.sync_profile_autoconnect_and_forget(state, root);
        self.sync_profile_shares(state, root);
        self.sync_hotspot_apply(state, root);
        self.sync_hotspot_qr(state, root);
        self.sync_profile_selectors(state, root);
        self.sync_ipv4_apply(state, root);
        self.sync_mobile_create(state, root);
        self.sync_advanced_toggle(state, root);
        self.sync_enterprise_editor(state, root);
        self.sync_diagnostics(state, root);
        self.sync_profile_policy(state, root);
        self.sync_mobile_unlock(state, root);
        self.sync_enterprise_connect(state, root);
        self.sync_enterprise_ca_choose(state, root);
        self.sync_captive_portal(state, root);
        self.sync_vpn_studio_handoff(state, root);
        self.sync_refresh(state, root);
        self.sync_network_connect_actions(state, root);
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(NetworkPane::new())
}

impl Widget for NetworkPane {
    fn sync(&mut self, shared: &mut AppModel, root: &ModelPath) {
        let (shared, _) = ui::doc::document(shared, root);
        let isolated = self.isolate_document;
        let mut state = if isolated {
            std::mem::take(&mut self.local_doc)
        } else {
            std::mem::take(shared)
        };
        let root = automerge::ROOT;
        self.sync_ca_picker_result(&mut state, &root);
        self.sync_connection_jobs(&mut state, &root);
        self.sync_diagnostics_result();
        self.sync_captive_portal_result();
        self.sync_action_results(&mut state, &root);
        self.sync_snapshot_result(&mut state, &root);
        if self.loaded {
            self.sync_controls(&mut state, &root);
        }

        if let Some(c) = &mut self.composed {
            c.sync(&mut state, &root);
        }
        if isolated {
            self.local_doc = state;
        } else {
            *shared = state;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        // `new_with_children` only fails when a child belongs to a different
        // tree; `child` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "child was just created in this same tree"
        )]
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "NetworkPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Bubble the expansion-driven rebuild request (the inline password
    /// prompt opening/closing) plus any child's own request, so the host
    /// re-runs `build_layout` and the prompt actually appears.
    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

    fn sample_networks() -> Vec<WifiNetworkInfo> {
        vec![
            WifiNetworkInfo {
                ssid: "Home_Network".to_string(),
                signal: 4,
                secure: true,
                security: os_actions::WifiSecurity::Wpa3,
                connected: true,
            },
            WifiNetworkInfo {
                ssid: "Cafe_Guest".to_string(),
                signal: 3,
                secure: false,
                security: os_actions::WifiSecurity::Open,
                connected: false,
            },
            WifiNetworkInfo {
                ssid: "Neighbour_5G".to_string(),
                signal: 2,
                secure: true,
                security: os_actions::WifiSecurity::Wpa2,
                connected: false,
            },
        ]
    }

    fn wait_for_password_prompt(pane: &mut NetworkPane, doc: &mut AutoCommit, root: &ObjId) {
        // A hang guard for the worker thread, not a latency budget.
        let deadline = tilo_test_env::hang_deadline();
        while ui::time::now() < deadline {
            pane.sync(doc, root);
            if pane.expanded_ssid.is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    struct SharedPasswordClient {
        commands: Arc<Mutex<Vec<ui::collaboration::WifiContinuityCommand>>>,
    }

    impl WifiContinuityClient for SharedPasswordClient {
        fn command(
            &self,
            command: ui::collaboration::WifiContinuityCommand,
        ) -> Result<ui::collaboration::WifiContinuityReply, String> {
            self.commands.lock().unwrap().push(command.clone());
            match command {
                ui::collaboration::WifiContinuityCommand::Lookup { .. } => {
                    Ok(ui::collaboration::WifiContinuityReply::Password {
                        password: "shared-secret".into(),
                    })
                }
                ui::collaboration::WifiContinuityCommand::Share { ssid, .. }
                | ui::collaboration::WifiContinuityCommand::ShareSavedProfile {
                    profile_uuid: ssid,
                } => Ok(ui::collaboration::WifiContinuityReply::Shared { ssid }),
            }
        }
    }

    /// One row per network, refresh and the first snapshot via the wake.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn wifi_list_arrives_through_the_wake_and_renders_each_network() {
        // Case `wifi_signal_levels_are_worded_not_drawn_with_font_glyphs`.
        // The signal level is a word: no private-use or block-element glyph
        // that a font may not carry.
        {
            let levels: Vec<String> = (1u8..=4).map(wifi_signal_label).collect();
            let unique: std::collections::HashSet<&String> = levels.iter().collect();
            assert_eq!(unique.len(), 4, "each level reads differently");
            for level in &levels {
                assert!(
                    level.chars().all(|c| c.is_ascii_alphabetic() || c == ' '),
                    "{level:?} is plain text"
                );
            }
        }

        // Case `wifi_list_renders_each_returned_network`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let nets = sample_networks();
            let pane =
                NetworkPane::with_dispatcher(rec as Arc<dyn Dispatcher + Send + Sync>, true, nets);
            assert_eq!(pane.networks().len(), 3);
            let by_ssid: Vec<&str> = pane.networks().iter().map(|n| n.ssid.as_str()).collect();
            assert_eq!(by_ssid, vec!["Home_Network", "Cafe_Guest", "Neighbour_5G"]);
            // Status line picks the connected network.
            assert_eq!(pane.connection_status_text(), "Connected to Home_Network");
        }

        // Case `refresh_button_replaces_network_list`.
        // Refresh only starts the rescan; the new list arrives through the
        // wake and is applied by the next sync.
        {
            let fresh = vec![WifiNetworkInfo {
                ssid: "NewSpot".to_string(),
                signal: 4,
                secure: false,
                security: os_actions::WifiSecurity::Open,
                connected: false,
            }];
            let expected = fresh.clone();
            let (before, pending, after) = on_ui_thread(move |wakes| {
                let rec = Arc::new(RecordingDispatcher::default());
                let mut pane = NetworkPane::with_dispatcher(
                    rec as Arc<dyn Dispatcher + Send + Sync>,
                    true,
                    sample_networks(),
                );
                // Swap the lister to a fresh set so we can observe the refresh.
                pane.lister = Arc::new(move || Ok(fresh.clone()));
                let mut doc = AutoCommit::new();
                let root = automerge::ROOT;
                pane.sync(&mut doc, &root);
                let _ = doc.put(&root, REFRESH_KEY, automerge::ScalarValue::Boolean(true));
                pane.sync(&mut doc, &root);
                let before = pane.networks().to_vec();
                let pending = pane.snapshot_request.is_pending();
                // Each wake is followed by the sync the host would run.
                while pane.snapshot_request.is_pending() {
                    wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                    pane.sync(&mut doc, &root);
                }
                (before, pending, pane.networks().to_vec())
            });
            assert_eq!(
                before,
                sample_networks(),
                "the sync does not wait for the scan"
            );
            assert!(pending);
            assert_eq!(after, expected);
        }

        // Case `the_first_snapshot_arrives_through_the_wake`.
        // The production pane is built without querying `NetworkManager`: it
        // shows a pending view and acts on no control until the first snapshot
        // arrives through the wake.
        {
            let outcome = on_ui_thread(|wakes| {
                // The scan waits for the test, so the pending state is observed
                // deterministically.
                let (release, gate) = std::sync::mpsc::channel::<()>();
                let gate = Mutex::new(gate);
                let rec = Arc::new(RecordingDispatcher::default());
                let mut pane = NetworkPane::from_parts(
                    rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
                    Arc::new(move || {
                        let _ = gate.lock().unwrap().recv();
                        Ok(sample_networks())
                    }),
                    Arc::new(Vec::<VpnInfo>::new),
                    Arc::new(EmptyWifiContinuityClient),
                    true,
                    Vec::new(),
                );
                pane.begin_initial_load();
                let mut doc = AutoCommit::new();
                let root = automerge::ROOT;
                pane.sync(&mut doc, &root);
                let loading = !pane.loaded && pane.networks().is_empty();
                let unseeded = doc.get(&root, WIFI_ENABLED_KEY).unwrap().is_none();
                // A control flipped before the snapshot is not acted on yet.
                let _ = doc.put(&root, AIRPLANE_KEY, true);
                pane.sync(&mut doc, &root);
                let inert = rec.calls().is_empty();
                release.send(()).unwrap();
                while !pane.loaded {
                    wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                    pane.sync(&mut doc, &root);
                }
                (
                    loading,
                    unseeded,
                    inert,
                    pane.loaded,
                    pane.networks().to_vec(),
                    doc.get(&root, WIFI_ENABLED_KEY).unwrap().is_some(),
                )
            });
            let (loading, unseeded, inert, loaded, networks, seeded) = outcome;
            assert!(loading, "construction does not wait for NetworkManager");
            assert!(unseeded, "controls are not seeded from unread state");
            assert!(inert, "no control acts before the first snapshot");
            assert!(loaded);
            assert_eq!(networks, sample_networks());
            assert!(seeded, "the first snapshot seeds the controls");
        }
    }

    /// Enterprise/VPN editors open only explicitly and seed per profile; diagnostics run off the UI thread.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn network_diagnostics_and_advanced_editors_are_explicit_and_off_thread() {
        // Case `enterprise_editor_opens_only_after_its_explicit_action`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane = NetworkPane::with_dispatcher(
                rec as Arc<dyn Dispatcher + Send + Sync>,
                true,
                sample_networks(),
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            assert!(!pane.enterprise_editor_open);

            let _ = doc.put(&root, ENTERPRISE_EDITOR_KEY, true);
            pane.sync(&mut doc, &root);
            assert!(pane.enterprise_editor_open);
            assert!(pane.take_structure_dirty());
            assert!(!read_bool(&doc, &root, ENTERPRISE_EDITOR_KEY).unwrap_or(false));
        }

        // Case `diagnostics_run_off_the_ui_thread_and_report_through_the_wake`.
        // Diagnostics read the system bus. The UI thread only starts the read;
        // the outcome — a report or an error, depending on the host — arrives
        // through the wake and is applied by the next sync.
        {
            let outcome = ui::rt::spawn_thread("settings-ui-test", || {
                ui::rt::mark_ui_thread();
                let mut scope = ui::WakeScope::new();
                let (woke, wakes) = std::sync::mpsc::channel();
                scope
                    .bind(move |_| {
                        let _ = woke.send(());
                    })
                    .unwrap();
                let _entry = scope.enter();
                let rec = Arc::new(RecordingDispatcher::default());
                let mut pane = NetworkPane::with_dispatcher(
                    rec as Arc<dyn Dispatcher + Send + Sync>,
                    true,
                    sample_networks(),
                );
                let mut doc = AutoCommit::new();
                let root = automerge::ROOT;
                pane.sync(&mut doc, &root);
                let _ = doc.put(&root, DIAGNOSTICS_KEY, true);
                pane.sync(&mut doc, &root);
                assert!(!read_bool(&doc, &root, DIAGNOSTICS_KEY).unwrap_or(true));
                assert!(pane.diagnostics_request.is_pending());
                wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                pane.sync(&mut doc, &root);
                (pane.diagnostics.is_some(), pane.connection_notice.clone())
            })
            .unwrap()
            .join()
            .unwrap();
            match outcome {
                (true, _) => {}
                (false, Some(notice)) => {
                    assert!(notice.starts_with("Diagnostics unavailable:"), "{notice}");
                    assert!(!notice.contains("UI thread"), "{notice}");
                }
                (false, None) => panic!("diagnostics finished without a report or an error"),
            }
        }

        // Case `advanced_vpn_action_opens_the_exact_profile_context`.
        {
            use std::sync::Mutex;

            let seen = Arc::new(Mutex::new(Vec::new()));
            let record = Arc::clone(&seen);
            let mut pane = NetworkPane::new_demo(sample_networks())
                .with_vpns(vec![VpnInfo {
                    name: "Work VPN".into(),
                    active: false,
                }])
                .with_tool_opener(Arc::new(move |intent| {
                    record.lock().unwrap().push(intent);
                    Ok(())
                }));
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, NetworkPane::vpn_studio_key("Work VPN"), true);
            pane.sync(&mut doc, &root);

            assert_eq!(
                seen.lock().unwrap().as_slice(),
                &[ToolIntent::Vpn {
                    route: VpnRoute::Profile,
                    profile: Some("Work VPN".into()),
                }]
            );
            assert!(
                !read_bool(&doc, &root, NetworkPane::vpn_studio_key("Work VPN")).unwrap_or(false)
            );
        }

        // Case `advanced_editors_seed_networkmanager_values_per_profile`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane = NetworkPane::with_dispatcher(
                rec as Arc<dyn Dispatcher + Send + Sync>,
                true,
                Vec::new(),
            );
            let wifi = SavedNetworkProfile {
                name: "Office".into(),
                uuid: "11111111-1111-1111-1111-111111111111".into(),
                kind: "802-11-wireless".into(),
                autoconnect: true,
            };
            let lan = SavedNetworkProfile {
                name: "Dock".into(),
                uuid: "22222222-2222-2222-2222-222222222222".into(),
                kind: "802-3-ethernet".into(),
                autoconnect: true,
            };
            pane.profiles = vec![wifi.clone(), lan.clone()];
            pane.selected_wifi_profile = Some(wifi.uuid.clone());
            pane.selected_wired_profile = Some(lan.uuid.clone());
            pane.profile_settings.insert(
                wifi.uuid.clone(),
                os_actions::ConnectionProfileSettings {
                    metered: Some(os_actions::MeteredPolicy::Metered),
                    mac: Some(os_actions::MacPolicy::Random),
                    proxy: Some(os_actions::ProxyConfiguration {
                        automatic_url: Some("https://proxy.example/pac".into()),
                        browser_only: true,
                    }),
                    ipv4: None,
                },
            );
            pane.profile_settings.insert(
                lan.uuid.clone(),
                os_actions::ConnectionProfileSettings {
                    ipv4: Some(IpConfiguration {
                        automatic: false,
                        addresses: vec!["192.0.2.44/24".into()],
                        gateway: Some("192.0.2.1".into()),
                        dns: vec!["1.1.1.1".into()],
                    }),
                    ..Default::default()
                },
            );
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                read_str_or_text(
                    &doc,
                    &automerge::ROOT,
                    format!("network.profile.metered.{}", wifi.uuid)
                )
                .as_deref(),
                Some("Metered")
            );
            assert_eq!(
                read_str_or_text(
                    &doc,
                    &automerge::ROOT,
                    format!("network.profile.proxy.{}", wifi.uuid)
                )
                .as_deref(),
                Some("https://proxy.example/pac")
            );
            assert!(
                !read_bool(&doc, &automerge::ROOT, ipv4_key(&lan.uuid, "automatic"))
                    .unwrap_or(false)
            );
            assert_eq!(
                read_str_or_text(&doc, &automerge::ROOT, ipv4_key(&lan.uuid, "address")).as_deref(),
                Some("192.0.2.44/24")
            );
        }
    }

    /// Drive the syncs the host runs on each wake until every queued change
    /// and the snapshot that follows have been applied.
    fn settle(pane: &mut NetworkPane, doc: &mut AutoCommit, wakes: &std::sync::mpsc::Receiver<()>) {
        let root = automerge::ROOT;
        while pane.actions_busy() || pane.snapshot_request.is_pending() {
            wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            pane.sync(doc, &root);
        }
    }

    /// Wi-Fi/wired toggles dispatch in order off-thread, revert on failure, and ignore replicated latches.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn radio_toggles_run_off_thread_and_ignore_replicated_values() {
        // Case `wifi_toggle_invokes_set_wifi`.
        {
            let calls = on_ui_thread(|wakes| {
                let rec = Arc::new(RecordingDispatcher::default());
                let mut pane = NetworkPane::with_dispatcher(
                    rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
                    true,
                    sample_networks(),
                );
                let mut doc = AutoCommit::new();
                let root = automerge::ROOT;
                pane.sync(&mut doc, &root);
                // User flips the toggle off.
                let _ = doc.put(
                    &root,
                    WIFI_ENABLED_KEY,
                    automerge::ScalarValue::Boolean(false),
                );
                pane.sync(&mut doc, &root);
                settle(&mut pane, &mut doc, wakes);
                rec.calls()
            });
            assert_eq!(calls, vec!["set_wifi(false)"]);
        }

        // Case `wifi_changes_run_off_the_ui_thread_in_order_and_revert_on_failure`.
        // Switching Wi-Fi never waits on the UI thread: the sync returns while
        // the switch is still running, changes run in request order, and a
        // failed one reverts the toggle.
        {
            let outcome = on_ui_thread(|wakes| {
                let (release, gate) = std::sync::mpsc::channel();
                let dispatcher = Arc::new(GatedWifi {
                    gate: Mutex::new(gate),
                    calls: Mutex::new(Vec::new()),
                });
                let mut pane = NetworkPane::with_dispatcher(
                    dispatcher.clone() as Arc<dyn Dispatcher + Send + Sync>,
                    true,
                    sample_networks(),
                );
                let mut doc = AutoCommit::new();
                let root = automerge::ROOT;
                pane.sync(&mut doc, &root);
                let _ = doc.put(&root, WIFI_ENABLED_KEY, false);
                pane.sync(&mut doc, &root);
                let busy_while_running = pane.actions_busy() && !pane.last_wifi_enabled;
                let _ = doc.put(&root, WIFI_ENABLED_KEY, true);
                pane.sync(&mut doc, &root);
                // The first switch succeeds, the second fails.
                release.send(Ok(())).unwrap();
                release.send(Err("radio is blocked".into())).unwrap();
                settle(&mut pane, &mut doc, wakes);
                (
                    busy_while_running,
                    dispatcher.calls.lock().unwrap().clone(),
                    pane.last_wifi_enabled,
                    read_bool(&doc, &root, WIFI_ENABLED_KEY),
                )
            });
            let (busy, calls, enabled, key) = outcome;
            assert!(busy, "the sync returns while the switch is running");
            assert_eq!(calls, ["set_wifi(false)", "set_wifi(true)"]);
            assert!(!enabled, "the failed switch back on is reverted");
            assert_eq!(key, Some(false));
        }

        // Case `replicated_radio_toggle_is_inert_without_its_local_token`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let status = ui::LocalState::new(crate::actions::LocalOperationStatus::Idle);
            let commands = ui::LocalState::new(None);
            let mut pane = NetworkPane::with_dispatcher(
                rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
                true,
                sample_networks(),
            )
            .with_command_state(&commands)
            .with_operation_status(&status);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let initial_heads = doc.get_heads();
            pane.sync(&mut doc, &root);
            assert_eq!(doc.get_heads(), initial_heads);
            let _ = doc.put(&root, WIFI_ENABLED_KEY, false);
            pane.sync(&mut doc, &root);
            assert!(rec.calls().is_empty());
            assert!(pane.last_wifi_enabled);
        }

        // Case `wired_toggle_only_dispatches_the_primary_lan_interface`.
        {
            let (calls, wifi) = on_ui_thread(|wakes| {
                let rec = Arc::new(RecordingDispatcher::default());
                let mut pane = NetworkPane::with_dispatcher(
                    rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
                    true,
                    sample_networks(),
                )
                .with_wired(WiredInfo {
                    connected: true,
                    device: Some("enp3s0".into()),
                    speed: Some("1000 Mb/s".into()),
                    ipv4: Some("192.0.2.10".into()),
                });
                let mut doc = AutoCommit::new();
                let root = automerge::ROOT;
                pane.sync(&mut doc, &root);
                let _ = doc.put(&root, WIRED_ENABLED_KEY, false);
                pane.sync(&mut doc, &root);
                settle(&mut pane, &mut doc, wakes);
                (rec.calls(), pane.last_wifi_enabled)
            });
            assert_eq!(calls, vec!["set_wired(false)"]);
            assert!(wifi, "LAN must not change the Wi-Fi radio");
        }
    }

    /// A dispatcher whose Wi-Fi switch waits for the test and can fail, so
    /// the pending state and the revert are observed deterministically.
    struct GatedWifi {
        gate: Mutex<std::sync::mpsc::Receiver<Result<(), String>>>,
        calls: Mutex<Vec<String>>,
    }

    impl Dispatcher for GatedWifi {
        fn set_brightness(&self, _p: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&self, _p: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi(&self, enabled: bool) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("set_wifi({enabled})"));
            self.gate
                .lock()
                .unwrap()
                .recv()
                .unwrap_or_else(|_| Err("gate closed".into()))
        }
        fn set_bluetooth(&self, _e: bool) -> Result<(), String> {
            Ok(())
        }
        fn connect_wifi(&self, _ssid: &str, _password: Option<&str>) -> Result<(), String> {
            Ok(())
        }
        fn set_power_profile(&self, _p: os_actions::PowerProfile) -> Result<(), String> {
            Ok(())
        }
    }

    /// Connect, secure prompts, shared passwords, explicit profile sharing, join and collapse.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn wifi_connect_prompts_shares_and_joins_explicitly() {
        // Case `connect_button_invokes_connect_wifi_once`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane = NetworkPane::with_dispatcher(
                rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
                true,
                sample_networks(),
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(
                &root,
                connect_key("Cafe_Guest").as_str(),
                automerge::ScalarValue::Boolean(true),
            );
            pane.sync(&mut doc, &root);
            pane.sync(&mut doc, &root);
            // Connection dispatch deliberately runs off the UI thread. Wait for
            // the recorder exactly as the password-bearing async path below does;
            // asserting immediately races the worker and can falsely report that
            // the visible Connect button did nothing.
            // A hang guard for the worker thread, not a latency budget.
            let deadline = tilo_test_env::hang_deadline();
            while ui::time::now() < deadline {
                if !rec.calls().is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert_eq!(rec.calls(), vec!["connect_wifi(Cafe_Guest)"]);
        }

        // Case `connect_on_secure_network_opens_password_prompt`.
        // Clicking "Connect" on a SECURE network does not connect immediately —
        // it expands the inline password prompt (mirrored into the document)
        // and fires no dispatch yet.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane = NetworkPane::with_dispatcher(
                rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
                true,
                sample_networks(),
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            // Press Connect on the secure "Neighbour_5G".
            let _ = doc.put(&root, connect_key("Neighbour_5G").as_str(), true);
            pane.sync(&mut doc, &root);
            wait_for_password_prompt(&mut pane, &mut doc, &root);
            assert_eq!(pane.expanded_ssid.as_deref(), Some("Neighbour_5G"));
            assert_eq!(
                read_str_or_text(&doc, &root, EXPANDED_KEY).as_deref(),
                Some("Neighbour_5G"),
                "expansion is mirrored into the document so it survives rebuilds"
            );
            assert!(rec.calls().is_empty(), "no connect dispatched before Join");
            assert!(pane.take_structure_dirty(), "expanding requests a rebuild");
        }

        // Case `secure_connect_uses_shared_password_before_prompting`.
        {
            let rec = Arc::new(PwRecorder::default());
            let commands = Arc::new(Mutex::new(Vec::new()));
            let mut pane = NetworkPane::with_dispatcher(
                rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
                true,
                sample_networks(),
            );
            pane.wifi_continuity = Arc::new(SharedPasswordClient {
                commands: Arc::clone(&commands),
            });
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, connect_key("Neighbour_5G"), true);
            pane.sync(&mut doc, &root);
            // A hang guard for the worker thread, not a latency budget.
            let deadline = tilo_test_env::hang_deadline();
            while ui::time::now() < deadline {
                pane.sync(&mut doc, &root);
                if !rec.calls.lock().unwrap().is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert_eq!(
                rec.calls.lock().unwrap().as_slice(),
                ["connect_wifi(Neighbour_5G, pw=shared-secret)"]
            );
            assert!(pane.expanded_ssid.is_none());
            assert!(matches!(
                commands.lock().unwrap().as_slice(),
                [ui::collaboration::WifiContinuityCommand::Lookup { ssid, .. }]
                    if ssid == "Neighbour_5G"
            ));
        }

        // Case `saved_wifi_profile_share_is_explicit_and_uses_only_local_handle`.
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let commands = Arc::new(Mutex::new(Vec::new()));
            let mut pane = NetworkPane::with_dispatcher(
                rec as Arc<dyn Dispatcher + Send + Sync>,
                true,
                sample_networks(),
            );
            pane.profiles = vec![SavedNetworkProfile {
                name: "Studio".into(),
                uuid: "12345678-1234-1234-1234-123456789abc".into(),
                kind: "802-11-wireless".into(),
                autoconnect: true,
            }];
            pane.wifi_continuity = Arc::new(SharedPasswordClient {
                commands: Arc::clone(&commands),
            });
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);

            assert!(commands.lock().unwrap().is_empty(), "sharing is opt-in");
            let _ = doc.put(
                &root,
                profile_share_key("12345678-1234-1234-1234-123456789abc"),
                true,
            );
            pane.sync(&mut doc, &root);
            // A hang guard for the worker thread, not a latency budget.
            let deadline = tilo_test_env::hang_deadline();
            while ui::time::now() < deadline {
                pane.sync(&mut doc, &root);
                if !commands.lock().unwrap().is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert!(matches!(
                commands.lock().unwrap().as_slice(),
                [ui::collaboration::WifiContinuityCommand::ShareSavedProfile { profile_uuid }]
                    if profile_uuid == "12345678-1234-1234-1234-123456789abc"
            ));
        }

        // Case `join_connects_with_typed_password`.
        // Pressing "Join" in the prompt connects WITH the typed password and
        // collapses the prompt.
        {
            let rec = Arc::new(PwRecorder::default());
            let mut pane = NetworkPane::with_dispatcher(
                rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
                true,
                sample_networks(),
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            // Open the prompt, type a password, press Join.
            let _ = doc.put(&root, connect_key("Neighbour_5G").as_str(), true);
            pane.sync(&mut doc, &root);
            wait_for_password_prompt(&mut pane, &mut doc, &root);
            *pane
                .secret_buffers
                .get("Neighbour_5G")
                .unwrap()
                .lock()
                .unwrap() = "hunter2".into();
            let _ = doc.put(&root, join_key("Neighbour_5G").as_str(), true);
            pane.sync(&mut doc, &root);
            // A hang guard for the worker thread, not a latency budget.
            let deadline = tilo_test_env::hang_deadline();
            while ui::time::now() < deadline {
                if !rec.calls.lock().unwrap().is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert_eq!(
                rec.calls.lock().unwrap().as_slice(),
                ["connect_wifi(Neighbour_5G, pw=hunter2)"]
            );
            assert_eq!(pane.expanded_ssid, None, "Join collapses the prompt");
            assert_eq!(
                read_str_or_text(&doc, &root, EXPANDED_KEY).as_deref(),
                Some("")
            );
        }

        // Case `second_connect_press_collapses_prompt`.
        // Pressing Connect a second time on the same expanded network collapses
        // the prompt (it acts as a toggle / Cancel).
        {
            let rec = Arc::new(RecordingDispatcher::default());
            let mut pane = NetworkPane::with_dispatcher(
                rec as Arc<dyn Dispatcher + Send + Sync>,
                true,
                sample_networks(),
            );
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            pane.sync(&mut doc, &root);
            let _ = doc.put(&root, connect_key("Neighbour_5G").as_str(), true);
            pane.sync(&mut doc, &root);
            wait_for_password_prompt(&mut pane, &mut doc, &root);
            assert_eq!(pane.expanded_ssid.as_deref(), Some("Neighbour_5G"));
            let _ = doc.put(&root, connect_key("Neighbour_5G").as_str(), true);
            pane.sync(&mut doc, &root);
            assert_eq!(pane.expanded_ssid, None);
        }
    }

    /// Run `body` on a thread marked as the UI thread, inside a bound wake
    /// scope; `body` receives the wakes the scope delivers.
    fn on_ui_thread<T: Send + 'static>(
        body: impl FnOnce(&std::sync::mpsc::Receiver<()>) -> T + Send + 'static,
    ) -> T {
        ui::rt::spawn_thread("settings-ui-test", move || {
            ui::rt::mark_ui_thread();
            let mut scope = ui::WakeScope::new();
            let (woke, wakes) = std::sync::mpsc::channel();
            scope
                .bind(move |_| {
                    let _ = woke.send(());
                })
                .unwrap();
            let _entry = scope.enter();
            body(&wakes)
        })
        .unwrap()
        .join()
        .unwrap()
    }

    /// A dispatcher that captures the password handed to `connect_wifi`, so
    /// the password-prompt test can assert it actually reached the backend
    /// (the shared `RecordingDispatcher` drops the password).
    #[derive(Default)]
    struct PwRecorder {
        calls: std::sync::Mutex<Vec<String>>,
    }
    impl Dispatcher for PwRecorder {
        fn set_brightness(&self, _p: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_volume(&self, _p: f32) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi(&self, _e: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_bluetooth(&self, _e: bool) -> Result<(), String> {
            Ok(())
        }
        fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!(
                "connect_wifi({ssid}, pw={})",
                password.unwrap_or("<none>")
            ));
            Ok(())
        }
        fn set_power_profile(&self, _p: os_actions::PowerProfile) -> Result<(), String> {
            Ok(())
        }
    }

    /// Wired rows, VPN parsing and rows, and their empty states.
    #[test]
    fn wired_and_vpn_rows_reflect_real_profiles() {
        // Case `wired_rows_reflect_connected_snapshot`.
        {
            let pane = NetworkPane::new_demo(sample_networks()).with_wired(WiredInfo {
                connected: true,
                device: Some("eth0".into()),
                speed: Some("100 Mb/s".into()),
                ipv4: Some("10.0.0.7".into()),
            });
            // The Wired card body builds three rows when connected.
            assert_eq!(pane.wired_rows().len(), 3);
        }

        // Case `wired_rows_show_not_connected_when_down`.
        {
            let pane = NetworkPane::new_demo(sample_networks());
            // Default WiredInfo is disconnected → status + caption only.
            assert_eq!(pane.wired_rows().len(), 2);
        }

        // Case `parse_vpn_connections_filters_to_vpn_types_and_marks_active`.
        {
            let all = "Wired connection 1:ethernet\n\
                   Work VPN:vpn\n\
                   home-wg:wireguard\n\
                   MyWifi:wifi\n";
            let active = "Wired connection 1\nhome-wg\n";
            let vpns = parse_vpn_connections(all, active);
            assert_eq!(vpns.len(), 2, "only vpn + wireguard rows: {vpns:?}");
            let work = vpns.iter().find(|v| v.name == "Work VPN").unwrap();
            let wg = vpns.iter().find(|v| v.name == "home-wg").unwrap();
            assert!(!work.active, "Work VPN isn't in the active set");
            assert!(wg.active, "home-wg is up");
        }

        // Case `vpn_rows_reflect_configured_profiles`.
        {
            let pane = NetworkPane::new_demo(sample_networks()).with_vpns(vec![
                VpnInfo {
                    name: "Work VPN".into(),
                    active: true,
                },
                VpnInfo {
                    name: "Home WG".into(),
                    active: false,
                },
            ]);
            assert_eq!(pane.vpns().len(), 2);
            // Two status rows.
            assert_eq!(pane.vpn_rows().len(), 2);
        }

        // Case `vpn_rows_show_empty_state_when_none`.
        {
            let pane = NetworkPane::new_demo(sample_networks());
            // Empty-state explanation plus the single Studio handoff.
            assert_eq!(pane.vpn_rows().len(), 2);
        }
    }
}
