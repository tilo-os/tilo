//! The invocations that hold an engine job: their handlers, their idle deadline, and their
//! cancellation.
//!
//! A job is the device's record (`devices/<device>/jobs/<job>`), kept by the daemon that runs the
//! engine. The call that started it stays open until the job ends, and the daemon ends it here:
//! the job's record names the invocation, its app and workspace and the handlers the app asked
//! for, and this runs them through the one dispatch, as child calls of that invocation.
//!
//! A `done` handler runs once when the job ends; a `progress` handler runs on each move, coalesced
//! so at most one of them is in flight, with a final one before `done`. Each is
//! `{"resource", "action", "arguments"}`: the address of an action the app publishes in its
//! workspace, with the arguments it is called with. The handler's outcome ends the invocation; a
//! job that names no `done` handler ends it with the job's own outcome, and a job that failed or
//! was cancelled ends it as failed or cancelled.
//!
//! While a live job is held the invocation is never expired as idle ([`AppBrokerState::
//! hold_jobs`]), and cancelling it cancels the jobs it started ([`AppBrokerState::
//! serve_job_canceller`]): each reads cancelled with its `done` handler run once, and the
//! invocation answers cancelled.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError, InvocationReceipt, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionFailure, ActionOutput, InvocationId, InvocationUpdate};

use super::AppBrokerState;

/// The argument of a handler the app left for the job: the job's record goes there.
const JOB_SLOT: &str = "$job";

/// How long a handler's own answer is waited for before its invocation hears it did not come.
const HANDLER_DEADLINE: Duration = Duration::from_secs(300);

/// What the daemon does for a job's invocation: the handlers the app asked for, its hold on the
/// idle deadline, and its cancellation.
pub trait JobEffects: Send + Sync {
    /// The job `record` moved and has not ended: run its `progress` handler, coalesced.
    fn progress(&self, record: &Value);

    /// The job `record` ended: run its final `progress` handler and then its `done` handler once,
    /// and end its invocation with that handler's outcome (or the job's own).
    fn ended(&self, record: &Value);

    /// The invocation `invocation` now runs a live job: never expire it as idle while it does.
    fn hold(&self, invocation: &str);

    /// The last live job of `invocation` ended.
    fn release(&self, invocation: &str);
}

impl AppBrokerState {
    /// The invocation `invocation` now runs a live job: it is never expired as idle while it does.
    pub fn hold_jobs(&self, invocation: &str) {
        if let Ok(mut held) = self.job_invocations.lock() {
            held.insert(invocation.to_owned());
        }
    }

    /// The last live job of `invocation` ended.
    pub fn release_jobs(&self, invocation: &str) {
        if let Ok(mut held) = self.job_invocations.lock() {
            held.remove(invocation);
        }
    }

    /// Whether `invocation` holds a live job: it is never expired as idle while it does.
    pub(super) fn held_by_job(&self, invocation: &InvocationId) -> bool {
        self.job_invocations
            .lock()
            .is_ok_and(|held| held.contains(&invocation.to_string()))
    }

    /// What cancels the jobs of a held invocation, once the daemon runs them.
    pub fn serve_job_canceller(&self, cancels: Arc<dyn Fn(&InvocationId) + Send + Sync>) {
        if let Ok(mut canceller) = self.job_canceller.lock() {
            *canceller = Some(cancels);
        }
    }

    /// Cancel the jobs of the held invocation `invocation`, when a daemon runs them: whether it
    /// was held by a job.
    pub(super) fn cancel_held_jobs(&self, invocation: &InvocationId) -> bool {
        if !self.held_by_job(invocation) {
            return false;
        }
        let cancels = self.job_canceller.lock().ok().and_then(|held| held.clone());
        if let Some(cancels) = cancels {
            cancels(invocation);
        }
        true
    }

    /// The peer the invocation `invocation` runs on, when it runs here.
    fn running_peer(&self, invocation: &InvocationId) -> Option<u64> {
        let mut matching = self
            .pending_invocations
            .lock()
            .ok()?
            .values()
            .filter(|pending| pending.update.invocation() == *invocation)
            .map(|pending| pending.peer_id)
            .take(2)
            .collect::<Vec<_>>();
        (matching.len() == 1).then(|| matching.pop()).flatten()
    }

    /// End the invocation `parent` with `update`, as its app's last update would, answering
    /// whether it was still running. The daemon does this when the job the invocation holds ends.
    pub fn settle_held_invocation(
        self: &Arc<Self>,
        parent: InvocationId,
        update: InvocationUpdate,
    ) -> bool {
        let Some(peer) = self.running_peer(&parent) else {
            return false;
        };
        let Some(job) = self.pending_invocations.lock().ok().and_then(|pending| {
            pending
                .iter()
                .find(|(_, pending)| {
                    pending.peer_id == peer && pending.update.invocation() == parent
                })
                .map(|(id, _)| *id)
        }) else {
            return false;
        };
        let state = Arc::clone(self);
        tilo_rt::app().spawn(async move {
            if let Err(error) = state.apply_invocation_update(peer, job, update, None).await {
                tracing::warn!(%error, "the invocation a job held could not be ended");
            }
        });
        true
    }

