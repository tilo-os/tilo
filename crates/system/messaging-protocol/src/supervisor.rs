//! Native lifecycle gate for one existing certified account assembly.
//! It owns no credentials and creates no grants. The caller supplies an already
//! authenticated adapter and retains current account/credential authority.
use std::sync::{Arc, Mutex};
use tilo_contact_directory::routing::Route;
use tilo_messaging::{
    AccountState, HistoryFuture, HistoryHost, MessagingClient, MessagingHost, SendError,
    SendFuture, SendRequest, ServiceAccount,
};

struct Runtime {
    state: AccountState,
    client: Option<MessagingClient>,
    observer: Option<Box<dyn std::any::Any + Send + Sync>>,
    receiver: Option<Receiver>,
    #[cfg(feature = "whatsapp-sdk")]
    receipt_task: Option<ReceiptTask>,
    #[cfg(feature = "whatsapp-sdk")]
    whatsapp_source: Option<Arc<whatsapp_rust::Client>>,
    #[cfg(feature = "whatsapp-sdk")]
    whatsapp_backend: Option<crate::whatsapp_store::EncryptedBackend>,
    #[cfg(feature = "whatsapp-sdk")]
    whatsapp_broker: Option<Arc<crate::external_broker::ExternalBroker>>,
    #[cfg(feature = "whatsapp-sdk")]
    lifecycle: Option<whatsapp_rust::wacore::types::events::Subscription>,
}
enum Receiver {
    Signal(crate::signal_broker::ReceiveGuard),
    #[cfg(all(target_os = "linux", feature = "sms-dbus"))]
    Sms(crate::external_broker::SmsReceiveGuard),
}
impl Receiver {
    fn stopped(&self) -> bool {
        match self {
            Self::Signal(guard) => guard.stopped(),
            #[cfg(all(target_os = "linux", feature = "sms-dbus"))]
            Self::Sms(guard) => guard.stopped(),
        }
    }
}
/// Existing process-owned admission/credential authority; never wire metadata.
pub trait AccountAuthority: Send + Sync {
    /// # Errors
    /// Refuses retired personal admission or a revoked native credential grant.
    fn check(&self) -> Result<(), SendError>;
}

