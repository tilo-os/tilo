//! Explicit SIM choice before protected account review; no unlock or default change.
use super::{Arc, NativeMessagingIdentity, Owner, SendError};
use crate::session_runtime::{PendingStart, Wake, runtime, start_blocking};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{Mutex, mpsc},
    time::Duration,
};
use tilo_messaging::{
    EnrollmentApprovalFuture, EnrollmentClient, EnrollmentHost, EnrollmentPhase, EnrollmentReview,
    EnrollmentReviewFuture, LinkedAccount, PairingFuture, ServiceAccount, SimChoice,
    SimChoicesFuture,
};
use tilo_messaging_protocol::sms_runtime::SimBinding;
use tokio::sync::oneshot;
const ORIGIN: &str = "https://modemmanager.org";
const CAPACITY: usize = 8;
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
type Reply<T> = oneshot::Sender<Result<T, SendError>>;
enum Request {
    Choices(Reply<Vec<SimChoice>>),
    Select(String, Reply<EnrollmentReview>),
    Review(ServiceAccount, String, Reply<String>),
}
struct Projection {
    phase: EnrollmentPhase,
    linked: Option<LinkedAccount>,
    cancelled: bool,
    native: Option<std::sync::Weak<super::sms::Account>>,
}
struct Port {
    principal: String,
    sender: mpsc::SyncSender<Request>,
    projection: Arc<Mutex<BTreeMap<ServiceAccount, Projection>>>,
    wake: Wake,
    chooser_epoch: Arc<std::sync::atomic::AtomicU64>,
}
impl EnrollmentHost for Port {
    fn begin(&self, _: String) -> PairingFuture {
        Box::pin(async { Err(SendError::Unavailable) })
    }
    fn finish(&self, _: ServiceAccount, _: String) -> EnrollmentReviewFuture {
        Box::pin(async { Err(SendError::Unavailable) })
    }
    fn review(&self, account: ServiceAccount, name: String) -> EnrollmentApprovalFuture {
        super::ask_owner(&self.sender, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Review(account, name, reply)
        })
    }
    fn phase(&self, account: &ServiceAccount) -> EnrollmentPhase {
        let Ok(states) = self.projection.lock() else {
            return EnrollmentPhase::Empty;
        };
        states.get(account).map_or(EnrollmentPhase::Empty, |state| {
            super::projected_phase(
                state.phase,
                state.cancelled,
                state
                    .native
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
                    .map(|n| n.state()),
            )
        })
    }
    fn linked(&self, account: &ServiceAccount) -> Option<LinkedAccount> {
        self.projection.lock().ok()?.get(account)?.linked.clone()
    }
    fn cancel(&self, account: &ServiceAccount) {
        if let Ok(mut states) = self.projection.lock()
            && let Some(state) = states.get_mut(account)
        {
            state.cancelled = true;
        }
        (self.wake)();
    }
    fn supports_sim_selection(&self) -> bool {
        true
    }
    fn sim_choices(&self) -> SimChoicesFuture {
        super::ask_owner(&self.sender, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Choices(reply)
        })
    }
    fn select_sim(&self, token: String) -> EnrollmentReviewFuture {
        super::ask_owner(&self.sender, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Select(token, reply)
        })
    }
    fn cancel_sim_selection(&self) {
        self.chooser_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        (self.wake)();
    }
}
pub(super) struct Selected {
    connection: zbus::Connection,
    binding: SimBinding,
    label: String,
}
pub(super) trait Source: Send + Sync {
    fn connection(&self) -> Result<zbus::Connection, SendError> {
        Err(SendError::Unavailable)
    }
    fn activate(
        &self,
        account: Arc<super::sms::Account>,
        selected: Selected,
        root: std::path::PathBuf,
        scope: String,
    ) -> Result<(), SendError> {
        activate(&account, selected, root, scope)
    }
    fn choices(&self, identity: NativeMessagingIdentity) -> Result<Vec<Selected>, SendError>;
}
pub(super) fn activate(
    account: &super::sms::Account,
    selected: Selected,
    root: std::path::PathBuf,
    scope: String,
) -> Result<(), SendError> {
    runtime()
        .map_err(|_| SendError::Unavailable)?
        .block_on(account.activate(selected.connection, selected.binding, root, scope))
}
struct SystemSource;
impl Source for SystemSource {
    fn connection(&self) -> Result<zbus::Connection, SendError> {
        runtime()
            .map_err(|_| SendError::Unavailable)?
            .block_on(async {
                zbus::Connection::system()
                    .await
                    .map_err(|_| SendError::Unavailable)
            })
    }
    fn choices(&self, identity: NativeMessagingIdentity) -> Result<Vec<Selected>, SendError> {
        identity.check()?;
        runtime()
            .map_err(|_| SendError::Unavailable)?
            .block_on(async {
                let connection = zbus::Connection::system()
                    .await
                    .map_err(|_| SendError::Unavailable)?;
                let choices = tokio::time::timeout(Duration::from_secs(10), capture(&connection))
                    .await
                    .map_err(|_| SendError::Unavailable)??;
                identity.check()?;
                Ok(choices
                    .into_iter()
                    .map(|(binding, label)| Selected {
                        connection: connection.clone(),
                        binding,
                        label,
                    })
                    .collect())
            })
    }
}
type Objects = std::collections::HashMap<
    zbus::zvariant::OwnedObjectPath,
    BTreeMap<String, BTreeMap<String, zbus::zvariant::OwnedValue>>,
