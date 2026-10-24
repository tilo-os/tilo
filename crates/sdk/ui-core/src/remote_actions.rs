//! An application's own client for the session's action door.
//!
//! [`RemoteActions`] lets an app read the action catalogue and invoke
//! published actions through the session broker — the same door the
//! palette, scripts and agents use. The broker decides every request for the
//! application under its own identity: it reaches what it owns (its own
//! surface, and actions published under its own package id) as the person
//! using it; reading the catalogue needs the manifest's `[actions] query`,
//! and invoking another app's action needs `[actions] invoke` plus a
//! workspace grant to the app. Nothing here widens that: the client only
//! carries requests and answers.
//!
//! Answers arrive as [`Arriving`] reads ([`crate::arriving`]), so a bound
//! view updates, an idle window wakes when one lands, and
//! [`Arriving::view`] shows them with the framework's placeholders. Each
//! read says where its answer comes from: this device's broker, or the
//! room's host for the room's own governance. A root that owns a client returns it
//! as its [`Collaborative`] capability
//! (or forwards to it), which is how its requests reach the connection and
//! the answers come back.
//!
//! [`RemoteActions::with_host`] serves the same client from an in-process
//! [`ActionHost`] instead, for tests and embedded hosts.

use crate::time::{Duration, Instant};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use tilo_actions::InvocationUpdate;
use tilo_actions::outcome::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, Confirmation,
    InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE, ROOM_GOVERNANCE_PROVIDER, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget, UpdateMark, serve,
};

use crate::arriving::{Arrival, ArrivalWriter, Arriving, Source};
use crate::collaboration::{CollabEvent, CollabRequest};
use crate::wake::WakeHandle;
use crate::widget::Collaborative;
use tilo_lock::MutexRecover as _;

type Served = (Arc<dyn ActionHost + Send + Sync>, Caller);

thread_local! {
    static SERVED: std::cell::RefCell<Option<Served>> = const { std::cell::RefCell::new(None) };
}

/// Keeps a [`RemoteActions::enter`] host current on this thread.
pub struct RemoteActionsGuard {
    previous: Option<Served>,
    thread_affinity: std::marker::PhantomData<*const ()>,
}

impl Drop for RemoteActionsGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        SERVED.with(|served| *served.borrow_mut() = previous);
    }
}

/// How long an invocation's receipt may be held for its outcome, so a quick
/// action answers in the one round trip that started it.
const INVOKE_WAIT_MS: u32 = 2_000;
/// How long one waiting observe may be held (the host caps it too). A host
/// answers sooner the moment the invocation moves.
const OBSERVE_WAIT_MS: u32 = 10_000;
/// How long a running invocation is left before it is observed again, for
/// a host that answers a waiting observe at once: one that predates waits.
const OBSERVE_INTERVAL: Duration = Duration::from_millis(200);
/// A remote publisher cannot keep an application walking an unbounded or
/// cyclic catalogue. Sixty-four full pages still cover the largest normal
/// application surfaces while giving every caller a deterministic refusal.
const MAX_CATALOGUE_PAGES: usize = 64;
/// How long one catalogue page may go unanswered. A connection that carries
/// the request nowhere (no host in reach to answer it) must not leave the
/// app waiting forever: past this, the read is refused as unavailable.
pub const CATALOGUE_WAIT: Duration = Duration::from_secs(20);
/// Why a catalogue read was refused when nothing answered it in time.
pub const CATALOGUE_UNANSWERED: &str =
    "nothing in reach answered the request for the actions apps offer";

/// A catalogue read: every page of one query, gathered.
pub type CatalogueCall = Arriving<CataloguePage, ActionHostError>;
/// An invocation: its latest lifecycle update, or the host's refusal (then
/// nothing ran).
pub type InvocationCall = Arriving<InvocationUpdate, ActionHostError>;
/// A request to cancel a running invocation: `Ready(())` is the host's
/// acknowledgement. Acknowledgement is not a terminal invocation outcome;
/// callers must keep observing the original [`InvocationCall`] until it
/// reports `Cancelled` or another terminal state.
pub type CancellationCall = Arriving<(), ActionHostError>;

