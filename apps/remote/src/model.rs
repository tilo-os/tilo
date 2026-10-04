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

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a saved connection. Backs every cross-reference
/// and the per-connection session state.
pub type ConnectionId = Uuid;

/// The transport / protocol a connection speaks.
///
/// RDP remains a variant so saved, older connection profiles can round-trip
/// without data loss. Tilo device control is backed by the shell's authenticated
/// workspace transport and is available for new profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionKind {
    /// Secure shell — an interactive terminal session.
    Ssh,
    /// Virtual Network Computing — a streamed remote framebuffer.
    Vnc,
    /// Remote Desktop Protocol — a streamed remote desktop.
    Rdp,
    /// A peer Tilo device reached through the canonical device mesh. The shell
    /// requires an authenticated exact-target route while reusing the
    /// target's consented surface stream and input firewall.
    Tilo,
}

impl ConnectionKind {
    /// Short uppercase tag shown on the connection row badge.
    pub fn tag(self) -> &'static str {
        match self {
            ConnectionKind::Ssh => "SSH",
            ConnectionKind::Vnc => "VNC",
            ConnectionKind::Rdp => "RDP",
            ConnectionKind::Tilo => "Tilo",
        }
    }

    /// Human-friendly protocol name for forms and detail panes.
    pub fn label(self) -> &'static str {
        match self {
            ConnectionKind::Ssh => "SSH",
            ConnectionKind::Vnc => "VNC",
            ConnectionKind::Rdp => "RDP",
            ConnectionKind::Tilo => "Tilo device",
        }
    }

    /// The port a fresh connection of this kind defaults to.
    pub fn default_port(self) -> u16 {
        match self {
            ConnectionKind::Ssh => 22,
            ConnectionKind::Vnc => 5900,
            ConnectionKind::Rdp => 3389,
            // Tilo device control rides the compositor's P2P fabric and has no
            // application-owned TCP port.
            ConnectionKind::Tilo => 0,
        }
    }

    /// Whether a session of this kind renders as a text terminal (true)
    /// or as a graphical surface (false).
    pub fn is_terminal(self) -> bool {
        matches!(self, ConnectionKind::Ssh)
    }

    /// Whether the remote side streams a graphical surface to us. Tilo reuses
    /// the compositor's existing consented surface stream rather than defining
    /// a parallel remote-desktop protocol.
    pub fn streams_framebuffer(self) -> bool {
        matches!(
            self,
            ConnectionKind::Vnc | ConnectionKind::Rdp | ConnectionKind::Tilo
        )
    }

    /// Kinds available when creating a new profile.
    ///
    /// Keep the persisted enum broader than this list: hiding an unavailable
    /// transport from creation must not silently destroy an older profile.
    pub fn available_to_create() -> [ConnectionKind; 3] {
        [
            ConnectionKind::Ssh,
            ConnectionKind::Vnc,
            ConnectionKind::Tilo,
        ]
    }

    /// Whether Tilo can open this transport today.
    pub fn is_supported(self) -> bool {
        matches!(
            self,
            ConnectionKind::Ssh | ConnectionKind::Vnc | ConnectionKind::Tilo
        )
    }

    /// Whether Remote can honestly start a connection attempt.
    pub fn can_attempt(self) -> bool {
        matches!(
            self,
            ConnectionKind::Ssh | ConnectionKind::Vnc | ConnectionKind::Tilo
        )
    }

    /// Iteration order for every persisted kind. Use
    /// [`Self::available_to_create`] in user-facing creation controls.
    pub fn all() -> [ConnectionKind; 4] {
        [
            ConnectionKind::Ssh,
            ConnectionKind::Vnc,
            ConnectionKind::Rdp,
            ConnectionKind::Tilo,
        ]
    }

    /// Parse the persisted tag back into a kind. Unknown tags fall back
    /// to SSH so a forward-compat document never drops a connection.
    pub fn from_tag(s: &str) -> ConnectionKind {
        match s {
            "VNC" => ConnectionKind::Vnc,
            "RDP" => ConnectionKind::Rdp,
            "Tilo" => ConnectionKind::Tilo,
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
    /// Tilo device identity — the P2P fabric authenticates the peer, so
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

    /// Authentication methods supported by the selected protocol.
    ///
    /// This keeps the form from advertising a credential shape that the live
    /// transport would silently ignore or reinterpret.
    pub const fn available_for(kind: ConnectionKind) -> &'static [AuthMethod] {
        match kind {
            ConnectionKind::Ssh => &[Self::Password, Self::PublicKey],
            ConnectionKind::Vnc | ConnectionKind::Rdp => &[Self::Password],
            ConnectionKind::Tilo => &[Self::DeviceIdentity],
        }
    }

    /// Whether this method is valid for the selected protocol.
    pub fn is_supported_by(self, kind: ConnectionKind) -> bool {
        Self::available_for(kind).contains(&self)
    }

    pub fn from_label(s: &str) -> AuthMethod {
        match s {
            "Public key" => AuthMethod::PublicKey,
            "Device identity" => AuthMethod::DeviceIdentity,
            _ => AuthMethod::Password,
        }
    }
}

