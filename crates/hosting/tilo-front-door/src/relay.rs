//! The person's workspaces on the relay, reached as a grant: the door's one
//! way in, the same a `tilo login` computer takes (`tilo_browser_peer::login`).
//!
//! The grant's own key is the device. It dials the relay's iroh endpoint
//! (which the relay's public origin names, `/peer/v1/host`) with the grant's
//! certificate, and the relay admits it as its person's agent
//! (`tilo_room_host::workspace_access`) in exactly the workspaces its scope
//! reaches where it redeemed its person's invitation:
//!
//! - **A grant reaching all the person's workspaces** goes through the
//!   site's door to the site's home workspace, as the person's page does
//!   (the door the site's `entry.json` names for the relay), and from there
//!   asks the relay for the person's own workspace (`room.create-own`),
//!   which answers it with an invitation for the person; the grant redeems
//!   it and joins.
//! - **A minted grant** reaches one workspace and never the home workspace.
//!   Its parent, which reaches all of them, asks for the invitation into
//!   that workspace (`room.create-own` naming it, which only answers the
//!   person's own workspace and never makes another), and the minted grant
//!   redeems it itself and joins.
//!
//! What either does there is decided by the relay, at every handshake and
//! audit: the grant's scope, its certificate's expiry, and its person's
//! revocations, a minted grant's issuer included.

use crate::grant_key::GrantKey;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_browser_peer::login::{self, Door};
use tilo_browser_peer::{HttpClient, HttpFuture, PeerHandle};
use tilo_user_identity::{AgentScope, DeviceCert};

/// The longest body read from the relay or the site.
const MAX_BODY: usize = 4 * 1024 * 1024;
/// How long entering a workspace may take.
const ENTERING: Duration = Duration::from_secs(30);

/// A workspace as the account lists it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Workspace {
    /// The workspace's id (a UUID): what every action names it by.
    pub id: String,
    pub name: String,
    /// The host serving it, named as hosts are (`eu1.tilo.sh`).
    pub host: String,
}

/// Why the relay did not answer what was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayError {
    /// Not one of the person's workspaces, or no such workspace: the same
    /// answer, so a guess learns nothing.
    NotFound,
    /// The relay refused, with its reason (one workspace more than the
    /// person's plan keeps, quota, policy).
    Refused(String),
    /// The relay or the site could not be reached.
    Unavailable(String),
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such workspace"),
            Self::Refused(reason) => write!(f, "the host refused: {reason}"),
            Self::Unavailable(reason) => write!(f, "unavailable: {reason}"),
        }
    }
}

