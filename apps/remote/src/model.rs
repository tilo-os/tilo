//! Pure data model for the Remote app.
//!
//! A workspace is a flat list of saved [`Connection`]s plus a small
//! amount of view state (which connection is selected, which screen the
//! user is looking at). The connections are the durable, collaborative
//! part — they live in the Automerge document (see [`crate::doc`]) so a
//! machine list edited on one device converges everywhere it's shared.
//!
//! View state (current screen, selection, draft form contents) is
//! deliberately kept out of the shared document: two people browsing the
//! same connection list should be able to look at different machines.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a saved connection. Backs every cross-reference
/// and the per-connection session state.
pub type ConnectionId = Uuid;

/// The transport / protocol a connection speaks. This is the single most
/// important attribute of a machine: it decides which session surface we
/// open (a terminal for SSH, a remote-framebuffer canvas for VNC/RDP) and
/// — crucially for qOS — whether the remote side renders pixels or simply
/// relays application/workspace data we render locally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionKind {
    /// Secure shell — an interactive terminal session.
    Ssh,
    /// Virtual Network Computing — a streamed remote framebuffer.
    Vnc,
    /// Remote Desktop Protocol — a streamed remote desktop.
    Rdp,
    /// A peer qOS device in **relay mode**. The remote machine doesn't
    /// stream pixels; it relays application + workspace state over the
    /// P2P fabric and we render it *locally* with our own toolkit. This
    /// is why qOS-to-qOS sessions stay crisp and theme-consistent: there
    /// is no framebuffer, only a CRDT/event stream.
    Qos,
}

impl ConnectionKind {
    /// Short uppercase tag shown on the connection row badge.
    pub fn tag(self) -> &'static str {
        match self {
            ConnectionKind::Ssh => "SSH",
            ConnectionKind::Vnc => "VNC",
            ConnectionKind::Rdp => "RDP",
            ConnectionKind::Qos => "qOS",
        }
    }

    /// Human-friendly protocol name for forms and detail panes.
    pub fn label(self) -> &'static str {
        match self {
            ConnectionKind::Ssh => "SSH",
            ConnectionKind::Vnc => "VNC",
            ConnectionKind::Rdp => "RDP",
            ConnectionKind::Qos => "qOS Relay",
        }
    }

    /// The port a fresh connection of this kind defaults to.
    pub fn default_port(self) -> u16 {
        match self {
            ConnectionKind::Ssh => 22,
            ConnectionKind::Vnc => 5900,
            ConnectionKind::Rdp => 3389,
            // qOS relay rides the compositor's P2P fabric; the "port" is
            // a logical relay channel rather than a TCP port.
            ConnectionKind::Qos => 0,
        }
    }

    /// Whether a session of this kind renders as a text terminal (true)
    /// or as a graphical surface (false). qOS relay renders graphically
    /// but *locally*, so it groups with the framebuffer kinds here.
    pub fn is_terminal(self) -> bool {
        matches!(self, ConnectionKind::Ssh)
    }

    /// Whether the remote side streams a pixel framebuffer to us. False
    /// for SSH (text) and for qOS relay (local render of relayed state).
    pub fn streams_framebuffer(self) -> bool {
        matches!(self, ConnectionKind::Vnc | ConnectionKind::Rdp)
    }

    /// Iteration order for the kind selector in the add/edit form.
    pub fn all() -> [ConnectionKind; 4] {
        [
            ConnectionKind::Ssh,
            ConnectionKind::Vnc,
            ConnectionKind::Rdp,
            ConnectionKind::Qos,
        ]
    }

    /// Parse the persisted tag back into a kind. Unknown tags fall back
    /// to SSH so a forward-compat document never drops a connection.
    pub fn from_tag(s: &str) -> ConnectionKind {
        match s {
            "VNC" => ConnectionKind::Vnc,
            "RDP" => ConnectionKind::Rdp,
            "qOS" => ConnectionKind::Qos,
            _ => ConnectionKind::Ssh,
        }
    }
}

/// How a connection authenticates. Stored alongside the connection but
/// never carries a live secret in the document — passwords are entered at
/// connect time and held only in volatile memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    /// Username + password prompted at connect time.
    Password,
    /// Public-key auth (an agent or on-disk key handles the secret).
    PublicKey,
    /// qOS device identity — the P2P fabric authenticates the peer, so
    /// there is no separate credential to manage.
    DeviceIdentity,
}

impl AuthMethod {
    pub fn label(self) -> &'static str {
        match self {
            AuthMethod::Password => "Password",
            AuthMethod::PublicKey => "Public key",
            AuthMethod::DeviceIdentity => "Device identity",
        }
    }

    pub fn all() -> [AuthMethod; 3] {
        [
            AuthMethod::Password,
            AuthMethod::PublicKey,
            AuthMethod::DeviceIdentity,
        ]
    }

    pub fn from_label(s: &str) -> AuthMethod {
        match s {
            "Public key" => AuthMethod::PublicKey,
            "Device identity" => AuthMethod::DeviceIdentity,
            _ => AuthMethod::Password,
        }
    }
}