/// Account-owned publication; it may be lent while linking/locked/offline so
/// the UI can explain availability, but those states cannot send/read history.
pub struct AccountSupervisor {
    owner: crate::journal::Owner,
    account: ServiceAccount,
    runtime: Mutex<Runtime>,
    observers: Arc<crate::signal_broker::observers::Observers>,
    authority: Option<Arc<dyn AccountAuthority>>,
    setup: Mutex<Option<tilo_messaging::SetupClient>>,
}
impl AccountSupervisor {
    /// The hosting caller supplies its certified persona and exact device.
    /// These labels create no authority: only an owner-matched broker can
    /// publish ready, and that broker still checks the current grant per I/O.
    /// # Errors
    /// Refuses malformed native owner identity.
    pub fn new(owner: &crate::journal::Owner) -> Result<Arc<Self>, SendError> {
        Self::new_with_authority(owner, None)
    }
    /// Retain the host's current admission/credential check for lifecycle and
    /// every operation. A passing check alone never publishes a ready adapter.
    /// # Errors
    /// Refuses malformed native owner identity.
    pub fn new_with_authority(
        owner: &crate::journal::Owner,
        authority: Option<Arc<dyn AccountAuthority>>,
    ) -> Result<Arc<Self>, SendError> {
        if !matches!(
            owner.service,
            crate::Service::Tilo
                | crate::Service::Whatsapp
                | crate::Service::Signal
                | crate::Service::Sms
        ) || !owner.valid_principal()
            || !crate::identifier(&owner.device)
            || !crate::identifier(&owner.account)
            || owner.linking_generation == 0
        {
            return Err(SendError::InvalidRequest);
        }
        let service = crate::signal_broker::history::service_name(owner.service);
        Ok(Arc::new(Self {
            owner: owner.clone(),
            account: ServiceAccount::new(service, &owner.account),
            runtime: Mutex::new(Runtime {
                state: AccountState::Unavailable,
                client: None,
                observer: None,
                receiver: None,
                #[cfg(feature = "whatsapp-sdk")]
                receipt_task: None,
                #[cfg(feature = "whatsapp-sdk")]
                whatsapp_source: None,
                #[cfg(feature = "whatsapp-sdk")]
                whatsapp_backend: None,
                #[cfg(feature = "whatsapp-sdk")]
                whatsapp_broker: None,
                #[cfg(feature = "whatsapp-sdk")]
                lifecycle: None,
            }),
            observers: Arc::default(),
            authority,
            setup: Mutex::new(None),
        }))
    }
    /// Install the native host's private linking capability for this exact
    /// frozen owner. This neither starts linking nor publishes Ready.
    /// # Errors
    /// Refuses another person/account/generation or duplicate setup ownership.
    pub fn install_setup(&self, setup: tilo_messaging::SetupClient) -> Result<(), SendError> {
        if setup.principal() != self.owner.principal
            || setup.account() != &self.account
            || setup.generation() != self.owner.linking_generation
        {
            return Err(SendError::Revoked);
        }
        let mut current = self.setup.lock().map_err(|_| SendError::Unavailable)?;
        if current.is_some() {
            return Err(SendError::AlreadyReserved);
        }
        *current = Some(setup);
        drop(current);
        self.observers.publish();
        Ok(())
    }
    pub fn owner(&self) -> &crate::journal::Owner {
        &self.owner
    }
    pub fn client(self: &Arc<Self>) -> MessagingClient {
        MessagingClient::new(self.owner.principal.clone(), self.clone())
    }
    /// Publish a recoverable non-ready state without replacing any adapter.
    /// Ready can only be published by verified installation below.
    /// # Errors
    /// Refuses ready-by-label and poisoned supervisor state.
    pub fn unavailable(&self, state: AccountState) -> Result<(), SendError> {
        if state == AccountState::Ready {
            return Err(SendError::InvalidRequest);
        }
        self.runtime
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .state = state;
        self.observers.publish();
        Ok(())
    }
    /// Publish only the owner-matched native private broker.
    /// # Errors
    /// Refuses foreign owner custody or duplicate installation.
    pub fn install_native(
        &self,
        broker: &crate::native_broker::NativeBroker,
    ) -> Result<(), SendError> {
        self.install(broker.owner(), broker.client())
    }
    /// Install an existing account-owned broker; no connection/login is started.
    /// # Errors
    /// Refuses another persona/device/service/account or duplicate installation.
    pub fn install_external(
        &self,
        broker: &crate::external_broker::ExternalBroker,
    ) -> Result<(), SendError> {
        if broker.owner().service != crate::Service::Sms {
            return Err(SendError::Unavailable);
        }
        self.install(broker.owner(), broker.client())
    }
    /// # Errors
    /// Refuses another persona/device/service/account or duplicate installation.
    pub fn install_signal(
        &self,
        broker: &crate::signal_broker::SignalBroker,
    ) -> Result<(), SendError> {
        self.install(&broker.owner()?, broker.client())
    }
    /// Install the verified independent Signal receive stream and sending as
    /// one lifecycle publication. Failed setup never publishes ready; teardown
    /// interrupts and joins only this account's receive worker.
    /// # Errors
    /// Refuses wrong owner, duplicate service or failed native receiver setup.
    pub fn install_signal_receiving(
        &self,
        broker: &crate::signal_broker::SignalBroker,
        receiver: crate::signal_runtime::SignalReceiver,
    ) -> Result<(), SendError> {
        if broker.owner()? != self.owner {
            return Err(SendError::Revoked);
        }
        if self
            .runtime
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .client
            .is_some()
        {
            return Err(SendError::AlreadyReserved);
        }
        let guard = broker.start_receiver(receiver)?;
        self.install_with_state(
            &self.owner,
            broker.client(),
            AccountState::Ready,
            Some(Receiver::Signal(guard)),
        )
    }
    /// Subscribe to the exact pinned modem before initial inbox enumeration,
    /// and retain its independent receive worker before publishing Ready.
    /// # Errors
    /// Refuses another owner/service, duplicate installation or failed native
    /// custody/subscription/durable inbox setup. Creates no credential grant.
    #[cfg(all(target_os = "linux", feature = "sms-dbus"))]
    pub async fn install_sms_receiving(
        &self,
        broker: &crate::external_broker::ExternalBroker,
    ) -> Result<(), SendError> {
        if broker.owner() != &self.owner || self.owner.service != crate::Service::Sms {
            return Err(SendError::Revoked);
        }
        if self
            .runtime
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .client
            .is_some()
        {
            return Err(SendError::AlreadyReserved);
        }
        let guard = broker.start_sms_receiver().await?;
        self.install_with_state(
            &self.owner,
            broker.client(),
            AccountState::Ready,
            Some(Receiver::Sms(guard)),
        )
    }
    /// Bind durable inbox admission and install recipient receipts before
    /// publishing the already linked `WhatsApp` adapter as ready. The SDK must
    /// have been built with this hook before any connection. The host supplies
    /// current caller/grant custody; this never connects or logs in.
    /// # Errors
    /// Refuses a mismatched owner/source/hook, missing worker runtime or duplicate.
    #[cfg(feature = "whatsapp-sdk")]
    pub async fn install_whatsapp(
        self: &Arc<Self>,
        broker: Arc<crate::external_broker::ExternalBroker>,
        bootstrap: whatsapp::WhatsappBootstrap,
    ) -> Result<(), SendError> {
        if broker.owner() != &self.owner || self.owner.service != crate::Service::Whatsapp {
            return Err(SendError::Revoked);
        }
        let (source, hook, backend) = (bootstrap.source, bootstrap.hook, bootstrap.backend);
        let handle = tokio::runtime::Handle::try_current().map_err(|_| SendError::Unavailable)?;
        if self
            .runtime
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .client
            .is_some()
        {
            return Err(SendError::AlreadyReserved);
        }
        hook.bind(&broker, &source).await?;
        hook.bind_groups(self)?;
        let mut subscription =
            crate::external_broker::WhatsappReceiptSubscription::attach(&broker, source.clone())
                .await?;
        let state = if !source.is_logged_in() {
            AccountState::NeedsRelink
        } else if !source.is_connected() {
            AccountState::Offline
        } else {
            AccountState::Ready
        };
        self.install_with_state(
            broker.owner(),
            broker.client(),
            AccountState::Unavailable,
            None,
        )?;
        let lifecycle = whatsapp::subscribe_lifecycle(self, &source);
        let (start, started) = tokio::sync::oneshot::channel();
        let supervisor = Arc::downgrade(self);
        let group_broker = broker.clone();
        let task = handle.spawn(async move {
            // Keep this precise supervisor binding alive until teardown. Client
            // clones alone must never prolong the durability hook's admission.
            let _broker = broker;
            if started.await.is_err() {
                return;
            }
            loop {
                if subscription.next().await.is_err() {
                    if let Some(supervisor) = supervisor.upgrade() {
                        let _ = supervisor.unavailable(AccountState::Failed);
                    }
                    break;
                }
            }
        });
        let mut runtime = self.runtime.lock().map_err(|_| SendError::Unavailable)?;
        runtime.whatsapp_source = Some(source);
        runtime.whatsapp_backend = Some(backend);
        runtime.whatsapp_broker = Some(group_broker);
        runtime.lifecycle = Some(lifecycle);
        runtime.receipt_task = Some(ReceiptTask(task));
        runtime.state = state;
        drop(runtime);
        let _ = start.send(());
        self.observers.publish();
        Ok(())
    }
    #[cfg(feature = "whatsapp-sdk")]
    fn update_whatsapp(
        &self,
        source: &Arc<whatsapp_rust::Client>,
        event: &whatsapp_rust::wacore::types::events::Event,
    ) {
        use whatsapp_rust::wacore::types::events::Event;
        let Ok(mut runtime) = self.runtime.lock() else {
            return;
        };
        if runtime.state == AccountState::Failed
            || !runtime
                .whatsapp_source
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, source))
        {
            return;
        }
        runtime.state = match event {
            Event::PairingQrCode(_) | Event::PairingCode(_) => AccountState::Linking,
            Event::PairError(_) => AccountState::Failed,
            Event::LoggedOut(_) => AccountState::NeedsRelink,
            _ if !source.is_logged_in() => AccountState::NeedsRelink,
            _ if !source.is_connected() => AccountState::Offline,
            _ => AccountState::Ready,
        };
        drop(runtime);
        self.observers.publish();
    }
    fn install(
        &self,
        owner: &crate::journal::Owner,
        client: MessagingClient,
    ) -> Result<(), SendError> {
        self.install_with_state(owner, client, AccountState::Ready, None)
    }
    fn install_with_state(
        &self,
        owner: &crate::journal::Owner,
        client: MessagingClient,
        state: AccountState,
        receiver: Option<Receiver>,
    ) -> Result<(), SendError> {
        if owner != &self.owner {
            return Err(SendError::Revoked);
        }
        let mut runtime = self.runtime.lock().map_err(|_| SendError::Unavailable)?;
        if runtime.client.is_some() {
            return Err(SendError::AlreadyReserved);
        }
        let observers = Arc::downgrade(&self.observers);
        let observer = client
            .history()
            .observe(Arc::new(move || {
                if let Some(observers) = observers.upgrade() {
                    observers.publish();
                }
            }))
            .ok_or(SendError::Unavailable)?;
        runtime.observer = Some(observer);
        runtime.receiver = receiver;
        runtime.client = Some(client);
        runtime.state = state;
        drop(runtime);
        self.observers.publish();
        Ok(())
    }
    fn active(&self, route: &Route) -> Result<MessagingClient, SendError> {
        if let Some(authority) = &self.authority {
            authority.check()?;
        }
        if route.service != self.account.service || route.sender.as_str() != self.account.account {
            return Err(SendError::Revoked);
        }
        let runtime = self.runtime.lock().map_err(|_| SendError::Unavailable)?;
        if runtime.state != AccountState::Ready
            || runtime.receiver.as_ref().is_some_and(Receiver::stopped)
        {
            return Err(SendError::Unavailable);
        }
        runtime.client.clone().ok_or(SendError::Unavailable)
    }
}
impl MessagingHost for AccountSupervisor {
    fn groups(&self, account: &ServiceAccount) -> Option<tilo_messaging::GroupClient> {
        #[cfg(feature = "whatsapp-sdk")]
        {
            self.whatsapp_groups(account)
        }
        #[cfg(not(feature = "whatsapp-sdk"))]
        {
            let _ = account;
            None
        }
    }
    fn setup(&self, account: &ServiceAccount) -> Option<tilo_messaging::SetupClient> {
        if account != &self.account {
            return None;
        }
        self.setup.lock().ok()?.clone()
    }
    fn account_state(&self, account: &ServiceAccount) -> AccountState {
        if account != &self.account {
            return AccountState::Unavailable;
        }
        if let Some(authority) = self.authority.as_ref()
            && let Err(error) = authority.check()
        {
            return if error == SendError::Revoked {
                AccountState::Locked
            } else {
                AccountState::Unavailable
            };
        }
        let (state, installed, stopped) = match self.runtime.lock() {
            Ok(runtime) => (
                runtime.state,
                runtime.client.is_some(),
                runtime.receiver.as_ref().is_some_and(Receiver::stopped),
            ),
            Err(_) => return AccountState::Failed,
        };
        if stopped {
            return AccountState::Failed;
        }
        if !installed {
            let setup = self.setup.lock().ok().and_then(|setup| setup.clone());
            if let Some(setup) = setup {
                return setup.state();
            }
        }
        state
    }
    fn supports(&self, route: &Route) -> bool {
        self.active(route)
            .is_ok_and(|client| client.supports(route))
    }
    fn send(&self, request: SendRequest) -> SendFuture {
        let client = self.active(&request.route);
        Box::pin(async move { client?.send(request).await })
    }
}
impl HistoryHost for AccountSupervisor {
    fn read(&self, route: Route, limit: usize) -> HistoryFuture {
        let client = self.active(&route);
        Box::pin(async move {
            if limit != tilo_messaging::MAX_HISTORY_MESSAGES {
                return Err(tilo_messaging::HistoryError::InvalidRoute);
            }
            client
                .map_err(|_| tilo_messaging::HistoryError::Unavailable)?
                .history()
                .read(route)
                .await
        })
    }
    fn observe(
        &self,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        self.observers.observe(changed)
    }
}

#[cfg(feature = "whatsapp-sdk")]
struct ReceiptTask(tokio::task::JoinHandle<()>);
#[cfg(feature = "whatsapp-sdk")]
impl Drop for ReceiptTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(feature = "whatsapp-sdk")]
pub mod whatsapp;