impl From<tilo_browser_peer::CertifyError> for RelayError {
    fn from(error: tilo_browser_peer::CertifyError) -> Self {
        match error {
            tilo_browser_peer::CertifyError::Denied(reason) => Self::Refused(reason),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

/// HTTPS to one origin, as the relay's and the site's routes take it.
#[derive(Clone)]
pub struct Web {
    client: tilo_net::Client,
    origin: String,
}

impl Web {
    #[must_use]
    pub fn new(client: tilo_net::Client, origin: &str) -> Self {
        Self {
            client,
            origin: origin.trim_end_matches('/').to_owned(),
        }
    }

    async fn get(&self, path: &str) -> Result<(u16, Vec<u8>), String> {
        let response = self
            .client
            .get(format!("{}{path}", self.origin))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let body = response
            .bytes_limited(MAX_BODY)
            .await
            .map_err(|error| error.to_string())?;
        Ok((status, body.to_vec()))
    }
}

impl HttpClient for Web {
    fn post(&self, path: &str, body: Vec<u8>) -> HttpFuture<'_> {
        let url = format!("{}{path}", self.origin);
        Box::pin(async move {
            let response = self
                .client
                .post(url)
                .header("content-type", "application/octet-stream")
                .body(body)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response
                .bytes_limited(MAX_BODY)
                .await
                .map_err(|error| error.to_string())?;
            Ok((status, body.to_vec()))
        })
    }
}

/// The relay the person's workspaces live on, and the site whose door lets
/// people into its home workspace there.
pub struct Relay {
    host: Web,
    site: Web,
    /// The relay's name, as hosts are named (`eu1.tilo.sh`).
    name: String,
}

impl Relay {
    /// The relay at `origin`, behind the site at `site`.
    #[must_use]
    pub fn new(client: &tilo_net::Client, origin: &str, site: &str) -> Self {
        let name = origin
            .split_once("://")
            .map_or(origin, |(_, rest)| rest)
            .trim_end_matches('/')
            .to_owned();
        Self {
            host: Web::new(client.clone(), origin),
            site: Web::new(client.clone(), site),
            name,
        }
    }

    /// The relay's name, as the account lists hosts.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The site's door to its home workspace on this relay, when it names
    /// one.
    async fn door(&self) -> Option<(Web, String)> {
        let (status, body) = self.site.get("/entry.json").await.ok()?;
        if status != 200 {
            return None;
        }
        let entry: Value = serde_json::from_slice(&body).ok()?;
        let (origin, path) = login::site_door(&entry, &self.host.origin)?;
        Some((Web::new(self.host.client.clone(), &origin), path))
    }

    /// `key` on the relay: its session, not in any workspace yet.
    ///
    /// # Errors
    /// The relay was not reached or takes no device sessions.
    pub async fn connect(&self, key: &GrantKey) -> Result<Connection, RelayError> {
        let info = tilo_browser_peer::host_info(&self.host)
            .await
            .map_err(|error| RelayError::Unavailable(error.to_string()))?;
        let address = info.address.ok_or_else(|| {
            RelayError::Unavailable("the relay takes no device sessions".to_owned())
        })?;
        let endpoint = match address
            .relay_url
            .as_deref()
            .and_then(|url| url.parse::<iroh_base::RelayUrl>().ok())
        {
            Some(relay) => address.endpoint_addr.with_relay_url(relay),
            None => address.endpoint_addr,
        };
        let peer = PeerHandle::start(
            tilo_browser_peer::PeerConfig {
                host: tilo_browser_peer::Host::at(endpoint),
                cert: key.cert().clone(),
                host_cert: info.host_cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            Arc::new(key.key().clone()),
            Arc::new(tilo_browser_peer::MemoryPeerStore::default()),
        );
        Ok(Connection {
            peer,
            host_cert: info.host_cert,
            key: key.clone(),
            relay: self.host.clone(),
            host: self.name.clone(),
            lobby: tokio::sync::Mutex::new(None),
            joined: Mutex::default(),
        })
    }

    /// Bring `connection` into the site's home workspace through its door,
    /// once: where a grant reaching all the person's workspaces asks the
    /// relay its own operations.
    async fn lobby(&self, connection: &Connection) -> Result<u64, RelayError> {
        let mut lobby = connection.lobby.lock().await;
        if let Some(id) = *lobby {
            return Ok(id);
        }
        if connection.key.scope() != AgentScope::AllWorkspaces {
            return Err(RelayError::Refused(
                "a key for one workspace reaches no other".to_owned(),
            ));
        }
        let (client, path) = self.door().await.ok_or_else(|| {
            RelayError::Unavailable("the site names no door to its workspace".to_owned())
        })?;
        let door = Door {
            client: &client,
            path,
        };
        let workspace = door.workspace().ok_or_else(|| {
            RelayError::Unavailable("the site's door names no workspace".to_owned())
        })?;
        let label = connection.key.cert().device_label.clone();
        login::through_door(
            &self.host,
            &door,
            connection.key.cert(),
            connection.key.key(),
            &label,
            &label,
        )
        .await?;
        let id = connection.join(&workspace).await?;
        *lobby = Some(id);
        drop(lobby);
        Ok(id)
    }

    /// The person's own workspace, as `connection` (reaching all of them)
    /// asks for it with `arguments`, and the invitation for its person.
    async fn own(
        &self,
        connection: &Connection,
        arguments: Map<String, Value>,
    ) -> Result<(Workspace, Vec<u8>), RelayError> {
        let lobby = self.lobby(connection).await?;
        let (listing, invitation) =
            login::own_workspace(&connection.peer, lobby, &connection.host_cert, arguments).await?;
        Ok((connection.workspace(&listing), invitation))
    }

    /// The workspaces `connection`'s grant reaches here: the person's own.
    ///
    /// # Errors
    /// The relay was not reached or refused.
    pub async fn workspaces(&self, connection: &Connection) -> Result<Vec<Workspace>, RelayError> {
        let lobby = self.lobby(connection).await?;
        let listed = connection
            .host_operation(
                lobby,
                tilo_relay_session::workspaces::LIST_ROOMS,
                Map::new(),
            )
            .await?;
        let listed: Vec<tilo_relay_session::workspaces::WorkspaceListing> =
            serde_json::from_value(listed)
                .map_err(|error| RelayError::Unavailable(error.to_string()))?;
        Ok(listed
            .iter()
            .map(|listing| connection.workspace(listing))
            .collect())
    }

    /// Make the person's workspace called `name` as `connection`, and enter
    /// it. The relay keeps as many workspaces of each person's own as their
    /// plan does, sharing their storage: asking for one more is refused,
    /// typed.
    ///
    /// # Errors
    /// The relay was not reached or refused.
    pub async fn create(
        &self,
        connection: &Connection,
        name: &str,
    ) -> Result<Workspace, RelayError> {
        // A fresh id: the relay makes the workspace under that id, or
        // refuses one more than the person's plan keeps rather than
        // answering one they have.
        let fresh = uuid::Uuid::new_v4().to_string();
        let mut arguments = Map::new();
        arguments.insert("name".into(), json!(name));
        arguments.insert("room".into(), json!(fresh));
        let (workspace, invitation) = self.own(connection, arguments).await?;
        connection.redeem(&invitation).await?;
        connection.join(&workspace.id).await?;
        Ok(workspace)
    }

    /// Bring `connection` into `workspace`: its grant's person's own, which
    /// it reaches. A grant reaching all of them asks for the invitation
    /// itself; a minted one has `parent`, the grant that minted it, ask.
    ///
    /// # Errors
    /// Not a workspace the grant reaches, or the relay refused or was not
    /// reached.
    pub async fn enter(
        &self,
        connection: &Connection,
        parent: Option<&Connection>,
        workspace: &str,
    ) -> Result<u64, RelayError> {
        if !connection.key.scope().reaches(workspace) {
            return Err(RelayError::NotFound);
        }
        if let Some(id) = connection.entered(workspace) {
            return Ok(id);
        }
        let asking = match connection.key.scope() {
            AgentScope::AllWorkspaces => connection,
            AgentScope::Workspace(_) => parent.ok_or_else(|| {
                RelayError::Refused(
                    "only a key an account key minted enters its workspace here".to_owned(),
                )
            })?,
        };
        let mut arguments = Map::new();
        arguments.insert("room".into(), json!(workspace));
        let (own, invitation) = match self.own(asking, arguments).await {
            Ok(answered) => answered,
            Err(RelayError::Refused(_)) => return Err(RelayError::NotFound),
            Err(other) => return Err(other),
        };
        if own.id != workspace {
            return Err(RelayError::NotFound);
        }
        connection.redeem(&invitation).await?;
        connection.join(workspace).await
    }
}

/// A grant's session with the relay.
pub struct Connection {
    pub peer: PeerHandle,
    pub host_cert: DeviceCert,
    pub key: GrantKey,
    relay: Web,
    host: String,
    lobby: tokio::sync::Mutex<Option<u64>>,
    /// The workspaces it entered, by id.
    joined: Mutex<HashSet<String>>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.peer.close();
    }
}

impl Connection {
    fn workspace(&self, listing: &tilo_relay_session::workspaces::WorkspaceListing) -> Workspace {
        Workspace {
            id: listing.workspace.clone(),
            name: listing.name.clone(),
            host: self.host.clone(),
        }
    }

    fn entered(&self, workspace: &str) -> Option<u64> {
        let joined = self
            .joined
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        joined
            .contains(workspace)
            .then(|| tilo_relay_protocol::room_workspace_id(workspace).ok())
            .flatten()
    }

    async fn redeem(&self, invitation: &[u8]) -> Result<(), RelayError> {
        let label = self.key.cert().device_label.clone();
        login::redeem(
            &self.relay,
            invitation,
            self.key.cert(),
            self.key.key(),
            &label,
        )
        .await
        .map_err(RelayError::from)
    }

    /// Join `workspace`, which admitted this grant: its id on the session.
    async fn join(&self, workspace: &str) -> Result<u64, RelayError> {
        let id =
            tilo_relay_protocol::room_workspace_id(workspace).map_err(|_| RelayError::NotFound)?;
        self.peer.join(id).map_err(RelayError::Unavailable)?;
        let admitted = tokio::time::timeout(ENTERING, login::admitted(&self.peer, id))
            .await
            .unwrap_or(false);
        if !admitted {
            return Err(RelayError::Refused(
                "the workspace did not admit this key".to_owned(),
            ));
        }
        self.joined
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(workspace.to_owned());
        Ok(id)
    }

    /// The relay's own operation `action` with `arguments`, asked in the
    /// workspace `workspace_id` this session is in.
    async fn host_operation(
        &self,
        workspace_id: u64,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<Value, RelayError> {
        use tilo_actions::remote::{
            ActionHost as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
            RemoteActionTarget,
        };
        use tilo_relay_session::workspaces::{HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE};
        let host = tilo_browser_peer::session_actions::SessionActions::new(
            &self.peer,
            workspace_id,
            self.host_cert.device_public_key,
        );
        let receipt = host
            .invoke(
                &Caller::local_person("tilo front door"),
                InvokeRequest {
                    on_behalf_of: None,
                    invocation: tilo_actions::InvocationId::new(),
                    target: RemoteActionTarget {
                        provider: HOST_ACTIONS_PROVIDER.to_owned(),
                        address: tilo_actions::ActionAddress::new(HOST_ROOMS_RESOURCE, action),
                    },
                    argument: InvokeArgument::Json(arguments),
                    context: None,
                    confirmation: None,
                    revision: None,
                    wait_ms: Some(20_000),
                    appearance: None,
                },
            )
            .await
            .ok_or_else(|| RelayError::Unavailable("the relay did not answer".to_owned()))?
            .map_err(|error| RelayError::Refused(error.to_string()))?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Failed { error } => Err(RelayError::Refused(error.message)),
            ReceiptStatus::Rejected { error } => Err(RelayError::Refused(error.to_string())),
            ReceiptStatus::Cancelled { .. } | ReceiptStatus::Accepted => Err(
                RelayError::Unavailable("the relay did not finish".to_owned()),
            ),
        }
    }
}
