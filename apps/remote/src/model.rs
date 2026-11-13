//! The app's data model: a flat list of saved [`Connection`]s, the collaborative part, and
//! a little per-peer view state. Two people browsing one list can look at different
//! machines.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Atom, Collection, Id, JsonSchema, Model, Refusal};
use uuid::Uuid;

use crate::resource::CATALOG;

pub type ConnectionId = Uuid;

/// The transport a connection speaks. RDP stays a variant so older profiles round-trip;
/// Tilo device control rides the shell's authenticated workspace transport.
#[derive(Model, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    /// Secure shell: an interactive terminal.
    Ssh,
    /// Virtual Network Computing: a streamed framebuffer.
    Vnc,
    /// Remote Desktop Protocol.
    Rdp,
    /// A peer Tilo device reached through the device mesh.
    Tilo,
}

impl ConnectionKind {
    pub fn tag(self) -> &'static str {
        match self {
            ConnectionKind::Ssh => "SSH",
            ConnectionKind::Vnc => "VNC",
            ConnectionKind::Rdp => "RDP",
            ConnectionKind::Tilo => "Tilo",
        }
    }

    pub fn label_message(self) -> &'static str {
        match self {
            ConnectionKind::Ssh => "remote-kind-ssh",
            ConnectionKind::Vnc => "remote-kind-vnc",
            ConnectionKind::Rdp => "remote-kind-rdp",
            ConnectionKind::Tilo => "remote-kind-tilo",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            ConnectionKind::Ssh => 22,
            ConnectionKind::Vnc => 5900,
            ConnectionKind::Rdp => 3389,
            ConnectionKind::Tilo => 0,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, ConnectionKind::Ssh)
    }

    pub fn streams_framebuffer(self) -> bool {
        matches!(
            self,
            ConnectionKind::Vnc | ConnectionKind::Rdp | ConnectionKind::Tilo
        )
    }

    /// The kinds available when creating a profile. The persisted enum stays broader, so
    /// hiding a transport never destroys an older profile.
    pub fn available_to_create() -> [ConnectionKind; 3] {
        [
            ConnectionKind::Ssh,
            ConnectionKind::Vnc,
            ConnectionKind::Tilo,
        ]
    }

    pub fn is_supported(self) -> bool {
        matches!(
            self,
            ConnectionKind::Ssh | ConnectionKind::Vnc | ConnectionKind::Tilo
        )
    }

    /// Every persisted kind, in order; creation controls use
    /// [`Self::available_to_create`].
    pub fn all() -> [ConnectionKind; 4] {
        [
            ConnectionKind::Ssh,
            ConnectionKind::Vnc,
            ConnectionKind::Rdp,
            ConnectionKind::Tilo,
        ]
    }
}

/// How a connection signs in. Never a live secret: passwords are entered at connect time
/// and held in memory.
#[derive(Model, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// Username and password, asked at connect time.
    Password,
    /// Public-key auth: an agent or a key on disk holds the secret.
    PublicKey,
    /// Tilo device identity: the mesh authenticates the peer, so there is no credential to
    /// manage.
    DeviceIdentity,
}

impl AuthMethod {
    pub fn label_message(self) -> &'static str {
        match self {
            AuthMethod::Password => "remote-auth-password",
            AuthMethod::PublicKey => "remote-auth-public-key",
            AuthMethod::DeviceIdentity => "remote-auth-device-identity",
        }
    }

    pub fn all() -> [AuthMethod; 3] {
        [
            AuthMethod::Password,
            AuthMethod::PublicKey,
            AuthMethod::DeviceIdentity,
        ]
    }

    /// The methods the protocol supports, so the form never advertises a credential shape
    /// the transport would ignore.
    pub const fn available_for(kind: ConnectionKind) -> &'static [AuthMethod] {
        match kind {
            ConnectionKind::Ssh => &[Self::Password, Self::PublicKey],
            ConnectionKind::Vnc | ConnectionKind::Rdp => &[Self::Password],
            ConnectionKind::Tilo => &[Self::DeviceIdentity],
        }
    }

    pub fn is_supported_by(self, kind: ConnectionKind) -> bool {
        Self::available_for(kind).contains(&self)
    }
}

