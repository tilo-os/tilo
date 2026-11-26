//! An application's own client for the session's action API. [`RemoteActions`] lets an app read the
//! action catalogue and invoke published actions through the session broker — the same API the
//! palette, scripts and agents use.

use crate::time::{Duration, Instant};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use tilo_actions::InvocationUpdate;
use tilo_actions::outcome::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Confirmation,
    InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget, ResourceReadRequest, UpdateMark, WatchScope, serve,
};

use crate::arriving::{Arrival, ArrivalWriter, Arriving, Source};
use crate::collaboration::{CollabEvent, CollabRequest};
use crate::wake::WakeHandle;
use crate::widget::Collaborative;
use tilo_lock::MutexRecover as _;

type Served = (Arc<dyn ActionProvider + Send + Sync>, Caller);

thread_local! {
    static SERVED: std::cell::RefCell<Option<Served>> = const { std::cell::RefCell::new(None) };
    /// The client the application carries on its collaboration connection, while it is carried:
    /// every [`RemoteActions::session`] on this thread asks over it, so what any part of the window
    /// asks leaves on the one connection the app drains, and the answers come back to it.
    static CARRIED: std::cell::RefCell<Vec<RemoteActions>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Keeps the client it was made with the one [`RemoteActions::session`] answers on this thread,
/// until it drops.
pub struct CarriedGuard(std::marker::PhantomData<*const ()>);

impl Drop for CarriedGuard {
    fn drop(&mut self) {
        CARRIED.with(|carried| {
            carried.borrow_mut().pop();
        });
    }
}

/// Keeps a [`RemoteActions::enter`] provider current on this thread.
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

/// How long an invocation's receipt may be held for its outcome, so a quick action answers in the one
/// round trip that started it.
const INVOKE_WAIT_MS: u32 = 2_000;
/// How long one waiting observe may be held (the provider caps it too).
const OBSERVE_WAIT_MS: u32 = 10_000;
/// How long a running invocation is left before it is observed again, for a provider that answers a waiting
/// observe at once: one that predates waits.
const OBSERVE_INTERVAL: Duration = Duration::from_millis(200);
/// How long a followed read asks its provider to hold the answer for the next write. A provider a connection
/// reaches caps it at what it holds any waiting answer for; a desktop that serves its windows in
/// process holds it this long, so a record nobody writes costs its reader nothing in between.
const FOLLOW_WAIT_MS: u32 = 60_000;
/// How long a followed record is left before it is read again by a provider that answers a waiting read
/// at once, and after the first read its provider could not answer.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(2);
/// The longest a followed record is left between reads its provider cannot answer: each one in a row
/// doubles the wait up to this.
const FOLLOW_RETRY_MAX: Duration = Duration::from_secs(64);
/// How long one catalogue page may go unanswered. A connection that carries the request nowhere (no
/// provider in reach to answer it) must not leave the app waiting forever: past this, the read is refused
/// as unavailable.
pub const CATALOGUE_WAIT: Duration = Duration::from_secs(20);
/// Why a catalogue read was refused when nothing answered it in time.
pub const CATALOGUE_UNANSWERED: &str =
    "nothing in reach answered the request for the actions apps offer";

/// A catalogue read: every page of one query, gathered.
pub type CatalogueCall = Arriving<CataloguePage, ActionCallError>;
/// An invocation: its latest lifecycle update, or the provider's refusal (then nothing ran).
pub type InvocationCall = Arriving<InvocationUpdate, ActionCallError>;
/// A request to cancel a running invocation: `Ready(())` is the provider's acknowledgement. Acknowledgement
/// is not a terminal invocation outcome.
pub type CancellationCall = Arriving<(), ActionCallError>;
/// A record read: the readings as every reader is answered them (`{"revision", "records": [{"record",
/// "value" | "absent" | "arriving"}]}`).
pub type ResourceReadCall = Arriving<serde_json::Value, ActionCallError>;
/// A wait for a change of the workspace: `Ready(())` once what the watch follows may have changed,
/// or why the provider cannot tell when it does.
pub type ChangedCall = Arriving<(), ActionCallError>;

impl Arrival<InvocationUpdate, ActionCallError> {
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

/// A live result [`RemoteActions::follow`] follows.
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
            self.remote.forget(self.invocation);
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

/// A record [`RemoteActions::follow_resource`] keeps current: its readings as the provider answers
/// them now and after every write to it, for as long as the handle lives.
pub struct FollowedRead {
    remote: RemoteActions,
    follow: u64,
    answer: ResourceReadCall,
}

impl FollowedRead {
    /// The record's latest readings: read again whenever a write to it lands, and failed with the
    /// provider's reason while it has never been read or once this reader may no longer read it.
    #[must_use]
    pub const fn call(&self) -> &ResourceReadCall {
        &self.answer
    }
}

impl Drop for FollowedRead {
    fn drop(&mut self) {
        self.remote.lock().following.remove(&self.follow);
    }
}

impl std::fmt::Debug for FollowedRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FollowedRead")
            .field("follow", &self.follow)
            .finish_non_exhaustive()
    }
}

