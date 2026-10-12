//! Original UI-owned first Tilo sender review and cold-start restoration.
use super::{Arc, NativeMessagingIdentity, Owner, SendError};
use crate::session_runtime::{PendingStart, Wake, runtime, start_blocking};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::{Mutex, mpsc},
    time::Duration,
};
use tilo_messaging::{
    EnrollmentApprovalFuture, EnrollmentClient, EnrollmentHost, EnrollmentPhase, EnrollmentReview,
    EnrollmentReviewFuture, LinkedAccount, PairingFuture, ServiceAccount,
};
use tokio::sync::oneshot;
// Credential provenance uses the existing canonical HTTPS origin policy.
// This label performs no HTTP request; the exact private scope is mandatory.
const ORIGIN: &str = "https://tilo.so";
const CAPACITY: usize = 8;
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
type Reply<T> = oneshot::Sender<Result<T, SendError>>;
enum Request {
    Prepare(Reply<EnrollmentReview>),
    Review(ServiceAccount, String, Reply<String>),
}
struct Projection {
    phase: EnrollmentPhase,
    cancelled: bool,
    linked: Option<LinkedAccount>,
    native: Option<std::sync::Weak<super::native::Account>>,
}
struct Port {
    principal: String,
    sender: mpsc::SyncSender<Request>,
    wake: Wake,
    states: Arc<Mutex<BTreeMap<ServiceAccount, Projection>>>,
}
impl EnrollmentHost for Port {
    fn prepare(&self, service: String) -> EnrollmentReviewFuture {
        if service != "tilo" {
            return Box::pin(async { Err(SendError::InvalidRequest) });
        }
        super::ask_owner(&self.sender, &self.wake, ANSWER_TIMEOUT, |reply| {
            Request::Prepare(reply)
        })
    }
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
        let Some((phase, cancelled, native)) = self.states.lock().ok().and_then(|states| {
            states
                .get(account)
                .map(|state| (state.phase, state.cancelled, state.native.clone()))
        }) else {
            return EnrollmentPhase::Empty;
        };
        if cancelled {
            return EnrollmentPhase::Interrupted;
        }
        if let Some(native) = native.and_then(|owner| owner.upgrade()) {
            match native.state() {
                tilo_messaging::AccountState::Ready => return EnrollmentPhase::Ready,
                tilo_messaging::AccountState::Locked | tilo_messaging::AccountState::Failed => {
                    return EnrollmentPhase::Interrupted;
                }
                _ if phase == EnrollmentPhase::Ready => return EnrollmentPhase::Interrupted,
                _ => {}
            }
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
            state.phase = EnrollmentPhase::Interrupted;
        }
        (self.wake)();
    }
}
struct Permission {
    request: accounts::AccessRequest,
    proposal: String,
    name: String,
}
struct Attempt {
    identity: NativeMessagingIdentity,
    review: EnrollmentReview,
    permission: Option<Permission>,
}
struct Active {
    account: Arc<super::native::Account>,
    pending: Option<PendingStart<Result<(), SendError>>>,
}
struct Actor {
    port: Arc<Port>,
    requests: mpsc::Receiver<Request>,
    attempt: RefCell<Option<Attempt>>,
    active: RefCell<BTreeMap<ServiceAccount, Active>>,
    restored: Cell<bool>,
}
pub(super) fn new(principal: String, wake: Wake) -> Rc<dyn super::first_account::FirstAccounts> {
    let (sender, requests) = mpsc::sync_channel(CAPACITY);
    Rc::new(Actor {
        port: Arc::new(Port {
            principal,
            sender,
            wake,
            states: Arc::default(),
        }),
        requests,
        attempt: RefCell::new(None),
        active: RefCell::new(BTreeMap::new()),
        restored: Cell::new(false),
    })
}
impl super::first_account::FirstAccounts for Actor {
    fn client(&self) -> EnrollmentClient {
        EnrollmentClient::new(self.port.principal.clone(), self.port.clone())
    }
    fn pump(&self, apps: &mut super::super::InProcessApps) {
        for request in self.requests.try_iter().take(CAPACITY) {
            match request {
                Request::Prepare(reply) => {
                    let _ = reply.send(self.prepare(apps));
                }
                Request::Review(account, name, reply) => {
                    let _ = reply.send(self.review(apps, &account, name));
                }
            }
        }
        self.restore(apps);
        self.approval(apps);
        self.active.borrow_mut().retain(|id, active| {
            let cancelled = self
                .port
                .states
                .lock()
                .map_or(true, |s| s.get(id).is_none_or(|p| p.cancelled));
            if cancelled {
                active.account.retire();
                return false;
            }
            if let Some(outcome) = active.pending.as_ref().and_then(PendingStart::poll) {
                active.pending = None;
                let phase = if outcome.is_ok_and(|r| r.is_ok()) {
                    EnrollmentPhase::Ready
                } else {
                    EnrollmentPhase::Interrupted
                };
                self.phase(id, phase);
            }
            if active.pending.is_none()
                && active.account.state() != tilo_messaging::AccountState::Ready
            {
                self.phase(id, EnrollmentPhase::Interrupted);
            }
            true
        });
    }
}
impl Actor {
    fn phase(&self, id: &ServiceAccount, phase: EnrollmentPhase) {
        if let Ok(mut states) = self.port.states.lock()
            && let Some(state) = states.get_mut(id)
            && state.phase != phase
        {
            state.phase = phase;
            (self.port.wake)();
        }
    }
    fn prepare(
        &self,
        apps: &mut super::super::InProcessApps,
    ) -> Result<EnrollmentReview, SendError> {
        if self.attempt.borrow().is_some()
            || !self.active.borrow().is_empty()
            || apps.native_messaging_transport.is_none()
        {
            return Err(SendError::Unavailable);
        }
        if !apps.default_native_accounts("tilo")?.is_empty() {
            return Err(SendError::AlreadyReserved);
        }
        let identity = apps
            .native_messaging_identity(Owner {
                principal: apps.author.0.clone(),
                device: apps.device_id.clone(),
                service: tilo_messaging_protocol::Service::Tilo,
                account: uuid::Uuid::new_v4().to_string(),
                linking_generation: 1,
            })
            .map_err(|_| SendError::Revoked)?;
        let review = EnrollmentReview {
            account: ServiceAccount::new("tilo", &identity.owner.account),
            generation: 1,
            scope: format!("tilo:{}", identity.owner.account),
            number: Some(identity.owner.principal.clone()),
        };
        self.port
            .states
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .insert(
                review.account.clone(),
                Projection {
                    phase: EnrollmentPhase::Review,
                    cancelled: false,
                    linked: None,
                    native: None,
                },
            );
        *self.attempt.borrow_mut() = Some(Attempt {
            identity,
            review: review.clone(),
            permission: None,
        });
        Ok(review)
    }
    fn review(
        &self,
        apps: &super::super::InProcessApps,
        account: &ServiceAccount,
        name: String,
    ) -> Result<String, SendError> {
        if name.trim().is_empty()
            || name.chars().count() > 160
            || name.chars().any(char::is_control)
        {
            return Err(SendError::InvalidRequest);
        }
        let proposal = {
            let mut slot = self.attempt.borrow_mut();
            let attempt = slot
                .as_mut()
                .filter(|a| &a.review.account == account)
                .ok_or(SendError::Revoked)?;
            attempt.identity.check()?;
            if attempt.permission.is_some() {
                return Err(SendError::AlreadyReserved);
            }
            let item_id = apps
                .prepare_native_messaging_profile_key(&attempt.identity, ORIGIN)
                .map_err(|_| SendError::Revoked)?;
            let request = accounts::AccessRequest {
                app_id: "so.tilo.messages".into(),
                item_id,
                purpose: "Private Tilo messaging using this personal identity".into(),
                origin: Some(ORIGIN.into()),
            };
            let Ok(proposal) =
                apps.propose_native_messaging_permission(&attempt.identity, request.clone())
            else {
                if let Some(vault) = &apps.credential_owner {
                    let _ = vault.edit(|store| store.remove(&request.item_id));
                }
                return Err(SendError::Revoked);
            };
            attempt.permission = Some(Permission {
                request,
                proposal: proposal.clone(),
                name,
            });
            proposal
        };
        self.phase(account, EnrollmentPhase::AwaitingPermission);
        Ok(proposal)
    }
    fn approval(&self, apps: &mut super::super::InProcessApps) {
        let mut slot = self.attempt.borrow_mut();
        let Some(attempt) = slot.as_mut() else {
            return;
        };
        let cancelled = self.port.states.lock().map_or(true, |s| {
            s.get(&attempt.review.account).is_none_or(|p| p.cancelled)
        });
        if cancelled || attempt.identity.check().is_err() {
            self.retire(apps, attempt);
            slot.take();
            return;
        }
        let Some(permission) = attempt.permission.as_ref() else {
            return;
        };
        let credential =
            apps.native_messaging_credential(&attempt.identity, permission.request.clone());
        if !credential.as_ref().is_ok_and(|c| c.check().is_ok()) {
            let pending = apps
                .credential_owner
                .as_ref()
                .and_then(|v| v.proposals().ok())
                .is_some_and(|p| p.iter().any(|p| p.id == permission.proposal));
            if !pending {
                self.retire(apps, attempt);
                slot.take();
            }
            return;
        }
        let account = attempt.review.account.clone();
        let result = (|| {
            let credential = credential.map_err(|_| SendError::Revoked)?;
            apps.publish_native_account(
                &attempt.identity,
                tilo_messaging_protocol::Service::Tilo,
                accounts::person::PersonAccounts::PUBLISH_TILO_ACCOUNT,
                permission.name.clone(),
                &attempt.review.scope,
            )
            .map_err(|_| SendError::Revoked)?;
            super::first_account::ensure_contacts(apps, &attempt.identity)?;
            self.start(
                apps,
                attempt.identity.clone(),
                credential,
                attempt.review.scope.clone(),
            )
        })();
        if result.is_err() {
            self.phase(&account, EnrollmentPhase::Interrupted);
        }
        slot.take();
    }
    fn start(
        &self,
        apps: &mut super::super::InProcessApps,
        identity: NativeMessagingIdentity,
        credential: super::NativeMessagingCredential,
        scope: String,
    ) -> Result<(), SendError> {
        let authority = apps
            .native_messaging_authority(&identity, &credential, &scope)
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
        let id = ServiceAccount::new("tilo", &identity.owner.account);
        let linked = LinkedAccount {
            account: id.clone(),
            generation: identity.owner.linking_generation,
            scope,
        };
        let active = {
            let account = apps
                .prepare_native_tilo_account(super::native::Startup {
                    identity,
                    credential,
                    authority,
                })
                .map_err(|_| SendError::Revoked)?;
            let worker = account.clone();
            let worker_owner = account.clone();
            drop(account);
            let pending = start_blocking(
                runtime().map_err(|_| SendError::Unavailable)?.handle(),
                self.port.wake.clone(),
                move || Ok(worker.activate(&root)),
            );
            Active {
                account: worker_owner,
                pending: Some(pending),
            }
        };
        self.port
            .states
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .insert(
                id.clone(),
                Projection {
                    phase: EnrollmentPhase::Linking,
                    cancelled: false,
                    linked: Some(linked),
                    native: Some(Arc::downgrade(&active.account)),
                },
            );
        self.active.borrow_mut().insert(id, active);
        Ok(())
    }
    fn restore(&self, apps: &mut super::super::InProcessApps) {
        if self.restored.get()
            || apps.native_messaging_transport.is_none()
            || apps.credential_owner.is_none()
        {
            return;
        }
        let Ok(rows) = apps.default_native_accounts("tilo") else {
            return;
        };
        let Some(grants) = apps.credential_owner.as_ref().and_then(|v| v.grants().ok()) else {
            return;
        };
        self.restored.set(true);
        if rows.len() != 1 {
            return;
        }
        for (id, row) in rows {
            let scope = format!("tilo:{id}");
            let Some(generation) = row
                .config
                .0
                .get("linking_generation")
                .and_then(serde_json::Value::as_u64)
                .filter(|g| *g > 0)
            else {
                continue;
            };
            if row
                .config
                .0
                .get("messaging_scope")
                .and_then(serde_json::Value::as_str)
                != Some(&scope)
            {
                continue;
            }
            let owner = Owner {
                principal: apps.author.0.clone(),
                device: apps.device_id.clone(),
                service: tilo_messaging_protocol::Service::Tilo,
                account: id.clone(),
                linking_generation: generation,
            };
            let Some(grant) = grants.iter().find(|g| {
                g.app_id == "so.tilo.messages"
                    && g.scope.as_ref() == Some(&super::credential_scope(&owner))
                    && g.origin.as_deref() == Some(ORIGIN)
            }) else {
                continue;
            };
            let result = (|| {
                let identity = apps
                    .native_messaging_identity(owner)
                    .map_err(|_| SendError::Revoked)?;
                let credential = apps
                    .native_messaging_credential(
                        &identity,
                        accounts::AccessRequest {
                            app_id: "so.tilo.messages".into(),
                            item_id: grant.item_id.clone(),
                            purpose: "Private Tilo messaging using this personal identity".into(),
                            origin: grant.origin.clone(),
                        },
                    )
                    .map_err(|_| SendError::Revoked)?;
                credential.check()?;
                super::first_account::ensure_contacts(apps, &identity)?;
                self.start(apps, identity, credential, scope)
            })();
            if result.is_err() {
                self.phase(
                    &ServiceAccount::new("tilo", id),
                    EnrollmentPhase::Interrupted,
                );
            }
        }
    }
    fn retire(&self, apps: &super::super::InProcessApps, attempt: &mut Attempt) {
        if let Some(permission) = attempt.permission.take()
            && let Some(vault) = &apps.credential_owner
        {
            let _ = vault.decide_proposal(&permission.proposal, accounts::AccessDecision::Deny);
            let _ = vault.edit(|store| store.remove(&permission.request.item_id));
        }
        self.phase(&attempt.review.account, EnrollmentPhase::Interrupted);
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        for active in self.active.get_mut().values() {
            active.account.retire();
        }
    }
}