impl Arrival<InvocationUpdate, ActionHostError> {
    /// Whether the invocation can no longer change: refused, or finished.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        match self {
            Self::Pending { .. } => false,
            Self::Ready(update) => update.is_terminal(),
            Self::Failed(_) => true,
        }
    }
}

/// A live result [`RemoteActions::follow`] follows. Dropping it stops
/// following: the invocation is cancelled.
pub struct Followed {
    remote: RemoteActions,
    invocation: InvocationId,
    answer: InvocationCall,
}

impl Followed {
    /// The invocation's latest update: each new result is an `Item`.
    #[must_use]
    pub const fn call(&self) -> &InvocationCall {
        &self.answer
    }

    /// The invocation it follows.
    #[must_use]
    pub const fn invocation(&self) -> InvocationId {
        self.invocation
    }
}

impl Drop for Followed {
    fn drop(&mut self) {
        if !self.answer.with(Arrival::is_finished) {
            let _ = self.remote.cancel(self.invocation);
        }
    }
}

impl std::fmt::Debug for Followed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Followed")
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

enum Transport {
    /// Through the application's collaboration connection.
    Session,
    /// Through an in-process host, as `caller`.
    Host {
        host: Arc<dyn ActionHost + Send + Sync>,
        caller: Caller,
    },
}

struct CatalogueWalk {
    query: CatalogueQuery,
    gathered: CataloguePage,
    answer: ArrivalWriter<CataloguePage, ActionHostError>,
    pages_remaining: usize,
    seen_cursors: HashSet<(Option<String>, Vec<u8>)>,
    follow_pages: bool,
    /// When the page asked for last is refused for want of an answer.
    due: Instant,
}

struct Running {
    answer: ArrivalWriter<InvocationUpdate, ActionHostError>,
    /// Told each update as it lands, before the call shows it: how a
    /// follower reads each result once.
    then: Option<Box<dyn FnMut(&Arrival<InvocationUpdate, ActionHostError>) + Send>>,
    cancellation: Option<CancellationCall>,
    /// When to observe it next, while a host that does not wait is still
    /// running it.
    observe_at: Option<Instant>,
    /// The waiting observe in flight: when it was sent, and how far it had
    /// seen the invocation get.
    waiting: Option<(Instant, UpdateMark)>,
}

impl Running {
    fn publish(&mut self, update: Arrival<InvocationUpdate, ActionHostError>) {
        if let Some(then) = &mut self.then {
            then(&update);
        }
        match update {
            Arrival::Ready(update) => self.answer.ready(update),
            Arrival::Failed(error) => self.answer.fail(error),
            Arrival::Pending { .. } => {}
        }
    }
}

struct Inner {
    transport: Transport,
    outbox: Vec<RemoteActionMessage>,
    next_correlation: u64,
    catalogue: HashMap<u64, CatalogueWalk>,
    invocations: HashMap<InvocationId, Running>,
    /// Whether the host holds a waiting observe until something changes;
    /// cleared the first time it answers one at once with nothing new.
    host_waits: bool,
    wake: WakeHandle,
}

/// An application's client for the session's action door. Clones share one
/// client.
#[derive(Clone)]
pub struct RemoteActions {
    inner: Arc<Mutex<Inner>>,
}

impl Default for RemoteActions {
    fn default() -> Self {
        Self::session()
    }
}

impl std::fmt::Debug for RemoteActions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteActions").finish_non_exhaustive()
    }
}

impl RemoteActions {
    /// A client that talks through the application's collaboration
    /// connection. Until the root hands it to the host (see the module
    /// docs), its requests wait.
    #[must_use]
    pub fn session() -> Self {
        Self::new(Transport::Session)
    }

    /// A client served by `host` in this process, as `caller`.
    #[must_use]
    pub fn with_host(host: Arc<dyn ActionHost + Send + Sync>, caller: Caller) -> Self {
        Self::new(Transport::Host { host, caller })
    }

    /// The client this thread's app should use: one served by the host a
    /// [`RemoteActions::enter`] guard installed, else [`session`](Self::session).
    #[must_use]
    pub fn current() -> Self {
        SERVED
            .with(|served| served.borrow().clone())
            .map_or_else(Self::session, |(host, caller)| {
                Self::with_host(host, caller)
            })
    }

