//! Effects and jobs: work with no document delta, invoked through the same
//! door as actions, answered by the app.
//!
//! An app names its effects as a typed vocabulary (`#[derive(Effect)]` on an
//! enum, see [`tilo_resource::effect`]) and handles them where the state they
//! need lives: a view registers a handler with
//! [`ViewCx::effects`](super::ViewCx::effects), a custom surface implements
//! [`ResourceSurface::effect`](super::ResourceSurface::effect). The framework
//! publishes the vocabulary on the resource (with the availability and labels
//! the handler reports right now), routes every invocation — a button, the
//! palette, an agent, a script — to the handler as a typed value, and runs the
//! job the handler starts: progress, cancellation and the result go back to
//! whoever invoked it. A job's completion runs on the UI thread with the
//! [`Dispatcher`], so what the effect changes in the document it changes with
//! ordinary actions.
//!
//! A projection — a document action that queued an outside consequence (an
//! outbox entry, a filesystem operation, a device configuration) — is drained
//! with [`ViewCx::drain`](super::ViewCx::drain): the framework runs it in the
//! live app after each change, on every participant's device.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use tilo_actions::{
    ActionFailure, ActionOutput, ActionSpec, Availability, InvocationId, InvocationUpdate,
};
use tilo_resource::{ActionRef, Effect, Invoke, Resource};
use ui_core::WakeHandle;
use ui_core::portal::{ContentsOutcome, PickRequest, PickedContents, PortalOutcome, SaveOutcome};
use ui_core::widget::{ResourceOperation, ResourceOperationStart};

use super::dispatch::Dispatcher;
use super::live::Live;

/// Why a file chooser answered nothing.
const CHOOSER_GONE: &str = "the file chooser closed without answering";

/// What an effect handler (or a job's completion) answers.
#[derive(Debug)]
pub enum Effected {
    /// Finished: the output goes back to whoever invoked the effect.
    Done(ActionOutput),
    /// Still running: the job started through the [`EffectCx`] answers later.
    Pending,
}

impl Effected {
    /// Finished, with nothing to report.
    pub fn done() -> Self {
        Self::Done(ActionOutput::default())
    }

    /// Finished, answering with `value` (an effect declared `returns`, or a
    /// job's result).
    pub fn value(value: impl serde::Serialize) -> Self {
        Self::Done(ActionOutput {
            value: Some(serde_json::to_value(value).unwrap_or(serde_json::Value::Null)),
            resources: Vec::new(),
        })
    }
}

/// What a handler or a job's completion returns: the answer, or why the
/// effect failed.
pub type EffectAnswer = Result<Effected, String>;

/// A message from a running job to the UI thread.
enum JobEvent {
    Progress {
        phase: String,
        completed: Option<u64>,
        total: Option<u64>,
    },
    Finished(Box<dyn Any + Send>),
}

/// A running job's channel back to the app: report progress, notice
/// cancellation. Cloneable and `Send`, for the worker.
#[derive(Clone)]
pub struct Progress {
    events: Sender<JobEvent>,
    cancelled: Arc<AtomicBool>,
    wake: WakeHandle,
}

impl Progress {
    /// Report where the job is: a phase, and how far through it.
    pub fn report(&self, phase: impl Into<String>, completed: Option<u64>, total: Option<u64>) {
        if self
            .events
            .send(JobEvent::Progress {
                phase: phase.into(),
                completed,
                total,
            })
            .is_ok()
        {
            self.wake.wake();
        }
    }

    /// Whether the caller cancelled the job (an effect declared
    /// `cancellable`). A worker checks between units of work and stops; its
    /// result is discarded.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn finish(&self, value: Box<dyn Any + Send>) {
        if self.events.send(JobEvent::Finished(value)).is_ok() {
            self.wake.wake();
        }
    }
}

/// The answer of an effect, given later from wherever its work settles — a
/// reply from another service, a callback, a worker. `Send`. Dropping it
/// unanswered fails the effect.
pub struct EffectReply {
    progress: Progress,
}

impl EffectReply {
    /// Report progress, as [`Progress::report`].
    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    /// Whether the caller cancelled the effect.
    pub fn is_cancelled(&self) -> bool {
        self.progress.is_cancelled()
    }

    /// Answer with `result`; a failure is told to the caller as its
    /// message.
    pub fn finish(self, result: Result<ActionOutput, String>) {
        self.answer(result.map_err(failure));
    }

    /// Answer with `result`, a failure carrying its code (`portal_failed`,
    /// `device_busy`) for programs and whether trying again may help.
    pub fn answer(self, result: Result<ActionOutput, ActionFailure>) {
        self.progress.finish(Box::new(result));
    }
}

impl std::fmt::Debug for EffectReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectReply").finish_non_exhaustive()
    }
}

type Then<R, H> = Box<dyn FnOnce(&mut H, Box<dyn Any + Send>, &mut EffectCx<R, H>) -> EffectAnswer>;

/// An answer a job waits for without a worker: polled on the UI thread each
/// time the job is, `Some` once it arrived.
type Source = Box<dyn FnMut() -> Option<Box<dyn Any + Send>>>;

/// A job a handler or a completion started, not yet tracked.
struct Started<R: Resource, H> {
    events: Receiver<JobEvent>,
    then: Option<Then<R, H>>,
    cancelled: Arc<AtomicBool>,
    task: Option<ui_core::rt::JoinHandle<()>>,
    source: Option<Source>,
}

impl<R: Resource, H> Started<R, H> {
    fn stop(self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(task) = self.task {
            task.abort();
        }
    }
}

