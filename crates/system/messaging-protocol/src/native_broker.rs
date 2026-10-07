//! Account-owned direct private text, encrypted outside every shared record.
use crate::{
    Service,
    journal::{EncryptedJournal, Owner, RecipientEvidence},
    signal_broker::{Authorization, History, SignalAuthority},
    signal_session::{Completion, SendJournal},
};
use std::{future::Future, pin::Pin, sync::Arc};
use tilo_contact_directory::routing::Route;
use tilo_messaging::{
    Delivery, HistoryError, HistoryFuture, HistoryHost, MessagingClient, MessagingHost, SendError,
    SendFuture, SendReceipt, SendRequest,
};

pub type SendText<'a> = Pin<Box<dyn Future<Output = Result<(), SendError>> + Send + 'a>>;
/// The native host binds this to its original authenticated endpoint and gate.
/// Successful completion means the pinned peer durably accepted this exact id.
pub trait Transport: Send + Sync {
    fn send<'a>(&'a self, request: &'a SendRequest, timestamp_ms: u64) -> SendText<'a>;
}
struct Storage {
    history: History,
    journal: EncryptedJournal,
}
#[derive(Clone)]
pub struct NativeBroker {
    owner: Owner,
    scope: String,
    authority: Arc<dyn SignalAuthority>,
    transport: Arc<dyn Transport>,
    storage: Arc<tokio::sync::Mutex<Storage>>,
    observers: Arc<crate::signal_broker::observers::Observers>,
}
pub struct Assembly {
    pub scope: String,
    pub authority: Arc<dyn SignalAuthority>,
    pub transport: Arc<dyn Transport>,
    pub history: History,
    pub journal: EncryptedJournal,
}
impl NativeBroker {
    /// Receives already approved key custody; never creates keys or grants.
    /// # Errors
    /// Refuses foreign ownership, missing namespace or stale original authority.
    pub fn new(assembly: Assembly) -> Result<Self, SendError> {
        let owner = assembly.journal.owner().clone();
        let canonical = uuid::Uuid::parse_str(&owner.account)
            .is_ok_and(|id| !id.is_nil() && id.to_string() == owner.account);
        if !canonical
            || owner.service != Service::Tilo
            || owner.linking_generation == 0
            || assembly.history.owner() != &owner
            || assembly.scope != format!("tilo:{}", owner.account)
        {
            return Err(SendError::Revoked);
        }
        assembly.authority.check_account_scope(&assembly.scope)?;
        Ok(Self {
            owner,
            scope: assembly.scope,
            authority: assembly.authority,
            transport: assembly.transport,
            storage: Arc::new(tokio::sync::Mutex::new(Storage {
                history: assembly.history,
                journal: assembly.journal,
            })),
            observers: Arc::default(),
        })
    }
    pub fn owner(&self) -> &Owner {
        &self.owner
    }
    pub fn client(&self) -> MessagingClient {
        MessagingClient::new(&self.owner.principal, Arc::new(self.clone()))
    }
    fn authorized(&self, route: &Route) -> Result<Authorization, SendError> {
        self.authority.check_account_scope(&self.scope)?;
        let auth = self.authority.authorize(route)?;
        if auth.route != *route
            || route.service != "tilo"
            || route.scope != self.scope
            || route.sender.as_str() != self.owner.account
        {
            return Err(SendError::Revoked);
        }
        let native = crate::Route::new(
            Service::Tilo,
            self.owner.account.clone(),
            route.recipient.clone(),
        )
        .map_err(|_| SendError::Revoked)?;
        crate::routing::check_native(&auth.accounts, &native, &self.owner, &auth.binding)
            .map_err(|_| SendError::Revoked)?;
        let sender = tilo_contact_directory::routing::SenderAccount {
            id: route.sender.clone(),
            service: route.service.clone(),
            scope: self.scope.clone(),
            enabled: true,
        };
        if !tilo_contact_directory::routing::routes(
            &auth.directory,
            &route.person,
            "tilo",
            &[sender],
        )
        .contains(route)
        {
            return Err(SendError::Revoked);
        }
        Ok(auth)
    }
    /// The host derives `recipient` from the authenticated certificate/device.
    /// It cannot be supplied by a Messages app action or message payload.
    /// # Errors
    /// Refuses unknown/currently revoked routes and conflicting redelivery.
    pub async fn receive(
        &self,
        recipient: String,
        id: String,
        text: String,
        timestamp_ms: u64,
    ) -> Result<(), SendError> {
        let broker = self.clone();
        let text = zeroize::Zeroizing::new(text);
        tokio::task::spawn_blocking(move || {
            if uuid::Uuid::parse_str(&id).is_err() || timestamp_ms == 0 {
                return Err(SendError::InvalidRequest);
            }
            let proposed = broker.authority.incoming(&recipient)?;
            let mut storage = broker.storage.blocking_lock();
            let auth = broker.authorized(&proposed.route)?;
            if auth.route.recipient != recipient {
                return Err(SendError::Revoked);
            }
            let changed =
                storage
                    .history
                    .incoming_changed(&auth.route, &id, &text, timestamp_ms)?;
            broker.authorized(&auth.route)?;
            drop(storage);
            if changed {
                broker.observers.publish();
            }
            Ok(())
        })
        .await
        .map_err(|_| SendError::Unavailable)?
    }
    async fn submit(&self, request: SendRequest) -> Result<SendReceipt, SendError> {
        let id =
            uuid::Uuid::parse_str(&request.operation).map_err(|_| SendError::InvalidRequest)?;
        if id.is_nil()
            || id.to_string() != request.operation
            || request.text.is_empty()
            || request.text.len() > 16 * 1024
        {
            return Err(SendError::InvalidRequest);
        }
        let native = crate::Route::new(
            Service::Tilo,
            self.owner.account.clone(),
            request.route.recipient.clone(),
        )
        .map_err(|_| SendError::InvalidRequest)?;
        let timestamp = tilo_clock::unix_now_ms();
        self.authorized(&request.route)?;
        {
            let mut storage = self.storage.lock().await;
            storage.history.check_operation(&request.operation)?;
            self.authorized(&request.route)?;
            storage
                .journal
                .begin(&request.operation, &native, request.text.as_bytes())
                .map_err(|_| SendError::AlreadyReserved)?;
            self.authorized(&request.route)?;
            storage
                .history
                .reserve(&request.route, &request.operation, &request.text)?;
        }
        // Never hold the private store across peer I/O: simultaneous replies
        // must be able to persist inbound messages on both peers.
        self.authorized(&request.route)
            .map_err(|_| SendError::Uncertain)?;
        let result = self.transport.send(&request, timestamp).await;
        let mut storage = self.storage.lock().await;
        self.authorized(&request.route)
            .map_err(|_| SendError::Uncertain)?;
        if result.is_err() {
            storage
                .journal
                .complete(&request.operation, Completion::Uncertain)
                .map_err(|_| SendError::Uncertain)?;
            return Err(SendError::Uncertain);
        }
        storage
            .journal
            .complete(
                &request.operation,
                Completion::Acknowledged(crate::ProtocolReference::Tilo {
                    message_id: request.operation.clone(),
                }),
            )
            .map_err(|_| SendError::Uncertain)?;
        self.authorized(&request.route)
            .map_err(|_| SendError::Uncertain)?;
        storage
            .journal
            .observe_recipient(&request.operation, &native, RecipientEvidence::Delivered)
            .map_err(|_| SendError::Uncertain)?;
        self.authorized(&request.route)
            .map_err(|_| SendError::Uncertain)?;
        storage.history.observe_recipient(
            &request.route,
            &request.operation,
            RecipientEvidence::Delivered,
        )?;
        drop(storage);
        self.observers.publish();
        Ok(SendReceipt {
            route: request.route,
            operation: request.operation,
            // Submission replies follow the SDK contract; the durable peer ACK
            // is separately projected as RecipientDelivered in encrypted history.
            delivery: Delivery::Submitted,
        })
    }
}
impl MessagingHost for NativeBroker {
    fn supports(&self, route: &Route) -> bool {
        self.authorized(route).is_ok()
    }
    fn send(&self, request: SendRequest) -> SendFuture {
        let broker = self.clone();
        Box::pin(async move {
            let runtime = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || runtime.block_on(broker.submit(request)))
                .await
                .map_err(|_| SendError::Uncertain)?
        })
    }
}
impl HistoryHost for NativeBroker {
    fn observe(
        &self,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        self.observers.observe(changed)
    }
    fn read(&self, route: Route, limit: usize) -> HistoryFuture {
        let broker = self.clone();
        Box::pin(async move {
            if limit != tilo_messaging::MAX_HISTORY_MESSAGES {
                return Err(HistoryError::InvalidRoute);
            }
            tokio::task::spawn_blocking(move || {
                broker
                    .authorized(&route)
                    .map_err(|_| HistoryError::Revoked)?;
                let storage = broker.storage.blocking_lock();
                broker
                    .authorized(&route)
                    .map_err(|_| HistoryError::Revoked)?;
                storage
                    .history
                    .page(route, limit)
                    .map_err(|_| HistoryError::Unavailable)
            })
            .await
            .map_err(|_| HistoryError::Unavailable)?
        })
    }
}
