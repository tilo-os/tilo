//! A workspace resource the session reads and changes as every caller does: followed through the
//! device's one read ([`tilo_actions::remote::ResourceReadRequest`], each answer waiting for the
//! next write), and changed only through its typed actions, which the device's daemon applies,
//! signs and seals. The session keeps no copy of its own: what it shows is the last reading.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, RESOURCE_PROVIDER, ReceiptStatus, RemoteActionTarget,
    ResourceReadRequest,
};
use tilo_lock::MutexRecover as _;
use tilo_resource::{Invoke, Resource};
use tilo_session_client::daemon_documents::DaemonShell;
use ui::permissions::{PermissionContext, PermissionScope, TreePath};

/// How long a follow waits for a write before it asks again.
const FOLLOW_WAIT_MS: u32 = 25_000;
/// How long a follow waits before it asks a daemon that did not answer again.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

/// What the last reading of a resource said.
#[derive(Clone, Default)]
enum Reading {
    /// Not read yet, or still on its way to this device.
    #[default]
    Arriving,
    /// Nothing wrote it yet.
    Absent,
    /// Its document, whole.
    Document(Vec<u8>),
}

/// The last reading, and how many readings moved it.
#[derive(Default)]
struct Latest {
    readings: u64,
    revision: Option<String>,
    copy: Reading,
}

/// How writes sent without waiting went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Went {
    /// The daemon made every one.
    Made,
    /// The daemon refused one, saying why: the ones after it were not sent, and the resource as
    /// the daemon holds it was read again.
    Refused(String),
    /// The daemon did not answer, or answered nothing settled: they may be made yet, and a
    /// caller asks again.
    Unreached(String),
}

impl Went {
    /// As a result: either kind of failure says why.
    pub(crate) fn result(self) -> Result<(), String> {
        match self {
            Self::Made => Ok(()),
            Self::Refused(why) | Self::Unreached(why) => Err(why),
        }
    }
}

/// Writes sent to the daemon without waiting ([`FollowedResource::send`]): how they went, once
/// they did.
#[derive(Clone, Default)]
pub(crate) struct Sending(Arc<(Mutex<Option<Went>>, std::sync::Condvar)>);

impl Sending {
    /// How they went: `None` while they are on their way.
    pub(crate) fn went(&self) -> Option<Went> {
        self.0.0.lock_recover().clone()
    }

    fn finish(&self, went: Went) {
        *self.0.0.lock_recover() = Some(went);
        self.0.1.notify_all();
    }

    /// Wait until they went: what a test takes its next step after.
    #[cfg(test)]
    pub(crate) fn wait(&self) -> Went {
        let (went, done) = &*self.0;
        let (went, timeout) = done
            .wait_timeout_while(
                went.lock_recover(),
                std::time::Duration::from_secs(120),
                |went| went.is_none(),
            )
            .expect("the writes' lock holds");
        assert!(!timeout.timed_out(), "the daemon answers the writes");
        went.clone().expect("they went")
    }
}

/// One followed resource of a workspace, of the type `P`. Cloning shares it; the follow stops once
/// the last clone goes.
pub(crate) struct FollowedResource<P: Resource> {
    shared: Rc<Shared<P>>,
}

impl<P: Resource> Clone for FollowedResource<P> {
    fn clone(&self) -> Self {
        Self {
            shared: Rc::clone(&self.shared),
        }
    }
}

struct Shared<P: Resource> {
    daemon: DaemonShell,
    workspace: String,
    app: &'static str,
    id: String,
    me: tilo_resource::MemberId,
    /// The grants it was followed at, where the session knows them: what decides whether the
    /// person may invoke an action.
    permissions: Option<PermissionContext>,
    latest: Arc<Mutex<Latest>>,
    /// Its state as of the reading it was made from.
    state: RefCell<Option<(u64, Rc<P::State>)>>,
    follow: tokio::task::AbortHandle,
    /// What tells the session the reading moved.
    wake: tilo_service::Wake,
    /// A read asked for now ([`FollowedResource::refresh`]) is on its way: another is not asked
    /// until it answers.
    refreshing: Arc<std::sync::atomic::AtomicBool>,
}

impl<P: Resource> Drop for Shared<P> {
    fn drop(&mut self) {
        self.follow.abort();
    }
}

