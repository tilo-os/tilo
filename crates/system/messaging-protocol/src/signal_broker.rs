//! Hosted Signal service: authenticated authority, durable private history and
//! the actual supervisor-owned signal-cli stream. No ambient sockets or grants.
#[path = "signal_broker/history.rs"]
pub(crate) mod history;
#[path = "signal_broker/observers.rs"]
pub(crate) mod observers;
use crate::{
    Service,
    routing::{LinkState, LiveBinding},
    signal_runtime::SignalRuntime,
};
pub use history::History;
use std::sync::{Arc, Mutex};
use tilo_contact_directory::routing::Route;
use tilo_messaging::{
    Delivery, HistoryError, HistoryFuture, HistoryHost, MessagingClient, MessagingHost, SendError,
    SendFuture, SendReceipt, SendRequest,
};

/// A snapshot from the host's current grant/Contacts/provider identity gate.
/// Returning it must verify the certified caller, account grant, exact current
/// association and provider fingerprint. Editable metadata alone is insufficient.
pub struct Authorization {
    pub directory: tilo_contact_directory::ContactDirectory,
    pub accounts: accounts::AccountStore,
    pub binding: LiveBinding,
    pub route: Route,
    retained: Option<Arc<dyn std::any::Any + Send + Sync>>,
}
impl Authorization {
    /// The host must check its current caller/account/identity gate first.
    /// This snapshot alone creates no credential permission.
    pub fn new(
        directory: tilo_contact_directory::ContactDirectory,
        accounts: accounts::AccountStore,
        binding: LiveBinding,
        route: Route,
    ) -> Self {
        Self {
            directory,
            accounts,
            binding,
            route,
            retained: None,
        }
    }
    /// Retain the host's existing account/credential read lease through native
    /// I/O. The lease is process-owned and never accepted from wire metadata.
    #[must_use]
    pub fn retaining(mut self, lease: Arc<dyn std::any::Any + Send + Sync>) -> Self {
        self.retained = Some(lease);
        self
    }
    /// Refuse `route` unless Contacts still routes its person through
    /// `route.sender` within `scope`, the frozen route matching exactly.
    pub(crate) fn listed(
        &self,
        route: &Route,
        scope: &str,
        enabled: bool,
    ) -> Result<(), SendError> {
        let sender = tilo_contact_directory::routing::SenderAccount {
            id: route.sender.clone(),
            service: route.service.clone(),
            scope: scope.to_owned(),
            enabled,
        };
        if tilo_contact_directory::routing::routes(
            &self.directory,
            &route.person,
            &route.service,
            &[sender],
        )
        .contains(route)
        {
            Ok(())
        } else {
            Err(SendError::Revoked)
        }
    }
}
pub trait SignalAuthority: Send + Sync {
    /// Revalidate the frozen account independently of any recipient route.
    /// A host without a current account producer cannot publish readiness.
    /// # Errors
    /// Refuses an absent, disabled, changed or closed original account owner.
    fn check_account(&self) -> Result<(), SendError> {
        Err(SendError::Unavailable)
    }
    /// Bind a verified provider self identity to this frozen current account.
    /// # Errors
    /// Refuses a different provider scope or a missing current account producer.
    fn check_account_scope(&self, _scope: &str) -> Result<(), SendError> {
        Err(SendError::Unavailable)
    }
    /// # Errors
    /// Refuses absent/revoked authority or stale Contacts/provider identity.
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError>;
    /// Resolve a provider-supplied ACI through current confirmed Contacts.
    /// # Errors
    /// Refuses unknown/ambiguous/revoked identities; never merges by name.
    fn incoming(&self, aci: &str) -> Result<Authorization, SendError>;
}
struct Worker {
    runtime: SignalRuntime,
    history: History,
}
#[derive(Clone)]
pub struct SignalBroker {
    worker: Arc<Mutex<Worker>>,
    authority: Arc<dyn SignalAuthority>,
    principal: String,
    owner: crate::journal::Owner,
    signal_account: String,
    wire_account: String,
    scope: String,
    observers: Arc<observers::Observers>,
}
impl SignalBroker {
    /// Register an already owned adapter and history with authenticated authority.
    /// No login/key provisioning or persistent grant is created.
    /// # Errors
    /// Refuses a mismatched owner, service or Signal identity.
    pub fn new(
        mut runtime: SignalRuntime,
        history: History,
        authority: Arc<dyn SignalAuthority>,
        signal_account: String,
    ) -> Result<Self, SendError> {
        if runtime.owner().service != Service::Signal
            || runtime.signal_account() != signal_account
            || history.owner() != runtime.owner()
        {
            return Err(SendError::Unavailable);
        }
        let setup = runtime
            .verify_account(tilo_clock::Duration::from_secs(10))
            .map_err(|_| SendError::Unavailable)?;
        let scope = setup.scope();
        let wire_account = setup.number.ok_or(SendError::Unavailable)?;
        let owner = runtime.owner().clone();
        let principal = runtime.owner().principal.clone();
        Ok(Self {
            worker: Arc::new(Mutex::new(Worker { runtime, history })),
            authority,
            principal,
            owner,
            signal_account,
            wire_account,
            scope,
            observers: Arc::default(),
        })
    }
    /// Assemble from a supervisor's verified handle and existing OS custody.
    /// # Errors
    /// Refuses absent/locked keychain, mismatched state or unusable daemon.
    pub fn open_system(
        root: &std::path::Path,
        owner: crate::journal::Owner,
        signal_account: String,
        stream: std::os::unix::net::UnixStream,
        authority: Arc<dyn SignalAuthority>,
    ) -> Result<Self, SendError> {
        let store = tilo_secrets::SecretStore::for_app("so.tilo.messaging-broker");
        if store.protection() != tilo_secrets::Protection::OsKeychain {
            return Err(SendError::Unavailable);
        }
        let key = crate::journal::FORMAT
            .load_or_create_system_key_in(true, &store)
            .ok_or(SendError::Unavailable)?;
        let history = History::open(&root.join("history"), &owner, key)?;
        let runtime = SignalRuntime::open_system(
            &root.join("journal"),
            owner,
            signal_account.clone(),
            stream,
        )
        .map_err(|_| SendError::Unavailable)?;
        Self::new(runtime, history, authority, signal_account)
    }
    /// Frozen authority for host registration, never inferred from metadata.
    /// # Errors
    /// Refuses a poisoned/retired owner worker.
    pub fn owner(&self) -> Result<crate::journal::Owner, SendError> {
        Ok(self
            .worker
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .runtime
            .owner()
            .clone())
    }
    pub fn client(&self) -> MessagingClient {
        MessagingClient::new(self.principal.clone(), Arc::new(self.clone()))
    }
    /// Self scope verified on this broker's actual owned provider stream.
    pub fn scope(&self) -> &str {
        &self.scope
    }
    fn authorized(&self, route: &Route) -> Result<Authorization, SendError> {
        let auth = self.authority.authorize(route)?;
        if auth.route != *route
            || route.service != "signal"
            || route.scope != self.scope
            || uuid::Uuid::parse_str(&route.recipient).is_err()
            || auth.binding.owner.principal != self.principal
            || auth.binding.state != LinkState::Linked
        {
            return Err(SendError::Revoked);
        }
        let protocol = crate::Route::new(
            Service::Signal,
            route.sender.as_str().into(),
            route.recipient.clone(),
        )
        .map_err(|_| SendError::Revoked)?;
        crate::routing::check(&auth.accounts, &protocol, &self.owner, &auth.binding)
            .map_err(|_| SendError::Revoked)?;
        let enabled = auth
            .accounts
            .get(&accounts::AccountId::new(route.sender.as_str()))
            .is_some_and(|account| account.enabled);
        auth.listed(route, &self.scope, enabled)?;
        Ok(auth)
    }
    fn send_once(&self, request: SendRequest) -> Result<SendReceipt, SendError> {
        let auth = self.authorized(&request.route)?;
        let route = crate::Route::new(
            Service::Signal,
            request.route.sender.as_str().into(),
            request.route.recipient.clone(),
        )
        .map_err(|_| SendError::InvalidRequest)?;
        let mut worker = self.worker.lock().map_err(|_| SendError::Unavailable)?;
        crate::routing::check(
            &auth.accounts,
            &route,
            worker.runtime.owner(),
            &auth.binding,
        )
        .map_err(|_| SendError::Revoked)?;
        worker.history.check_operation(&request.operation)?;
        if worker.runtime.status(&request.operation).is_some() {
            return Err(SendError::AlreadyReserved);
        }
        worker
            .runtime
            .verify_recipient(
                &request.route.recipient,
                &request.route.revision,
                tilo_clock::Duration::from_secs(10),
            )
            .map_err(|_| SendError::Revoked)?;
        worker
            .history
            .reserve(&request.route, &request.operation, &request.text)?;
        // Revalidate after durable reservation, immediately before the adapter.
        let auth = self.authorized(&request.route)?;
        let result = worker.runtime.send(
            &auth.accounts,
            &auth.binding,
            &route,
            &request.operation,
            &request.text,
            tilo_clock::Duration::from_secs(10),
        );
        let acknowledgment = result.map_err(|error| match error {
            crate::signal_session::CheckedSendError::Routing(_) => SendError::Revoked,
            crate::signal_session::CheckedSendError::Transport(
                crate::signal_session::SendError::Journal(
                    crate::signal_session::JournalError::AlreadyReserved,
                ),
            ) => SendError::AlreadyReserved,
            crate::signal_session::CheckedSendError::Transport(_) => SendError::Uncertain,
        })?;
        self.authorized(&request.route)
            .map_err(|_| SendError::Uncertain)?;
        worker
            .history
            .acknowledge(&request.operation, acknowledgment.timestamp)?;
        let events = worker.runtime.take_events();
        worker
            .history
            .stage_events(&events)
            .map_err(|_| SendError::Uncertain)?;
        self.project_pending(&mut worker)
            .map_err(|_| SendError::Uncertain)?;
        drop(worker);
        self.observers.publish();
        Ok(SendReceipt {
            route: request.route,
            operation: request.operation,
            delivery: Delivery::ServerAcknowledged,
        })
    }
    fn project_pending(&self, worker: &mut Worker) -> Result<(), SendError> {
        let mut retained = Vec::new();
        for frame in worker.history.pending() {
            if let Ok(incoming) = crate::signal::incoming(&frame, &self.wire_account)
                && self
                    .persist_incoming(&mut worker.history, &incoming)
                    .is_ok()
            {
                continue;
            }
            if let Ok(receipt) = crate::signal::receipt(&frame, &self.wire_account)
                && self.persist_receipt(worker, &receipt).is_ok()
            {
                continue;
            }
            retained.push(frame);
        }
        worker.history.retain_pending(&retained)
    }
    fn persist_receipt(
        &self,
        worker: &mut Worker,
        receipt: &crate::signal::Receipt,
    ) -> Result<(), SendError> {
        let auth = self.authority.incoming(&receipt.sender_aci)?;
        let current = self.authorized(&auth.route)?;
        if current.route.recipient != receipt.sender_aci || receipt.account != self.wire_account {
            return Err(SendError::Revoked);
        }
        let native = crate::Route::new(
            Service::Signal,
            self.owner.account.clone(),
            receipt.sender_aci.clone(),
        )
        .map_err(|_| SendError::Revoked)?;
        let operations = worker
            .runtime
            .receipt_operations(&native, &receipt.timestamps)
            .map_err(|_| SendError::InvalidRequest)?;
        for operation in &operations {
            if worker.history.outgoing_route(operation)? != current.route {
                return Err(SendError::Revoked);
            }
        }
        for operation in operations {
            self.authorized(&current.route)?;
            worker
                .runtime
                .observe_recipient(
                    &current.accounts,
                    &current.binding,
                    &native,
                    &operation,
                    receipt.evidence,
                )
                .map_err(|_| SendError::Revoked)?;
            self.authorized(&current.route)?;
            worker
                .history
                .observe_recipient(&current.route, &operation, receipt.evidence)?;
        }
        Ok(())
    }
    fn persist_incoming(
        &self,
        history: &mut History,
        incoming: &crate::signal::Incoming,
    ) -> Result<(), SendError> {
        let auth = self.authority.incoming(&incoming.id.sender_aci)?;
        let current = self.authorized(&auth.route)?;
        if current.route.recipient != incoming.id.sender_aci
            || incoming.id.account != self.wire_account
        {
            return Err(SendError::Revoked);
        }
        history.incoming(
            &current.route,
            &format!("{}:{}", incoming.id.sender_aci, incoming.id.timestamp),
            &incoming.text,
            incoming.id.timestamp,
        )
    }
    /// Start independent inbound persistence under this frozen owner.
    /// The host retains the guard; dropping it shuts down only this connection.
    /// # Errors
    /// Refuses another owner/provider or unavailable receive handle.
    pub fn start_receiver(
        &self,
        mut receiver: crate::signal_runtime::SignalReceiver,
    ) -> Result<ReceiveGuard, SendError> {
        if receiver.owner() != &self.owner
            || receiver.signal_account() != self.signal_account
            || receiver.scope() != self.scope
            || receiver.wire_account() != self.wire_account
        {
            return Err(SendError::Revoked);
        }
        let control = receiver.interrupt().map_err(|_| SendError::Unavailable)?;
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = stopped.clone();
        let broker = self.clone();
        let thread = std::thread::Builder::new()
            .name("tilo-signal-inbox".into())
            .spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let incoming = receiver.receive();
                    let Ok(mut worker) = broker.worker.lock() else {
                        break;
                    };
                    let events = receiver.take_events();
                    if worker.history.stage_events(&events).is_err() {
                        break;
                    }
                    if let Ok(mut incoming) = incoming {
                        incoming.id.account.clone_from(&broker.wire_account);
                        if worker
                            .history
                            .stage_incoming(&broker.wire_account, &incoming)
                            .is_err()
                        {
                            break;
                        }
                    } else if !matches!(incoming, Err(crate::Error::Unsupported)) {
                        break;
                    }
                    if broker.project_pending(&mut worker).is_err() {
                        break;
                    }
                    drop(worker);
                    broker.observers.publish();
                }
                stop.store(true, std::sync::atomic::Ordering::SeqCst);
                broker.observers.publish();
            })
            .map_err(|_| SendError::Unavailable)?;
        Ok(ReceiveGuard {
            control,
            stopped,
            thread: Some(thread),
        })
    }
}
impl MessagingHost for SignalBroker {
    fn supports(&self, route: &Route) -> bool {
        self.authorized(route).is_ok()
    }
    fn send(&self, request: SendRequest) -> SendFuture {
        let broker = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || broker.send_once(request))
                .await
                .map_err(|_| SendError::Uncertain)?
        })
    }
}
impl HistoryHost for SignalBroker {
    fn observe(
        &self,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        self.observers.observe(changed)
    }
    fn read(&self, route: Route, limit: usize) -> HistoryFuture {
        let broker = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                broker
                    .authorized(&route)
                    .map_err(|_| HistoryError::Revoked)?;
                let mut worker = broker
                    .worker
                    .lock()
                    .map_err(|_| HistoryError::Unavailable)?;
                broker
                    .authorized(&route)
                    .map_err(|_| HistoryError::Revoked)?;
                broker
                    .project_pending(&mut worker)
                    .map_err(|_| HistoryError::Unavailable)?;
                worker
                    .history
                    .page(route, limit.min(tilo_messaging::MAX_HISTORY_MESSAGES))
                    .map_err(|_| HistoryError::Unavailable)
            })
            .await
            .map_err(|_| HistoryError::Unavailable)?
        })
    }
}

/// Supervisor-owned lifetime and health of one inbound worker.
pub struct ReceiveGuard {
    control: std::os::unix::net::UnixStream,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl ReceiveGuard {
    pub fn stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }
}
impl Drop for ReceiveGuard {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = self.control.shutdown(std::net::Shutdown::Both);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
