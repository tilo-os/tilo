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
//! Answers arrive as [`LocalState`] values, so a bound view updates and an
//! idle window wakes when one lands. A root that owns a client returns it
//! from [`Widget::collaborative_mut`](crate::widget::Widget::collaborative_mut)
//! (or forwards to it), which is how its requests reach the connection and
//! the answers come back.
//!
//! [`RemoteActions::with_host`] serves the same client from an in-process
//! [`ActionHost`] instead, for tests and embedded hosts.

use crate::time::{Duration, Instant};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tilo_actions::InvocationUpdate;
use tilo_actions::outcome::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, Confirmation,
    InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget, serve,
};

use crate::collaboration::{CollabEvent, CollabRequest};
use crate::local_state::{LocalState, LocalStateWriter};
use crate::wake::WakeHandle;
use crate::widget::Collaborative;

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

/// How long a running invocation is left before it is observed again.
const OBSERVE_INTERVAL: Duration = Duration::from_millis(200);

/// Where one request stands.
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteCall<T> {
    /// Sent; no answer yet.
    Pending,
    /// The host answered.
    Done(T),
    /// The host refused: nothing ran.
    Refused(ActionHostError),
}

impl<T> RemoteCall<T> {
    /// Whether no answer has arrived yet.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

impl RemoteCall<InvocationUpdate> {
    /// Whether the invocation can no longer change: refused, or finished.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        match self {
            Self::Pending => false,
            Self::Done(update) => update.is_terminal(),
            Self::Refused(_) => true,
        }
    }
}

/// A catalogue read: every page of one query, gathered.
pub type CatalogueCall = RemoteCall<CataloguePage>;
/// An invocation: its latest lifecycle update.
pub type InvocationCall = RemoteCall<InvocationUpdate>;

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
    answer: LocalStateWriter<CatalogueCall>,
}

struct Running {
    answer: LocalStateWriter<InvocationCall>,
    /// When to observe it next, while the host is still running it.
    observe_at: Option<Instant>,
}

struct Inner {
    transport: Transport,
    outbox: Vec<RemoteActionMessage>,
    next_correlation: u64,
    catalogue: HashMap<u64, CatalogueWalk>,
    invocations: HashMap<InvocationId, Running>,
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
                wake: WakeHandle::new(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    /// Read every page of `query` from the catalogue the app is offered.
    #[must_use]
    pub fn catalogue(&self, query: CatalogueQuery) -> LocalState<CatalogueCall> {
        let answer = LocalState::new(RemoteCall::Pending);
        let message = {
            let mut inner = self.lock();
            let correlation = inner.next_correlation;
            inner.next_correlation = inner.next_correlation.wrapping_add(1);
            let mut first = query.clone();
            first.max_results = first.max_results.clamp(1, MAX_CATALOGUE_PAGE);
            inner.catalogue.insert(
                correlation,
                CatalogueWalk {
                    query: first.clone(),
                    gathered: CataloguePage::default(),
                    answer: answer.writer(),
                },
            );
            RemoteActionMessage::CatalogueRequest {
                correlation,
                query: first,
            }
        };
        self.send(message);
        answer
    }

    /// Invoke the action at `target` with `argument`, carrying the person's
    /// explicit `confirmation` when its contract asks for one.
    #[must_use]
    pub fn invoke(
        &self,
        target: RemoteActionTarget,
        argument: InvokeArgument,
        confirmation: Option<Confirmation>,
    ) -> LocalState<InvocationCall> {
        let answer = LocalState::new(RemoteCall::Pending);
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target,
            argument,
            context: None,
            confirmation,
            revision: None,
        };
        self.lock().invocations.insert(
            request.invocation,
            Running {
                answer: answer.writer(),
                observe_at: None,
            },
        );
        self.send(RemoteActionMessage::Invoke(request));
        answer
    }

    /// Whether any request is still waiting for its answer.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        let inner = self.lock();
        !inner.catalogue.is_empty() || !inner.invocations.is_empty()
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
        let Some(answer) = serve(host.as_ref(), &caller, message) else {
            return;
        };
        let inner = Arc::clone(&self.inner);
        answer.then(move |answer| {
            if let Some(answer) = answer {
                let follow_up = apply(&mut lock(&inner), answer);
                if let Some(next) = follow_up {
                    Self { inner }.send(next);
                }
            }
        });
    }

    /// Ask after invocations the host is still running, once each is due.
    /// The host connection does this on every pump; call it directly only
    /// for a client served [`with_host`](Self::with_host).
    pub fn poll(&self) {
        let now = crate::background::Background::current().now();
        let due: Vec<InvocationId> = {
            let mut inner = self.lock();
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
            self.send(RemoteActionMessage::Observe { invocation });
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

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
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
                    walk.answer.set(RemoteCall::Refused(error));
                    return None;
                }
            };
            if page.stale_cursor {
                // The offer changed mid-walk; start again from the top.
                walk.gathered = CataloguePage::default();
                walk.query.cursor.clear();
                walk.query.revision = None;
            } else {
                walk.gathered.revision.clone_from(&page.revision);
                walk.gathered.rows.extend(page.rows);
                if walk.gathered.providers.is_empty() {
                    walk.gathered.providers = page.providers;
                }
                if page.exhausted || page.next_cursor.is_empty() {
                    walk.answer.set(RemoteCall::Done(walk.gathered));
                    return None;
                }
                walk.query.cursor = page.next_cursor;
                walk.query.revision = Some(page.revision);
            }
            let next = inner.next_correlation;
            inner.next_correlation = inner.next_correlation.wrapping_add(1);
            let query = walk.query.clone();
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
                    let running = inner.invocations.remove(&invocation)?;
                    running.answer.set(RemoteCall::Refused(error));
                    return None;
                }
            };
            settle(inner, invocation, update);
            None
        }
        RemoteActionMessage::Update { invocation, state } => {
            match state {
                Some(state) => settle(inner, invocation, state.update),
                None => {
                    if let Some(running) = inner.invocations.remove(&invocation) {
                        running
                            .answer
                            .set(RemoteCall::Refused(ActionHostError::UnknownInvocation));
                    }
                }
            }
            None
        }
        RemoteActionMessage::CatalogueRequest { .. }
        | RemoteActionMessage::Invoke(_)
        | RemoteActionMessage::Observe { .. }
        | RemoteActionMessage::Cancel { .. }
        | RemoteActionMessage::CancelResponse { .. }
        | RemoteActionMessage::RecordRequest { .. }
        | RemoteActionMessage::RecordResponse { .. } => None,
    }
}

/// Publish `update` for `invocation`; keep following it while it runs.
fn settle(inner: &mut Inner, invocation: InvocationId, update: InvocationUpdate) {
    let terminal = update.is_terminal();
    if terminal {
        if let Some(running) = inner.invocations.remove(&invocation) {
            running.answer.set(RemoteCall::Done(update));
        }
        return;
    }
    let Some(running) = inner.invocations.get_mut(&invocation) else {
        return;
    };
    running.answer.set(RemoteCall::Done(update));
    let background = crate::background::Background::current();
    running.observe_at = Some(background.now() + OBSERVE_INTERVAL);
    background.wake_after(OBSERVE_INTERVAL, inner.wake.clone());
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