/// A read of `app`'s `id` of `workspace`, whole, once it moved past `after`.
fn read_of(workspace: &str, app: &str, id: &str, after: Option<String>) -> ResourceReadRequest {
    ResourceReadRequest {
        workspace: workspace.to_owned(),
        app: app.to_owned(),
        record: id.to_owned(),
        joined: false,
        wait_ms: after.is_some().then_some(FOLLOW_WAIT_MS),
        after,
        answer: tilo_actions::remote::ReadAnswer::Document,
    }
}

/// Take `readings` in as what `latest` says now. Whether it moved.
fn take(latest: &Mutex<Latest>, readings: &serde_json::Value) -> bool {
    let revision = readings["revision"].as_str().map(str::to_owned);
    let copy = match tilo_session_client::daemon_documents::document_read(readings) {
        Ok(tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. }) => {
            Reading::Document(bytes)
        }
        Ok(tilo_peer_state::workspace_documents::DocumentRead::Absent) => Reading::Absent,
        Ok(_) => Reading::Arriving,
        Err(error) => {
            tracing::warn!(%error, "a resource's reading does not read");
            Reading::Arriving
        }
    };
    let mut latest = latest.lock_recover();
    if latest.revision.is_some() && latest.revision == revision {
        return false;
    }
    latest.revision = revision;
    latest.copy = copy;
    latest.readings = latest.readings.wrapping_add(1);
    true
}

impl<P: Resource> FollowedResource<P> {
    /// Follow `app`'s resource `id` of `workspace` from now on, as `person` at the grants
    /// `policy` gives them where the session reads them, calling `wake` each time it moved.
    pub(crate) fn follow(
        daemon: &DaemonShell,
        (workspace, app, id): (&str, &'static str, &str),
        person: &str,
        policy: Option<tilo_action_policy::WorkspacePolicy>,
        wake: tilo_service::Wake,
    ) -> Self {
        let latest = Arc::new(Mutex::new(Latest::default()));
        let follow = {
            let (daemon, latest, wake) = (daemon.clone(), Arc::clone(&latest), Arc::clone(&wake));
            let (workspace, id) = (workspace.to_owned(), id.to_owned());
            daemon.runtime().clone().spawn(async move {
                let mut after = None;
                loop {
                    match daemon
                        .read(read_of(&workspace, app, &id, after.clone()))
                        .await
                    {
                        Ok(readings) => {
                            after = readings["revision"].as_str().map(str::to_owned);
                            if take(&latest, &readings) {
                                wake();
                            }
                        }
                        Err(error) => {
                            tilo_failure::ignored!(
                                "a follow asks again a moment later",
                                tracing::debug!(%error, %workspace, app, "a followed resource did not read")
                            );
                            tokio::time::sleep(ASK_AGAIN).await;
                        }
                    }
                }
            })
        };
        let permissions = policy.map(|policy| {
            PermissionContext::from_access(tilo_action_policy::MemberAccess::person(
                policy,
                person.to_owned(),
            ))
            .with_policy_time_sampler(Arc::new(|| Ok(tilo_clock::unix_now())))
        });
        Self {
            shared: Rc::new(Shared {
                daemon: daemon.clone(),
                workspace: workspace.to_owned(),
                app,
                id: id.to_owned(),
                me: tilo_resource::MemberId::person(person.to_owned()),
                permissions,
                latest,
                state: RefCell::default(),
                follow: follow.abort_handle(),
                wake,
                refreshing: Arc::default(),
            }),
        }
    }

    /// Whether the workspace has the resource: `None` until it was read, or while it is on its way.
    pub(crate) fn held(&self) -> Option<bool> {
        match self.shared.latest.lock_recover().copy {
            Reading::Arriving => None,
            Reading::Absent => Some(false),
            Reading::Document(_) => Some(true),
        }
    }

    /// A number that moves each time the reading does.
    pub(crate) fn revision(&self) -> u64 {
        self.shared.latest.lock_recover().readings
    }

    /// Who the session reads and acts as.
    pub(crate) fn me(&self) -> &tilo_resource::MemberId {
        &self.shared.me
    }

    /// Its state as of the last reading; its first state while the workspace has none of it.
    pub(crate) fn get(&self) -> Rc<P::State> {
        let (readings, copy) = {
            let latest = self.shared.latest.lock_recover();
            (latest.readings, latest.copy.clone())
        };
        if let Some((at, state)) = self.shared.state.borrow().as_ref()
            && *at == readings
        {
            return Rc::clone(state);
        }
        let state = state_of::<P>(&copy);
        *self.shared.state.borrow_mut() = Some((readings, Rc::clone(&state)));
        state
    }

    /// Its document as of the last reading, with the history of its changes: `None` while the
    /// workspace has none of it, or it is on its way.
    pub(crate) fn document(&self) -> Option<automerge::AutoCommit> {
        let Reading::Document(bytes) = self.shared.latest.lock_recover().copy.clone() else {
            return None;
        };
        automerge::AutoCommit::load(&bytes)
            .inspect_err(|error| tracing::warn!(%error, kind = P::KIND, "a resource does not read"))
            .ok()
    }

    /// Whether the workspace's grants let the person invoke the action `verb`.
    pub(crate) fn may(&self, verb: &str) -> bool {
        let Some(declared) = P::declarations().into_iter().find(|decl| decl.id == verb) else {
            return false;
        };
        let route = ui::resource::DocumentId::parse(&self.shared.id)
            .map_or_else(|| self.shared.id.clone(), |id| id.route().to_owned());
        let node = tilo_actions::action_node(
            &TreePath::root(),
            self.shared.app,
            P::KIND,
            &route,
            declared.field,
            verb,
        );
        self.shared.permissions.as_ref().is_some_and(|permissions| {
            permissions.check(&PermissionScope::Action {
                node,
                kind: declared.kind,
            })
        })
    }

    /// Run `invoke` at the device's daemon, as the person: decided there by the workspace's
    /// grants, and written as this device's. Read again once it ran, so what it changed shows.
    ///
    /// # Errors
    /// The arguments do not bind, or the daemon refused the action, saying why.
    pub(crate) fn invoke(&self, invoke: &Invoke<P>) -> Result<(), String> {
        if let Some(refusal) = invoke.refusal() {
            return Err(refusal.to_owned());
        }
        self.run(invoke.address(), invoke.arguments.clone(), None)
    }

    /// Run the action `action` with `arguments` at the device's daemon, as `invoker` acting for
    /// the person: what an agent or a script asks of the resource.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub(crate) fn invoke_as(
        &self,
        invoker: &tilo_actions::ActionInvoker,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        self.run(
            tilo_actions::ActionAddress::new(P::KIND, action),
            arguments,
            Some(invoker.clone()),
        )
    }

