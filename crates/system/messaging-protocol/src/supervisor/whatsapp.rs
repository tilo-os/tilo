//! Construct the SDK and its durability hook together, before any connection.
use std::sync::Arc;
use tilo_messaging::SendError;
use whatsapp_rust::{
    Client,
    http::{HttpClient, HttpRequest, HttpResponse},
    transport::{Transport, TransportEvent, TransportFactory},
};

/// The native hosting process supplies its authorized network implementations.
/// This wrapper never discovers an account, transport or ambient connection.
pub struct Network {
    pub http: Arc<dyn HttpClient>,
    pub transport: Arc<dyn TransportFactory>,
}
pub(super) mod groups;
mod linking;
#[cfg(target_os = "linux")]
mod native_network;
mod network;
pub use linking::WhatsappLinking;
struct Http(Arc<dyn HttpClient>);
#[whatsapp_rust::async_trait]
impl HttpClient for Http {
    async fn execute(&self, request: HttpRequest) -> whatsapp_rust::anyhow::Result<HttpResponse> {
        self.0.execute(request).await
    }
}
struct TransportProvider(Arc<dyn TransportFactory>);
#[whatsapp_rust::async_trait]
impl TransportFactory for TransportProvider {
    async fn create_transport(
        &self,
    ) -> whatsapp_rust::anyhow::Result<(
        Arc<dyn Transport>,
        whatsapp_rust::async_channel::Receiver<TransportEvent>,
    )> {
        self.0.create_transport().await
    }
}
/// Opaque pair: the supervisor cannot accidentally bind a hook which was not
/// installed in this SDK's builder. This does not certify a caller or grant.
pub struct WhatsappBootstrap {
    pub(super) source: Arc<Client>,
    pub(super) hook: Arc<crate::external_broker::WhatsappInboxHook>,
    pub(super) backend: crate::whatsapp_store::EncryptedBackend,
}
struct StateGrant(Arc<dyn super::AccountAuthority>);
impl crate::whatsapp_store::StateAuthority for StateGrant {
    fn allowed(&self) -> bool {
        self.0.check().is_ok()
    }
}
impl WhatsappBootstrap {
    /// Build with the original protected account key and grant before SDK I/O.
    /// No key, permission, account or network connection is created here.
    /// # Errors
    /// Refuses revoked authority, unsafe paths or unusable encrypted SDK state.
    pub async fn open_profile(
        root: &std::path::Path,
        owner: crate::journal::Owner,
        key: secret_vault::VaultKey,
        network: Network,
        authority: Arc<dyn super::AccountAuthority>,
    ) -> Result<Self, SendError> {
        if !root.is_absolute() {
            return Err(SendError::InvalidRequest);
        }
        authority.check()?;
        let backend = crate::whatsapp_store::EncryptedBackend::open_profile(
            root,
            owner,
            key,
            Arc::new(StateGrant(authority.clone())),
        )
        .map_err(|_| SendError::Unavailable)?;
        let bootstrap = Self::build(backend, network.guarded(authority.clone())).await?;
        authority.check()?;
        Ok(bootstrap)
    }
    /// Open existing OS-keychain custody on the native supervisor worker.
    /// No key is provisioned and no weaker store or ambient account is used.
    /// # Errors
    /// Refuses missing/locked custody, invalid paths and unusable SDK state.
    pub async fn open_system(
        root: &std::path::Path,
        owner: crate::journal::Owner,
        network: Network,
        authority: Arc<dyn super::AccountAuthority>,
    ) -> Result<Self, SendError> {
        if !root.is_absolute() {
            return Err(SendError::InvalidRequest);
        }
        let backend = crate::whatsapp_store::EncryptedBackend::open_system(
            root,
            owner,
            Arc::new(StateGrant(authority)),
        )
        .map_err(|_| SendError::Unavailable)?;
        Self::build(backend, network).await
    }
    /// Build with already owned encrypted custody and supplied native transports.
    /// No connection, login, grant or key is created. Linking is a separate
    /// explicit user operation on the native host's returned client.
    /// # Errors
    /// Refuses invalid owner/state and a backend without durable inbound support.
    pub async fn build(
        backend: crate::whatsapp_store::EncryptedBackend,
        network: Network,
    ) -> Result<Self, SendError> {
        let hook = Arc::new(crate::external_broker::WhatsappInboxHook::new(
            backend.owner().clone(),
        )?);
        let manager = Arc::new(
            whatsapp_rust::store::persistence_manager::PersistenceManager::new(Arc::new(
                backend.clone(),
            ))
            .await
            .map_err(|_| SendError::Unavailable)?,
        );
        let source = Client::builder()
            .with_runtime(whatsapp_rust::TokioRuntime)
            .with_persistence_manager(manager)
            .with_http_client(Http(network.http))
            .with_transport_factory(TransportProvider(network.transport))
            .with_inbound_durability_hook_arc(hook.clone())
            .build()
            .await
            .map_err(|_| SendError::Unavailable)?
            .into_client();
        Ok(Self {
            source,
            hook,
            backend,
        })
    }
    pub fn client(&self) -> Arc<Client> {
        self.source.clone()
    }
}

struct Lifecycle {
    supervisor: std::sync::Weak<super::AccountSupervisor>,
    source: std::sync::Weak<Client>,
}
impl whatsapp_rust::wacore::types::events::EventHandler for Lifecycle {
    fn interest(&self) -> whatsapp_rust::wacore::types::events::EventInterest {
        use whatsapp_rust::wacore::types::events::{EventInterest, EventKind};
        EventInterest::of(&[
            EventKind::Connected,
            EventKind::Disconnected,
            EventKind::LoggedOut,
            EventKind::PairSuccess,
            EventKind::PairError,
            EventKind::PairingQrCode,
            EventKind::PairingCode,
        ])
    }
    fn handle_event(&self, event: Arc<whatsapp_rust::wacore::types::events::Event>) {
        if let (Some(supervisor), Some(source)) = (self.supervisor.upgrade(), self.source.upgrade())
        {
            supervisor.update_whatsapp(&source, &event);
        }
    }
}
pub(super) fn subscribe_lifecycle(
    supervisor: &Arc<super::AccountSupervisor>,
    source: &Arc<Client>,
) -> whatsapp_rust::wacore::types::events::Subscription {
    source.subscribe_handler(Arc::new(Lifecycle {
        supervisor: Arc::downgrade(supervisor),
        source: Arc::downgrade(source),
    }))
}
