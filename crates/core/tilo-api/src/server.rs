//! The person's workspaces on the server, reached as a grant: the same way
//! into a workspace every device takes ([`Device::enter_workspace`]).
//!
//! The grant's own key is the device. It dials the server's iroh endpoint
//! (which the server's public origin names, `/peer/v1/host`) with the grant's
//! certificate, and asks the server's own operations (its person's list, making
//! and deleting their workspaces, their account) on that session, in no
//! workspace. It enters one of the person's workspaces its scope reaches as
//! any device of theirs does: the server invites it in on the session, it
//! redeems the invitation there, and joins.
//!
//! What it does there is decided by the server, at every handshake and audit:
//! the grant's scope, its certificate's expiry, and its person's
//! revocations, a minted grant's issuer included.

use crate::grant_key::GrantKey;
use collab_types::WorkspaceId;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_server_client::CertifyError;
/// Where a server is dialed over iroh, and that with its certificate.
pub use tilo_server_protocol::web::{ServerAddress, ServerInfo};
use tilo_server_protocol::workspaces::{AdminRefusal as WorkspaceRefusal, WorkspaceLabels};
use tilo_session_client::PeerConfig;
use tilo_session_client::device::{Device, DeviceRefusal};

/// How long entering a workspace may take.
const ENTERING: Duration = Duration::from_secs(30);

/// A workspace as the account lists it: what `/v1` answers, and what an MCP
/// client and the desktop read of the person's workspaces.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct Workspace {
    /// What every route names it by.
    pub id: String,
    pub name: String,
    /// The host that keeps it: a server's name, or the person's device that
    /// hosts it; absent while none of their devices said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// `owner` for one of the person's own, else the role they hold there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What the person labelled it with when they made it (`customer_id` →
    /// `cus_8121`), read by their own keys alone.
    #[serde(default, skip_serializing_if = "WorkspaceLabels::is_empty")]
    pub labels: WorkspaceLabels,
    /// Keys made for this workspace alone run commands in its Linux machine
    /// without asking the person each time; absent while they are asked, as
    /// every workspace starts. Read by the owner's keys alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unattended: bool,
    /// One of the person's own, rather than one they were invited into.
    #[serde(default)]
    pub own: bool,
    /// Whether the device answering keeps a copy of it.
    #[serde(default)]
    pub kept_here: bool,
    /// Where the device keeping it is reached, as the person's list says.
    #[serde(skip)]
    pub address: Option<tilo_server_protocol::workspaces::KeepingDeviceAddress>,
}

impl Workspace {
    /// `listed`, a workspace of the person's list as their own workspace's record names it,
    /// joined with its membership.
    pub(crate) fn record(listed: tilo_server_protocol::workspaces::ListedWorkspace) -> Self {
        Self {
            id: listed.workspace,
            name: listed.name,
            host: listed.host.as_ref().map(|host| host.name.clone()),
            own: listed.role.as_deref() == Some(tilo_server_protocol::workspaces::OWNER_ROLE),
            kept_here: false,
            role: listed.role,
            labels: listed.labels,
            unattended: listed.unattended,
            address: listed.host,
        }
    }

    /// `listing`, a workspace the server `host` made or answered.
    pub(crate) fn listed(
        listing: &tilo_server_protocol::workspaces::WorkspaceListing,
        host: Option<String>,
    ) -> Self {
        Self {
            id: listing.workspace.clone(),
            name: listing.name.clone(),
            host,
            own: listing.role.as_deref() == Some("owner"),
            kept_here: false,
            role: listing.role.clone(),
            labels: listing.labels.clone(),
            unattended: listing.unattended,
            address: None,
        }
    }
}

