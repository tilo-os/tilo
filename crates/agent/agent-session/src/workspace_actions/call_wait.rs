//! A call into an app's worker that takes long to answer goes on off the thread that serves its
//! workspace.
//!
//! One thread serves the requests of many workspaces, one after another. A request that waited
//! on that thread for an app's worker to answer (a large import, an app's long effect, a worker
//! that hangs, a big document's view or listing) held every other workspace of the thread behind
//! it, for as long as the worker took. The call now goes on in a thread of its own once it has
//! taken longer than [`CALL_GRACE`], and the request waits beside the thread, as a write waiting
//! for its part does ([`super::part_wait`]): applied again once the worker answered, it takes
//! that answer and goes on as it would have.
//!
//! That holds for every call the thread makes of a worker: an invocation
//! ([`ConfinedReplica::invoke_handed`]), a view or a listing of a document
//! ([`Worker::hand`]). A request held that way is one call into a worker that goes on, and it
//! is applied again to take the answer of the call it handed off, never to make the call again.
//!
//! Meanwhile the thread answers the other workspaces, and those requests of the same workspace
//! that read neither the document the call changes nor anything a write changes: listings
//! of its other documents, searches, its changes and its history of invocations. Every other
//! request of the workspace waits for the call, and once it answered goes back to the
//! thread's queue ahead of what came since ([`super::queue`]): a person's request still goes
//! before an agent's, and a later write still follows the earlier one.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use tilo_actions::ActionOutput;
use tilo_actions::api::ApiError;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;

use super::part_wait::Tried;
use super::queue::{Origin, Waiting};
use super::{Job, Shared, Worker, resource_of};
use crate::worker::{ConfinedReplica, Invoked, Operation};

/// How long the thread waits for an app's worker to answer before the call goes on off it: most
/// calls answer well within it and cost nothing more, and a request of another workspace waits
/// for one at most this long.
const CALL_GRACE: Duration = Duration::from_millis(20);

/// The code of the refusal a request is held with while its call goes on off the thread: its
/// caller never hears it.
const RUNNING: &str = "app_call_running";

/// What a call made off the thread answers, as the request that asked for it takes it when it is
/// applied again.
type Answer = Box<dyn std::any::Any + Send>;

/// A request held off the thread, with how it waited for parts so far.
struct Held {
    queued: Instant,
    tried: Option<Tried>,
    job: Job,
}

/// The request being served handed a call off this thread.
struct HandedCall {
    /// The document it reads or changes.
    holder: DocumentRoute,
    /// How the request waited for parts so far.
    tried: Option<Tried>,
    /// The step of the request that asked the call, and where its answer comes once it has:
    /// `None` for an invocation, which the worker answers itself ([`ConfinedReplica::invoke_handed`]).
    answer: Option<(u32, mpsc::Receiver<Answer>)>,
}

/// One call into an app's worker going on off the thread.
struct Call {
    /// The document it reads or changes.
    holder: DocumentRoute,
    /// The step of the request that asked it ([`Worker::hand`]).
    step: u32,
    /// The request whose call it is, applied again once it answered: `None` for work its call
    /// answers by itself, such as the feedback projection, which completes its caller from
    /// there and is let go when it has.
    request: Option<Held>,
    /// Where its answer comes, for a call [`Worker::hand`] asked.
    answer: Option<mpsc::Receiver<Answer>>,
}

/// A workspace's calls going on off the thread, and what waits for them.
#[derive(Default)]
struct Running {
    calls: Vec<Call>,
    /// The workspace's requests that came since and wait for them, in the order they came.
    behind: VecDeque<Held>,
}

/// The calls of a thread's workspaces running off it.
#[derive(Default)]
pub(super) struct CallWaits {
    running: HashMap<WorkspaceId, Running>,
    /// The request being served handed its call off this thread: it is held, not answered.
    held: Option<HandedCall>,
    /// What waited for a call that answered, for the thread's queue to take again.
    returned: VecDeque<Held>,
    /// How many calls of the request being served asked the worker so far.
    step: u32,
    /// The request being served handed a call off this thread already: what more of it does
    /// not ask the worker again.
    dispatched: bool,
    /// The answer of the call the request about to be served handed off, and the step that
    /// asked it, taken when it is applied again.
    pending: Option<(u32, Answer)>,
    /// The request being served takes the answer of the call it handed off: it waits for
    /// nothing else of its workspace.
    taking_an_answer: bool,
}

