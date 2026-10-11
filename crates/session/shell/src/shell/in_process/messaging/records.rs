//! Current canonical metadata queried on its original native UI owner.
//! Workers get bounded replies, never an independently opened file snapshot.
use super::{
    Authorization, NativeMessagingCredential, NativeMessagingIdentity, Owner, Route, SendError,
    SignalAuthority,
};
use accounts::person::PersonAccounts;
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
    sync::{Arc, mpsc},
    thread::ThreadId,
};
use tilo_contact_directory::{ContactDirectory, routing::SenderAccount};
use tilo_messaging_protocol::{
    Service,
    routing::{LinkState, LiveBinding},
};
use tilo_resource::{Resource, SharedRecord};
use ui::record::{RecordCheckout, RecordId, RecordKind, RecordOwners, RecordStore};
use ui::time::{Duration, Instant};

const CAPACITY: usize = 32;
const DEADLINE: Duration = Duration::from_secs(2);
thread_local! {
    static OWNERS: RefCell<HashMap<uuid::Uuid, Weak<Records>>> = RefCell::new(HashMap::new());
}
struct Metadata {
    store: RecordStore,
    id: RecordId,
}
impl Metadata {
    fn existing<P: Resource + SharedRecord>(
        owners: &mut RecordOwners,
        principal: &str,
    ) -> Result<Self, SendError> {
        let store = owners
            .open_person(
                P::OWNER,
                &ui::record::person::scope_of(&ui::permissions::UserId(principal.into())),
            )
            .map_err(|_| SendError::Unavailable)?;
        let kind = RecordKind::new(P::KIND);
        let id = ui::record::person::record_id(&kind);
        // Existing-only admission deliberately has no WriterBusy replica fallback.
        store
            .try_open_record(&RecordCheckout::new(id.clone(), kind))
            .map_err(|_| SendError::Unavailable)?;
        Ok(Self { store, id })
    }
    fn read<P: Resource>(&self) -> Result<P::State, SendError> {
        self.store
            .read(&self.id, P::read)
            .ok_or(SendError::Unavailable)?
            .map_err(|_| SendError::Unavailable)
    }
}
enum Query {
    Account,
    Route(Route),
    Incoming(String),
}
struct Request {
    identity: NativeMessagingIdentity,
    credential: NativeMessagingCredential,
    scope: String,
    query: Query,
    expires: Instant,
    reply: mpsc::SyncSender<Result<Option<Authorization>, SendError>>,
}
struct Port {
    id: uuid::Uuid,
    thread: ThreadId,
    sender: mpsc::SyncSender<Request>,
    wake: crate::session_runtime::Wake,
}
/// UI-affine lifetime retained by the native app host, not a protocol worker.
pub(crate) trait RecordQueries {
    fn authority(
        &self,
        identity: &NativeMessagingIdentity,
        credential: &NativeMessagingCredential,
        scope: &str,
    ) -> Result<Arc<CurrentAuthority>, SendError>;
    fn pump(&self);
}

