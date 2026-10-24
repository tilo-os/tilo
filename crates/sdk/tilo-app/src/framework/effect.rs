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
//! An outside consequence a verb queued in an
//! [`Outbox`](tilo_resource::Outbox) is carried out with
//! [`RecordEnv::outbox`](super::RecordEnv::outbox): the framework runs the app's
//! handler in the live app on the device that owns the entry and records how
//! it settled.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::{Rc, Weak};

mod causal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use ui_core::automerge::transaction::Transactable;

use tilo_actions::{
    ActionFailure, ActionOutput, ActionSpec, Availability, InvocationId, InvocationUpdate,
};
use tilo_resource::{ActionRef, Effect, Id, Outbox, Path, Resource, Settled};
use ui_core::WakeHandle;
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart};
use ui_core::invocation_scope::{ResourceEffectScope, ResourceInvocationScope};
use ui_core::portal::{
    ContentsOutcome, DestinationOutcome, PickRequest, PickedContents, PortalOutcome,
    SaveDestination, SaveOutcome,
};

use super::dispatch::Dispatcher;
use super::live::Live;

fn attach_approved_check_reporter(
    checked: ui_core::invocation_scope::ApprovedCheckReportedWork,
    progress: Progress,
) -> ui_core::invocation_scope::ApprovedCheckFuture {
    checked(Box::new(move |observation| {
        progress.with_effect(|| {
            match observation {
                ui_core::invocation_scope::ApprovedCheckReportObservation::Reported {
                    phase,
                    completed,
                    total,
                } => {
                    progress.report(phase, completed, total);
                }
                ui_core::invocation_scope::ApprovedCheckReportObservation::Disabled { .. } => {
                    progress.report(
                        ui_core::i18n::I18nManager::lookup_plain("approved-check-reports-disabled"),
                        None,
                        None,
                    );
                }
            }
            Ok(())
        })
    }))
}

/// A file the person chose with the host's chooser
/// ([`EffectCx::choose_file`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChosenFile {
    /// The name shown to the person.
    pub name: String,
    /// The media type the host reported, when it did.
    pub media_type: Option<String>,
    pub bytes: Vec<u8>,
    /// The file's durable identity on this device, when the host chose a
    /// file it can name: the same file chosen again, even renamed or moved,
    /// has the same identity, so an import can tell it already came in.
    /// `None` where files have no paths (a browser page).
    pub identity: Option<ui_core::record::RecordId>,
}

impl From<PickedContents> for ChosenFile {
    fn from(picked: PickedContents) -> Self {
        Self {
            identity: picked
                .location
                .as_deref()
                .map(ui_core::record::RecordId::identify_file),
            name: picked.name,
            media_type: picked.media_type,
            bytes: picked.bytes,
        }
    }
}

/// Why a file chooser answered nothing.
const CHOOSER_GONE: &str = "the file chooser closed without answering";

/// What an effect handler (or a job's completion) answers.
#[derive(Clone, Debug, PartialEq)]
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

    /// An outbox entry carried out, or failed for good: how it settled
    /// ([`RecordEnv::outbox`](super::RecordEnv::outbox)). [`Self::value`] with the
    /// result alone settles it done.
    pub fn settled<O: serde::Serialize>(settled: &Settled<O>) -> Self {
        Self::value(settled)
    }

    /// Finished, answering with the content of `hash` in this device's
    /// content store (an effect declared `content`), which another app's
    /// content-collecting operation takes.
    ///
    /// # Errors
    /// Returns why `hash` names no content.
    pub fn content(hash: &str) -> Result<Self, String> {
        let resource = tilo_actions::content_resource(hash)
            .ok_or_else(|| format!("{hash:?} is not a content hash"))?;
        Ok(Self::Done(ActionOutput {
            value: None,
            resources: vec![resource],
        }))
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
    Failed(ActionFailure),
}

/// A running job's channel back to the app: report progress, notice
/// cancellation. Cloneable and `Send`, for the worker.
#[derive(Clone)]
pub struct Progress {
    events: Sender<JobEvent>,
    cancelled: Arc<AtomicBool>,
    wake: WakeHandle,
    custody: Option<ResourceEffectScope>,
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
    /// result is discarded. For caller-scoped work this also performs a fresh,
    /// potentially blocking authority check. Do not call while holding an owner
    /// store guard that the authority check may need.
    pub fn is_cancelled(&self) -> bool {
        self.with_effect(|| Ok(())).is_err()
    }

    /// Share the actual job's cancellation flag with blocking native I/O.
    /// This is explicit cancellation, not authority: callers still use
    /// [`Self::with_effect`] at work boundaries to recheck current custody.
    /// Treat the returned flag as read-only.
    #[must_use]
    pub fn cancellation_signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    /// Recheck the original caller immediately before a unit of external work.
    /// Blocking workers should use this between units as well as checking
    /// cancellation. It grants no access to the canonical document.
    /// The authority check may block; release owner store guards before calling.
    ///
    /// # Errors
    ///
    /// Returns an error if the job was cancelled, the original caller's authority
    /// lapsed, or the unit of work returned an error.
    pub fn with_effect<T>(&self, effect: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err("the effect was cancelled".into());
        }
        let admitted = || {
            // Authority checks may block while cancellation arrives. Recheck at
            // the actual work boundary, not only before acquiring authority.
            if self.cancelled.load(Ordering::Acquire) {
                return Err("the effect was cancelled".into());
            }
            effect()
        };
        match &self.custody {
            Some(custody) => custody.with_effect(admitted),
            None => admitted(),
        }
    }

    fn fail(&self, error: String) {
        self.cancelled.store(true, Ordering::Release);
        if self.events.send(JobEvent::Failed(failure(error))).is_ok() {
            self.wake.wake();
        }
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
    completion: bool,
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
        if self.completion {
            match result {
                Ok(output) => self.progress.finish(Box::new(output)),
                Err(error) => {
                    if self.progress.events.send(JobEvent::Failed(error)).is_ok() {
                        self.progress.wake.wake();
                    }
                }
            }
        } else {
            self.progress.finish(Box::new(result));
        }
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

/// What a job reports and how it is stopped: the same for every resource,
/// so one copy of it serves them all.
struct JobCore {
    events: Receiver<JobEvent>,
    cancelled: Arc<AtomicBool>,
    task: Option<ui_core::rt::JoinHandle<()>>,
    source: Option<Source>,
    sequence: u64,
    custody: Option<ResourceInvocationScope>,
}

impl JobCore {
    fn stop(self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(task) = self.task {
            task.abort();
        }
    }

    /// The next thing the job reports, if it reported anything.
    fn next(&mut self) -> Option<Next> {
        if let Some(error) = self
            .custody
            .as_ref()
            .and_then(|custody| custody.check().err())
        {
            return Some(Next::Failed(failure(error)));
        }
        let answered = self.source.as_mut().and_then(|source| source());
        let event = match answered.map_or_else(
            || self.events.try_recv(),
            |value| Ok(JobEvent::Finished(value)),
        ) {
            Ok(event) => event,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                return Some(Next::Failed(failure(
                    "the effect ended without an answer".into(),
                )));
            }
        };
        if let Some(error) = self
            .custody
            .as_ref()
            .and_then(|custody| custody.check().err())
        {
            return Some(Next::Failed(failure(error)));
        }
        Some(match event {
            JobEvent::Failed(error) => Next::Failed(error),
            JobEvent::Progress {
                phase,
                completed,
                total,
            } => {
                self.sequence = self.sequence.saturating_add(1);
                Next::Progress(Polled::Progress {
                    sequence: self.sequence,
                    phase,
                    completed,
                    total,
                })
            }
            JobEvent::Finished(value) => Next::Finished(value),
        })
    }
}

/// What [`JobCore::next`] found.
enum Next {
    Progress(Polled),
    /// The job failed or lost its custody: it is over.
    Failed(ActionFailure),
    Finished(Box<dyn Any + Send>),
}

/// The answer a job without a completion of its own gave.
fn answered(value: Box<dyn Any + Send>) -> Polled {
    Polled::Finished(Some(
        match value.downcast::<Result<ActionOutput, ActionFailure>>() {
            Ok(result) => *result,
            Err(_) => Err(failure("the effect answered with another type".into())),
        },
    ))
}

/// What a completion's answer, with the job it may have chained, comes to.
fn completed(answer: EffectAnswer, chained: bool) -> Option<Result<ActionOutput, ActionFailure>> {
    match answer {
        Ok(Effected::Pending) if chained => None,
        Ok(Effected::Pending) => Some(Err(failure(
            "the effect's completion answered later but started no job".into(),
        ))),
        Ok(Effected::Done(output)) => Some(Ok(output)),
        Err(error) => Some(Err(failure(error))),
    }
}

