//! The person's workspaces on the relay, reached as a grant: the same way
//! into a workspace every device takes ([`Device::enter_workspace`]).
//!
//! The grant's own key is the device. It dials the relay's iroh endpoint
//! (which the relay's public origin names, `/peer/v1/host`) with the grant's
//! certificate, and asks the relay's own operations (its person's list, making
//! and deleting their workspaces, their account) on that session, in no
//! workspace. It enters one of the person's workspaces its scope reaches as
//! any device of theirs does: the relay invites it in on the session, it
//! redeems the invitation there, and joins.
//!
//! What it does there is decided by the relay, at every handshake and audit:
//! the grant's scope, its certificate's expiry, and its person's
//! revocations, a minted grant's issuer included.

use crate::grant_key::GrantKey;
use collab_types::WorkspaceId;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_relay_client::CertifyError;
use tilo_relay_client::https::{self, Origin};
use tilo_session_client::PeerConfig;
use tilo_session_client::device::{Device, HostRefusal};

/// How long entering a workspace may take.
const ENTERING: Duration = Duration::from_secs(30);

/// A workspace as the account lists it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Workspace {
    /// The workspace's id (a UUID): what every action names it by.
    pub id: String,
    pub name: String,
    /// The host serving it, named as hosts are: a relay's name
    /// (`eu1.tilo.sh`), or the person's device that hosts it. `None` while
    /// none of their devices said where it is hosted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The part the person takes in it: `owner` for a workspace of their own,
    /// else the role they hold there (`viewer`, `commenter`, `editor`,
    /// `moderator`). `None` when the answer did not say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Where its host is reached, as the person's list says.
    #[serde(skip)]
    pub address: Option<tilo_relay_session::workspaces::WorkspaceHostAddress>,
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
    /// The relay is busy with this person's changes: nothing ran, and the
    /// same call works once `retry_after_ms` passed.
    RateLimited { retry_after_ms: u64 },
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such workspace"),
            Self::Refused(reason) => write!(f, "the host refused: {reason}"),
            Self::Unavailable(reason) => write!(f, "unavailable: {reason}"),
            Self::RateLimited { retry_after_ms } => write!(
                f,
                "the host takes your changes more slowly; retry in {retry_after_ms} ms"
            ),
        }
    }
}

impl From<CertifyError> for RelayError {
    fn from(error: CertifyError) -> Self {
        match error {
            CertifyError::Denied(reason) => Self::Refused(reason),
            CertifyError::RateLimited { retry_after_ms } => Self::RateLimited { retry_after_ms },
            other => Self::Unavailable(other.to_string()),
        }
    }
}

impl From<HostRefusal> for RelayError {
    fn from(refusal: HostRefusal) -> Self {
        CertifyError::from(refusal).into()
    }
}

/// The relay the person's workspaces live on.
pub struct Relay {
    host: Origin,
    /// The relay's name, as hosts are named (`eu1.tilo.sh`).
    name: String,
}