    /// Run the handler `handler`, `{"resource", "action", "arguments"}`, as a child of the
    /// invocation `parent`, handed the job as `record` says it stands in each argument the app left
    /// for it (`{"$job": null}`): the app `app` publishes it in `workspace`, and it is admitted as
    /// that invocation's own caller. `None` when no such invocation runs here, or when `handler` names
    /// no action.
    ///
    /// The app is started with no window when it publishes nothing there yet: a handler the app
    /// asked for runs whether or not a window of it is open.
    pub fn invoke_job_handler(
        self: &Arc<Self>,
        parent: InvocationId,
        workspace: &str,
        app: &str,
        handler: &Value,
        record: &Value,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        let peer = self.running_peer(&parent)?;
        let (caller, _reach) = self.running_for(peer, parent)?;
        let (resource, action) = handler_address(handler)?;
        let arguments = handler_arguments(handler, record);
        // A handler runs whether or not a window of its app is open: an app that publishes
        // nothing here yet is started with no window first.
        if !self.publishes_in(workspace, app)
            && let Some(launcher) = self.services.launcher.as_ref()
        {
            let _ = launcher.windowless(app, workspace, None, None);
        }
        let request = InvokeRequest {
            asks: false,
            invocation: handler_invocation(parent, workspace, app, &resource, &action),
            target: RemoteActionTarget {
                provider: tilo_actions::remote::workspace_provider(workspace),
                address: ActionAddress::new(resource, action),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: Some(parent),
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        Some(self.action_invoke_with_origin(&caller, None, None, None, &request))
    }

    /// Whether the app `app` publishes anything in `workspace` now.
    fn publishes_in(&self, workspace: &str, app: &str) -> bool {
        self.publishers.surfaces.lock().is_ok_and(|surfaces| {
            surfaces
                .values()
                .any(|surface| surface.app_id == app && surface.route.cluster_id == workspace)
        })
    }
}

/// The arguments `handler` is invoked with: its own, the job's `record` in each the app left for
/// the job.
fn handler_arguments(handler: &Value, record: &Value) -> serde_json::Map<String, Value> {
    let mut arguments = handler
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for argument in arguments.values_mut() {
        if argument.get(JOB_SLOT).is_some() {
            argument.clone_from(record);
        }
    }
    arguments
}

/// The resource and action `handler` names, when it names one.
fn handler_address(handler: &Value) -> Option<(String, String)> {
    let resource = handler.get("resource").and_then(Value::as_str)?.to_owned();
    let action = handler.get("action").and_then(Value::as_str)?.to_owned();
    if resource.is_empty() || action.is_empty() {
        return None;
    }
    Some((resource, action))
}

/// A handler's invocation id: derived from its parent and what it runs, so a handler dispatched
/// twice for one job's end is one call, never two.
fn handler_invocation(
    parent: InvocationId,
    workspace: &str,
    app: &str,
    resource: &str,
    action: &str,
) -> InvocationId {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo.engine-job.handler.v1\0");
    hash.update(parent.0.as_bytes());
    for part in [workspace, app, resource, action] {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize().as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    InvocationId(uuid::Uuid::from_bytes(bytes))
}

/// A job's first words, as the handlers and the end read them.
#[derive(Clone)]
struct Job {
    id: String,
    invocation: InvocationId,
    workspace: String,
    app: String,
    record: Value,
}

impl Job {
    fn of(record: &Value) -> Option<Self> {
        Some(Self {
            id: record.get("id").and_then(Value::as_str)?.to_owned(),
            invocation: record
                .get("invocation")
                .and_then(Value::as_str)?
                .parse::<InvocationId>()
                .ok()?,
            workspace: record.get("workspace").and_then(Value::as_str)?.to_owned(),
            app: record.get("app").and_then(Value::as_str)?.to_owned(),
            record: record.clone(),
        })
    }

    /// The handler `kind` the app asked for, when it asked for one.
    fn handler(&self, kind: &str) -> Option<Value> {
        self.record
            .get(kind)
            .filter(|handler| !handler.is_null())
            .cloned()
    }
}

/// The broker's job handlers: what a daemon's `DeviceJobs` calls as its jobs move.
pub struct JobHandlers {
    inner: Arc<Inner>,
}

/// The broker a job's handlers run on, and each job's coalesced `progress` call.
struct Inner {
    broker: Arc<AppBrokerState>,
    /// Each job's `progress` handler: whether one is in flight, whether another is owed, and
    /// the newest record the owed one runs for.
    progress: Mutex<HashMap<String, Coalesced>>,
}

#[derive(Default)]
struct Coalesced {
    in_flight: bool,
    owed: Option<Value>,
}

impl JobHandlers {
    /// The handlers of a broker's engine jobs.
    #[must_use]
    pub fn new(broker: &Arc<AppBrokerState>) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Inner {
                broker: Arc::clone(broker),
                progress: Mutex::default(),
            }),
        })
    }
}