/// How a saved machine last answered, as this device observed it; never part of the
/// profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Presence {
    Online,
    Offline,
    Unknown,
}

/// One saved machine. Its id is its item's in [`Remote`]; the address parts are written
/// whole, the name merges as text.
#[derive(Model, Clone, Debug, PartialEq, Eq)]
#[model(check = Connection::valid)]
pub struct Connection {
    /// The connection's id, its item's in [`Remote::connections`].
    #[model(skip)]
    pub id: ConnectionId,
    /// Name
    #[model(label)]
    pub name: String,
    /// Host
    pub host: Atom,
    /// Port
    pub port: u16,
    /// User
    pub user: Atom,
    /// Protocol
    pub kind: ConnectionKind,
    /// Authentication
    pub auth: AuthMethod,
    /// Last used
    #[model(readonly)]
    pub last_used_ms: Option<i64>,
    /// Group
    pub group: Atom,
    /// Pinned
    pub favorite: bool,
}

/// The saved connections. What this device observes live, its sessions, the form being
/// filled in and its credentials are never part of it.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Remote {
    /// Saved connections
    pub connections: Collection<Connection>,
}

impl Remote {
    pub fn list(&self) -> Vec<Connection> {
        self.connections
            .iter()
            .filter_map(|(id, connection)| {
                Some(Connection {
                    id: id.as_str().parse().ok()?,
                    ..connection.clone()
                })
            })
            .collect()
    }

    pub fn item(id: ConnectionId) -> Id {
        Id::from_raw(id.to_string())
    }
}

impl Connection {
    pub fn address(&self) -> String {
        if self.kind == ConnectionKind::Tilo {
            // A certified endpoint id is long: show a stable prefix here, while the form
            // keeps the exact target id.
            return compact_tilo_endpoint(&self.host.0);
        }
        let host_part = if self.port == 0 || self.port == self.kind.default_port() {
            self.host.0.clone()
        } else {
            format!("{}:{}", self.host.0, self.port)
        };
        if self.user.0.is_empty() || self.kind != ConnectionKind::Ssh {
            host_part
        } else {
            format!("{}@{}", self.user.0, host_part)
        }
    }

    /// Why this profile cannot safely start a transport, as the catalogue key a window
    /// paints. No connect attempt starts until it returns `None`.
    pub fn profile_issue(&self) -> Option<&'static str> {
        profile_issue(self.kind, &self.host.0, self.auth)
    }

    /// What every saved connection is, however it was written.
    fn valid(&self) -> Result<(), Refusal> {
        let draft = ConnectionDraft::from_connection(self);
        match draft.problems().into_iter().next() {
            Some(problem) => Err(Refusal::new(problem)),
            None => Ok(()),
        }
    }
}

/// An exact Tilo endpoint id cut short for list rows and picker labels, never by byte
/// offset.
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
            ConnectionKind::Ssh => "remote-issue-ssh-auth",
            ConnectionKind::Vnc => "remote-issue-vnc-auth",
            ConnectionKind::Rdp => "remote-issue-rdp-auth",
            ConnectionKind::Tilo => "remote-issue-tilo-auth",
        });
    }
    if kind == ConnectionKind::Tilo
        && !host.trim().is_empty()
        && host.trim().parse::<iroh_base::EndpointId>().is_err()
    {
        return Some("remote-issue-tilo-endpoint");
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    List,
    Form { editing: Option<ConnectionId> },
    Session { id: ConnectionId },
}

/// A draft of the add/edit form, private to the editor until Save.
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
    pub fn from_connection(c: &Connection) -> Self {
        Self {
            name: c.name.clone(),
            host: c.host.0.clone(),
            port: c.port.to_string(),
            user: c.user.0.clone(),
            kind: c.kind,
            auth: c.auth,
            group: c.group.0.clone(),
        }
    }

    /// What is wrong with the draft; an empty list means it can be saved.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.name.trim().is_empty() {
            out.push("Name is required".into());
        }
        if self.host.trim().is_empty() {
            out.push("Host is required".into());
        }
        // A Tilo relay uses a logical channel, so it has no numeric port.
        if self.kind != ConnectionKind::Tilo {
            match self.port.trim().parse::<u16>() {
                Ok(0) | Err(_) => out.push("Port must be 1–65535".into()),
                Ok(_) => {}
            }
        }
        if let Some(issue) = profile_issue(self.kind, &self.host, self.auth) {
            out.push(CATALOG.t(issue));
        }
        out
    }

    /// The [`Connection`] the draft describes, reusing `id`.
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
            host: Atom(self.host.trim().to_string()),
            port,
            user: Atom(
                if self.kind == ConnectionKind::Ssh || self.kind == ConnectionKind::Rdp {
                    self.user.trim().to_string()
                } else {
                    String::new()
                },
            ),
            kind: self.kind,
            auth: self.auth,
            last_used_ms: None,
            group: Atom(self.group.trim().to_string()),
            favorite: false,
        }
    }
}