pub(crate) struct Records {
    principal: String,
    device: String,
    directory: Metadata,
    accounts: Metadata,
    receiver: mpsc::Receiver<Request>,
    port: Arc<Port>,
}
impl Records {
    pub(crate) fn open(
        owners: &mut RecordOwners,
        principal: &str,
        device: &str,
        wake: crate::session_runtime::Wake,
    ) -> Result<Rc<dyn RecordQueries>, SendError> {
        let directory = Metadata::existing::<ContactDirectory>(owners, principal)?;
        let accounts = Metadata::existing::<PersonAccounts>(owners, principal)?;
        let (sender, receiver) = mpsc::sync_channel(CAPACITY);
        let owner = Rc::new(Self {
            principal: principal.into(),
            device: device.into(),
            directory,
            accounts,
            receiver,
            port: Arc::new(Port {
                id: uuid::Uuid::new_v4(),
                thread: std::thread::current().id(),
                sender,
                wake,
            }),
        });
        OWNERS.with(|owners| {
            let mut owners = owners.borrow_mut();
            owners.retain(|_, owner| owner.strong_count() > 0);
            owners.insert(owner.port.id, Rc::downgrade(&owner));
            drop(owners);
        });
        Ok(owner)
    }
}
impl RecordQueries for Records {
    fn authority(
        &self,
        identity: &NativeMessagingIdentity,
        credential: &NativeMessagingCredential,
        scope: &str,
    ) -> Result<Arc<CurrentAuthority>, SendError> {
        identity.check()?;
        credential.check()?;
        if identity.owner.principal != self.principal
            || identity.owner.device != self.device
            || identity.owner.linking_generation == 0
            || scope.is_empty()
            || scope.len() > 512
            || scope.chars().any(char::is_control)
        {
            return Err(SendError::Revoked);
        }
        Ok(Arc::new(CurrentAuthority {
            identity: identity.clone(),
            credential: credential.clone(),
            scope: scope.into(),
            port: self.port.clone(),
        }))
    }
    /// Drain bounded work on the actual owner, even when no app window is open.
    fn pump(&self) {
        for request in self.receiver.try_iter().take(CAPACITY) {
            let result = if Instant::now() >= request.expires {
                Err(SendError::Unavailable)
            } else {
                request.identity.check().and_then(|()| {
                    request.credential.check()?;
                    let result =
                        self.resolve(&request.identity.owner, &request.scope, &request.query);
                    request.identity.check()?;
                    request.credential.check()?;
                    result
                })
            };
            let _ = request.reply.try_send(result);
        }
    }
}
impl Records {
    fn resolve(
        &self,
        owner: &Owner,
        scope: &str,
        query: &Query,
    ) -> Result<Option<Authorization>, SendError> {
        if owner.principal != self.principal || owner.device != self.device {
            return Err(SendError::Revoked);
        }
        let accounts = self.accounts.read::<PersonAccounts>()?.store();
        let service = match owner.service {
            Service::Signal => "signal",
            Service::Whatsapp => "whatsapp",
            Service::Sms => "sms",
            Service::Tilo => "tilo",
        };
        let account = accounts
            .get(&accounts::AccountId::new(&owner.account))
            .filter(|account| {
                account.enabled
                    && account.type_id.as_str() == service
                    && account.owner.is_none()
                    && account
                        .config
                        .get("messaging_scope")
                        .and_then(serde_json::Value::as_str)
                        == Some(scope)
                    && account
                        .config
                        .get("linking_generation")
                        .and_then(serde_json::Value::as_u64)
                        == Some(owner.linking_generation)
            })
            .ok_or(SendError::Revoked)?;
        if matches!(query, Query::Account) {
            return Ok(None);
        }
        let directory = self.directory.read::<ContactDirectory>()?;
        let sender = SenderAccount {
            id: tilo_resource::Id::from_raw(&owner.account),
            service: service.into(),
            scope: scope.into(),
            enabled: account.enabled,
        };
        let route = match query {
            Query::Account => return Err(SendError::Unavailable),
            Query::Route(route) => {
                if !tilo_contact_directory::routing::routes(
                    &directory,
                    &route.person,
                    service,
                    &[sender],
                )
                .contains(route)
                {
                    return Err(SendError::Revoked);
                }
                route.clone()
            }
            Query::Incoming(recipient) => {
                let mut matches = directory
                    .people
                    .iter()
                    .flat_map(|(person, _)| {
                        tilo_contact_directory::routing::routes(
                            &directory,
                            person,
                            service,
                            std::slice::from_ref(&sender),
                        )
                    })
                    .filter(|route| route.recipient == *recipient);
                let found = matches.next().ok_or(SendError::Revoked)?;
                if matches.next().is_some() {
                    return Err(SendError::Revoked);
                }
                found
            }
        };
        // The enclosing native account gate and verified adapter supply linkage;
        // canonical record metadata is never credential/admission evidence.
        Ok(Some(Authorization::new(
            directory,
            accounts,
            LiveBinding {
                owner: owner.clone(),
                state: LinkState::Linked,
            },
            route,
        )))
    }
}
/// Restricted current metadata provider, bound by the native startup gate.
pub struct CurrentAuthority {
    identity: NativeMessagingIdentity,
    credential: NativeMessagingCredential,
    scope: String,
    port: Arc<Port>,
}
impl CurrentAuthority {
    fn query(&self, query: Query) -> Result<Option<Authorization>, SendError> {
        self.identity.check()?;
        self.credential.check()?;
        if std::thread::current().id() == self.port.thread {
            let result = OWNERS.with(|owners| {
                let current = owners
                    .borrow()
                    .get(&self.port.id)
                    .and_then(Weak::upgrade)
                    .ok_or(SendError::Unavailable)?;
                current.resolve(&self.identity.owner, &self.scope, &query)
            });
            self.identity.check()?;
            self.credential.check()?;
            return result;
        }
        let (reply, answer) = mpsc::sync_channel(1);
        let expires = Instant::now()
            .checked_add(DEADLINE)
            .ok_or(SendError::Unavailable)?;
        self.port
            .sender
            .try_send(Request {
                identity: self.identity.clone(),
                credential: self.credential.clone(),
                scope: self.scope.clone(),
                query,
                expires,
                reply,
            })
            .map_err(|_| SendError::Unavailable)?;
        (self.port.wake)();
        let result = answer
            .recv_timeout(DEADLINE)
            .map_err(|_| SendError::Unavailable)?;
        self.identity.check()?;
        self.credential.check()?;
        result
    }
}
impl SignalAuthority for CurrentAuthority {
    fn check_account_scope(&self, scope: &str) -> Result<(), SendError> {
        if scope != self.scope {
            return Err(SendError::Revoked);
        }
        self.check_account()
    }
    fn check_account(&self) -> Result<(), SendError> {
        match self.query(Query::Account)? {
            None => Ok(()),
            Some(_) => Err(SendError::Unavailable),
        }
    }
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        self.query(Query::Route(route.clone()))?
            .ok_or(SendError::Unavailable)
    }
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        if recipient.is_empty() || recipient.len() > 512 || recipient.chars().any(char::is_control)
        {
            return Err(SendError::Revoked);
        }
        self.query(Query::Incoming(recipient.into()))?
            .ok_or(SendError::Unavailable)
    }
}
impl tilo_messaging_protocol::external_broker::ExternalAuthority for CurrentAuthority {
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        SignalAuthority::authorize(self, route)
    }
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        SignalAuthority::incoming(self, recipient)
    }
}