    /// Run `invoke` at the device's daemon, as the person, without waiting for it: what the
    /// session says of itself on its loop, which must never wait on the daemon's store. `done`
    /// hears how it went, on the daemon link's runtime, once the resource was read again.
    pub(crate) fn invoke_unwaited(
        &self,
        invoke: &Invoke<P>,
        done: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        self.unwaited(std::slice::from_ref(invoke), |went| done(went.result()));
    }

    /// Run `invokes` at the device's daemon, in order, as the person, without waiting for them:
    /// one that is refused stops the rest. The resource is read again once they ran, so what
    /// [`Self::get`] reads by the time the answer says they went shows them, and the session is
    /// woken then.
    pub(crate) fn send(&self, invokes: &[Invoke<P>]) -> Sending {
        let sending = Sending::default();
        let (finished, wake) = (sending.clone(), Arc::clone(&self.shared.wake));
        self.unwaited(invokes, move |went| {
            finished.finish(went);
            wake();
        });
        sending
    }

    fn unwaited(&self, invokes: &[Invoke<P>], done: impl FnOnce(Went) + Send + 'static) {
        let requests: Result<Vec<(&'static str, InvokeRequest)>, String> = invokes
            .iter()
            .map(|invoke| match invoke.refusal() {
                Some(refusal) => Err(refusal.to_owned()),
                None => Ok((
                    invoke.id,
                    self.request(invoke.address(), invoke.arguments.clone(), None),
                )),
            })
            .collect();
        let requests = match requests {
            Ok(requests) => requests,
            Err(refusal) => {
                done(Went::Refused(refusal));
                return;
            }
        };
        let shared = &self.shared;
        let (daemon, workspace, latest, wake) = (
            shared.daemon.clone(),
            shared.workspace.clone(),
            Arc::clone(&shared.latest),
            Arc::clone(&shared.wake),
        );
        let read = read_of(&shared.workspace, shared.app, &shared.id, None);
        shared.daemon.runtime().spawn(async move {
            let mut went = Went::Made;
            for (action, request) in requests {
                went = match daemon.invoke_receipt(Some(&workspace), request).await {
                    Ok(receipt) => match receipt.status {
                        ReceiptStatus::Completed { .. } => Went::Made,
                        ReceiptStatus::Rejected { error } => Went::Refused(error.to_string()),
                        ReceiptStatus::Failed { error } => Went::Refused(error.message),
                        status => Went::Unreached(format!("the action did not settle: {status:?}")),
                    },
                    Err(error) => Went::Unreached(error),
                };
                if let Went::Refused(error) | Went::Unreached(error) = &went {
                    tracing::warn!(%error, kind = P::KIND, action, "a resource was not changed");
                    break;
                }
            }
            // What they changed, or what a refusal left, shows before the caller hears how they
            // went.
            match daemon.read(read).await {
                Ok(readings) => {
                    if take(&latest, &readings) {
                        wake();
                    }
                }
                Err(error) => tilo_failure::ignored!(
                    "the follow reads what the writes changed a moment later",
                    tracing::debug!(%error, "a resource was not read again after its writes")
                ),
            }
            done(went);
        });
    }

    /// Read it again now and wait for the answer: what a test reads its next step against.
    #[cfg(test)]
    pub(crate) fn read_now(&self) {
        let shared = &self.shared;
        let readings = shared
            .daemon
            .read_blocking(read_of(&shared.workspace, shared.app, &shared.id, None))
            .expect("the resource reads");
        take(&shared.latest, &readings);
    }

    fn run(
        &self,
        address: tilo_actions::ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
        invoker: Option<tilo_actions::ActionInvoker>,
    ) -> Result<(), String> {
        let request = self.request(address, arguments, invoker);
        self.shared
            .daemon
            .invoke_blocking(&self.shared.workspace, request)?;
        self.refresh();
        Ok(())
    }

    /// The request that runs `address` with `arguments` on this resource, as `invoker` where one
    /// acts for the person.
    fn request(
        &self,
        address: tilo_actions::ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
        invoker: Option<tilo_actions::ActionInvoker>,
    ) -> InvokeRequest {
        let shared = &self.shared;
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: format!("{RESOURCE_PROVIDER}:{}:{}", shared.app, shared.id),
                address,
            },
            argument: InvokeArgument::Json(arguments),
            context: invoker
                .map(|invoker| tilo_actions::ActionContext::default().with_invoker(invoker)),
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// Ask for its reading again now, rather than when the follow next reads it: what a change
    /// just made shows as soon as the daemon answers, and the session is woken then. The caller
    /// never waits for the daemon, whose store may be slow; until the answer comes, the last
    /// reading stands.
    pub(crate) fn refresh(&self) {
        let shared = &self.shared;
        if shared
            .refreshing
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let (daemon, latest, wake, refreshing) = (
            shared.daemon.clone(),
            Arc::clone(&shared.latest),
            Arc::clone(&shared.wake),
            Arc::clone(&shared.refreshing),
        );
        let read = read_of(&shared.workspace, shared.app, &shared.id, None);
        shared.daemon.runtime().spawn(async move {
            let read = daemon.read(read).await;
            refreshing.store(false, std::sync::atomic::Ordering::Release);
            match read {
                Ok(readings) => {
                    if take(&latest, &readings) {
                        wake();
                    }
                }
                Err(error) => {
                    // A resource read on every pass may answer before this device holds it, or
                    // the daemon may be busy: the follow asks again a moment later, so this is
                    // not the person's to see.
                    tilo_failure::ignored!(
                        "a resource read now reads when the follow does",
                        tracing::debug!(%error, "a resource asked for now did not read")
                    );
                }
            }
        });
    }
}

/// `P`'s state as `copy` holds it: its first state while there is no document to read.
#[allow(
    clippy::panic,
    reason = "a type whose first state does not read is a bug in it, never a state to show"
)]
fn state_of<P: Resource>(copy: &Reading) -> Rc<P::State> {
    if let Reading::Document(bytes) = copy {
        match automerge::AutoCommit::load(bytes)
            .map_err(|error| error.to_string())
            .and_then(|doc| {
                tilo_resource::Reader::<P>::read(&doc).map_err(|error| error.to_string())
            }) {
            Ok(reader) => return Rc::clone(reader.state()),
            Err(error) => tracing::warn!(%error, kind = P::KIND, "a resource does not read"),
        }
    }
    let mut doc = automerge::AutoCommit::new();
    let initialized = P::initialize(&mut doc).map_err(|error| error.to_string());
    initialized
        .and_then(|()| tilo_resource::Reader::<P>::read(&doc).map_err(|error| error.to_string()))
        .map_or_else(
            |error| panic!("{}'s first state does not read: {error}", P::KIND),
            |reader| Rc::clone(reader.state()),
        )
}