/// What every effect context of one resource shares.
pub(crate) struct EffectEnv<R: Resource> {
    pub(crate) dispatch: Dispatcher<R>,
    pub(crate) live: Live<R>,
    pub(crate) wake: WakeHandle,
}

impl<R: Resource> Clone for EffectEnv<R> {
    fn clone(&self) -> Self {
        Self {
            dispatch: self.dispatch.clone(),
            live: self.live.clone(),
            wake: self.wake.clone(),
        }
    }
}

/// What an effect handler, a job's completion and a projection drain
/// receive: the resource, its dispatcher, and the means to start a job.
///
/// `H` is what a completion gets mutable access to: the surface that
/// handles the effect (`&mut self`), or `()` for a handler a view registered.
pub struct EffectCx<R: Resource, H = ()> {
    env: EffectEnv<R>,
    invocation: Option<InvocationId>,
    started: Option<Started<R, H>>,
}

impl<R: Resource, H: 'static> EffectCx<R, H> {
    pub(crate) fn new(env: EffectEnv<R>, invocation: Option<InvocationId>) -> Self {
        Self {
            env,
            invocation,
            started: None,
        }
    }

    /// Where the effect's document changes go: ordinary actions, checked,
    /// applied, undone and replicated like any other.
    pub fn dispatcher(&self) -> Dispatcher<R> {
        self.env.dispatch.clone()
    }

    /// Submit one typed action, as [`Dispatcher::apply`].
    pub fn apply(&self, action: &R::Action) {
        self.env.dispatch.apply(action);
    }

    /// Invoke a declared action, as [`Dispatcher::invoke`].
    pub fn invoke(&self, invoke: Invoke<R>) {
        self.env.dispatch.invoke(invoke);
    }

    /// The resource's live state.
    pub fn live(&self) -> &Live<R> {
        &self.env.live
    }

    /// The invocation being answered; `None` for a projection drain, which
    /// nobody invoked.
    pub fn invocation(&self) -> Option<InvocationId> {
        self.invocation
    }

    /// Wakes the app from another thread, to draw what a worker changed.
    pub fn waker(&self) -> WakeHandle {
        self.env.wake.clone()
    }

    fn track(&mut self, then: Option<Then<R, H>>) -> (Progress, Arc<AtomicBool>) {
        let (events, receiver) = channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let progress = Progress {
            events,
            cancelled: Arc::clone(&cancelled),
            wake: self.env.wake.clone(),
        };
        if let Some(previous) = self.started.replace(Started {
            events: receiver,
            then,
            cancelled: Arc::clone(&cancelled),
            task: None,
            source: None,
        }) {
            tracing::warn!("an effect started a second job; the first one is stopped");
            previous.stop();
        }
        (progress, cancelled)
    }

    fn then<T: Send + 'static>(
        then: impl FnOnce(&mut H, T, &mut Self) -> EffectAnswer + 'static,
    ) -> Then<R, H> {
        Box::new(move |host, value, cx| match value.downcast::<T>() {
            Ok(value) => then(host, *value, cx),
            Err(_) => Err("a job finished with a value of another type".into()),
        })
    }

    /// Run `work`'s future on the app runtime, then `then` with its output on
    /// the UI thread. The future reports through the [`Progress`] it is
    /// built with; cancelling the effect aborts it.
    pub fn spawn<T, F>(
        &mut self,
        work: impl FnOnce(Progress) -> F,
        then: impl FnOnce(&mut H, T, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected
    where
        T: Send + 'static,
        F: std::future::Future<Output = T> + Send + 'static,
    {
        let (progress, _) = self.track(Some(Self::then(then)));
        let future = work(progress.clone());
        let task = ui_core::rt::app().spawn(async move {
            let value = future.await;
            progress.finish(Box::new(value));
        });
        if let Some(started) = self.started.as_mut() {
            started.task = Some(task);
        }
        Effected::Pending
    }

    /// Run blocking `work` off the UI thread, then `then` with its result on
    /// the UI thread. `work` reports through its [`Progress`] and checks it
    /// for cancellation.
    pub fn thread<T>(
        &mut self,
        work: impl FnOnce(Progress) -> T + Send + 'static,
        then: impl FnOnce(&mut H, T, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected
    where
        T: Send + 'static,
    {
        let (progress, _) = self.track(Some(Self::then(then)));
        let task = ui_core::rt::app().spawn_blocking(move || {
            let value = work(progress.clone());
            progress.finish(Box::new(value));
        });
        if let Some(started) = self.started.as_mut() {
            started.task = Some(task);
        }
        Effected::Pending
    }

    /// Answer later, from wherever the work settles. Return
    /// [`Effected::Pending`] and keep the reply until then.
    pub fn reply(&mut self) -> EffectReply {
        let (progress, _) = self.track(None);
        EffectReply { progress }
    }

    /// Run `then` on the UI thread with what `receiver` delivers, `None`
    /// when its sender went away unanswered. Nothing waits on a thread (a
    /// browser page has none to spare): the job looks each time the app
    /// wakes, so the sender must wake it, as the host's services do.
    fn wait_for<T: Send + 'static>(
        &mut self,
        receiver: Receiver<T>,
        then: impl FnOnce(&mut H, Option<T>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let (progress, _) = self.track(Some(Self::then(then)));
        if let Some(started) = self.started.as_mut() {
            started.source = Some(Box::new(move || {
                // The job's own channel stays open while it waits here.
                let _waiting = &progress;
                match receiver.try_recv() {
                    Ok(value) => Some(Box::new(Some(value)) as Box<dyn Any + Send>),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(Box::new(None::<T>)),
                }
            }));
        }
        Effected::Pending
    }

    /// Ask the person to choose a file with the host's chooser (`request`
    /// is an Open request), then run `then` on the UI thread with its name
    /// and contents, or `None` when they cancelled. Works where files have
    /// no paths, such as a browser page. The effect must be declared `job`.
    pub fn choose_file(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Option<PickedContents>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let chosen = request.send_contents();
        self.wait_for(chosen, move |host, outcome, cx| match outcome {
            Some(ContentsOutcome::Picked(files)) => then(host, files.into_iter().next(), cx),
            Some(ContentsOutcome::Cancelled) => then(host, None, cx),
            Some(ContentsOutcome::Failed(reason)) => Err(reason),
            None => Err(CHOOSER_GONE.into()),
        })
    }

    /// Ask the person to choose a file with the host's chooser and run
    /// `then` on the UI thread with its path on this device, or `None` when
    /// they cancelled: for a file the app reads in place rather than into
    /// memory (a disk image, a video). Only a host whose files have paths
    /// answers; elsewhere use [`Self::choose_file`]. The effect must be
    /// declared `job`.
    pub fn choose_path(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Option<std::path::PathBuf>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let chosen = request.send();
        self.wait_for(chosen, move |host, outcome, cx| match outcome {
            Some(PortalOutcome::Opened(path)) => then(host, Some(path), cx),
            Some(PortalOutcome::Cancelled) => then(host, None, cx),
            Some(PortalOutcome::Failed(reason)) => Err(reason),
            None => Err(CHOOSER_GONE.into()),
        })
    }

    /// Ask the person where to save `bytes` with the host's chooser
    /// (`request` is a Save request suggesting a file name; a browser page
    /// downloads them under it), then run `then` on the UI thread with
    /// whether they were saved: `false` when the person cancelled. The
    /// effect must be declared `job`.
    pub fn save_file(
        &mut self,
        request: PickRequest,
        bytes: Vec<u8>,
        then: impl FnOnce(&mut H, bool, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let saved = request.send_save_contents(bytes);
        self.wait_for(saved, move |host, outcome, cx| match outcome {
            Some(SaveOutcome::Saved) => then(host, true, cx),
            Some(SaveOutcome::Cancelled) => then(host, false, cx),
            Some(SaveOutcome::Failed(reason)) => Err(reason),
            None => Err(CHOOSER_GONE.into()),
        })
    }
}

/// Work a view or surface starts on its own, with nobody invoking an
/// effect: a route fetched when the destination changes, a feed refreshed
/// when it opens. Each job runs under a key; starting another under a key
/// still running stops the earlier one, so the latest request wins. A job
/// is started like an effect's ([`EffectCx::thread`], [`EffectCx::spawn`])
/// and finishes the same way, on the UI thread, with ordinary actions; a
/// surface's completion gets `&mut` the surface. Jobs run only in a window
/// someone uses: a past version and a capture start none.
///
/// Get one from [`SurfaceCx::jobs`](super::SurfaceCx::jobs) or
/// [`ViewCx::jobs`](super::ViewCx::jobs). Clones share the jobs.
pub struct Jobs<R: Resource> {
    env: Option<EffectEnv<R>>,
    queue: Rc<RefCell<JobQueue>>,
}

impl<R: Resource> Clone for Jobs<R> {
    fn clone(&self) -> Self {
        Self {
            env: self.env.clone(),
            queue: Rc::clone(&self.queue),
        }
    }
}

/// Where a running job last said it is, as its [`Progress`] reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobProgress {
    pub phase: String,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

/// What the jobs' owner has yet to take up.
#[derive(Default)]
struct JobQueue {
    /// Jobs started and not yet tracked, each a `Started<R, H>`.
    started: Vec<(String, Box<dyn Any>)>,
    cancelled: Vec<String>,
    running: std::collections::HashSet<String>,
    /// The last progress each running job reported.
    progress: HashMap<String, JobProgress>,
}

impl<R: Resource> Jobs<R> {
    pub(crate) fn new(env: Option<EffectEnv<R>>) -> Self {
        Self {
            env,
            queue: Rc::default(),
        }
    }

    /// Start the job `key` as `start` does it, through an [`EffectCx`] whose
    /// completions get `&mut H`: the surface itself for a surface's jobs
    /// (`jobs.start::<Self>(…)`), `()` for a view's. Work it finishes at
    /// once needs no job; a failure is logged, as nobody waits for it.
    pub fn start<H: 'static>(
        &self,
        key: impl Into<String>,
        start: impl FnOnce(&mut EffectCx<R, H>) -> EffectAnswer,
    ) {
        let Some(env) = &self.env else {
            return;
        };
        let key = key.into();
        let mut cx = EffectCx::<R, H>::new(env.clone(), None);
        let answer = start(&mut cx);
        match (answer, cx.started.take()) {
            (Ok(Effected::Pending), Some(started)) => {
                let mut queue = self.queue.borrow_mut();
                queue.cancelled.retain(|cancelled| *cancelled != key);
                queue.started.retain(|(queued, _)| *queued != key);
                queue.running.insert(key.clone());
                queue.progress.remove(&key);
                queue.started.push((key, Box::new(started)));
            }
            (answer, started) => {
                if let Some(started) = started {
                    started.stop();
                }
                if let Err(error) = answer {
                    tracing::warn!(%error, kind = R::KIND, %key, "a job failed to start");
                }
            }
        }
    }

    /// Stop the job `key`; its result, if it comes, is dropped.
    pub fn cancel(&self, key: &str) {
        let mut queue = self.queue.borrow_mut();
        queue.started.retain(|(queued, _)| queued != key);
        queue.progress.remove(key);
        if queue.running.remove(key) {
            queue.cancelled.push(key.to_owned());
        }
    }

    /// Whether the job `key` is still running.
    pub fn is_running(&self, key: &str) -> bool {
        self.queue.borrow().running.contains(key)
    }

    /// Where the running job `key` last said it is, for the view to show
    /// and publish; `None` before its first report and once it ended.
    pub fn progress(&self, key: &str) -> Option<JobProgress> {
        self.queue.borrow().progress.get(key).cloned()
    }
}

/// The jobs one owner runs, polled where its completions get `&mut H`.
pub(crate) struct OwnedJobs<R: Resource, H> {
    jobs: Jobs<R>,
    running: Running<R, H, String>,
}

impl<R: Resource, H: 'static> OwnedJobs<R, H> {
    pub(crate) fn new(jobs: Jobs<R>) -> Self {
        Self {
            jobs,
            running: Running::default(),
        }
    }

    /// Take up the jobs started since, stop the cancelled ones, and finish
    /// those that are done with `host`.
    pub(crate) fn poll(&mut self, host: &mut H) {
        let Some(env) = self.jobs.env.clone() else {
            return;
        };
        {
            let mut queue = self.jobs.queue.borrow_mut();
            for key in std::mem::take(&mut queue.cancelled) {
                self.running.cancel(&key);
            }
            let mut others = Vec::new();
            for (key, started) in std::mem::take(&mut queue.started) {
                match started.downcast::<Started<R, H>>() {
                    Ok(started) => self.running.insert(key, *started),
                    Err(started) => others.push((key, started)),
                }
            }
            queue.started = others;
        }
        for key in self.running.keys() {
            while let Some(polled) = self.running.poll(&key, host, &env, None) {
                let result = match polled {
                    Polled::Progress {
                        phase,
                        completed,
                        total,
                        ..
                    } => {
                        self.jobs.queue.borrow_mut().progress.insert(
                            key.clone(),
                            JobProgress {
                                phase,
                                completed,
                                total,
                            },
                        );
                        continue;
                    }
                    Polled::Finished(result) => result,
                };
                if let Some(result) = result {
                    if let Err(error) = result {
                        tracing::warn!(error = %error.message, kind = R::KIND, %key, "a job failed");
                    }
                    let mut queue = self.jobs.queue.borrow_mut();
                    queue.running.remove(&key);
                    queue.progress.remove(&key);
                }
                break;
            }
        }
    }
}

/// What an effect, or one of the resource's own actions, looks like to
/// callers right now, when it differs from its declaration: unavailable
/// while the engine is offline, labelled "Stop recording" while recording.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectStatus {
    id: &'static str,
    availability: Option<Availability>,
    label: Option<String>,
}

impl EffectStatus {
    /// `effect` cannot be invoked now: `code` names why for programs,
    /// `reason` for people.
    pub fn unavailable<E, A>(
        effect: ActionRef<E, A>,
        code: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: effect.id,
            availability: Some(Availability::Unavailable {
                code: code.into(),
                reason: reason.into(),
            }),
            label: None,
        }
    }

    /// `effect` is offered as `label` now.
    pub fn labelled<E, A>(effect: ActionRef<E, A>, label: impl Into<String>) -> Self {
        Self {
            id: effect.id,
            availability: None,
            label: Some(label.into()),
        }
    }

    /// Offer the effect as `label` too.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The id of the action or effect this status is about.
    pub(crate) fn id(&self) -> &'static str {
        self.id
    }

    /// Show `spec` as this status says it is right now.
    pub(crate) fn overlay(&self, spec: &mut ActionSpec) {
        if let Some(availability) = &self.availability {
            spec.contract.availability = availability.clone();
        }
        if let Some(label) = &self.label {
            spec.label.clone_from(label);
        }
    }
}

