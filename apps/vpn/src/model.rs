//! Pure data model for the VPN client.
//!
//! Everything here is plain Rust with no rendering or IO. The live
//! source of truth is the Automerge document (see [`crate::doc`]); this
//! module defines the in-memory shapes the UI reads/writes and the
//! sample fleet used to populate the headless `--shots` screenshot.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for one saved connection.
pub type ConnId = Uuid;

/// Tunnel protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnKind {
    WireGuard,
    OpenVpn,
}

impl ConnKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnKind::WireGuard => "wireguard",
            ConnKind::OpenVpn => "openvpn",
        }
    }
    pub fn from_str(s: &str) -> ConnKind {
        match s {
            "openvpn" => ConnKind::OpenVpn,
            _ => ConnKind::WireGuard,
        }
    }
    /// Short uppercase badge label.
    pub fn badge(self) -> &'static str {
        match self {
            ConnKind::WireGuard => "WG",
            ConnKind::OpenVpn => "OVPN",
        }
    }
    pub fn long_name(self) -> &'static str {
        match self {
            ConnKind::WireGuard => "WireGuard",
            ConnKind::OpenVpn => "OpenVPN",
        }
    }
    /// File extension callers import from.
    pub fn ext(self) -> &'static str {
        match self {
            ConnKind::WireGuard => "conf",
            ConnKind::OpenVpn => "ovpn",
        }
    }
}

/// Live tunnel state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnStatus {
    Disconnected,
    Connecting,
    Connected,
}

impl ConnStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnStatus::Disconnected => "disconnected",
            ConnStatus::Connecting => "connecting",
            ConnStatus::Connected => "connected",
        }
    }
    pub fn from_str(s: &str) -> ConnStatus {
        match s {
            "connected" => ConnStatus::Connected,
            "connecting" => ConnStatus::Connecting,
            _ => ConnStatus::Disconnected,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ConnStatus::Disconnected => "Disconnected",
            ConnStatus::Connecting => "Connecting…",
            ConnStatus::Connected => "Connected",
        }
    }
    pub fn is_active(self) -> bool {
        matches!(self, ConnStatus::Connected | ConnStatus::Connecting)
    }
}

/// One saved VPN connection. Mirrors the per-connection Map in the CRDT.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: ConnId,
    pub name: String,
    pub kind: ConnKind,
    /// `host:port` the tunnel dials.
    pub endpoint: String,
    /// Interface / assigned address handed out by the peer (e.g. `10.7.0.2/32`).
    pub address: String,
    pub dns: String,
    /// `WireGuard` peer public key, or `OpenVPN` server CN.
    pub public_key: String,
    /// Routed networks (`AllowedIPs` / pushed routes).
    pub allowed_ips: String,
    pub status: ConnStatus,
    /// Seconds since the last successful handshake, when connected.
    pub last_handshake_secs: Option<u64>,
    /// Human "last used" label (`2h ago`, `Yesterday`, `Just now`).
    pub last_used: String,
    /// Route this tunnel's traffic over Tor as well.
    pub tor: bool,
    /// Total bytes received this session.
    pub rx_bytes: u64,
    /// Total bytes transmitted this session.
    pub tx_bytes: u64,
    /// Baseline downstream rate (MB/s) used to shape the live sparkline.
    pub base_down_mbps: f64,
    /// Baseline upstream rate (MB/s) used to shape the live sparkline.
    pub base_up_mbps: f64,
    /// Absolute path to the real config file this tunnel brings up
    /// (`wg-quick up <path>` / `openvpn --config <path>`). Populated when a
    /// `.conf` / `.ovpn` is imported and persisted to the app's data dir.
    /// `None` for hand-added stubs that have no real config yet.
    pub config_path: Option<String>,
}

impl Connection {
    pub fn new(name: &str, kind: ConnKind) -> Self {
        Connection {
            id: Uuid::new_v4(),
            name: name.to_string(),
            kind,
            endpoint: String::new(),
            address: String::new(),
            dns: String::new(),
            public_key: String::new(),
            allowed_ips: String::new(),
            status: ConnStatus::Disconnected,
            last_handshake_secs: None,
            last_used: "Never".to_string(),
            tor: false,
            rx_bytes: 0,
            tx_bytes: 0,
            base_down_mbps: 3.0,
            base_up_mbps: 0.8,
            config_path: None,
        }
    }

    /// The real config file this tunnel would bring up, if one was imported.
    pub fn config_path(&self) -> Option<&str> {
        self.config_path.as_deref().filter(|p| !p.is_empty())
    }

    pub fn is_connected(&self) -> bool {
        self.status == ConnStatus::Connected
    }
}

/// Build a fresh saved connection stub with placeholder config, as created by
/// the "add connection" affordances (the add form and the Ctrl+K palette). The
/// `endpoint` is left blank for the caller to fill from its own input. The id is
/// minted here, once, at authoring time — so both affordances dispatch a single
/// [`crate::actions::VpnAction::AddConnection`] carrying an identical payload,
/// and replay reproduces the exact same connection on a peer.
pub fn stub_connection(name: &str, kind: ConnKind) -> Connection {
    let name = if name.trim().is_empty() {
        format!("New {}", kind.long_name())
    } else {
        name.trim().to_string()
    };
    let mut c = Connection::new(&name, kind);
    c.last_used = "Just added".to_string();
    c.address = "10.0.0.2/32".to_string();
    c.dns = "1.1.1.1".to_string();
    c.allowed_ips = "0.0.0.0/0".to_string();
    c.public_key = match kind {
        ConnKind::WireGuard => "(import .conf to populate key)".to_string(),
        ConnKind::OpenVpn => format!("CN={name}"),
    };
    c
}

