//! Host-to-Messages adapters for verified `WhatsApp` and SMS accounts.
//! The publisher owns custody, provider handles and the current credential/SIM
//! lease. Neither editable metadata nor the app can create these capabilities.
use crate::{
    journal::Owner,
    signal_broker::history::service_name,
    signal_broker::{Authorization, History},
};
use std::{future::Future, pin::Pin, sync::Arc};
use tilo_contact_directory::routing::Route;
use tilo_messaging::{
    Delivery, HistoryError, HistoryFuture, HistoryHost, MessagingClient, MessagingHost, SendError,
    SendFuture, SendReceipt, SendRequest,
};
#[cfg(all(target_os = "linux", feature = "sms-dbus"))]
mod sms_inbox;
#[cfg(all(target_os = "linux", feature = "sms-dbus"))]
pub use sms_inbox::SmsReceiveGuard;

pub type NativeSend<'a> = Pin<
    Box<
        dyn Future<
                Output = Result<crate::ProtocolReference, crate::signal_session::CheckedSendError>,
            > + Send
            + 'a,
    >,
>;
#[cfg(all(target_os = "linux", feature = "sms-dbus"))]
pub type NativeReceive<'a> =
    Pin<Box<dyn Future<Output = Result<crate::sms_runtime::ReceivedSms, SendError>> + Send + 'a>>;