/// The specs of `E`, as callers see them now.
pub(crate) fn published_specs<E: Effect>(statuses: &[EffectStatus]) -> Vec<ActionSpec> {
    E::specs()
        .into_iter()
        .map(|mut spec| {
            let id = spec.id.clone();
            for status in statuses.iter().filter(|status| status.id == id) {
                status.overlay(&mut spec);
            }
            spec
        })
        .collect()
}

/// A tracked job.
struct Job<R: Resource, H> {
    events: Receiver<JobEvent>,
    then: Option<Then<R, H>>,
    cancelled: Arc<AtomicBool>,
    task: Option<ui_core::rt::JoinHandle<()>>,
    source: Option<Source>,
    sequence: u64,
}

impl<R: Resource, H> From<Started<R, H>> for Job<R, H> {
    fn from(started: Started<R, H>) -> Self {
        Self {
            events: started.events,
            then: started.then,
            cancelled: started.cancelled,
            task: started.task,
            source: started.source,
            sequence: 0,
        }
    }
}

impl<R: Resource, H> Job<R, H> {
    fn stop(self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(task) = self.task {
            task.abort();
        }
    }
}

/// What a job reported when polled.
enum Polled {
    Progress {
        sequence: u64,
        phase: String,
        completed: Option<u64>,
        total: Option<u64>,
    },
    /// Finished, or chained into another job (`None`).
    Finished(Option<Result<ActionOutput, ActionFailure>>),
}

