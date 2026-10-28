//! `org.freedesktop.NetworkManager.SecretAgent` on the system bus,
//! registered with `NetworkManager`'s agent manager for this session.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::store::SecretStore;
use super::{
    AgentError, ConnectionDict, SecretPrompter, SecretRequest, SheetAnswer, agent_owned_secrets,
    connection_uuid, request_secrets,
};
use tilo_lock::MutexRecover as _;

/// What a sheet answers: what the person typed, or `None` when they
/// dismissed it or it could not be shown.
pub type SheetReply = Pin<Box<dyn Future<Output = Option<SheetAnswer>> + Send>>;

/// The sheets the agent asks in, drawn by its host — in the shell, the
/// compositor's own dialog queue, which no application can imitate.
pub trait Sheets: Send + Sync {
    /// Show `request` in the sheet named `handle`, resolving its
    /// [`crate::Message`]s in the host's catalogue.
    fn ask(&self, handle: &str, request: &SecretRequest) -> SheetReply;
    /// Take the sheet named `handle` down, if it is still up.
    fn cancel(&self, handle: &str);
    /// `request` needed secrets the agent could not ask for: `NetworkManager`
    /// connected on its own and nothing usable was remembered. The host
    /// tells the person, who joins the network themselves to be asked.
    fn cannot_ask(&self, _request: &SecretRequest) {}
}

/// Where `NetworkManager` looks for every agent.
pub const AGENT_PATH: &str = "/org/freedesktop/NetworkManager/SecretAgent";
/// This agent's identifier: stable, so `NetworkManager` treats a restarted
/// shell as the same agent.
pub const AGENT_IDENTIFIER: &str = "so.tilo.shell";
pub const NM_BUS_NAME: &str = "org.freedesktop.NetworkManager";
pub const AGENT_MANAGER_PATH: &str = "/org/freedesktop/NetworkManager/AgentManager";
pub const AGENT_MANAGER_INTERFACE: &str = "org.freedesktop.NetworkManager.AgentManager";
/// `NM_SECRET_AGENT_CAPABILITY_VPN_HINTS`: VPN secret keys arrive as hints,
/// so the sheet can ask for them without a plugin's own dialog.
pub const CAPABILITY_VPN_HINTS: u32 = 0x1;

/// The sheet the host draws for one request.
pub struct SheetPrompter {
    sheets: Arc<dyn Sheets>,
    handle: String,
}

impl SheetPrompter {
    /// `number` tells apart requests for the same setting, so a request
    /// `NetworkManager` replaced never takes down its successor's sheet.
    pub fn new(
        sheets: Arc<dyn Sheets>,
        number: u64,
        connection_path: &str,
        setting_name: &str,
    ) -> Self {
        Self {
            sheets,
            handle: format!("nm-secrets:{number}:{connection_path}/{setting_name}"),
        }
    }

    pub fn handle(&self) -> &str {
        &self.handle
    }
}

impl SecretPrompter for SheetPrompter {
    async fn ask(&self, request: &SecretRequest) -> Option<SheetAnswer> {
        self.sheets.ask(&self.handle, request).await
    }

    fn cannot_ask(&self, request: &SecretRequest) {
        self.sheets.cannot_ask(request);
    }
}

/// Whether a call came from `NetworkManager`. On a bus that is the owner of
/// `NetworkManager`'s name; a peer-to-peer connection has only the one peer.
async fn from_network_manager(connection: &zbus::Connection, header: &Header<'_>) -> bool {
    if connection.unique_name().is_none() {
        return true;
    }
    let Some(sender) = header.sender() else {
        return false;
    };
    let Ok(bus) = zbus::fdo::DBusProxy::new(connection).await else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from(NM_BUS_NAME) else {
        return false;
    };
    bus.get_name_owner(name)
        .await
        .is_ok_and(|owner| owner.as_str() == sender.as_str())
}

/// Requests in flight by connection path and setting, each with the
/// number it was started under.
type LiveRequests = Arc<Mutex<HashMap<(String, String), (u64, CancellationToken)>>>;

pub struct SecretAgent {
    sheets: Arc<dyn Sheets>,
    store: Arc<dyn SecretStore>,
    live: LiveRequests,
    started: std::sync::atomic::AtomicU64,
}

