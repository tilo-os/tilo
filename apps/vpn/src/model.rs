//! Pure data model for the VPN client.
//!
//! Everything here is plain Rust with no rendering or IO. The live
//! source of truth is the Automerge document (see [`crate::doc`]); this
//! module defines the in-memory shapes the UI reads/writes and the
//! sample fleet the `--shots` scenarios show.

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
    /// Parse the stored spelling, falling back to the spec default
    /// `WireGuard` for anything unrecognised. Named `parse` rather than
    /// `from_str` so it isn't confused with `std::str::FromStr::from_str`
    /// (this never fails, so it has no `Result`/`Err` to report).
    pub fn parse(s: &str) -> ConnKind {
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
    /// Parse the stored spelling, falling back to `Disconnected` for
    /// anything unrecognised. Named `parse` rather than `from_str` so it
    /// isn't confused with `std::str::FromStr::from_str` (this never fails,
    /// so it has no `Result`/`Err` to report).
    pub fn parse(s: &str) -> ConnStatus {
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
    pub status: ConnStatus,
    /// Seconds since the last successful handshake, when connected.
    pub last_handshake_secs: Option<u64>,
    /// Human "last used" label (`2h ago`, `Yesterday`, `Just now`).
    pub last_used: String,
    /// Route this tunnel's traffic over Tor as well.
    pub tor: bool,
}

/// Device-local, non-collaborative tunnel facts. This type is intentionally
/// separate from [`Connection`]: it is never serialized, action-encoded,
/// resource-indexed or written to the Automerge document. `NetworkManager` owns
/// raw credentials/configuration; this holds only useful display facts parsed
/// during a local import and the stable `NetworkManager` UUID used to find that
/// exact profile again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProfileDetails {
    pub network_manager_uuid: String,
    pub endpoint: String,
    pub address: String,
    pub dns: String,
    pub public_key: String,
    pub allowed_ips: String,
}

impl Connection {
    pub fn new(name: &str, kind: ConnKind) -> Self {
        Connection {
            id: Uuid::new_v4(),
            name: name.to_string(),
            kind,
            status: ConnStatus::Disconnected,
            last_handshake_secs: None,
            last_used: "Never".to_string(),
            tor: false,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.status == ConnStatus::Connected
    }
}

/// Build an empty connection draft without inventing network configuration.
///
/// A draft is not ready to activate until a complete provider configuration is
/// imported and validated. In particular, this function never guesses an
/// address, resolver, route, peer identity, or credential.
pub fn draft_connection(name: &str, kind: ConnKind) -> Connection {
    let name = if name.trim().is_empty() {
        format!("New {}", kind.long_name())
    } else {
        name.trim().to_string()
    };
    let mut c = Connection::new(&name, kind);
    c.last_used = "Never".to_string();
    c
}

/// The whole saved fleet plus this window's selection.
#[derive(Clone, Debug, Default)]
pub struct Vault {
    pub connections: Vec<Connection>,
    pub selected: Option<ConnId>,
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

/// A sample profile with a fixed id, so a screenshot is the same every run.
fn sample_connection(n: u128, name: &str, kind: ConnKind) -> Connection {
    Connection {
        id: Uuid::from_u128(0x5a3e_0000_0000_4000_8000_0000_0000_0000 | n),
        ..Connection::new(name, kind)
    }
}

/// The sample fleet the screenshots show: a 'Home' `WireGuard` that is
/// **connected**, a 'Work' `OpenVPN` that is **disconnected**, and a few
/// more so the list feels real.
pub fn sample_vault() -> Vault {
    let mut home = sample_connection(1, "Home", ConnKind::WireGuard);
    home.status = ConnStatus::Connected;
    home.last_handshake_secs = Some(34);
    home.last_used = "Active now".to_string();

    let mut work = sample_connection(2, "Work", ConnKind::OpenVpn);
    work.status = ConnStatus::Disconnected;
    work.last_used = "Yesterday".to_string();

    let mut relay = sample_connection(3, "Cloud Relay", ConnKind::WireGuard);
    relay.status = ConnStatus::Disconnected;
    relay.last_used = "3 days ago".to_string();
    relay.tor = true;

    let mut lab = sample_connection(4, "Lab", ConnKind::OpenVpn);
    lab.status = ConnStatus::Disconnected;
    lab.last_used = "Last week".to_string();

    let selected = Some(home.id);
    Vault {
        connections: vec![home, work, relay, lab],
        selected,
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
    fn vpn_model_sample_formatting_and_enum_round_trips() {
        // Case `sample_has_home_connected_and_work_disconnected`.
        {
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
        // Case `byte_and_rate_formatting`.
        {
            // Transfer counters are base-1024, so they wear IEC binary units.
            assert_eq!(ui::format::bytes(512), "512 B");
            assert_eq!(ui::format::rate(6_710_886.0), "6.4 MiB/s");
            assert_eq!(human_ago(34), "34s ago");
            assert_eq!(human_ago(240), "4m ago");
        }
        // Case `kind_status_round_trip`.
        {
            assert_eq!(
                ConnKind::parse(ConnKind::OpenVpn.as_str()),
                ConnKind::OpenVpn
            );
            assert_eq!(
                ConnStatus::parse(ConnStatus::Connecting.as_str()),
                ConnStatus::Connecting
            );
        }
    }
}