/// The jobs one handler runs, keyed by what they answer.
pub(crate) struct Running<R: Resource, H, K = InvocationId> {
    running: HashMap<K, Job<R, H>>,
}

impl<R: Resource, H, K> Default for Running<R, H, K> {
    fn default() -> Self {
        Self {
            running: HashMap::new(),
        }
    }
}

impl<R: Resource, H: 'static, K: Eq + Hash + Clone> Running<R, H, K> {
    fn insert(&mut self, key: K, started: Started<R, H>) {
        if let Some(previous) = self.running.insert(key, started.into()) {
            previous.stop();
        }
    }

    fn contains(&self, key: &K) -> bool {
        self.running.contains_key(key)
    }

    fn keys(&self) -> Vec<K> {
        self.running.keys().cloned().collect()
    }

    /// Stop the job answering `key`; its result, if it comes, is dropped.
    fn cancel(&mut self, key: &K) -> bool {
        self.running.remove(key).map(Job::stop).is_some()
    }

    /// The next thing the job `key` reports, running its completion on this
    /// thread when it finished.
    fn poll(
        &mut self,
        key: &K,
        host: &mut H,
        env: &EffectEnv<R>,
        invocation: Option<InvocationId>,
    ) -> Option<Polled> {
        let job = self.running.get_mut(key)?;
        let answered = job.source.as_mut().and_then(|source| source());
        let event = match answered.map_or_else(
            || job.events.try_recv(),
            |value| Ok(JobEvent::Finished(value)),
        ) {
            Ok(event) => event,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.running.remove(key);
                return Some(Polled::Finished(Some(Err(failure(
                    "the effect ended without an answer".into(),
                )))));
            }
        };
        let value = match event {
            JobEvent::Progress {
                phase,
                completed,
                total,
            } => {
                job.sequence = job.sequence.saturating_add(1);
                return Some(Polled::Progress {
                    sequence: job.sequence,
                    phase,
                    completed,
                    total,
                });
            }
            JobEvent::Finished(value) => value,
        };
        let job = self.running.remove(key)?;
        let Some(then) = job.then else {
            let answered = match value.downcast::<Result<ActionOutput, ActionFailure>>() {
                Ok(result) => *result,
                Err(_) => Err(failure("the effect answered with another type".into())),
            };
            return Some(Polled::Finished(Some(answered)));
        };
        let mut cx = EffectCx::new(env.clone(), invocation);
        let answer = then(host, value, &mut cx);
        Some(match (answer, cx.started.take()) {
            (Ok(Effected::Pending), Some(next)) => {
                self.insert(key.clone(), next);
                Polled::Finished(None)
            }
            (Ok(Effected::Pending), None) => Polled::Finished(Some(Err(failure(
                "the effect's completion answered later but started no job".into(),
            )))),
            (answer, next) => {
                if let Some(next) = next {
                    next.stop();
                }
                Polled::Finished(Some(
                    answer
                        .map(|answer| match answer {
                            Effected::Done(output) => output,
                            Effected::Pending => ActionOutput::default(),
                        })
                        .map_err(failure),
                ))
            }
        })
    }
}