impl SecretAgent {
    pub fn new(sheets: Arc<dyn Sheets>, store: Arc<dyn SecretStore>) -> Self {
        Self {
            sheets,
            store,
            live: Arc::default(),
            started: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

fn outcome_label(outcome: &Result<super::SettingSecrets, AgentError>) -> &'static str {
    match outcome {
        Ok(_) => "answered",
        Err(AgentError::UserCanceled(_)) => "dismissed",
        Err(AgentError::AgentCanceled(_)) => "withdrawn",
        Err(AgentError::NoSecrets(_)) => "no secrets",
        Err(_) => "failed",
    }
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.SecretAgent")]
impl SecretAgent {
    /// `NetworkManager` needs secrets for `setting_name` of a connection.
    /// Answered from what is remembered, from the sheet, or with an error.
    async fn get_secrets(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] bus: &zbus::Connection,
        connection: ConnectionDict,
        connection_path: OwnedObjectPath,
        setting_name: String,
        hints: Vec<String>,
        flags: u32,
    ) -> Result<HashMap<String, HashMap<String, OwnedValue>>, AgentError> {
        if !from_network_manager(bus, &header).await {
            return Err(AgentError::PermissionDenied(
                "only NetworkManager may ask for secrets".into(),
            ));
        }
        let request = SecretRequest::parse(
            &connection,
            connection_path.as_str(),
            &setting_name,
            &hints,
            flags,
        )?;
        let key = (connection_path.to_string(), setting_name.clone());
        let number = self
            .started
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let token = CancellationToken::new();
        let stale = self
            .live
            .lock_recover()
            .insert(key.clone(), (number, token.clone()));
        if let Some((_, stale)) = stale {
            stale.cancel();
        }
        let prompter = SheetPrompter::new(
            Arc::clone(&self.sheets),
            number,
            connection_path.as_str(),
            &setting_name,
        );
        let outcome = tokio::select! {
            outcome = request_secrets(&request, &self.store, &prompter) => outcome,
            () = token.cancelled() => Err(AgentError::AgentCanceled("withdrawn by NetworkManager".into())),
        };
        self.sheets.cancel(prompter.handle());
        {
            let mut live = self.live.lock_recover();
            if live
                .get(&key)
                .is_some_and(|(current, _)| *current == number)
            {
                live.remove(&key);
            }
        }
        tracing::info!(
            target: "shell::network_secrets",
            connection = %request.connection_id,
            setting = %setting_name,
            flags,
            outcome = outcome_label(&outcome),
            "network secrets request finished"
        );
        outcome.map(|secrets| request.response(&secrets))
    }

    async fn cancel_get_secrets(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] bus: &zbus::Connection,
        connection_path: OwnedObjectPath,
        setting_name: String,
    ) -> Result<(), AgentError> {
        if !from_network_manager(bus, &header).await {
            return Err(AgentError::PermissionDenied(
                "only NetworkManager may cancel".into(),
            ));
        }
        let token = self
            .live
            .lock_recover()
            .remove(&(connection_path.to_string(), setting_name));
        if let Some((_, token)) = token {
            token.cancel();
        }
        Ok(())
    }

    /// Keep the agent-owned secrets of a connection `NetworkManager` saved.
    async fn save_secrets(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] bus: &zbus::Connection,
        connection: ConnectionDict,
        connection_path: OwnedObjectPath,
    ) -> Result<(), AgentError> {
        if !from_network_manager(bus, &header).await {
            return Err(AgentError::PermissionDenied(
                "only NetworkManager may save secrets".into(),
            ));
        }
        let _ = connection_path;
        let uuid = connection_uuid(&connection)
            .ok_or_else(|| AgentError::InvalidConnection("the connection has no uuid".into()))?;
        let owned = agent_owned_secrets(&connection);
        let store = Arc::clone(&self.store);
        let saved = tokio::task::spawn_blocking(move || {
            owned.into_iter().try_for_each(|(setting, secrets)| {
                let key = super::store::StoreKey {
                    uuid: uuid.clone(),
                    setting,
                    roaming_wifi: None,
                };
                store.keep(&key, &secrets)
            })
        })
        .await
        .map_err(|_| AgentError::InvalidConnection("the secret store stopped".into()))?;
        saved.map_err(|error| {
            tracing::warn!(target: "shell::network_secrets", %error, "could not keep saved secrets");
            AgentError::InvalidConnection(error)
        })
    }