/// The whole saved fleet plus UI selection + global toggles.
#[derive(Clone, Debug, Default)]
pub struct Vault {
    pub connections: Vec<Connection>,
    pub selected: Option<ConnId>,
    /// Global "route everything over Tor" master toggle.
    pub tor_global: bool,
}

impl Vault {
    pub fn get(&self, id: ConnId) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }
    pub fn get_mut(&mut self, id: ConnId) -> Option<&mut Connection> {
        self.connections.iter_mut().find(|c| c.id == id)
    }
    pub fn selected_conn(&self) -> Option<&Connection> {
        self.selected.and_then(|id| self.get(id))
    }
    pub fn active_count(&self) -> usize {
        self.connections.iter().filter(|c| c.is_connected()).count()
    }
    /// Index of `id` in the list (for keyboard up/down navigation).
    pub fn index_of(&self, id: ConnId) -> Option<usize> {
        self.connections.iter().position(|c| c.id == id)
    }
}

/// Build the demo fleet shown in `--shots` and on a fresh install.
///
/// Per the brief: a 'Home' `WireGuard` that is **connected** with
/// live-looking throughput, and a 'Work' `OpenVPN` that is
/// **disconnected**. A few extra entries make the list feel real.
pub fn sample_vault() -> Vault {
    let mut home = Connection::new("Home", ConnKind::WireGuard);
    home.endpoint = "home.aurora.net:51820".to_string();
    home.address = "10.7.0.2/32".to_string();
    home.dns = "10.7.0.1, 1.1.1.1".to_string();
    home.public_key = "kP8x2yQ1mZ7vR3nL9fH0sJtW6cB4dE5gA1iU2oXyz0=".to_string();
    home.allowed_ips = "0.0.0.0/0, ::/0".to_string();
    home.status = ConnStatus::Connected;
    home.last_handshake_secs = Some(34);
    home.last_used = "Active now".to_string();
    home.rx_bytes = 4_812_330_000;
    home.tx_bytes = 612_004_500;
    home.base_down_mbps = 6.4;
    home.base_up_mbps = 1.3;

    let mut work = Connection::new("Work", ConnKind::OpenVpn);
    work.endpoint = "vpn.workcorp.example:1194".to_string();
    work.address = "172.20.4.18/24".to_string();
    work.dns = "172.20.4.1".to_string();
    work.public_key = "CN=vpn.workcorp.example".to_string();
    work.allowed_ips = "172.20.0.0/16, 10.0.0.0/8".to_string();
    work.status = ConnStatus::Disconnected;
    work.last_used = "Yesterday".to_string();
    work.base_down_mbps = 2.1;
    work.base_up_mbps = 0.6;

    let mut relay = Connection::new("Cloud Relay", ConnKind::WireGuard);
    relay.endpoint = "fra1.relay.cloud:51820".to_string();
    relay.address = "10.66.0.9/32".to_string();
    relay.dns = "10.66.0.1".to_string();
    relay.public_key = "Lx0pQ9mNv4Kc2bA7dF1sR8tY3uI6oP5lK4jH9gZ2wE=".to_string();
    relay.allowed_ips = "0.0.0.0/0".to_string();
    relay.status = ConnStatus::Disconnected;
    relay.last_used = "3 days ago".to_string();
    relay.tor = true;
    relay.base_down_mbps = 4.0;
    relay.base_up_mbps = 1.0;

    let mut lab = Connection::new("Lab", ConnKind::OpenVpn);
    lab.endpoint = "lab.internal:1195".to_string();
    lab.address = "192.168.88.5/24".to_string();
    lab.dns = "192.168.88.1".to_string();
    lab.public_key = "CN=lab-ca".to_string();
    lab.allowed_ips = "192.168.88.0/24".to_string();
    lab.status = ConnStatus::Disconnected;
    lab.last_used = "Last week".to_string();

    let selected = Some(home.id);
    Vault {
        connections: vec![home, work, relay, lab],
        selected,
        tor_global: false,
    }
}

// ----------------------------------------------------------------------
// Formatting helpers (pure).
// ----------------------------------------------------------------------

/// Compact "time ago" for a seconds delta, e.g. `34s ago`, `4m ago`.
pub fn human_ago(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_has_home_connected_and_work_disconnected() {
        let v = sample_vault();
        let home = v.connections.iter().find(|c| c.name == "Home").unwrap();
        assert_eq!(home.kind, ConnKind::WireGuard);
        assert_eq!(home.status, ConnStatus::Connected);
        let work = v.connections.iter().find(|c| c.name == "Work").unwrap();
        assert_eq!(work.kind, ConnKind::OpenVpn);
        assert_eq!(work.status, ConnStatus::Disconnected);
        assert_eq!(v.selected, Some(home.id));
        assert_eq!(v.active_count(), 1);
    }

    #[test]
    fn byte_and_rate_formatting() {
        // Transfer counters are base-1024, so they wear IEC binary units.
        assert_eq!(ui::format::bytes(512), "512 B");
        assert_eq!(ui::format::bytes(4_812_330_000), "4.5 GiB");
        assert_eq!(ui::format::rate(6_710_886.0), "6.4 MiB/s");
        assert_eq!(human_ago(34), "34s ago");
        assert_eq!(human_ago(240), "4m ago");
    }

    #[test]
    fn kind_status_round_trip() {
        assert_eq!(
            ConnKind::from_str(ConnKind::OpenVpn.as_str()),
            ConnKind::OpenVpn
        );
        assert_eq!(
            ConnStatus::from_str(ConnStatus::Connecting.as_str()),
            ConnStatus::Connecting
        );
    }
}