impl CallWaits {
    /// Whether the request being served waits off the thread: its caller is not answered now,
    /// and what it did is not recorded as its outcome.
    pub(super) const fn held(&self) -> bool {
        self.held.is_some()
    }

    /// Start serving the request asked now: it asks its calls from the first step, and holds
    /// nothing yet.
    pub(super) fn begin(&mut self) {
        self.step = 0;
        self.dispatched = false;
        self.held = None;
    }

    /// Whether the request being served takes the answer of the call it handed off, and so
    /// waits for nothing else of its workspace.
    pub(super) const fn taking_an_answer(&self) -> bool {
        self.taking_an_answer
    }
}

/// Whether `tried` is a call still running off the thread.
pub(super) fn running(tried: &Result<ActionOutput, ApiError>) -> bool {
    matches!(tried, Err(ApiError::Unavailable { code, .. }) if code == RUNNING)
}

/// What the request being served answers while its call goes on off the thread.
pub(super) fn still_running() -> ApiError {
    ApiError::Unavailable {
        code: RUNNING.to_owned(),
        reason: "the app's worker is still answering".to_owned(),
    }
}

/// Whether `job`, of a workspace whose call on `holder` goes on off the thread, reads
/// nothing that call changes: a listing of another document, a search, the workspace's
/// changes and invocations, content it keeps.
fn beside(job: &Job, holder: &DocumentRoute) -> bool {
    match job {
        Job::Catalogue { query, .. } => query
            .provider
            .as_deref()
            .and_then(|provider| resource_of(provider, &holder.cluster_id))
            .is_some_and(|route| {
                route.app_id != holder.app_id || route.document != holder.document
            }),
        Job::Search { .. }
        | Job::Changes { .. }
        | Job::Changed { .. }
        | Job::Invocations { .. }
        | Job::ContentGet { .. } => true,
        _ => false,
    }
}

/// What a call into an app's worker needs of the thread serving its workspace: the state a
/// helper that is no [`Worker`] itself asks its calls through ([`super::canonical`]).
pub(super) struct Handing<'a> {
    calls: &'a mut CallWaits,
    parts: &'a mut super::part_wait::PartWaits,
    shared: &'a Arc<Shared>,
}

impl<'a> Handing<'a> {
    /// A call into an app's worker asked from what `calls`, `parts` and `shared` are held apart
    /// of: the worker's own fields, borrowed beside whatever else the caller reads.
    pub(super) const fn of(
        calls: &'a mut CallWaits,
        parts: &'a mut super::part_wait::PartWaits,
        shared: &'a Arc<Shared>,
    ) -> Self {
        Self {
            calls,
            parts,
            shared,
        }
    }

    /// Ask an app's worker as the request being served, for the document at `holder`: its
    /// answer now, or `None` once the call goes on off this thread ([`CALL_GRACE`]), the
    /// request held until it answers. Applied again, the request takes the answer of the call
    /// it handed off instead of asking again.
    pub(super) fn hand<T: Send + 'static>(
        &mut self,
        workspace_id: WorkspaceId,
        holder: &DocumentRoute,
        ask: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        let step = self.calls.step;
        self.calls.step = step.wrapping_add(1);
        let mut inline = false;
        if let Some((asked, answer)) = self.calls.pending.take() {
            // The answer belongs to the step that asked it: a step whose work changed while
            // its answer was out asks again.
            if asked == step {
                if let Ok(answer) = answer.downcast::<T>() {
                    return Some(*answer);
                }
            } else {
                // A later step's call answered and this one was asked before it: it asks here,
                // as it did the first time, so that a call that outlasts the grace every time
                // is not asked again and again while what came after it is never reached.
                inline = true;
                self.calls.pending = Some((asked, answer));
            }
        }
        if inline {
            return Some(ask());
        }
        if self.calls.dispatched {
            return None;
        }
        let shared = Arc::clone(self.shared);
        let (send, answer) = mpsc::channel();
        // Kept where a thread that did not start leaves it: the request then asks here, as the
        // thread always did.
        let ask = Arc::new(std::sync::Mutex::new(Some(ask)));
        let asking = Arc::clone(&ask);
        // In the worker this thread leases in: a call that opens a worker of its own would start
        // one, which costs more than the call it makes.
        let affinity = crate::worker::affinity();
        let spawned = std::thread::Builder::new()
            .name("app-call".into())
            .spawn(move || {
                if let Some(index) = affinity {
                    crate::worker::set_affinity(index);
                }
                let Some(ask) = asking.lock_recover().take() else {
                    return;
                };
                let output = ask();
                if send.send(Box::new(output) as Answer).is_ok() {
                    answered(&shared, workspace_id);
                }
            });
        if spawned.is_err() {
            let ask = ask.lock_recover().take()?;
            return Some(ask());
        }
        match answer.recv_timeout(CALL_GRACE) {
            Ok(answer) => {
                // It answers what it was asked, by construction: told apart, it is asked again.
                let Ok(answer) = answer.downcast::<T>() else {
                    tracing::error!("app worker: a call answered another kind than it was asked");
                    return None;
                };
                Some(*answer)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.calls.dispatched = true;
                self.calls.held = Some(HandedCall {
                    holder: holder.clone(),
                    tried: self.parts.serving_confirmed(false),
                    answer: Some((step, answer)),
                });
                None
            }
            // The thread ended without an answer: the request asks again when it is next
            // served.
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        }
    }
}

