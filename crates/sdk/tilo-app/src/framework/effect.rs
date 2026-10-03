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
use std::cell::RefCell;
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
use ui_core::widget::{ResourceOperation, ResourceOperationStart};

use super::dispatch::Dispatcher;
use super::live::Live;

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

    /// Answer with `result`.
    pub fn finish(self, result: Result<ActionOutput, String>) {
        self.progress.finish(Box::new(result));
    }
}

impl std::fmt::Debug for EffectReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectReply").finish_non_exhaustive()
    }
}

type Then<R, H> = Box<dyn FnOnce(&mut H, Box<dyn Any + Send>, &mut EffectCx<R, H>) -> EffectAnswer>;

/// A job a handler or a completion started, not yet tracked.
struct Started<R: Resource, H> {
    events: Receiver<JobEvent>,
    then: Option<Then<R, H>>,
    cancelled: Arc<AtomicBool>,
    task: Option<ui_core::rt::JoinHandle<()>>,
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
}

/// What an effect looks like to callers right now, when it differs from its
/// declaration: unavailable while the engine is offline, labelled "Stop
/// recording" while recording.
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

    fn overlay(&self, spec: &mut ActionSpec) {
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
    sequence: u64,
}

impl<R: Resource, H> From<Started<R, H>> for Job<R, H> {
    fn from(started: Started<R, H>) -> Self {
        Self {
            events: started.events,
            then: started.then,
            cancelled: started.cancelled,
            task: started.task,
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
    Finished(Option<Result<ActionOutput, String>>),
}

/// The jobs one handler runs, keyed by what they answer.
pub(crate) struct Jobs<R: Resource, H, K = InvocationId> {
    running: HashMap<K, Job<R, H>>,
}

impl<R: Resource, H, K> Default for Jobs<R, H, K> {
    fn default() -> Self {
        Self {
            running: HashMap::new(),
        }
    }
}

impl<R: Resource, H: 'static, K: Eq + Hash + Clone> Jobs<R, H, K> {
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
        let event = match job.events.try_recv() {
            Ok(event) => event,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.running.remove(key);
                return Some(Polled::Finished(Some(Err(
                    "the effect ended without an answer".into(),
                ))));
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
            return Some(Polled::Finished(Some(
                value
                    .downcast::<Result<ActionOutput, String>>()
                    .map_or_else(
                        |_| Err("the effect answered with another type".into()),
                        |result| *result,
                    ),
            )));
        };
        let mut cx = EffectCx::new(env.clone(), invocation);
        let answer = then(host, value, &mut cx);
        Some(match (answer, cx.started.take()) {
            (Ok(Effected::Pending), Some(next)) => {
                self.insert(key.clone(), next);
                Polled::Finished(None)
            }
            (Ok(Effected::Pending), None) => Polled::Finished(Some(Err(
                "the effect's completion answered later but started no job".into(),
            ))),
            (answer, next) => {
                if let Some(next) = next {
                    next.stop();
                }
                Polled::Finished(Some(answer.map(|answer| match answer {
                    Effected::Done(output) => output,
                    Effected::Pending => ActionOutput::default(),
                })))
            }
        })
    }
}

fn failed(invocation: InvocationId, message: String) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: ActionFailure {
            code: "effect_failed".into(),
            message,
            details: None,
            retryable: false,
        },
    }
}

/// The handler of one effect vocabulary `E`, and the jobs it runs.
pub(crate) struct EffectRunner<R: Resource, H, E> {
    env: EffectEnv<R>,
    jobs: Jobs<R, H>,
    marker: std::marker::PhantomData<fn(E)>,
}

impl<R: Resource, H: 'static, E: Effect> EffectRunner<R, H, E> {
    pub(crate) fn new(env: EffectEnv<R>) -> Self {
        Self {
            env,
            jobs: Jobs::default(),
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
        if request.kind != tilo_actions::OperationKind::Effect || request.type_id != E::OPERATION {
            return Ok(None);
        }
        let effect = E::from_payload(request.payload)?;
        let id = effect.id();
        let job = E::declarations()
            .iter()
            .any(|decl| decl.id == id && decl.is_job());
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
            Polled::Finished(Some(Err(message))) => Some(failed(invocation, message)),
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

/// A projection drain, type-erased for the root.
trait Drainer<R: Resource> {
    /// Start what is pending in `state` and not running or just settled.
    fn run(&mut self, state: &R::State, revision: u64);
    /// Settle finished jobs at `revision`; whether any did.
    fn poll(&mut self, revision: u64) -> bool;
}

type Pending<R, K, T> = Box<dyn Fn(&<R as Resource>::State) -> Vec<(K, T)>>;

struct Drain<R: Resource, K, T> {
    env: EffectEnv<R>,
    pending: Pending<R, K, T>,
    run: Handle<R, T>,
    jobs: Jobs<R, (), K>,
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
                        tracing::warn!(%error, kind = R::KIND, "a projection entry failed");
                    }
                    self.settled.insert(key.clone(), revision);
                    settled = true;
                }
                break;
            }
        }
        settled
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
}

/// Every effect handler and projection drain of one live resource root, and
/// what they publish. Clones share it.
pub(crate) struct Effects<R: Resource> {
    env: Option<EffectEnv<R>>,
    inner: Rc<RefCell<Inner<R>>>,
}

impl<R: Resource> Clone for Effects<R> {
    fn clone(&self) -> Self {
        Self {
            env: self.env.clone(),
            inner: Rc::clone(&self.inner),
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
        }
    }
}

impl<R: Resource> Effects<R> {
    /// Effects of the live app, which run.
    pub(crate) fn live(dispatch: Dispatcher<R>, live: Live<R>) -> Self {
        Self {
            env: Some(EffectEnv {
                dispatch,
                live,
                wake: WakeHandle::new(),
            }),
            inner: Rc::default(),
        }
    }

    /// Effects of a past version, which never run: History shows what the
    /// document was, it does not do again what was done.
    pub(crate) fn inert() -> Self {
        Self {
            env: None,
            inner: Rc::default(),
        }
    }

    pub(crate) fn env(&self) -> Option<&EffectEnv<R>> {
        self.env.as_ref()
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
    ) {
        let Some(env) = self.env.clone() else {
            return;
        };
        let mut inner = self.inner.borrow_mut();
        inner.drains.push(Drains {
            alive,
            drain: Box::new(Drain {
                env,
                pending: Box::new(pending),
                run: Box::new(run),
                jobs: Jobs::default(),
                settled: HashMap::new(),
            }),
        });
        // A new drain looks at the document as it stands.
        inner.drained = None;
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
        if !settled && inner.drained == Some(revision) {
            return;
        }
        inner.drained = Some(revision);
        for entry in &mut inner.drains {
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
            dispatch: Dispatcher::new("test", Probe::KIND, false),
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