/// Reachability of a saved machine, as this device last observed it. Live
/// state of this device, never part of the saved profile.
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
    /// Tilo relay this is the exact certified device endpoint id; `name` is
    /// the friendly label shown in the UI.
    pub host: String,
    pub port: u16,
    /// Login user. Empty for kinds that don't need one (Tilo relay).
    pub user: String,
    pub kind: ConnectionKind,
    pub auth: AuthMethod,
    /// When a session to this machine was last opened, in milliseconds
    /// since the Unix epoch, on any of the person's devices.
    pub last_used_ms: Option<i64>,
    /// Optional one-line note / tag (e.g. "prod", "lab").
    pub group: String,
    /// Whether the user pinned this connection to the top of the list.
    pub favorite: bool,
}

impl Connection {
    /// Compact address line, omitting parts that don't apply.
    pub fn address(&self) -> String {
        if self.kind == ConnectionKind::Tilo {
            // A certified endpoint id is intentionally long. The row already
            // carries the user-assigned connection name; show a stable prefix
            // here while the editable form retains the exact target id.
            return format!("Tilo device {}", compact_tilo_endpoint(&self.host));
        }
        let host_part = if self.port == 0 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };
        if self.user.is_empty() || self.kind != ConnectionKind::Ssh {
            host_part
        } else {
            format!("{}@{}", self.user, host_part)
        }
    }

    /// Explain why this persisted profile cannot safely start a transport.
    ///
    /// The result is deliberately a stable, non-secret string: it is shared by
    /// the canvas UI, accessibility tree, resource catalogue, and agent-facing
    /// availability contract. No connect attempt is made until this returns
    /// `None`.
    pub fn profile_issue(&self) -> Option<&'static str> {
        profile_issue(self.kind, &self.host, self.auth)
    }
}

