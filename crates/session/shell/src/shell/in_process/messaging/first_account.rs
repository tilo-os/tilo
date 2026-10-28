//! The original UI owner accepts worker evidence, asks the protected Passwords
//! owner for a decision, publishes canonical metadata and starts the provider.
use super::working_profile::provision::{self, UnpublishedSignalProfile};
use super::{
    Arc, NativeMessagingCredential, NativeMessagingIdentity, NativeSignalEnrollment,
    NativeVerifiedSignalEnrollment, Owner, SendError,
};
use crate::session_runtime::{PendingStart, Wake, runtime, start_blocking};
use launcher::owned_service::ServiceCancellation;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{Mutex, mpsc},
    time::Duration,
};
use tilo_messaging::{
    EnrollmentApprovalFuture, EnrollmentClient, EnrollmentHost, EnrollmentPhase, EnrollmentReview,
    EnrollmentReviewFuture, LinkedAccount, Pairing, PairingFuture, ServiceAccount,
};
use tokio::sync::oneshot;
const ORIGIN: &str = "https://signal.org";
const CAPACITY: usize = 8;
const ANSWER_TIMEOUT: Duration = Duration::from_secs(90);

type Reply<T> = oneshot::Sender<Result<T, SendError>>;
enum Request {
    Begin(String, Reply<Pairing>),
    Finish(ServiceAccount, String, Reply<EnrollmentReview>),
    Review(ServiceAccount, String, Reply<String>),
}
#[derive(Clone)]
struct Status {
    phase: EnrollmentPhase,
    linked: Option<LinkedAccount>,
    cancellation: ServiceCancellation,
}
struct Port {
    principal: String,
    requests: mpsc::SyncSender<Request>,
    statuses: Arc<Mutex<BTreeMap<ServiceAccount, Status>>>,
    wake: Wake,
}
impl EnrollmentHost for Port {
    fn begin(&self, service: String) -> PairingFuture {
        super::ask_owner(&self.requests, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Begin(service, reply)
        })
    }
    fn finish(&self, account: ServiceAccount, attempt: String) -> EnrollmentReviewFuture {
        super::ask_owner(&self.requests, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Finish(account, attempt, reply)
        })
    }
    fn review(&self, account: ServiceAccount, name: String) -> EnrollmentApprovalFuture {
        super::ask_owner(&self.requests, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Review(account, name, reply)
        })
    }
    fn phase(&self, account: &ServiceAccount) -> EnrollmentPhase {
        self.statuses
            .lock()
            .ok()
            .and_then(|states| states.get(account).map(|state| state.phase))
            .unwrap_or(EnrollmentPhase::Empty)
    }
    fn linked(&self, account: &ServiceAccount) -> Option<LinkedAccount> {
        self.statuses.lock().ok()?.get(account)?.linked.clone()
    }
    fn cancel(&self, account: &ServiceAccount) {
        let cancellation = self
            .statuses
            .lock()
            .ok()
            .and_then(|states| states.get(account).map(|state| state.cancellation.clone()));
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
        }
        (self.wake)();
    }
}
struct Begun {
    service_stop: ServiceCancellation,
    enrollment: Arc<NativeSignalEnrollment>,
    stopped: mpsc::Receiver<Result<UnpublishedSignalProfile, SendError>>,
    pairing: Pairing,
}
struct Completed {
    candidate: NativeVerifiedSignalEnrollment,
    profile: UnpublishedSignalProfile,
}
struct Permission {
    completed: Completed,
    asked: super::OwnerPermission,
    name: String,
}
struct PreparedAccount {
    account: Arc<super::NativeSignalAccount>,
    storage: super::provider::NativeSignalStorage,
    cancellation: ServiceCancellation,
}
type BeginPending = (PendingStart<Result<Begun, SendError>>, Reply<Pairing>);
type ActiveEnrollment = (
    Arc<NativeSignalEnrollment>,
    mpsc::Receiver<Result<UnpublishedSignalProfile, SendError>>,
    ServiceCancellation,
);
type FinishPending = (
    PendingStart<Result<Completed, SendError>>,
    Reply<EnrollmentReview>,
);
struct Attempt {
    identity: NativeMessagingIdentity,
    cancellation: ServiceCancellation,
    begin: Option<BeginPending>,
    begun: Option<ActiveEnrollment>,
    finish: Option<FinishPending>,
    completed: Option<Completed>,
    permission: Option<Permission>,
    token: Option<String>,
    published: bool,
    seal: Option<PendingStart<Result<PreparedAccount, SendError>>>,
}
pub(in crate::shell::in_process) trait FirstAccounts {
    fn client(&self) -> EnrollmentClient;
    fn pump(&self, apps: &mut super::super::InProcessApps);
}
pub(super) trait Source: Send + Sync {
    fn provision(
        &self,
        identity: NativeMessagingIdentity,
        root: &std::path::Path,
        policy: super::network::Policy,
    ) -> Result<provision::Provisioning, SendError>;
    fn activate(
        &self,
        account: &Arc<super::NativeSignalAccount>,
        wake: Wake,
        storage: super::provider::NativeSignalStorage,
        policy: super::network::Policy,
    ) -> Result<super::NativeSignalActivation, SendError>;
}
struct SignedSource;
impl Source for SignedSource {
    fn provision(
        &self,
        identity: NativeMessagingIdentity,
        root: &std::path::Path,
        policy: super::network::Policy,
    ) -> Result<provision::Provisioning, SendError> {
        provision::provision(identity, root, &policy)
    }
    fn activate(
        &self,
        account: &Arc<super::NativeSignalAccount>,
        wake: Wake,
        storage: super::provider::NativeSignalStorage,
        policy: super::network::Policy,
    ) -> Result<super::NativeSignalActivation, SendError> {
        account.activate_default_on_worker(wake, storage, policy)
    }
}
struct Actor {
    source: Arc<dyn Source>,
    requests: mpsc::Receiver<Request>,
    port: Arc<Port>,
    attempt: RefCell<Option<Attempt>>,
}
pub(in crate::shell::in_process) fn new(principal: String, wake: Wake) -> Rc<dyn FirstAccounts> {
    let signal = with_source(principal.clone(), wake.clone(), Arc::new(SignedSource));
    let sms = super::sms_first::new(principal.clone(), wake.clone());
    let whatsapp = super::whatsapp_first::new(principal.clone(), wake.clone());
    let native = super::native_first::new(principal.clone(), wake);
    combine_all(principal, signal, sms, whatsapp, Some(native))
}
#[cfg(test)]
fn combine(
    principal: String,
    signal: Rc<dyn FirstAccounts>,
    sms: Rc<dyn FirstAccounts>,
    whatsapp: Rc<dyn FirstAccounts>,
) -> Rc<dyn FirstAccounts> {
    combine_all(principal, signal, sms, whatsapp, None)
}
fn combine_all(
    principal: String,
    signal: Rc<dyn FirstAccounts>,
    sms: Rc<dyn FirstAccounts>,
    whatsapp: Rc<dyn FirstAccounts>,
    native: Option<Rc<dyn FirstAccounts>>,
) -> Rc<dyn FirstAccounts> {
    Rc::new(Combined {
        port: Arc::new(CombinedPort {
            principal,
            signal: signal.client(),
            sms: sms.client(),
            whatsapp: whatsapp.client(),
            native: native.as_ref().map(|owner| owner.client()),
        }),
        signal,
        sms,
        whatsapp,
        native,
    })
}
fn with_source(principal: String, wake: Wake, source: Arc<dyn Source>) -> Rc<dyn FirstAccounts> {
    let (sender, requests) = mpsc::sync_channel(CAPACITY);
    Rc::new(Actor {
        source,
        requests,
        port: Arc::new(Port {
            principal,
            requests: sender,
            statuses: Arc::default(),
            wake,
        }),
        attempt: RefCell::new(None),
    })
}
impl FirstAccounts for Actor {
    fn client(&self) -> EnrollmentClient {
        EnrollmentClient::new(self.port.principal.clone(), self.port.clone())
    }
    fn pump(&self, apps: &mut super::super::InProcessApps) {
        // A reset can cancel and then enqueue a fresh Begin before this pump.
        // Retire the old owned attempt before admitting the explicit new one.
        {
            let mut slot = self.attempt.borrow_mut();
            if let Some(attempt) = slot.as_mut()
                && (attempt.identity.check().is_err() || attempt.cancellation.is_cancelled())
            {
                self.retire(apps, attempt);
                slot.take();
            }
        }
        for request in self.requests.try_iter().take(CAPACITY) {
            self.request(apps, request);
        }
        let mut slot = self.attempt.borrow_mut();
        let Some(attempt) = slot.as_mut() else {
            return;
        };
        if attempt.identity.check().is_err() || attempt.cancellation.is_cancelled() {
            self.retire(apps, attempt);
            slot.take();
            return;
        }
        Self::poll_begin(attempt);
        self.poll_finish(attempt);
        self.poll_seal(apps, attempt);
        if let Some(permission) = attempt.permission.take() {
            self.permission(apps, attempt, permission);
        }
        if attempt.published {
            let identity = service_account(&attempt.identity.owner);
            if let Some(client) = &apps.messaging {
                match client.account_state(&identity) {
                    tilo_messaging::AccountState::Ready => {
                        self.phase(&identity, EnrollmentPhase::Ready);
                    }
                    tilo_messaging::AccountState::Locked
                    | tilo_messaging::AccountState::Failed
                    | tilo_messaging::AccountState::Unavailable => {
                        self.phase(&identity, EnrollmentPhase::Interrupted);
                    }
                    _ => {}
                }
            }
        }
    }
}
fn service_account(owner: &Owner) -> ServiceAccount {
    ServiceAccount::new("signal", &owner.account)
}
impl Actor {
    fn phase(&self, account: &ServiceAccount, phase: EnrollmentPhase) {
        let changed = self.port.statuses.lock().ok().is_some_and(|mut states| {
            let Some(state) = states.get_mut(account) else {
                return false;
            };
            if state.phase == phase {
                return false;
            }
            state.phase = phase;
            true
        });
        if changed {
            (self.port.wake)();
        }
    }
    fn request(&self, apps: &super::super::InProcessApps, request: Request) {
        match request {
            Request::Begin(service, reply) => self.begin(apps, &service, reply),
            Request::Finish(account, attempt, reply) => self.finish(&account, attempt, reply),
            Request::Review(account, name, reply) => self.review(apps, &account, name, reply),
        }
    }
    fn begin(&self, apps: &super::super::InProcessApps, service: &str, reply: Reply<Pairing>) {
        if service != "signal" {
            let _ = reply.send(Err(SendError::Unavailable));
            return;
        }
        let mut slot = self.attempt.borrow_mut();
        if slot.is_some() {
            let _ = reply.send(Err(SendError::AlreadyReserved));
            return;
        }
        let result = (|| {
            let owner = Owner {
                principal: apps.author.0.clone(),
                device: apps.device_id.clone(),
                service: tilo_messaging_protocol::Service::Signal,
                account: uuid::Uuid::new_v4().to_string(),
                linking_generation: 1,
            };
            let identity = apps
                .native_messaging_identity(owner)
                .map_err(|_| SendError::Revoked)?;
            let root = tilo_paths::xdg::runtime_dir().ok_or(SendError::Unavailable)?;
            let cancellation =
                ServiceCancellation::for_owner(Arc::downgrade(&apps.messaging_lifetime));
            let original = identity.clone();
            let service = cancellation.child();
            let executor = runtime().map_err(|_| SendError::Unavailable)?;
            let source = self.source.clone();
            let prompts = apps.messaging_network_prompts.clone();
            let worker = start_blocking(executor.handle(), self.port.wake.clone(), move || {
                Ok((|| {
                    let provision = source.provision(
                        original.clone(),
                        &root,
                        super::network::Policy {
                            cancellation: service.clone(),
                            prompts,
                        },
                    )?;
                    let enrollment = Arc::new(NativeSignalEnrollment::from_original(
                        original,
                        provision.stream,
                    )?);
                    let private = executor.handle().block_on(enrollment.begin())?;
                    let pairing = Pairing::new(
                        service_account(enrollment.owner()),
                        private.attempt.clone(),
                        enrollment.owner().linking_generation,
                        zeroize::Zeroizing::new(private.qr_bytes().to_vec()),
                    );
                    Ok(Begun {
                        enrollment,
                        stopped: provision.stopped,
                        pairing,
                        service_stop: service,
                    })
                })())
            });
            Ok((identity, cancellation, worker))
        })();
        match result {
            Ok((identity, cancellation, worker)) => {
                if let Ok(mut states) = self.port.statuses.lock() {
                    states.clear();
                    states.insert(
                        service_account(&identity.owner),
                        Status {
                            phase: EnrollmentPhase::Linking,
                            linked: None,
                            cancellation: cancellation.clone(),
                        },
                    );
                }
                *slot = Some(Attempt {
                    identity,
                    cancellation,
                    begin: Some((worker, reply)),
                    begun: None,
                    finish: None,
                    completed: None,
                    permission: None,
                    token: None,
                    published: false,
                    seal: None,
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
        drop(slot);
    }
    fn poll_begin(attempt: &mut Attempt) {
        let Some((pending, reply)) = &attempt.begin else {
            return;
        };
        if reply.is_closed() {
            attempt.cancellation.cancel();
            return;
        }
        let Some(outcome) = pending.poll() else {
            return;
        };
        let Some((_, reply)) = attempt.begin.take() else {
            return;
        };
        match outcome.unwrap_or(Err(SendError::Unavailable)) {
            Ok(begun) => {
                attempt.token = Some(begun.pairing.attempt.clone());
                attempt.begun = Some((begun.enrollment, begun.stopped, begun.service_stop));
                if reply.send(Ok(begun.pairing)).is_err() {
                    attempt.cancellation.cancel();
                }
            }
            Err(error) => {
                attempt.cancellation.cancel();
                let _ = reply.send(Err(error));
            }
        }
    }
    fn finish(&self, account: &ServiceAccount, token: String, reply: Reply<EnrollmentReview>) {
        let mut slot = self.attempt.borrow_mut();
        let Some(attempt) = slot.as_mut().filter(|attempt| {
            service_account(&attempt.identity.owner) == *account
                && attempt.token.as_ref() == Some(&token)
                && attempt.finish.is_none()
        }) else {
            let _ = reply.send(Err(SendError::Revoked));
            return;
        };
        let Some((enrollment, stopped, service_stop)) = attempt.begun.take() else {
            let _ = reply.send(Err(SendError::AlreadyReserved));
            return;
        };
        let Ok(executor) = runtime() else {
            attempt.cancellation.cancel();
            let _ = reply.send(Err(SendError::Unavailable));
            return;
        };
        let worker = start_blocking(executor.handle(), self.port.wake.clone(), move || {
            Ok((|| {
                let candidate = executor.handle().block_on(enrollment.finish(token))?;
                service_stop.cancel();
                let profile = stopped
                    .recv_timeout(Duration::from_secs(25))
                    .map_err(|_| SendError::Unavailable)??;
                candidate.identity.check()?;
                Ok(Completed { candidate, profile })
            })())
        });
        attempt.finish = Some((worker, reply));
    }
    fn poll_finish(&self, attempt: &mut Attempt) {
        let Some((pending, reply)) = &attempt.finish else {
            return;
        };
        if reply.is_closed() {
            attempt.cancellation.cancel();
            return;
        }
        let Some(outcome) = pending.poll() else {
            return;
        };
        let Some((_, reply)) = attempt.finish.take() else {
            return;
        };
        match outcome.unwrap_or(Err(SendError::Unavailable)) {
            Ok(completed) => {
                let result = EnrollmentReview {
                    account: service_account(completed.candidate.owner()),
                    generation: completed.candidate.owner().linking_generation,
                    scope: completed.candidate.account().scope(),
                    number: completed.candidate.account().number.clone(),
                };
                self.phase(&result.account, EnrollmentPhase::Review);
                attempt.completed = Some(completed);
                if reply.send(Ok(result)).is_err() {
                    attempt.cancellation.cancel();
                }
            }
            Err(error) => {
                attempt.cancellation.cancel();
                let _ = reply.send(Err(error));
            }
        }
    }
    fn review(
        &self,
        apps: &super::super::InProcessApps,
        account: &ServiceAccount,
        name: String,
        reply: Reply<String>,
    ) {
        let mut slot = self.attempt.borrow_mut();
        let Some(attempt) = slot
            .as_mut()
            .filter(|attempt| service_account(&attempt.identity.owner) == *account)
        else {
            let _ = reply.send(Err(SendError::Revoked));
            return;
        };
        let Some(completed) = attempt.completed.take() else {
            let _ = reply.send(Err(SendError::AlreadyReserved));
            return;
        };
        let result = apps
            .prepare_native_signal_profile_key(&completed.candidate, ORIGIN)
            .map_err(|_| SendError::Revoked)
            .and_then(|item_id| {
                super::OwnerPermission::ask(
                    apps,
                    &attempt.identity,
                    item_id,
                    "Private Signal messaging".into(),
                    ORIGIN,
                )
            });
        match result {
            Ok(asked) => {
                self.phase(account, EnrollmentPhase::AwaitingPermission);
                let proposal = asked.proposal.clone();
                attempt.permission = Some(Permission {
                    completed,
                    asked,
                    name,
                });
                if reply.send(Ok(proposal)).is_err() {
                    attempt.cancellation.cancel();
                }
            }
            Err(error) => {
                attempt.cancellation.cancel();
                let _ = reply.send(Err(error));
            }
        }
    }
    fn permission(
        &self,
        apps: &mut super::super::InProcessApps,
        attempt: &mut Attempt,
        permission: Permission,
    ) {
        let credential = match permission.asked.grant(apps, &attempt.identity) {
            super::Grant::Approved(credential) => credential,
            super::Grant::Pending => {
                attempt.permission = Some(permission);
                return;
            }
            super::Grant::Refused => {
                permission.asked.withdraw(apps);
                attempt.cancellation.cancel();
                return;
            }
        };
        if self
            .activate(apps, attempt, permission, credential)
            .is_err()
        {
            attempt.cancellation.cancel();
        }
    }
    fn activate(
        &self,
        apps: &mut super::super::InProcessApps,
        attempt: &mut Attempt,
        permission: Permission,
        credential: NativeMessagingCredential,
    ) -> Result<(), SendError> {
        let identity = attempt.identity.clone();
        let linked = apps
            .publish_native_signal_enrollment(permission.completed.candidate, permission.name)
            .map_err(|_| SendError::Revoked)?;
        attempt.published = true;
        let authority = apps
            .native_messaging_authority(&identity, &credential, &linked.scope)
            .map_err(|_| SendError::Revoked)?;
        let account = apps
            .prepare_signal_account(super::SignalAccountStartup {
                identity: identity.clone(),
                credential,
                authority,
            })
            .map_err(|_| SendError::Revoked)?;
        let root = apps
            .native_messaging_root
            .as_ref()
            .ok_or(SendError::Unavailable)?;
        let domain = serde_json::to_vec(&identity.owner).map_err(|_| SendError::Unavailable)?;
        drop(identity);
        let private = root.join(blake3::hash(&domain).to_hex().as_str());
        let runtime_root = tilo_paths::xdg::runtime_dir().ok_or(SendError::Unavailable)?;
        let cancellation = ServiceCancellation::for_owner(Arc::downgrade(&apps.messaging_lifetime));
        let executor = runtime().map_err(|_| SendError::Unavailable)?;
        let signal_account = linked
            .scope
            .strip_prefix("signal:")
            .ok_or(SendError::Revoked)?
            .to_owned();
        let pending = start_blocking(executor.handle(), self.port.wake.clone(), move || {
            Ok((|| {
                tilo_files::native::private::directory(&private)
                    .map_err(|_| SendError::Unavailable)?;
                permission
                    .completed
                    .profile
                    .seal(&account, &private.join("profile"))?;
                let storage = super::provider::NativeSignalStorage {
                    profile: private.join("profile"),
                    history: private.join("history"),
                    runtime: runtime_root,
                    signal_account,
                };
                Ok(PreparedAccount {
                    account,
                    storage,
                    cancellation,
                })
            })())
        });
        attempt.seal = Some(pending);
        if let Ok(mut states) = self.port.statuses.lock()
            && let Some(state) = states.get_mut(&linked.account)
        {
            state.linked = Some(linked);
        }
        Ok(())
    }
    fn poll_seal(&self, apps: &mut super::super::InProcessApps, attempt: &mut Attempt) {
        let Some(outcome) = attempt.seal.as_ref().and_then(PendingStart::poll) else {
            return;
        };
        attempt.seal.take();
        let result = (|| {
            let PreparedAccount {
                account,
                storage,
                cancellation,
            } = outcome.map_err(|_| SendError::Unavailable)??;
            attempt.identity.check()?;
            if attempt.cancellation.is_cancelled() {
                return Err(SendError::Revoked);
            }
            let wake = apps
                .messaging_record_wake
                .clone()
                .ok_or(SendError::Unavailable)?;
            if apps.messaging_starts.is_none() {
                apps.messaging_starts = Some(super::activation::starts());
            }
            let starts = apps
                .messaging_starts
                .as_ref()
                .ok_or(SendError::Unavailable)?;
            if !starts.available() {
                return Err(SendError::Unavailable);
            }
            starts
                .push(self.source.activate(
                    &account,
                    wake,
                    storage,
                    super::network::Policy {
                        cancellation: cancellation.clone(),
                        prompts: apps.messaging_network_prompts.clone(),
                    },
                )?)
                .inspect_err(|_| cancellation.cancel())?;
            apps.messaging_providers.push(cancellation);
            apps.messaging_native_accounts.push(account);
            Ok::<_, SendError>(())
        })();
        if result.is_err() {
            attempt.cancellation.cancel();
        }
    }
    fn retire(&self, apps: &super::super::InProcessApps, attempt: &mut Attempt) {
        attempt.cancellation.cancel();
        if let Some((enrollment, _, service)) = &attempt.begun {
            enrollment.cancel();
            service.cancel();
        }
        if let Some(permission) = attempt.permission.take() {
            permission.asked.withdraw(apps);
        }
        self.phase(
            &service_account(&attempt.identity.owner),
            EnrollmentPhase::Interrupted,
        );
    }
}
#[cfg(test)]
pub(super) fn fixture(
    principal: String,
    wake: Wake,
    source: Arc<dyn Source>,
) -> Rc<dyn FirstAccounts> {
    with_source(principal, wake, source)
}

struct Combined {
    native: Option<Rc<dyn FirstAccounts>>,
    port: Arc<CombinedPort>,
    signal: Rc<dyn FirstAccounts>,
    sms: Rc<dyn FirstAccounts>,
    whatsapp: Rc<dyn FirstAccounts>,
}
struct CombinedPort {
    native: Option<EnrollmentClient>,
    principal: String,
    signal: EnrollmentClient,
    sms: EnrollmentClient,
    whatsapp: EnrollmentClient,
}
impl CombinedPort {
    fn account(&self, account: &ServiceAccount) -> &EnrollmentClient {
        match account.service.as_str() {
            "sms" => &self.sms,
            "whatsapp" => &self.whatsapp,
            "tilo" => self.native.as_ref().unwrap_or(&self.signal),
            _ => &self.signal,
        }
    }
}
impl FirstAccounts for Combined {
    fn client(&self) -> EnrollmentClient {
        EnrollmentClient::new(self.port.principal.clone(), self.port.clone())
    }
    fn pump(&self, apps: &mut super::super::InProcessApps) {
        self.signal.pump(apps);
        self.sms.pump(apps);
        self.whatsapp.pump(apps);
        if let Some(native) = &self.native {
            native.pump(apps);
        }
    }
}
impl EnrollmentHost for CombinedPort {
    fn prepare(&self, service: String) -> EnrollmentReviewFuture {
        let whatsapp = self.whatsapp.clone();
        let native = self.native.clone();
        Box::pin(async move {
            match service.as_str() {
                "whatsapp" => whatsapp.prepare(service).await,
                "tilo" => native.ok_or(SendError::Unavailable)?.prepare(service).await,
                _ => Err(SendError::Unavailable),
            }
        })
    }
    fn begin(&self, service: String) -> PairingFuture {
        let client = self.account(&ServiceAccount::new(&service, "")).clone();
        Box::pin(async move { client.begin(service).await })
    }
    fn finish(&self, account: ServiceAccount, attempt: String) -> EnrollmentReviewFuture {
        let client = self.account(&account).clone();
        Box::pin(async move { client.finish(account, attempt).await })
    }
    fn review(&self, account: ServiceAccount, name: String) -> EnrollmentApprovalFuture {
        let client = self.account(&account).clone();
        Box::pin(async move { client.review(account, name).await })
    }
    fn phase(&self, account: &ServiceAccount) -> EnrollmentPhase {
        self.account(account).phase(account)
    }
    fn linked(&self, account: &ServiceAccount) -> Option<LinkedAccount> {
        self.account(account).linked(account)
    }
    fn cancel(&self, account: &ServiceAccount) {
        self.account(account).cancel(account);
    }
    fn supports_sim_selection(&self) -> bool {
        self.sms.supports_sim_selection()
    }
    fn sim_choices(&self) -> tilo_messaging::SimChoicesFuture {
        let client = self.sms.clone();
        Box::pin(async move { client.sim_choices().await })
    }
    fn select_sim(&self, token: String) -> EnrollmentReviewFuture {
        let client = self.sms.clone();
        Box::pin(async move { client.select_sim(token).await })
    }
    fn cancel_sim_selection(&self) {
        self.sms.cancel_sim_selection();
    }
}

#[cfg(test)]
pub(super) fn fixture_sms(
    principal: String,
    wake: Wake,
    source: Arc<dyn super::sms_first::Source>,
) -> Rc<dyn FirstAccounts> {
    let signal = with_source(principal.clone(), wake.clone(), Arc::new(SignedSource));
    let sms = super::sms_first::fixture(principal.clone(), wake.clone(), source);
    let whatsapp = super::whatsapp_first::new(principal.clone(), wake);
    combine(principal, signal, sms, whatsapp)
}

#[cfg(test)]
pub(super) fn fixture_whatsapp(
    principal: String,
    wake: Wake,
    source: Arc<dyn super::whatsapp_first::Source>,
) -> Rc<dyn FirstAccounts> {
    let signal = with_source(principal.clone(), wake.clone(), Arc::new(SignedSource));
    let sms = super::sms_first::new(principal.clone(), wake.clone());
    let whatsapp = super::whatsapp_first::fixture(principal.clone(), wake, source);
    combine(principal, signal, sms, whatsapp)
}
