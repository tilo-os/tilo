//! The service: the person's ownership, their running accounts, the one
//! enrollment flow per service, and the apps it serves.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use launcher::owned_service::ServiceCancellation;
use tilo_messaging::{
    EnrollmentReview, MessagingClient, MessagingRegistry, Pairing, SendError, ServiceAccount,
};
use tilo_messaging_protocol::journal::Owner;
use tilo_messaging_protocol::supervisor::AccountSupervisor;

use crate::link::{Handler, Responder};
use crate::session::{self, SessionLink};
use crate::wire::{self, AccountView, AppLink, SetupView, Snapshot};

mod consent;
mod flow;
mod guard;
mod identity;
mod records;
mod serve;
mod signal;
mod sms;
mod whatsapp;

pub use serve::listen;

use flow::Flow;
use guard::{AccountGuard, Running};
use identity::{Credential, Identity, Ownership};

/// The service's four flows.
struct Flows {
    signal: Arc<Flow<signal::Signal>>,
    whatsapp: Arc<Flow<whatsapp::Whatsapp>>,
    sms: Arc<Flow<sms::Sms>>,
}

pub struct Service {
    ownership: Arc<Ownership>,
    session: SessionLink,
    registry: Arc<dyn MessagingRegistry>,
    messaging: MessagingClient,
    flows: Flows,
    running: Mutex<Vec<Arc<dyn Running>>>,
    /// Where each account's private state lives, by owner.
    root: PathBuf,
    /// Owns every provider start: dropping the service stops them.
    lifetime: Arc<()>,
    changed: Arc<tokio::sync::Notify>,
    clients: Mutex<Vec<AppLink>>,
    restored: AtomicBool,
    runtime: tokio::runtime::Handle,
    history: OnceLock<Box<dyn std::any::Any + Send + Sync>>,
}

impl Service {
    /// The service for `actor` on SDK device `device`, keeping account
    /// state under `profile`, asking the compositor through `session`.
    ///
    /// # Errors
    /// The person's ownership cannot be read.
    pub fn new(
        actor: &str,
        device: &str,
        profile: &std::path::Path,
        session: SessionLink,
        runtime: tokio::runtime::Handle,
    ) -> Result<Arc<Self>, String> {
        let ownership = Arc::new(Ownership::load(actor, device)?);
        Self::assemble(ownership, profile, session, runtime)
    }

    fn assemble(
        ownership: Arc<Ownership>,
        profile: &std::path::Path,
        session: SessionLink,
        runtime: tokio::runtime::Handle,
    ) -> Result<Arc<Self>, String> {
        let actor = ownership.actor().to_owned();
        let actor = actor.as_str();
        let registry =
            tilo_messaging::messaging_registry(actor).map_err(|_| "invalid person".to_owned())?;
        let messaging = registry.clone().client();
        let service = Arc::new_cyclic(|this: &Weak<Self>| Self {
            ownership,
            session,
            registry,
            messaging,
            flows: Flows {
                signal: Flow::new(signal::Signal, this.clone()),
                whatsapp: Flow::new(whatsapp::Whatsapp, this.clone()),
                sms: Flow::new(sms::Sms, this.clone()),
            },
            running: Mutex::default(),
            root: profile.join("private-messages"),
            lifetime: Arc::new(()),
            changed: Arc::new(tokio::sync::Notify::new()),
            clients: Mutex::default(),
            restored: AtomicBool::new(false),
            runtime,
            history: OnceLock::new(),
        });
        let observed = Arc::downgrade(&service);
        if let Some(guard) = service.messaging.history().observe(Arc::new(move || {
            if let Some(service) = observed.upgrade() {
                for client in service.clients() {
                    client.event(wire::Event::HistoryChanged);
                }
            }
        })) {
            let _ = service.history.set(guard);
        }
        service
            .runtime
            .spawn(Self::keep_published(Arc::downgrade(&service)));
        Ok(service)
    }