impl Worker {
    /// [`Handing::hand`] for the request being served.
    pub(super) fn hand<T: Send + 'static>(
        &mut self,
        workspace_id: WorkspaceId,
        holder: &DocumentRoute,
        ask: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        self.handing().hand(workspace_id, holder, ask)
    }

    /// This thread's calls into app workers, asked from beside what else the caller reads of
    /// the worker ([`Handing`]).
    pub(super) fn handing(&mut self) -> Handing<'_> {
        Handing::of(&mut self.call_waits, &mut self.part_waits, &self.shared)
    }

    /// `operation` on `app`, the document at `holder` of `workspace_id`, for `invocation`: its
    /// answer, or [`still_running`] once it goes on off this thread, the request being served held
    /// until it answers. `confirmed` is whether its person confirmed it, kept for when it is
    /// applied again. `descriptor` is the action as the resource published it before it ran, kept
    /// for the request applied again.
    pub(super) fn invoke_handed(
        &mut self,
        (workspace_id, holder): (WorkspaceId, &DocumentRoute),
        (app, invocation): (&Arc<ConfinedReplica>, tilo_actions::InvocationId),
        confirmed: bool,
        descriptor: Option<tilo_actions::ActionDescriptor>,
        operation: impl FnOnce() -> Operation,
    ) -> Invoked {
        let shared = Arc::clone(&self.shared);
        let answered = move || answered(&shared, workspace_id);
        if let Some(answer) =
            app.invoke_handed(invocation, CALL_GRACE, answered, descriptor, operation)
        {
            return answer;
        }
        let tried = self.part_waits.serving_confirmed(confirmed);
        self.call_waits.held = Some(HandedCall {
            holder: holder.clone(),
            tried,
            answer: None,
        });
        Err(still_running())
    }

    /// Hold `job`, the request just served, until the call it handed off this thread answers.
    pub(super) fn await_call(&mut self, workspace_id: WorkspaceId, queued: Instant, job: Job) {
        let Some(HandedCall {
            holder,
            tried,
            answer,
        }) = self.call_waits.held.take()
        else {
            return;
        };
        let step = answer.as_ref().map_or(0, |(step, _)| *step);
        let call = Call {
            holder,
            step,
            request: Some(Held { queued, tried, job }),
            answer: answer.map(|(_, answer)| answer),
        };
        self.call_waits
            .running
            .entry(workspace_id)
            .or_default()
            .calls
            .push(call);
    }

    /// Hold the call the request being served handed off this thread, whose work answers its
    /// caller by itself ([`Self::hand`]): nothing of it is applied again once it answered, and
    /// what waits for what it reads is served when it has.
    pub(super) fn await_completed(&mut self, workspace_id: WorkspaceId) {
        let Some(HandedCall {
            holder, answer, ..
        }) = self.call_waits.held.take()
        else {
            return;
        };
        let step = answer.as_ref().map_or(0, |(step, _)| *step);
        let call = Call {
            holder,
            step,
            request: None,
            answer: answer.map(|(_, answer)| answer),
        };
        self.call_waits
            .running
            .entry(workspace_id)
            .or_default()
            .calls
            .push(call);
    }

    /// `job`, asked at `queued`, unless a call of its workspace goes on off this thread and the
    /// job reads what it changes: then it waits for that call, after those of the workspace
    /// that waited already.
    pub(super) fn unless_behind_call(&mut self, queued: Instant, job: Job) -> Option<Job> {
        // It waited off this thread for a call it asked of its own: it goes on to take that
        // call's answer, whatever goes on beside it.
        if self.call_waits.taking_an_answer() {
            return Some(job);
        }
        let Some(workspace_id) = job.workspace() else {
            return Some(job);
        };
        let waits = self
            .call_waits
            .running
            .get(&workspace_id)
            .is_some_and(|running| !running.calls.iter().all(|call| beside(&job, &call.holder)));
        if !waits {
            return Some(job);
        }
        let tried = self.part_waits.take_resumed();
        if let Some(running) = self.call_waits.running.get_mut(&workspace_id) {
            running.behind.push_back(Held { queued, tried, job });
        }
        None
    }

    /// A call of `workspace_id`'s requests handed off this thread answered: the request whose
    /// call it is applied again and takes its answer, and what waited for it goes back to the
    /// thread's queue ([`Self::returned_to_queue`]).
    pub(super) fn call_answered(&mut self, workspace_id: WorkspaceId) {
        let Some(mut running) = self.call_waits.running.remove(&workspace_id) else {
            return;
        };
        let mut answered: Vec<(Call, Option<Answer>)> = Vec::new();
        let mut going: Vec<Call> = Vec::new();
        for call in std::mem::take(&mut running.calls) {
            match call.answer.as_ref().map(mpsc::Receiver::try_recv) {
                // An invocation takes its answer from the app's worker, and a thread that
                // ended without one leaves the request to ask again when it is applied again.
                None | Some(Err(mpsc::TryRecvError::Disconnected)) => answered.push((call, None)),
                Some(Ok(answer)) => answered.push((call, Some(answer))),
                Some(Err(mpsc::TryRecvError::Empty)) => going.push(call),
            }
        }
        // What waited for the calls that answered goes back to the queue, unless it reads what
        // a call still going changes.
        let released: Vec<Held> = std::mem::take(&mut running.behind)
            .into_iter()
            .filter(|held| going.iter().all(|call| beside(&held.job, &call.holder)))
            .collect();
        self.call_waits.returned.extend(released);
        // Back in hand before a request is applied again: one applied again that hands off a
        // call of its own is held for it beside the calls still going.
        if !going.is_empty() {
            running.calls = going;
            self.call_waits.running.insert(workspace_id, running);
        }
        for (call, answer) in answered {
            let Some(Held { queued, tried, job }) = call.request else {
                continue;
            };
            self.part_waits.resume_with(tried);
            // The request whose call ran reads nothing of what changed since it handed it off:
            // the call answered already.
            self.part_waits.takes_answer(true);
            self.call_waits.pending = answer.map(|answer| (call.step, answer));
            self.call_waits.taking_an_answer = true;
            self.timed(queued, job);
            self.call_waits.taking_an_answer = false;
            self.call_waits.pending = None;
            self.part_waits.takes_answer(false);
        }
    }

    /// What waited for a call that answered since this was last asked, in the order it came,
    /// for the queue to take in its own order: served here at once, an agent's write that came
    /// before a person's listing went first and, its call handed off again, the listing waited
    /// for it as well.
    pub(super) fn returned_to_queue(&mut self) -> impl DoubleEndedIterator<Item = Waiting> + use<> {
        std::mem::take(&mut self.call_waits.returned)
            .into_iter()
            .map(|Held { queued, tried, job }| Waiting {
                queued,
                job,
                from: Origin::Returned(tried),
            })
    }

    /// `job`, asked at `queued`, which waited off this thread for a call of its workspace and
    /// goes on with how it waited for parts so far (`tried`).
    pub(super) fn serve_returned(&mut self, queued: Instant, job: Job, tried: Option<Tried>) {
        self.part_waits.resume_with(tried);
        self.timed(queued, job);
        self.wake_watchers();
    }
}

/// Tell the thread serving `workspace_id` that a call one of its requests handed off answered.
fn answered(shared: &Shared, workspace_id: WorkspaceId) {
    if let Some(jobs) = shared.jobs.lock_recover().as_ref() {
        let _ = jobs.send((Instant::now(), Job::Called(workspace_id)));
    }
}
