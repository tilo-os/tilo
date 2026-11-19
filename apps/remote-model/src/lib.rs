//! The app's data model: a flat list of saved [`Connection`]s, the collaborative part, and
//! a little per-peer view state. Two people browsing one list can look at different
//! machines.

use serde::{Deserialize, Serialize};
use tilo_resource::{self, Atom, Collection, Id, JsonSchema, Model, Refusal, resource};
use uuid::Uuid;

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
            Some(problem) => Err(Refusal::new(problem.sentence())),
            None => Ok(()),
        }
    }
}

/// What is wrong with a draft, as its catalogue key: the form names it in the person's
/// language, and the check that saves a connection refuses with [`sentence`].
///
/// [`sentence`]: DraftProblem::sentence
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftProblem {
    /// The connection is unnamed.
    Name,
    /// The connection has no address.
    Address,
    /// The port is not one a connection can use.
    Port,
    /// The saved profile's sign-in does not suit its protocol: the `remote-issue-*` key.
    ProfileIssue(&'static str),
}

impl DraftProblem {
    /// The catalogue key that says it.
    #[must_use]
    pub fn message_key(self) -> &'static str {
        match self {
            Self::Name => "remote-issue-needs-name",
            Self::Address => "remote-issue-needs-address",
            Self::Port => "remote-issue-port",
            Self::ProfileIssue(issue) => issue,
        }
    }

    /// What it says, in the model's own words: what a refusal answers a caller with.
    #[must_use]
    pub fn sentence(self) -> String {
        match self {
            Self::Name => "a connection needs a name".to_owned(),
            Self::Address => "a connection needs an address".to_owned(),
            Self::Port => "a port is 1–65535".to_owned(),
            Self::ProfileIssue(issue) => match issue {
                "remote-issue-ssh-auth" => "SSH signs in with a password or a key".to_owned(),
                "remote-issue-vnc-auth" => "VNC signs in with a password".to_owned(),
                "remote-issue-rdp-auth" => "RDP signs in with a password".to_owned(),
                "remote-issue-tilo-auth" => {
                    "a Tilo device signs in with the device's identity".to_owned()
                }
                _ => "the address is not this device's exact endpoint id".to_owned(),
            },
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
    if kind == ConnectionKind::Tilo && !host.trim().is_empty() && !is_endpoint_id(host.trim()) {
        return Some("remote-issue-tilo-endpoint");
    }
    None
}

/// Whether `text` is a device's endpoint id: an Ed25519 public key (32 bytes,
/// as lowercase hex or unpadded base32, as iroh writes and reads one) whose
/// point is on the curve: exactly what `iroh_base::EndpointId` accepts.
fn is_endpoint_id(text: &str) -> bool {
    let mut bytes = [0u8; 32];
    let decoded = if text.len() == 64 {
        data_encoding::HEXLOWER.decode_mut(text.as_bytes(), &mut bytes)
    } else {
        let upper = text.to_ascii_uppercase();
        if data_encoding::BASE32_NOPAD.decode_len(upper.len()) != Ok(bytes.len()) {
            return false;
        }
        data_encoding::BASE32_NOPAD.decode_mut(upper.as_bytes(), &mut bytes)
    };
    decoded == Ok(32) && ed25519_dalek::VerifyingKey::from_bytes(&bytes).is_ok()
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
    #[must_use]
    pub fn problems(&self) -> Vec<DraftProblem> {
        let mut out = Vec::new();
        if self.name.trim().is_empty() {
            out.push(DraftProblem::Name);
        }
        if self.host.trim().is_empty() {
            out.push(DraftProblem::Address);
        }
        // A Tilo relay uses a logical channel, so it has no numeric port.
        if self.kind != ConnectionKind::Tilo {
            match self.port.trim().parse::<u16>() {
                Ok(0) | Err(_) => out.push(DraftProblem::Port),
                Ok(_) => {}
            }
        }
        if let Some(issue) = profile_issue(self.kind, &self.host, self.auth) {
            out.push(DraftProblem::ProfileIssue(issue));
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

    tilo_test_env::scenario! {
        fn addresses_format_per_transport;

        phase address_formats_user_host_port;
        {
            let mut c = sample("s", ConnectionKind::Ssh);
            c.host = Atom("10.0.0.1".into());
            c.user = Atom("root".into());
            c.port = 2222;
            assert_eq!(c.address(), "root@10.0.0.1:2222");
            c.port = 22;
            assert_eq!(c.address(), "root@10.0.0.1", "the default port goes unsaid");
        }

        phase tilo_address_drops_port;
        {
            let mut c = sample("q", ConnectionKind::Tilo);
            c.host = Atom("studio-node".into());
            c.user = Atom::default();
            c.port = 0;
            assert_eq!(c.address(), "studio-node");
        }

        phase tilo_address_compacts_a_long_endpoint_without_string_slicing;
        {
            let mut c = sample("q", ConnectionKind::Tilo);
            c.host = Atom("abcdefghijklmnopqrstuvwxyz".into());
            assert_eq!(c.address(), "abcdefghijkl…");
        }
    }

    tilo_test_env::scenario! {
        fn drafts_validate_only_backed_transports_and_live_auth_paths;

        phase only_backed_transports_are_available_to_create;
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
            assert_eq!(ConnectionKind::Tilo.label_message(), "remote-kind-tilo");
        }

        phase draft_validation_catches_empty_and_bad_port;
        {
            let mut d = ConnectionDraft::default();
            assert!(!d.problems().is_empty());
            d.name = "x".into();
            d.host = "h".into();
            d.port = "0".into();
            assert!(d.problems().contains(&DraftProblem::Port));
            d.port = "22".into();
            assert!(d.problems().is_empty());
        }

        phase tilo_draft_ignores_port;
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

        phase q_os_profiles_require_an_exact_device_identity_and_endpoint;
        {
            let mut connection = sample("q", ConnectionKind::Tilo);
            connection.host = Atom("friendly-name".into());
            connection.auth = AuthMethod::Password;
            assert_eq!(
                connection.profile_issue(),
                Some("remote-issue-tilo-auth")
            );

            connection.auth = AuthMethod::DeviceIdentity;
            assert_eq!(
                connection.profile_issue(),
                Some("remote-issue-tilo-endpoint")
            );

            connection.host = Atom(SecretKey::generate().public().to_string());
            assert_eq!(connection.profile_issue(), None);
        }

        phase an_endpoint_id_is_read_as_iroh_reads_it;
        {
            let key = SecretKey::generate().public();
            let hex = key.to_string();
            let base32 = data_encoding::BASE32_NOPAD
                .encode(key.as_bytes())
                .to_ascii_lowercase();
            // 32 bytes that are not a point on the curve.
            let off_curve = data_encoding::HEXLOWER.encode(&[0xff; 32]);
            let short: String = hex.chars().skip(1).collect();
            for text in [
                hex.as_str(),
                base32.as_str(),
                &hex.to_ascii_uppercase(),
                short.as_str(),
                off_curve.as_str(),
                "friendly-name",
                "",
            ] {
                assert_eq!(
                    is_endpoint_id(text),
                    text.parse::<iroh_base::EndpointId>().is_ok(),
                    "{text:?}"
                );
            }
        }

        phase protocol_specific_authentication_choices_never_offer_dead_paths;
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
}

impl Remote {
    /// What the resource holds: the saved connections.
    ///
    /// # Errors
    /// When a field cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "remote.connection-list",
    read_content = Remote::read_document
)]
impl Remote {
    /// Save a new connection, signing in the protocol's first way.
    #[action]
    pub fn add_connection(
        &mut self,
        cx: &mut tilo_resource::ActionCx,
        #[param("Name")] name: String,
        #[param("Address, or a Tilo endpoint id")] host: String,
        #[param("ssh, vnc or tilo")] kind: ConnectionKind,
        #[param("Port; the protocol's own when none")] port: Option<u16>,
        #[param("User")] user: Option<String>,
    ) -> Result<(), Refusal> {
        if !kind.is_supported() {
            return Err(Refusal::new("`kind` is ssh, vnc or tilo"));
        }
        let port = port.unwrap_or_else(|| kind.default_port());
        let draft = ConnectionDraft {
            name,
            host,
            port: port.to_string(),
            user: user.unwrap_or_default(),
            kind,
            auth: AuthMethod::available_for(kind)
                .first()
                .copied()
                .unwrap_or(AuthMethod::Password),
            group: String::new(),
        };
        if let Some(problem) = draft.problems().into_iter().next() {
            return Err(Refusal::new(problem.sentence()));
        }
        let id = cx.uuid();
        self.connections.push(Self::item(id), draft.build(id));
        Ok(())
    }

    /// Remove a saved connection from every device, and the passwords each device saved for
    /// it.
    #[action(
        destructive,
        irreversible,
        item = connections,
        label = "remote-delete-connection-button"
    )]
    pub fn delete_connection(&mut self, connection: &Id) -> Result<(), Refusal> {
        self.connections
            .remove(connection)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such connection"))
    }

    /// Set when a session was opened at `at_ms`. The later of two concurrent resources wins
    /// on every replica.
    #[action(housekeeping, item = connections)]
    pub fn mark_used(
        &mut self,
        connection: &Id,
        #[param("When the session was opened, in milliseconds since the Unix epoch")] at_ms: i64,
    ) -> Result<(), Refusal> {
        let connection = self
            .connections
            .get_mut(connection)
            .ok_or("no such connection")?;
        if connection.last_used_ms.is_none_or(|last| last < at_ms) {
            connection.last_used_ms = Some(at_ms);
        }
        Ok(())
    }
}