    /// Keep every app's snapshot current: on each change, and every half
    /// second for what runs on its own (an account going offline).
    async fn keep_published(service: Weak<Self>) {
        let mut last = String::new();
        loop {
            let Some(current) = service.upgrade() else {
                return;
            };
            let notify = current.changed.clone();
            let changed = notify.notified();
            current.flows.signal.check();
            current.flows.whatsapp.check();
            current.flows.sms.check();
            let snapshot = current.snapshot();
            let encoded = serde_json::to_string(&snapshot).unwrap_or_default();
            if encoded != last {
                for client in current.clients() {
                    client.event(wire::Event::State(snapshot.clone()));
                }
                last = encoded;
            }
            drop(current);
            let _ = tokio::time::timeout(Duration::from_millis(500), changed).await;
        }
    }

    pub(crate) fn changed(&self) {
        self.changed.notify_one();
    }

    fn clients(&self) -> Vec<AppLink> {
        let mut clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        clients.retain(|client| !client.is_closed());
        clients.clone()
    }

    pub(crate) fn serve(&self, client: AppLink) {
        client.event(wire::Event::State(self.snapshot()));
        self.clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(client);
    }

    fn snapshot(&self) -> Snapshot {
        let running = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let accounts = running
            .iter()
            .map(|account| {
                let id = account.guard().account();
                AccountView {
                    state: self.messaging.account_state(&id),
                    groups: self.messaging.groups(&id).is_some(),
                    setup: self.messaging.setup(&id).map(|setup| SetupView {
                        generation: setup.generation(),
                        state: setup.state(),
                        wake_after_ms: setup
                            .wake_after()
                            .map(|after| u64::try_from(after.as_millis()).unwrap_or(u64::MAX)),
                    }),
                    account: id,
                }
            })
            .collect();
        let mut enrollments = self.flows.signal.views();
        enrollments.extend(self.flows.whatsapp.views());
        enrollments.extend(self.flows.sms.views());
        Snapshot {
            actor: self.ownership.actor().into(),
            accounts,
            enrollments,
        }
    }