    /// Forget everything remembered for a deleted connection.
    async fn delete_secrets(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] bus: &zbus::Connection,
        connection: ConnectionDict,
        connection_path: OwnedObjectPath,
    ) -> Result<(), AgentError> {
        if !from_network_manager(bus, &header).await {
            return Err(AgentError::PermissionDenied(
                "only NetworkManager may delete secrets".into(),
            ));
        }
        let _ = connection_path;
        let uuid = connection_uuid(&connection)
            .ok_or_else(|| AgentError::InvalidConnection("the connection has no uuid".into()))?;
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || store.forget_connection(&uuid))
            .await
            .map_err(|_| AgentError::InvalidConnection("the secret store stopped".into()))?
            .map_err(AgentError::InvalidConnection)
    }
}

/// Keeps the agent registered while held.
pub struct AgentHandle {
    connection: zbus::Connection,
    reregister: Option<tokio::task::JoinHandle<()>>,
}

impl AgentHandle {
    pub fn connection(&self) -> &zbus::Connection {
        &self.connection
    }
}

impl Drop for AgentHandle {
    fn drop(&mut self) {
        if let Some(task) = self.reregister.take() {
            task.abort();
        }
    }
}

/// Tell `NetworkManager` this agent answers for the session.
///
/// # Errors
///
/// When `NetworkManager`'s agent manager refuses or cannot be reached.
pub async fn register(connection: &zbus::Connection) -> zbus::Result<()> {
    let manager = zbus::Proxy::new(
        connection,
        NM_BUS_NAME,
        AGENT_MANAGER_PATH,
        AGENT_MANAGER_INTERFACE,
    )
    .await?;
    manager
        .call::<_, _, ()>(
            "RegisterWithCapabilities",
            &(AGENT_IDENTIFIER, CAPABILITY_VPN_HINTS),
        )
        .await
}

/// `NetworkManager` forgets its agents when it restarts; register again
/// whenever its name gets a new owner.
async fn reregister_on_restart(connection: zbus::Connection) {
    use futures_util::StreamExt as _;

    let Ok(bus) = zbus::fdo::DBusProxy::new(&connection).await else {
        return;
    };
    let Ok(mut changes) = bus
        .receive_name_owner_changed_with_args(&[(0, NM_BUS_NAME)])
        .await
    else {
        return;
    };
    while let Some(change) = changes.next().await {
        let Ok(args) = change.args() else {
            continue;
        };
        if args.new_owner().is_none() {
            continue;
        }
        match register(&connection).await {
            Ok(()) => tracing::info!(
                target: "shell::network_secrets",
                "registered again with a restarted NetworkManager"
            ),
            Err(error) => tracing::warn!(
                target: "shell::network_secrets",
                %error,
                "could not register with a restarted NetworkManager"
            ),
        }
    }
}

/// Serve the agent on `connection` and register it with `NetworkManager`.
///
/// # Errors
///
/// When the object cannot be served or `NetworkManager` refuses the agent.
pub async fn serve_on(
    connection: zbus::Connection,
    sheets: Arc<dyn Sheets>,
    store: Arc<dyn SecretStore>,
) -> zbus::Result<AgentHandle> {
    connection
        .object_server()
        .at(AGENT_PATH, SecretAgent::new(sheets, store))
        .await?;
    register(&connection).await?;
    let reregister = connection
        .unique_name()
        .is_some()
        .then(|| tokio::spawn(reregister_on_restart(connection.clone())));
    Ok(AgentHandle {
        connection,
        reregister,
    })
}

/// Serve the agent on the system bus for this session.
///
/// # Errors
///
/// When the system bus or `NetworkManager` cannot be reached, or no vault
/// location exists for this session.
pub async fn serve(
    sheets: Arc<dyn Sheets>,
    roaming: super::store::RoamingWifiSlot,
) -> zbus::Result<AgentHandle> {
    // Choosing the vault's custody probes the secret store over a blocking
    // D-Bus call, which must not run on a runtime thread.
    let store = tokio::task::spawn_blocking(move || super::store::VaultStore::for_session(roaming))
        .await
        .map_err(|error| zbus::Error::Failure(format!("network secrets vault: {error}")))?
        .ok_or_else(|| {
            zbus::Error::Failure("no configuration directory for the network secrets vault".into())
        })?;
    let connection = zbus::Connection::system().await?;
    serve_on(connection, sheets, Arc::new(store)).await
}