>;
async fn capture(connection: &zbus::Connection) -> Result<Vec<(SimBinding, String)>, SendError> {
    let bus = zbus::fdo::DBusProxy::new(connection)
        .await
        .map_err(|_| SendError::Unavailable)?;
    let daemon = bus
        .get_name_owner(
            "org.freedesktop.ModemManager1"
                .try_into()
                .map_err(|_| SendError::Unavailable)?,
        )
        .await
        .map_err(|_| SendError::Unavailable)?;
    let manager = zbus::Proxy::new(
        connection,
        daemon.clone(),
        "/org/freedesktop/ModemManager1",
        "org.freedesktop.DBus.ObjectManager",
    )
    .await
    .map_err(|_| SendError::Unavailable)?;
    let objects: Objects = manager
        .call("GetManagedObjects", &())
        .await
        .map_err(|_| SendError::Unavailable)?;
    if objects.len() > 128 {
        return Err(SendError::Unavailable);
    }
    let mut choices = Vec::new();
    for (path, interfaces) in &objects {
        let Some(modem) = interfaces.get("org.freedesktop.ModemManager1.Modem") else {
            continue;
        };
        if !interfaces.contains_key("org.freedesktop.ModemManager1.Modem.Messaging") {
            continue;
        }
        let Some(sim_path) = modem
            .get("Sim")
            .and_then(|v| <&zbus::zvariant::ObjectPath<'_>>::try_from(v).ok())
        else {
            continue;
        };
        let Some(sim) = objects
            .get(sim_path)
            .and_then(|i| i.get("org.freedesktop.ModemManager1.Sim"))
        else {
            continue;
        };
        let Some(iccid) = sim
            .get("SimIdentifier")
            .and_then(|v| <&str>::try_from(v).ok())
        else {
            continue;
        };
        let operator = sim
            .get("OperatorName")
            .and_then(|v| <&str>::try_from(v).ok())
            .filter(|v| !v.is_empty() && v.len() <= 160 && !v.chars().any(char::is_control))
            .unwrap_or("");
        let suffix = iccid
            .get(iccid.len().saturating_sub(4)..)
            .ok_or(SendError::Unavailable)?;
        let label = if operator.is_empty() {
            format!("…{suffix}")
        } else {
            format!("{operator} · …{suffix}")
        };
        let binding = SimBinding::new(
            daemon.to_string(),
            path.to_string(),
            sim_path.to_string(),
            iccid.into(),
            1,
        )
        .map_err(|_| SendError::Unavailable)?;
        choices.push((binding, label));
        if choices.len() > 32 {
            return Err(SendError::Unavailable);
        }
    }
    if bus
        .get_name_owner(
            "org.freedesktop.ModemManager1"
                .try_into()
                .map_err(|_| SendError::Unavailable)?,
        )
        .await
        .map_err(|_| SendError::Unavailable)?
        != daemon
    {
        return Err(SendError::Revoked);
    }
    Ok(choices)
}
struct Permission {
    asked: super::OwnerPermission,
    name: String,
}
struct Attempt {
    identity: NativeMessagingIdentity,
    selected: Option<Selected>,
    review: EnrollmentReview,
    permission: Option<Permission>,
    account: Option<Arc<super::sms::Account>>,
    activation: Option<PendingStart<Result<(), SendError>>>,
}
type ChoicesPending = (
    PendingStart<Result<Vec<Selected>, SendError>>,
    Reply<Vec<SimChoice>>,
);
struct Chooser {
    identity: NativeMessagingIdentity,
    epoch: u64,
    choices: BTreeMap<String, Selected>,
    pending: Option<ChoicesPending>,
}
impl Chooser {
    fn select(
        &mut self,
        token: &str,
        epoch: u64,
    ) -> Result<(NativeMessagingIdentity, Selected), SendError> {
        self.identity.check()?;
        if self.epoch != epoch {
            return Err(SendError::Revoked);
        }
        let selected = self
            .choices
            .remove(token)
            .ok_or(SendError::InvalidRequest)?;
        Ok((self.identity.clone(), selected))
    }
}
struct Actor {
    port: Arc<Port>,
    requests: mpsc::Receiver<Request>,
    source: Arc<dyn Source>,
    chooser: RefCell<Option<Chooser>>,
    active: RefCell<BTreeMap<ServiceAccount, Arc<super::sms::Account>>>,
    resumes: RefCell<BTreeMap<ServiceAccount, PendingStart<Result<(), SendError>>>>,
    restored: std::cell::Cell<bool>,
    attempt: RefCell<Option<Attempt>>,
}
pub(super) fn new(principal: String, wake: Wake) -> Rc<dyn super::first_account::FirstAccounts> {
    with_source(principal, wake, Arc::new(SystemSource))
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
            projection: Arc::default(),
            wake,
            chooser_epoch: Arc::default(),
        }),
        requests,
        source,
        chooser: RefCell::new(None),
        active: RefCell::default(),
        resumes: RefCell::default(),
        restored: std::cell::Cell::new(false),
        attempt: RefCell::new(None),
    })
}
impl super::first_account::FirstAccounts for Actor {
    fn client(&self) -> EnrollmentClient {
        EnrollmentClient::new(self.port.principal.clone(), self.port.clone())
    }
    fn pump(&self, apps: &mut super::super::InProcessApps) {
        for request in self.requests.try_iter().take(CAPACITY) {
            self.request(apps, request);
        }
        self.poll_choices();
        self.restore(apps);
        self.poll_permission(apps);
        let mut slot = self.attempt.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|a| a.activation.is_none() && a.account.is_some())
            && let Some(mut attempt) = slot.take()
            && let Some(account) = attempt.account.take()
        {
            self.active
                .borrow_mut()
                .insert(attempt.review.account, account);
        }
        drop(slot);
        self.resumes
            .borrow_mut()
            .retain(|_, pending| pending.poll().is_none());
        self.active.borrow_mut().retain(|id, account| {
            if self
                .port
                .projection
                .lock()
                .map_or(true, |states| states.get(id).is_none_or(|s| s.cancelled))
            {
                account.retire();
                return false;
            }
            self.phase(
                id,
                if account.state() == tilo_messaging::AccountState::Ready {
                    EnrollmentPhase::Ready
                } else {
                    EnrollmentPhase::Interrupted
                },
            );
            true
        });
    }
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
        let Ok(rows) = apps.default_native_accounts("sms") else {
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
                .any(|(a, _)| a.service == "sms" && a.account == id)
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
            let scope = format!("sms:{id}");
            if row
                .config
                .get("messaging_scope")
                .and_then(serde_json::Value::as_str)
                != Some(scope.as_str())
            {
                continue;
            }
            let _ = self.resume_account(apps, &id, generation, scope, &grants);
        }
    }
    fn resume_account(
        &self,
        apps: &mut super::super::InProcessApps,
        id: &str,
        generation: u64,
        scope: String,
        grants: &[accounts::CredentialGrant],
    ) -> Result<(), SendError> {
        if uuid::Uuid::parse_str(id)
            .map_err(|_| SendError::Revoked)?
            .to_string()
            != id
        {
            return Err(SendError::Revoked);
        }
        let owner = Owner {
            principal: apps.author.0.clone(),
            device: apps.device_id.clone(),
            service: tilo_messaging_protocol::Service::Sms,
            account: id.to_owned(),
            linking_generation: generation,
        };
        let grant = grants
            .iter()
            .find(|g| {
                g.app_id == "so.tilo.messages"
                    && g.scope.as_ref() == Some(&super::credential_scope(&owner))
                    && g.origin.as_deref() == Some(ORIGIN)
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
                    purpose: "Private SMS messaging using the selected SIM".into(),
                    origin: grant.origin.clone(),
                },
            )
            .map_err(|_| SendError::Revoked)?;
        credential.check()?;
        let authority = apps
            .native_messaging_authority(&identity, &credential, &scope)
            .map_err(|_| SendError::Revoked)?;
        let account = apps
            .prepare_sms_account(super::sms::Startup {
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
        drop(identity);
        self.start_resume(id, generation, scope, account, root)
    }
    fn start_resume(
        &self,
        id: &str,
        generation: u64,
        scope: String,
        account: Arc<super::sms::Account>,
        root: std::path::PathBuf,
    ) -> Result<(), SendError> {
        let worker = account.clone();
        let source = self.source.clone();
        let executor = runtime().map_err(|_| SendError::Unavailable)?;
        let pending = start_blocking(executor.handle(), self.port.wake.clone(), move || {
            let connection = source
                .connection()
                .map_err(|_| "SMS bus unavailable".to_owned())?;
            Ok(runtime()?.block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(10),
                    worker.resume(connection, root, scope),
                )
                .await
                .map_err(|_| SendError::Unavailable)?
            }))
        });
        let service = ServiceAccount::new("sms", id);
        self.port
            .projection
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .insert(
                service.clone(),
                Projection {
                    phase: EnrollmentPhase::Linking,
                    linked: Some(LinkedAccount {
                        account: service.clone(),
                        generation,
                        scope: format!("sms:{id}"),
                    }),
                    cancelled: false,
                    native: Some(Arc::downgrade(&account)),
                },
            );
        self.active.borrow_mut().insert(service.clone(), account);
        self.resumes.borrow_mut().insert(service, pending);
        Ok(())
    }
    fn request(&self, apps: &super::super::InProcessApps, request: Request) {
        match request {
            Request::Choices(reply) => self.choices(apps, reply),
            Request::Select(token, reply) => {
                let result = self.select(&token);
                let account = result.as_ref().ok().map(|review| review.account.clone());
                if reply.send(result).is_err()
                    && let Some(account) = account
                {
                    self.port.cancel(&account);
                }
            }
            Request::Review(account, name, reply) => {
                if reply.send(self.review(apps, &account, name)).is_err() {
                    self.port.cancel(&account);
                }
            }
        }
    }
    fn choices(&self, apps: &super::super::InProcessApps, reply: Reply<Vec<SimChoice>>) {
        if self
            .chooser
            .borrow()
            .as_ref()
            .is_some_and(|chooser| chooser.pending.is_none())
        {
            self.chooser.borrow_mut().take();
        }
        if self.attempt.borrow().is_some() || self.chooser.borrow().is_some() {
            let _ = reply.send(Err(SendError::AlreadyReserved));
            return;
        }
        let result = (|| {
            let identity = apps
                .native_messaging_identity(Owner {
                    principal: apps.author.0.clone(),
                    device: apps.device_id.clone(),
                    service: tilo_messaging_protocol::Service::Sms,
                    account: uuid::Uuid::new_v4().to_string(),
                    linking_generation: 1,
                })
                .map_err(|_| SendError::Revoked)?;
            let source = self.source.clone();
            let retained = identity.clone();
            let pending = start_blocking(
                runtime().map_err(|_| SendError::Unavailable)?.handle(),
                self.port.wake.clone(),
                move || Ok(source.choices(retained)),
            );
            Ok((identity, pending))
        })();
        match result {
            Ok((identity, pending)) => {
                *self.chooser.borrow_mut() = Some(Chooser {
                    identity,
                    epoch: self
                        .port
                        .chooser_epoch
                        .load(std::sync::atomic::Ordering::Acquire),
                    choices: BTreeMap::new(),
                    pending: Some((pending, reply)),
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }
    fn poll_choices(&self) {
        let mut slot = self.chooser.borrow_mut();
        let Some(chooser) = slot.as_mut() else {
            return;
        };
        if chooser.identity.check().is_err()
            || chooser.epoch
                != self
                    .port
                    .chooser_epoch
                    .load(std::sync::atomic::Ordering::Acquire)
        {
            slot.take();
            return;
        }
        let Some(outcome) = chooser
            .pending
            .as_ref()
            .and_then(|(pending, _)| pending.poll())
        else {
            return;
        };
        let Some((_, reply)) = chooser.pending.take() else {
            return;
        };
        let result = outcome
            .map_err(|_| SendError::Unavailable)
            .and_then(std::convert::identity)
            .and_then(|choices| {
                chooser.identity.check()?;
                let mut projection = Vec::new();
                for selected in choices {
                    let token = uuid::Uuid::new_v4().to_string();
                    projection.push(SimChoice {
                        selection: token.clone(),
                        label: selected.label.clone(),
                    });
                    chooser.choices.insert(token, selected);
                }
                Ok(projection)
            });
        if result.is_err() || reply.send(result).is_err() {
            slot.take();
        }
    }
    fn select(&self, token: &str) -> Result<EnrollmentReview, SendError> {
        if self.attempt.borrow().is_some() {
            return Err(SendError::AlreadyReserved);
        }
        let (identity, selected) = {
            let mut slot = self.chooser.borrow_mut();
            let values = slot.as_mut().ok_or(SendError::Revoked)?.select(
                token,
                self.port
                    .chooser_epoch
                    .load(std::sync::atomic::Ordering::Acquire),
            )?;
            slot.take();
            values
        };
        let review = EnrollmentReview {
            account: ServiceAccount::new("sms", &identity.owner.account),
            generation: 1,
            scope: format!("sms:{}", identity.owner.account),
            number: Some(selected.label.clone()),
        };
        self.port
            .projection
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
        *self.attempt.borrow_mut() = Some(Attempt {
            identity,
            selected: Some(selected),
            review: review.clone(),
            permission: None,
            account: None,
            activation: None,
        });
        Ok(review)
    }
    fn review(
        &self,
        apps: &super::super::InProcessApps,
        account: &ServiceAccount,
        name: String,
    ) -> Result<String, SendError> {
        let (attempt, proposal) = {
            let mut slot = self.attempt.borrow_mut();
            if slot
                .as_ref()
                .is_none_or(|attempt| &attempt.review.account != account)
            {
                return Err(SendError::Revoked);
            }
            slot.as_ref().ok_or(SendError::Revoked)?.identity.check()?;
            if name.trim().is_empty()
                || name.chars().count() > 160
                || name.chars().any(char::is_control)
            {
                return Err(SendError::InvalidRequest);
            }
            if slot
                .as_ref()
                .is_some_and(|attempt| attempt.permission.is_some() || attempt.account.is_some())
            {
                return Err(SendError::AlreadyReserved);
            }
            let mut attempt = slot.take().ok_or(SendError::Revoked)?;
            drop(slot);
            let item_id = apps
                .prepare_native_messaging_profile_key(&attempt.identity, ORIGIN)
                .map_err(|_| SendError::Revoked)?;
            let asked = super::OwnerPermission::ask(
                apps,
                &attempt.identity,
                item_id,
                "Private SMS messaging using the selected SIM".into(),
                ORIGIN,
            )?;
            let proposal = asked.proposal.clone();
            attempt.permission = Some(Permission { asked, name });
            (attempt, proposal)
        };
        self.attempt.replace(Some(attempt));
        self.phase(account, EnrollmentPhase::AwaitingPermission);
        Ok(proposal)
    }
    fn phase(&self, account: &ServiceAccount, phase: EnrollmentPhase) {
        let changed = self.port.projection.lock().ok().is_some_and(|mut states| {
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
    fn poll_permission(&self, apps: &mut super::super::InProcessApps) {
        let mut slot = self.attempt.borrow_mut();
        let Some(attempt) = slot.as_mut() else {
            return;
        };
        let cancelled = self.port.projection.lock().map_or(true, |states| {
            states
                .get(&attempt.review.account)
                .is_none_or(|s| s.cancelled)
        });
        if cancelled || attempt.identity.check().is_err() {
            self.retire(apps, attempt);
            slot.take();
            return;
        }
        if let Some(outcome) = attempt.activation.as_ref().and_then(PendingStart::poll) {
            attempt.activation.take();
            self.phase(
                &attempt.review.account,
                if outcome.is_ok_and(|r| r.is_ok()) {
                    EnrollmentPhase::Ready
                } else {
                    EnrollmentPhase::Interrupted
                },
            );
        }
        if attempt.activation.is_none()
            && let Some(account) = &attempt.account
        {
            self.phase(
                &attempt.review.account,
                if account.state() == tilo_messaging::AccountState::Ready {
                    EnrollmentPhase::Ready
                } else {
                    EnrollmentPhase::Interrupted
                },
            );
        }
        let Some(permission) = attempt.permission.take() else {
            return;
        };
        let credential = match permission.asked.grant(apps, &attempt.identity) {
            super::Grant::Approved(credential) => credential,
            grant => {
                let refused = matches!(grant, super::Grant::Refused);
                attempt.permission = Some(permission);
                if refused {
                    self.retire(apps, attempt);
                    slot.take();
                }
                return;
            }
        };
        if self
            .activate(apps, attempt, permission, credential)
            .is_err()
        {
            self.phase(&attempt.review.account, EnrollmentPhase::Interrupted);
        }
    }
    fn activate(
        &self,
        apps: &mut super::super::InProcessApps,
        attempt: &mut Attempt,
        permission: Permission,
        credential: super::NativeMessagingCredential,
    ) -> Result<(), SendError> {
        credential.check()?;
        apps.publish_native_account(
            &attempt.identity,
            tilo_messaging_protocol::Service::Sms,
            accounts::person::PersonAccounts::PUBLISH_SMS_ACCOUNT,
            permission.name,
            &attempt.review.scope,
        )
        .map_err(|_| SendError::Revoked)?;
        let authority = apps
            .native_messaging_authority(&attempt.identity, &credential, &attempt.review.scope)
            .map_err(|_| SendError::Revoked)?;
        attempt.account = Some(
            apps.prepare_sms_account(super::sms::Startup {
                identity: attempt.identity.clone(),
                credential,
                authority,
            })
            .map_err(|_| SendError::Revoked)?,
        );
        if let Ok(mut states) = self.port.projection.lock()
            && let Some(state) = states.get_mut(&attempt.review.account)
        {
            state.native = attempt.account.as_ref().map(Arc::downgrade);
        }
        let selected = attempt.selected.take().ok_or(SendError::Revoked)?;
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
        let scope = attempt.review.scope.clone();
        let worker = attempt.account.clone().ok_or(SendError::Revoked)?;
        let source = self.source.clone();
        attempt.activation = Some(start_blocking(
            runtime().map_err(|_| SendError::Unavailable)?.handle(),
            self.port.wake.clone(),
            move || Ok(source.activate(worker, selected, root, scope)),
        ));
        if let Ok(mut states) = self.port.projection.lock()
            && let Some(state) = states.get_mut(&attempt.review.account)
        {
            state.linked = Some(LinkedAccount {
                account: attempt.review.account.clone(),
                generation: attempt.review.generation,
                scope: attempt.review.scope.clone(),
            });
        }
        Ok(())
    }
    fn retire(&self, apps: &super::super::InProcessApps, attempt: &mut Attempt) {
        if let Some(account) = &attempt.account {
            account.retire();
        }
        if let Some(permission) = attempt.permission.take() {
            permission.asked.withdraw(apps);
        }
        self.phase(&attempt.review.account, EnrollmentPhase::Interrupted);
    }
}

impl Drop for Actor {
    fn drop(&mut self) {
        for account in self.active.get_mut().values() {
            account.retire();
        }
        if let Some(attempt) = self.attempt.get_mut().as_mut()
            && let Some(account) = &attempt.account
        {
            account.retire();
        }
    }
}

#[cfg(test)]
pub(super) fn fixture(
    principal: String,
    wake: Wake,
    source: Arc<dyn Source>,
) -> Rc<dyn super::first_account::FirstAccounts> {
    with_source(principal, wake, source)
}
#[cfg(test)]
pub(super) async fn fixture_choices(
    connection: &zbus::Connection,
) -> Result<Vec<Selected>, SendError> {
    Ok(capture(connection)
        .await?
        .into_iter()
        .map(|(binding, label)| Selected {
            connection: connection.clone(),
            binding,
            label,
        })
        .collect())
}