/// An effect's failure told only by its message.
fn failure(message: String) -> ActionFailure {
    ActionFailure {
        code: "effect_failed".into(),
        message,
        details: None,
        retryable: false,
    }
}

fn failed(invocation: InvocationId, error: ActionFailure) -> InvocationUpdate {
    InvocationUpdate::Failed { invocation, error }
}

/// The handler of one effect vocabulary `E`, and the jobs it runs.
pub(crate) struct EffectRunner<R: Resource, H, E> {
    env: EffectEnv<R>,
    jobs: Running<R, H>,
    marker: std::marker::PhantomData<fn(E)>,
}

impl<R: Resource, H: 'static, E: Effect> EffectRunner<R, H, E> {
    pub(crate) fn new(env: EffectEnv<R>) -> Self {
        Self {
            env,
            jobs: Running::default(),
            marker: std::marker::PhantomData,
        }
    }

    /// Answer `op` when it is one of `E`'s effects: decode it, hand it to
    /// `handle`, and start what it started. `Ok(None)` for any other
    /// operation.
    ///
    /// # Errors
    /// Returns the handler's failure, or why its answer breaks the effect's
    /// declaration.
    pub(crate) fn start(
        &mut self,
        op: &ResourceOperation<'_>,
        host: &mut H,
        handle: impl FnOnce(&mut H, E, &mut EffectCx<R, H>) -> EffectAnswer,
    ) -> Result<Option<ResourceOperationStart>, String> {
        // An invocation in a caller's custody stays with handlers that
        // carry it.
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if request.type_id != E::OPERATION {
            return Ok(None);
        }
        let effect = E::from_payload(request.payload)?;
        let id = effect.id();
        let declaration = E::declarations().into_iter().find(|decl| decl.id == id);
        // A view only changes what its caller sees and is admitted as one;
        // an effect invoked as a view would skip the checks an effect gets.
        if declaration.as_ref().map(|decl| decl.kind) != Some(request.kind) {
            return Err(format!("`{id}` was invoked as another kind of operation"));
        }
        let job = declaration.is_some_and(|decl| decl.is_job());
        let mut cx = EffectCx::new(self.env.clone(), Some(request.invocation));
        let answer = handle(host, effect, &mut cx);
        let started = cx.started.take();
        match (answer?, started, job) {
            (Effected::Done(output), started, false) => {
                if let Some(started) = started {
                    started.stop();
                }
                Ok(Some(ResourceOperationStart::Completed(output)))
            }
            // A job's caller learns its result as the job's, even when it
            // was ready at once.
            (Effected::Done(output), started, true) => {
                if let Some(started) = started {
                    started.stop();
                }
                let reply = cx.reply();
                reply.finish(Ok(output));
                if let Some(ready) = cx.started.take() {
                    self.jobs.insert(request.invocation, ready);
                }
                Ok(Some(ResourceOperationStart::Accepted))
            }
            (Effected::Pending, Some(started), true) => {
                self.jobs.insert(request.invocation, started);
                Ok(Some(ResourceOperationStart::Accepted))
            }
            (Effected::Pending, Some(started), false) => {
                started.stop();
                Err(format!(
                    "`{id}` is declared to answer at once; declare it `#[effect(job)]` to answer later"
                ))
            }
            (Effected::Pending, None, _) => {
                Err(format!("`{id}` answered later but started no job"))
            }
        }
    }

    /// The next update of the job answering `invocation`, if this runner
    /// runs it.
    pub(crate) fn poll(
        &mut self,
        invocation: InvocationId,
        cancel: bool,
        host: &mut H,
    ) -> Option<InvocationUpdate> {
        if !self.jobs.contains(&invocation) {
            return None;
        }
        if cancel && self.jobs.cancel(&invocation) {
            return Some(InvocationUpdate::Cancelled {
                invocation,
                message: None,
            });
        }
        match self
            .jobs
            .poll(&invocation, host, &self.env, Some(invocation))?
        {
            Polled::Progress {
                sequence,
                phase,
                completed,
                total,
            } => Some(InvocationUpdate::Progress {
                invocation,
                sequence,
                phase,
                completed,
                total,
                message: None,
            }),
            Polled::Finished(None) => None,
            Polled::Finished(Some(Ok(output))) => {
                Some(InvocationUpdate::Succeeded { invocation, output })
            }
            Polled::Finished(Some(Err(error))) => Some(failed(invocation, error)),
        }
    }
}

