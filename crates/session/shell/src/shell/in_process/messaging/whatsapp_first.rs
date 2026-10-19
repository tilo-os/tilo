//! Original storage review, native SDK linking and explicit self-identity review.
use super::{Arc, NativeMessagingIdentity, Owner, SendError};
use crate::session_runtime::{PendingStart, Wake, runtime, start_blocking};
use launcher::owned_service::ServiceCancellation;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::Path,
    rc::Rc,
    sync::{Mutex, mpsc},
    time::Duration,
};
use tilo_messaging::{
    EnrollmentApprovalFuture, EnrollmentClient, EnrollmentHost, EnrollmentPhase, EnrollmentReview,
    EnrollmentReviewFuture, LinkedAccount, Pairing, PairingFuture, ServiceAccount,
};
use tilo_messaging_protocol::supervisor::whatsapp::WhatsappLinking;
use tokio::sync::oneshot;
const ORIGIN: &str = "https://web.whatsapp.com";
const CAPACITY: usize = 8;
const ANSWER_TIMEOUT: Duration = Duration::from_secs(90);
type Reply<T> = oneshot::Sender<Result<T, SendError>>;
enum Request {
    Prepare(Reply<EnrollmentReview>),
    Begin(Reply<Pairing>),
    Finish(ServiceAccount, String, Reply<EnrollmentReview>),
    Review(ServiceAccount, String, Reply<String>),
}
struct Projection {
    phase: EnrollmentPhase,
    linked: Option<LinkedAccount>,
    cancelled: bool,
    native: Option<std::sync::Weak<super::whatsapp::NativeWhatsappAccount>>,
}
struct Port {
    principal: String,
    sender: mpsc::SyncSender<Request>,
    states: Arc<Mutex<BTreeMap<ServiceAccount, Projection>>>,
    wake: Wake,
}
impl EnrollmentHost for Port {
    fn prepare(&self, service: String) -> EnrollmentReviewFuture {
        if service != "whatsapp" {
            return Box::pin(async { Err(SendError::InvalidRequest) });
        }
        super::ask_owner(&self.sender, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Prepare(reply)
        })
    }
    fn begin(&self, service: String) -> PairingFuture {
        if service != "whatsapp" {
            return Box::pin(async { Err(SendError::InvalidRequest) });
        }
        super::ask_owner(&self.sender, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Begin(reply)
        })
    }
    fn finish(&self, account: ServiceAccount, attempt: String) -> EnrollmentReviewFuture {
        super::ask_owner(&self.sender, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Finish(account, attempt, reply)
        })
    }
    fn review(&self, account: ServiceAccount, name: String) -> EnrollmentApprovalFuture {
        super::ask_owner(&self.sender, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Review(account, name, reply)
        })
    }
    fn phase(&self, account: &ServiceAccount) -> EnrollmentPhase {
        let Some((phase, cancelled, native)) = self.states.lock().ok().and_then(|states| {
            states
                .get(account)
                .map(|s| (s.phase, s.cancelled, s.native.clone()))
        }) else {
            return EnrollmentPhase::Empty;
        };
        if cancelled {
            return EnrollmentPhase::Interrupted;
        }
        if native.and_then(|n| n.upgrade()).is_some_and(|n| {
            matches!(
                n.state(),
                tilo_messaging::AccountState::Locked | tilo_messaging::AccountState::Failed
            )
        }) {
            return EnrollmentPhase::Interrupted;
        }
        phase
    }
    fn linked(&self, account: &ServiceAccount) -> Option<LinkedAccount> {
        self.states.lock().ok()?.get(account)?.linked.clone()
    }
    fn cancel(&self, account: &ServiceAccount) {
        if let Ok(mut states) = self.states.lock()
            && let Some(state) = states.get_mut(account)
        {
            state.cancelled = true;
            if let Some(native) = state.native.as_ref().and_then(std::sync::Weak::upgrade) {
                native.retire();
            }
        }
        (self.wake)();
    }
}
struct Attempt {
    identity: NativeMessagingIdentity,
    review: EnrollmentReview,
    cancellation: ServiceCancellation,
    permission: Option<super::OwnerPermission>,
    approved: Option<super::OwnerPermission>,
    native: Option<Arc<super::whatsapp::NativeWhatsappAccount>>,
    activation: Option<PendingStart<Result<Arc<WhatsappLinking>, SendError>>>,
    linker: Option<Arc<WhatsappLinking>>,
    pairing: Option<PendingStart<Result<Pairing, SendError>>>,
    reply: Option<Reply<Pairing>>,
    token: Option<String>,
    verified: Option<String>,
    published: bool,
}
impl Attempt {
    fn new(identity: NativeMessagingIdentity, review: EnrollmentReview) -> Result<Self, SendError> {
        identity.check()?;
        Ok(Self {
            identity,
            review,
            cancellation: ServiceCancellation::default(),
            permission: None,
            approved: None,
            native: None,
            activation: None,
            linker: None,
            pairing: None,
            reply: None,
            token: None,
            verified: None,
            published: false,
        })
    }
}
pub(super) trait Source: Send + Sync {
    fn resume(
        &self,
        account: &super::whatsapp::NativeWhatsappAccount,
        root: &Path,
        scope: String,
        expected: &str,
        policy: &super::network::Policy,
    ) -> Result<Arc<WhatsappLinking>, SendError> {
        account.resume_default(root.to_owned(), scope, expected, policy)
    }
    fn activate(
        &self,
        account: &super::whatsapp::NativeWhatsappAccount,
        root: &Path,
        scope: String,
        policy: &super::network::Policy,
    ) -> Result<Arc<WhatsappLinking>, SendError>;
}
struct SignedSource;
impl Source for SignedSource {
    fn activate(
        &self,
        account: &super::whatsapp::NativeWhatsappAccount,
        root: &Path,
        scope: String,
        policy: &super::network::Policy,
    ) -> Result<Arc<WhatsappLinking>, SendError> {
        account.activate_default(root.to_owned(), scope, policy)?;
        account.linking()
    }
}
struct Resume {
    native: Arc<super::whatsapp::NativeWhatsappAccount>,
    pending: Option<PendingStart<Result<Arc<WhatsappLinking>, SendError>>>,
    session: Option<Arc<WhatsappLinking>>,
    cancellation: ServiceCancellation,
}
struct Actor {
    port: Arc<Port>,
    requests: mpsc::Receiver<Request>,
    attempt: RefCell<Option<Attempt>>,
    completed: RefCell<Vec<Attempt>>,
    source: Arc<dyn Source>,
    restored: std::cell::Cell<bool>,
    resumes: RefCell<BTreeMap<ServiceAccount, Resume>>,
}
pub(in crate::shell::in_process) fn new(
    principal: String,
    wake: Wake,
) -> Rc<dyn super::first_account::FirstAccounts> {
    with_source(principal, wake, Arc::new(SignedSource))
}
fn with_source(
    principal: String,
    wake: Wake,
    source: Arc<dyn Source>,
) -> Rc<dyn super::first_account::FirstAccounts> {
    let (sender, requests) = mpsc::sync_channel(CAPACITY);
    Rc::new(Actor {
        port: Arc::new(Port {
            principal,
            sender,
            states: Arc::default(),
            wake,
        }),
        requests,
        attempt: RefCell::new(None),
        completed: RefCell::new(Vec::new()),
        source,
        restored: std::cell::Cell::new(false),
        resumes: RefCell::default(),
    })
}
#[cfg(test)]
pub(super) fn fixture(
    principal: String,
    wake: Wake,
    source: Arc<dyn Source>,
) -> Rc<dyn super::first_account::FirstAccounts> {
    with_source(principal, wake, source)
}
impl Actor {
    fn restore(&self, apps: &mut super::super::InProcessApps) {
        if self.restored.get()
            || apps.credential_owner.is_none()
            || apps.native_messaging_root.is_none()
        {
            return;
        }
        self.restored.set(true);
        let Ok(rows) = apps.default_native_accounts("whatsapp") else {
            return;
        };
        let Some(grants) = apps
            .credential_owner
            .as_ref()
            .and_then(|vault| vault.grants().ok())
        else {
            return;
        };
        for (id, row) in rows.into_iter().take(32) {
            if apps
                .messaging_services
                .iter()
                .any(|(account, _)| account.service == "whatsapp" && account.account == id)
            {
                continue;
            }
            let Some(generation) = row
                .config
                .get("linking_generation")
                .and_then(serde_json::Value::as_u64)
                .filter(|g| *g > 0)
            else {
                continue;
            };
            let scope = format!("whatsapp:{id}");
            if row
                .config
                .get("messaging_scope")
                .and_then(serde_json::Value::as_str)
                != Some(scope.as_str())
            {
                continue;
            }
            let Some(expected) = row
                .config
                .get("whatsapp_jid")
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let owner = Owner {
                principal: apps.author.0.clone(),
                device: apps.device_id.clone(),
                service: tilo_messaging_protocol::Service::Whatsapp,
                account: id,
                linking_generation: generation,
            };
            let _ = self.resume_account(apps, owner, scope, expected.to_owned(), &grants);
        }
    }
    fn resume_account(
        &self,
        apps: &mut super::super::InProcessApps,
        owner: Owner,
        scope: String,
        expected: String,
        grants: &[accounts::CredentialGrant],
    ) -> Result<(), SendError> {
        if uuid::Uuid::parse_str(&owner.account)
            .map_err(|_| SendError::Revoked)?
            .to_string()
            != owner.account
        {
            return Err(SendError::Revoked);
        }
        let grant = grants
            .iter()
            .find(|grant| {
                grant.app_id == "so.tilo.messages"
                    && grant.scope.as_ref() == Some(&super::credential_scope(&owner))
                    && grant.origin.as_deref() == Some(ORIGIN)
            })
            .ok_or(SendError::Revoked)?;
        let identity = apps
            .native_messaging_identity(owner)
            .map_err(|_| SendError::Revoked)?;
        let credential = apps
            .native_messaging_credential(
                &identity,
                accounts::AccessRequest {
                    app_id: "so.tilo.messages".into(),
                    item_id: grant.item_id.clone(),
                    purpose: "Resume the reviewed private WhatsApp identity".into(),
                    origin: grant.origin.clone(),
                },
            )
            .map_err(|_| SendError::Revoked)?;
        credential.check()?;
        super::first_account::ensure_contacts(apps, &identity)?;
        let authority = apps
            .native_messaging_authority(&identity, &credential, &scope)
            .map_err(|_| SendError::Revoked)?;
        let native = apps
            .prepare_whatsapp_account(super::whatsapp::WhatsappAccountStartup {
                identity: identity.clone(),
                credential,
                authority,
            })
            .map_err(|_| SendError::Revoked)?;
        let root = apps
            .native_messaging_root
            .as_ref()
            .ok_or(SendError::Unavailable)?
            .join(
                blake3::hash(
                    &serde_json::to_vec(&identity.owner).map_err(|_| SendError::Unavailable)?,
                )
                .to_hex()
                .as_str(),
            );
        let service = ServiceAccount::new("whatsapp", &identity.owner.account);
        drop(identity);
        let cancellation = ServiceCancellation::default();
        let policy = super::network::Policy {
            cancellation: cancellation.clone(),
            prompts: apps.messaging_network_prompts.clone(),
        };
        let source = self.source.clone();
        let retained = native.clone();
        let executor = runtime().map_err(|_| SendError::Unavailable)?;
        let pending = start_blocking(executor.handle(), self.port.wake.clone(), move || {
            Ok(source.resume(&retained, &root, scope, &expected, &policy))
        });
        self.resumes.borrow_mut().insert(
            service,
            Resume {
                native: native.clone(),
                pending: Some(pending),
                session: None,
                cancellation,
            },
        );
        drop(native);
        Ok(())
    }
    fn progress_resumes(&self) {
        for resumed in self.resumes.borrow_mut().values_mut() {
            if let Some(outcome) = resumed.pending.as_ref().and_then(PendingStart::poll) {
                resumed.pending.take();
                match outcome {
                    Ok(Ok(session)) => resumed.session = Some(session),
                    _ => resumed.native.retire(),
                }
            }
            if resumed
                .session
                .as_ref()
                .is_some_and(|session| session.verify_resume().is_err())
            {
                resumed.native.retire();
            }
            if matches!(
                resumed.native.state(),
                tilo_messaging::AccountState::Locked | tilo_messaging::AccountState::Failed
            ) {
                resumed.cancellation.cancel();
                if let Some(session) = resumed.session.take() {
                    session.cancel();
                }
            }
        }
    }
    fn phase(&self, account: &ServiceAccount, phase: EnrollmentPhase) {
        if let Ok(mut states) = self.port.states.lock()
            && let Some(state) = states.get_mut(account)
        {
            state.phase = phase;
        }
        (self.port.wake)();
    }
    fn prepare(&self, apps: &super::super::InProcessApps) -> Result<EnrollmentReview, SendError> {
        if self.attempt.borrow().is_some() {
            return Err(SendError::AlreadyReserved);
        }
        if self
            .port
            .states
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .len()
            >= 128
        {
            return Err(SendError::Unavailable);
        }
        let owner = Owner {
            principal: self.port.principal.clone(),
            device: apps.device_id.clone(),
            service: tilo_messaging_protocol::Service::Whatsapp,
            account: uuid::Uuid::new_v4().to_string(),
            linking_generation: 1,
        };
        let review = EnrollmentReview {
            account: ServiceAccount::new("whatsapp", &owner.account),
            generation: owner.linking_generation,
            scope: format!("whatsapp:{}", owner.account),
            number: None,
        };
        let admitted = Attempt::new(
            apps.native_messaging_identity(owner)
                .map_err(|_| SendError::Revoked)?,
            review.clone(),
        )?;
        self.port
            .states
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .insert(
                review.account.clone(),
                Projection {
                    phase: EnrollmentPhase::Review,
                    linked: None,
                    cancelled: false,
                    native: None,
                },
            );
        *self.attempt.borrow_mut() = Some(admitted);
        Ok(review)
    }
    fn begin(&self, reply: Reply<Pairing>) {
        let mut slot = self.attempt.borrow_mut();
        let Some(attempt) = slot.as_mut() else {
            let _ = reply.send(Err(SendError::Unavailable));
            return;
        };
        if attempt.published || attempt.pairing.is_some() || attempt.identity.check().is_err() {
            let _ = reply.send(Err(SendError::Revoked));
            return;
        }
        let Some(linker) = attempt.linker.clone() else {
            let _ = reply.send(Err(SendError::Unavailable));
            return;
        };
        let Ok(executor) = runtime() else {
            let _ = reply.send(Err(SendError::Unavailable));
            return;
        };
        attempt.pairing = Some(start_blocking(
            executor.handle(),
            self.port.wake.clone(),
            move || Ok(executor.handle().block_on(linker.begin())),
        ));
        attempt.reply = Some(reply);
    }
    fn finish(&self, account: &ServiceAccount, token: &str) -> Result<EnrollmentReview, SendError> {
        let mut slot = self.attempt.borrow_mut();
        let attempt = slot.as_mut().ok_or(SendError::Unavailable)?;
        attempt.identity.check()?;
        if &attempt.review.account != account
            || attempt.token.as_deref() != Some(token)
            || attempt.published
        {
            return Err(SendError::Revoked);
        }
        let jid = attempt
            .linker
            .as_ref()
            .ok_or(SendError::Unavailable)?
            .finish(token)?;
        let number = jid
            .strip_suffix("@s.whatsapp.net")
            .ok_or(SendError::Revoked)?;
        attempt.review.number = Some(format!("+{number}"));
        attempt.verified = Some(jid);
        self.phase(account, EnrollmentPhase::Review);
        Ok(attempt.review.clone())
    }
    fn review(
        &self,
        apps: &mut super::super::InProcessApps,
        account: &ServiceAccount,
        name: String,
    ) -> Result<String, SendError> {
        let proposal = {
            let mut slot = self.attempt.borrow_mut();
            let attempt = slot.as_mut().ok_or(SendError::Unavailable)?;
            attempt.identity.check()?;
            if &attempt.review.account != account
                || attempt.published
                || name.trim().is_empty()
                || name.len() > 256
                || name.chars().any(char::is_control)
            {
                return Err(SendError::Revoked);
            }
            if attempt.verified.is_some() {
                return self.publish(apps, attempt, name);
            }
            if attempt.permission.is_some() || attempt.approved.is_some() {
                return Err(SendError::AlreadyReserved);
            }
            Self::request_permission(apps, attempt, &name)?
        };
        self.phase(account, EnrollmentPhase::AwaitingPermission);
        Ok(proposal)
    }
    fn request_permission(
        apps: &super::super::InProcessApps,
        attempt: &mut Attempt,
        name: &str,
    ) -> Result<String, SendError> {
        let item_id = apps
            .prepare_native_messaging_profile_key(&attempt.identity, ORIGIN)
            .map_err(|_| SendError::Revoked)?;
        let asked = super::OwnerPermission::ask(
            apps,
            &attempt.identity,
            item_id,
            format!("Private WhatsApp profile for {name}"),
            ORIGIN,
        )?;
        let proposal = asked.proposal.clone();
        attempt.permission = Some(asked);
        Ok(proposal)
    }
    fn publish(
        &self,
        apps: &mut super::super::InProcessApps,
        attempt: &mut Attempt,
        name: String,
    ) -> Result<String, SendError> {
        let permission = attempt.approved.as_ref().ok_or(SendError::Revoked)?;
        let credential = apps
            .native_messaging_credential(&attempt.identity, permission.request.clone())
            .map_err(|_| SendError::Revoked)?;
        credential.check()?;
        let token = attempt.token.as_deref().ok_or(SendError::Revoked)?;
        let jid = attempt
            .linker
            .as_ref()
            .ok_or(SendError::Revoked)?
            .finish(token)?;
        if attempt.verified.as_ref() != Some(&jid)
            || attempt
                .native
                .as_ref()
                .is_none_or(|n| n.state() != tilo_messaging::AccountState::Ready)
        {
            return Err(SendError::Revoked);
        }
        apps.publish_native_whatsapp_account(&attempt.identity, name, jid)
            .map_err(|_| SendError::Revoked)?;
        credential.check()?;
        attempt.published = true;
        let linked = LinkedAccount {
            account: attempt.review.account.clone(),
            generation: attempt.review.generation,
            scope: attempt.review.scope.clone(),
        };
        if let Ok(mut states) = self.port.states.lock()
            && let Some(state) = states.get_mut(&linked.account)
        {
            state.linked = Some(linked);
            state.phase = EnrollmentPhase::Ready;
        }
        Ok(permission.proposal.clone())
    }
    fn activate(
        &self,
        apps: &mut super::super::InProcessApps,
        attempt: &mut Attempt,
        permission: super::OwnerPermission,
        credential: super::NativeMessagingCredential,
    ) -> Result<(), SendError> {
        super::first_account::ensure_contacts(apps, &attempt.identity)?;
        ensure_accounts(apps)?;
        let authority = apps
            .native_messaging_authority(&attempt.identity, &credential, &attempt.review.scope)
            .map_err(|_| SendError::Revoked)?;
        let account = apps
            .prepare_whatsapp_account(super::whatsapp::WhatsappAccountStartup {
                identity: attempt.identity.clone(),
                credential,
                authority,
            })
            .map_err(|_| SendError::Revoked)?;
        let root = apps
            .native_messaging_root
            .as_ref()
            .ok_or(SendError::Unavailable)?
            .join(
                blake3::hash(
                    &serde_json::to_vec(&attempt.identity.owner)
                        .map_err(|_| SendError::Unavailable)?,
                )
                .to_hex()
                .as_str(),
            );
        let policy = super::network::Policy {
            cancellation: attempt.cancellation.clone(),
            prompts: apps.messaging_network_prompts.clone(),
        };
        let source = self.source.clone();
        let retained = account.clone();
        let scope = attempt.review.scope.clone();
        let executor = runtime().map_err(|_| SendError::Unavailable)?;
        attempt.activation = Some(start_blocking(
            executor.handle(),
            self.port.wake.clone(),
            move || Ok(source.activate(&retained, &root, scope, &policy)),
        ));
        if let Ok(mut states) = self.port.states.lock()
            && let Some(state) = states.get_mut(&attempt.review.account)
        {
            state.native = Some(Arc::downgrade(&account));
        }
        attempt.native = Some(account.clone());
        drop(account);
        attempt.approved = Some(permission);
        Ok(())
    }
    fn progress(&self, apps: &mut super::super::InProcessApps, attempt: &mut Attempt) {
        let cancelled = self
            .port
            .states
            .lock()
            .ok()
            .and_then(|s| s.get(&attempt.review.account).map(|s| s.cancelled))
            .unwrap_or(true);
        if cancelled
            || attempt.identity.check().is_err()
            || attempt
                .linker
                .as_ref()
                .is_some_and(|linker| linker.check_linking().is_err())
            || attempt.native.as_ref().is_some_and(|native| {
                matches!(
                    native.state(),
                    tilo_messaging::AccountState::Locked | tilo_messaging::AccountState::Failed
                )
            })
        {
            self.retire(apps, attempt);
            return;
        }
        if let Some(outcome) = attempt.activation.as_ref().and_then(PendingStart::poll) {
            attempt.activation.take();
            if let Ok(Ok(linker)) = outcome {
                attempt.linker = Some(linker);
                self.phase(&attempt.review.account, EnrollmentPhase::Linking);
            } else {
                self.retire(apps, attempt);
                return;
            }
        }
        if let Some(outcome) = attempt.pairing.as_ref().and_then(PendingStart::poll) {
            attempt.pairing.take();
            let result = outcome.map_err(|_| SendError::Unavailable).and_then(|r| r);
            if let Ok(pairing) = &result {
                attempt.token = Some(pairing.attempt.clone());
            }
            if let Some(reply) = attempt.reply.take() {
                let _ = reply.send(result);
            }
        }
        let Some(permission) = attempt.permission.take() else {
            return;
        };
        let credential = match permission.grant(apps, &attempt.identity) {
            super::Grant::Approved(credential) => credential,
            grant => {
                let refused = matches!(grant, super::Grant::Refused);
                attempt.permission = Some(permission);
                if refused {
                    self.retire(apps, attempt);
                }
                return;
            }
        };
        if self
            .activate(apps, attempt, permission, credential)
            .is_err()
        {
            self.retire(apps, attempt);
        }
    }
    fn retire(&self, apps: &super::super::InProcessApps, attempt: &mut Attempt) {
        attempt.cancellation.cancel();
        if let Some(native) = &attempt.native {
            native.retire();
        }
        if let Some(linker) = &attempt.linker {
            linker.cancel();
        }
        if let Some(reply) = attempt.reply.take() {
            let _ = reply.send(Err(SendError::Revoked));
        }
        if let Some(permission) = attempt.permission.take() {
            permission.withdraw(apps);
        }
        self.phase(&attempt.review.account, EnrollmentPhase::Interrupted);
    }
}
impl super::first_account::FirstAccounts for Actor {
    fn client(&self) -> EnrollmentClient {
        EnrollmentClient::new(self.port.principal.clone(), self.port.clone())
    }
    fn pump(&self, apps: &mut super::super::InProcessApps) {
        self.restore(apps);
        self.progress_resumes();
        for completed in self.completed.borrow_mut().iter_mut() {
            self.progress(apps, completed);
        }
        {
            let mut slot = self.attempt.borrow_mut();
            if let Some(attempt) = slot.as_mut() {
                self.progress(apps, attempt);
            }
            if slot.as_ref().is_some_and(|a| a.published) {
                if let Some(completed) = slot.take() {
                    self.completed.borrow_mut().push(completed);
                }
            } else if slot
                .as_ref()
                .is_some_and(|a| self.port.phase(&a.review.account) == EnrollmentPhase::Interrupted)
                && let Some(mut attempt) = slot.take()
            {
                self.retire(apps, &mut attempt);
            }
        }
        for request in self.requests.try_iter().take(CAPACITY) {
            match request {
                Request::Prepare(reply) => {
                    let _ = reply.send(self.prepare(apps));
                }
                Request::Begin(reply) => self.begin(reply),
                Request::Finish(account, token, reply) => {
                    let _ = reply.send(self.finish(&account, &token));
                }
                Request::Review(account, name, reply) => {
                    let _ = reply.send(self.review(apps, &account, name));
                }
            }
        }
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        for resumed in self.resumes.get_mut().values() {
            resumed.cancellation.cancel();
            resumed.native.retire();
            if let Some(session) = &resumed.session {
                session.cancel();
            }
        }
        for attempt in self
            .completed
            .get_mut()
            .iter()
            .chain(self.attempt.get_mut().iter())
        {
            attempt.cancellation.cancel();
            if let Some(native) = &attempt.native {
                native.retire();
            }
            if let Some(linker) = &attempt.linker {
                linker.cancel();
            }
        }
    }
}
fn ensure_accounts(apps: &mut super::super::InProcessApps) -> Result<(), SendError> {
    use tilo_resource::Resource as _;
    let records = ui::record::person::PersonRecords::open(
        &mut apps.owners,
        accounts::person::ACCOUNTS_OWNER,
        apps.author.clone(),
    )
    .map_err(|_| SendError::Unavailable)?;
    records
        .open_original_record(
            ui::record::RecordKind::new(accounts::person::PersonAccounts::KIND),
            |doc| {
                accounts::person::PersonAccounts::initialize(doc).map_err(|error| error.to_string())
            },
        )
        .map_err(|_| SendError::Unavailable)?;
    records.flush().map_err(|_| SendError::Unavailable)
}
