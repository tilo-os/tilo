//! Account enrollment, once for every service.
//!
//! Every enrollment admits a new account owner, has the person review the
//! account, keeps a fresh key for it in their workspace vault, publishes it
//! into their accounts and starts it. Services differ only in the order of
//! those steps and in how they link: Signal links a device before the person
//! reviews it, `WhatsApp` starts first and links on the running SDK, and SMS
//! starts from a SIM the person chooses. A [`Provider`] says which ([`Order`]) and supplies those
//! steps; this module runs them, one enrollment per service at a time,
//! with the same checks, phases, cancellation and restart for all.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use launcher::owned_service::ServiceCancellation;
use tilo_messaging::{
    EnrollmentPhase, EnrollmentReview, LinkedAccount, Pairing, SendError, ServiceAccount, SimChoice,
};
use tilo_messaging_protocol::journal::Owner;

use super::Service;
use super::guard::Running;
use super::identity::{Credential, Identity, service_name};
use crate::wire::EnrollmentView;

/// The order a service's enrollment takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Order {
    /// Link a device, review the verified account, keep its key, publish,
    /// start.
    LinkThenStart,
    /// Review, keep its key, start, link on the running account, review the
    /// verified identity again, publish.
    StartThenLink,
    /// Choose a SIM, review, keep its key, publish, start.
    ChooseThenStart,
}
/// How many enrollments one service remembers.
const MAX_ATTEMPTS: usize = 32;

/// What [`Provider::approved`] started: the account, the scope it runs
/// under, and what the enrollment still holds.
pub(crate) type Approved<P> = (
    Arc<<P as Provider>::Account>,
    String,
    Option<<P as Provider>::Held>,
);

/// What one service brings to the flow. Every hook may block: the flow
/// runs them on blocking workers.
pub(crate) trait Provider: Send + Sync + 'static {
    /// What an enrollment keeps between its steps (a link in progress, a
    /// chosen SIM).
    type Held: Send + 'static;
    type Account: Running + 'static;

    const SERVICE: tilo_messaging_protocol::Service;
    /// The site the account's key is kept for in the vault.
    const ORIGIN: &'static str;
    const ORDER: Order;

    /// The title of the vault entry that keeps the key of the account the
    /// person named `name`.
    fn key_title(name: &str) -> String;

    /// Whether a new enrollment may start now.
    fn admits(&self, _service: &Service) -> Result<(), SendError> {
        Ok(())
    }

    /// The number the first review shows, before any link.
    fn number(_identity: &Identity) -> Option<String> {
        None
    }

    /// Begin linking: before the review for [`Order::LinkThenStart`], on the
    /// started `account` for [`Order::StartThenLink`].
    fn begin(
        &self,
        _service: &Arc<Service>,
        _identity: &Identity,
        _held: &mut Option<Self::Held>,
        _account: Option<&Arc<Self::Account>>,
    ) -> Result<Pairing, SendError> {
        Err(SendError::Unavailable)
    }

    /// Finish the link the person completed; the verified number.
    fn finish(
        &self,
        _held: &mut Option<Self::Held>,
        _attempt: String,
    ) -> Result<Option<String>, SendError> {
        Err(SendError::Unavailable)
    }

    /// The SIMs (or other choices) the person may start from, labelled.
    fn choices(
        &self,
        _service: &Arc<Service>,
        _identity: &Identity,
    ) -> Result<Vec<(String, Self::Held)>, SendError> {
        Err(SendError::Unavailable)
    }

    /// The person approved: publish (unless the order publishes later) and
    /// start the account. Returns the account and the scope it runs under.
    fn approved(
        &self,
        service: &Arc<Service>,
        identity: &Identity,
        credential: Credential,
        name: &str,
        held: Option<Self::Held>,
    ) -> Result<Approved<Self>, SendError>;

    /// [`Order::StartThenLink`] — publish the account the person reviewed
    /// again after linking.
    fn confirm(
        &self,
        _service: &Arc<Service>,
        _identity: &Identity,
        _name: &str,
        _held: &Self::Held,
        _account: &Arc<Self::Account>,
    ) -> Result<(), SendError> {
        Err(SendError::Unavailable)
    }

    /// The scope a published account row records, when it is this
    /// service's and well formed.
    fn restored_scope(id: &str, row: &accounts::Account) -> Option<String> {
        let scope = format!("{}:{id}", service_name(Self::SERVICE));
        (row.config
            .get("messaging_scope")
            .and_then(serde_json::Value::as_str)
            == Some(scope.as_str()))
        .then_some(scope)
    }

    /// Whether the published accounts may be started (Tilo keeps one).
    fn restorable(_rows: usize) -> bool {
        true
    }

    /// Start an account published earlier.
    fn resume(
        &self,
        service: &Arc<Service>,
        identity: &Identity,
        credential: Credential,
        scope: String,
        row: &accounts::Account,
    ) -> Result<Arc<Self::Account>, SendError>;

    /// Whether what `held` started still works; an enrollment whose link
    /// broke is retired.
    fn alive(_held: &Self::Held) -> bool {
        true
    }

    /// Stop what `held` started.
    fn cancel(_held: &Self::Held) {}
}