    /// Serve every client [`current`](Self::current) makes on this thread
    /// from `host`, as `caller`, until the guard drops — how a test or an
    /// embedding host stands in for the session.
    #[must_use = "the host serves clients only while the guard lives"]
    pub fn enter(host: Arc<dyn ActionHost + Send + Sync>, caller: Caller) -> RemoteActionsGuard {
        let previous = SERVED.with(|served| served.replace(Some((host, caller))));
        RemoteActionsGuard {
            previous,
            thread_affinity: std::marker::PhantomData,
        }
    }

    fn new(transport: Transport) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                transport,
                outbox: Vec::new(),
                next_correlation: 1,
                catalogue: HashMap::new(),
                invocations: HashMap::new(),
                host_waits: true,
                wake: WakeHandle::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock_recover()
    }

    /// Read every page of `query` from the catalogue the app is offered.
    #[must_use]
    pub fn catalogue(&self, query: CatalogueQuery) -> CatalogueCall {
        self.start_catalogue(query, true)
    }

    /// Read exactly one bounded page of `query`.
    ///
    /// This is for clients that traverse a resource hierarchy and therefore
    /// must own one global page/work budget across parents. The returned
    /// cursor is opaque and may only be sent back with the same provider,
    /// parent and revision.
    #[must_use]
    pub fn catalogue_page(&self, query: CatalogueQuery) -> CatalogueCall {
        self.start_catalogue(query, false)
    }

    fn start_catalogue(&self, query: CatalogueQuery, follow_pages: bool) -> CatalogueCall {
        let client = self.clone();
        let asked = query.clone();
        let answer = Arriving::pending(self.source(None)).on_retry(move |answer| {
            client.ask_catalogue(answer, asked.clone(), follow_pages);
        });
        self.ask_catalogue(&answer, query, follow_pages);
        answer
    }

    /// Ask for `query`'s catalogue into `answer`: first, or again after a
    /// failure.
    fn ask_catalogue(&self, answer: &CatalogueCall, query: CatalogueQuery, follow_pages: bool) {
        let writer = answer.writer();
        writer.pending(self.source(None));
        let background = crate::background::Background::current();
        let due = background.now() + CATALOGUE_WAIT;
        let message = {
            let mut inner = self.lock();
            background.wake_after(CATALOGUE_WAIT, inner.wake.clone());
            let correlation = inner.next_correlation;
            inner.next_correlation = inner.next_correlation.wrapping_add(1);
            let mut first = query.clone();
            if first.provider.is_some() || first.max_results != 0 {
                first.max_results = first.max_results.clamp(1, MAX_CATALOGUE_PAGE);
            }
            let seen_cursors = HashSet::from([(first.revision.clone(), first.cursor.clone())]);
            inner.catalogue.insert(
                correlation,
                CatalogueWalk {
                    query: first.clone(),
                    gathered: CataloguePage::default(),
                    answer: writer,
                    pages_remaining: MAX_CATALOGUE_PAGES,
                    seen_cursors,
                    follow_pages,
                    due,
                },
            );
            RemoteActionMessage::CatalogueRequest {
                correlation,
                query: first,
            }
        };
        self.send(message);
    }

    /// Where this client's answer to a request for `target` (none: the
    /// catalogue) comes from. An in-process host and the session's broker
    /// are this device; a room's own governance is answered by the room's
    /// host, named as the person knows its workspace.
    fn source(&self, target: Option<&RemoteActionTarget>) -> Source {
        let session = matches!(self.lock().transport, Transport::Session);
        let governance = target.is_some_and(|target| target.provider == ROOM_GOVERNANCE_PROVIDER);
        if session && governance {
            if let Some(sync) = crate::room_sync::current() {
                let workspace = sync.workspace.trim();
                if !workspace.is_empty() {
                    return Source::host(workspace);
                }
            }
        }
        Source::Local
    }

    /// Invoke the action at `target` with `argument`, carrying the person's
    /// explicit `confirmation` when its contract asks for one.
    #[must_use]
    pub fn invoke(
        &self,
        target: RemoteActionTarget,
        argument: InvokeArgument,
        confirmation: Option<Confirmation>,
    ) -> InvocationCall {
        self.invoke_with_id(InvocationId::new(), target, argument, confirmation)
    }

    /// Invoke an action with a caller-owned stable identity.
    ///
    /// Use this when the durable object that caused the request already has a
    /// UUID. Reusing that UUID across reconnects lets the host deduplicate a
    /// retry after an acknowledgement is lost; it does not grant any authority.
    #[must_use]
    pub fn invoke_with_id(
        &self,
        invocation: InvocationId,
        target: RemoteActionTarget,
        argument: InvokeArgument,
        confirmation: Option<Confirmation>,
    ) -> InvocationCall {
        self.invoke_continuing(None, invocation, target, argument, confirmation)
    }

    /// [`Self::invoke_with_id`] for a request made while running
    /// `on_behalf_of`, an invocation the host delivered here: the host
    /// decides it as that invocation's caller.
    #[must_use]
    pub fn invoke_continuing(
        &self,
        on_behalf_of: Option<InvocationId>,
        invocation: InvocationId,
        target: RemoteActionTarget,
        argument: InvokeArgument,
        confirmation: Option<Confirmation>,
    ) -> InvocationCall {
        let answer = Arriving::pending(self.source(Some(&target)));
        let request = InvokeRequest {
            on_behalf_of,
            invocation,
            target,
            argument,
            context: None,
            confirmation,
            revision: None,
            // A host that predates waits answers at admission, as before.
            wait_ms: Some(INVOKE_WAIT_MS),
            appearance: None,
        };
        self.lock().invocations.insert(
            request.invocation,
            Running {
                answer: answer.writer(),
                then: None,
                cancellation: None,
                observe_at: None,
                waiting: None,
            },
        );
        self.send(RemoteActionMessage::Invoke(request));
        answer
    }

    /// Follow the live result of the action at `target`
    /// ([`ExecutionMode::Live`](tilo_actions::ExecutionMode::Live)): the
    /// call's latest update is each new result as an `Item`, for as long as
    /// the returned handle lives; dropping it cancels the invocation. An
    /// action that answers once settles as any invocation does.
    #[must_use]
    pub fn follow(&self, target: RemoteActionTarget, argument: InvokeArgument) -> Followed {
        self.follow_with(target, argument, |_| {})
    }

    /// [`Self::follow`], telling `then` each update as it lands, before
    /// the call shows it.
    #[must_use]
    pub fn follow_with(
        &self,
        target: RemoteActionTarget,
        argument: InvokeArgument,
        then: impl FnMut(&Arrival<InvocationUpdate, ActionHostError>) + Send + 'static,
    ) -> Followed {
        let invocation = InvocationId::new();
        let answer = Arriving::pending(self.source(Some(&target)));
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation,
            target,
            argument,
            context: None,
            confirmation: None,
            revision: None,
            // It never settles: the receipt is the admission, and each
            // result arrives on the observe that follows it.
            wait_ms: None,
            appearance: None,
        };
        self.lock().invocations.insert(
            invocation,
            Running {
                answer: answer.writer(),
                then: Some(Box::new(then)),
                cancellation: None,
                observe_at: None,
                waiting: None,
            },
        );
        self.send(RemoteActionMessage::Invoke(request));
        Followed {
            remote: self.clone(),
            invocation,
            answer,
        }
    }

    /// Whether any request is still waiting for its answer.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        let inner = self.lock();
        !inner.catalogue.is_empty() || !inner.invocations.is_empty()
    }

    /// Ask the host to cancel an invocation this client is still observing.
    /// Repeated calls share one request and receipt. `Acknowledged` means only
    /// that the host accepted the cancellation request; the invocation's own
    /// call is the authority for whether it actually stopped.
    #[must_use]
    pub fn cancel(&self, invocation: InvocationId) -> Option<CancellationCall> {
        let source = self.source(None);
        let answer = {
            let mut inner = self.lock();
            let running = inner.invocations.get_mut(&invocation)?;
            if let Some(answer) = &running.cancellation {
                let refused = answer.with(|state| matches!(state, Arrival::Failed(_)));
                if !refused {
                    return Some(answer.clone());
                }
            }
            let answer = Arriving::pending(source);
            running.cancellation = Some(answer.clone());
            answer
        };
        self.send(RemoteActionMessage::Cancel { invocation });
        Some(answer)
    }

    fn send(&self, message: RemoteActionMessage) {
        let served = {
            let mut inner = self.lock();
            let served = match &inner.transport {
                Transport::Session => None,
                Transport::Host { host, caller } => Some((Arc::clone(host), caller.clone())),
            };
            if served.is_none() {
                inner.outbox.push(message);
                inner.wake.wake();
                return;
            }
            served
        };
        let Some((host, caller)) = served else {
            return;
        };
        let Some(answer) = serve(&host, &caller, message) else {
            return;
        };
        let inner = Arc::clone(&self.inner);
        answer.then(move |answer| {
            if let Some(answer) = answer {
                let follow_up = apply(&mut inner.lock_recover(), answer);
                if let Some(next) = follow_up {
                    Self { inner }.send(next);
                }
            }
        });
    }

    /// Ask after invocations a host that does not wait is still running,
    /// once each is due. The host connection does this on every pump; call
    /// it directly only for a client served [`with_host`](Self::with_host).
    pub fn poll(&self) {
        let now = crate::background::Background::current().now();
        let due: Vec<InvocationId> = {
            let mut inner = self.lock();
            inner.catalogue.retain(|_, walk| {
                if walk.due > now {
                    return true;
                }
                walk.answer.fail(ActionHostError::Unavailable {
                    reason: CATALOGUE_UNANSWERED.into(),
                });
                false
            });
            inner
                .invocations
                .iter_mut()
                .filter(|(_, running)| running.observe_at.is_some_and(|at| at <= now))
                .map(|(id, running)| {
                    running.observe_at = None;
                    *id
                })
                .collect()
        };
        for invocation in due {
            self.send(RemoteActionMessage::observe(invocation));
        }
    }

    /// Apply one answer from the session connection.
    fn receive(&self, message: &RemoteActionMessage) {
        let follow_up = apply(&mut self.lock(), message.clone());
        if let Some(next) = follow_up {
            self.send(next);
        }
    }
}