/// A resource the device's daemon publishes of its own, as a reading of `T` (where this
/// computer's folders stand): followed through the same read as [`FollowedResource`], each answer
/// waiting for the next change. The session keeps no copy of its own: what it shows is the last
/// reading. Cloning shares it; the follow stops once the last clone goes.
pub(crate) struct FollowedReading<T> {
    shared: Arc<ReadingShared<T>>,
}

impl<T> Clone for FollowedReading<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

struct ReadingShared<T> {
    /// How many readings moved it, and the last one.
    latest: Arc<Mutex<(u64, T)>>,
    follow: tokio::task::AbortHandle,
}

impl<T> Drop for ReadingShared<T> {
    fn drop(&mut self) {
        self.follow.abort();
    }
}

impl<T> FollowedReading<T>
where
    T: serde::de::DeserializeOwned + Default + Clone + Send + 'static,
{
    /// Follow the daemon's own resource `app` of `workspace` from now on, calling `wake` each
    /// time it moved.
    pub(crate) fn follow(
        daemon: &DaemonShell,
        (workspace, app): (&str, &'static str),
        wake: tilo_service::Wake,
    ) -> Self {
        let latest = Arc::new(Mutex::new((0_u64, T::default())));
        let follow = {
            let (daemon, latest) = (daemon.clone(), Arc::clone(&latest));
            let workspace = workspace.to_owned();
            daemon.runtime().clone().spawn(async move {
                let mut after = None;
                loop {
                    match daemon
                        .read(reading_of(&workspace, app, after.clone()))
                        .await
                    {
                        Ok(readings) => {
                            after = readings["revision"].as_str().map(str::to_owned);
                            if let Some(now) = reading_value::<T>(&readings) {
                                let mut latest = latest.lock_recover();
                                latest.0 = latest.0.wrapping_add(1);
                                latest.1 = now;
                                drop(latest);
                                wake();
                            }
                        }
                        Err(error) => {
                            tilo_failure::ignored!(
                                "a follow asks again a moment later",
                                tracing::debug!(%error, app, "a followed reading did not read")
                            );
                            tokio::time::sleep(ASK_AGAIN).await;
                        }
                    }
                }
            })
        };
        Self {
            shared: Arc::new(ReadingShared {
                latest,
                follow: follow.abort_handle(),
            }),
        }
    }

    /// A number that moves each time the reading does.
    pub(crate) fn revision(&self) -> u64 {
        self.shared.latest.lock_recover().0
    }

    /// The last reading; `T`'s default before the first.
    pub(crate) fn get(&self) -> T {
        self.shared.latest.lock_recover().1.clone()
    }
}

/// A read of the daemon's own resource `app` of `workspace`, once it moved past `after`.
fn reading_of(workspace: &str, app: &str, after: Option<String>) -> ResourceReadRequest {
    ResourceReadRequest {
        workspace: workspace.to_owned(),
        app: app.to_owned(),
        record: String::new(),
        joined: false,
        wait_ms: after.is_some().then_some(FOLLOW_WAIT_MS),
        after,
        answer: tilo_actions::remote::ReadAnswer::Reading,
    }
}

/// The reading `readings` answers, as `T`: its default for a resource nothing published, and
/// `None` while it is on its way or does not read.
fn reading_value<T: serde::de::DeserializeOwned + Default>(
    readings: &serde_json::Value,
) -> Option<T> {
    let record = readings["records"].get(0)?;
    if record["absent"].as_bool() == Some(true) {
        return Some(T::default());
    }
    match serde_json::from_value(record.get("value")?.clone()) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%error, "a followed reading does not read");
            None
        }
    }
}