/// Where an enrollment ended, if it did.
#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Open,
    /// The account is published and runs.
    Published,
    Cancelled,
}

struct Attempt<P: Provider> {
    identity: Identity,
    review: EnrollmentReview,
    phase: EnrollmentPhase,
    end: End,
    /// A step runs on a worker; no other may start.
    busy: bool,
    held: Option<P::Held>,
    token: Option<String>,
    verified: bool,
    account: Option<Arc<P::Account>>,
    linked: Option<LinkedAccount>,
    cancellation: ServiceCancellation,
}

impl<P: Provider> Attempt<P> {
    fn new(identity: Identity, review: EnrollmentReview, phase: EnrollmentPhase) -> Self {
        Self {
            identity,
            review,
            phase,
            end: End::Open,
            busy: false,
            held: None,
            token: None,
            verified: false,
            account: None,
            linked: None,
            cancellation: ServiceCancellation::default(),
        }
    }

    /// Still in progress: not cancelled and not running as published.
    fn open(&self) -> bool {
        self.end == End::Open
    }

    /// The phase the app sees: a started account is ready only while it
    /// runs, and only once published.
    fn projected(&self) -> EnrollmentPhase {
        use tilo_messaging::AccountState;
        if self.end == End::Cancelled {
            return EnrollmentPhase::Interrupted;
        }
        match self.account.as_ref().map(|account| account.state()) {
            Some(AccountState::Ready) if self.end == End::Published => EnrollmentPhase::Ready,
            Some(AccountState::Locked | AccountState::Failed) => EnrollmentPhase::Interrupted,
            Some(_) if self.phase == EnrollmentPhase::Ready => EnrollmentPhase::Interrupted,
            _ => self.phase,
        }
    }

    /// End the attempt: stop what it started.
    fn retire(&mut self) {
        self.end = End::Cancelled;
        self.cancellation.cancel();
        if let Some(held) = &self.held {
            P::cancel(held);
        }
        if let Some(account) = &self.account {
            account.retire();
        }
        self.phase = EnrollmentPhase::Interrupted;
    }
}

struct Chooser<P: Provider> {
    identity: Identity,
    epoch: u64,
    choices: BTreeMap<String, (String, P::Held)>,
}

struct State<P: Provider> {
    attempts: BTreeMap<ServiceAccount, Attempt<P>>,
    chooser: Option<Chooser<P>>,
    /// Bumped when the person closes the chooser: a slow scan answers
    /// nothing after that.
    epoch: u64,
    running: Vec<Arc<P::Account>>,
}

/// One service's enrollments and the accounts it restarted.
pub(crate) struct Flow<P: Provider> {
    provider: Arc<P>,
    service: Weak<Service>,
    state: Mutex<State<P>>,
}

fn name_valid(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 160 && !name.chars().any(char::is_control)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, SendError> + Send + 'static,
) -> Result<T, SendError> {
    tokio::task::spawn_blocking(move || {
        // A provider that unwinds takes only its own attempt with it.
        tilo_rt::panic_boundary::isolate(work).unwrap_or(Err(SendError::Unavailable))
    })
    .await
    .unwrap_or(Err(SendError::Unavailable))
}