/// Implemented by account-owned runtimes; authority remains in the host gate.
pub trait NativeAdapter: Send {
    fn owner(&self) -> &Owner;
    /// Read-only source from the exact host-owned SMS runtime, never an app input.
    #[cfg(all(target_os = "linux", feature = "sms-dbus"))]
    fn sms_inbox(&self) -> Option<crate::sms_runtime::SmsInbox> {
        None
    }
    /// # Errors
    /// Refuses invalid authority, unavailable transport or uncertain native submission.
    fn send<'a>(
        &'a mut self,
        auth: &'a Authorization,
        route: &'a crate::Route,
        request: &'a SendRequest,
    ) -> NativeSend<'a>;
    /// Read complete text from the pinned modem without changing native state.
    /// # Errors
    /// Refuses adapters without SMS receive support or stale native identity.
    #[cfg(all(target_os = "linux", feature = "sms-dbus"))]
    fn sms_incoming<'a>(
        &'a self,
        _auth: &'a Authorization,
        _route: &'a crate::Route,
        _object: &'a str,
    ) -> NativeReceive<'a> {
        Box::pin(async { Err(SendError::Unavailable) })
    }
    /// Admit recipient evidence only from the runtime's own SDK subscription.
    /// # Errors
    /// Refuses an unsupported service, foreign SDK client or unrelated receipt.
    #[cfg(feature = "whatsapp-sdk")]
    fn whatsapp_receipt(
        &mut self,
        _context: &crate::whatsapp_runtime::ReceiptContext<'_>,
        _event: &whatsapp_rust::wacore::types::events::Receipt,
    ) -> Result<crate::whatsapp::Evidence, SendError> {
        Err(SendError::Unavailable)
    }
    /// Whether this adapter owns this precise SDK instance, without I/O.
    #[cfg(feature = "whatsapp-sdk")]
    fn whatsapp_source(&self, _source: &Arc<whatsapp_rust::Client>) -> bool {
        false
    }
    /// # Errors
    /// Refuses unsupported adapters, foreign sources or non-direct text.
    #[cfg(feature = "whatsapp-sdk")]
    fn whatsapp_incoming(
        &self,
        _context: &crate::whatsapp_runtime::IncomingContext<'_>,
        _event: &whatsapp_rust::wacore::types::events::InboundMessage,
    ) -> Result<crate::whatsapp_runtime::IncomingText, SendError> {
        Err(SendError::Unavailable)
    }
}
#[cfg(feature = "whatsapp-sdk")]
impl NativeAdapter for crate::whatsapp_runtime::WhatsappRuntime {
    fn owner(&self) -> &Owner {
        self.owner()
    }
    fn send<'a>(
        &'a mut self,
        auth: &'a Authorization,
        route: &'a crate::Route,
        request: &'a SendRequest,
    ) -> NativeSend<'a> {
        Box::pin(self.send(
            &auth.accounts,
            &auth.binding,
            route,
            &request.operation,
            &request.text,
            tilo_clock::Duration::from_secs(10),
        ))
    }
    fn whatsapp_receipt(
        &mut self,
        context: &crate::whatsapp_runtime::ReceiptContext<'_>,
        event: &whatsapp_rust::wacore::types::events::Receipt,
    ) -> Result<crate::whatsapp::Evidence, SendError> {
        self.observe_receipt(context, event)
            .map_err(|_| SendError::Revoked)
    }
    fn whatsapp_source(&self, source: &Arc<whatsapp_rust::Client>) -> bool {
        self.owns_source(source)
    }
    fn whatsapp_incoming(
        &self,
        context: &crate::whatsapp_runtime::IncomingContext<'_>,
        event: &whatsapp_rust::wacore::types::events::InboundMessage,
    ) -> Result<crate::whatsapp_runtime::IncomingText, SendError> {
        self.incoming_text(context, event)
            .map_err(|error| match error {
                crate::Error::WrongAccount | crate::Error::InvalidIdentity => SendError::Revoked,
                crate::Error::Unsupported => SendError::Unavailable,
                _ => SendError::InvalidRequest,
            })
    }
}
#[cfg(all(target_os = "linux", feature = "sms-dbus"))]
impl NativeAdapter for crate::sms_runtime::SmsRuntime {
    fn owner(&self) -> &Owner {
        self.owner()
    }
    fn sms_inbox(&self) -> Option<crate::sms_runtime::SmsInbox> {
        Some(self.inbox())
    }
    fn send<'a>(
        &'a mut self,
        auth: &'a Authorization,
        route: &'a crate::Route,
        request: &'a SendRequest,
    ) -> NativeSend<'a> {
        Box::pin(self.send(
            &auth.accounts,
            &auth.binding,
            route,
            &request.operation,
            &request.text,
            tilo_clock::Duration::from_secs(10),
        ))
    }
    fn sms_incoming<'a>(
        &'a self,
        auth: &'a Authorization,
        route: &'a crate::Route,
        object: &'a str,
    ) -> NativeReceive<'a> {
        Box::pin(async move {
            self.read_received(
                &auth.accounts,
                &auth.binding,
                route,
                object,
                tilo_clock::Duration::from_secs(10),
            )
            .await
            .map_err(|error| match error {
                crate::Error::WrongAccount | crate::Error::InvalidIdentity => SendError::Revoked,
                crate::Error::Unsupported | crate::Error::ConnectionUnavailable => {
                    SendError::Unavailable
                }
                _ => SendError::InvalidRequest,
            })
        })
    }
}
/// Current certified caller, account lease and reviewed provider identity gate.
/// The publisher must hold exclusive credential/SIM authority across adapter I/O.
pub trait ExternalAuthority: Send + Sync {
    /// # Errors
    /// Refuses revoked caller/account authority or stale provider identity.
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError>;
    /// Resolve one exact provider identity through current reviewed Contacts.
    /// Ambiguous associations must refuse; names and alternate JIDs are not
    /// identity evidence. The default provides no inbound permission.
    /// # Errors
    /// Refuses absent or revoked receive authority and ambiguous identities.
    fn incoming(&self, _recipient: &str) -> Result<Authorization, SendError> {
        Err(SendError::Unavailable)
    }
}
struct Worker {
    adapter: Box<dyn NativeAdapter>,
    history: History,
}
#[derive(Clone)]
pub struct ExternalBroker {
    worker: Arc<tokio::sync::Mutex<Worker>>,
    authority: Arc<dyn ExternalAuthority>,
    owner: Owner,
    scope: String,
    observers: Arc<crate::signal_broker::observers::Observers>,
}
impl ExternalBroker {
    /// Assemble an already linked native adapter using existing OS custody.
    /// The host supplies the authenticated provider handle and current grant
    /// gate. This path never connects to an ambient service or creates keys.
    /// Run on the account supervisor's worker, not the rendering thread.
    /// # Errors
    /// Refuses absent/locked custody, wrong ownership or invalid private state.
    pub fn open_system(
        root: &std::path::Path,
        adapter: Box<dyn NativeAdapter>,
        scope: String,
        authority: Arc<dyn ExternalAuthority>,
    ) -> Result<Self, SendError> {
        if !root.is_absolute() {
            return Err(SendError::Unavailable);
        }
        let store = tilo_secrets::SecretStore::for_app("so.tilo.messaging-broker");
        if store.protection() != tilo_secrets::Protection::OsKeychain {
            return Err(SendError::Unavailable);
        }
        let key = crate::journal::FORMAT
            .load_or_create_system_key_in(true, &store)
            .ok_or(SendError::Unavailable)?;
        let history = History::open(&root.join("history"), adapter.owner(), key)?;
        Self::new(adapter, history, scope, authority)
    }
    /// Bind an already verified runtime and encrypted store; creates no grant.
    /// # Errors
    /// Refuses another owner, unsupported service or missing provider namespace.
    pub fn new(
        adapter: Box<dyn NativeAdapter>,
        history: History,
        scope: String,
        authority: Arc<dyn ExternalAuthority>,
    ) -> Result<Self, SendError> {
        let owner = adapter.owner().clone();
        if history.owner() != &owner
            || !matches!(
                owner.service,
                crate::Service::Whatsapp | crate::Service::Sms
            )
            || scope.is_empty()
            || scope.len() > 512
            || scope.chars().any(char::is_control)
        {
            return Err(SendError::Unavailable);
        }
        Ok(Self {
            worker: Arc::new(tokio::sync::Mutex::new(Worker { adapter, history })),
            authority,
            owner,
            scope,
            observers: Arc::default(),
        })
    }
    pub fn owner(&self) -> &Owner {
        &self.owner
    }
    pub fn client(&self) -> MessagingClient {
        MessagingClient::new(&self.owner.principal, Arc::new(self.clone()))
    }
    /// Read one host-selected native SMS into exact-route encrypted history.
    /// The supervisor supplies the object and holds the current SIM lease.
    /// No Create, Send, Store, Delete or acknowledgement is performed.
    /// # Errors
    /// Refuses another service, stale grants, foreign objects and incomplete SMS.
    #[cfg(all(target_os = "linux", feature = "sms-dbus"))]
    pub async fn observe_sms_message(
        &self,
        route: Route,
        object: String,
    ) -> Result<bool, SendError> {
        if self.owner.service != crate::Service::Sms {
            return Err(SendError::Unavailable);
        }
        self.authorized(&route)?;
        let mut worker = self.worker.lock().await;
        let auth = self.authorized(&route)?;
        let native = crate::Route::new(
            crate::Service::Sms,
            self.owner.account.clone(),
            route.recipient.clone(),
        )
        .map_err(|_| SendError::Revoked)?;
        let incoming = worker.adapter.sms_incoming(&auth, &native, &object).await?;
        self.authorized(&route)?;
        if incoming.number != route.recipient {
            return Err(SendError::Revoked);
        }
        let changed = worker.history.incoming_changed(
            &route,
            &incoming.id,
            &incoming.text,
            incoming.timestamp_ms,
        )?;
        drop(worker);
        if changed {
            self.observers.publish();
        }
        Ok(changed)
    }
    /// Durably project direct text from the host's exact SDK instance.
    /// Apps cannot submit events. The supervisor must await this before its
    /// durable inbound hook succeeds; event-only observation does not promise
    /// at-least-once delivery. No network operation or ACK is performed here.
    /// # Errors
    /// Refuses foreign sources, unsupported message semantics, stale identity
    /// and conflicting redeliveries before changing the visible history.
    #[cfg(feature = "whatsapp-sdk")]
    pub async fn observe_whatsapp_message(
        &self,
        source: Arc<whatsapp_rust::Client>,
        event: whatsapp_rust::wacore::types::events::InboundMessage,
    ) -> Result<bool, SendError> {
        self.observe_whatsapp_batch(source, vec![event])
            .await
            .map(|count| count > 0)
    }
    /// Atomically persist a bounded SDK batch before its durability hook succeeds.
    /// The supervisor retains account custody throughout; no ACK or network I/O
    /// is performed by this API. Unsupported or conflicting batches fail whole.
    /// # Errors
    /// Refuses another SDK/service, stale identities, unsupported payloads and
    /// non-durable storage before publishing a view wake.
    #[cfg(feature = "whatsapp-sdk")]
    pub async fn observe_whatsapp_batch(
        &self,
        source: Arc<whatsapp_rust::Client>,
        events: Vec<whatsapp_rust::wacore::types::events::InboundMessage>,
    ) -> Result<usize, SendError> {
        if self.owner.service != crate::Service::Whatsapp || events.is_empty() || events.len() > 64
        {
            return Err(SendError::InvalidRequest);
        }
        let broker = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut worker = broker.worker.blocking_lock();
            if !worker.adapter.whatsapp_source(&source) {
                return Err(SendError::Revoked);
            }
            let mut projected = Vec::with_capacity(events.len());
            for event in &events {
                let recipient = event.info.source.sender.to_string();
                let proposed = broker.authority.incoming(&recipient)?;
                let auth = broker.authorized(&proposed.route)?;
                if auth.route.recipient != recipient {
                    return Err(SendError::Revoked);
                }
                let native = crate::Route::new(
                    crate::Service::Whatsapp,
                    broker.owner.account.clone(),
                    recipient,
                )
                .map_err(|_| SendError::Revoked)?;
                let incoming = worker.adapter.whatsapp_incoming(
                    &crate::whatsapp_runtime::IncomingContext {
                        accounts: &auth.accounts,
                        binding: &auth.binding,
                        source: &source,
                        route: &native,
                    },
                    event,
                )?;
                projected.push((auth, incoming));
            }
            for (auth, _) in &mut projected {
                *auth = broker.authorized(&auth.route)?;
            }
            let batch: Vec<_> = projected
                .iter()
                .map(
                    |(auth, incoming)| crate::signal_broker::history::IncomingRecord {
                        route: &auth.route,
                        id: &incoming.id,
                        text: &incoming.text,
                        timestamp_ms: incoming.timestamp_ms,
                    },
                )
                .collect();
            let count = worker.history.incoming_batch(&batch)?;
            drop(worker);
            if count > 0 {
                broker.observers.publish();
            }
            Ok(count)
        })
        .await
        .map_err(|_| SendError::Unavailable)?
    }
    /// Project a receipt delivered by the supervisor's account-owned SDK.
    /// This is a host API; apps receive only the private Messaging client.
    /// No send/retry occurs. The SDK source, immutable operation route and
    /// current identity/account grant are checked before publishing evidence.
    /// # Errors
    /// Refuses stale authority, unknown operations, foreign SDK sources and
    /// receipts without the exact recipient/message identity.
    #[cfg(feature = "whatsapp-sdk")]
    pub async fn observe_whatsapp_receipt(
        &self,
        operation: String,
        source: Arc<whatsapp_rust::Client>,
        event: whatsapp_rust::wacore::types::events::Receipt,
    ) -> Result<bool, SendError> {
        if self.owner.service != crate::Service::Whatsapp || !crate::identifier(&operation) {
            return Err(SendError::InvalidRequest);
        }
        let broker = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut worker = broker.worker.blocking_lock();
            let route = worker.history.outgoing_route(&operation)?;
            let auth = broker.authorized(&route)?;
            let native = crate::Route::new(
                crate::Service::Whatsapp,
                broker.owner.account.clone(),
                route.recipient.clone(),
            )
            .map_err(|_| SendError::Revoked)?;
            let evidence = worker.adapter.whatsapp_receipt(
                &crate::whatsapp_runtime::ReceiptContext {
                    accounts: &auth.accounts,
                    binding: &auth.binding,
                    source: &source,
                    route: &native,
                    operation: &operation,
                },
                &event,
            )?;
            broker.authorized(&route)?;
            let recipient = match evidence {
                crate::whatsapp::Evidence::RecipientDelivered => {
                    crate::journal::RecipientEvidence::Delivered
                }
                crate::whatsapp::Evidence::RecipientRead => crate::journal::RecipientEvidence::Read,
                _ => return Ok(false),
            };
            let changed = worker
                .history
                .observe_recipient(&route, &operation, recipient)?;
            drop(worker);
            if changed {
                broker.observers.publish();
            }
            Ok(changed)
        })
        .await
        .map_err(|_| SendError::Unavailable)?
    }
    fn authorized(&self, route: &Route) -> Result<Authorization, SendError> {
        let auth = self.authority.authorize(route)?;
        if auth.route != *route
            || route.service != service_name(self.owner.service)
            || route.sender.as_str() != self.owner.account
            || route.scope != self.scope
        {
            return Err(SendError::Revoked);
        }
        let native = crate::Route::new(
            self.owner.service,
            self.owner.account.clone(),
            route.recipient.clone(),
        )
        .map_err(|_| SendError::Revoked)?;
        crate::routing::check(&auth.accounts, &native, &self.owner, &auth.binding)
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
            &route.service,
            &[sender],
        )
        .contains(route)
        {
            return Err(SendError::Revoked);
        }
        Ok(auth)
    }
    async fn submit(&self, request: SendRequest) -> Result<SendReceipt, SendError> {
        self.authorized(&request.route)?;
        let mut worker = self.worker.lock().await;
        worker.history.check_operation(&request.operation)?;
        self.authorized(&request.route)?;
        worker
            .history
            .reserve(&request.route, &request.operation, &request.text)?;
        let auth = self.authorized(&request.route)?;
        let native = crate::Route::new(
            self.owner.service,
            self.owner.account.clone(),
            request.route.recipient.clone(),
        )
        .map_err(|_| SendError::InvalidRequest)?;
        let reference = worker
            .adapter
            .send(&auth, &native, &request)
            .await
            .map_err(|error| match error {
                crate::signal_session::CheckedSendError::Routing(_) => SendError::Revoked,
                crate::signal_session::CheckedSendError::Transport(_) => SendError::Uncertain,
            })?;
        let matched = match (&reference, self.owner.service) {
            (crate::ProtocolReference::Whatsapp { message_id }, crate::Service::Whatsapp) => {
                message_id == &request.operation
            }
            (
                crate::ProtocolReference::Sms {
                    modem_object,
                    message_object,
                    ..
                },
                crate::Service::Sms,
            ) => {
                modem_object.starts_with('/')
                    && message_object.starts_with('/')
                    && modem_object != message_object
            }
            _ => false,
        };
        if !matched {
            return Err(SendError::Uncertain);
        }
        self.authorized(&request.route)
            .map_err(|_| SendError::Uncertain)?;
        worker.history.submitted(&request.operation)?;
        drop(worker);
        self.observers.publish();
        Ok(SendReceipt {
            route: request.route,
            operation: request.operation,
            delivery: Delivery::Submitted,
        })
    }
}
impl MessagingHost for ExternalBroker {
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
impl HistoryHost for ExternalBroker {
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
                let worker = broker.worker.blocking_lock();
                broker
                    .authorized(&route)
                    .map_err(|_| HistoryError::Revoked)?;
                worker
                    .history
                    .page(route, limit)
                    .map_err(|_| HistoryError::Unavailable)
            })
            .await
            .map_err(|_| HistoryError::Unavailable)?
        })
    }
}

#[cfg(test)]
#[cfg(feature = "whatsapp-sdk")]
#[path = "external_broker/receipt_tests.rs"]
mod receipt_tests;
#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
#[cfg(feature = "whatsapp-sdk")]
#[path = "external_broker/incoming_tests.rs"]
mod incoming_tests;

#[cfg(feature = "whatsapp-sdk")]
mod whatsapp_hook;
#[cfg(feature = "whatsapp-sdk")]
pub use whatsapp_hook::WhatsappInboxHook;

#[cfg(feature = "whatsapp-sdk")]
mod whatsapp_subscription;
#[cfg(feature = "whatsapp-sdk")]
pub use whatsapp_subscription::WhatsappReceiptSubscription;

#[cfg(test)]
#[cfg(feature = "whatsapp-sdk")]
#[path = "external_broker/supervisor_tests.rs"]
mod supervisor_tests;
