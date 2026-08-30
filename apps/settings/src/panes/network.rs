//! Network pane — Wi-Fi radio toggle, scanned network list, refresh
//! button, connection-status row.
//!
//! Plumbs `os_actions::network::{get_wifi, set_wifi,
//! list_wifi_networks, connect_wifi}`. The network list is sampled once
//! on construction (cached) and refreshed when the user clicks the
//! `Refresh` button — re-scanning every frame would be wasteful and
//! visibly stutter the UI on slow nmcli backends.
//
// CRDT keys:
//   `network.wifi_enabled` — bool, wifi toggle
//   `network.refresh`      — bool, click sets true; pane resets on
//                             sync after rescan
//   `network.connect.<ssid>` — bool, click sets true; pane resets on
//                             sync after dispatch

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
use winit::event::{Modifiers, WindowEvent};
use zeroize::{Zeroize, Zeroizing};

use crate::dispatcher::dispatcher_or_real;
use crate::widgets::{
    El, caption, card, card_with_action, choice_row, rows, setting_row, value_row,
};

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
pub const ENTERPRISE_CA_CHOOSE_KEY: &str = "network.enterprise.ca.choose";
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

/// Nerd Font (Material Design Icons) Wi-Fi signal-strength glyph for a 0..=4
/// bar level. These are the same MDI `wifi-strength-*` family the sidebar's
/// Network icon draws, so they render as real signal arcs. Unicode
/// block-element bars (`▁▂▄▆█`) have no glyph in the shipped fonts and fall
/// back to a `.notdef` tofu box — do not substitute those here.
fn wifi_signal_glyph(signal: u8) -> &'static str {
    match signal {
        0 => "\u{f092b}", // wifi-strength-outline (no bars)
        1 => "\u{f091f}", // wifi-strength-1
        2 => "\u{f0922}", // wifi-strength-2
        3 => "\u{f0925}", // wifi-strength-3
        _ => "\u{f0928}", // wifi-strength-4 (full)
    }
}

/// Read a boolean scalar at `key`, defaulting to `false`.
fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    matches!(
        state.get(root, key),
        Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
    )
}