/// Why the server did not answer what was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerError {
    /// Not one of the person's workspaces, or no such workspace: the same
    /// answer, so a guess learns nothing.
    NotFound,
    /// The server refused, with its reason (one workspace more than the
    /// person's plan keeps, quota, policy).
    Refused(String),
    /// The server or the site could not be reached.
    Unavailable(String),
    /// The server refused one of its own workspace operations, saying which
    /// case it was instead of leaving the caller to read it from words.
    RefusedWorkspace(WorkspaceRefusal),
    /// No server answered with a session, so nothing that can read the
    /// workspace is online.
    NoHolder,
    /// It waits for the person to approve it on a device that holds their
    /// identity, and goes on once they allow it there: what the person is
    /// told.
    AwaitingApproval(String),
    /// The server is part of a Tilo desktop, which answers its own
    /// workspaces and keeps no account: nothing makes, branches or deletes
    /// workspaces or keys there.
    NoAccountHere,
    /// The server does not know the person yet, and says what makes them known there: its
    /// operator declares them an owner, or an invitation brings them in.
    NotOnThisServer(String),
    /// The workspace keeps the key's grant as revoked: nothing it asks is
    /// answered again, and its person approves another.
    KeyRevoked,
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such workspace of yours"),
            Self::KeyRevoked => f.write_str("this key was revoked"),
            Self::AwaitingApproval(said) | Self::NotOnThisServer(said) => f.write_str(said),
            Self::NoAccountHere => f.write_str(
                "This Tilo desktop answers its own workspaces only: do this in Tilo itself.",
            ),
            Self::NoHolder => f.write_str(
                "No device that can read this workspace is online. Open Tilo on a device that \
                 holds it, then try again.",
            ),
            // What the server refused is kept for the log: the person (or the
            // agent relaying it) reads what they can do about it.
            Self::Refused(reason) => {
                tracing::info!(reason, "the server refused a call");
                match tilo_action_services::server_account::Refusal::from_reason(reason) {
                    Some(refusal) => f.write_str(&refusal.message),
                    None => f.write_str(tilo_server_protocol::refusal::PLAIN_REFUSED),
                }
            }
            Self::Unavailable(reason) => {
                tracing::debug!(reason, "the server was not reached");
                f.write_str("Tilo couldn't reach your workspace; try again in a minute.")
            }
            Self::RefusedWorkspace(refusal) => {
                tracing::info!(?refusal, "the server refused a workspace operation");
                match refusal {
                    WorkspaceRefusal::NotYourOwn => f.write_str("no such workspace of yours"),
                    WorkspaceRefusal::BranchNotMade { part } => {
                        write!(
                            f,
                            "The branch wasn't made: {part} didn't come along. Nothing of it was kept."
                        )
                    }
                    WorkspaceRefusal::NoDeviceToFound => f.write_str(
                        "This server can't make a workspace: it has no device of its own to make \
                         it on. Make it on one of your devices.",
                    ),
                    WorkspaceRefusal::FoundOnYourDevice => f.write_str(
                        "Your workspaces are made on your own device, which places them on your \
                         server. Make it there.",
                    ),
                }
            }
        }
    }
}

