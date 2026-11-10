//! The person's workspaces on the relay, reached as a grant: the door's one
//! way in, the same a `tilo login` computer takes (`tilo_relay_client::login`).
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
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_relay_client::CertifyError;
use tilo_relay_client::https::{self, Origin};
use tilo_relay_client::login::{Door, door_at, through_door};
use tilo_session_host::PeerConfig;
use tilo_session_host::device::Device;
use tilo_user_identity::AgentScope;

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

impl From<CertifyError> for RelayError {
    fn from(error: CertifyError) -> Self {
        match error {
            CertifyError::Denied(reason) => Self::Refused(reason),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

/// The relay the person's workspaces live on, and the site whose door lets
/// people into its home workspace there.
pub struct Relay {
    host: Origin,
    site: Origin,
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
            host: Origin::new(client, origin),
            site: Origin::new(client, site),
            name,
        }
    }

    /// The relay's name, as the account lists hosts.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `key` on the relay: its session, not in any workspace yet.
    ///
    /// # Errors
    /// The relay was not reached or takes no device sessions.
    pub async fn connect(&self, key: &GrantKey) -> Result<Connection, RelayError> {
        let info = https::host_info(&self.host)
            .await
            .map_err(|error| RelayError::Unavailable(error.to_string()))?;
        let endpoint = tilo_relay_client::https::host_endpoint(&info).ok_or_else(|| {
            RelayError::Unavailable("the relay takes no device sessions".to_owned())
        })?;
        let device = Device::over(
            PeerConfig {
                host: tilo_relay_client::Host::at(endpoint),
                cert: key.cert().clone(),
                host_cert: info.host_cert,
                revocations: tilo_user_identity::RevocationList::new(),
            },
            Arc::new(key.key().clone()),
            &key.cert().device_label,
        );
        Ok(Connection {
            device,
            key: key.clone(),
            relay: self.host.clone(),
            host: self.name.clone(),
            lobby: tokio::sync::Mutex::new(None),
            entered: Mutex::default(),
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
        let (client, path) = door_at(&self.site, self.host.name()).await.ok_or_else(|| {
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
        through_door(
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
        let (listing, invitation) = connection
            .device
            .own_workspace(lobby, arguments, "tilo front door")
            .await?;
        Ok((connection.workspace(&listing), invitation))
    }

    /// The workspaces `connection`'s grant reaches here: the person's own.
    ///
    /// # Errors
    /// The relay was not reached or refused.
    pub async fn workspaces(&self, connection: &Connection) -> Result<Vec<Workspace>, RelayError> {
        let lobby = self.lobby(connection).await?;
        let listed = connection
            .device
            .host_action(
                lobby,
                tilo_relay_session::workspaces::LIST_ROOMS,
                Map::new(),
                "tilo front door",
            )
            .await
            .map_err(RelayError::from)?;
        let listed: Vec<tilo_relay_session::workspaces::WorkspaceListing> =
            serde_json::from_value(listed)
                .map_err(|error| RelayError::Unavailable(error.to_string()))?;
        Ok(listed
            .iter()
            .map(|listing| connection.workspace(listing))
            .collect())
    }

    /// The person's account here, as `connection` (reaching all their
    /// workspaces) reads it: their plan, and per workspace what it keeps and
    /// the VM time it ran.
    ///
    /// # Errors
    /// The relay was not reached or refused.
    pub async fn account(&self, connection: &Connection) -> Result<Value, RelayError> {
        use tilo_actions::remote::host_account;
        let lobby = self.lobby(connection).await?;
        connection
            .device
            .ask_host(
                lobby,
                host_account::target(host_account::ACCOUNT),
                Map::new(),
                "tilo front door",
            )
            .await
            .map_err(RelayError::from)
    }

    /// Make the person's workspace called `name` as `connection`. A person
    /// has any number of workspaces of their own, sharing what the host
    /// keeps for them.
    ///
    /// The grant enters it only once a run acts there ([`Relay::enter`]): a
    /// backend that makes a workspace per customer keeps hundreds, and its
    /// session with the relay subscribes only to those it works in.
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
        let (workspace, _invitation) = self.own(connection, arguments).await?;
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
        // Every run of the grant shares its session, so runs entering the
        // same workspace at once enter it once, and a failed entry is tried
        // again by the next.
        let entering = Arc::clone(
            lock(&connection.entered)
                .entry(workspace.to_owned())
                .or_default(),
        );
        let entered = entering
            .get_or_try_init(|| self.admit(connection, parent, workspace))
            .await
            .copied();
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

    /// Ask for `connection`'s invitation into `workspace`, redeem it and
    /// join.
    async fn admit(
        &self,
        connection: &Connection,
        parent: Option<&Connection>,
        workspace: &str,
    ) -> Result<u64, RelayError> {
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

/// A grant's session with the relay. The relay keeps one session per key,
/// so every run of the grant shares this one.
pub struct Connection {
    pub device: Device,
    pub key: GrantKey,
    relay: Origin,
    host: String,
    lobby: tokio::sync::Mutex<Option<u64>>,
    /// The workspaces it entered or is entering, by id: each joined once,
    /// and left with the session.
    entered: Mutex<HashMap<String, Arc<tokio::sync::OnceCell<u64>>>>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.device.close();
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

    /// Whether the session still serves: not closed, and not refused by
    /// the relay for good.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.device.is_live()
    }

    async fn redeem(&self, invitation: &[u8]) -> Result<(), RelayError> {
        let label = self.key.cert().device_label.clone();
        self.device
            .redeem(&self.relay, invitation, &label)
            .await
            .map_err(RelayError::from)
    }

    /// Join `workspace`, which admitted this grant: its id on the session.
    async fn join(&self, workspace: &str) -> Result<u64, RelayError> {
        if tilo_relay_protocol::room_workspace_id(workspace).is_err() {
            return Err(RelayError::NotFound);
        }
        tokio::time::timeout(ENTERING, self.device.join(workspace))
            .await
            .unwrap_or_else(|_| {
                Err(CertifyError::Denied(
                    "the workspace did not admit this key".to_owned(),
                ))
            })
            .map_err(RelayError::from)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