    /// A new running account for `identity` under its key, guarded by the
    /// person's records, registered with the apps' client.
    pub(crate) fn account<A: Running + 'static>(
        &self,
        identity: &Identity,
        credential: Credential,
        scope: &str,
        make: impl FnOnce(Arc<AccountGuard>, Arc<AccountSupervisor>) -> A,
    ) -> Result<Arc<A>, SendError> {
        let identity = identity.running();
        let authority = records::CurrentAuthority::new(&identity, scope, self.session.clone())?;
        let guard = AccountGuard::new(identity, credential, authority)?;
        let supervisor =
            AccountSupervisor::new_with_authority(&guard.identity.owner, Some(guard.clone()))?;
        let account = Arc::new(make(guard, supervisor));
        self.registry
            .register(account.guard().account(), account.client())
            .map_err(|_| SendError::AlreadyReserved)?;
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(account.clone());
        self.changed();
        Ok(account)
    }

    pub(crate) fn runs(&self, account: &ServiceAccount) -> bool {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|running| &running.guard().account() == account)
    }

    /// The directory an account's private state lives in; never named by a
    /// raw account or device label.
    pub(crate) fn private_root(&self, owner: &Owner) -> Result<PathBuf, SendError> {
        let domain = serde_json::to_vec(owner).map_err(|_| SendError::Unavailable)?;
        Ok(self.root.join(blake3::hash(&domain).to_hex().as_str()))
    }

    /// A provider start's consent policy, stopped with the service.
    pub(crate) fn policy(&self) -> consent::Policy {
        consent::Policy {
            cancellation: ServiceCancellation::for_owner(Arc::downgrade(&self.lifetime)),
            session: self.session.clone(),
        }
    }

    /// The workspace's accounts record now. Blocks.
    pub(crate) fn person_accounts(&self) -> Result<accounts::person::AccountList, SendError> {
        match self
            .session
            .call_blocking(session::Call::Person, Duration::from_secs(5))
        {
            Some(session::Reply::Person { accounts, .. }) => {
                <accounts::person::AccountList as tilo_resource::Model>::from_json(&accounts)
                    .map_err(|_| SendError::Unavailable)
            }
            _ => Err(SendError::Unavailable),
        }
    }

    /// Publish an approved account into the person's accounts, while its
    /// owner is admitted. Blocks.
    pub(crate) fn publish(
        &self,
        identity: &Identity,
        publication: session::Publication,
    ) -> Result<(), SendError> {
        identity.check()?;
        match self
            .session
            .call_blocking(session::Call::Publish(publication), Duration::from_secs(10))
        {
            Some(session::Reply::Done) => {}
            _ => return Err(SendError::Revoked),
        }
        identity.check()
    }

    /// The session became usable or stopped being so.
    fn session(self: &Arc<Self>, active: bool, generation: u64) {
        self.ownership.session(active, generation);
        if active && !self.restored.swap(true, Ordering::AcqRel) {
            let service = self.clone();
            self.runtime.spawn(async move {
                let worker = service.clone();
                let Ok(Ok(store)) =
                    tokio::task::spawn_blocking(move || worker.person_accounts()).await
                else {
                    service.restored.store(false, Ordering::Release);
                    return;
                };
                service.flows.signal.clone().restore(store.clone()).await;
                service.flows.whatsapp.clone().restore(store.clone()).await;
                service.flows.sms.clone().restore(store).await;
            });
        }
        self.changed();
    }

    /// Stop every account and enrollment; the session closed.
    pub fn stop(&self) {
        self.flows.signal.retire_all();
        self.flows.whatsapp.retire_all();
        self.flows.sms.retire_all();
        for account in self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            account.retire();
        }
    }

    fn prepare(&self, service: &str) -> Result<EnrollmentReview, SendError> {
        match service {
            "whatsapp" => self.flows.whatsapp.prepare(),
            _ => Err(SendError::InvalidRequest),
        }
    }

    async fn begin(&self, service: &str) -> Result<Pairing, SendError> {
        match service {
            "signal" => self.flows.signal.clone().begin().await,
            "whatsapp" => self.flows.whatsapp.clone().begin().await,
            _ => Err(SendError::InvalidRequest),
        }
    }

    async fn finish(
        &self,
        account: ServiceAccount,
        attempt: String,
    ) -> Result<EnrollmentReview, SendError> {
        match account.service.as_str() {
            "signal" => self.flows.signal.clone().finish(account, attempt).await,
            "whatsapp" => self.flows.whatsapp.clone().finish(account, attempt).await,
            _ => Err(SendError::InvalidRequest),
        }
    }

    async fn review(&self, account: ServiceAccount, name: String) -> Result<(), SendError> {
        match account.service.as_str() {
            "signal" => self.flows.signal.clone().review(account, name).await,
            "whatsapp" => self.flows.whatsapp.clone().review(account, name).await,
            "sms" => self.flows.sms.clone().review(account, name).await,
            _ => Err(SendError::InvalidRequest),
        }
    }

    fn cancel(&self, account: &ServiceAccount) {
        match account.service.as_str() {
            "signal" => self.flows.signal.cancel(account),
            "whatsapp" => self.flows.whatsapp.cancel(account),
            "sms" => self.flows.sms.cancel(account),
            _ => {}
        }
    }
}

/// The session's private runtime directory, where providers run.
pub(crate) fn runtime_dir() -> Result<PathBuf, SendError> {
    tilo_paths::xdg::runtime_dir().ok_or(SendError::Unavailable)
}

/// What the service answers the compositor over the session.
pub struct SessionHandler {
    service: OnceLock<Weak<Service>>,
    /// The session's state as last reported, kept for a service bound after.
    session: Mutex<Option<(bool, u64)>>,
}

impl SessionHandler {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            service: OnceLock::new(),
            session: Mutex::new(None),
        })
    }

    pub fn bind(&self, service: &Arc<Service>) {
        let _ = self.service.set(Arc::downgrade(service));
        let reported = *self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((active, generation)) = reported {
            service.session(active, generation);
        }
    }

    fn service(&self) -> Option<Arc<Service>> {
        self.service.get().and_then(Weak::upgrade)
    }
}