/// Compact an exact Tilo endpoint id for list rows and device-picker labels.
/// The editable form retains the full identity; this rendering helper avoids
/// overflow and never slices user-controlled UTF-8 by byte offset.
pub fn compact_tilo_endpoint(endpoint: &str) -> String {
    let prefix: String = endpoint.chars().take(12).collect();
    if endpoint.chars().nth(12).is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn profile_issue(kind: ConnectionKind, host: &str, auth: AuthMethod) -> Option<&'static str> {
    if !auth.is_supported_by(kind) {
        return Some(match kind {
            ConnectionKind::Ssh => "SSH profiles support password or public-key authentication.",
            ConnectionKind::Vnc => "VNC profiles support password authentication only.",
            ConnectionKind::Rdp => "RDP profiles support password authentication only.",
            ConnectionKind::Tilo => "Tilo device profiles must use Tilo device identity.",
        });
    }
    if kind == ConnectionKind::Tilo
        && !host.trim().is_empty()
        && host.trim().parse::<iroh_base::EndpointId>().is_err()
    {
        return Some("Tilo device node must be an exact certified endpoint id.");
    }
    None
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
        // Tilo relay uses a logical channel, no numeric port.
        if self.kind != ConnectionKind::Tilo {
            match self.port.trim().parse::<u16>() {
                Ok(0) | Err(_) => out.push("Port must be 1–65535".into()),
                Ok(_) => {}
            }
        }
        if let Some(issue) = profile_issue(self.kind, &self.host, self.auth) {
            out.push(issue.into());
        }
        out
    }

    /// Build a [`Connection`] from a validated draft, reusing `id`.
    pub fn build(&self, id: ConnectionId) -> Connection {
        let port = if self.kind == ConnectionKind::Tilo {
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
            user: if self.kind == ConnectionKind::Ssh || self.kind == ConnectionKind::Rdp {
                self.user.trim().to_string()
            } else {
                String::new()
            },
            kind: self.kind,
            auth: self.auth,
            last_used_ms: None,
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
    /// Reachability this device observed, per connection.
    pub presence: HashMap<ConnectionId, Presence>,
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

    /// Reachability this device last observed for `id`.
    pub fn presence_of(&self, id: ConnectionId) -> Presence {
        self.presence.get(&id).copied().unwrap_or(Presence::Unknown)
    }

    /// Count of currently-online connections — drives the header stat.
    pub fn online_count(&self) -> usize {
        self.connections
            .iter()
            .filter(|c| self.presence_of(c.id) == Presence::Online)
            .count()
    }
}

/// "Last used" as a person reads it, `now_ms` being the current time.
pub fn last_used_label(last_used_ms: Option<i64>, now_ms: i64) -> String {
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let Some(at) = last_used_ms else {
        return "Never".into();
    };
    let ago = now_ms.saturating_sub(at).max(0);
    match ago {
        _ if ago < MINUTE => "just now".into(),
        _ if ago < HOUR => format!("{}m ago", ago / MINUTE),
        _ if ago < DAY => format!("{}h ago", ago / HOUR),
        _ if ago < 2 * DAY => "yesterday".into(),
        _ if ago < 7 * DAY => format!("{}d ago", ago / DAY),
        _ => format!("{}w ago", ago / (7 * DAY)),
    }
}

#[cfg(test)]
mod tests {
    use iroh_base::SecretKey;

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
            last_used_ms: None,
            group: String::new(),
            favorite: false,
        }
    }

    #[test]
    fn addresses_format_per_transport() {
        eprintln!("phase address_formats_user_host_port");
        {
            let mut c = sample("s", ConnectionKind::Ssh);
            c.host = "10.0.0.1".into();
            c.user = "root".into();
            c.port = 2222;
            assert_eq!(c.address(), "root@10.0.0.1:2222");
        }

        eprintln!("phase tilo_address_drops_port");
        {
            let mut c = sample("q", ConnectionKind::Tilo);
            c.host = "studio-node".into();
            c.user = String::new();
            c.port = 0;
            assert_eq!(c.address(), "Tilo device studio-node");
        }

        eprintln!("phase tilo_address_compacts_a_long_endpoint_without_string_slicing");
        {
            let mut c = sample("q", ConnectionKind::Tilo);
            c.host = "abcdefghijklmnopqrstuvwxyz".into();
            assert_eq!(c.address(), "Tilo device abcdefghijkl…");
        }
    }

    #[test]
    fn drafts_validate_only_backed_transports_and_live_auth_paths() {
        eprintln!("phase only_backed_transports_are_available_to_create");
        {
            assert_eq!(
                ConnectionKind::available_to_create(),
                [
                    ConnectionKind::Ssh,
                    ConnectionKind::Vnc,
                    ConnectionKind::Tilo
                ]
            );
            assert!(ConnectionKind::Ssh.is_supported());
            assert!(ConnectionKind::Vnc.is_supported());
            assert!(!ConnectionKind::Rdp.is_supported());
            assert!(ConnectionKind::Tilo.is_supported());
            assert!(ConnectionKind::Tilo.can_attempt());
            assert!(ConnectionKind::Tilo.streams_framebuffer());
            assert_eq!(ConnectionKind::Tilo.label(), "Tilo device");
            assert!(!ConnectionKind::Rdp.can_attempt());
        }

        eprintln!("phase draft_validation_catches_empty_and_bad_port");
        {
            let mut d = ConnectionDraft::default();
            assert!(!d.problems().is_empty());
            d.name = "x".into();
            d.host = "h".into();
            d.port = "0".into();
            assert!(d.problems().iter().any(|p| p.contains("Port")));
            d.port = "22".into();
            assert!(d.problems().is_empty());
        }

        eprintln!("phase tilo_draft_ignores_port");
        {
            let d = ConnectionDraft {
                kind: ConnectionKind::Tilo,
                name: "node".into(),
                host: SecretKey::from_bytes(&[7; 32]).public().to_string(),
                port: String::new(),
                auth: AuthMethod::DeviceIdentity,
                ..Default::default()
            };
            assert!(d.problems().is_empty());
            let built = d.build(Uuid::new_v4());
            assert_eq!(built.port, 0);
        }

        eprintln!("phase q_os_profiles_require_an_exact_device_identity_and_endpoint");
        {
            let mut connection = sample("q", ConnectionKind::Tilo);
            connection.host = "friendly-name".into();
            connection.auth = AuthMethod::Password;
            assert_eq!(
                connection.profile_issue(),
                Some("Tilo device profiles must use Tilo device identity.")
            );

            connection.auth = AuthMethod::DeviceIdentity;
            assert_eq!(
                connection.profile_issue(),
                Some("Tilo device node must be an exact certified endpoint id.")
            );

            connection.host = SecretKey::generate().public().to_string();
            assert_eq!(connection.profile_issue(), None);
        }

        eprintln!("phase protocol_specific_authentication_choices_never_offer_dead_paths");
        {
            assert_eq!(
                AuthMethod::available_for(ConnectionKind::Ssh),
                &[AuthMethod::Password, AuthMethod::PublicKey]
            );
            assert_eq!(
                AuthMethod::available_for(ConnectionKind::Vnc),
                &[AuthMethod::Password]
            );
            assert_eq!(
                AuthMethod::available_for(ConnectionKind::Tilo),
                &[AuthMethod::DeviceIdentity]
            );
        }
    }

    #[test]
    fn roster_upserts_and_filters_favorites_first() {
        eprintln!("phase upsert_replaces_same_id");
        {
            let mut w = Workspace::new();
            let mut c = sample("a", ConnectionKind::Ssh);
            let id = w.upsert(c.clone());
            c.name = "renamed".into();
            w.upsert(c);
            assert_eq!(w.connections().len(), 1);
            assert_eq!(w.get(id).unwrap().name, "renamed");
        }

        eprintln!("phase filtered_orders_favorites_first");
        {
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

        eprintln!("phase filtered_matches_query");
        {
            let mut w = Workspace::new();
            w.upsert(sample("Build Box", ConnectionKind::Ssh));
            w.upsert(sample("Office NAS", ConnectionKind::Vnc));
            let hits: Vec<_> = w.filtered("build").iter().map(|c| c.name.clone()).collect();
            assert_eq!(hits, vec!["Build Box"]);
        }

        eprintln!("phase reachability_is_observed_per_device");
        {
            let mut w = Workspace::new();
            let a = w.upsert(sample("a", ConnectionKind::Ssh));
            let b = w.upsert(sample("b", ConnectionKind::Ssh));
            assert_eq!(w.presence_of(a), Presence::Unknown, "never probed");
            w.presence.insert(a, Presence::Online);
            w.presence.insert(b, Presence::Offline);
            assert_eq!(w.online_count(), 1);
        }
    }

    #[test]
    fn last_used_reads_relative_to_now() {
        let now = 1_000_000_000_000;
        let minutes = |m: i64| Some(now - m * 60_000);
        assert_eq!(last_used_label(None, now), "Never");
        assert_eq!(last_used_label(minutes(0), now), "just now");
        assert_eq!(last_used_label(minutes(12), now), "12m ago");
        assert_eq!(last_used_label(minutes(5 * 60), now), "5h ago");
        assert_eq!(last_used_label(minutes(30 * 60), now), "yesterday");
        assert_eq!(last_used_label(minutes(3 * 24 * 60), now), "3d ago");
        assert_eq!(last_used_label(minutes(7 * 24 * 60), now), "1w ago");
        assert_eq!(
            last_used_label(Some(now + 60_000), now),
            "just now",
            "a clock that runs behind another device's reads as now"
        );
    }
}