/// One registered effect vocabulary: what the root publishes for it.
struct Published {
    alive: Weak<()>,
    operation: &'static str,
    specs: Box<dyn Fn() -> Vec<ActionSpec>>,
}

/// A view's effect handler, type-erased for the root.
trait ViewHandler<R: Resource> {
    fn start(
        &mut self,
        op: &ResourceOperation<'_>,
    ) -> Result<Option<ResourceOperationStart>, String>;
    fn poll(&mut self, invocation: InvocationId, cancel: bool) -> Option<InvocationUpdate>;
}

type Handle<R, E> = Box<dyn FnMut(E, &mut EffectCx<R>) -> EffectAnswer>;

struct Handler<R: Resource, E> {
    runner: EffectRunner<R, (), E>,
    handle: Handle<R, E>,
}

impl<R: Resource, E: Effect> ViewHandler<R> for Handler<R, E> {
    fn start(
        &mut self,
        op: &ResourceOperation<'_>,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let handle = &mut self.handle;
        self.runner
            .start(op, &mut (), |(), effect, cx| handle(effect, cx))
    }

    fn poll(&mut self, invocation: InvocationId, cancel: bool) -> Option<InvocationUpdate> {
        self.runner.poll(invocation, cancel, &mut ())
    }
}

/// A registered projection drain ([`ViewCx::drain`](super::ViewCx::drain)),
/// as the view or surface that registered it holds it.
///
/// A drain looks at the document after every change. When what it compares
/// the document against lives outside — the configuration a service reports
/// back — tell it that changed with [`Self::outside_changed`], and on the
/// next frame it carries out again every entry it still lists.
#[derive(Clone, Default)]
pub struct Projection {
    /// `None` where drains never run: History, a capture.
    outside_changed: Option<Rc<Cell<bool>>>,
}

impl Projection {
    /// The outside world the drain compares the document against changed:
    /// carry out what is still pending on the next frame, even what was
    /// carried out at this revision already.
    pub fn outside_changed(&self) {
        if let Some(flag) = &self.outside_changed {
            flag.set(true);
        }
    }
}

impl std::fmt::Debug for Projection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Projection")
            .field("runs", &self.outside_changed.is_some())
            .finish()
    }
}

/// A projection drain, type-erased for the root.
trait Drainer<R: Resource> {
    /// Start what is pending in `state` and not running or just settled.
    fn run(&mut self, state: &R::State, revision: u64);
    /// Settle finished jobs at `revision`; whether any did.
    fn poll(&mut self, revision: u64) -> bool;
    /// Forget which entries finished, so those still pending run again.
    fn forget_settled(&mut self);
}

type Pending<R, K, T> = Box<dyn Fn(&<R as Resource>::State) -> Vec<(K, T)>>;

struct Drain<R: Resource, K, T> {
    env: EffectEnv<R>,
    pending: Pending<R, K, T>,
    run: Handle<R, T>,
    jobs: Running<R, (), K>,
    /// Entries whose run finished, and the revision it finished at: they run
    /// again only once the document moved on and still lists them.
    settled: HashMap<K, u64>,
}

impl<R: Resource, K: Eq + Hash + Clone + 'static, T: 'static> Drainer<R> for Drain<R, K, T> {
    fn run(&mut self, state: &R::State, revision: u64) {
        let pending = (self.pending)(state);
        self.settled
            .retain(|key, _| pending.iter().any(|(entry, _)| entry == key));
        for (key, entry) in pending {
            if self.jobs.contains(&key) || self.settled.get(&key) == Some(&revision) {
                continue;
            }
            let mut cx = EffectCx::new(self.env.clone(), None);
            let answer = (self.run)(entry, &mut cx);
            match (answer, cx.started.take()) {
                (Ok(Effected::Pending), Some(started)) => self.jobs.insert(key, started),
                (answer, started) => {
                    if let Some(started) = started {
                        started.stop();
                    }
                    if let Err(error) = answer {
                        tracing::warn!(%error, kind = R::KIND, "a projection entry failed");
                    }
                    self.settled.insert(key, revision);
                }
            }
        }
    }

