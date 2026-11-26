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
use tilo_ui_base::{
    permissions::PermissionContext, permissions::PermissionScope, permissions::TreePath,
};

/// How long a follow waits for a write before it asks again.
const FOLLOW_WAIT_MS: u32 = 25_000;
/// How long a change asked of the daemon is waited for: a linked daemon answers at once, and one
/// that does not is not waited on forever.
const INVOKE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
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
/// the last clone, and the last [`SharedFollow`] of it, goes.
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
    core: Arc<Core>,
    me: tilo_resource::MemberId,
    /// The grants it was followed at, where the session knows them: what decides whether the
    /// person may invoke an action.
    permissions: Option<PermissionContext>,
    /// Its state as of the reading it was made from.
    state: RefCell<Option<(u64, Rc<P::State>)>>,
}

/// What one follow is, whichever thread reads it: the resource it reads, the last reading, and
/// the way to the daemon that changes it. The session's thread holds it through a
/// [`FollowedResource`]; a part of the session that runs elsewhere holds the same follow through a
/// [`SharedFollow`].
struct Core {
    daemon: DaemonShell,
    workspace: String,
    app: &'static str,
    id: String,
    readings: Arc<Readings>,
    follow: tokio::task::AbortHandle,
    /// A read asked for now ([`Core::refresh`]) is on its way: another is not asked until it
    /// answers.
    refreshing: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Core {
    fn drop(&mut self) {
        self.follow.abort();
    }
}

/// The last reading of a follow, and who hears that it moved.
struct Readings {
    latest: Mutex<Latest>,
    /// Moves to the count of readings each time the reading moves: what a part of the session off
    /// its thread waits on.
    moved: tokio::sync::watch::Sender<u64>,
    /// What tells the session the reading moved.
    wake: tilo_service::Wake,
}

impl Readings {
    /// Take `readings` in as what the resource says now, telling whoever follows it when it moved.
    fn take(&self, readings: &serde_json::Value) {
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
        let moved = {
            let mut latest = self.latest.lock_recover();
            if latest.revision.is_some() && latest.revision == revision {
                return;
            }
            latest.revision = revision;
            latest.copy = copy;
            latest.readings = latest.readings.wrapping_add(1);
            latest.readings
        };
        self.moved.send_replace(moved);
        (self.wake)();
    }

    fn held(&self) -> Option<bool> {
        match self.latest.lock_recover().copy {
            Reading::Arriving => None,
            Reading::Absent => Some(false),
            Reading::Document(_) => Some(true),
        }
    }

    fn revision(&self) -> u64 {
        self.latest.lock_recover().readings
    }