/// A job a handler or a completion started: what it reports, and the
/// completion that runs with what it finished with. The same type for
/// every resource and host, so one copy of the job bookkeeping serves them
/// all; only running the completion is typed ([`complete`]).
struct Job {
    core: JobCore,
    /// The completion, a `Then<R, H>`.
    then: Option<Box<dyn Any>>,
    /// The `Then<R, H>` type the job was started for: whose jobs it is.
    owner: std::any::TypeId,
}

impl Job {
    fn stop(self) {
        self.core.stop();
    }
}

/// The custody a job runs under: its worker's, and the invocation's its
/// completion is checked against.
struct JobCustody {
    worker: Option<ResourceEffectScope>,
    custody: Option<ResourceInvocationScope>,
}

impl JobCustody {
    /// The custody of a job an effect running under `worker` and `custody`
    /// starts.
    #[inline(never)]
    fn of(worker: Option<&ResourceEffectScope>, custody: Option<&ResourceInvocationScope>) -> Self {
        Self {
            worker: worker.cloned(),
            custody: custody.cloned(),
        }
    }
}

/// `custody` owned, for an effect context to keep.
#[inline(never)]
fn owned_scope(custody: Option<&ResourceInvocationScope>) -> Option<ResourceInvocationScope> {
    custody.cloned()
}

/// Start tracking a job in `started` for `owner`, with the completion
/// `then`; the [`Progress`] its work reports through. A job already
/// started there is stopped: an effect runs one job at a time.
fn track_job(
    started: &mut Option<Job>,
    wake: &WakeHandle,
    custody: JobCustody,
    then: Option<Box<dyn Any>>,
    owner: std::any::TypeId,
) -> Progress {
    let (events, receiver) = channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let progress = Progress {
        events,
        cancelled: Arc::clone(&cancelled),
        wake: wake.clone(),
        custody: custody.worker,
    };
    if let Some(previous) = started.replace(Job {
        core: JobCore {
            events: receiver,
            cancelled,
            task: None,
            source: None,
            sequence: 0,
            custody: custody.custody,
        },
        then,
        owner,
    }) {
        tracing::warn!("an effect started a second job; the first one is stopped");
        previous.stop();
    }
    progress
}

/// Run `run`, a handler or a completion, for `invocation` and under
/// `custody` when it has one: it fails once the custody is gone.
fn run_under(
    invocation: Option<InvocationId>,
    custody: Option<&ResourceInvocationScope>,
    run: &mut dyn FnMut() -> EffectAnswer,
) -> EffectAnswer {
    let _acting = ActingFor::enter(invocation);
    match custody {
        Some(custody) => custody
            .with_effect(run)
            .and_then(|answer| custody.check().map(|()| answer)),
        None => run(),
    }
}

/// A job's completion run: the completion, what the job finished with, its
/// custody and the invocation it answers; what it answered and the job it
/// chained, if any.
type Complete<'a> = dyn FnMut(
        Box<dyn Any>,
        Box<dyn Any + Send>,
        Option<ResourceInvocationScope>,
        Option<InvocationId>,
    ) -> Result<(EffectAnswer, Option<Job>), String>
    + 'a;

/// Run a job's completion `then` (a `Then<R, H>`) with `host` and what the
/// job finished with, on the UI thread: the typed part of polling a job.
fn complete<R: Resource, H: 'static>(
    env: &EffectEnv<R>,
    host: &mut H,
    then: Box<dyn Any>,
    value: Box<dyn Any + Send>,
    custody: Option<ResourceInvocationScope>,
    invocation: Option<InvocationId>,
) -> Result<(EffectAnswer, Option<Job>), String> {
    let Ok(then) = then.downcast::<Then<R, H>>() else {
        return Err("a job's completion is of another type".into());
    };
    let scope = owned_scope(custody.as_ref());
    let mut cx = EffectCx::<R, H>::new(env.clone(), invocation).with_custody(custody)?;
    let mut then = Some(*then);
    let mut value = Some(value);
    let answer = run_under(
        invocation,
        scope.as_ref(),
        &mut || match (then.take(), value.take()) {
            (Some(then), Some(value)) => then(host, value, &mut cx),
            _ => Err("a job's completion ran twice".into()),
        },
    );
    Ok((answer, cx.started.take()))
}

/// What every effect context of one resource shares.
pub(crate) struct EffectEnv<R: Resource> {
    pub(crate) dispatch: Dispatcher<R>,
    pub(crate) live: Live<R>,
    pub(crate) wake: WakeHandle,
    scope: Option<std::rc::Weak<super::surface::SurfaceScope>>,
}

impl<R: Resource> Clone for EffectEnv<R> {
    fn clone(&self) -> Self {
        Self {
            dispatch: self.dispatch.clone(),
            live: self.live.clone(),
            wake: self.wake.clone(),
            scope: self.scope.clone(),
        }
    }
}

impl<R: Resource> EffectEnv<R> {
    pub(crate) fn scoped(&self, scope: &Rc<super::surface::SurfaceScope>) -> Self {
        let mut env = self.clone();
        env.scope = Some(Rc::downgrade(scope));
        env
    }
    fn active(&self) -> bool {
        self.scope
            .as_ref()
            .is_none_or(|scope| scope.upgrade().is_some_and(|scope| scope.active()))
    }
    fn track(&self, progress: &Progress) {
        if let Some(scope) = &self.scope {
            if let Some(scope) = scope.upgrade() {
                scope.track(&progress.cancelled);
            } else {
                progress.cancelled.store(true, Ordering::Release);
            }
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
    started: Option<Job>,
    /// The resource's document while a surface's handler answers an
    /// invocation, lent to it for reading.
    document: Option<Rc<RefCell<tilo_resource::Doc>>>,
    /// The lent document's heads, taken when it was lent.
    heads: Vec<ui_core::automerge::ChangeHash>,
    custody: Option<ResourceInvocationScope>,
    worker_custody: Option<ResourceEffectScope>,
    target: Option<ui_core::host_operation::OperationTarget>,
    /// Where the invoker asked the effect to act (`#[param(place)]`).
    place: Option<tilo_resource::Place>,
    /// What its completions get `&mut` access to.
    host: std::marker::PhantomData<fn(&mut H)>,
}

/// The place `effect` was told to act at: its argument declared
/// `#[param(place)]`, when given.
fn place_of<E: Effect>(
    decl: &tilo_resource::effect::EffectDecl,
    effect: &E,
) -> Option<tilo_resource::Place> {
    let param = decl.params.iter().find(|param| param.is_place())?;
    let value = effect.arguments().remove(param.name.as_ref())?;
    serde_json::from_value(value).ok()
}

/// The resource as it was at a past version a view asked for: its
/// document there, the heads that name it and the name a caller gives it
/// back ([`EffectCx::version`]).
pub struct DocumentVersion {
    pub doc: tilo_resource::Doc,
    pub heads: Vec<ui_core::automerge::ChangeHash>,
    pub name: String,
}

/// An effect's future, its output boxed.
type BoxedFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Box<dyn Any + Send>> + Send>>;

/// An effect's blocking work, its output boxed.
type BoxedWork = Box<dyn FnOnce(Progress) -> Box<dyn Any + Send> + Send>;

/// Run an effect's future on the app runtime, polled under its custody, and
/// deliver what it finished with. Every effect's future runs through this
/// one function and task type.
fn spawn_future(progress: Progress, future: BoxedFuture) -> ui_core::rt::JoinHandle<()> {
    ui_core::rt::app().spawn(async move {
        let mut future = future;
        let value = std::future::poll_fn(|cx| {
            match progress.with_effect(|| Ok(future.as_mut().poll(cx))) {
                Ok(std::task::Poll::Ready(value)) => std::task::Poll::Ready(Ok(value)),
                Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
                Err(error) => std::task::Poll::Ready(Err(error)),
            }
        })
        .await;
        match value {
            Ok(value) => progress.finish(value),
            Err(error) => progress.fail(error),
        }
    })
}

/// Run an effect's blocking work on the app runtime's blocking threads,
/// under its custody, and deliver what it finished with ([`spawn_future`]).
fn spawn_work(progress: Progress, work: BoxedWork) -> ui_core::rt::JoinHandle<()> {
    ui_core::rt::app().spawn_blocking(move || {
        match progress.with_effect(|| Ok(work(progress.clone()))) {
            Ok(value) => progress.finish(value),
            Err(error) => progress.fail(error),
        }
    })
}

impl<R: Resource, H: 'static> EffectCx<R, H> {
    pub(crate) fn new(env: EffectEnv<R>, invocation: Option<InvocationId>) -> Self {
        Self {
            env,
            invocation,
            started: None,
            document: None,
            heads: Vec::new(),
            custody: None,
            worker_custody: None,
            target: None,
            place: None,
            host: std::marker::PhantomData,
        }
    }

    /// Where the invoker asked this effect to act (its `#[param(place)]`
    /// argument): act there instead of at their caret. Once it succeeds
    /// for a participant, they land there ([`Live::place_of`]).
    pub fn place(&self) -> Option<&tilo_resource::Place> {
        self.place.as_ref()
    }

    fn with_custody(mut self, custody: Option<ResourceInvocationScope>) -> Result<Self, String> {
        let (worker, lent) = custody_scopes(custody.as_ref())?;
        self.worker_custody = worker;
        if let Some(scope) = lent {
            self.env.dispatch = self.env.dispatch.for_custody(scope);
        }
        self.custody = custody;
        Ok(self)
    }

    /// The resource's document as it is now, to read what a view answers
    /// with: the state at its heads, a selection's measurements, a picture
    /// of it. Only a surface's handler answering an invocation has it; a
    /// job's completion, which runs later, reads the live state instead.
    /// Changes go through the dispatcher, never here.
    pub fn document(&self) -> Option<std::cell::Ref<'_, tilo_resource::Doc>> {
        self.document.as_ref().map(|document| document.borrow())
    }

