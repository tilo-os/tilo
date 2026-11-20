//! An operation an app's worker takes long to answer goes on off the thread that serves its
//! workspace.
//!
//! One thread serves the requests of many workspaces, one after another. A request that waited
//! on that thread for an app's worker to answer (a large import, an app's long effect, a worker
//! that hangs) held every other workspace of the thread behind it, for as long as the worker
//! took. Its operation now goes on in a thread of its own once it has taken longer than
//! [`CALL_GRACE`] ([`crate::worker::ConfinedReplica::invoke_handed`]), and the request waits
//! beside the thread, as a write waiting for its part does ([`super::part_wait`]): applied again
//! once the worker answered, it takes that answer and goes on as it would have.
//!
//! Meanwhile the thread answers the other workspaces, and those requests of the same workspace
//! that read neither the document the operation changes nor anything a write changes: listings
//! of its other documents, searches, its changes and its history of invocations. Every other
//! request of the workspace waits for the operation, and once it answered goes back to the
//! thread's queue ahead of what came since ([`super::queue`]): a person's request still goes
//! before an agent's, and a later write still follows the earlier one.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
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

/// How long the thread waits for an app's worker to answer before the operation goes on off
/// it: most operations answer well within it and cost nothing more, and a request of another
/// workspace waits for one at most this long.
const CALL_GRACE: Duration = Duration::from_millis(20);

/// The code of the refusal a request is held with while its operation goes on off the thread: its
/// caller never hears it.
const RUNNING: &str = "app_call_running";

/// A request held off the thread, with how it waited for parts so far.
struct Held {
    queued: Instant,
    tried: Option<Tried>,
    job: Job,
}

/// The operation one workspace's request has going on off the thread, and what of the workspace waits
/// for it.
struct Running {
    /// The document it changes.
    holder: DocumentRoute,
    /// The request whose operation it is.
    request: Option<Held>,
    /// The workspace's requests that came since and wait for it, in the order they came.
    behind: VecDeque<Held>,
}

/// The operations of a thread's workspaces running off it.
#[derive(Default)]
pub(super) struct CallWaits {
    running: HashMap<WorkspaceId, Running>,
    /// The request being served handed its operation off this thread on `holder`, with how it
    /// waited for parts: it is held, not answered.
    held: Option<(DocumentRoute, Option<Tried>)>,
    /// What waited for an operation that answered, for the thread's queue to take again.
    returned: VecDeque<Held>,
}

impl CallWaits {
    /// Whether the request being served waits for its operation: its caller is not answered
    /// now, and what it did is not recorded as its outcome.
    pub(super) const fn held(&self) -> bool {
        self.held.is_some()
    }
}

/// Whether `tried` is an operation still running off the thread.
pub(super) fn running(tried: &Result<ActionOutput, ApiError>) -> bool {
    matches!(tried, Err(ApiError::Unavailable { code, .. }) if code == RUNNING)
}

/// What the request being served answers while its operation goes on off the thread.
fn still_running() -> ApiError {
    ApiError::Unavailable {
        code: RUNNING.to_owned(),
        reason: "the app's worker is still answering".to_owned(),
    }
}

/// Whether `job`, of a workspace whose operation on `holder` goes on off the thread, reads
/// nothing that operation changes: a listing of another document, a search, the workspace's
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

impl Worker {
    /// `operation` on `app`, the document at `holder` of `workspace_id`, for `invocation`: its
    /// answer, or [`still_running`] once it goes on off this thread, the request being served held
    /// until it answers. `confirmed` is whether its person confirmed it, kept for when it is
    /// applied again.
    pub(super) fn invoke_handed(
        &mut self,
        (workspace_id, holder): (WorkspaceId, &DocumentRoute),
        (app, invocation): (&Arc<ConfinedReplica>, tilo_actions::InvocationId),
        confirmed: bool,
        operation: impl FnOnce() -> Operation,
    ) -> Invoked {
        let shared = Arc::clone(&self.shared);
        let answered = move || answered(&shared, workspace_id);
        if let Some(answer) = app.invoke_handed(invocation, CALL_GRACE, answered, operation) {
            return answer;
        }
        let tried = self.part_waits.serving_confirmed(confirmed);
        self.call_waits.held = Some((holder.clone(), tried));
        Err(still_running())
    }

    /// Hold `job`, the request just served, until the operation it handed off this thread
    /// answers.
    pub(super) fn await_call(&mut self, workspace_id: WorkspaceId, queued: Instant, job: Job) {
        let Some((holder, tried)) = self.call_waits.held.take() else {
            return;
        };
        let request = Some(Held { queued, tried, job });
        match self.call_waits.running.get_mut(&workspace_id) {
            // Applied again, it handed off one more operation: it waits for that one, ahead of
            // what waited for the last.
            Some(running) => {
                running.holder = holder;
                running.request = request;
            }
            None => {
                self.call_waits.running.insert(
                    workspace_id,
                    Running {
                        holder,
                        request,
                        behind: VecDeque::new(),
                    },
                );
            }
        }
    }

    /// `job`, asked at `queued`, unless an operation of its workspace goes on off this thread and
    /// the job reads what it changes: then it waits for that operation, after those of the
    /// workspace that waited already.
    pub(super) fn unless_behind_call(&mut self, queued: Instant, job: Job) -> Option<Job> {
        let Some(running) = job
            .workspace()
            .and_then(|workspace_id| self.call_waits.running.get_mut(&workspace_id))
        else {
            return Some(job);
        };
        if beside(&job, &running.holder) {
            return Some(job);
        }
        running.behind.push_back(Held {
            queued,
            tried: self.part_waits.take_resumed(),
            job,
        });
        None
    }

    /// The operation `workspace_id`'s request handed off this thread answered: the request is
    /// applied again and takes its answer, and what waited for it goes back to the thread's
    /// queue ([`Self::returned_to_queue`]).
    pub(super) fn call_answered(&mut self, workspace_id: WorkspaceId) {
        let Some(Running {
            request, behind, ..
        }) = self.call_waits.running.remove(&workspace_id)
        else {
            return;
        };
        self.call_waits.returned.extend(behind);
        if let Some(Held { queued, tried, job }) = request {
            self.part_waits.resume_with(tried);
            self.timed(queued, job);
        }
    }

    /// What waited for an operation that answered since this was last asked, in the order it
    /// came, for the queue to take in its own order: served here at once, an agent's write that
    /// came before a person's listing went first and, its operation handed off again, the
    /// listing waited for it as well.
    pub(super) fn returned_to_queue(&mut self) -> impl DoubleEndedIterator<Item = Waiting> + use<> {
        std::mem::take(&mut self.call_waits.returned)
            .into_iter()
            .map(|Held { queued, tried, job }| Waiting {
                queued,
                job,
                from: Origin::Returned(tried),
            })
    }

    /// `job`, asked at `queued`, which waited off this thread for an operation of its workspace
    /// and goes on with how it waited for parts so far (`tried`).
    pub(super) fn serve_returned(&mut self, queued: Instant, job: Job, tried: Option<Tried>) {
        self.part_waits.resume_with(tried);
        self.timed(queued, job);
        self.wake_watchers();
    }
}

/// Tell the thread serving `workspace_id` that the operation one of its requests handed off
/// answered.
fn answered(shared: &Shared, workspace_id: WorkspaceId) {
    if let Some(jobs) = shared.jobs.lock_recover().as_ref() {
        let _ = jobs.send((Instant::now(), Job::Called(workspace_id)));
    }
}