impl Handler<session::Call, session::Reply, session::Event> for SessionHandler {
    fn call(
        &self,
        _call: session::Call,
        reply: Responder<session::Call, session::Reply, session::Event>,
    ) {
        if self.service().is_none() {
            reply.send(session::Reply::Unavailable);
            return;
        }
        reply.send(session::Reply::Refused);
    }

    fn event(&self, event: session::Event) {
        let session::Event::Session { active, generation } = event;
        *self.session.lock().unwrap_or_else(PoisonError::into_inner) = Some((active, generation));
        if let Some(service) = self.service() {
            service.session(active, generation);
        }
    }

    fn closed(&self) {
        // The compositor that started the service is gone: so is the session.
        if let Some(service) = self.service() {
            service.stop();
        }
        std::process::exit(0);
    }
}

/// What the service answers Messages, the one app it serves.
struct AppHandler {
    service: Weak<Service>,
}

impl Handler<wire::Call, wire::Reply, wire::Event> for AppHandler {
    fn call(&self, call: wire::Call, reply: Responder<wire::Call, wire::Reply, wire::Event>) {
        let Some(service) = self.service.upgrade() else {
            reply.send(wire::Reply::Refused);
            return;
        };
        let runtime = service.runtime.clone();
        runtime.spawn(async move { reply.send(answer(&service, call).await) });
    }

    fn event(&self, _: wire::Event) {}
}

async fn answer(service: &Arc<Service>, call: wire::Call) -> wire::Reply {
    use wire::{Call, GroupCall, Reply};
    let messaging = &service.messaging;
    match call {
        Call::Prepare(kind) => Reply::Review(service.prepare(&kind)),
        Call::Begin(kind) => Reply::Pairing(service.begin(&kind).await),
        Call::Finish(account, attempt) => Reply::Review(service.finish(account, attempt).await),
        Call::Review(account, name) => Reply::Reviewed(service.review(account, name).await),
        Call::Cancel(account) => {
            service.cancel(&account);
            Reply::Done
        }
        Call::SimChoices => Reply::Choices(service.flows.sms.clone().choices().await),
        Call::SelectSim(selection) => Reply::Review(service.flows.sms.select(&selection)),
        Call::CancelSimSelection => {
            service.flows.sms.cancel_choices();
            Reply::Done
        }
        Call::Send(request) => Reply::Sent(messaging.send(request).await),
        Call::History {
            route,
            cursor,
            limit,
        } => {
            if limit == tilo_messaging::MAX_HISTORY_MESSAGES {
                Reply::History(messaging.history().read(route, cursor).await)
            } else {
                Reply::History(Err(tilo_messaging::HistoryError::InvalidPage))
            }
        }
        Call::Group(account, call) => {
            let Some(groups) = messaging.groups(&account) else {
                return Reply::Refused;
            };
            match call {
                GroupCall::List => Reply::Groups(groups.list().await),
                GroupCall::Read(group) => Reply::Membership(groups.read(group).await),
                GroupCall::Change(request) => Reply::GroupReceipt(groups.change(request).await),
                GroupCall::Send(request) => Reply::GroupReceipt(groups.send(request).await),
                GroupCall::History(group) => Reply::GroupHistory(groups.history(group).await),
            }
        }
        Call::SetupBegin(account) => match messaging.setup(&account) {
            Some(setup) => Reply::Pairing(setup.begin().await),
            None => Reply::Pairing(Err(SendError::Unavailable)),
        },
        Call::SetupFinish(account, attempt) => match messaging.setup(&account) {
            Some(setup) => Reply::Linked(setup.finish(attempt).await),
            None => Reply::Linked(Err(SendError::Unavailable)),
        },
        Call::SetupCancel(account) => {
            if let Some(setup) = messaging.setup(&account) {
                setup.cancel();
            }
            Reply::Done
        }
    }
}