/// Read a string value at `key` — handles both a plain scalar string and
/// an Automerge `Text` object (which is what `TextInput` writes through).
fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
        _ => None,
    }
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
    MobileProfile,
    SimUnlock,
    EnterpriseWifi,
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
    /// Configured VPN connections (read-only status), sampled at
    /// construction and on refresh.
    vpns: Vec<VpnInfo>,
    vpn_lister: VpnLister,
    tool_opener: ToolOpener,
    initial_wifi_enabled: bool,
    last_wifi_enabled: bool,
    needs_seed: bool,
    /// Track which `network.join.<ssid>` keys we already dispatched
    /// for, so a single button press doesn't re-fire on every frame.
    dispatched_connects: Vec<String>,
    connection_tx: std::sync::mpsc::Sender<(NetworkJob, Result<(), String>)>,
    connection_rx: std::sync::mpsc::Receiver<(NetworkJob, Result<(), String>)>,
    connection_notice: Option<String>,
    ca_picker_rx: Option<std::sync::mpsc::Receiver<Result<Option<String>, String>>>,
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
    pub fn new() -> Self {
        let (connection_tx, connection_rx) = std::sync::mpsc::channel();
        let lister: NetworkLister = Arc::new(os_actions::list_wifi_networks);
        let networks = lister().unwrap_or_default();
        let initial_wifi = os_actions::get_wifi().unwrap_or(true);
        let wired = os_actions::get_wired().unwrap_or_default();
        let radios = os_actions::get_radio_state().unwrap_or_default();
        let connectivity = os_actions::get_connectivity().unwrap_or_default();
        let profiles = os_actions::list_saved_network_profiles().unwrap_or_default();
        let vpn_lister: VpnLister = Arc::new(read_vpn_connections);
        let vpns = vpn_lister();
        Self {
            dispatcher: dispatcher_or_real(None),
            lister,
            networks,
            wired,
            radios,
            connectivity,
            profiles,
            vpns,
            vpn_lister,
            tool_opener: Arc::new(os_actions::open_tool),
            initial_wifi_enabled: initial_wifi,
            last_wifi_enabled: initial_wifi,
            needs_seed: true,
            dispatched_connects: Vec::new(),
            connection_tx,
            connection_rx,
            connection_notice: None,
            ca_picker_rx: None,
            hotspot_password: Arc::new(Mutex::new(String::new())),
            hotspot_ssid: "qOS Hotspot".into(),
            hotspot_band: "Automatic".into(),
            hotspot_qr_visible: false,
            hotspot_clients: os_actions::list_hotspot_clients().unwrap_or_default(),
            secret_buffers: initial_secret_buffers(),
            expanded_ssid: None,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test-only constructor that pins the dispatcher, the wifi-state,
    /// and the network-list source. The list is captured into the
    /// pane's cache immediately.
    pub fn with_dispatcher(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        initial_wifi_enabled: bool,
        networks: Vec<WifiNetworkInfo>,
    ) -> Self {
        let (connection_tx, connection_rx) = std::sync::mpsc::channel();
        let snapshot = networks.clone();
        let lister: NetworkLister = Arc::new(move || Ok(snapshot.clone()));
        Self {
            dispatcher,
            lister,
            networks,
            wired: WiredInfo::default(),
            radios: os_actions::RadioState::default(),
            connectivity: ConnectivityState::Unknown,
            profiles: Vec::new(),
            vpns: Vec::new(),
            vpn_lister: Arc::new(Vec::<VpnInfo>::new),
            tool_opener: Arc::new(os_actions::open_tool),
            initial_wifi_enabled,
            last_wifi_enabled: initial_wifi_enabled,
            needs_seed: true,
            dispatched_connects: Vec::new(),
            connection_tx,
            connection_rx,
            connection_notice: None,
            ca_picker_rx: None,
            hotspot_password: Arc::new(Mutex::new(String::new())),
            hotspot_ssid: "qOS Hotspot".into(),
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

    /// `--shots` helper: a pane with a fixed network list and the real
    /// dispatcher (clicks would shell out, but the render never clicks).
    pub fn new_demo(networks: Vec<WifiNetworkInfo>) -> Self {
        let (connection_tx, connection_rx) = std::sync::mpsc::channel();
        let snapshot = networks.clone();
        let lister: NetworkLister = Arc::new(move || Ok(snapshot.clone()));
        Self {
            dispatcher: dispatcher_or_real(None),
            lister,
            networks,
            wired: WiredInfo::default(),
            radios: os_actions::RadioState::default(),
            connectivity: ConnectivityState::Full,
            profiles: Vec::new(),
            vpns: Vec::new(),
            vpn_lister: Arc::new(Vec::<VpnInfo>::new),
            tool_opener: Arc::new(os_actions::open_tool),
            initial_wifi_enabled: true,
            last_wifi_enabled: true,
            needs_seed: true,
            dispatched_connects: Vec::new(),
            connection_tx,
            connection_rx,
            connection_notice: None,
            ca_picker_rx: None,
            hotspot_password: Arc::new(Mutex::new(String::new())),
            hotspot_ssid: "qOS Hotspot".into(),
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

    /// Test/`--shots` helper: pin the wired snapshot so the Wired card
    /// renders deterministic content.
    pub fn with_wired(mut self, wired: WiredInfo) -> Self {
        self.wired = wired;
        self
    }

    /// Test/`--shots` helper: pin the VPN list so the VPN card renders
    /// deterministic content. Also pins the refresh lister to the same set.
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

    fn vpn_key(name: &str) -> String {
        format!("network.vpn.{name}")
    }

    fn vpn_studio_key(name: &str) -> String {
        format!("network.vpn.studio.{name}")
    }

    const VPN_STUDIO_OVERVIEW_KEY: &'static str = "network.vpn.studio.open";

    /// Rows for the VPN card use the same NetworkManager profiles and
    /// action boundary as quick settings.
    fn vpn_rows(&self) -> Vec<El> {
        if self.vpns.is_empty() {
            return vec![
                caption(
                    "No VPN profiles yet. VPN Studio can import WireGuard and OpenVPN configurations.",
                ),
                Box::new(
                    Row::new()
                        .push(Box::new(Spacer::new()))
                        .push(Box::new(Button::new(
                            "settings.network.vpn.studio.open",
                            "Open VPN Studio",
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
                    .push(Box::new(Text::new(&v.name).with_size(13.0)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(Button::new(
                        &format!("settings.network.vpn.studio.{}", v.name),
                        "Advanced…",
                        Some(&Self::vpn_studio_key(&v.name)),
                    )))
                    .push(Box::new(Toggle::new(&id, &key))),
            ));
        }
        out.push(caption(
            "Connect here day to day. Use VPN Studio only for imports, DNS, routing, and kill-switch rules.",
        ));
        out
    }

    /// Test/`--shots` helper: pre-expand a network's inline password prompt
    /// so the deterministic render shows it. Also forces the radio "on" so
    /// the network list renders.
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
            return "Wi-Fi is off".to_string();
        }
        match self.networks.iter().find(|n| n.connected) {
            Some(n) => format!("Connected to {}", n.ssid),
            None => "Not connected".to_string(),
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // --- Wi-Fi card: header carries the toggle; body is status +
        //     scanned-network rows + a Refresh action.
        let wifi_toggle: El = Box::new(
            Toggle::new("settings.network.wifi", WIFI_ENABLED_KEY).with_id("network.wifi"),
        );

        let mut wifi_rows: Vec<El> = Vec::new();
        wifi_rows.push(value_row("Status", &self.connection_status_text()));
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
        wifi_rows.push(Box::new(Row::new().push(Box::new(Spacer::new())).push(
            Box::new(Button::new(
                "settings.network.refresh",
                "\u{f0450}  Refresh",
                Some(REFRESH_KEY),
            )),
        )));
        wifi_rows.push(caption(
            "Wi-Fi scanning and connection are best-effort via nmcli; \
             secure networks prompt for a password before joining.",
        ));

        let wifi_card = card_with_action("Wi-Fi", wifi_toggle, rows(wifi_rows));

        // --- Wired card (real nmcli snapshot). NetworkManager applies the
        // switch to this interface only; Wi-Fi and VPN stay untouched.
        let wired_card = if self.wired.device.is_some() {
            card_with_action(
                "Wired",
                Box::new(
                    Toggle::new("settings.network.wired", WIRED_ENABLED_KEY)
                        .with_id("network.wired"),
                ),
                rows(self.wired_rows()),
            )
        } else {
            card("Wired", rows(self.wired_rows()))
        };

        // --- VPN card (real read-only status of configured profiles).
        let vpn_card = card("VPN", rows(self.vpn_rows()));

        let airplane = card_with_action(
            "Airplane mode",
            Box::new(Toggle::new("settings.network.airplane", AIRPLANE_KEY)),
            rows(vec![caption(
                "Disables Wi-Fi and mobile broadband together.",
            )]),
        );
        let hotspot_secret = self
            .hotspot_password
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        let mut hotspot_rows: Vec<El> = vec![
            value_row(
                "Status",
                if self.radios.hotspot_active {
                    "Active"
                } else {
                    "Off"
                },
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.hotspot.ssid")
                    .with_on_change_path(HOTSPOT_SSID_KEY)
                    .with_placeholder("Hotspot name"),
            ),
            Box::new(
                TextInput::local()
                    .with_id("network.hotspot.password")
                    .with_external_buffer(Arc::clone(&self.hotspot_password))
                    .with_initial_value(&hotspot_secret)
                    .with_secret(true)
                    .with_placeholder("Password · at least 8 characters"),
            ),
            choice_row(
                "Band",
                Box::new(
                    Dropdown::new(
                        ["Automatic", "2.4 GHz", "5 GHz"]
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                        HOTSPOT_BAND_KEY,
                    )
                    .with_id("network.hotspot.band")
                    .with_placeholder("Automatic"),
                ),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.hotspot.apply",
                        "Apply and start",
                        Some(HOTSPOT_APPLY_KEY),
                    )))
                    .push(Box::new(Button::new(
                        "settings.network.hotspot.qr",
                        if self.hotspot_qr_visible {
                            "Hide QR"
                        } else {
                            "Share QR"
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
                hotspot_rows.push(caption(
                    "Scan to join. The QR code contains the hotspot password.",
                ));
            }
        }
        if self.radios.hotspot_active {
            hotspot_rows.push(value_row(
                "Connected devices",
                &self.hotspot_clients.len().to_string(),
            ));
            for client in &self.hotspot_clients {
                let signal = client
                    .signal_dbm
                    .map_or_else(|| "signal unknown".to_string(), |dbm| format!("{dbm} dBm"));
                hotspot_rows.push(value_row(&client.mac, &signal));
            }
            hotspot_rows.push(caption(
                "Client names are not guessed from traffic. Turn the hotspot off to disconnect every client immediately.",
            ));
        }
        let hotspot = card_with_action(
            "Wi-Fi hotspot",
            Box::new(Toggle::new("settings.network.hotspot", HOTSPOT_KEY)),
            rows(hotspot_rows),
        );
        let profile_card = card("Connection profiles", rows(self.profile_rows()));
        let mut cards = vec![
            card(
                "Internet access",
                rows({
                    let mut content = vec![
                        value_row("Connectivity", self.connectivity.label()),
                        caption(if self.connectivity == ConnectivityState::Portal {
                            "This network requires browser sign-in. NetworkManager will keep it below a fully connected route until sign-in succeeds."
                        } else {
                            "Connectivity comes from NetworkManager's captive-portal check, not merely from having an IP address."
                        }),
                    ];
                    if self.connectivity == ConnectivityState::Portal {
                        content.push(Box::new(
                            Row::new()
                                .push(Box::new(Button::new(
                                    "settings.network.captive-portal",
                                    "Open sign-in page",
                                    Some(CAPTIVE_PORTAL_KEY),
                                )))
                                .push(Box::new(Spacer::new())),
                        ));
                    }
                    content
                }),
            ),
            wifi_card,
            card("Enterprise Wi-Fi", rows(self.enterprise_rows())),
            wired_card,
            profile_card,
            vpn_card,
            airplane,
            hotspot,
        ];
        if self.radios.wwan_available {
            cards.push(card_with_action(
                "Mobile broadband",
                Box::new(Toggle::new("settings.network.mobile", MOBILE_KEY)),
                rows(self.mobile_rows()),
            ));
        }
        crate::widgets::cards(cards)
    }

    fn profile_rows(&self) -> Vec<El> {
        let wifi_profiles: Vec<&SavedNetworkProfile> = self
            .profiles
            .iter()
            .filter(|profile| profile.kind == "802-11-wireless")
            .collect();
        if wifi_profiles.is_empty() {
            return vec![caption("No saved Wi-Fi networks.")];
        }
        let mut rows_out: Vec<El> = Vec::new();
        for profile in wifi_profiles {
            rows_out.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(&profile.name).with_size(13.0)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(Button::new(
                        &format!("settings.network.profile.forget.{}", profile.uuid),
                        "Forget",
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
        rows_out.push(caption(
            "Auto-connect and Forget modify the NetworkManager profile itself; Settings keeps no duplicate profile database.",
        ));
        rows_out
    }

    fn secret_field(&self, key: &str, id: &str, placeholder: &str) -> El {
        let buffer = self
            .secret_buffers
            .get(key)
            .cloned()
            .expect("fixed secret buffer must be initialized");
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
        vec![
            caption(
                "Create the carrier profile through NetworkManager. The APN is supplied by your carrier.",
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.mobile.profile")
                    .with_on_change_path(MOBILE_PROFILE_KEY)
                    .with_placeholder("Profile name"),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.mobile.apn")
                    .with_on_change_path(MOBILE_APN_KEY)
                    .with_placeholder("Access point name (APN)"),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.mobile.create",
                        "Create profile",
                        Some(MOBILE_CREATE_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ),
            caption(
                "If the SIM is locked, enter its PIN. It is sent directly to ModemManager over the system bus and is never saved.",
            ),
            self.secret_field(MOBILE_PIN_BUFFER, "network.mobile.pin", "SIM PIN"),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.mobile.unlock",
                        "Unlock SIM",
                        Some(MOBILE_UNLOCK_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ),
        ]
    }

    fn enterprise_rows(&self) -> Vec<El> {
        vec![
            caption(
                "PEAP with MSCHAPv2. Certificate validation is mandatory; ask your organization for its CA certificate.",
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.profile")
                    .with_on_change_path(ENTERPRISE_PROFILE_KEY)
                    .with_placeholder("Profile name"),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.ssid")
                    .with_on_change_path(ENTERPRISE_SSID_KEY)
                    .with_placeholder("Network name (SSID)"),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.identity")
                    .with_on_change_path(ENTERPRISE_IDENTITY_KEY)
                    .with_placeholder("Username / identity"),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.anonymous")
                    .with_on_change_path(ENTERPRISE_ANONYMOUS_KEY)
                    .with_placeholder("Anonymous identity (optional)"),
            ),
            Box::new(
                TextInput::new(Vec::new())
                    .with_id("network.enterprise.ca")
                    .with_on_change_path(ENTERPRISE_CA_KEY)
                    .with_placeholder("Absolute path to CA certificate"),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.enterprise.ca.choose",
                        "Choose CA certificate…",
                        Some(ENTERPRISE_CA_CHOOSE_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ),
            self.secret_field(
                ENTERPRISE_PASSWORD_BUFFER,
                "network.enterprise.password",
                "Password",
            ),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.network.enterprise.connect",
                        "Save and connect",
                        Some(ENTERPRISE_CONNECT_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ),
        ]
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
        let band = match band {
            "2.4 GHz" => HotspotBand::TwoPointFourGhz,
            "5 GHz" => HotspotBand::FiveGhz,
            _ => HotspotBand::Automatic,
        };
        let config = HotspotConfiguration {
            ssid: ssid.to_string(),
            password,
            band,
        };
        config.validate()?;
        Ok(config)
    }

    /// One scanned-network row: signal + lock glyph + SSID on the left, a
    /// Connect / Connected button on the right.
    fn network_row(&self, net: &WifiNetworkInfo) -> El {
        let signal_glyph = wifi_signal_glyph(net.signal);
        // A closed padlock for secure networks, its open-padlock pair for open
        // ones — both from the MDI `lock` family the shipped fonts carry, so the
        // security state reads at a glance without a stray unrelated glyph.
        let lock_glyph = if net.secure { "\u{f033e}" } else { "\u{f033f}" };
        let label = format!("{signal_glyph}  {lock_glyph}  {ssid}", ssid = net.ssid);
        let key = connect_key(&net.ssid);
        let cta_id = format!("settings.network.connect.{}", net.ssid);
        let cta_label = if net.connected {
            "Connected"
        } else if self.expanded_ssid.as_deref() == Some(net.ssid.as_str()) {
            "Cancel"
        } else {
            "Connect"
        };
        Box::new(
            Row::new()
                .push(Box::new(Text::new(&label).with_size(13.0)))
                .push(Box::new(Spacer::new()))
                .push(Box::new(Button::new(&cta_id, cta_label, Some(&key)))),
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
            .with_placeholder("Password")
            .with_font_size(13.0);
        Box::new(
            Row::new()
                .push(Box::new(field))
                .push(Box::new(Spacer::new()))
                .push(Box::new(Button::new(&join_id, "Join", Some(&jk)))),
        )
    }

    /// Rows for the Wired card, derived from the live `nmcli` snapshot.
    fn wired_rows(&self) -> Vec<El> {
        if !self.wired.connected {
            return vec![
                value_row("Ethernet", "Not connected"),
                caption("Plug in an Ethernet cable to use a wired connection."),
            ];
        }
        let mut out = Vec::new();
        let status = match &self.wired.speed {
            Some(s) => format!("Connected · {s}"),
            None => "Connected".to_string(),
        };
        out.push(value_row("Ethernet", &status));
        if let Some(dev) = &self.wired.device {
            out.push(value_row("Interface", dev));
        }
        out.push(value_row("IPv4", self.wired.ipv4.as_deref().unwrap_or("—")));
        if self.profiles.iter().any(|p| p.kind == "802-3-ethernet") {
            out.extend([
                setting_row(
                    "Automatic IPv4",
                    Box::new(Toggle::new(
                        "settings.network.ipv4.automatic",
                        IPV4_AUTOMATIC_KEY,
                    )),
                ),
                Box::new(
                    TextInput::new(Vec::new())
                        .with_id("network.ipv4.address")
                        .with_on_change_path(IPV4_ADDRESS_KEY)
                        .with_placeholder("Address/prefix · e.g. 192.0.2.10/24"),
                ),
                Box::new(
                    TextInput::new(Vec::new())
                        .with_id("network.ipv4.gateway")
                        .with_on_change_path(IPV4_GATEWAY_KEY)
                        .with_placeholder("Gateway"),
                ),
                Box::new(
                    TextInput::new(Vec::new())
                        .with_id("network.ipv4.dns")
                        .with_on_change_path(IPV4_DNS_KEY)
                        .with_placeholder("DNS servers · comma-separated"),
                ),
                Box::new(
                    Row::new()
                        .push(Box::new(Button::new(
                            "settings.network.ipv4.apply",
                            "Apply IP settings",
                            Some(IPV4_APPLY_KEY),
                        )))
                        .push(Box::new(Spacer::new())),
                ),
                caption("NetworkManager validates and applies this profile. Custom DNS disables DHCP-provided DNS for this connection only."),
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
    fn do_connect(&mut self, ssid: &str, password: Option<&str>) {
        if self.dispatched_connects.iter().any(|s| s == ssid) {
            return;
        }
        self.dispatched_connects.push(ssid.to_string());
        self.connection_notice = Some(format!("Connecting to {ssid}…"));
        let dispatcher = Arc::clone(&self.dispatcher);
        let tx = self.connection_tx.clone();
        let ssid = ssid.to_string();
        let password = password.map(|value| Zeroizing::new(value.to_string()));
        std::thread::spawn(move || {
            let result = dispatcher.connect_wifi(&ssid, password.as_deref().map(String::as_str));
            let _ = tx.send((NetworkJob::Wifi(ssid), result));
        });
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(NetworkPane::new())
}

impl Widget for NetworkPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(receiver) = &self.ca_picker_rx
            && let Ok(result) = receiver.try_recv()
        {
            match result {
                Ok(Some(path)) => {
                    let object = crate::ensure_text_object(state, root, ENTERPRISE_CA_KEY);
                    let old_len =
                        state.text(&object).map_or(0, |text| text.chars().count()) as isize;
                    let _ = state.splice_text(&object, 0, old_len, &path);
                    self.connection_notice = Some("CA certificate selected.".into());
                }
                Ok(None) => {}
                Err(error) => {
                    self.connection_notice = Some(format!("Could not choose certificate: {error}"));
                }
            }
            self.ca_picker_rx = None;
            self.structure_dirty = true;
        }
        while let Ok((job, result)) = self.connection_rx.try_recv() {
            let (subject, success) = match &job {
                NetworkJob::Wifi(ssid) => (ssid.as_str(), format!("Connected to {ssid}.")),
                NetworkJob::MobileProfile => ("mobile profile", "Mobile profile created.".into()),
                NetworkJob::SimUnlock => ("SIM", "SIM unlocked.".into()),
                NetworkJob::EnterpriseWifi => {
                    ("enterprise Wi-Fi", "Enterprise Wi-Fi connected.".into())
                }
            };
            if let NetworkJob::Wifi(ssid) = &job {
                self.dispatched_connects.retain(|pending| pending != ssid);
            }
            match result {
                Ok(()) => {
                    self.connection_notice = Some(success);
                    if let Ok(networks) = (self.lister)() {
                        self.networks = networks;
                    }
                    self.profiles = os_actions::list_saved_network_profiles()
                        .unwrap_or_else(|_| self.profiles.clone());
                }
                Err(error) => {
                    self.connection_notice =
                        Some(format!("Could not configure {subject}: {error}"));
                    warn!(target: "qos::settings", op = "network_job", %subject, %error, "network pane operation failed");
                }
            }
            self.structure_dirty = true;
        }
        if self.needs_seed {
            // Seed only keys the document doesn't already carry, so a
            // structure rebuild (which reconstructs this pane and re-runs
            // the seed) never clobbers a value the user already changed —
            // e.g. toggling Wi-Fi off and then expanding a password prompt.
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
            let _ = state.put(root, Self::VPN_STUDIO_OVERVIEW_KEY, false);
            let _ = state.put(root, HOTSPOT_APPLY_KEY, false);
            let _ = state.put(root, HOTSPOT_QR_KEY, false);
            let _ = state.put(root, HOTSPOT_SSID_KEY, self.hotspot_ssid.as_str());
            let _ = crate::ensure_text_object(state, root, HOTSPOT_SSID_KEY);
            let _ = state.put(root, HOTSPOT_BAND_KEY, self.hotspot_band.as_str());
            let _ = state.put(root, IPV4_AUTOMATIC_KEY, true);
            for key in [IPV4_ADDRESS_KEY, IPV4_GATEWAY_KEY, IPV4_DNS_KEY] {
                let _ = crate::ensure_text_object(state, root, key);
            }
            let _ = state.put(root, IPV4_APPLY_KEY, false);
            for key in [
                MOBILE_PROFILE_KEY,
                MOBILE_APN_KEY,
                ENTERPRISE_PROFILE_KEY,
                ENTERPRISE_SSID_KEY,
                ENTERPRISE_IDENTITY_KEY,
                ENTERPRISE_ANONYMOUS_KEY,
                ENTERPRISE_CA_KEY,
            ] {
                let _ = crate::ensure_text_object(state, root, key);
            }
            for key in [
                MOBILE_CREATE_KEY,
                MOBILE_UNLOCK_KEY,
                ENTERPRISE_CONNECT_KEY,
                ENTERPRISE_CA_CHOOSE_KEY,
                CAPTIVE_PORTAL_KEY,
            ] {
                let _ = state.put(root, key, false);
            }
            for profile in &self.profiles {
                let _ = state.put(
                    root,
                    profile_autoconnect_key(&profile.uuid),
                    profile.autoconnect,
                );
                let _ = state.put(root, profile_forget_key(&profile.uuid), false);
            }
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

        // Wi-Fi toggle.
        if let Ok(Some((val, _))) = state.get(root, WIFI_ENABLED_KEY)
            && let Value::Scalar(s) = val
            && let automerge::ScalarValue::Boolean(b) = s.as_ref()
            && *b != self.last_wifi_enabled
        {
            self.last_wifi_enabled = *b;
            if let Err(e) = self.dispatcher.set_wifi(*b) {
                warn!(target: "qos::settings", op = "set_wifi", error = %e, "network pane dispatch failed");
            }
        }

        // Wired interface toggle. Missing hardware has no switch, and this
        // never uses NetworkManager's global networking radio.
        if let Ok(Some((Value::Scalar(s), _))) = state.get(root, WIRED_ENABLED_KEY)
            && let automerge::ScalarValue::Boolean(enabled) = s.as_ref()
            && *enabled != self.wired.connected
            && self.wired.device.is_some()
        {
            match self.dispatcher.set_wired(*enabled) {
                Ok(()) => self.wired.connected = *enabled,
                Err(e) => {
                    warn!(target: "qos::settings", op = "set_wired", error = %e, "network pane dispatch failed");
                    let _ = state.put(
                        root,
                        WIRED_ENABLED_KEY,
                        automerge::ScalarValue::Boolean(self.wired.connected),
                    );
                }
            }
        }

        for (key, old, op) in [
            (AIRPLANE_KEY, self.radios.airplane_mode, "airplane"),
            (HOTSPOT_KEY, self.radios.hotspot_active, "hotspot"),
            (MOBILE_KEY, self.radios.wwan_enabled, "mobile"),
        ] {
            let Some(enabled) = read_bool(state, root, key).then_some(true).or_else(|| {
                state
                    .get(root, key)
                    .ok()
                    .flatten()
                    .and_then(|(v, _)| match v {
                        Value::Scalar(s) => match s.as_ref() {
                            automerge::ScalarValue::Boolean(b) => Some(*b),
                            _ => None,
                        },
                        _ => None,
                    })
            }) else {
                continue;
            };
            if enabled == old {
                continue;
            }
            let result = match op {
                "airplane" => self.dispatcher.set_airplane_mode(enabled),
                "hotspot" => self.dispatcher.set_hotspot(enabled),
                _ => self.dispatcher.set_mobile_broadband(enabled),
            };
            match result {
                Ok(()) => match op {
                    "airplane" => self.radios.airplane_mode = enabled,
                    "hotspot" => self.radios.hotspot_active = enabled,
                    _ => self.radios.wwan_enabled = enabled,
                },
                Err(e) => {
                    warn!(target: "qos::settings", op, error = %e, "radio action failed");
                    let _ = state.put(root, key, old);
                }
            }
        }
        for vpn in &mut self.vpns {
            let key = Self::vpn_key(&vpn.name);
            let enabled = read_bool(state, root, &key);
            if enabled != vpn.active {
                match self.dispatcher.set_vpn_connection(&vpn.name, enabled) {
                    Ok(()) => vpn.active = enabled,
                    Err(e) => {
                        warn!(target: "qos::settings", op = "set_vpn_connection", profile = %vpn.name, error = %e, "VPN action failed");
                        let _ = state.put(root, key, vpn.active);
                    }
                }
            }
        }

        let profile_snapshot = self.profiles.clone();
        for profile in profile_snapshot {
            let auto_key = profile_autoconnect_key(&profile.uuid);
            if let Some(enabled) =
                state
                    .get(root, &auto_key)
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| match value {
                        Value::Scalar(s) => match s.as_ref() {
                            automerge::ScalarValue::Boolean(value) => Some(*value),
                            _ => None,
                        },
                        _ => None,
                    })
                && enabled != profile.autoconnect
            {
                match os_actions::set_network_autoconnect(&profile.uuid, enabled) {
                    Ok(()) => {
                        if let Some(saved) =
                            self.profiles.iter_mut().find(|p| p.uuid == profile.uuid)
                        {
                            saved.autoconnect = enabled;
                        }
                    }
                    Err(error) => {
                        self.connection_notice =
                            Some(format!("Could not update {}: {error}", profile.name));
                        let _ = state.put(root, auto_key, profile.autoconnect);
                    }
                }
            }
            let forget_key = profile_forget_key(&profile.uuid);
            if read_bool(state, root, &forget_key) {
                let _ = state.put(root, &forget_key, false);
                match os_actions::forget_network_profile(&profile.uuid) {
                    Ok(()) => {
                        self.profiles.retain(|p| p.uuid != profile.uuid);
                        self.connection_notice = Some(format!("Forgot {}.", profile.name));
                    }
                    Err(error) => {
                        self.connection_notice =
                            Some(format!("Could not forget {}: {error}", profile.name));
                    }
                }
                self.structure_dirty = true;
            }
        }

        if read_bool(state, root, HOTSPOT_APPLY_KEY) {
            let _ = state.put(root, HOTSPOT_APPLY_KEY, false);
            self.hotspot_ssid = read_str(state, root, HOTSPOT_SSID_KEY)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "qOS Hotspot".into());
            self.hotspot_band =
                read_str(state, root, HOTSPOT_BAND_KEY).unwrap_or_else(|| "Automatic".into());
            match self.hotspot_config_from_values(&self.hotspot_ssid, &self.hotspot_band) {
                Ok(config) => match os_actions::configure_hotspot(&config) {
                    Ok(()) => {
                        self.radios.hotspot_active = true;
                        self.hotspot_clients =
                            os_actions::list_hotspot_clients().unwrap_or_default();
                        let _ = state.put(root, HOTSPOT_KEY, true);
                        self.connection_notice = Some("Hotspot is active.".into());
                    }
                    Err(error) => self.connection_notice = Some(format!("Hotspot failed: {error}")),
                },
                Err(error) => self.connection_notice = Some(error),
            }
            self.structure_dirty = true;
        }
        if read_bool(state, root, HOTSPOT_QR_KEY) {
            let _ = state.put(root, HOTSPOT_QR_KEY, false);
            self.hotspot_ssid = read_str(state, root, HOTSPOT_SSID_KEY)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "qOS Hotspot".into());
            self.hotspot_band =
                read_str(state, root, HOTSPOT_BAND_KEY).unwrap_or_else(|| "Automatic".into());
            match self.hotspot_config_from_values(&self.hotspot_ssid, &self.hotspot_band) {
                Ok(_) => self.hotspot_qr_visible = !self.hotspot_qr_visible,
                Err(error) => self.connection_notice = Some(error),
            }
            self.structure_dirty = true;
        }
        if read_bool(state, root, IPV4_APPLY_KEY) {
            let _ = state.put(root, IPV4_APPLY_KEY, false);
            if let Some(profile) = self.profiles.iter().find(|p| p.kind == "802-3-ethernet") {
                let config = IpConfiguration {
                    automatic: read_bool(state, root, IPV4_AUTOMATIC_KEY),
                    addresses: read_str(state, root, IPV4_ADDRESS_KEY)
                        .into_iter()
                        .flat_map(|value| {
                            value
                                .split(',')
                                .map(str::trim)
                                .filter(|v| !v.is_empty())
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .collect(),
                    gateway: read_str(state, root, IPV4_GATEWAY_KEY)
                        .filter(|v| !v.trim().is_empty()),
                    dns: read_str(state, root, IPV4_DNS_KEY)
                        .into_iter()
                        .flat_map(|value| {
                            value
                                .split(',')
                                .map(str::trim)
                                .filter(|v| !v.is_empty())
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .collect(),
                };
                match os_actions::configure_ipv4(&profile.uuid, &config) {
                    Ok(()) => {
                        self.connection_notice =
                            Some(format!("Applied IP settings to {}.", profile.name))
                    }
                    Err(error) => {
                        self.connection_notice =
                            Some(format!("IP settings were not applied: {error}"))
                    }
                }
                self.structure_dirty = true;
            }
        }
        if read_bool(state, root, MOBILE_CREATE_KEY) {
            let _ = state.put(root, MOBILE_CREATE_KEY, false);
            let name = read_str(state, root, MOBILE_PROFILE_KEY).unwrap_or_default();
            let apn = read_str(state, root, MOBILE_APN_KEY).unwrap_or_default();
            let tx = self.connection_tx.clone();
            self.connection_notice = Some("Creating mobile profile…".into());
            std::thread::spawn(move || {
                let result = os_actions::create_mobile_profile(&name, &apn);
                let _ = tx.send((NetworkJob::MobileProfile, result));
            });
        }
        if read_bool(state, root, MOBILE_UNLOCK_KEY) {
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
            self.connection_notice = Some("Unlocking SIM…".into());
            std::thread::spawn(move || {
                let result = os_actions::unlock_mobile_sim(&pin);
                let _ = tx.send((NetworkJob::SimUnlock, result));
            });
            self.structure_dirty = true;
        }
        if read_bool(state, root, ENTERPRISE_CONNECT_KEY) {
            let _ = state.put(root, ENTERPRISE_CONNECT_KEY, false);
            let config = EnterpriseWifiConfiguration {
                profile_name: read_str(state, root, ENTERPRISE_PROFILE_KEY).unwrap_or_default(),
                ssid: read_str(state, root, ENTERPRISE_SSID_KEY).unwrap_or_default(),
                identity: read_str(state, root, ENTERPRISE_IDENTITY_KEY).unwrap_or_default(),
                anonymous_identity: read_str(state, root, ENTERPRISE_ANONYMOUS_KEY)
                    .filter(|value| !value.trim().is_empty()),
                ca_certificate: read_str(state, root, ENTERPRISE_CA_KEY).unwrap_or_default(),
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
                let _ = tx.send((NetworkJob::EnterpriseWifi, result));
            });
            self.structure_dirty = true;
        }
        if read_bool(state, root, ENTERPRISE_CA_CHOOSE_KEY) {
            let _ = state.put(root, ENTERPRISE_CA_CHOOSE_KEY, false);
            if self.ca_picker_rx.is_none() {
                let (tx, rx) = std::sync::mpsc::channel();
                self.ca_picker_rx = Some(rx);
                std::thread::spawn(move || {
                    let result =
                        runtime_portal_xdg::FileChooser::open("Choose organization CA certificate")
                            .map(|files| {
                                files
                                    .into_iter()
                                    .next()
                                    .and_then(|file| file.path)
                                    .map(|path| path.display().to_string())
                            })
                            .map_err(|error| error.to_string());
                    let _ = tx.send(result);
                });
            }
        }
        if read_bool(state, root, CAPTIVE_PORTAL_KEY) {
            let _ = state.put(root, CAPTIVE_PORTAL_KEY, false);
            let result = os_actions::captive_portal_uri().and_then(|uri| {
                runtime_portal_xdg::OpenUri::open(&uri).map_err(|error| {
                    format!("could not open browser through the desktop portal: {error}")
                })
            });
            if let Err(error) = result {
                self.connection_notice = Some(format!("Could not open network sign-in: {error}"));
                self.structure_dirty = true;
            }
        }

        // Contextual handoff: Settings owns daily status/toggles; VPN Studio
        // owns profile editing. Reset each command bit before requesting the
        // shell so a failed launch cannot loop every frame.
        let requested_profiles: Vec<String> = self
            .vpns
            .iter()
            .filter(|vpn| read_bool(state, root, &Self::vpn_studio_key(&vpn.name)))
            .map(|vpn| vpn.name.clone())
            .collect();
        for profile in requested_profiles {
            let _ = state.put(root, Self::vpn_studio_key(&profile), false);
            if let Err(e) = (self.tool_opener)(ToolIntent::Vpn {
                route: VpnRoute::Profile,
                profile: Some(profile.clone()),
            }) {
                warn!(target: "qos::settings", profile = %profile, error = %e, "could not open VPN Studio");
            }
        }
        if read_bool(state, root, Self::VPN_STUDIO_OVERVIEW_KEY) {
            let _ = state.put(root, Self::VPN_STUDIO_OVERVIEW_KEY, false);
            if let Err(e) = (self.tool_opener)(ToolIntent::Vpn {
                route: VpnRoute::Overview,
                profile: None,
            }) {
                warn!(target: "qos::settings", error = %e, "could not open VPN Studio");
            }
        }

        // Refresh button — when the button writes `true`, rescan and
        // clear the bit so subsequent frames don't loop.
        let refresh_pressed = matches!(
            state.get(root, REFRESH_KEY),
            Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
        );
        if refresh_pressed {
            let _ = state.put(root, REFRESH_KEY, automerge::ScalarValue::Boolean(false));
            match (self.lister)() {
                Ok(nets) => self.networks = nets,
                Err(e) => {
                    warn!(target: "qos::settings", op = "list_wifi_networks", error = %e, "rescan failed");
                }
            }
            if let Ok(wired) = os_actions::get_wired() {
                self.wired = wired;
                let _ = state.put(
                    root,
                    WIRED_ENABLED_KEY,
                    automerge::ScalarValue::Boolean(self.wired.connected),
                );
            }
            if let Ok(radios) = os_actions::get_radio_state() {
                self.radios = radios;
            }
            self.hotspot_clients = os_actions::list_hotspot_clients().unwrap_or_default();
            self.connectivity = os_actions::get_connectivity().unwrap_or_default();
            self.profiles = os_actions::list_saved_network_profiles().unwrap_or_default();
            self.vpns = (self.vpn_lister)();
        }

        // Restore the expanded-prompt SSID from the document so it
        // survives the host's structure rebuilds (which reconstruct this
        // pane). An empty string means "no prompt open".
        let doc_expanded = read_str(state, root, EXPANDED_KEY).filter(|s| !s.is_empty());
        if doc_expanded != self.expanded_ssid {
            self.expanded_ssid = doc_expanded;
        }

        // Snapshot the network attributes we need without holding a borrow
        // across the mutating `state` calls below.
        let nets: Vec<(String, bool)> = self
            .networks
            .iter()
            .map(|n| (n.ssid.clone(), n.secure))
            .collect();

        // Per-network "Connect" buttons. Open networks connect immediately;
        // secure networks toggle the inline password prompt instead.
        for (ssid, secure) in &nets {
            let key = connect_key(ssid);
            let pressed = read_bool(state, root, &key);
            if !pressed {
                continue;
            }
            let _ = state.put(root, key.as_str(), automerge::ScalarValue::Boolean(false));
            if *secure {
                // Toggle this network's prompt: collapse if it's already
                // the open one, otherwise open it (closing any other).
                let now = if self.expanded_ssid.as_deref() == Some(ssid.as_str()) {
                    None
                } else {
                    Some(ssid.clone())
                };
                self.set_expanded(state, root, now);
            } else {
                // Open network — connect with no password.
                self.do_connect(ssid, None);
                self.set_expanded(state, root, None);
            }
        }

        // Per-network "Join" buttons in the expanded password prompt:
        // connect with the typed password, then collapse the prompt.
        for (ssid, _secure) in &nets {
            let jk = join_key(ssid);
            if !read_bool(state, root, &jk) {
                continue;
            }
            let _ = state.put(root, jk.as_str(), automerge::ScalarValue::Boolean(false));
            let mut pw = self
                .secret_buffers
                .get(ssid)
                .and_then(|buffer| buffer.lock().ok().map(|value| value.clone()))
                .unwrap_or_default();
            let password = (!pw.is_empty()).then_some(pw.as_str());
            self.do_connect(ssid, password);
            pw.zeroize();
            if let Some(buffer) = self.secret_buffers.get(ssid)
                && let Ok(mut value) = buffer.lock()
            {
                value.zeroize();
            }
            self.set_expanded(state, root, None);
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
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
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
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
                connected: true,
            },
            WifiNetworkInfo {
                ssid: "Cafe_Guest".to_string(),
                signal: 3,
                secure: false,
                connected: false,
            },
            WifiNetworkInfo {
                ssid: "Neighbour_5G".to_string(),
                signal: 2,
                secure: true,
                connected: false,
            },
        ]
    }

    /// The signal indicator must use the Material Design Icons `wifi-strength`
    /// family (Private Use Area, `U+F09xx`) — the same family the sidebar's
    /// Network glyph uses and which the shipped fonts actually carry — never the
    /// Unicode block elements (`U+258x`), which fall back to a `.notdef` tofu
    /// box and convey no signal level.
    #[test]
    fn wifi_signal_glyph_uses_the_mdi_family_not_block_elements() {
        for signal in 0u8..=6 {
            let g = wifi_signal_glyph(signal);
            let cp = g.chars().next().unwrap() as u32;
            assert!(
                (0xF091F..=0xF092B).contains(&cp),
                "signal {signal} glyph U+{cp:04X} must be an MDI wifi-strength glyph"
            );
            // Never a Unicode block element (the old tofu bars).
            assert!(
                !(0x2580..=0x259F).contains(&cp),
                "block-element bars are tofu"
            );
        }
        // Distinct levels for 1..=4 so strength is legible at a glance.
        let levels: Vec<&str> = (1u8..=4).map(wifi_signal_glyph).collect();
        let unique: std::collections::HashSet<&&str> = levels.iter().collect();
        assert_eq!(unique.len(), 4, "each bar level is a distinct glyph");
    }

    #[test]
    fn wifi_list_renders_each_returned_network() {
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

    #[test]
    fn wifi_toggle_invokes_set_wifi() {
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
        assert_eq!(rec.calls(), vec!["set_wifi(false)"]);
    }

    #[test]
    fn wired_toggle_only_dispatches_the_primary_lan_interface() {
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
        assert_eq!(rec.calls(), vec!["set_wired(false)"]);
        assert!(
            pane.last_wifi_enabled,
            "LAN must not change the Wi-Fi radio"
        );
    }

    #[test]
    fn connect_button_invokes_connect_wifi_once() {
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
        assert_eq!(rec.calls(), vec!["connect_wifi(Cafe_Guest)"]);
    }

    #[test]
    fn refresh_button_replaces_network_list() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = NetworkPane::with_dispatcher(
            rec as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_networks(),
        );
        // Swap the lister to a fresh set so we can observe the refresh.
        let fresh = vec![WifiNetworkInfo {
            ssid: "NewSpot".to_string(),
            signal: 4,
            secure: false,
            connected: false,
        }];
        pane.lister = {
            let f = fresh.clone();
            Arc::new(move || Ok(f.clone()))
        };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, REFRESH_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert_eq!(pane.networks(), fresh.as_slice());
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

    /// Clicking "Connect" on a SECURE network does not connect immediately —
    /// it expands the inline password prompt (mirrored into the document)
    /// and fires no dispatch yet.
    #[test]
    fn connect_on_secure_network_opens_password_prompt() {
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
        assert_eq!(pane.expanded_ssid.as_deref(), Some("Neighbour_5G"));
        assert_eq!(
            read_str(&doc, &root, EXPANDED_KEY).as_deref(),
            Some("Neighbour_5G"),
            "expansion is mirrored into the document so it survives rebuilds"
        );
        assert!(rec.calls().is_empty(), "no connect dispatched before Join");
        assert!(pane.take_structure_dirty(), "expanding requests a rebuild");
    }

    /// Pressing "Join" in the prompt connects WITH the typed password and
    /// collapses the prompt.
    #[test]
    fn join_connects_with_typed_password() {
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
        *pane
            .secret_buffers
            .get("Neighbour_5G")
            .unwrap()
            .lock()
            .unwrap() = "hunter2".into();
        let _ = doc.put(&root, join_key("Neighbour_5G").as_str(), true);
        pane.sync(&mut doc, &root);
        for _ in 0..50 {
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
        assert_eq!(read_str(&doc, &root, EXPANDED_KEY).as_deref(), Some(""));
    }

    /// Pressing Connect a second time on the same expanded network collapses
    /// the prompt (it acts as a toggle / Cancel).
    #[test]
    fn second_connect_press_collapses_prompt() {
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
        assert_eq!(pane.expanded_ssid.as_deref(), Some("Neighbour_5G"));
        let _ = doc.put(&root, connect_key("Neighbour_5G").as_str(), true);
        pane.sync(&mut doc, &root);
        assert_eq!(pane.expanded_ssid, None);
    }

    #[test]
    fn wired_rows_reflect_connected_snapshot() {
        let pane = NetworkPane::new_demo(sample_networks()).with_wired(WiredInfo {
            connected: true,
            device: Some("eth0".into()),
            speed: Some("100 Mb/s".into()),
            ipv4: Some("10.0.0.7".into()),
        });
        // The Wired card body builds three rows when connected.
        assert_eq!(pane.wired_rows().len(), 3);
    }

    #[test]
    fn wired_rows_show_not_connected_when_down() {
        let pane = NetworkPane::new_demo(sample_networks());
        // Default WiredInfo is disconnected → status + caption only.
        assert_eq!(pane.wired_rows().len(), 2);
    }

    #[test]
    fn parse_vpn_connections_filters_to_vpn_types_and_marks_active() {
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

    #[test]
    fn vpn_rows_reflect_configured_profiles() {
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
        // 2 status rows + 1 caption.
        assert_eq!(pane.vpn_rows().len(), 3);
    }

    #[test]
    fn vpn_rows_show_empty_state_when_none() {
        let pane = NetworkPane::new_demo(sample_networks());
        // Empty-state explanation plus the single Studio handoff.
        assert_eq!(pane.vpn_rows().len(), 2);
    }

    #[test]
    fn advanced_vpn_action_opens_the_exact_profile_context() {
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
        assert!(!read_bool(
            &doc,
            &root,
            &NetworkPane::vpn_studio_key("Work VPN")
        ));
    }
}