impl From<CertifyError> for ServerError {
    fn from(error: CertifyError) -> Self {
        match error {
            // A revoked key is told so wherever its workspace refuses it.
            CertifyError::Denied(reason) if reason.contains("key_revoked") => Self::KeyRevoked,
            CertifyError::Denied(reason) => Self::Refused(reason),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

impl From<DeviceRefusal> for ServerError {
    fn from(refusal: DeviceRefusal) -> Self {
        if refusal.code == DeviceRefusal::AWAITING_APPROVAL {
            return Self::AwaitingApproval(refusal.message);
        }
        if refusal.code == DeviceRefusal::NOT_ON_THIS_SERVER {
            return Self::NotOnThisServer(refusal.message);
        }
        // A workspace operation of the server's own answers a typed refusal;
        // the server's other refusals are read as they were.
        if let Some(refused) = WorkspaceRefusal::from_reason(&refusal.message) {
            return Self::RefusedWorkspace(refused);
        }
        CertifyError::from(refusal).into()
    }
}

/// The server the person's workspaces live on.
pub struct Server {
    /// The server's name, as hosts are named (`eu1.tilo.sh`).
    name: String,
    /// The web origin its links name (`https://eu1.tilo.sh`).
    origin: String,
    /// Its certificate and iroh address, as its deployment named it: what a session is dialled
    /// with, never asked of its web origin.
    info: ServerInfo,
    /// What each grant's device keeps between its sessions, by its device key: its place in
    /// the key group of each workspace it reads. A grant's session ends with its last run and
    /// the next run opens another; that one takes the place up again, where a session with a
    /// state of its own asked with a new key, took the device's place from the session before
    /// it and was handed it back, commit after commit.
    kept: Mutex<HashMap<[u8; 32], Arc<tilo_session_client::MemoryPeerStore>>>,
}

impl Server {
    /// The server `info` names: its certificate, where its endpoint is dialled, and its HTTPS
    /// origin, which names it and its links (its endpoint's key where it serves no web: a key
    /// file's links use its own site, and this is only what the API calls the host). Never its
    /// WebTransport listener's origin, which may be another name and port.
    #[must_use]
    pub fn new(info: ServerInfo) -> Self {
        let origin = info
            .origin
            .as_ref()
            .map(|origin| origin.trim_end_matches('/').to_owned())
            .or_else(|| {
                info.address
                    .as_ref()
                    .map(|address| address.endpoint_addr.id.to_string())
            })
            .unwrap_or_default();
        let name = origin
            .split_once("://")
            .map_or(origin.as_str(), |(_, rest)| rest)
            .to_owned();
        Self {
            name,
            origin,
            info,
            kept: Mutex::default(),
        }
    }

    /// The server's name, as the account lists hosts.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its origin, as a link that admits into it names it.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// `key`'s session that `device` already holds with the server, in the
    /// workspace it entered: calls go over it rather than a session of their
    /// own, which would cut it off.
    #[must_use]
    pub fn held(self: &Arc<Self>, key: &GrantKey, device: Device) -> Connection {
        let entered = device.workspace_id();
        let mut workspaces = HashMap::new();
        if entered != WorkspaceId::from_u128(0) {
            workspaces.insert(
                entered.to_string(),
                Arc::new(tokio::sync::OnceCell::new_with(Some(entered))),
            );
        }
        Connection {
            device,
            key: key.clone(),
            server: Arc::clone(self),
            host: self.name.clone(),
            entered: Mutex::new(workspaces),
            let_in: Mutex::default(),
            endpoint: tokio::sync::OnceCell::new(),
        }
    }

    /// `key` on the server: its session, not in any workspace yet.
    ///
    /// # Errors
    /// The server was not reached or takes no device sessions.
    pub async fn connect(self: &Arc<Self>, key: &GrantKey) -> Result<Connection, ServerError> {
        let info = self.info.clone();
        let host = info
            .address
            .as_ref()
            .map(tilo_server_client::ServerAddress::published)
            .ok_or_else(|| {
                tracing::warn!(
                    relay = self.name(),
                    "the relay published no device address: no workspace can be reached through it"
                );
                ServerError::Unavailable("it publishes no device address".to_owned())
            })?;
        let label = key.cert().device_label.clone();
        let kept = Arc::clone(
            lock(&self.kept)
                .entry(key.cert().device_public_key)
                .or_insert_with(|| Arc::new(tilo_session_client::MemoryPeerStore::for_session())),
        );
        let device = Device::kept_in(
            PeerConfig {
                host,
                cert: key.cert().clone(),
                server_cert: info.server_cert,
                revocations: tilo_user_identity::RevocationList::new(),
                entering: None,
            },
            Arc::new(key.key().clone()),
            &label,
            kept,
        );
        let connection = Connection {
            device,
            key: key.clone(),
            server: Arc::clone(self),
            host: self.name.clone(),
            entered: Mutex::default(),
            let_in: Mutex::default(),
            endpoint: tokio::sync::OnceCell::new(),
        };
        connection.device.connected().await?;
        Ok(connection)
    }

    /// The workspaces `connection`'s grant reaches: the person's list as the
    /// server keeps it, every workspace of theirs wherever it is hosted (one
    /// of their desktops, a paired device, this server), each naming its host.
    ///
    /// # Errors
    /// The server was not reached or refused.
    pub async fn workspaces(&self, connection: &Connection) -> Result<Vec<Workspace>, ServerError> {
        self.labelled(connection, &WorkspaceLabels::new()).await
    }

    /// The workspaces `connection`'s grant reaches that are labelled with
    /// every one of `labels`: the server reads only those, however many the
    /// person has.
    ///
    /// # Errors
    /// The server was not reached or refused.
    pub async fn labelled(
        &self,
        connection: &Connection,
        labels: &WorkspaceLabels,
    ) -> Result<Vec<Workspace>, ServerError> {
        self.listed(connection, labels, None).await
    }

    /// `workspace` as `connection`'s grant reaches it, when it does: the
    /// server reads that one alone.
    ///
    /// # Errors
    /// The server was not reached or refused.
    pub async fn workspace(
        &self,
        connection: &Connection,
        workspace: &str,
    ) -> Result<Option<Workspace>, ServerError> {
        Ok(self
            .listed(connection, &WorkspaceLabels::new(), Some(workspace))
            .await?
            .into_iter()
            .find(|listed| listed.id == workspace))
    }

    async fn listed(
        &self,
        connection: &Connection,
        labels: &WorkspaceLabels,
        workspace: Option<&str>,
    ) -> Result<Vec<Workspace>, ServerError> {
        let listed = connection
            .device
            .person_workspaces(labels, workspace)
            .await
            .map_err(ServerError::from)?;
        // A list is kept, and a workspace listed, only for a person who
        // takes part there.
        if !listed.workspaces.is_empty() {
            connection.let_in();
        }
        Ok(listed
            .workspaces
            .into_iter()
            .map(Workspace::record)
            .collect())
    }

    /// The person's account here, as `connection` (reaching all their
    /// workspaces) reads it: their plan, and per workspace what it keeps and
    /// the VM time it ran.
    ///
    /// # Errors
    /// The server was not reached or refused.
    pub async fn account(&self, connection: &Connection) -> Result<Value, ServerError> {
        use tilo_action_services::server_account;
        connection
            .device
            .ask_host(
                tilo_session_client::session::HOST_ITSELF,
                server_account::target(server_account::ACCOUNT),
                Map::new(),
            )
            .await
            .map_err(ServerError::from)
    }

    /// Make one of the person's workspaces as `connection`, with the server's own operation
    /// `action` (making one, or branching one of theirs) and its `arguments`: the workspace it
    /// made, or, asked again under the same id, the one it made before. The grant enters it only
    /// once a run acts there ([`Server::enter`]).
    ///
    /// # Errors
    /// Not one of the person's own workspaces to branch ([`ServerError::NotFound`]), or the server
    /// refused (a branch naming what did not come) or was not reached.
    pub async fn make(
        &self,
        connection: &Connection,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<Workspace, ServerError> {
        let output = connection
            .device
            .host_action(action, arguments)
            .await
            .map_err(|refusal| match ServerError::from(refusal) {
                ServerError::RefusedWorkspace(WorkspaceRefusal::NotYourOwn) => {
                    ServerError::NotFound
                }
                other => other,
            })?;
        let made: tilo_server_protocol::workspaces::CreatedWorkspace =
            serde_json::from_value(output).map_err(|error| {
                ServerError::Unavailable(format!("the server answered no workspace: {error}"))
            })?;
        connection.let_in();
        Ok(connection.workspace(&made.workspace))
    }

    /// Delete the person's own `workspace` as `connection` (reaching all
    /// their workspaces): final, with everything the server keeps of it.
    ///
    /// # Errors
    /// Not one of the person's own workspaces ([`ServerError::NotFound`]),
    /// or the server refused or was not reached.
    pub async fn delete(
        &self,
        connection: &Connection,
        workspace: &str,
    ) -> Result<(), ServerError> {
        if tilo_server_protocol::WorkspaceId::parse(workspace).is_none() {
            return Err(ServerError::NotFound);
        }
        let mut arguments = Map::new();
        arguments.insert("workspace".into(), json!(workspace));
        connection
            .device
            .host_action(
                tilo_server_protocol::workspaces::DELETE_WORKSPACE,
                arguments,
            )
            .await
            .map_err(|refusal| match ServerError::from(refusal) {
                ServerError::RefusedWorkspace(WorkspaceRefusal::NotYourOwn) => {
                    ServerError::NotFound
                }
                other => other,
            })?;
        lock(&connection.entered).remove(workspace);
        Ok(())
    }

    /// Name the workspace `workspace` `name` as `connection`: the one name the
    /// workspace keeps, which every device and list of it reads. The key asks
    /// it in the workspace, and the workspace decides who may: its owners and
    /// moderators.
    ///
    /// # Errors
    /// Not a workspace the grant reaches, or the workspace refused (its
    /// reason, [`Administration::Refused`]) or did not answer.
    pub async fn rename(
        &self,
        connection: &Connection,
        workspace: &str,
        name: &str,
    ) -> Result<Workspace, Administration> {
        if !connection.key.scope().reaches(workspace)
            || tilo_server_protocol::WorkspaceId::parse(workspace).is_none()
        {
            return Err(Administration::Refused(ServerError::NotFound.to_string()));
        }
        let entered = self
            .enter(connection, workspace)
            .await
            .map_err(|error| Administration::Refused(error.to_string()))?;
        let mut arguments = Map::new();
        arguments.insert("name".into(), json!(name.trim()));
        connection
            .device
            .ask_host(
                entered,
                tilo_actions::remote::RemoteActionTarget::workspace_governance(
                    workspace,
                    tilo_actions::policy::roles::governance::RENAME,
                ),
                arguments,
            )
            .await
            .map_err(|refusal| match ServerError::from(refusal) {
                ServerError::Unavailable(why) => Administration::Unanswered(why),
                refused => Administration::Refused(refused.to_string()),
            })?;
        // The workspace as the person's list says it now, with its server
        // and their role.
        self.workspace(connection, workspace)
            .await
            .ok()
            .flatten()
            .ok_or_else(|| Administration::Unanswered("the workspace was not listed".into()))
    }

    /// Bring `connection` into `workspace`, one its grant reaches: its id on
    /// the session.
    ///
    /// # Errors
    /// Not a workspace the grant reaches, or the server refused or was not
    /// reached.
    pub async fn enter(
        &self,
        connection: &Connection,
        workspace: &str,
    ) -> Result<WorkspaceId, ServerError> {
        if !connection.key.scope().reaches(workspace)
            || tilo_server_protocol::WorkspaceId::parse(workspace).is_none()
        {
            return Err(ServerError::NotFound);
        }
        // Every run of the grant shares its session, so runs entering the
        // same workspace at once enter it once, and a failed entry is tried
        // again by the next.
        let entering = Arc::clone(
            lock(&connection.entered)
                .entry(workspace.to_owned())
                .or_default(),
        );
        let entered = entering
            .get_or_try_init(|| connection.enter(workspace))
            .await
            .copied();
        if entered.is_ok() {
            connection.let_in();
        }
        if entered.is_err() {
            let mut all = lock(&connection.entered);
            if all
                .get(workspace)
                .is_some_and(|known| Arc::ptr_eq(known, &entering) && !known.initialized())
            {
                all.remove(workspace);
            }
        }
        entered
    }
}

/// A grant's session with the server. The server keeps one session per key,
/// so every run of the grant shares this one.
pub struct Connection {
    pub device: Device,
    pub key: GrantKey,
    /// The server this session answers on: where the workspace it reaches is
    /// served, and the one the API server forwarded the call to.
    server: Arc<Server>,
    host: String,
    /// The workspaces it entered or is entering, by id: each joined once,
    /// and left with the session.
    entered: Mutex<HashMap<String, Arc<tokio::sync::OnceCell<WorkspaceId>>>>,
    /// What is told the next time the server this session is with does for
    /// its key what it does only for a person it lets in
    /// ([`Self::when_let_in`]).
    let_in: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// The grant's own iroh endpoint, keyed by it: what dials the person's
    /// devices that host a workspace of theirs.
    endpoint: tokio::sync::OnceCell<iroh::Endpoint>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.device.close();
        if let Some(endpoint) = self.endpoint.get().cloned() {
            tokio::spawn(async move { endpoint.close().await });
        }
    }
}

/// Why the server's workspace administration did not do what was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Administration {
    /// The workspace refused, saying why.
    Refused(String),
    /// The server did not answer.
    Unanswered(String),
}

impl Connection {
    /// Have `told` called the next time the server this session is with
    /// does for its key what it does only for a person it lets in: lists a
    /// workspace of theirs, makes one for them, or lets the key into one. A
    /// session alone says less, since a server that keeps workspaces for
    /// people takes a session from any certified key.
    pub fn when_let_in(&self, told: impl FnOnce() + Send + 'static) {
        *lock(&self.let_in) = Some(Box::new(told));
    }

    /// The server did for this session's key what it does only for a person
    /// it lets in.
    fn let_in(&self) {
        let told = lock(&self.let_in).take();
        if let Some(told) = told {
            told();
        }
    }

    /// The server this session answers on.
    #[must_use]
    pub fn server(&self) -> &Arc<Server> {
        &self.server
    }

    /// The grant's endpoint, bound the first time a workspace on one of the
    /// person's devices is entered.
    ///
    /// # Errors
    /// The endpoint could not bind.
    pub async fn endpoint(&self) -> Result<iroh::Endpoint, ServerError> {
        self.endpoint
            .get_or_try_init(|| async {
                // It only dials: the relays and directory the setting names find the
                // person's device, and nothing about the grant is published.
                let relays = tilo_endpoint_reachability::relays();
                let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                    .secret_key(iroh::SecretKey::from_bytes(&self.key.key().to_bytes()))
                    .portmapper_config(relays.portmapper())
                    .relay_mode(iroh::RelayMode::Custom(relays.relays));
                if relays.n0_dns {
                    builder =
                        builder.address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns());
                }
                builder
                    .bind()
                    .await
                    .map_err(|error| ServerError::Unavailable(error.to_string()))
            })
            .await
            .cloned()
    }

    fn workspace(&self, listing: &tilo_server_protocol::workspaces::WorkspaceListing) -> Workspace {
        Workspace::listed(listing, Some(self.host.clone()))
    }

    /// Whether the session still serves: not closed, and not refused by
    /// the server for good.
    #[must_use]
    pub fn is_live(&self) -> bool {
        // A workspace it entered that lets it in no more (a key its person revoked there) ends
        // the session: the key's next call opens another, which that workspace refuses saying
        // why, rather than going on answering from one it is no longer in.
        self.device.is_live()
            && lock(&self.entered)
                .values()
                .filter_map(|entered| entered.get())
                .all(|workspace| self.device.holds(*workspace))
    }

    /// Enter `workspace`, as every device enters one, in at most [`ENTERING`]:
    /// one the person does not have is not found, as one never made.
    async fn enter(&self, workspace: &str) -> Result<WorkspaceId, ServerError> {
        // Listed in the workspace by the name its person approved it as.
        let name = self.key.name();
        let began = std::time::Instant::now();
        let Ok(entered) =
            tokio::time::timeout(ENTERING, self.device.enter_workspace(workspace, name)).await
        else {
            tracing::warn!(
                workspace,
                waited_ms = began.elapsed().as_millis(),
                "api: a workspace did not admit the key in time"
            );
            return Err(ServerError::Unavailable(
                "the workspace did not admit this key in time".to_owned(),
            ));
        };
        tracing::debug!(
            workspace,
            waited_ms = began.elapsed().as_millis(),
            admitted = entered.is_ok(),
            "api: a key entered its workspace"
        );
        entered.map_err(|refusal| match ServerError::from(refusal) {
            // Not theirs, or not one at all: the same answer, so a guess
            // learns nothing; why is for the API server's operator.
            refused @ (ServerError::Refused(_) | ServerError::RefusedWorkspace(_)) => {
                tracing::info!(workspace, %refused, "api: a workspace refused a key's entry");
                ServerError::NotFound
            }
            other => other,
        })
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