/// Record `answer`, returning the request that continues it (the next
/// catalogue page), if any.
fn apply(inner: &mut Inner, answer: RemoteActionMessage) -> Option<RemoteActionMessage> {
    match answer {
        RemoteActionMessage::CatalogueResponse {
            correlation,
            result,
        } => {
            let mut walk = inner.catalogue.remove(&correlation)?;
            let page = match result {
                Ok(page) => page,
                Err(error) => {
                    walk.answer.fail(error);
                    return None;
                }
            };
            walk.pages_remaining = walk.pages_remaining.saturating_sub(1);
            if !walk.follow_pages {
                walk.answer.ready(page);
                return None;
            }
            if page.budget_exhausted {
                walk.answer.fail(ActionHostError::Unavailable {
                    reason: "catalogue provider exhausted its work budget".into(),
                });
                return None;
            }
            if !page.stale_cursor && page.exhausted != page.next_cursor.is_empty() {
                walk.answer.fail(ActionHostError::Unavailable {
                    reason: "catalogue provider returned inconsistent completion metadata".into(),
                });
                return None;
            }
            if !page.stale_cursor
                && walk
                    .query
                    .revision
                    .as_ref()
                    .is_some_and(|revision| revision != &page.revision)
            {
                walk.answer.fail(ActionHostError::Unavailable {
                    reason: "catalogue provider changed revision without a stale response".into(),
                });
                return None;
            }
            if page.stale_cursor {
                // The offer changed mid-walk; start again from the top.
                walk.gathered = CataloguePage::default();
                walk.query.cursor.clear();
                walk.query.revision = None;
                walk.seen_cursors.clear();
            } else {
                walk.gathered.revision.clone_from(&page.revision);
                walk.gathered.rows.extend(page.rows);
                if walk.gathered.providers.is_empty() {
                    walk.gathered.providers = page.providers;
                }
                if page.exhausted {
                    walk.gathered.exhausted = true;
                    walk.answer.ready(walk.gathered);
                    return None;
                }
                walk.query.cursor = page.next_cursor;
                walk.query.revision = Some(page.revision);
            }
            if walk.pages_remaining == 0 {
                walk.answer.fail(ActionHostError::Unavailable {
                    reason: "catalogue traversal exceeded its page budget".into(),
                });
                return None;
            }
            let cursor = (walk.query.revision.clone(), walk.query.cursor.clone());
            if !walk.seen_cursors.insert(cursor) {
                walk.answer.fail(ActionHostError::Unavailable {
                    reason: "catalogue traversal repeated a cursor".into(),
                });
                return None;
            }
            let next = inner.next_correlation;
            inner.next_correlation = inner.next_correlation.wrapping_add(1);
            let query = walk.query.clone();
            let background = crate::background::Background::current();
            walk.due = background.now() + CATALOGUE_WAIT;
            background.wake_after(CATALOGUE_WAIT, inner.wake.clone());
            inner.catalogue.insert(next, walk);
            Some(RemoteActionMessage::CatalogueRequest {
                correlation: next,
                query,
            })
        }
        RemoteActionMessage::Receipt(receipt) => {
            let invocation = receipt.invocation;
            let update = match receipt.status {
                ReceiptStatus::Accepted => InvocationUpdate::Accepted { invocation },
                ReceiptStatus::Completed { output } => {
                    InvocationUpdate::Succeeded { invocation, output }
                }
                ReceiptStatus::Failed { error } => InvocationUpdate::Failed { invocation, error },
                ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
                    invocation,
                    message,
                },
                ReceiptStatus::Rejected { error } => {
                    let mut running = inner.invocations.remove(&invocation)?;
                    running.publish(Arrival::Failed(error));
                    return None;
                }
            };
            settle(inner, invocation, update)
        }
        RemoteActionMessage::Update { invocation, state } => {
            if let Some(state) = state {
                return settle(inner, invocation, state.update);
            }
            if let Some(mut running) = inner.invocations.remove(&invocation) {
                running.publish(Arrival::Failed(ActionHostError::UnknownInvocation));
            }
            None
        }
        RemoteActionMessage::CancelResponse { invocation, result } => {
            let running = inner.invocations.get_mut(&invocation)?;
            let answer = running.cancellation.as_ref()?;
            answer.writer().finish(result);
            None
        }
        RemoteActionMessage::CatalogueRequest { .. }
        | RemoteActionMessage::Invoke(_)
        | RemoteActionMessage::Observe { .. }
        | RemoteActionMessage::Cancel { .. }
        | RemoteActionMessage::RecordRequest { .. }
        | RemoteActionMessage::RecordResponse { .. }
        | RemoteActionMessage::FeedbackRequest { .. }
        | RemoteActionMessage::FeedbackResponse { .. } => None,
    }
}