/// Reachability of a saved machine, as last observed. For live
/// connections this is refreshed by a background probe; offline it's the
/// seed value so the screenshot reads as a populated, lived-in list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Presence {
    /// Reachable on the last probe.
    Online,
    /// Unreachable on the last probe.
    Offline,
    /// Never probed / status unknown.
    Unknown,
}

/// One saved remote machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub id: ConnectionId,
    /// Display name shown in the list (e.g. "Home Server").
    pub name: String,
    /// Hostname or IP (e.g. "10.0.0.4" or "build.example.com"). For a
    /// qOS relay this is the device's friendly node name.
    pub host: String,
    pub port: u16,
    /// Login user. Empty for kinds that don't need one (qOS relay).
    pub user: String,
    pub kind: ConnectionKind,
    pub auth: AuthMethod,
    /// "Last used" descriptor, kept as a short human string so it renders
    /// without a clock dependency (e.g. "2h ago", "yesterday", "Never").
    pub last_used: String,
    pub presence: Presence,
    /// Optional one-line note / tag (e.g. "prod", "lab").
    pub group: String,
    /// Whether the user pinned this connection to the top of the list.
    pub favorite: bool,
}

impl Connection {
    /// `user@host:port` address line, omitting parts that don't apply.
    pub fn address(&self) -> String {
        let host_part = if self.port == 0 || self.kind == ConnectionKind::Qos {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };
        if self.user.is_empty() {
            host_part
        } else {
            format!("{}@{}", self.user, host_part)
        }
    }
}

/// Which top-level screen the UI is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The saved-connections list (home).
    List,
    /// The add / edit connection form. Carries the id being edited, or
    /// `None` for a brand-new connection.
    Form { editing: Option<ConnectionId> },
    /// A live (here: sample) session for a specific connection.
    Session { id: ConnectionId },
}

/// A draft of the add/edit form. Held in the app (not the CRDT) because
/// an in-progress form is private to the editor until they hit Save.
#[derive(Clone, Debug)]
pub struct ConnectionDraft {
    pub name: String,
    pub host: String,
    pub port: String,
    pub user: String,
    pub kind: ConnectionKind,
    pub auth: AuthMethod,
    pub group: String,
}

impl Default for ConnectionDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: ConnectionKind::Ssh.default_port().to_string(),
            user: String::new(),
            kind: ConnectionKind::Ssh,
            auth: AuthMethod::Password,
            group: String::new(),
        }
    }
}

impl ConnectionDraft {
    /// Seed a draft from an existing connection (for the edit flow).
    pub fn from_connection(c: &Connection) -> Self {
        Self {
            name: c.name.clone(),
            host: c.host.clone(),
            port: c.port.to_string(),
            user: c.user.clone(),
            kind: c.kind,
            auth: c.auth,
            group: c.group.clone(),
        }
    }

    /// Validate the draft, returning a list of human-readable problems.
    /// An empty list means the draft is ready to save.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.name.trim().is_empty() {
            out.push("Name is required".into());
        }
        if self.host.trim().is_empty() {
            out.push("Host is required".into());
        }
        // qOS relay uses a logical channel, no numeric port.
        if self.kind != ConnectionKind::Qos {
            match self.port.trim().parse::<u16>() {
                Ok(0) | Err(_) => out.push("Port must be 1–65535".into()),
                Ok(_) => {}
            }
        }
        out
    }

    /// Build a [`Connection`] from a validated draft, reusing `id`.
    pub fn build(&self, id: ConnectionId) -> Connection {
        let port = if self.kind == ConnectionKind::Qos {
            0
        } else {
            self.port
                .trim()
                .parse::<u16>()
                .unwrap_or_else(|_| self.kind.default_port())
        };
        Connection {
            id,
            name: self.name.trim().to_string(),
            host: self.host.trim().to_string(),
            port,
            user: self.user.trim().to_string(),
            kind: self.kind,
            auth: self.auth,
            last_used: "Never".to_string(),
            presence: Presence::Unknown,
            group: self.group.trim().to_string(),
            favorite: false,
        }
    }
}