enum Transport {
    Session,
    /// Through an in-process provider, as `caller`.
    Provider {
        provider: Arc<dyn ActionProvider + Send + Sync>,
        caller: Caller,
    },
}

struct CatalogueWalk {
    answer: ArrivalWriter<CataloguePage, ActionCallError>,
    /// When the page asked for is refused for want of an answer.
    due: Instant,
}

struct Running {
    answer: ArrivalWriter<InvocationUpdate, ActionCallError>,
    /// Told each update as it lands, before the call shows it: how a follower reads each result once.
    then: Option<Box<dyn FnMut(&Arrival<InvocationUpdate, ActionCallError>) + Send>>,
    cancellation: Option<CancellationCall>,
    observe_at: Option<Instant>,
    waiting: Option<(Instant, UpdateMark)>,
}

impl Running {
    fn publish(&mut self, update: Arrival<InvocationUpdate, ActionCallError>) {
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

/// One record read in flight.
enum Reading {
    /// Asked once: its answer is the call's.
    Once(ArrivalWriter<serde_json::Value, ActionCallError>),
    /// The next reading of a record that is followed.
    Followed(u64),
}

/// A record [`RemoteActions::follow_resource`] keeps current.
struct Following {
    on_behalf_of: Option<InvocationId>,
    /// The record as it was asked for; each read after the first names the revision shown and
    /// waits for a write past it.
    read: ResourceReadRequest,
    answer: ArrivalWriter<serde_json::Value, ActionCallError>,
    /// The revision of the readings shown; `None` until the first arrived.
    revision: Option<String>,
    /// Whether the call says why the record, never read yet, could not be read.
    unread: bool,
    /// When the read in flight was sent to wait for a write past `revision`.
    waiting: Option<Instant>,
    /// When the record is read again: after a read its provider could not answer, after the first
    /// read while nothing answers it, and on the interval for a provider that holds no read.
    read_at: Option<Instant>,
    /// How long it is left after the next read its provider cannot answer.
    retry: Duration,
    /// The correlation of the read in flight. A read asked again replaces it: its answer, arriving
    /// late, is not this follow's, so one record is never read twice over for one follow.
    in_flight: Option<u64>,
}

struct Inner {
    transport: Transport,
    /// Made by [`RemoteActions::current`] with no provider entered: served by the provider entered on its
    /// thread when it first sends under one.
    unbound: bool,
    /// What the app asked before its transport was entered, each with the input serial of the
    /// press it followed, if any.
    outbox: Vec<(RemoteActionMessage, Option<u32>)>,
    next_correlation: u64,
    catalogue: HashMap<u64, CatalogueWalk>,
    invocations: HashMap<InvocationId, Running>,
    reads: HashMap<u64, Reading>,
    following: HashMap<u64, Following>,
    next_follow: u64,
    changes: HashMap<u64, ArrivalWriter<(), ActionCallError>>,
    /// Whether the provider holds a waiting observe until something changes; cleared the first time it
    /// answers one at once with nothing new.
    host_waits: bool,
    /// Whether the provider holds a waiting read until a write lands; cleared the first time it answers
    /// one at once with the revision it was told.
    host_holds_reads: bool,
    wake: WakeHandle,
}

/// An application's client for the session's action API.
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
    /// A client that talks through the application's collaboration connection: the one the
    /// application carries ([`Self::carrying`]) where it carries one, a client of its own otherwise.
    #[must_use]
    pub fn session() -> Self {
        CARRIED
            .with(|carried| carried.borrow().last().cloned())
            .unwrap_or_else(|| Self::new(Transport::Session))
    }

    /// Carry `client` on this thread's collaboration connection: every [`Self::session`] made here
    /// from now on asks over it, so a part of the window that makes its client later — History
    /// following what it lists — is heard and answered with the rest.
    ///
    /// The application's framework carries the client its root drains, once, for the app's life.
    #[must_use]
    pub fn carrying(client: Self) -> CarriedGuard {
        CARRIED.with(|carried| carried.borrow_mut().push(client));
        CarriedGuard(std::marker::PhantomData)
    }

    /// A client served by `provider` in this process, as `caller`.
    #[must_use]
    pub fn with_provider(provider: Arc<dyn ActionProvider + Send + Sync>, caller: Caller) -> Self {
        Self::new(Transport::Provider { provider, caller })
    }

    /// The client this thread's app should use: one served by the provider a [`RemoteActions::enter`] guard
    /// installed, else [`session`](Self::session).
    ///
    /// A window a desktop builds before it serves its windows (the one it opens with) makes its
    /// client with no provider entered. Such a client is the entered provider's from the first request it
    /// sends under one, with whatever it asked before: a window never talks past the desktop that
    /// serves it.
    #[must_use]
    pub fn current() -> Self {
        SERVED.with(|served| served.borrow().clone()).map_or_else(
            || {
                let client = Self::session();
                client.lock().unbound = true;
                client
            },
            |(provider, caller)| Self::with_provider(provider, caller),
        )
    }

    /// The client a window's root carries for the app's life: served by the provider entered on
    /// this thread, else one of its own. Never another window's client, which this thread's
    /// carried stack holds while a window is built inside another (a test host's app in the
    /// person's own workspace, a component another window shows): the requests of one window
    /// are its own to send and answer.
    #[must_use]
    pub fn window() -> Self {
        SERVED.with(|served| served.borrow().clone()).map_or_else(
            || Self::new(Transport::Session),
            |(provider, caller)| Self::with_provider(provider, caller),
        )
    }

    /// Serve every client [`current`](Self::current) makes on this thread from `provider`, as `caller`,
    /// until the guard drops — how a test or an embedding provider stands in for the session.
    #[must_use = "the provider serves clients only while the guard lives"]
    pub fn enter(
        provider: Arc<dyn ActionProvider + Send + Sync>,
        caller: Caller,
    ) -> RemoteActionsGuard {
        let previous = SERVED.with(|served| served.replace(Some((provider, caller))));
        RemoteActionsGuard {
            previous,
            thread_affinity: std::marker::PhantomData,
        }
    }

    fn new(transport: Transport) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                transport,
                unbound: false,
                outbox: Vec::new(),
                next_correlation: 1,
                catalogue: HashMap::new(),
                invocations: HashMap::new(),
                reads: HashMap::new(),
                following: HashMap::new(),
                next_follow: 1,
                changes: HashMap::new(),
                host_waits: true,
                host_holds_reads: true,
                wake: WakeHandle::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock_recover()
    }