/// Publish `update` for `invocation`; keep following it while it runs,
/// returning the waiting observe that does, if the host holds one.
fn settle(
    inner: &mut Inner,
    invocation: InvocationId,
    update: InvocationUpdate,
) -> Option<RemoteActionMessage> {
    let terminal = update.is_terminal();
    if terminal {
        if let Some(mut running) = inner.invocations.remove(&invocation) {
            running.publish(Arrival::Ready(update));
        }
        return None;
    }
    let running = inner.invocations.get_mut(&invocation)?;
    let mark = UpdateMark::of(&update);
    let background = crate::background::Background::current();
    let now = background.now();
    // A host that waits answers early only with something new; nothing new,
    // and long before the wait was up, is a host that does not wait.
    let unheld = running.waiting.take().is_some_and(|(sent, seen)| {
        seen == mark
            && now.saturating_duration_since(sent)
                < Duration::from_millis(u64::from(OBSERVE_WAIT_MS / 2))
    });
    running.publish(Arrival::Ready(update));
    if unheld {
        inner.host_waits = false;
    }
    if inner.host_waits {
        running.waiting = Some((now, mark));
        return Some(RemoteActionMessage::Observe {
            invocation,
            wait_ms: Some(OBSERVE_WAIT_MS),
            seen: Some(mark),
        });
    }
    running.observe_at = Some(now + OBSERVE_INTERVAL);
    background.wake_after(OBSERVE_INTERVAL, inner.wake.clone());
    None
}

impl Collaborative for RemoteActions {
    fn take_collab_requests(&mut self) -> Vec<CollabRequest> {
        self.poll();
        std::mem::take(&mut self.lock().outbox)
            .into_iter()
            .map(|message| CollabRequest::RemoteAction { message })
            .collect()
    }

    fn handle_collab_event(&mut self, event: &CollabEvent) {
        if let CollabEvent::RemoteAction { message } = event {
            self.receive(message);
        }
    }
}

#[cfg(test)]
#[path = "remote_actions_tests.rs"]
mod tests;