impl Relay {
    /// The relay at `origin`.
    #[must_use]
    pub fn new(client: &tilo_net::Client, origin: &str) -> Self {
        let name = origin
            .split_once("://")
            .map_or(origin, |(_, rest)| rest)
            .trim_end_matches('/')
            .to_owned();
        Self {
            host: Origin::new(client, origin),
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
        let host = info
            .address
            .as_ref()
            .map(tilo_relay_client::Host::published)
            .ok_or_else(|| {
                RelayError::Unavailable("the relay takes no device sessions".to_owned())
            })?;
        let label = key.cert().device_label.clone();
        let device = Device::over(
            PeerConfig {
                host,
                cert: key.cert().clone(),
                host_cert: info.host_cert,
                revocations: tilo_user_identity::RevocationList::new(),
                entering: None,
            },
            Arc::new(key.key().clone()),
            &label,
        );
        let connection = Connection {
            device,
            key: key.clone(),
            host: self.name.clone(),
            entered: Mutex::default(),
            endpoint: tokio::sync::OnceCell::new(),
        };
        connection.device.connected().await?;
        Ok(connection)
    }

    /// The workspaces `connection`'s grant reaches: the person's list as the
    /// relay keeps it, every workspace of theirs wherever it is hosted (one
    /// of their desktops, a paired device, this relay), each naming its host.
    ///
    /// # Errors
    /// The relay was not reached or refused.
    pub async fn workspaces(&self, connection: &Connection) -> Result<Vec<Workspace>, RelayError> {
        let listed = connection
            .device
            .personal_list()
            .await
            .map_err(RelayError::from)?;
        Ok(listed
            .workspaces
            .into_iter()
            .map(|listed| Workspace {
                id: listed.workspace,
                name: listed.name,
                host: listed.host.as_ref().map(|host| host.name.clone()),
                role: listed.role,
                address: listed.host,
            })
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
        connection
            .device
            .ask_host(
                tilo_session_client::session::HOST_ITSELF,
                host_account::target(host_account::ACCOUNT),
                Map::new(),
                "tilo api",
            )
            .await
            .map_err(RelayError::from)
    }

    /// Make the person's workspace called `name` as `connection`, under `id`
    /// when the caller chose one. A person has any number of workspaces of
    /// their own, sharing what the host keeps for them.
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
        id: Option<uuid::Uuid>,
    ) -> Result<Workspace, RelayError> {
        // The relay makes the workspace under this id, or answers the one it
        // made under it before (a retried create), or refuses one more than
        // the person's plan keeps rather than answering one they have.
        let fresh = id.unwrap_or_else(uuid::Uuid::new_v4).to_string();
        let mut arguments = Map::new();
        arguments.insert("name".into(), json!(name));
        arguments.insert("room".into(), json!(fresh));
        let (listing, _invitation) = connection
            .device
            .own_workspace(arguments, "tilo api")
            .await?;
        Ok(connection.workspace(&listing))
    }

    /// Delete the person's own `workspace` as `connection` (reaching all
    /// their workspaces): final, with everything the relay keeps of it.
    ///
    /// # Errors
    /// Not one of the person's own workspaces ([`RelayError::NotFound`]),
    /// or the relay refused or was not reached.
    pub async fn delete(&self, connection: &Connection, workspace: &str) -> Result<(), RelayError> {
        if tilo_relay_protocol::WorkspaceId::parse(workspace).is_none() {
            return Err(RelayError::NotFound);
        }
        let mut arguments = Map::new();
        arguments.insert("room".into(), json!(workspace));
        connection
            .device
            .host_action(
                tilo_relay_session::workspaces::DELETE_ROOM,
                arguments,
                "tilo api",
            )
            .await
            .map_err(|refusal| match RelayError::from(refusal) {
                RelayError::Refused(reason)
                    if reason.contains(tilo_relay_session::workspaces::NOT_YOUR_OWN) =>
                {
                    RelayError::NotFound
                }
                other => other,
            })?;
        lock(&connection.entered).remove(workspace);
        Ok(())
    }

    /// Bring `connection` into `workspace`, one its grant reaches: its id on
    /// the session.
    ///
    /// # Errors
    /// Not a workspace the grant reaches, or the relay refused or was not
    /// reached.
    pub async fn enter(
        &self,
        connection: &Connection,
        workspace: &str,
    ) -> Result<WorkspaceId, RelayError> {
        if !connection.key.scope().reaches(workspace)
            || tilo_relay_protocol::WorkspaceId::parse(workspace).is_none()
        {
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
            .get_or_try_init(|| connection.enter(workspace))
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
}

/// A grant's session with the relay. The relay keeps one session per key,
/// so every run of the grant shares this one.
pub struct Connection {
    pub device: Device,
    pub key: GrantKey,
    host: String,
    /// The workspaces it entered or is entering, by id: each joined once,
    /// and left with the session.
    entered: Mutex<HashMap<String, Arc<tokio::sync::OnceCell<WorkspaceId>>>>,
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

impl Connection {
    /// The grant's endpoint, bound the first time a workspace on one of the
    /// person's devices is entered.
    ///
    /// # Errors
    /// The endpoint could not bind.
    pub async fn endpoint(&self) -> Result<iroh::Endpoint, RelayError> {
        self.endpoint
            .get_or_try_init(|| async {
                // It only dials: the relays and directory the setting names find the
                // person's device, and nothing about the grant is published.
                let relays = tilo_endpoint_reachability::relays();
                let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                    .secret_key(iroh::SecretKey::from_bytes(&self.key.key().to_bytes()))
                    .relay_mode(iroh::RelayMode::Custom(relays.relays));
                if relays.n0_dns {
                    builder =
                        builder.address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns());
                }
                builder
                    .bind()
                    .await
                    .map_err(|error| RelayError::Unavailable(error.to_string()))
            })
            .await
            .cloned()
    }

    /// The key as the shared way into a workspace takes it.
    #[must_use]
    pub fn entering_key(&self) -> tilo_runtime::enter::Key {
        tilo_runtime::enter::Key {
            key: self.key.key().clone(),
            cert: self.key.cert().clone(),
        }
    }

    fn workspace(&self, listing: &tilo_relay_session::workspaces::WorkspaceListing) -> Workspace {
        Workspace {
            id: listing.workspace.clone(),
            name: listing.name.clone(),
            host: Some(self.host.clone()),
            role: listing.role.clone(),
            address: None,
        }
    }

    /// Whether the session still serves: not closed, and not refused by
    /// the relay for good.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.device.is_live()
    }

    /// Enter `workspace`, as every device enters one, in at most [`ENTERING`]:
    /// one the person does not have is not found, as one never made.
    async fn enter(&self, workspace: &str) -> Result<WorkspaceId, RelayError> {
        let label = self.key.cert().device_label.clone();
        let Ok(entered) =
            tokio::time::timeout(ENTERING, self.device.enter_workspace(workspace, &label)).await
        else {
            return Err(RelayError::Unavailable(
                "the workspace did not admit this key in time".to_owned(),
            ));
        };
        entered.map_err(|refusal| match RelayError::from(refusal) {
            // Not theirs, or not one at all: the same answer, so a guess learns nothing.
            RelayError::Refused(_) => RelayError::NotFound,
            other => other,
        })
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