    /// Read exactly one bounded page of `query`. This is for clients that traverse a resource hierarchy
    /// and therefore must own one global page/work budget across parents.
    #[must_use]
    pub fn catalogue_page(&self, query: CatalogueQuery) -> CatalogueCall {
        self.start_catalogue(query)
    }

    fn start_catalogue(&self, query: CatalogueQuery) -> CatalogueCall {
        let client = self.clone();
        let asked = query.clone();
        let answer = Arriving::pending(self.source()).on_retry(move |answer| {
            client.ask_catalogue(answer, asked.clone());
        });
        self.ask_catalogue(&answer, query);
        answer
    }

    fn ask_catalogue(&self, answer: &CatalogueCall, query: CatalogueQuery) {
        let writer = answer.writer();
        writer.pending(self.source());
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
            inner.catalogue.insert(
                correlation,
                CatalogueWalk {
                    answer: writer,
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

    /// Where this client's answer comes from: this device, whose broker answers every request — an
    /// app's own actions, the catalogue it is offered, and a workspace's governance (its people and its
    /// own name), which the workspace's person changes here and this device signs.
    const fn source(&self) -> Source {
        Source::Local
    }

    /// Invoke the action at `target` with `argument`, carrying the person's explicit `confirmation`
    /// when its contract asks for one.
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

    /// [`Self::invoke_with_id`] for a request made while running `on_behalf_of`, an invocation the provider
    /// delivered here: the provider decides it as that invocation's caller.
    #[must_use]
    pub fn invoke_continuing(
        &self,
        on_behalf_of: Option<InvocationId>,
        invocation: InvocationId,
        target: RemoteActionTarget,
        argument: InvokeArgument,
        confirmation: Option<Confirmation>,
    ) -> InvocationCall {
        let answer = Arriving::pending(self.source());
        let request = InvokeRequest {
            on_behalf_of,
            invocation,
            target,
            argument,
            context: None,
            confirmation,
            revision: None,
            wait_ms: Some(INVOKE_WAIT_MS),
            appearance: None,
            signed: None,
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
    /// ([`ExecutionMode::Live`](tilo_actions::ExecutionMode::Live)): the call's latest update is each
    /// new result as an `Item`, for as long as the returned handle lives; dropping it cancels the
    /// invocation.
    #[must_use]
    pub fn follow(&self, target: RemoteActionTarget, argument: InvokeArgument) -> Followed {
        self.follow_with(target, argument, |_| {})
    }

    /// [`Self::follow`], telling `then` each update as it lands, before the call shows it.
    #[must_use]
    pub fn follow_with(
        &self,
        target: RemoteActionTarget,
        argument: InvokeArgument,
        then: impl FnMut(&Arrival<InvocationUpdate, ActionCallError>) + Send + 'static,
    ) -> Followed {
        let invocation = InvocationId::new();
        let answer = Arriving::pending(self.source());
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
            signed: None,
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

    /// Read the record `read` names, in the workspace it names (the window's, for a record of
    /// its own workspace), through the record API, as a request of its own or
    /// continuing `on_behalf_of`, an invocation the provider delivered here: the provider decides it as that
    /// invocation's caller.
    #[must_use]
    pub fn read_resource(
        &self,
        on_behalf_of: Option<InvocationId>,
        read: ResourceReadRequest,
    ) -> ResourceReadCall {
        let answer = Arriving::pending(self.source());
        let correlation = {
            let mut inner = self.lock();
            let correlation = inner.next_correlation;
            inner.next_correlation = inner.next_correlation.wrapping_add(1);
            inner
                .reads
                .insert(correlation, Reading::Once(answer.writer()));
            correlation
        };
        self.send(RemoteActionMessage::ResourceRead {
            correlation,
            on_behalf_of,
            read,
        });
        answer
    }

    /// Keep the record `read` names current: `read` as the provider answers it now, and
    /// again after every write to it, for as long as the returned handle lives. Each read is the
    /// provider's to decide, as [`Self::read_resource`]'s is: a reader that may no longer read the
    /// record is told so and shown nothing more.
    ///
    /// Nothing runs between writes. Each read after the first names the revision shown and asks the
    /// provider to hold its answer until the record differs (`after`, `wait_ms`), so a record nobody
    /// writes costs its reader no frame and no timer; a provider that holds no read is asked again on
    /// an interval. `read`'s own `after` and `wait_ms` are not used.
    #[must_use]
    pub fn follow_resource(
        &self,
        on_behalf_of: Option<InvocationId>,
        mut read: ResourceReadRequest,
    ) -> FollowedRead {
        read.after = None;
        read.wait_ms = None;
        let answer = Arriving::pending(self.source());
        let now = crate::background::Background::current().now();
        let (follow, first) = {
            let mut inner = self.lock();
            let follow = inner.next_follow;
            inner.next_follow = inner.next_follow.wrapping_add(1);
            inner.following.insert(
                follow,
                Following {
                    on_behalf_of,
                    read,
                    answer: answer.writer(),
                    revision: None,
                    unread: false,
                    waiting: None,
                    read_at: None,
                    retry: FOLLOW_INTERVAL,
                    in_flight: None,
                },
            );
            (follow, next_read(&mut inner, follow, false, now))
        };
        if let Some(first) = first {
            self.send(first);
        }
        FollowedRead {
            remote: self.clone(),
            follow,
            answer,
        }
    }

    /// Ask the provider to answer when what a watch of the workspace follows under `scope` may have
    /// changed: a write landed in its records, or who is in it moved. A provider that cannot tell
    /// says so at once.
    #[must_use]
    pub fn changed(&self, scope: WatchScope) -> ChangedCall {
        let answer = Arriving::pending(self.source());
        let correlation = {
            let mut inner = self.lock();
            let correlation = inner.next_correlation;
            inner.next_correlation = inner.next_correlation.wrapping_add(1);
            inner.changes.insert(correlation, answer.writer());
            correlation
        };
        self.send(RemoteActionMessage::ChangedRequest { correlation, scope });
        answer
    }

    /// Whether any request is still waiting for its answer. A followed record waits for its first
    /// readings only: the read held for its next write is not something this client is busy with.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        let inner = self.lock();
        !inner.catalogue.is_empty()
            || !inner.invocations.is_empty()
            || !inner.changes.is_empty()
            || inner
                .reads
                .values()
                .any(|reading| matches!(reading, Reading::Once(_)))
            || inner
                .following
                .values()
                .any(|following| following.revision.is_none() && !following.unread)
    }

    /// Stop following `invocation`: its provider is asked to cancel it, and nothing it answers is
    /// read any more, so its acknowledgement wakes no window.
    fn forget(&self, invocation: InvocationId) {
        let followed = self.lock().invocations.remove(&invocation);
        if followed.is_some() {
            self.send(RemoteActionMessage::Cancel { invocation });
        }
    }

    /// Ask the provider to cancel an invocation this client is still observing.
    #[must_use]
    pub fn cancel(&self, invocation: InvocationId) -> Option<CancellationCall> {
        let source = self.source();
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
        let (served, asked_before) = {
            let mut inner = self.lock();
            if inner.unbound
                && let Some((provider, caller)) = SERVED.with(|served| served.borrow().clone())
            {
                inner.transport = Transport::Provider { provider, caller };
                inner.unbound = false;
            }
            let served = match &inner.transport {
                Transport::Session => None,
                Transport::Provider { provider, caller } => {
                    Some((Arc::clone(provider), caller.clone()))
                }
            };
            // Only an invocation can run as the person: what a call names is the press behind it.
            let press = matches!(message, RemoteActionMessage::Invoke(_))
                .then(crate::press::current)
                .flatten();
            if served.is_none() {
                inner.outbox.push((message, press));
                inner.wake.wake();
                return;
            }
            // What it asked before its provider was entered is that provider's to answer too.
            (served, std::mem::take(&mut inner.outbox))
        };
        let Some((provider, caller)) = served else {
            return;
        };
        for (message, _) in asked_before
            .into_iter()
            .chain(std::iter::once((message, None)))
        {
            let Some(answer) = serve(&provider, &caller, message) else {
                continue;
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
    }

    /// Ask after invocations a provider that does not wait is still running, once each is due.
    pub fn poll(&self) {
        let now = crate::background::Background::current().now();
        let (due, reads): (Vec<InvocationId>, Vec<RemoteActionMessage>) = {
            let mut inner = self.lock();
            inner.catalogue.retain(|_, walk| {
                if walk.due > now {
                    return true;
                }
                walk.answer.fail(ActionCallError::Unavailable {
                    reason: CATALOGUE_UNANSWERED.into(),
                });
                false
            });
            let due = inner
                .invocations
                .iter_mut()
                .filter(|(_, running)| running.observe_at.is_some_and(|at| at <= now))
                .map(|(id, running)| {
                    running.observe_at = None;
                    *id
                })
                .collect();
            let follows: Vec<u64> = inner
                .following
                .iter()
                .filter(|(_, following)| following.read_at.is_some_and(|at| at <= now))
                .map(|(follow, _)| *follow)
                .collect();
            let hold = inner.host_holds_reads;
            let reads = follows
                .into_iter()
                .filter_map(|follow| next_read(&mut inner, follow, hold, now))
                .collect();
            (due, reads)
        };
        for invocation in due {
            self.send(RemoteActionMessage::observe(invocation));
        }
        for read in reads {
            self.send(read);
        }
    }

    fn receive(&self, message: &RemoteActionMessage) {
        let follow_up = apply(&mut self.lock(), message.clone());
        if let Some(next) = follow_up {
            self.send(next);
        }
    }
}

fn apply(inner: &mut Inner, answer: RemoteActionMessage) -> Option<RemoteActionMessage> {
    match answer {
        RemoteActionMessage::CatalogueResponse {
            correlation,
            result,
        } => {
            let walk = inner.catalogue.remove(&correlation)?;
            match result {
                Ok(page) => walk.answer.ready(page),
                Err(error) => walk.answer.fail(error),
            }
            None
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
        RemoteActionMessage::Update {
            invocation, state, ..
        } => {
            if let Some(state) = state {
                return settle(inner, invocation, state.update);
            }
            if let Some(mut running) = inner.invocations.remove(&invocation) {
                running.publish(Arrival::Failed(ActionCallError::UnknownInvocation));
            }
            None
        }
        RemoteActionMessage::CancelResponse { invocation, result } => {
            let running = inner.invocations.get_mut(&invocation)?;
            let answer = running.cancellation.as_ref()?;
            answer.writer().finish(result);
            None
        }
        RemoteActionMessage::ResourceReading {
            correlation,
            result,
        } => {
            let reading = inner.reads.remove(&correlation)?;
            let result = result.and_then(|text| {
                serde_json::from_str(&text).map_err(|error| ActionCallError::Unavailable {
                    reason: format!("the record read answered no readings: {error}"),
                })
            });
            match reading {
                Reading::Once(answer) => {
                    answer.finish(result);
                    None
                }
                Reading::Followed(follow) => followed(inner, follow, result),
            }
        }
        RemoteActionMessage::ChangedResponse {
            correlation,
            result,
        } => {
            inner.changes.remove(&correlation)?.finish(result);
            None
        }
        RemoteActionMessage::CatalogueRequest { .. }
        | RemoteActionMessage::Invoke(_)
        | RemoteActionMessage::Observe { .. }
        | RemoteActionMessage::Cancel { .. }
        | RemoteActionMessage::ResourceRead { .. }
        | RemoteActionMessage::RetiredFeedbackRequest { .. }
        | RemoteActionMessage::RetiredFeedbackResponse { .. }
        | RemoteActionMessage::ConfirmationRequest { .. }
        | RemoteActionMessage::ConfirmationResponse { .. }
        | RemoteActionMessage::ContentPutRequest { .. }
        | RemoteActionMessage::ContentPutResponse { .. }
        | RemoteActionMessage::ContentGetRequest { .. }
        | RemoteActionMessage::ContentLinkRequest { .. }
        | RemoteActionMessage::ContentGetResponse { .. }
        | RemoteActionMessage::ContentLinkResponse { .. }
        | RemoteActionMessage::ChangesRequest { .. }
        | RemoteActionMessage::ChangesResponse { .. }
        | RemoteActionMessage::ChangedRequest { .. }
        | RemoteActionMessage::SurfaceRequest { .. }
        | RemoteActionMessage::SurfaceResponse { .. }
        | RemoteActionMessage::CatalogueChanged { .. }
        | RemoteActionMessage::CallCarryRequest { .. }
        | RemoteActionMessage::CallCarryResponse { .. }
        | RemoteActionMessage::CallReport { .. }
        | RemoteActionMessage::CallReportResponse { .. }
        | RemoteActionMessage::CallHeard(_)
        | RemoteActionMessage::CallSaid(_)
        | RemoteActionMessage::CallStatus(_)
        | RemoteActionMessage::AudioOutputRequest { .. }
        | RemoteActionMessage::AudioOutputResponse { .. }
        | RemoteActionMessage::StoreRequest { .. }
        | RemoteActionMessage::StoreResponse { .. }
        | RemoteActionMessage::CallMediaRequest { .. }
        | RemoteActionMessage::CallMediaResult { .. }
        | RemoteActionMessage::PresenceSaid { .. }
        | RemoteActionMessage::PresenceSaidResult { .. }
        | RemoteActionMessage::PresenceHeard { .. } => None,
    }
}

/// The next read of the followed record `follow`: held for a write past the revision shown when
/// `hold` and one is shown, else answered with the record as it is now.
fn next_read(
    inner: &mut Inner,
    follow: u64,
    hold: bool,
    now: Instant,
) -> Option<RemoteActionMessage> {
    let previous = inner.following.get(&follow)?.in_flight;
    if let Some(previous) = previous {
        inner.reads.remove(&previous);
    }
    let correlation = inner.next_correlation;
    inner.next_correlation = inner.next_correlation.wrapping_add(1);
    let following = inner.following.get_mut(&follow)?;
    let hold = hold && following.revision.is_some();
    // A first read nothing answers is asked again: the workspace it reads from may be served,
    // or told of, after the window asked. One that waits for a write is its provider's to answer.
    let again = !hold && following.revision.is_none();
    following.read_at = again.then(|| now + FOLLOW_INTERVAL);
    following.waiting = hold.then_some(now);
    following.in_flight = Some(correlation);
    let mut read = following.read.clone();
    read.after = following.revision.clone().filter(|_| hold);
    read.wait_ms = hold.then_some(FOLLOW_WAIT_MS);
    let on_behalf_of = following.on_behalf_of;
    if again {
        crate::background::Background::current().wake_after(FOLLOW_INTERVAL, inner.wake.clone());
    }
    inner.reads.insert(correlation, Reading::Followed(follow));
    Some(RemoteActionMessage::ResourceRead {
        correlation,
        on_behalf_of,
        read,
    })
}

/// Show what a read of the followed record `follow` answered, and read it again: held for the next
/// write where its provider holds reads, after a wait where it does not or could not answer. A refusal
/// ends the following: what was shown is no longer this reader's to see.
fn followed(
    inner: &mut Inner,
    follow: u64,
    result: Result<serde_json::Value, ActionCallError>,
) -> Option<RemoteActionMessage> {
    let background = crate::background::Background::current();
    let now = background.now();
    let again = |inner: &mut Inner, after: Duration| {
        if let Some(following) = inner.following.get_mut(&follow) {
            following.read_at = Some(now + after);
        }
        background.wake_after(after, inner.wake.clone());
        None
    };
    let following = inner.following.get_mut(&follow)?;
    let sent = following.waiting.take();
    match result {
        Ok(readings) => {
            let revision = readings
                .get("revision")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            following.retry = FOLLOW_INTERVAL;
            if revision.is_none() || revision != following.revision {
                following.revision.clone_from(&revision);
                following.unread = false;
                following.answer.ready(readings);
            } else if sent.is_some_and(|sent| {
                now.saturating_duration_since(sent)
                    < Duration::from_millis(u64::from(OBSERVE_WAIT_MS / 2))
            }) {
                // A provider that holds a read answers early only with something new.
                inner.host_holds_reads = false;
            }
            if inner.host_holds_reads && revision.is_some() {
                next_read(inner, follow, true, now)
            } else {
                again(inner, FOLLOW_INTERVAL)
            }
        }
        Err(
            error @ (ActionCallError::Unavailable { .. }
            | ActionCallError::NotReady
            | ActionCallError::Unanswered),
        ) => {
            // A record that was read keeps what it shows while its provider cannot answer; one never
            // read says why, once.
            if following.revision.is_none() && !following.unread {
                following.unread = true;
                following.answer.fail(error);
            }
            let wait = following.retry;
            following.retry = wait.saturating_mul(2).min(FOLLOW_RETRY_MAX);
            again(inner, wait)
        }
        Err(error) => {
            inner.following.remove(&follow)?.answer.fail(error);
            None
        }
    }
}

/// Publish `update` for `invocation`; keep following it while it runs, returning the waiting observe
/// that does, if the provider holds one.
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
    // A provider that waits answers early only with something new; nothing new,
    // and long before the wait was up, is a provider that does not wait.
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
            correlation: 0,
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
            .map(|(message, press)| CollabRequest::RemoteAction { message, press })
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