/// In-memory connection store with the runtime operations the UI needs.
/// Pure logic — no IO, no rendering, no CRDT. The CRDT bridge lives in
/// [`crate::doc`].
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub connections: Vec<Connection>,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    pub fn get(&self, id: ConnectionId) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }

    pub fn get_mut(&mut self, id: ConnectionId) -> Option<&mut Connection> {
        self.connections.iter_mut().find(|c| c.id == id)
    }

    /// Insert a new connection (or replace one with the same id). Returns
    /// the id for convenience.
    pub fn upsert(&mut self, c: Connection) -> ConnectionId {
        let id = c.id;
        if let Some(existing) = self.get_mut(id) {
            *existing = c;
        } else {
            self.connections.push(c);
        }
        id
    }

    /// Remove a connection. Returns true if it existed.
    pub fn remove(&mut self, id: ConnectionId) -> bool {
        let before = self.connections.len();
        self.connections.retain(|c| c.id != id);
        self.connections.len() != before
    }

    /// Toggle the favorite flag; returns the new value (or false if the
    /// connection is unknown).
    pub fn toggle_favorite(&mut self, id: ConnectionId) -> bool {
        if let Some(c) = self.get_mut(id) {
            c.favorite = !c.favorite;
            c.favorite
        } else {
            false
        }
    }

    /// Connections filtered by a case-insensitive query over name / host
    /// / user / group, then ordered favorites-first and by name. An empty
    /// query returns everything (still ordered).
    pub fn filtered(&self, query: &str) -> Vec<&Connection> {
        let q = query.trim().to_lowercase();
        let mut out: Vec<&Connection> = self
            .connections
            .iter()
            .filter(|c| {
                if q.is_empty() {
                    return true;
                }
                c.name.to_lowercase().contains(&q)
                    || c.host.to_lowercase().contains(&q)
                    || c.user.to_lowercase().contains(&q)
                    || c.group.to_lowercase().contains(&q)
                    || c.kind.tag().to_lowercase().contains(&q)
            })
            .collect();
        out.sort_by(|a, b| {
            b.favorite
                .cmp(&a.favorite)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        out
    }

    /// Count of currently-online connections — drives the header stat.
    pub fn online_count(&self) -> usize {
        self.connections
            .iter()
            .filter(|c| c.presence == Presence::Online)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, kind: ConnectionKind) -> Connection {
        Connection {
            id: Uuid::new_v4(),
            name: name.into(),
            host: "host".into(),
            port: kind.default_port(),
            user: "user".into(),
            kind,
            auth: AuthMethod::Password,
            last_used: "now".into(),
            presence: Presence::Online,
            group: String::new(),
            favorite: false,
        }
    }

    #[test]
    fn address_formats_user_host_port() {
        let mut c = sample("s", ConnectionKind::Ssh);
        c.host = "10.0.0.1".into();
        c.user = "root".into();
        c.port = 2222;
        assert_eq!(c.address(), "root@10.0.0.1:2222");
    }

    #[test]
    fn qos_address_drops_port() {
        let mut c = sample("q", ConnectionKind::Qos);
        c.host = "studio-node".into();
        c.user = String::new();
        c.port = 0;
        assert_eq!(c.address(), "studio-node");
    }

    #[test]
    fn upsert_replaces_same_id() {
        let mut w = Workspace::new();
        let mut c = sample("a", ConnectionKind::Ssh);
        let id = w.upsert(c.clone());
        c.name = "renamed".into();
        w.upsert(c);
        assert_eq!(w.connections().len(), 1);
        assert_eq!(w.get(id).unwrap().name, "renamed");
    }

    #[test]
    fn filtered_orders_favorites_first() {
        let mut w = Workspace::new();
        let mut a = sample("alpha", ConnectionKind::Ssh);
        let mut b = sample("zeta", ConnectionKind::Vnc);
        b.favorite = true;
        w.upsert(a.clone());
        w.upsert(b);
        a.favorite = false;
        let order: Vec<_> = w.filtered("").iter().map(|c| c.name.clone()).collect();
        assert_eq!(order, vec!["zeta", "alpha"]);
    }

    #[test]
    fn filtered_matches_query() {
        let mut w = Workspace::new();
        w.upsert(sample("Build Box", ConnectionKind::Ssh));
        w.upsert(sample("Office NAS", ConnectionKind::Vnc));
        let hits: Vec<_> = w.filtered("build").iter().map(|c| c.name.clone()).collect();
        assert_eq!(hits, vec!["Build Box"]);
    }

    #[test]
    fn draft_validation_catches_empty_and_bad_port() {
        let mut d = ConnectionDraft::default();
        assert!(!d.problems().is_empty());
        d.name = "x".into();
        d.host = "h".into();
        d.port = "0".into();
        assert!(d.problems().iter().any(|p| p.contains("Port")));
        d.port = "22".into();
        assert!(d.problems().is_empty());
    }

    #[test]
    fn qos_draft_ignores_port() {
        let d = ConnectionDraft {
            kind: ConnectionKind::Qos,
            name: "node".into(),
            host: "studio".into(),
            port: String::new(),
            ..Default::default()
        };
        assert!(d.problems().is_empty());
        let built = d.build(Uuid::new_v4());
        assert_eq!(built.port, 0);
    }
}