impl<P: Provider> Flow<P> {
    pub(crate) fn new(provider: P, service: Weak<Service>) -> Arc<Self> {
        Arc::new(Self {
            provider: Arc::new(provider),
            service,
            state: Mutex::new(State {
                attempts: BTreeMap::new(),
                chooser: None,
                epoch: 0,
                running: Vec::new(),
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<P>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn service(&self) -> Result<Arc<Service>, SendError> {
        self.service.upgrade().ok_or(SendError::Unavailable)
    }

    fn name() -> &'static str {
        service_name(P::SERVICE)
    }

    fn account_of(owner: &Owner) -> ServiceAccount {
        ServiceAccount::new(Self::name(), &owner.account)
    }

    fn changed(&self) {
        if let Ok(service) = self.service() {
            service.changed();
        }
    }

    /// Admit a new attempt under `phase`, dropping finished ones.
    fn admit(&self, service: &Arc<Service>) -> Result<(ServiceAccount, Identity), SendError> {
        self.provider.admits(service)?;
        let mut state = self.lock();
        if state.attempts.values().any(Attempt::open) {
            return Err(SendError::AlreadyReserved);
        }
        state
            .attempts
            .retain(|_, attempt| attempt.end == End::Published);
        if state.attempts.len() >= MAX_ATTEMPTS {
            return Err(SendError::Unavailable);
        }
        drop(state);
        let identity = service
            .ownership
            .admit(service.ownership.new_owner(P::SERVICE))?;
        let account = Self::account_of(&identity.owner);
        Ok((account, identity))
    }

    fn review_of(identity: &Identity, number: Option<String>) -> EnrollmentReview {
        EnrollmentReview {
            account: Self::account_of(&identity.owner),
            generation: identity.owner.linking_generation,
            scope: format!("{}:{}", Self::name(), identity.owner.account),
            number,
        }
    }

    pub(crate) fn prepare(&self) -> Result<EnrollmentReview, SendError> {
        if P::ORDER != Order::StartThenLink {
            return Err(SendError::Unavailable);
        }
        let service = self.service()?;
        let (account, identity) = self.admit(&service)?;
        let review = Self::review_of(&identity, P::number(&identity));
        self.lock().attempts.insert(
            account,
            Attempt::new(identity, review.clone(), EnrollmentPhase::Review),
        );
        self.changed();
        Ok(review)
    }

    pub(crate) async fn begin(self: Arc<Self>) -> Result<Pairing, SendError> {
        let service = self.service()?;
        let (account, identity, mut held, started) = match P::ORDER {
            Order::LinkThenStart => {
                let (account, identity) = self.admit(&service)?;
                let review = Self::review_of(&identity, None);
                let mut attempt = Attempt::new(identity.clone(), review, EnrollmentPhase::Linking);
                attempt.busy = true;
                self.lock().attempts.insert(account.clone(), attempt);
                self.changed();
                (account, identity, None, None)
            }
            Order::StartThenLink => {
                let mut state = self.lock();
                let (account, attempt) = state
                    .attempts
                    .iter_mut()
                    .find(|(_, attempt)| attempt.open() && attempt.account.is_some())
                    .ok_or(SendError::Unavailable)?;
                if attempt.busy || attempt.verified || attempt.identity.check().is_err() {
                    return Err(SendError::Revoked);
                }
                attempt.busy = true;
                let taken = (
                    account.clone(),
                    attempt.identity.clone(),
                    attempt.held.take(),
                    attempt.account.clone(),
                );
                drop(state);
                taken
            }
            Order::ChooseThenStart => return Err(SendError::Unavailable),
        };
        let provider = self.provider.clone();
        let worker = service.clone();
        let linking = identity.clone();
        let (result, held) = blocking(move || {
            let result = provider.begin(&worker, &linking, &mut held, started.as_ref());
            Ok((result, held))
        })
        .await
        .unwrap_or((Err(SendError::Unavailable), None));
        let mut state = self.lock();
        let Some(attempt) = state.attempts.get_mut(&account) else {
            return Err(SendError::Revoked);
        };
        attempt.busy = false;
        attempt.held = held;
        let outcome = match result {
            Ok(pairing) if attempt.open() => {
                attempt.token = Some(pairing.attempt.clone());
                Ok(pairing)
            }
            Ok(_) => Err(SendError::Revoked),
            Err(error) => Err(error),
        };
        if outcome.is_err() {
            attempt.retire();
        }
        drop(state);
        self.changed();
        outcome
    }

    pub(crate) async fn finish(
        self: Arc<Self>,
        account: ServiceAccount,
        token: String,
    ) -> Result<EnrollmentReview, SendError> {
        self.service()?;
        let mut held = {
            let mut state = self.lock();
            let attempt = state
                .attempts
                .get_mut(&account)
                .filter(|attempt| {
                    attempt.open()
                        && !attempt.busy
                        && !attempt.verified
                        && attempt.token.as_ref() == Some(&token)
                })
                .ok_or(SendError::Revoked)?;
            attempt.identity.check()?;
            attempt.busy = true;
            let held = attempt.held.take();
            drop(state);
            held
        };
        let provider = self.provider.clone();
        let (result, held) = blocking(move || {
            let result = provider.finish(&mut held, token);
            Ok((result, held))
        })
        .await
        .unwrap_or((Err(SendError::Unavailable), None));
        let mut state = self.lock();
        let attempt = state.attempts.get_mut(&account).ok_or(SendError::Revoked)?;
        attempt.busy = false;
        attempt.held = held;
        let outcome = match result {
            Ok(number) if attempt.open() && attempt.identity.check().is_ok() => {
                attempt.review.number = number;
                attempt.verified = true;
                attempt.phase = EnrollmentPhase::Review;
                Ok(attempt.review.clone())
            }
            Ok(_) => Err(SendError::Revoked),
            Err(error) => Err(error),
        };
        if outcome.is_err() {
            attempt.retire();
        }
        drop(state);
        self.changed();
        outcome
    }

    pub(crate) async fn choices(self: Arc<Self>) -> Result<Vec<SimChoice>, SendError> {
        if P::ORDER != Order::ChooseThenStart {
            return Err(SendError::Unavailable);
        }
        let service = self.service()?;
        {
            let mut state = self.lock();
            if state.attempts.values().any(Attempt::open) {
                return Err(SendError::AlreadyReserved);
            }
            state.chooser = None;
        }
        let identity = service
            .ownership
            .admit(service.ownership.new_owner(P::SERVICE))?;
        let epoch = self.lock().epoch;
        let provider = self.provider.clone();
        let worker = service.clone();
        let scanning = identity.clone();
        let found = blocking(move || provider.choices(&worker, &scanning)).await?;
        identity.check()?;
        let mut choices = BTreeMap::new();
        let mut shown = Vec::new();
        for (label, held) in found {
            let token = uuid::Uuid::new_v4().to_string();
            shown.push(SimChoice {
                selection: token.clone(),
                label: label.clone(),
            });
            choices.insert(token, (label, held));
        }
        let mut state = self.lock();
        if state.epoch != epoch || state.chooser.is_some() {
            return Err(SendError::Revoked);
        }
        state.chooser = Some(Chooser {
            identity,
            epoch,
            choices,
        });
        drop(state);
        Ok(shown)
    }

    pub(crate) fn select(&self, token: &str) -> Result<EnrollmentReview, SendError> {
        let mut state = self.lock();
        if state.attempts.values().any(Attempt::open) {
            return Err(SendError::AlreadyReserved);
        }
        let mut chooser = state.chooser.take().ok_or(SendError::Revoked)?;
        chooser.identity.check()?;
        if chooser.epoch != state.epoch {
            return Err(SendError::Revoked);
        }
        let (label, held) = chooser
            .choices
            .remove(token)
            .ok_or(SendError::InvalidRequest)?;
        state
            .attempts
            .retain(|_, attempt| attempt.end == End::Published);
        let review = Self::review_of(&chooser.identity, Some(label));
        let mut attempt = Attempt::new(chooser.identity, review.clone(), EnrollmentPhase::Review);
        attempt.held = Some(held);
        state.attempts.insert(review.account.clone(), attempt);
        drop(state);
        self.changed();
        Ok(review)
    }

    pub(crate) fn cancel_choices(&self) {
        let mut state = self.lock();
        state.epoch = state.epoch.wrapping_add(1);
        state.chooser = None;
    }

    pub(crate) async fn review(
        self: Arc<Self>,
        account: ServiceAccount,
        name: String,
    ) -> Result<(), SendError> {
        if !name_valid(&name) {
            return Err(SendError::InvalidRequest);
        }
        let service = self.service()?;
        let (identity, confirm) = {
            let mut state = self.lock();
            let attempt = state
                .attempts
                .get_mut(&account)
                .filter(|attempt| attempt.open() && !attempt.busy)
                .ok_or(SendError::Revoked)?;
            attempt.identity.check()?;
            let confirm = P::ORDER == Order::StartThenLink && attempt.verified;
            if !confirm {
                if attempt.account.is_some() {
                    return Err(SendError::AlreadyReserved);
                }
                if P::ORDER == Order::LinkThenStart && !attempt.verified
                    || P::ORDER == Order::ChooseThenStart && attempt.held.is_none()
                {
                    return Err(SendError::Revoked);
                }
            }
            attempt.busy = true;
            let identity = attempt.identity.clone();
            drop(state);
            (identity, confirm)
        };
        let result = if confirm {
            self.confirm(&service, &account, identity, name).await
        } else {
            let worker = service.clone();
            let keeping = identity.clone();
            let title = P::key_title(&name);
            match blocking(move || Credential::keep(&worker.session, &keeping, P::ORIGIN, title))
                .await
            {
                Ok(credential) => {
                    self.start(&service, &account, credential, identity, name)
                        .await
                }
                Err(error) => Err(error),
            }
        };
        {
            let mut state = self.lock();
            if let Some(attempt) = state.attempts.get_mut(&account) {
                attempt.busy = false;
                if result.is_err() {
                    attempt.retire();
                }
            }
        }
        self.changed();
        result
    }

    /// [`Order::StartThenLink`] — the person reviewed the linked identity;
    /// publish it.
    async fn confirm(
        &self,
        service: &Arc<Service>,
        account: &ServiceAccount,
        identity: Identity,
        name: String,
    ) -> Result<(), SendError> {
        let (held, started) = {
            let mut state = self.lock();
            let attempt = state.attempts.get_mut(account).ok_or(SendError::Revoked)?;
            let started = attempt.account.clone().ok_or(SendError::Revoked)?;
            let held = attempt.held.take().ok_or(SendError::Revoked)?;
            drop(state);
            (held, started)
        };
        let provider = self.provider.clone();
        let worker = service.clone();
        let (result, held) = blocking(move || {
            let result = provider.confirm(&worker, &identity, &name, &held, &started);
            Ok((result, held))
        })
        .await?;
        let mut state = self.lock();
        let attempt = state.attempts.get_mut(account).ok_or(SendError::Revoked)?;
        attempt.held = Some(held);
        result?;
        attempt.end = End::Published;
        attempt.phase = EnrollmentPhase::Ready;
        attempt.linked = Some(LinkedAccount {
            account: attempt.review.account.clone(),
            generation: attempt.review.generation,
            scope: attempt.review.scope.clone(),
        });
        drop(state);
        Ok(())
    }

    /// Start the account the person reviewed, under the key kept for it.
    async fn start(
        &self,
        service: &Arc<Service>,
        account: &ServiceAccount,
        credential: Credential,
        identity: Identity,
        name: String,
    ) -> Result<(), SendError> {
        let held = self
            .lock()
            .attempts
            .get_mut(account)
            .filter(|attempt| attempt.open())
            .ok_or(SendError::Revoked)?
            .held
            .take();
        let provider = self.provider.clone();
        let worker = service.clone();
        let starting = identity.clone();
        let (started, scope, held) =
            blocking(move || provider.approved(&worker, &starting, credential, &name, held))
                .await?;
        let mut state = self.lock();
        let attempt = state.attempts.get_mut(account).ok_or(SendError::Revoked)?;
        attempt.held = held;
        attempt.account = Some(started);
        attempt.phase = EnrollmentPhase::Linking;
        attempt.linked = Some(LinkedAccount {
            account: attempt.review.account.clone(),
            generation: identity.owner.linking_generation,
            scope,
        });
        let cancelled = attempt.end == End::Cancelled;
        if !cancelled && P::ORDER != Order::StartThenLink {
            attempt.end = End::Published;
        }
        drop(state);
        if cancelled {
            return Err(SendError::Revoked);
        }
        Ok(())
    }

    pub(crate) fn cancel(&self, account: &ServiceAccount) {
        let mut state = self.lock();
        if let Some(attempt) = state.attempts.get_mut(account) {
            attempt.retire();
        }
        drop(state);
        self.changed();
    }

    /// Retire every enrollment whose admission ended.
    pub(crate) fn check(&self) {
        let mut retired = false;
        let mut state = self.lock();
        for attempt in state.attempts.values_mut() {
            if attempt.open()
                && (attempt.identity.check().is_err()
                    || attempt.held.as_ref().is_some_and(|held| !P::alive(held)))
            {
                attempt.retire();
                retired = true;
            }
        }
        if state
            .chooser
            .as_ref()
            .is_some_and(|chooser| chooser.identity.check().is_err())
        {
            state.chooser = None;
        }
        drop(state);
        if retired {
            self.changed();
        }
    }

    pub(crate) fn views(&self) -> Vec<EnrollmentView> {
        self.lock()
            .attempts
            .values()
            .map(|attempt| EnrollmentView {
                account: attempt.review.account.clone(),
                phase: attempt.projected(),
                linked: attempt.linked.clone(),
            })
            .collect()
    }

    /// Start the accounts the person published earlier, each under the key
    /// the vault entry it names keeps.
    pub(crate) async fn restore(self: Arc<Self>, store: accounts::person::AccountList) {
        let Ok(service) = self.service() else {
            return;
        };
        let rows: Vec<accounts::Account> = store
            .of_type(Self::name())
            .into_iter()
            .filter(|row| row.enabled)
            .take(32)
            .collect();
        if !P::restorable(rows.len()) {
            return;
        }
        for row in rows {
            let id = row.id.as_str().to_owned();
            let account = ServiceAccount::new(Self::name(), &id);
            if service.runs(&account) {
                continue;
            }
            let Some(generation) = row
                .config
                .get("linking_generation")
                .and_then(serde_json::Value::as_u64)
                .filter(|generation| *generation > 0)
            else {
                continue;
            };
            let Some(scope) = P::restored_scope(&id, &row) else {
                continue;
            };
            let Some(item) = accounts::email::password_item(&row.config).map(str::to_owned) else {
                continue;
            };
            if uuid::Uuid::parse_str(&id)
                .map(|parsed| parsed.to_string())
                .ok()
                != Some(id.clone())
            {
                continue;
            }
            let owner = Owner {
                actor: service.ownership.actor().into(),
                device: service.ownership.device().into(),
                service: P::SERVICE,
                account: id,
                linking_generation: generation,
            };
            let provider = self.provider.clone();
            let worker = service.clone();
            let resumed = blocking(move || {
                let identity = worker.ownership.admit(owner)?.running();
                let credential = Credential::kept(&worker.session, &identity, item)?;
                provider.resume(&worker, &identity, credential, scope, &row)
            })
            .await;
            match resumed {
                Ok(account) => self.lock().running.push(account),
                Err(error) => {
                    tracing::info!(target: "tilo::messaging", service = Self::name(), ?error, "a published account did not start");
                }
            }
        }
        self.changed();
    }

    /// Stop every account and enrollment.
    pub(crate) fn retire_all(&self) {
        let mut state = self.lock();
        for account in state.running.drain(..) {
            account.retire();
        }
        for attempt in state.attempts.values_mut() {
            attempt.retire();
        }
    }
}