    fn poll(&mut self, revision: u64) -> bool {
        let mut settled = false;
        for key in self.jobs.keys() {
            while let Some(polled) = self.jobs.poll(&key, &mut (), &self.env, None) {
                let Polled::Finished(result) = polled else {
                    continue;
                };
                if let Some(result) = result {
                    if let Err(error) = result {
                        tracing::warn!(error = %error.message, kind = R::KIND, "a projection entry failed");
                    }
                    self.settled.insert(key.clone(), revision);
                    settled = true;
                }
                break;
            }
        }
        settled
    }

    fn forget_settled(&mut self) {
        self.settled.clear();
    }
}

struct Registered<R: Resource> {
    alive: Weak<()>,
    operation: &'static str,
    handler: Box<dyn ViewHandler<R>>,
}

struct Drains<R: Resource> {
    alive: Weak<()>,
    drain: Box<dyn Drainer<R>>,
}

struct Inner<R: Resource> {
    published: Vec<Published>,
    handlers: Vec<Registered<R>>,
    drains: Vec<Drains<R>>,
    /// The revision drains last ran at.
    drained: Option<u64>,
    /// Set when the outside world a drain compares against changed, so the
    /// drains look again although the document did not move.
    outside_changed: Rc<Cell<bool>>,
}

/// Every effect handler and projection drain of one live resource root, and
/// what they publish. Clones share it.
pub(crate) struct Effects<R: Resource> {
    env: Option<EffectEnv<R>>,
    inner: Rc<RefCell<Inner<R>>>,
    /// The jobs the views start on their own, polled with the drains.
    jobs: Rc<RefCell<OwnedJobs<R, ()>>>,
}

impl<R: Resource> Clone for Effects<R> {
    fn clone(&self) -> Self {
        Self {
            env: self.env.clone(),
            inner: Rc::clone(&self.inner),
            jobs: Rc::clone(&self.jobs),
        }
    }
}

impl<R: Resource> Default for Inner<R> {
    fn default() -> Self {
        Self {
            published: Vec::new(),
            handlers: Vec::new(),
            drains: Vec::new(),
            drained: None,
            outside_changed: Rc::default(),
        }
    }
}

impl<R: Resource> Effects<R> {
    /// Effects of the live app, which run.
    pub(crate) fn live(dispatch: Dispatcher<R>, live: Live<R>) -> Self {
        let env = EffectEnv {
            dispatch,
            live,
            wake: WakeHandle::new(),
        };
        Self {
            jobs: Rc::new(RefCell::new(OwnedJobs::new(Jobs::new(Some(env.clone()))))),
            env: Some(env),
            inner: Rc::default(),
        }
    }

    /// Effects of a past version, which never run: History shows what the
    /// document was, it does not do again what was done.
    pub(crate) fn inert() -> Self {
        Self {
            env: None,
            inner: Rc::default(),
            jobs: Rc::new(RefCell::new(OwnedJobs::new(Jobs::new(None)))),
        }
    }

    pub(crate) fn env(&self) -> Option<&EffectEnv<R>> {
        self.env.as_ref()
    }

    /// The jobs the views start on their own.
    pub(crate) fn view_jobs(&self) -> Jobs<R> {
        self.jobs.borrow().jobs.clone()
    }

    /// Jobs for one surface, whose completions get `&mut` the surface.
    pub(crate) fn surface_jobs(&self) -> Jobs<R> {
        Jobs::new(self.env.clone())
    }

    /// Take up and finish the views' jobs.
    pub(crate) fn poll_jobs(&self) {
        self.jobs.borrow_mut().poll(&mut ());
    }

    /// Publish vocabulary `E` while `alive` lives, as `specs` describes it
    /// at each publication.
    pub(crate) fn publish<E: Effect>(
        &self,
        alive: Weak<()>,
        specs: impl Fn() -> Vec<ActionSpec> + 'static,
    ) {
        self.inner.borrow_mut().published.push(Published {
            alive,
            operation: E::OPERATION,
            specs: Box::new(specs),
        });
    }