    /// The heads of the document as it is now; empty without one.
    pub fn heads(&self) -> Vec<ui_core::automerge::ChangeHash> {
        self.heads.clone()
    }

    /// The resource at `version`, as a caller names one: a change id from
    /// its history (or enough of one) or a checkpoint's name.
    ///
    /// # Errors
    /// Returns why `version` names no version of this document, or that
    /// the handler has no document to read.
    pub fn version(&self, version: &str) -> Result<DocumentVersion, String> {
        let document = self
            .document
            .as_ref()
            .ok_or_else(|| "this effect has no document to read".to_owned())?;
        let mut doc = document
            .try_borrow_mut()
            .map_err(|_| "read a past version before borrowing the document".to_owned())?;
        let heads = ui_core::history_source::steps::resolve(&doc, version)?;
        let past = doc
            .fork_at(&heads)
            .map_err(|error| format!("version {version:?} is not in this document: {error}"))?;
        Ok(DocumentVersion {
            doc: past,
            name: super::history::version_name(&heads),
            heads,
        })
    }

    /// Where the effect's document changes go: ordinary actions, checked,
    /// applied, undone and replicated like any other.
    pub fn dispatcher(&self) -> Dispatcher<R> {
        self.env.dispatch.clone()
    }

    /// The person this invocation runs for: its invoker, when a host
    /// delivered it for someone, else the participant the view is for.
    /// `None` for a bot, an agent or an app acting, and in a past version.
    /// An effect that answers or changes what is someone's own ("my link")
    /// reads whose it is here, never from the view.
    pub fn person(&self) -> Option<String> {
        match ui_core::invocation_scope::current_invoker() {
            Some(invoker) => {
                (invoker.kind == tilo_actions::ActorKind::Person).then_some(invoker.actor)
            }
            None => self.env.dispatch.me().map(str::to_owned),
        }
    }

    /// Submit one typed action, as [`Dispatcher::apply`].
    pub fn apply(&self, action: &R::Action) {
        self.env.dispatch.apply(action);
    }

    /// Submit a field edit or a verb, as [`Dispatcher::submit`].
    pub fn submit(&self, submit: impl super::Submit<R>) {
        self.env.dispatch.submit(submit);
    }

    /// The resource's live state.
    pub fn live(&self) -> &Live<R> {
        &self.env.live
    }

    /// The actual source invocation this work continues. Ordinary background
    /// projection drains have none; causal drains retain the current Document
    /// caller. Its source identity may differ from the local reply operation.
    pub fn invocation(&self) -> Option<InvocationId> {
        self.invocation
    }

    /// Submit the exact approved moderation operation through the record's host.
    /// Pass the returned Send future to the action's existing job.
    ///
    /// # Errors
    /// Refuses projection drains, changed dispatch custody or unavailable transport.
    pub fn external_moderation(
        &self,
        operation: registry_types::ExternalModerationOperation,
    ) -> Result<ui_core::invocation_scope::ExternalModerationFuture, String> {
        if let Some(custody) = &self.custody {
            return custody.external_moderation(operation);
        }
        let target = self
            .target
            .as_ref()
            .ok_or("external moderation requires an exact dispatched record target")?;
        if target.route().app_id != self.env.dispatch.app_id() {
            return Err("external moderation dispatch app changed".into());
        }
        target.external_moderation(
            self.invocation
                .ok_or("external moderation requires a dispatched invocation")?,
            operation,
        )
    }

    /// Name the paths this invocation is about to write (absolute paths; a
    /// folder covers what is under it), before writing them: when an agent
    /// acting for the session's person invoked it, the versions those
    /// writes make record that agent, for its person. The host takes who
    /// acted from its own custody of the invocation, never from here. Wait
    /// on the receipt where the writes run (a [`Self::thread`]), then write
    /// whatever it answered. `None` when there is nobody to name: no
    /// invocation's custody, or one that is not an agent's.
    pub fn attribute_writes(
        &self,
        paths: Vec<String>,
    ) -> Option<ui_core::invocation_scope::WriteAttributionReceipt> {
        match self.custody.as_ref()?.attribute_writes(paths) {
            Ok(receipt) => receipt,
            Err(error) => {
                tracing::debug!(%error, "these writes stay their person's in file history");
                None
            }
        }
    }

    /// Run an approved project check through this effect's admitted host channel.
    /// Commands, source paths and resource bounds are chosen by the project host.
    /// Pass the returned future to the existing effect job with [`Self::spawn`].
    ///
    /// # Errors
    /// Refuses projection drains, absent original custody or unavailable transport.
    pub fn approved_check(
        &self,
        check_id: &str,
    ) -> Result<ui_core::invocation_scope::ApprovedCheckFuture, String> {
        if let Some(custody) = &self.custody {
            return custody.approved_check(check_id);
        }
        let target = self
            .target
            .as_ref()
            .ok_or("approved checks require an exact dispatched record target")?;
        if target.route().app_id != self.env.dispatch.app_id() {
            return Err("approved check dispatch app changed".into());
        }
        target.approved_check(
            self.invocation
                .ok_or("approved checks require a dispatched invocation")?,
            check_id,
        )
    }

    /// Attach approved-check reports to this action's existing job progress.
    /// The app remains the sole publisher of invocation-update sequences.
    ///
    /// # Errors
    /// Refuses absent dispatch custody or unavailable approved transport.
    pub fn approved_check_reported(
        &self,
        check_id: &str,
    ) -> Result<
        impl FnOnce(Progress) -> ui_core::invocation_scope::ApprovedCheckFuture
        + Send
        + 'static
        + use<R, H>,
        String,
    > {
        let checked = if let Some(custody) = &self.custody {
            custody.approved_check_reported(check_id)?
        } else {
            let target = self
                .target
                .as_ref()
                .ok_or("approved checks require an exact dispatched record target")?;
            if target.route().app_id != self.env.dispatch.app_id() {
                return Err("approved check dispatch app changed".into());
            }
            target.approved_check_reported(
                self.invocation
                    .ok_or("approved checks require a dispatched invocation")?,
                check_id,
            )?
        };
        Ok(move |progress: Progress| attach_approved_check_reporter(checked, progress))
    }

    /// Wakes the app from another thread, to draw what a worker changed.
    pub fn waker(&self) -> WakeHandle {
        self.env.wake.clone()
    }

    fn track(&mut self, then: Option<Box<dyn Any>>) -> Progress {
        let progress = track_job(
            &mut self.started,
            &self.env.wake,
            JobCustody::of(self.worker_custody.as_ref(), self.custody.as_ref()),
            then,
            std::any::TypeId::of::<Then<R, H>>(),
        );
        self.env.track(&progress);
        progress
    }