impl Inner {
    /// Run the handler `kind` for `job` and wait for its answer, `None` when the job names no
    /// such handler or the invocation no longer runs.
    fn call(&self, job: &Job, kind: &str) -> Option<InvocationReceipt> {
        let handler = job.handler(kind)?;
        self.broker
            .invoke_job_handler(
                job.invocation,
                &job.workspace,
                &job.app,
                &handler,
                &job.record,
            )?
            .wait_timeout(HANDLER_DEADLINE)
            .ok()
            .flatten()
            .and_then(Result::ok)
    }

    /// Run the job's `progress` handler now, or owe one after the call in flight.
    fn progress(self: &Arc<Self>, job: &Job) {
        if job.handler("progress").is_none() {
            return;
        }
        {
            let Ok(mut progress) = self.progress.lock() else {
                return;
            };
            let entry = progress.entry(job.id.clone()).or_default();
            if entry.in_flight {
                entry.owed = Some(job.record.clone());
                return;
            }
            entry.in_flight = true;
        }
        self.run_progress(job.clone());
    }

    /// Run one progress call, then the newest owed one, on a thread of its own.
    fn run_progress(self: &Arc<Self>, job: Job) {
        let state = Arc::clone(self);
        let id = job.id.clone();
        std::thread::spawn(move || {
            let _ = state.call(&job, "progress");
            state.finished(&id);
        });
    }

    /// A progress call finished: run the one owed, or let the next move start a new one.
    fn finished(self: &Arc<Self>, job: &str) {
        let owed = {
            let Ok(mut progress) = self.progress.lock() else {
                return;
            };
            let Some(entry) = progress.get_mut(job) else {
                return;
            };
            if let Some(record) = entry.owed.take() {
                Some(record)
            } else {
                progress.remove(job);
                None
            }
        };
        if let Some(job) = owed.and_then(|record| Job::of(&record)) {
            self.run_progress(job);
        }
    }
}

impl JobEffects for JobHandlers {
    fn progress(&self, record: &Value) {
        let Some(job) = Job::of(record) else {
            return;
        };
        // Coalesced, on a thread of its own: the reporter never waits on the app.
        let inner = Arc::clone(&self.inner);
        std::thread::spawn(move || inner.progress(&job));
    }

    fn ended(&self, record: &Value) {
        let Some(job) = Job::of(record) else {
            return;
        };
        let inner = Arc::clone(&self.inner);
        // A final progress runs before done, then the invocation ends with done's outcome.
        std::thread::spawn(move || {
            let _ = inner.call(&job, "progress");
            let outcome = match inner.call(&job, "done") {
                Some(receipt) => update_of(job.invocation, receipt),
                None => job_outcome(job.invocation, &job.record),
            };
            inner.broker.settle_held_invocation(job.invocation, outcome);
        });
    }

    fn hold(&self, invocation: &str) {
        self.inner.broker.hold_jobs(invocation);
    }

    fn release(&self, invocation: &str) {
        self.inner.broker.release_jobs(invocation);
    }
}

/// What a ended job's own record comes to, for an invocation no handler ends.
fn job_outcome(invocation: InvocationId, record: &Value) -> InvocationUpdate {
    let detail = record.get("detail").cloned().unwrap_or(Value::Null);
    match record.get("state").and_then(Value::as_str) {
        Some("done") => InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(detail),
                ..ActionOutput::default()
            },
        },
        Some("cancelled") => InvocationUpdate::Cancelled {
            invocation,
            message: why(&detail).map(str::to_owned),
        },
        _ => failed(invocation, why(&detail).unwrap_or("the job failed")),
    }
}

/// Why a job's record ended as it did, under `why`, the one field its detail names it in.
fn why(detail: &Value) -> Option<&str> {
    detail.get("why").and_then(Value::as_str)
}

/// What a handler's receipt comes to, as its invocation's ending.
fn update_of(invocation: InvocationId, receipt: InvocationReceipt) -> InvocationUpdate {
    match receipt.status {
        ReceiptStatus::Completed { output } => InvocationUpdate::Succeeded { invocation, output },
        ReceiptStatus::Failed { error } => InvocationUpdate::Failed { invocation, error },
        ReceiptStatus::Rejected { error } => failed(invocation, &error.to_string()),
        ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
            invocation,
            message,
        },
        ReceiptStatus::Accepted => {
            failed(invocation, "the handler did not answer within the deadline")
        }
    }
}

fn failed(invocation: InvocationId, why: &str) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: "job_handler_failed".into(),
            message: why.to_owned(),
            details: None,
            retryable: false,
        },
    }
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