/// The connections in memory with the operations the window needs. No IO, no CRDT.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub connections: Vec<Connection>,
    /// How each connection last answered here.
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

    /// Insert a new connection, or replace one with the same id.
    pub fn upsert(&mut self, c: Connection) -> ConnectionId {
        let id = c.id;
        if let Some(existing) = self.get_mut(id) {
            *existing = c;
        } else {
            self.connections.push(c);
        }
        id
    }

    pub fn remove(&mut self, id: ConnectionId) -> bool {
        let before = self.connections.len();
        self.connections.retain(|c| c.id != id);
        self.connections.len() != before
    }

    pub fn toggle_favorite(&mut self, id: ConnectionId) -> bool {
        if let Some(c) = self.get_mut(id) {
            c.favorite = !c.favorite;
            c.favorite
        } else {
            false
        }
    }

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
                    || c.host.0.to_lowercase().contains(&q)
                    || c.user.0.to_lowercase().contains(&q)
                    || c.group.0.to_lowercase().contains(&q)
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
}

#[cfg(test)]
mod tests {
    use iroh_base::SecretKey;

    use super::*;

    fn sample(name: &str, kind: ConnectionKind) -> Connection {
        Connection {
            id: Uuid::new_v4(),
            name: name.into(),
            host: Atom("host".into()),
            port: kind.default_port(),
            user: Atom("user".into()),
            kind,
            auth: AuthMethod::Password,
            last_used_ms: None,
            group: Atom::default(),
            favorite: false,
        }
    }

    #[test]
    fn addresses_format_per_transport() {
        eprintln!("phase address_formats_user_host_port");
        {
            let mut c = sample("s", ConnectionKind::Ssh);
            c.host = Atom("10.0.0.1".into());
            c.user = Atom("root".into());
            c.port = 2222;
            assert_eq!(c.address(), "root@10.0.0.1:2222");
            c.port = 22;
            assert_eq!(c.address(), "root@10.0.0.1", "the default port goes unsaid");
        }

        eprintln!("phase tilo_address_drops_port");
        {
            let mut c = sample("q", ConnectionKind::Tilo);
            c.host = Atom("studio-node".into());
            c.user = Atom::default();
            c.port = 0;
            assert_eq!(c.address(), "studio-node");
        }

        eprintln!("phase tilo_address_compacts_a_long_endpoint_without_string_slicing");
        {
            let mut c = sample("q", ConnectionKind::Tilo);
            c.host = Atom("abcdefghijklmnopqrstuvwxyz".into());
            assert_eq!(c.address(), "abcdefghijkl…");
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
            assert!(ConnectionKind::Tilo.streams_framebuffer());
            assert_eq!(
                CATALOG.t(ConnectionKind::Tilo.label_message()),
                "Tilo device"
            );
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
            connection.host = Atom("friendly-name".into());
            connection.auth = AuthMethod::Password;
            assert_eq!(
                connection
                    .profile_issue()
                    .map(|issue| CATALOG.t(issue))
                    .as_deref(),
                Some("Tilo device profiles must use Tilo device identity.")
            );

            connection.auth = AuthMethod::DeviceIdentity;
            assert_eq!(
                connection
                    .profile_issue()
                    .map(|issue| CATALOG.t(issue))
                    .as_deref(),
                Some("Tilo device node must be the device's exact endpoint id.")
            );

            connection.host = Atom(SecretKey::generate().public().to_string());
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
            assert_eq!(w.presence_of(a), Presence::Online);
            assert_eq!(w.presence_of(b), Presence::Offline);
        }
    }
}