    fn then<T: Send + 'static>(
        then: impl FnOnce(&mut H, T, &mut Self) -> EffectAnswer + 'static,
    ) -> Box<dyn Any> {
        let then: Then<R, H> = Box::new(move |host, value, cx| match value.downcast::<T>() {
            Ok(value) => then(host, *value, cx),
            Err(_) => Err("a job finished with a value of another type".into()),
        });
        Box::new(then)
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
        let progress = self.track(Some(Self::then(then)));
        let future = work(progress.clone());
        let task = spawn_future(
            progress,
            Box::pin(async move { Box::new(future.await) as Box<dyn Any + Send> }),
        );
        if let Some(started) = self.started.as_mut() {
            started.core.task = Some(task);
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
        let progress = self.track(Some(Self::then(then)));
        let task = spawn_work(
            progress,
            Box::new(move |progress| Box::new(work(progress)) as Box<dyn Any + Send>),
        );
        if let Some(started) = self.started.as_mut() {
            started.core.task = Some(task);
        }
        Effected::Pending
    }

    /// Answer later, from wherever the work settles. Return
    /// [`Effected::Pending`] and keep the reply until then.
    pub fn reply(&mut self) -> EffectReply {
        let progress = self.track(None);
        EffectReply {
            progress,
            completion: false,
        }
    }

    /// Answer later, then settle the successful receipt on the UI thread
    /// under this effect's retained invocation. Refusal skips the completion
    /// and preserves its structured failure. Cancellation and current custody
    /// are checked by the same job path as [`Self::spawn`].
    pub fn reply_then(
        &mut self,
        then: impl FnOnce(&mut H, ActionOutput, &mut Self) -> EffectAnswer + 'static,
    ) -> EffectReply {
        let progress = self.track(Some(Self::then(then)));
        EffectReply {
            progress,
            completion: true,
        }
    }

    /// Answer once what this effect submitted through its
    /// [`dispatcher`](Self::dispatcher) landed: each submission goes through
    /// the door like any other, checked again for the effect's invoker, and
    /// `then` runs with the last one's [`Landed`](super::Landed) — what the door answered
    /// (what an edit changed, as the resource's surface reports it) and the
    /// items it created — to reply to the effect's caller. A refused
    /// submission fails the effect with why. The effect must be declared
    /// `job`, since it answers after the edits apply.
    ///
    /// ```ignore
    /// cx.dispatcher().submit(Doc::BODY.edit(op));
    /// cx.when_landed(|_, landed, _| Ok(Effected::value(landed.answer)))
    /// ```
    ///
    /// # Errors
    /// When the effect was not invoked (a view's own job) or submitted
    /// nothing to wait for.
    pub fn when_landed(
        &mut self,
        then: impl FnOnce(&mut H, super::Landed, &mut Self) -> EffectAnswer + 'static,
    ) -> EffectAnswer {
        let invocation = self
            .invocation
            .ok_or("only an invoked effect waits for what it submitted")?;
        let receiver = self
            .env
            .dispatch
            .wait_landed(invocation)
            .ok_or("the effect submitted nothing to wait for")?;
        Ok(self.wait_for(receiver, |host, landed, cx| match landed {
            Some(Ok(landed)) => then(host, landed, cx),
            Some(Err(refused)) => Err(refused),
            None => Err("the submission was dropped before it landed".into()),
        }))
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
        self.wait_on(move || receiver.try_recv(), then)
    }

    /// [`Self::wait_for`] for an answer `poll` looks for, in the way of
    /// [`Receiver::try_recv`].
    fn wait_on<T: Send + 'static>(
        &mut self,
        mut poll: impl FnMut() -> Result<T, TryRecvError> + 'static,
        then: impl FnOnce(&mut H, Option<T>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let progress = self.track(Some(Self::then(then)));
        if let Some(started) = self.started.as_mut() {
            started.core.source = Some(Box::new(move || {
                // The job's own channel stays open while it waits here.
                let _waiting = &progress;
                match poll() {
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
    /// no paths, such as a browser page. Bound reads with
    /// [`PickRequest::max_contents_bytes`]; an oversized selection fails the
    /// job before calling `then`. The effect must be declared `job`.
    pub fn choose_file(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Option<ChosenFile>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let chosen = request.send_contents();
        self.wait_for(chosen, move |host, outcome, cx| match outcome {
            Some(ContentsOutcome::Picked(files)) => {
                then(host, files.into_iter().next().map(ChosenFile::from), cx)
            }
            Some(ContentsOutcome::Cancelled) => then(host, None, cx),
            Some(ContentsOutcome::Failed(reason)) => Err(reason),
            None => Err(CHOOSER_GONE.into()),
        })
    }

    /// [`Self::choose_file`] for a request that lets the person choose
    /// several files ([`PickRequest::open_multiple`]): `then` receives every
    /// chosen file, none when they cancelled. A request contents limit applies
    /// to the total bytes across the entire selection.
    pub fn choose_files(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Vec<ChosenFile>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let chosen = request.send_contents();
        self.wait_for(chosen, move |host, outcome, cx| match outcome {
            Some(ContentsOutcome::Picked(files)) => {
                then(host, files.into_iter().map(ChosenFile::from).collect(), cx)
            }
            Some(ContentsOutcome::Cancelled) => then(host, Vec::new(), cx),
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

    /// Ask the person where to save with the host's chooser (`request` is
    /// a Save request suggesting a file name), then run `then` on the UI
    /// thread with the place, or `None` when they cancelled. The place takes
    /// bytes made after the choice, and can be kept to write there again on
    /// every later Save without asking: a path where files have paths, the
    /// browser's own Save dialog file on a browser page. The effect must be
    /// declared `job`.
    pub fn choose_destination(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Option<SaveDestination>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let chosen = request.send_save_destination();
        self.wait_on(
            move || chosen.try_recv(),
            move |host, outcome, cx| match outcome {
                Some(DestinationOutcome::Chosen(destination)) => then(host, Some(destination), cx),
                Some(DestinationOutcome::Cancelled) => then(host, None, cx),
                Some(DestinationOutcome::Failed(reason)) => Err(reason),
                None => Err(CHOOSER_GONE.into()),
            },
        )
    }

    /// Run `then` on the UI thread once `destination` holds `bytes`: `Ok`
    /// when they were saved, the reason when not. The effect must be
    /// declared `job`.
    pub fn write_destination(
        &mut self,
        destination: &SaveDestination,
        bytes: Vec<u8>,
        then: impl FnOnce(&mut H, Result<(), String>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let written = destination.write(bytes);
        self.wait_for(written, move |host, outcome, cx| match outcome {
            Some(SaveOutcome::Saved) => then(host, Ok(()), cx),
            Some(SaveOutcome::Cancelled) => then(host, Err("the save was cancelled".into()), cx),
            Some(SaveOutcome::Failed(reason)) => then(host, Err(reason), cx),
            None => Err(CHOOSER_GONE.into()),
        })
    }

    /// Ask the host to show what a launch naming `arguments` opens in this
    /// window, in place of the record it shows ([`OpenHere::open`](super::OpenHere::open)), then
    /// run `then` on the UI thread with its answer: shown, or why not. The
    /// answer arrives even when the window shows the new record before this
    /// surface draws another frame. Cancelling the effect withdraws a
    /// request the host has not taken up yet. The effect must be declared
    /// `job`.
    pub fn open_here(
        &mut self,
        opener: &super::OpenHere,
        arguments: &[String],
        then: impl FnOnce(&mut H, Result<(), String>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        /// Withdraws the request when the job stops unanswered.
        struct Asked(ui_core::application::ReplacementOutcome);
        impl Drop for Asked {
            fn drop(&mut self) {
                self.0.withdraw();
            }
        }
        let asked = Asked(opener.open(arguments));
        let progress = self.track(Some(Self::then(then)));
        if let Some(started) = self.started.as_mut() {
            started.core.source = Some(Box::new(move || {
                // The job's own channel stays open while it waits here.
                let _waiting = &progress;
                asked
                    .0
                    .get()
                    .map(|answer| Box::new(answer) as Box<dyn Any + Send>)
            }));
        }
        Effected::Pending
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
    /// Jobs started and not yet tracked.
    started: Vec<(String, Job)>,
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
        if !env.active() {
            return;
        }
        let mut cx = EffectCx::<R, H>::new(env.clone(), None);
        let answer = start(&mut cx);
        self.queue
            .borrow_mut()
            .started(key.into(), answer, cx.started.take(), R::KIND);
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
    running: Running<String>,
    marker: std::marker::PhantomData<fn(&mut H)>,
}

impl<R: Resource, H: 'static> OwnedJobs<R, H> {
    pub(crate) fn new(jobs: Jobs<R>) -> Self {
        Self {
            jobs,
            running: Running::default(),
            marker: std::marker::PhantomData,
        }
    }

    /// Take up the jobs started since, stop the cancelled ones, and finish
    /// those that are done with `host`.
    pub(crate) fn poll(&mut self, host: &mut H) {
        let Some(env) = self.jobs.env.clone() else {
            return;
        };
        poll_owned(
            &self.jobs.queue,
            &mut self.running,
            std::any::TypeId::of::<Then<R, H>>(),
            &mut |then, value, custody, invocation| {
                complete::<R, H>(&env, host, then, value, custody, invocation)
            },
        );
    }
}

/// [`OwnedJobs::poll`] of the jobs of `owner` in `queue`, running in
/// `running`, their completions run by `complete`.
fn poll_owned(
    queue: &RefCell<JobQueue>,
    running: &mut Running<String>,
    owner: std::any::TypeId,
    complete: &mut Complete<'_>,
) {
    {
        let mut queue = queue.borrow_mut();
        for key in std::mem::take(&mut queue.cancelled) {
            running.cancel(&key);
        }
        let mut others = Vec::new();
        for (key, started) in std::mem::take(&mut queue.started) {
            if started.owner == owner {
                running.insert(key, started);
            } else {
                others.push((key, started));
            }
        }
        queue.started = others;
    }
    for key in running.keys() {
        while let Some(polled) = running.poll(&key, complete, None) {
            if !queue.borrow_mut().note(&key, polled) {
                break;
            }
        }
    }
}

impl JobQueue {
    /// Queue the job `key` a start answered `answer` for, when it runs on;
    /// stop what it started otherwise. A job still queued under `key` is
    /// replaced.
    fn started(&mut self, key: String, answer: EffectAnswer, started: Option<Job>, kind: &str) {
        match (answer, started) {
            (Ok(Effected::Pending), Some(started)) => {
                self.cancelled.retain(|cancelled| *cancelled != key);
                self.started.retain(|(queued, _)| *queued != key);
                self.running.insert(key.clone());
                self.progress.remove(&key);
                self.started.push((key, started));
            }
            (answer, started) => {
                if let Some(started) = started {
                    started.stop();
                }
                if let Err(error) = answer {
                    tracing::warn!(%error, kind, %key, "a job failed to start");
                }
            }
        }
    }

    /// Note what the job `key` reported; whether to poll it again.
    fn note(&mut self, key: &str, polled: Polled) -> bool {
        match polled {
            Polled::Progress {
                phase,
                completed,
                total,
                ..
            } => {
                self.progress.insert(
                    key.to_owned(),
                    JobProgress {
                        phase,
                        completed,
                        total,
                    },
                );
                true
            }
            Polled::Finished(None) => false,
            Polled::Finished(Some(result)) => {
                if let Err(error) = result {
                    tracing::warn!(error = %error.message, %key, "a job failed");
                }
                self.running.remove(key);
                self.progress.remove(key);
                false
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
/// The specs [`published_specs`] last built for one vocabulary: the
/// locale and statuses they were built at, kept so a publication that finds
/// both unchanged copies them instead of building every declaration again.
pub(crate) type KeptSpecs = std::cell::RefCell<Option<(u64, Vec<EffectStatus>, Vec<ActionSpec>)>>;

/// [`published_specs`] of `E` at `statuses`, taken from `kept` while the
/// locale and the statuses are those it was built at.
pub(crate) fn kept_specs<E: Effect>(
    kept: &KeptSpecs,
    statuses: &[EffectStatus],
) -> Vec<ActionSpec> {
    let locale = ui_core::i18n::I18nManager::revision();
    if let Some((_, _, specs)) = kept
        .borrow()
        .as_ref()
        .filter(|(at, was, _)| *at == locale && was.as_slice() == statuses)
    {
        return specs.clone();
    }
    let specs = published_specs::<E>(statuses);
    *kept.borrow_mut() = Some((locale, statuses.to_vec(), specs.clone()));
    specs
}

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
pub(crate) struct Running<K = InvocationId> {
    running: HashMap<K, Job>,
}

impl<K> Default for Running<K> {
    fn default() -> Self {
        Self {
            running: HashMap::new(),
        }
    }
}

impl<K> Drop for Running<K> {
    fn drop(&mut self) {
        for (_, job) in self.running.drain() {
            job.stop();
        }
    }
}

impl<K: Eq + Hash + Clone> Running<K> {
    fn insert(&mut self, key: K, started: Job) {
        if let Some(previous) = self.running.insert(key, started) {
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
        complete: &mut Complete<'_>,
        invocation: Option<InvocationId>,
    ) -> Option<Polled> {
        let value = match self.running.get_mut(key)?.core.next()? {
            Next::Progress(progress) => return Some(progress),
            Next::Failed(error) => {
                self.cancel(key);
                return Some(Polled::Finished(Some(Err(error))));
            }
            Next::Finished(value) => value,
        };
        let job = self.running.remove(key)?;
        let Some(then) = job.then else {
            return Some(answered(value));
        };
        let (answer, next) = match complete(then, value, job.core.custody, invocation) {
            Ok(completed) => completed,
            Err(error) => return Some(Polled::Finished(Some(Err(failure(error))))),
        };
        let chained = matches!(answer, Ok(Effected::Pending)) && next.is_some();
        match next {
            Some(next) if chained => self.insert(key.clone(), next),
            Some(next) => next.stop(),
            None => {}
        }
        Some(Polled::Finished(completed(answer, chained)))
    }
}

thread_local! {
    /// The effect invocation whose handler or completion runs on this
    /// thread now.
    static ACTING_FOR: Cell<Option<InvocationId>> = const { Cell::new(None) };
}

/// The effect invocation whose handler or completion is running: what a
/// dispatcher queues now continues it, so the host applies it for the
/// effect's invoker.
pub(crate) fn acting_for() -> Option<InvocationId> {
    ACTING_FOR.get()
}

/// Marks an effect invocation's handler or completion as running, until
/// dropped.
struct ActingFor(Option<InvocationId>);

impl ActingFor {
    fn enter(invocation: Option<InvocationId>) -> Self {
        Self(ACTING_FOR.replace(invocation))
    }
}

impl Drop for ActingFor {
    fn drop(&mut self) {
        ACTING_FOR.set(self.0);
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
    jobs: Running,
    marker: std::marker::PhantomData<fn(E, &mut H)>,
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
        doc: Option<&tilo_resource::Doc>,
        handle: impl FnOnce(&mut H, E, &mut EffectCx<R, H>) -> EffectAnswer,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let (request, custody) = op.with_custody();
        if request.type_id != E::OPERATION {
            return Ok(None);
        }
        op.check()?;
        let effect = E::from_payload(request.payload)?;
        let id = effect.id();
        let declaration = E::declaration(id);
        let place = declaration
            .as_ref()
            .and_then(|decl| place_of(decl, &effect));
        let job = admitted(declaration.as_ref(), id, request.kind)?;
        let mut cx = EffectCx::new(self.env.clone(), Some(request.invocation))
            .with_custody(owned_scope(custody))?;
        cx.target = op.target().cloned();
        cx.place.clone_from(&place);
        // Keep an immutable snapshot for this effect invocation, including any
        // pending edits. The effect may also read a historical version; neither
        // read borrows the author's document mutably.
        (cx.document, cx.heads) = lend(doc);
        let mut handling = Some((handle, effect));
        let answer = run_under(
            Some(request.invocation),
            custody,
            &mut || match handling.take() {
                Some((handle, effect)) => handle(host, effect, &mut cx),
                None => Err("an effect's handler ran twice".into()),
            },
        );
        cx.document = None;
        let live = &self.env.live;
        let started = settle_start(
            &mut self.jobs,
            Answered {
                id,
                invocation: request.invocation,
                job,
                answer,
                started: cx.started.take(),
            },
            &mut || {
                let progress = track_job(
                    &mut cx.started,
                    &cx.env.wake,
                    JobCustody::of(cx.worker_custody.as_ref(), cx.custody.as_ref()),
                    None,
                    std::any::TypeId::of::<Then<R, H>>(),
                );
                cx.env.track(&progress);
                progress
            },
            &mut |participant| {
                if let Some(place) = &place {
                    live.land(participant, place.clone());
                }
            },
        );
        if let Some(ready) = cx.started.take() {
            self.jobs.insert(request.invocation, ready);
        }
        started.map(Some)
    }

    /// The next update of the job answering `invocation`, if this runner
    /// runs it.
    pub(crate) fn poll(
        &mut self,
        invocation: InvocationId,
        cancel: bool,
        host: &mut H,
    ) -> Option<InvocationUpdate> {
        let env = &self.env;
        poll_invocation(
            &mut self.jobs,
            invocation,
            cancel,
            &mut |then, value, custody, invocation| {
                complete::<R, H>(env, host, then, value, custody, invocation)
            },
        )
    }
}

/// The scope a worker runs under for `custody`, and the custody the
/// effect's dispatcher keeps.
fn custody_scopes(
    custody: Option<&ResourceInvocationScope>,
) -> Result<(Option<ResourceEffectScope>, Option<ResourceInvocationScope>), String> {
    let Some(custody) = custody else {
        return Ok((None, None));
    };
    Ok((Some(custody.effect_scope()?), Some(custody.clone())))
}

/// A snapshot of `doc` to lend an effect's handler, and its heads.
fn lend(
    doc: Option<&tilo_resource::Doc>,
) -> (
    Option<Rc<RefCell<tilo_resource::Doc>>>,
    Vec<ui_core::automerge::ChangeHash>,
) {
    match doc {
        Some(doc) => (Some(Rc::new(RefCell::new(doc.clone()))), doc.base_heads()),
        None => (None, Vec::new()),
    }
}

/// Whether an effect invoked as `kind` with `declaration` is admitted, and
/// whether it is a job.
///
/// # Errors
/// A view only changes what its caller sees and is admitted as one; an
/// effect invoked as a view would skip the checks an effect gets.
fn admitted(
    declaration: Option<&tilo_resource::effect::EffectDecl>,
    id: &str,
    kind: tilo_actions::OperationKind,
) -> Result<bool, String> {
    if declaration.map(|decl| decl.kind) != Some(kind) {
        return Err(format!("`{id}` was invoked as another kind of operation"));
    }
    Ok(declaration.is_some_and(tilo_resource::effect::EffectDecl::is_job))
}

/// What an effect's handler answered.
struct Answered<'a> {
    id: &'a str,
    invocation: InvocationId,
    /// Whether the effect is declared a job.
    job: bool,
    answer: EffectAnswer,
    /// The job the handler started, if any.
    started: Option<Job>,
}

/// Start what an effect's handler answered: its output at once, or the job
/// it started in `jobs`. `reply` tracks a job for an output a job's caller
/// learns as the job's; `land` moves the participant where the effect was
/// told to act once it succeeded.
///
/// # Errors
/// The handler's failure, or why its answer breaks the effect's
/// declaration.
fn settle_start(
    jobs: &mut Running,
    answered: Answered<'_>,
    reply: &mut dyn FnMut() -> Progress,
    land: &mut dyn FnMut(tilo_actions::ParticipantId),
) -> Result<ResourceOperationStart, String> {
    let Answered {
        id,
        invocation,
        job,
        answer,
        started,
    } = answered;
    let answer = match answer {
        Ok(answer) => answer,
        Err(error) => {
            if let Some(started) = started {
                started.stop();
            }
            return Err(error);
        }
    };
    // A participant told where to act is there now.
    if let Some(participant) = ui_core::invocation_scope::current_participant() {
        land(participant);
    }
    match (answer, started, job) {
        (Effected::Done(output), started, false) => {
            if let Some(started) = started {
                started.stop();
            }
            Ok(ResourceOperationStart::Completed(output))
        }
        // A job's caller learns its result as the job's, even when it
        // was ready at once.
        (Effected::Done(output), started, true) => {
            if let Some(started) = started {
                started.stop();
            }
            let reply = EffectReply {
                progress: reply(),
                completion: false,
            };
            reply.finish(Ok(output));
            Ok(ResourceOperationStart::Accepted)
        }
        (Effected::Pending, Some(started), true) => {
            jobs.insert(invocation, started);
            Ok(ResourceOperationStart::Accepted)
        }
        (Effected::Pending, Some(started), false) => {
            started.stop();
            Err(format!(
                "`{id}` is declared to answer at once; declare it `#[effect(job)]` to answer later"
            ))
        }
        (Effected::Pending, None, _) => Err(format!("`{id}` answered later but started no job")),
    }
}

/// The next update of the job in `jobs` answering `invocation`, its
/// completion run by `complete`; `None` when it runs no such job or has
/// nothing new.
fn poll_invocation(
    jobs: &mut Running,
    invocation: InvocationId,
    cancel: bool,
    complete: &mut Complete<'_>,
) -> Option<InvocationUpdate> {
    if !jobs.contains(&invocation) {
        return None;
    }
    if cancel && jobs.cancel(&invocation) {
        return Some(InvocationUpdate::Cancelled {
            invocation,
            message: None,
        });
    }
    match jobs.poll(&invocation, complete, Some(invocation))? {
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

/// One registered effect vocabulary: what the root publishes for it.
struct Published {
    alive: Weak<()>,
    operation: &'static str,
    specs: Box<dyn Fn() -> Vec<ActionSpec>>,
}

/// A view's effect handler, type-erased for the root.
trait ViewHandler {
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

impl<R: Resource, E: Effect> ViewHandler for Handler<R, E> {
    fn start(
        &mut self,
        op: &ResourceOperation<'_>,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let handle = &mut self.handle;
        self.runner
            .start(op, &mut (), None, |(), effect, cx| handle(effect, cx))
    }

    fn poll(&mut self, invocation: InvocationId, cancel: bool) -> Option<InvocationUpdate> {
        self.runner.poll(invocation, cancel, &mut ())
    }
}

/// A registered projection drain ([`RecordEnv::outbox`](super::RecordEnv::outbox)),
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
trait Drainer {
    /// Start what is pending in `state` (the resource's state) and not
    /// running or just settled.
    fn run(&mut self, state: &dyn Any, revision: u64);
    /// Settle finished jobs at `revision`; whether any did.
    fn poll(&mut self, revision: u64) -> bool;
    /// Forget which entries finished, so those still pending run again.
    fn forget_settled(&mut self);
}

/// The pending entries a resource state (as `&dyn Any`) holds that this
/// device owns, each boxed; `None` for the state of another resource.
type OwnedEntries = Box<dyn Fn(&dyn Any) -> Option<Vec<(Id, Box<dyn Any>)>>>;
/// Run one entry (boxed) as an invocation under a custody: what it answered
/// and the job it started.
type StartEntry = Box<
    dyn FnMut(
        &Id,
        Box<dyn Any>,
        Option<InvocationId>,
        Option<&ResourceInvocationScope>,
    ) -> Result<(EffectAnswer, Option<Job>), String>,
>;
/// How an entry's handler answer settles it: the settle edit and what to
/// tell an invocation waiting on it, `None` when the outbox offers no settle
/// edit, or why the entry stays pending.
type SettleEntry = Box<
    dyn Fn(
        &Id,
        Result<ActionOutput, String>,
    ) -> Result<Option<(ApplySettlement, Result<ActionOutput, ActionFailure>)>, String>,
>;

/// The drain of one [`Outbox`](tilo_resource::Outbox) on the device that
/// owns its entries ([`RecordEnv::outbox`](super::RecordEnv::outbox)). Only
/// reading, running and settling an entry know the resource and the entry's
/// types ([`Effects::outbox`] builds them); the bookkeeping is compiled once.
struct OutboxDrain {
    kind: &'static str,
    owned: OwnedEntries,
    start: StartEntry,
    settle: SettleEntry,
    /// Runs a job's completion, typed by the resource.
    complete: Box<Complete<'static>>,
    /// What an entry abandoned by its invocation settles with.
    abandoned: fn() -> Option<serde_json::Value>,
    jobs: Running<Id>,
    /// Entries whose run failed for now, and the revision it failed at: run
    /// again once the document moved on or the outside changed.
    retry: HashMap<Id, u64>,
    /// Entries this drain settled, never run again here while their
    /// settlement is on its way into the state.
    settled: std::collections::HashSet<Id>,
    /// Entries running for the local invocation whose action queued them,
    /// which completes once they settled.
    caused: HashMap<Id, (InvocationId, causal::Group)>,
    /// The revision the drain last looked at.
    revision: u64,
    /// Settlements to write in the next settle pass, shared with the root.
    settling: Settling,
}

/// One outbox entry's settlement, written in the settle pass of the frame
/// that learned it: the settle edit, and the invocation that waits for it
/// with what to tell it once the edit is in the document.
struct Settlement {
    apply: ApplySettlement,
    waiting: Option<(causal::Group, Result<ActionOutput, ActionFailure>)>,
}

type Settling = Rc<RefCell<Vec<Settlement>>>;
type ApplySettlement =
    Box<dyn FnOnce(&mut ui_core::widget::Edits<'_>) -> Result<(), tilo_actions::ActionError>>;

/// Write the settlements in `settling`, then tell the invocations waiting on
/// them how they settled.
fn apply_settlements(settling: &Settling, edits: &mut ui_core::widget::Edits<'_>) {
    let settling = std::mem::take(&mut *settling.borrow_mut());
    for Settlement { apply, waiting } in settling {
        let applied = apply(edits);
        if let Some((group, told)) = waiting {
            group.borrow_mut().finished(match applied {
                Ok(()) => told,
                Err(error) => Err(failure(error.to_string())),
            });
        } else if let Err(error) = applied {
            tracing::warn!(%error, "an outbox settlement was refused");
        }
    }
}

/// How a handler's answer settles its entry: a [`Settled`] it answered
/// ([`Effected::settled`]), the result `O` it answered, or nothing to report
/// (`O`'s default).
fn settlement<O>(output: &ActionOutput) -> Result<Settled<O>, String>
where
    O: serde::de::DeserializeOwned + Default,
{
    let Some(value) = &output.value else {
        return Ok(Settled::Done(O::default()));
    };
    serde_json::from_value::<Settled<O>>(value.clone())
        .or_else(|_| serde_json::from_value::<O>(value.clone()).map(Settled::Done))
        .map_err(|error| {
            format!("an outbox handler answered what its entry does not settle with: {error}")
        })
}

/// What an outbox entry abandoned by whoever it was for settles with.
fn abandoned<O: serde::Serialize + Default>() -> Option<serde_json::Value> {
    serde_json::to_value(Settled::<O>::failed("cancelled")).ok()
}

impl OutboxDrain {
    /// Record how the entry `id` settled, or leave it pending to run again;
    /// an invocation waiting on it learns which.
    fn finish(&mut self, id: &Id, answer: Result<ActionOutput, String>) -> bool {
        let group = self.caused.remove(id).map(|(_, group)| group);
        match (self.settle)(id, answer) {
            Ok(Some((apply, told))) => {
                self.settled.insert(id.clone());
                // The settlement is in the document before whoever waits on
                // the entry hears it settled: their next request sees it.
                self.settling.borrow_mut().push(Settlement {
                    apply,
                    waiting: group.map(|group| (group, told)),
                });
                true
            }
            Ok(None) => {
                self.settled.insert(id.clone());
                false
            }
            Err(error) => {
                tracing::warn!(%error, kind = self.kind, "an outbox entry stays pending");
                if let Some(group) = group {
                    group.borrow_mut().finished(Err(failure(error)));
                }
                self.retry.insert(id.clone(), self.revision);
                false
            }
        }
    }

    /// The pending entries of `state` this device owns.
    fn owned(&self, state: &dyn Any) -> Option<Vec<(Id, Box<dyn Any>)>> {
        (self.owned)(state)
    }

    /// Run `value` (the entry `id`) as `invocation`, under its custody when
    /// it has one. Whether it finished at once.
    fn start_entry(
        &mut self,
        id: Id,
        value: Box<dyn Any>,
        invocation: Option<InvocationId>,
        custody: Option<&ResourceInvocationScope>,
    ) -> Result<(), String> {
        let (answer, started) = (self.start)(&id, value, invocation, custody)?;
        match (answer, started) {
            (Ok(Effected::Pending), Some(started)) => self.jobs.insert(id, started),
            (answer, started) => {
                if let Some(started) = started {
                    started.stop();
                }
                let answer = answer.and_then(|effected| match effected {
                    Effected::Done(output) => Ok(output),
                    Effected::Pending => {
                        Err("an outbox handler answered later without starting a job".into())
                    }
                });
                self.finish(&id, answer);
            }
        }
        Ok(())
    }
}

impl Drainer for OutboxDrain {
    fn run(&mut self, state: &dyn Any, revision: u64) {
        let Some(pending) = self.owned(state) else {
            return;
        };
        self.revision = revision;
        self.settled
            .retain(|id| pending.iter().any(|(entry, _)| entry == id));
        self.retry
            .retain(|id, _| pending.iter().any(|(entry, _)| entry == id));
        for (id, value) in pending {
            if self.jobs.contains(&id)
                || self.settled.contains(&id)
                || self.retry.get(&id) == Some(&revision)
            {
                continue;
            }
            if let Err(error) = self.start_entry(id, value, None, None) {
                tracing::warn!(%error, kind = self.kind, "an outbox entry did not start");
            }
        }
    }

    fn poll(&mut self, revision: u64) -> bool {
        self.revision = revision;
        let mut finished = false;
        for id in self.jobs.keys() {
            let invocation = self.caused.get(&id).map(|(invocation, _)| *invocation);
            while let Some(polled) = self.jobs.poll(&id, &mut *self.complete, invocation) {
                let Polled::Finished(result) = polled else {
                    continue;
                };
                if let Some(result) = result {
                    self.finish(&id, result.map_err(|error| error.message));
                    finished = true;
                }
                break;
            }
        }
        finished
    }

    fn forget_settled(&mut self) {
        self.retry.clear();
    }
}

/// A drain shared by the drains that run what is pending and, for
/// [`RecordEnv::outbox_awaited`](super::RecordEnv::outbox_awaited), by those that
/// run what an invocation queued.
impl<D: Drainer> Drainer for Rc<RefCell<D>> {
    fn run(&mut self, state: &dyn Any, revision: u64) {
        self.borrow_mut().run(state, revision);
    }

    fn poll(&mut self, revision: u64) -> bool {
        self.borrow_mut().poll(revision)
    }

    fn forget_settled(&mut self) {
        self.borrow_mut().forget_settled();
    }
}

struct Registered {
    alive: Weak<()>,
    operation: &'static str,
    handler: Box<dyn ViewHandler>,
}

struct Drains {
    alive: Weak<()>,
    drain: Box<dyn Drainer>,
}

#[derive(Default)]
struct Inner {
    causal: causal::CausalEffects,
    published: Vec<Published>,
    handlers: Vec<Registered>,
    drains: Vec<Drains>,
    /// The revision drains last ran at.
    drained: Option<u64>,
    /// Set when the outside world a drain compares against changed, so the
    /// drains look again although the document did not move.
    outside_changed: Rc<Cell<bool>>,
    /// Outbox settlements waiting for the settle pass.
    settling: Settling,
}

/// Every effect handler and projection drain of one live resource root, and
/// what they publish. Clones share it.
pub(crate) struct Effects<R: Resource> {
    env: Option<EffectEnv<R>>,
    inner: Rc<RefCell<Inner>>,
    /// The jobs the views start on their own, polled with the drains.
    jobs: Rc<RefCell<OwnedJobs<R, ()>>>,
    /// Whether views and surfaces start work on their own (jobs, projection
    /// drains), or only the effects someone invokes run.
    starts_work: bool,
}

/// An empty [`Inner`], shared. Not generic over the resource, so its
/// construction is compiled once rather than in every app's `Effects`.
#[inline(never)]
fn new_inner() -> Rc<RefCell<Inner>> {
    Rc::default()
}

impl<R: Resource> Clone for Effects<R> {
    fn clone(&self) -> Self {
        Self {
            env: self.env.clone(),
            inner: Rc::clone(&self.inner),
            jobs: Rc::clone(&self.jobs),
            starts_work: self.starts_work,
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
            scope: None,
        };
        Self {
            jobs: Rc::new(RefCell::new(OwnedJobs::new(Jobs::new(Some(env.clone()))))),
            env: Some(env),
            inner: new_inner(),
            starts_work: true,
        }
    }

    /// Effects of a capture a headless drive invokes actions on: an effect
    /// it invokes runs, with the work it starts, as it would in a window
    /// someone uses; the views and surfaces start nothing on their own and
    /// no projection drains, so the scene stays the scenario's fixture.
    pub(crate) fn invoked(dispatch: Dispatcher<R>, live: Live<R>) -> Self {
        Self {
            env: Some(EffectEnv {
                dispatch,
                live,
                wake: WakeHandle::new(),
                scope: None,
            }),
            inner: new_inner(),
            jobs: Rc::new(RefCell::new(OwnedJobs::new(Jobs::new(None)))),
            starts_work: false,
        }
    }

    /// Effects of a past version, which never run: History shows what the
    /// document was, it does not do again what was done.
    pub(crate) fn inert() -> Self {
        Self {
            env: None,
            inner: new_inner(),
            jobs: Rc::new(RefCell::new(OwnedJobs::new(Jobs::new(None)))),
            starts_work: false,
        }
    }

    pub(crate) fn env(&self) -> Option<&EffectEnv<R>> {
        self.env.as_ref()
    }

    /// The jobs the views start on their own.
    #[inline(never)]
    pub(crate) fn view_jobs(&self) -> Jobs<R> {
        self.jobs.borrow().jobs.clone()
    }

    /// Jobs for one surface, whose completions get `&mut` the surface.
    #[inline(never)]
    pub(crate) fn surface_jobs(&self) -> Jobs<R> {
        Jobs::new(self.working_env().cloned())
    }

    pub(crate) fn scoped_surface_jobs(&self, scope: &Rc<super::surface::SurfaceScope>) -> Jobs<R> {
        Jobs::new(self.working_env().map(|env| env.scoped(scope)))
    }

    /// The environment work a view or surface starts on its own runs in;
    /// `None` where none runs.
    fn working_env(&self) -> Option<&EffectEnv<R>> {
        self.env.as_ref().filter(|_| self.starts_work)
    }

    /// Take up and finish the views' jobs.
    #[inline(never)]
    pub(crate) fn poll_jobs(&self) {
        self.jobs.borrow_mut().poll(&mut ());
        self.inner.borrow_mut().causal.poll();
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

    pub(crate) fn outbox<T, O>(
        &self,
        alive: Weak<()>,
        outbox: Path<R, Outbox<T, O>>,
        owns: impl Fn(&R::State, &T) -> bool + 'static,
        run: impl FnMut(&Id, T, &mut EffectCx<R>) -> EffectAnswer + 'static,
        awaited: bool,
    ) -> Projection
    where
        R: Resource<State = R> + tilo_resource::Model + 'static,
        T: tilo_resource::Model + Clone + 'static,
        O: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
    {
        let Some(env) = self.working_env().cloned() else {
            return Projection::default();
        };
        let reader = outbox.clone();
        let owned: OwnedEntries = Box::new(move |state: &dyn Any| {
            let state = state.downcast_ref::<R>()?;
            Some(reader.get(state).map_or_else(Vec::new, |outbox| {
                outbox
                    .pending()
                    .filter(|(_, value)| owns(state, value))
                    .map(|(id, value)| (id.clone(), Box::new(value.clone()) as Box<dyn Any>))
                    .collect()
            }))
        });
        let mut run = run;
        let starting = env.clone();
        let start: StartEntry = Box::new(move |id, value, invocation, custody| {
            let Ok(value) = value.downcast::<T>() else {
                return Err("an outbox entry is of another type".into());
            };
            let mut cx =
                EffectCx::new(starting.clone(), invocation).with_custody(owned_scope(custody))?;
            let mut value = Some(*value);
            let answer = run_under(invocation, custody, &mut || match value.take() {
                Some(value) => run(id, value, &mut cx),
                None => Err("an outbox entry ran twice".into()),
            });
            Ok((answer, cx.started.take()))
        });
        let dispatch = env.dispatch.clone();
        let settle: SettleEntry = Box::new(move |id, answer| {
            let settled = answer.and_then(|output| settlement::<O>(&output))?;
            let told = match &settled {
                Settled::Done(_) => Ok(ActionOutput::default()),
                Settled::Failed { reason, .. } => Err(failure(reason.clone())),
            };
            let Some(action) = R::edit_action(outbox.settle(id, &settled)) else {
                return Ok(None);
            };
            let dispatch = dispatch.clone();
            let apply: ApplySettlement = Box::new(move |edits| dispatch.apply_now(edits, &action));
            Ok(Some((apply, told)))
        });
        let completing = env;
        let complete: Box<Complete<'static>> = Box::new(move |then, value, custody, invocation| {
            complete::<R, ()>(&completing, &mut (), then, value, custody, invocation)
        });
        let drain = Rc::new(RefCell::new(OutboxDrain {
            kind: R::KIND,
            owned,
            start,
            settle,
            complete,
            abandoned: abandoned::<O>,
            jobs: Running::default(),
            retry: HashMap::new(),
            settled: std::collections::HashSet::new(),
            caused: HashMap::new(),
            revision: 0,
            settling: Rc::clone(&self.inner.borrow().settling),
        }));
        let mut inner = self.inner.borrow_mut();
        if awaited {
            inner
                .causal
                .register_drainer(alive.clone(), Box::new(Rc::clone(&drain)));
        }
        inner.drains.push(Drains {
            alive,
            drain: Box::new(drain),
        });
        inner.drained = None;
        Projection {
            outside_changed: Some(Rc::clone(&inner.outside_changed)),
        }
    }

    /// The operation types of every published vocabulary.
    pub(crate) fn operations(&self) -> Vec<&'static str> {
        self.inner.borrow_mut().operations()
    }

    /// Every published effect, as callers see it now.
    #[inline(never)]
    pub(crate) fn specs(&self) -> Vec<ActionSpec> {
        self.inner.borrow_mut().specs()
    }

    /// Answer `op` with the view handler of its vocabulary.
    pub(crate) fn start(
        &self,
        op: &ResourceOperation<'_>,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.inner.borrow_mut().start(op)
    }

    pub(crate) fn poll(&self, invocation: InvocationId, cancel: bool) -> Option<InvocationUpdate> {
        self.inner.borrow_mut().poll(invocation, cancel)
    }

    pub(crate) fn has_causal_drains(&self) -> bool {
        self.inner.borrow().causal.registered()
    }

    pub(crate) fn start_document_effects(
        &self,
        operation: &ResourceOperation<'_>,
        applied: &ui_core::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.inner
            .borrow_mut()
            .causal
            .start(operation, applied, read_state::<R>)
    }

    /// Write the outbox settlements learned since the last settle pass, then
    /// tell the invocations waiting on them how they settled.
    #[inline(never)]
    pub(crate) fn apply_settlements(&self, edits: &mut ui_core::widget::Edits<'_>) {
        let settling = Rc::clone(&self.inner.borrow().settling);
        apply_settlements(&settling, edits);
    }

    /// Settle finished projection jobs, then start what the document at
    /// `revision` lists and is not running yet.
    #[inline(never)]
    pub(crate) fn drain_projections(&self, state: &R::State, revision: u64) {
        self.inner.borrow_mut().drain_projections(state, revision);
    }
}

/// The resource state `doc` holds, read as `R`'s.
fn read_state<R: Resource>(doc: &tilo_resource::Doc) -> Result<Box<dyn Any>, String> {
    R::read(doc)
        .map(|state| Box::new(state) as Box<dyn Any>)
        .map_err(|error| error.to_string())
}

impl Inner {
    fn prune(&mut self) {
        self.published
            .retain(|entry| entry.alive.strong_count() > 0);
        self.handlers.retain(|entry| entry.alive.strong_count() > 0);
        self.drains.retain(|entry| entry.alive.strong_count() > 0);
    }

    fn operations(&mut self) -> Vec<&'static str> {
        self.prune();
        self.published.iter().map(|entry| entry.operation).collect()
    }

    fn specs(&mut self) -> Vec<ActionSpec> {
        self.prune();
        self.published
            .iter()
            .flat_map(|entry| (entry.specs)())
            .collect()
    }

    fn start(
        &mut self,
        op: &ResourceOperation<'_>,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.prune();
        for entry in &mut self.handlers {
            if op.with_custody().0.type_id == entry.operation
                && let Some(started) = entry.handler.start(op)?
            {
                return Ok(Some(started));
            }
        }
        Ok(None)
    }

    fn poll(&mut self, invocation: InvocationId, cancel: bool) -> Option<InvocationUpdate> {
        if let Some(update) = self.causal.poll_invocation(invocation, cancel) {
            return Some(update);
        }
        self.handlers
            .iter_mut()
            .find_map(|entry| entry.handler.poll(invocation, cancel))
    }

    fn drain_projections(&mut self, state: &dyn Any, revision: u64) {
        if self.drains.is_empty() {
            return;
        }
        self.prune();
        let mut settled = false;
        for entry in &mut self.drains {
            settled |= entry.drain.poll(revision);
        }
        let outside_changed = self.outside_changed.replace(false);
        if !settled && !outside_changed && self.drained == Some(revision) {
            return;
        }
        self.drained = Some(revision);
        for entry in &mut self.drains {
            if outside_changed {
                entry.drain.forget_settled();
            }
            entry.drain.run(state, revision);
        }
    }
}

#[cfg(test)]
mod tests;