    /// How many readings moved it, and the last one.
    fn last(&self) -> (u64, Reading) {
        let latest = self.latest.lock_recover();
        (latest.readings, latest.copy.clone())
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
        let readings = Arc::new(Readings {
            latest: Mutex::new(Latest::default()),
            moved: tokio::sync::watch::Sender::new(0),
            wake,
        });
        let follow = {
            let (daemon, readings) = (daemon.clone(), Arc::clone(&readings));
            let (workspace, id) = (workspace.to_owned(), id.to_owned());
            daemon.runtime().clone().spawn(async move {
                let mut after = None;
                loop {
                    match daemon
                        .read(read_of(&workspace, app, &id, after.clone()))
                        .await
                    {
                        Ok(answer) => {
                            after = answer
                                .get("revision")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned);
                            readings.take(&answer);
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
                core: Arc::new(Core {
                    daemon: daemon.clone(),
                    workspace: workspace.to_owned(),
                    app,
                    id: id.to_owned(),
                    readings,
                    follow: follow.abort_handle(),
                    refreshing: Arc::default(),
                }),
                me: tilo_resource::MemberId::person(person.to_owned()),
                permissions,
                state: RefCell::default(),
            }),
        }
    }

    /// The same follow, for a part of the session that reads and changes the resource off the
    /// session's thread: it reads the one reading this does, and keeps the follow going while
    /// it is held.
    pub(crate) fn shared(&self) -> SharedFollow<P> {
        SharedFollow {
            core: Arc::clone(&self.shared.core),
            state: Arc::default(),
            kind: std::marker::PhantomData,
        }
    }

    /// Whether the workspace has the resource: `None` until it was read, or while it is on its way.
    pub(crate) fn held(&self) -> Option<bool> {
        self.shared.core.readings.held()
    }

    /// A number that moves each time the reading does.
    pub(crate) fn revision(&self) -> u64 {
        self.shared.core.readings.revision()
    }

    /// Who the session reads and acts as.
    pub(crate) fn me(&self) -> &tilo_resource::MemberId {
        &self.shared.me
    }

    /// Its state as of the last reading; its first state while the workspace has none of it.
    pub(crate) fn get(&self) -> Rc<P::State> {
        let (readings, copy) = self.shared.core.readings.last();
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
        let (_, Reading::Document(bytes)) = self.shared.core.readings.last() else {
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
        let core = &self.shared.core;
        let route = ui::resource::DocumentId::parse(&core.id)
            .map_or_else(|| core.id.clone(), |id| id.route().to_owned());
        let node = tilo_actions::action_node(
            &TreePath::root(),
            core.app,
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
        self.shared
            .core
            .unwaited(std::slice::from_ref(invoke), |went| done(went.result()));
    }

    /// Run `invokes` at the device's daemon, in order, as the person, without waiting for them:
    /// one that is refused stops the rest. The resource is read again once they ran, so what
    /// [`Self::get`] reads by the time the answer says they went shows them, and the session is
    /// woken then.
    pub(crate) fn send(&self, invokes: &[Invoke<P>]) -> Sending {
        let sending = Sending::default();
        let (finished, wake) = (sending.clone(), Arc::clone(&self.shared.core.readings.wake));
        self.shared.core.unwaited(invokes, move |went| {
            finished.finish(went);
            wake();
        });
        sending
    }

    /// Read it again now and wait for the answer: what a test reads its next step against.
    #[cfg(test)]
    pub(crate) fn read_now(&self) {
        self.shared.core.read_now();
    }

    fn run(
        &self,
        address: tilo_actions::ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
        invoker: Option<tilo_actions::ActionInvoker>,
    ) -> Result<(), String> {
        let core = &self.shared.core;
        let request = core.request(address, arguments, invoker);
        went_by(
            core.daemon
                .invoke_receipt_blocking(Some(&core.workspace), request),
        )
        .result()?;
        core.refresh();
        Ok(())
    }

    /// Ask for its reading again now, rather than when the follow next reads it: what a change
    /// just made shows as soon as the daemon answers, and the session is woken then. The caller
    /// never waits for the daemon, whose store may be slow; until the answer comes, the last
    /// reading stands.
    pub(crate) fn refresh(&self) {
        self.shared.core.refresh();
    }
}

/// How an action went, by its receipt. One the daemon took up without finishing (the person's
/// write to their own workspace before this device holds it) is made once it can be: a caller
/// that waits for it hears that, rather than waiting on a device that may never be taken in.
fn went_by(receipt: Result<tilo_actions::remote::InvocationReceipt, String>) -> Went {
    match receipt.map(|receipt| receipt.status) {
        Ok(ReceiptStatus::Completed { .. }) => Went::Made,
        Ok(ReceiptStatus::Rejected { error }) => Went::Refused(error.to_string()),
        Ok(ReceiptStatus::Failed { error }) => Went::Refused(error.message),
        Ok(ReceiptStatus::Accepted) => {
            Went::Unreached("the action was taken up and has not finished yet".to_owned())
        }
        Ok(ReceiptStatus::Cancelled { message }) => {
            Went::Refused(message.unwrap_or_else(|| "the action was stopped".to_owned()))
        }
        Err(error) => Went::Unreached(error),
    }
}

/// How the write `invocation`, answered [`ReceiptStatus::Accepted`], went: the daemon's record of
/// the invocation says once it ran, or why it could not, so a caller that follows one hears
/// either rather than taking the write as made. A daemon that keeps no record of it is one whose
/// provider answered at once: the write is then heard as this resource's reading moving.
async fn taken_up(
    daemon: &DaemonShell,
    workspace: &str,
    invocation: tilo_actions::InvocationId,
    moved: &mut tokio::sync::watch::Receiver<u64>,
) -> Went {
    loop {
        match daemon
            .observe(Some(workspace), invocation, Some(FOLLOW_WAIT_MS))
            .await
        {
            Ok(Some(state)) => match state.update {
                tilo_actions::InvocationUpdate::Succeeded { .. } => return Went::Made,
                tilo_actions::InvocationUpdate::Failed { error, .. } => {
                    return Went::Refused(error.message);
                }
                tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                    return Went::Refused(
                        message.unwrap_or_else(|| "the action was stopped".to_owned()),
                    );
                }
                tilo_actions::InvocationUpdate::Accepted { .. }
                | tilo_actions::InvocationUpdate::Progress { .. }
                | tilo_actions::InvocationUpdate::Item { .. } => {}
            },
            Ok(None) => {
                return match moved.changed().await {
                    Ok(()) => Went::Made,
                    Err(_) => Went::Unreached("the follow stopped".to_owned()),
                };
            }
            Err(error) => return Went::Unreached(error),
        }
    }
}

impl Core {
    /// Run `invokes` at the daemon, in order, without waiting for them, and read the resource
    /// again once they ran; `done` hears how they went then.
    fn unwaited<P: Resource>(
        &self,
        invokes: &[Invoke<P>],
        done: impl FnOnce(Went) + Send + 'static,
    ) {
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
        let (daemon, workspace, readings) = (
            self.daemon.clone(),
            self.workspace.clone(),
            Arc::clone(&self.readings),
        );
        let read = read_of(&self.workspace, self.app, &self.id, None);
        // Seen before the first is sent, so a write taken up and made before its receipt is
        // looked at still counts as having moved the reading.
        let mut moved = self.readings.moved.subscribe();
        moved.borrow_and_update();
        self.daemon.runtime().spawn(async move {
            let mut went = Went::Made;
            for (action, request) in requests {
                let invocation = request.invocation;
                let receipt = daemon.invoke_receipt(Some(&workspace), request).await;
                went = match receipt {
                    // Taken up and made once the daemon can (the person's write to their own
                    // workspace before the device holds it): it is never sent again meanwhile,
                    // and the daemon's record of the invocation says once it ran, or why it
                    // could not.
                    Ok(receipt) if receipt.status == ReceiptStatus::Accepted => {
                        taken_up(&daemon, &workspace, invocation, &mut moved).await
                    }
                    receipt => went_by(receipt),
                };
                if let Went::Refused(error) | Went::Unreached(error) = &went {
                    tracing::warn!(%error, kind = P::KIND, action, "a resource was not changed");
                    break;
                }
            }
            // What they changed, or what a refusal left, shows before the caller hears how they
            // went.
            match daemon.read(read).await {
                Ok(answer) => readings.take(&answer),
                Err(error) => tilo_failure::ignored!(
                    "the follow reads what the writes changed a moment later",
                    tracing::debug!(%error, "a resource was not read again after its writes")
                ),
            }
            done(went);
        });
    }

    /// The request that runs `address` with `arguments` on this resource, as `invoker` where one
    /// acts for the person.
    fn request(
        &self,
        address: tilo_actions::ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
        invoker: Option<tilo_actions::ActionInvoker>,
    ) -> InvokeRequest {
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: format!("{RESOURCE_PROVIDER}:{}:{}", self.app, self.id),
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

    /// Ask for the reading again now; see [`FollowedResource::refresh`].
    fn refresh(&self) {
        if self
            .refreshing
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let (daemon, readings, refreshing) = (
            self.daemon.clone(),
            Arc::clone(&self.readings),
            Arc::clone(&self.refreshing),
        );
        let read = read_of(&self.workspace, self.app, &self.id, None);
        self.daemon.runtime().spawn(async move {
            let read = daemon.read(read).await;
            refreshing.store(false, std::sync::atomic::Ordering::Release);
            match read {
                Ok(answer) => readings.take(&answer),
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

    /// Read it again now and wait for the answer.
    #[cfg(test)]
    fn read_now(&self) {
        let answer = self
            .daemon
            .read_blocking(read_of(&self.workspace, self.app, &self.id, None))
            .expect("the resource reads");
        self.readings.take(&answer);
    }
}

/// A state as of the reading it was made from, by that reading's number.
type SharedState<S> = Arc<Mutex<Option<(u64, Arc<S>)>>>;

/// A [`FollowedResource`]'s follow as a part of the session that runs off the session's thread
/// holds it: the network actor answering another device's boot, the link onto the person's other
/// devices, the watcher of who drives whose keyboard. It reads the one reading the session reads,
/// and changes the resource the one way the session does, through its typed actions at the
/// daemon. Cloning shares it. It is `pub` only so the session's entry into the peer network
/// ([`crate::sync::Entry`]) can carry it; nothing outside the session names it.
pub struct SharedFollow<P: Resource> {
    core: Arc<Core>,
    /// Its state as of the reading it was made from.
    state: SharedState<P::State>,
    kind: std::marker::PhantomData<fn() -> P>,
}

impl<P: Resource> Clone for SharedFollow<P> {
    fn clone(&self) -> Self {
        Self {
            core: Arc::clone(&self.core),
            state: Arc::clone(&self.state),
            kind: std::marker::PhantomData,
        }
    }
}

impl<P: Resource> SharedFollow<P>
where
    P::State: Clone + Send + Sync,
{
    /// Its state as of the last reading; its first state while the workspace has none of it.
    pub(crate) fn get(&self) -> Arc<P::State> {
        let (readings, copy) = self.core.readings.last();
        let mut kept = self.state.lock_recover();
        if let Some((at, state)) = kept.as_ref()
            && *at == readings
        {
            return Arc::clone(state);
        }
        let state = Arc::new(P::State::clone(&state_of::<P>(&copy)));
        *kept = Some((readings, Arc::clone(&state)));
        state
    }

    /// A number that moves each time the reading does.
    pub(crate) fn revision(&self) -> u64 {
        self.core.readings.revision()
    }

    /// Moves each time the reading does, to [`Self::revision`]: what a watcher off the session's
    /// thread waits on.
    pub(crate) fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.core.readings.moved.subscribe()
    }

    /// Run `invoke` at the device's daemon, as the person, without waiting for it; `done` hears
    /// how it went, on the daemon link's runtime, once the resource was read again.
    pub(crate) fn invoke_unwaited(
        &self,
        invoke: &Invoke<P>,
        done: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        self.core
            .unwaited(std::slice::from_ref(invoke), |went| done(went.result()));
    }

    /// Run `invoke` at the device's daemon and wait for how it went: for a part of the session
    /// that changes the resource from another thread, where the answer is what the caller needs.
    ///
    /// # Errors
    /// The daemon refused the action, saying why, or did not answer in time.
    pub(crate) fn invoke(&self, invoke: &Invoke<P>) -> Result<(), String> {
        let (finished, answered) = std::sync::mpsc::sync_channel(1);
        self.invoke_unwaited(invoke, move |went| {
            let _ = finished.send(went);
        });
        answered
            .recv_timeout(INVOKE_WAIT)
            .map_err(|_| "this device's daemon did not answer in time".to_owned())?
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
    // A record of no workspace's, made only to read the type's first state.
    tilo_resource::founding::new_record::<P>("", "")
        .and_then(|doc| tilo_resource::Reader::<P>::read(&doc))
        .map_err(|error| error.to_string())
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
                            after = readings
                                .get("revision")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned);
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