    pub(crate) fn handle<E: Effect>(
        &self,
        alive: Weak<()>,
        handle: impl FnMut(E, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) {
        let Some(env) = self.env.clone() else {
            return;
        };
        self.inner.borrow_mut().handlers.push(Registered {
            alive,
            operation: E::OPERATION,
            handler: Box::new(Handler {
                runner: EffectRunner::<R, (), E>::new(env),
                handle: Box::new(handle),
            }),
        });
    }

    pub(crate) fn drain<K: Eq + Hash + Clone + 'static, T: 'static>(
        &self,
        alive: Weak<()>,
        pending: impl Fn(&R::State) -> Vec<(K, T)> + 'static,
        run: impl FnMut(T, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> Projection {
        let Some(env) = self.env.clone() else {
            return Projection::default();
        };
        let mut inner = self.inner.borrow_mut();
        inner.drains.push(Drains {
            alive,
            drain: Box::new(Drain {
                env,
                pending: Box::new(pending),
                run: Box::new(run),
                jobs: Running::default(),
                settled: HashMap::new(),
            }),
        });
        // A new drain looks at the document as it stands.
        inner.drained = None;
        Projection {
            outside_changed: Some(Rc::clone(&inner.outside_changed)),
        }
    }

    fn prune(inner: &mut Inner<R>) {
        inner
            .published
            .retain(|entry| entry.alive.strong_count() > 0);
        inner
            .handlers
            .retain(|entry| entry.alive.strong_count() > 0);
        inner.drains.retain(|entry| entry.alive.strong_count() > 0);
    }

    /// The operation types of every published vocabulary.
    pub(crate) fn operations(&self) -> Vec<&'static str> {
        let mut inner = self.inner.borrow_mut();
        Self::prune(&mut inner);
        inner
            .published
            .iter()
            .map(|entry| entry.operation)
            .collect()
    }

    /// Every published effect, as callers see it now.
    pub(crate) fn specs(&self) -> Vec<ActionSpec> {
        let mut inner = self.inner.borrow_mut();
        Self::prune(&mut inner);
        inner
            .published
            .iter()
            .flat_map(|entry| (entry.specs)())
            .collect()
    }

    /// Answer `op` with the view handler of its vocabulary.
    pub(crate) fn start(
        &self,
        op: &ResourceOperation<'_>,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let mut inner = self.inner.borrow_mut();
        Self::prune(&mut inner);
        for entry in &mut inner.handlers {
            if op
                .unscoped()
                .is_some_and(|request| request.type_id == entry.operation)
                && let Some(started) = entry.handler.start(op)?
            {
                return Ok(Some(started));
            }
        }
        Ok(None)
    }

    pub(crate) fn poll(&self, invocation: InvocationId, cancel: bool) -> Option<InvocationUpdate> {
        let mut inner = self.inner.borrow_mut();
        inner
            .handlers
            .iter_mut()
            .find_map(|entry| entry.handler.poll(invocation, cancel))
    }

    /// Settle finished projection jobs, then start what the document at
    /// `revision` lists and is not running yet.
    pub(crate) fn drain_projections(&self, state: &R::State, revision: u64) {
        let mut inner = self.inner.borrow_mut();
        if inner.drains.is_empty() {
            return;
        }
        Self::prune(&mut inner);
        let mut settled = false;
        for entry in &mut inner.drains {
            settled |= entry.drain.poll(revision);
        }
        let outside_changed = inner.outside_changed.replace(false);
        if !settled && !outside_changed && inner.drained == Some(revision) {
            return;
        }
        inner.drained = Some(revision);
        for entry in &mut inner.drains {
            if outside_changed {
                entry.drain.forget_settled();
            }
            entry.drain.run(state, revision);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;
    use tilo_resource::{Model, Reader, resource};

    #[derive(Model, Default, Debug, Clone)]
    pub struct Probe {
        pub count: i64,
    }

    #[resource(kind = "test.probe", label = "Probe")]
    impl Probe {
        /// Set the count.
        #[action]
        pub fn set(&mut self, count: i64) {
            self.count = count;
        }
    }

    #[derive(Clone, Debug, tilo_resource::Effect)]
    #[effect(operation = "test.probe.effects")]
    enum Work {
        /// Count until cancelled.
        #[effect(cancellable)]
        Count,
        /// Answer at once.
        Ping,
    }

    fn runner() -> EffectRunner<Probe, (), Work> {
        let mut doc = tilo_resource::Doc::new();
        Probe::initialize(&mut doc).unwrap();
        EffectRunner::new(EffectEnv {
            dispatch: Dispatcher::new("test", Probe::KIND, false, None),
            live: Live::new(Reader::<Probe>::read(&doc).unwrap()),
            wake: WakeHandle::detached(),
        })
    }

    fn payload(effect: &Work) -> Vec<u8> {
        Work::specs()
            .into_iter()
            .find(|spec| spec.id == effect.id())
            .unwrap()
            .build_json(&effect.arguments())
            .unwrap()
            .payload
    }

    #[test]
    fn cancelling_a_job_stops_its_worker_and_drops_its_result() {
        let mut runner = runner();
        let invocation = InvocationId::new();
        let payload = payload(&Work::Count);
        let op = ResourceOperation::local(
            invocation,
            tilo_actions::OperationKind::Effect,
            Work::OPERATION,
            &payload,
        );
        let (stopped, observed) = channel();
        let started = runner
            .start(&op, &mut (), |(), _, cx| {
                Ok(cx.thread(
                    move |progress| {
                        while !progress.is_cancelled() {
                            std::thread::yield_now();
                        }
                        let _ = stopped.send(());
                        7
                    },
                    |(), count: i32, _| Ok(Effected::value(count)),
                ))
            })
            .unwrap();
        assert!(matches!(started, Some(ResourceOperationStart::Accepted)));
        assert!(runner.poll(invocation, false, &mut ()).is_none());
        assert!(matches!(
            runner.poll(invocation, true, &mut ()),
            Some(InvocationUpdate::Cancelled { .. })
        ));
        // The worker stops, or never starts when cancelled first.
        assert!(!matches!(
            observed.recv_timeout(std::time::Duration::from_secs(30)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(
            runner.poll(invocation, false, &mut ()).is_none(),
            "a cancelled job's result is dropped"
        );
    }

    #[test]
    fn an_effect_declared_immediate_cannot_answer_later() {
        let mut runner = runner();
        let payload = payload(&Work::Ping);
        let op = ResourceOperation::local(
            InvocationId::new(),
            tilo_actions::OperationKind::Effect,
            Work::OPERATION,
            &payload,
        );
        let refused = runner
            .start(&op, &mut (), |(), _, cx| {
                let _reply = cx.reply();
                Ok(Effected::Pending)
            })
            .unwrap_err();
        assert!(refused.contains("#[effect(job)]"), "{refused}");
        let answered = runner
            .start(&op, &mut (), |(), _, _| Ok(Effected::value("pong")))
            .unwrap();
        assert!(matches!(
            answered,
            Some(ResourceOperationStart::Completed(output))
                if output.value == Some(serde_json::json!("pong"))
        ));
    }
}
